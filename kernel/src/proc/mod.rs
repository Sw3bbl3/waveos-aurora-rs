//! User processes: address space, open handles, lifecycle.
//!
//! A process owns an [`AddressSpace`], a table of [`Handle`]s (files, pipe
//! ends, the log console) and one or more threads (scheduler tasks sharing
//! the address space). WaveOS programs are started with [`spawn`]; Linux
//! programs may also `fork` ([`fork`]) and `exec` ([`exec`]).
//!
//! Termination is cooperative: exiting, crashing or being killed records the
//! exit code and flags the process; every thread then leaves at its next safe
//! point (syscall return, a timer tick in ring 3, or a blocking wait), so no
//! thread ever dies holding a kernel lock. When the scheduler has reaped the
//! last thread, the `reaper` kernel task frees the process's resources.

pub mod futex;
pub mod pipe;
pub mod readiness;

use crate::fs::{self, OpenFile};
use crate::mm::vmm::{AddressSpace, Prot};
use crate::sched;
use crate::sync::{IrqMutex, Mutex};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::{err::*, layout};
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use pipe::Pipe;

pub type Pid = u32;

const RUNNING: i64 = i64::MIN;
/// Open files a process may have (Linux programs expect a thousand).
pub const MAX_FDS: usize = 1024;
/// Exit code used when a process is killed by a CPU exception.
pub const CRASH_CODE: i64 = -11;
pub const KILLED_CODE: i64 = -9;

/// An open object referenced by a file descriptor.
pub enum Handle {
    File(Mutex<OpenFile>),
    PipeRead {
        pipe: Arc<Pipe>,
        nonblocking: bool,
    },
    PipeWrite(Arc<Pipe>),
    /// Writes go to the kernel serial log, prefixed with the process name.
    Log,
    /// A sound playback stream.
    Audio(Arc<crate::drivers::audio::Stream>),
    /// A network socket.
    Socket(crate::net::socket::Socket),
    /// A directory, generated file or device opened by a Linux program.
    Special(Mutex<crate::compat::linux::Special>),
}

impl Drop for Handle {
    fn drop(&mut self) {
        match self {
            Handle::PipeRead { pipe, .. } => pipe.close_reader(),
            Handle::PipeWrite(pipe) => pipe.close_writer(),
            Handle::Audio(stream) => stream.close(),
            _ => {}
        }
    }
}

pub fn new_pipe(nonblocking_read: bool) -> (Arc<Handle>, Arc<Handle>) {
    let p = Arc::new(Pipe::new());
    (Arc::new(Handle::PipeRead { pipe: p.clone(), nonblocking: nonblocking_read }), Arc::new(Handle::PipeWrite(p)))
}

pub struct Process {
    pub pid: Pid,
    pub parent: Pid,
    /// The program's name (it changes with `exec`).
    name: Mutex<String>,
    /// Program path, e.g. `/System/Apps/Notes.elf`.
    path: Mutex<String>,
    pub aspace: Mutex<Option<AddressSpace>>,
    /// CR3 of the address space the process runs in (changes with `exec`).
    cr3: AtomicU64,
    /// The process whose address space this one runs in: itself, or its
    /// parent between `vfork` and `exec` (see [`Process::mem`]).
    vm_owner: AtomicU32,
    /// Set once a `vfork` child has called `exec` or ended (its parent waits for it).
    pub vfork_done: AtomicBool,
    /// Process group and session (Linux job control).
    pub pgid: AtomicU32,
    pub sid: AtomicU32,
    pub fds: Mutex<Vec<Option<Arc<Handle>>>>,
    pub cwd: Mutex<String>,
    exit: AtomicI64,
    pub killed: AtomicBool,
    /// Set once a parent has collected the exit code with `wait`.
    waited: AtomicBool,
    /// Set once the reaper has freed the process's resources.
    reaped: AtomicBool,
    /// The thread carrying out `exec`: every other thread leaves.
    exec_leader: AtomicU64,
    pub main_task: AtomicU64,
    /// Threads that have not been reaped yet.
    threads: AtomicU32,
    pub resident_kib: AtomicU64,
    /// Set for Linux programs (their system calls go to `compat::linux`).
    pub linux: Option<crate::compat::linux::Linux>,
    entry: u64,
    user_sp: u64,
    arg: u64,
}

/// The address space a process runs in, locked (see [`Process::mem`]).
pub struct MemGuard<'a> {
    // Declared first, so it is dropped before the owner it borrows from.
    guard: crate::sync::MutexGuard<'a, Option<AddressSpace>>,
    _owner: Option<Arc<Process>>,
}

impl core::ops::Deref for MemGuard<'_> {
    type Target = Option<AddressSpace>;
    fn deref(&self) -> &Option<AddressSpace> {
        &self.guard
    }
}

impl core::ops::DerefMut for MemGuard<'_> {
    fn deref_mut(&mut self) -> &mut Option<AddressSpace> {
        &mut self.guard
    }
}

impl Process {
    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    pub fn path(&self) -> String {
        self.path.lock().clone()
    }

    pub fn cr3(&self) -> u64 {
        self.cr3.load(Ordering::Acquire)
    }

    /// The address space this process runs in: its own, or, for a `vfork`
    /// child that has not called `exec` yet, its parent's.
    pub fn mem(&self) -> MemGuard<'_> {
        let owner = self.vm_owner.load(Ordering::Acquire);
        if owner != self.pid {
            if let Some(o) = get(owner) {
                let guard = o.aspace.lock();
                // SAFETY: the guard borrows from `o`, which the MemGuard owns
                // and drops after the guard (field order).
                let guard = unsafe {
                    core::mem::transmute::<
                        crate::sync::MutexGuard<'_, Option<AddressSpace>>,
                        crate::sync::MutexGuard<'_, Option<AddressSpace>>,
                    >(guard)
                };
                return MemGuard { guard, _owner: Some(o) };
            }
        }
        MemGuard { guard: self.aspace.lock(), _owner: None }
    }

    pub fn exit_code(&self) -> Option<i64> {
        let c = self.exit.load(Ordering::Acquire);
        (c != RUNNING).then_some(c)
    }

    pub fn fd(&self, fd: usize) -> Result<Arc<Handle>, isize> {
        self.fds.lock().get(fd).and_then(|h| h.clone()).ok_or(EBADF)
    }

    pub fn install_fd(&self, h: Arc<Handle>) -> Result<usize, isize> {
        let mut fds = self.fds.lock();
        if let Some(i) = fds.iter().position(|f| f.is_none()) {
            fds[i] = Some(h);
            return Ok(i);
        }
        if fds.len() >= MAX_FDS {
            return Err(EMFILE);
        }
        fds.push(Some(h));
        Ok(fds.len() - 1)
    }
}

static TABLE: IrqMutex<BTreeMap<Pid, Arc<Process>>> = IrqMutex::new(BTreeMap::new());
static NEXT_PID: AtomicU32 = AtomicU32::new(1);
static REAP_QUEUE: IrqMutex<VecDeque<Pid>> = IrqMutex::new(VecDeque::new());
static REAPER: AtomicU64 = AtomicU64::new(u64::MAX);

/// A crash report for the window server to show to the user.
pub struct Crash {
    /// Parent pid (0 = launched by the desktop).
    pub parent: Pid,
    pub name: String,
    pub path: String,
    pub reason: String,
}

static CRASHES: IrqMutex<VecDeque<Crash>> = IrqMutex::new(VecDeque::new());

pub fn take_crash() -> Option<Crash> {
    CRASHES.lock().pop_front()
}

pub fn get(pid: Pid) -> Option<Arc<Process>> {
    TABLE.lock().get(&pid).cloned()
}

pub fn current() -> Option<Arc<Process>> {
    match sched::current_pid() {
        0 => None,
        pid => get(pid),
    }
}

pub fn all() -> Vec<Arc<Process>> {
    TABLE.lock().values().cloned().collect()
}

pub fn init() {
    REAPER.store(sched::spawn("reaper", reaper), Ordering::Relaxed);
}

fn prot_for(flags: u32) -> Prot {
    if flags & aurora_elf::PF_X != 0 {
        Prot::ReadExec
    } else if flags & aurora_elf::PF_W != 0 {
        Prot::ReadWrite
    } else {
        Prot::ReadOnly
    }
}

/// Starts `path` as a new process. `stdio` handles become fds 0, 1, 2
/// (missing ones default to the log console).
pub fn spawn(path: &str, argv: &[&str], stdio: [Option<Arc<Handle>>; 3], parent: Pid) -> Result<Pid, isize> {
    spawn_env(path, argv, None, stdio, parent)
}

/// Like [`spawn`], with the environment a Linux program gets (`None`: the
/// default one). WaveOS programs have no environment.
pub fn spawn_env(
    path: &str,
    argv: &[&str],
    env: Option<&[String]>,
    stdio: [Option<Arc<Handle>>; 3],
    parent: Pid,
) -> Result<Pid, isize> {
    use crate::compat::linux;
    let data = fs::read_all(path)?;
    let image = aurora_elf::parse(&data).ok_or(ENOEXEC)?;
    let mut aspace = AddressSpace::new().ok_or(ENOMEM)?;
    let (entry, user_sp, arg, linux_state) = if linux::is_linux(&image, path) {
        let user = linux::fs::account_user();
        linux::fs::prepare_root(&user);
        let default_env;
        let env = match env {
            Some(e) => e,
            None => {
                default_env = linux::exec::default_env(&user);
                &default_env[..]
            }
        };
        let start = linux::exec::load(&data, &image, &linux::fs::to_guest(path), argv, env, &mut aspace)?;
        (start.entry, start.sp, 0, Some(linux::Linux::new(start.brk)))
    } else {
        let (sp, block) = load_waveos(&data, &image, argv, &mut aspace)?;
        (image.entry, sp, block, None)
    };

    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    let name = String::from(path.rsplit('/').next().unwrap_or(path).trim_end_matches(".elf"));
    let [i, o, e] = stdio;
    let log = || Some(Arc::new(Handle::Log));
    let fds = alloc::vec![i.or_else(log), o.or_else(log), e.or_else(log)];
    let cwd = if parent != 0 { get(parent).map(|p| p.cwd.lock().clone()) } else { None };
    let process = Arc::new(Process {
        pid,
        parent,
        name: Mutex::new(name.clone()),
        path: Mutex::new(String::from(path)),
        cr3: AtomicU64::new(aspace.cr3()),
        vm_owner: AtomicU32::new(pid),
        vfork_done: AtomicBool::new(false),
        // A program started this way leads its own process group (`fork` children join their parent's).
        pgid: AtomicU32::new(pid),
        sid: AtomicU32::new(pid),
        resident_kib: AtomicU64::new(aspace.resident / 1024),
        aspace: Mutex::new(Some(aspace)),
        fds: Mutex::new(fds),
        cwd: Mutex::new(cwd.unwrap_or_else(|| String::from("/"))),
        exit: AtomicI64::new(RUNNING),
        killed: AtomicBool::new(false),
        waited: AtomicBool::new(false),
        reaped: AtomicBool::new(false),
        exec_leader: AtomicU64::new(0),
        main_task: AtomicU64::new(0),
        threads: AtomicU32::new(1),
        linux: linux_state,
        entry,
        user_sp,
        arg,
    });
    TABLE.lock().insert(pid, process.clone());
    let task = sched::spawn_task(&name, user_main, pid as u64, pid, process.cr3());
    process.main_task.store(task, Ordering::Relaxed);
    log!("proc", "started pid {} '{}' ({} KiB)", pid, path, process.resident_kib.load(Ordering::Relaxed));
    crate::telemetry::process("start", pid, &name, path);
    Ok(pid)
}

/// Loads a WaveOS program and its argument block: [argc][argv pointers][strings].
/// Returns the stack pointer and the block's address.
fn load_waveos(
    data: &[u8],
    image: &aurora_elf::Image,
    argv: &[&str],
    aspace: &mut AddressSpace,
) -> Result<(u64, u64), isize> {
    if image.virt_start < layout::PROGRAM_BASE || image.virt_end > layout::MMAP_BASE {
        return Err(EINVAL);
    }
    // Program segments. Map all first (pages may be shared between segments),
    // then copy file contents in.
    for seg in &image.segments {
        let start = seg.vaddr & !0xFFF;
        let pages = (seg.vaddr + seg.memsz - start).div_ceil(4096);
        aspace.map_anon(start, pages, prot_for(seg.flags))?;
    }
    for seg in &image.segments {
        let bytes = &data[seg.offset as usize..(seg.offset + seg.filesz) as usize];
        aspace.write_bytes(seg.vaddr, bytes)?;
    }

    // Stack with an argument block at the top: [argc][argv pointers][strings].
    let stack_pages = layout::STACK_SIZE / 4096;
    aspace.map_anon(layout::STACK_TOP - layout::STACK_SIZE, stack_pages, Prot::ReadWrite)?;
    let strings_len: usize = argv.iter().map(|a| a.len() + 1).sum();
    let block_len = (8 + 8 * argv.len() + strings_len + 15) & !15;
    if block_len as u64 > layout::STACK_SIZE / 4 {
        return Err(EINVAL);
    }
    let block_va = layout::STACK_TOP - block_len as u64;
    let mut block = Vec::with_capacity(block_len);
    block.extend_from_slice(&(argv.len() as u64).to_le_bytes());
    let mut str_va = block_va + 8 + 8 * argv.len() as u64;
    for a in argv {
        block.extend_from_slice(&str_va.to_le_bytes());
        str_va += a.len() as u64 + 1;
    }
    for a in argv {
        block.extend_from_slice(a.as_bytes());
        block.push(0);
    }
    block.resize(block_len, 0);
    aspace.write_bytes(block_va, &block)?;
    // Entered like a function call: rsp ≡ 8 (mod 16), with a null return address.
    let user_sp = block_va - 8;
    aspace.write_bytes(user_sp, &0u64.to_le_bytes())?;
    Ok((user_sp, block_va))
}

/// Main task of a user process: drops to ring 3.
fn user_main(pid: u64) {
    let Some(p) = get(pid as Pid) else { return };
    let (entry, sp, arg) = (p.entry, p.user_sp, p.arg);
    drop(p);
    crate::arch::syscall::enter_user(entry, sp, arg);
}

struct ThreadStart {
    entry: u64,
    stack: u64,
    arg: u64,
}

/// Starts another thread in the calling process at `entry(arg)` on `stack`.
pub fn spawn_thread(entry: u64, stack: u64, arg: u64) -> Result<u64, isize> {
    let p = current().ok_or(EPERM)?;
    if entry >= layout::USER_END || !(4096..layout::USER_END).contains(&stack) {
        return Err(EINVAL);
    }
    if p.threads.load(Ordering::Acquire) >= 64 {
        return Err(EAGAIN);
    }
    p.threads.fetch_add(1, Ordering::AcqRel);
    let start = alloc::boxed::Box::new(ThreadStart { entry, stack: stack & !0xF, arg });
    let raw = alloc::boxed::Box::into_raw(start) as u64;
    Ok(sched::spawn_task(&p.name(), user_thread, raw, p.pid, p.cr3()))
}

/// Starts a Linux thread in the calling process: it begins as a copy of
/// `frame` (the caller's registers at `clone`), with thread pointer `fs_base`.
pub fn spawn_linux_thread(frame: crate::arch::syscall::SyscallFrame, fs_base: u64) -> Result<u64, isize> {
    let p = current().ok_or(EPERM)?;
    if p.threads.load(Ordering::Acquire) >= 512 {
        return Err(EAGAIN);
    }
    p.threads.fetch_add(1, Ordering::AcqRel);
    let raw = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(frame)) as u64;
    Ok(sched::spawn_task_tls(&p.name(), linux_thread, raw, p.pid, p.cr3(), fs_base))
}

fn linux_thread(raw: u64) {
    let frame = unsafe { alloc::boxed::Box::from_raw(raw as *mut crate::arch::syscall::SyscallFrame) };
    let f = *frame;
    drop(frame);
    if interrupted() {
        return;
    }
    crate::arch::syscall::enter_user_frame(&f);
}

fn user_thread(raw: u64) {
    let start = unsafe { alloc::boxed::Box::from_raw(raw as *mut ThreadStart) };
    // Entered like a function call: rsp ≡ 8 (mod 16).
    let (entry, sp, arg) = (start.entry, start.stack - 8, start.arg);
    drop(start);
    if interrupted() {
        return; // the process is already going away
    }
    crate::arch::syscall::enter_user(entry, sp, arg);
}

/// Called by the scheduler once a thread of `pid` has been switched away from
/// for the last time. The last thread hands the process to the reaper.
pub fn thread_gone(pid: Pid) {
    let Some(p) = get(pid) else { return };
    if p.threads.fetch_sub(1, Ordering::AcqRel) != 1 {
        return;
    }
    // A process whose threads all returned without calling exit.
    let _ = p.exit.compare_exchange(RUNNING, 0, Ordering::AcqRel, Ordering::Acquire);
    // A `vfork` parent may go on now: nothing runs on its memory any more.
    p.vfork_done.store(true, Ordering::Release);
    p.vm_owner.store(p.pid, Ordering::Release);
    if let Some(parent) = get(p.parent) {
        if let Some(l) = &parent.linux {
            let code = p.exit_code().unwrap_or(0);
            l.signals.child_exited(&parent, p.pid, code);
        }
    }
    sched::wake_process(p.parent);
    REAP_QUEUE.lock().push_back(pid);
    sched::wake(REAPER.load(Ordering::Relaxed));
}

/// Records the exit code (the first one wins) and asks every thread to leave.
fn terminate(p: &Process, code: i64) {
    let _ = p.exit.compare_exchange(RUNNING, code, Ordering::AcqRel, Ordering::Acquire);
    p.killed.store(true, Ordering::Release);
    sched::wake_process(p.pid);
}

/// Ends process `p` as signal `sig` does (its exit code is `-sig`).
pub fn terminate_by_signal(p: &Process, sig: u32) {
    terminate(p, -(sig as i64));
}

/// Terminates the calling process. Never returns.
pub fn exit_current(code: i64) -> ! {
    if let Some(p) = current() {
        terminate(&p, code);
    }
    sched::exit();
}

/// Ends only the calling thread (the process lives on while others remain).
pub fn exit_thread() -> ! {
    sched::exit();
}

/// Called from CPU exception handlers when ring-3 code faults. Runs with
/// interrupts disabled (possibly on an IST stack), so it must not block.
pub fn crash_current(reason: core::fmt::Arguments) -> ! {
    let pid = sched::current_pid();
    let mut text = crate::gui::StackText::<256>::new();
    let _ = core::fmt::Write::write_fmt(&mut text, reason);
    if let Some(p) = get(pid) {
        log!("proc", "pid {} '{}' crashed: {}", pid, p.name(), text.as_str());
        if p.exit_code().is_none() {
            CRASHES.lock().push_back(Crash {
                parent: p.parent,
                name: p.name(),
                path: p.path(),
                reason: String::from(text.as_str()),
            });
        }
        terminate(&p, CRASH_CODE);
    }
    sched::exit();
}

/// Requests termination of `pid`. Its threads exit themselves at their next
/// safe point, so none dies while holding a kernel lock.
pub fn kill(pid: Pid) -> Result<(), isize> {
    let p = get(pid).ok_or(ESRCH)?;
    if p.exit_code().is_some() {
        return Ok(());
    }
    terminate(&p, KILLED_CODE);
    if pid == sched::current_pid() {
        sched::exit();
    }
    Ok(())
}

/// True if the current thread should stop waiting: its process is
/// terminating, another thread is replacing the program (`exec`), or (Linux)
/// a signal is waiting for it.
pub fn interrupted() -> bool {
    let pid = sched::current_pid();
    if pid == 0 {
        return false;
    }
    let Some(p) = get(pid) else { return false };
    p.killed.load(Ordering::Acquire)
        || leaving_for_exec(&p)
        || p.linux.as_ref().is_some_and(|l| l.signals.deliverable(sched::current_id()))
}

/// True if another thread of `p` is carrying out `exec` (this one must leave).
fn leaving_for_exec(p: &Process) -> bool {
    let leader = p.exec_leader.load(Ordering::Acquire);
    leader != 0 && leader != sched::current_id()
}

/// Ends the current thread if its process is terminating (or another thread
/// is replacing the program). Call only at safe points.
pub fn check_killed() {
    let pid = sched::current_pid();
    if pid != 0 && get(pid).is_some_and(|p| p.killed.load(Ordering::Acquire) || leaving_for_exec(&p)) {
        sched::exit();
    }
}

/// Waits for `pid` (a child of the caller) to exit; returns its exit code.
pub fn wait(pid: Pid, timeout_ms: u64) -> Result<i64, isize> {
    let me = sched::current_pid();
    let p = get(pid).ok_or(ECHILD)?;
    if p.parent != me {
        return Err(ECHILD);
    }
    let deadline = crate::time::uptime_ms().saturating_add(timeout_ms);
    loop {
        if let Some(code) = p.exit_code() {
            collect(&p);
            return Ok(code);
        }
        let now = crate::time::uptime_ms();
        if now >= deadline || interrupted() {
            return Err(EAGAIN);
        }
        sched::wait_until(deadline - now, || p.exit_code().is_some() || interrupted());
    }
}

/// Marks an exited child as waited for, dropping it from the table once
/// the reaper is done with it (otherwise the reaper will).
fn collect(p: &Process) {
    p.waited.store(true, Ordering::Release);
    if p.reaped.load(Ordering::Acquire) {
        TABLE.lock().remove(&p.pid);
    }
}

/// Which children a Linux `wait` accepts.
#[derive(Clone, Copy)]
pub enum Which {
    Any,
    Pid(Pid),
    Group(Pid),
}

/// Linux `wait4`/`waitid`: an exited child of the caller matching `which`,
/// as (pid, exit code). With `nohang`, `Ok(None)` while none has exited;
/// `ECHILD` if there is no such child at all. `consume` false leaves it
/// waitable (`WNOWAIT`).
pub fn wait_child(which: Which, nohang: bool, consume: bool) -> Result<Option<(Pid, i64)>, isize> {
    let me = sched::current_pid();
    loop {
        let children: Vec<Arc<Process>> = TABLE
            .lock()
            .values()
            .filter(|c| c.parent == me && !c.waited.load(Ordering::Acquire))
            .filter(|c| match which {
                Which::Any => true,
                Which::Pid(pid) => c.pid == pid,
                Which::Group(g) => c.pgid.load(Ordering::Relaxed) == g,
            })
            .cloned()
            .collect();
        if children.is_empty() {
            return Err(ECHILD);
        }
        if let Some(c) = children.iter().find(|c| c.exit_code().is_some()) {
            let code = c.exit_code().unwrap_or(0);
            if consume {
                collect(c);
            }
            return Ok(Some((c.pid, code)));
        }
        if nohang {
            return Ok(None);
        }
        if interrupted() {
            return Err(EINTR);
        }
        sched::wait_until(100, || children.iter().any(|c| c.exit_code().is_some()) || interrupted());
    }
}

/// A child of `parent` (a Linux `fork`, `vfork` or process-creating
/// `clone`): it starts as a copy of the caller's registers `frame`, with
/// thread pointer `fs_base`, in a copy of the caller's memory — or, with
/// `share_vm` (`vfork`), in the caller's memory itself until it calls
/// `exec` or ends.
pub fn fork(
    parent: &Process,
    frame: crate::arch::syscall::SyscallFrame,
    fs_base: u64,
    share_vm: bool,
    linux: crate::compat::linux::Linux,
) -> Result<Arc<Process>, isize> {
    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    let (aspace, cr3, owner, resident) = if share_vm {
        (None, parent.cr3(), parent.vm_owner.load(Ordering::Acquire), 0)
    } else {
        let mem = parent.mem();
        let copy = mem.as_ref().ok_or(ESRCH)?.fork_copy().ok_or(ENOMEM)?;
        let (cr3, resident) = (copy.cr3(), copy.resident);
        (Some(copy), cr3, pid, resident)
    };
    let name = parent.name();
    let process = Arc::new(Process {
        pid,
        parent: parent.pid,
        name: Mutex::new(name.clone()),
        path: Mutex::new(parent.path()),
        cr3: AtomicU64::new(cr3),
        vm_owner: AtomicU32::new(owner),
        vfork_done: AtomicBool::new(false),
        pgid: AtomicU32::new(parent.pgid.load(Ordering::Relaxed)),
        sid: AtomicU32::new(parent.sid.load(Ordering::Relaxed)),
        resident_kib: AtomicU64::new(resident / 1024),
        aspace: Mutex::new(aspace),
        fds: Mutex::new(parent.fds.lock().clone()),
        cwd: Mutex::new(parent.cwd.lock().clone()),
        exit: AtomicI64::new(RUNNING),
        killed: AtomicBool::new(false),
        waited: AtomicBool::new(false),
        reaped: AtomicBool::new(false),
        exec_leader: AtomicU64::new(0),
        main_task: AtomicU64::new(0),
        threads: AtomicU32::new(1),
        linux: Some(linux),
        entry: 0,
        user_sp: 0,
        arg: 0,
    });
    TABLE.lock().insert(pid, process.clone());
    // The child's SIMD registers are the caller's, as they are right now.
    let mut fpu = alloc::boxed::Box::new(crate::arch::fpu::FpuState::new());
    fpu.save();
    let raw = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(frame)) as u64;
    log!("proc", "pid {} forked pid {}{}", parent.pid, pid, if share_vm { " (sharing memory)" } else { "" });
    let task = sched::spawn_user(&name, fork_main, raw, pid, cr3, fs_base, fpu);
    process.main_task.store(task, Ordering::Release);
    crate::telemetry::process("start", pid, &name, &parent.path());
    Ok(process)
}

/// Main task of a forked child: returns to user mode from the copied frame.
fn fork_main(raw: u64) {
    let frame = unsafe { alloc::boxed::Box::from_raw(raw as *mut crate::arch::syscall::SyscallFrame) };
    let f = *frame;
    drop(frame);
    if let Some(p) = current() {
        p.main_task.store(sched::current_id(), Ordering::Release);
    }
    if interrupted() && current().is_some_and(|p| p.killed.load(Ordering::Acquire)) {
        return;
    }
    crate::arch::syscall::enter_user_frame(&f);
}

/// First step of `exec`: every other thread of the caller's process leaves.
/// Returns `EINTR` (and nothing changes) if the process is being killed.
pub fn exec_single_thread(p: &Process) -> Result<(), isize> {
    let me = sched::current_id();
    if p.exec_leader.compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire).is_err() {
        // Another thread is already replacing the program: this one leaves.
        sched::exit();
    }
    sched::wake_process(p.pid);
    while p.threads.load(Ordering::Acquire) > 1 {
        if p.killed.load(Ordering::Acquire) {
            p.exec_leader.store(0, Ordering::Release);
            return Err(EINTR);
        }
        sched::wait_until(5, || p.threads.load(Ordering::Acquire) <= 1);
    }
    Ok(())
}

/// Last step of `exec`: the caller's process runs `aspace` (already loaded)
/// as the program `path`. The calling thread becomes its main thread.
pub fn exec_replace(p: &Process, aspace: AddressSpace, path: &str) {
    let cr3 = aspace.cr3();
    p.resident_kib.store(aspace.resident / 1024, Ordering::Relaxed);
    let old = p.aspace.lock().replace(aspace);
    p.cr3.store(cr3, Ordering::Release);
    let was_vfork = p.vm_owner.swap(p.pid, Ordering::AcqRel) != p.pid;
    sched::switch_address_space(cr3);
    // Not running on it any more (and no other thread is): free it.
    drop(old);
    let name = String::from(path.rsplit('/').next().unwrap_or(path));
    sched::rename_current(&name);
    *p.name.lock() = name;
    *p.path.lock() = String::from(path);
    p.main_task.store(sched::current_id(), Ordering::Release);
    p.exec_leader.store(0, Ordering::Release);
    if was_vfork {
        p.vfork_done.store(true, Ordering::Release);
        sched::wake_process(p.parent);
    }
    crate::telemetry::process("exec", p.pid, &p.name(), path);
}

/// Processes in process group `pgid`.
pub fn group(pgid: Pid) -> Vec<Arc<Process>> {
    TABLE
        .lock()
        .values()
        .filter(|p| p.pgid.load(Ordering::Relaxed) == pgid && p.exit_code().is_none())
        .cloned()
        .collect()
}

/// Frees resources of exited processes (runs as a kernel task).
fn reaper() {
    loop {
        sched::wait_until(1000, || !REAP_QUEUE.lock().is_empty());
        // (Pop in its own statement: a `while let` would hold the lock — with
        // interrupts off — for the whole body.)
        loop {
            let Some(pid) = REAP_QUEUE.lock().pop_front() else { break };
            let Some(p) = get(pid) else { continue };
            // `vfork` children still running in its memory go first.
            let borrowers: Vec<Arc<Process>> = TABLE
                .lock()
                .values()
                .filter(|c| c.pid != pid && c.vm_owner.load(Ordering::Acquire) == pid)
                .cloned()
                .collect();
            if !borrowers.is_empty() {
                for b in &borrowers {
                    terminate(b, KILLED_CODE);
                }
                REAP_QUEUE.lock().push_back(pid);
                sched::sleep_ms(10);
                continue;
            }
            // Closing handles signals EOF/EPIPE to pipe peers.
            p.fds.lock().clear();
            crate::gui::server::process_exited(pid);
            p.aspace.lock().take();
            p.reaped.store(true, Ordering::Release);
            let code = p.exit_code().unwrap_or(0);
            log!("proc", "pid {} '{}' exited with code {}", pid, p.name(), code);
            let mut detail = String::new();
            let _ = core::fmt::Write::write_fmt(&mut detail, format_args!("{code}"));
            crate::telemetry::process(if code == CRASH_CODE { "crash" } else { "exit" }, pid, &p.name(), &detail);
            // Nobody will wait for children of the kernel or of dead parents.
            let orphan = p.parent == 0 || get(p.parent).is_none_or(|pp| pp.exit_code().is_some());
            if orphan || p.waited.load(Ordering::Acquire) {
                TABLE.lock().remove(&pid);
            }
            // Children of this process become orphans; reap any that already exited.
            let zombies: Vec<Pid> = TABLE
                .lock()
                .values()
                .filter(|c| c.parent == pid && c.reaped.load(Ordering::Acquire))
                .map(|c| c.pid)
                .collect();
            for z in zombies {
                TABLE.lock().remove(&z);
            }
        }
    }
}
