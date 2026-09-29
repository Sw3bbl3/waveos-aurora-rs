//! User processes: address space, open handles, lifecycle.
//!
//! A process owns an [`AddressSpace`], a table of [`Handle`]s (files, pipe
//! ends, the log console) and one or more threads (scheduler tasks sharing
//! the address space). Processes are created with [`spawn`] (no fork).
//!
//! Termination is cooperative: exiting, crashing or being killed records the
//! exit code and flags the process; every thread then leaves at its next safe
//! point (syscall return, a timer tick in ring 3, or a blocking wait), so no
//! thread ever dies holding a kernel lock. When the scheduler has reaped the
//! last thread, the `reaper` kernel task frees the process's resources.

pub mod futex;
pub mod pipe;

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
const MAX_FDS: usize = 64;
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
}

impl Drop for Handle {
    fn drop(&mut self) {
        match self {
            Handle::PipeRead { pipe, .. } => pipe.close_reader(),
            Handle::PipeWrite(pipe) => pipe.close_writer(),
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
    pub name: String,
    /// Program path, e.g. `/System/Apps/Notes.elf`.
    pub path: String,
    pub aspace: Mutex<Option<AddressSpace>>,
    /// CR3 of the address space (stable for the process lifetime).
    pub cr3: u64,
    pub fds: Mutex<Vec<Option<Arc<Handle>>>>,
    pub cwd: Mutex<String>,
    exit: AtomicI64,
    pub killed: AtomicBool,
    /// Set once a parent has collected the exit code with `wait`.
    waited: AtomicBool,
    pub main_task: AtomicU64,
    /// Threads that have not been reaped yet.
    threads: AtomicU32,
    pub resident_kib: AtomicU64,
    entry: u64,
    user_sp: u64,
    arg: u64,
}

impl Process {
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
    let data = fs::read_all(path)?;
    let image = aurora_elf::parse(&data).ok_or(EINVAL)?;
    if image.virt_start < layout::PROGRAM_BASE || image.virt_end > layout::MMAP_BASE {
        return Err(EINVAL);
    }
    let mut aspace = AddressSpace::new().ok_or(ENOMEM)?;

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

    let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
    let name = String::from(path.rsplit('/').next().unwrap_or(path).trim_end_matches(".elf"));
    let [i, o, e] = stdio;
    let log = || Some(Arc::new(Handle::Log));
    let fds = alloc::vec![i.or_else(log), o.or_else(log), e.or_else(log)];
    let cwd = if parent != 0 { get(parent).map(|p| p.cwd.lock().clone()) } else { None };
    let process = Arc::new(Process {
        pid,
        parent,
        name: name.clone(),
        path: String::from(path),
        cr3: aspace.cr3(),
        resident_kib: AtomicU64::new(aspace.resident / 1024),
        aspace: Mutex::new(Some(aspace)),
        fds: Mutex::new(fds),
        cwd: Mutex::new(cwd.unwrap_or_else(|| String::from("/"))),
        exit: AtomicI64::new(RUNNING),
        killed: AtomicBool::new(false),
        waited: AtomicBool::new(false),
        main_task: AtomicU64::new(0),
        threads: AtomicU32::new(1),
        entry: image.entry,
        user_sp,
        arg: block_va,
    });
    TABLE.lock().insert(pid, process.clone());
    let task = sched::spawn_task(&name, user_main, pid as u64, pid, process.cr3);
    process.main_task.store(task, Ordering::Relaxed);
    log!("proc", "started pid {} '{}' ({} KiB)", pid, path, process.resident_kib.load(Ordering::Relaxed));
    crate::telemetry::process("start", pid, &name, path);
    Ok(pid)
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
    if entry >= layout::USER_END || stack >= layout::USER_END || stack < 4096 {
        return Err(EINVAL);
    }
    if p.threads.load(Ordering::Acquire) >= 64 {
        return Err(EAGAIN);
    }
    p.threads.fetch_add(1, Ordering::AcqRel);
    let start = alloc::boxed::Box::new(ThreadStart { entry, stack: stack & !0xF, arg });
    let raw = alloc::boxed::Box::into_raw(start) as u64;
    Ok(sched::spawn_task(&p.name, user_thread, raw, p.pid, p.cr3))
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
        log!("proc", "pid {} '{}' crashed: {}", pid, p.name, text.as_str());
        if p.exit_code().is_none() {
            CRASHES.lock().push_back(Crash {
                parent: p.parent,
                name: p.name.clone(),
                path: p.path.clone(),
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

/// True if the current process has been asked to terminate.
pub fn interrupted() -> bool {
    let pid = sched::current_pid();
    pid != 0 && get(pid).is_some_and(|p| p.killed.load(Ordering::Acquire))
}

/// Ends the current thread if its process is terminating. Call only at safe points.
pub fn check_killed() {
    if interrupted() {
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
            p.waited.store(true, Ordering::Release);
            // Already reaped: drop the zombie now (otherwise the reaper will).
            if p.aspace.lock().is_none() {
                TABLE.lock().remove(&pid);
            }
            return Ok(code);
        }
        let now = crate::time::uptime_ms();
        if now >= deadline || interrupted() {
            return Err(EAGAIN);
        }
        sched::wait_until(deadline - now, || p.exit_code().is_some() || interrupted());
    }
}

/// Frees resources of exited processes (runs as a kernel task).
fn reaper() {
    loop {
        sched::wait_until(1000, || !REAP_QUEUE.lock().is_empty());
        while let Some(pid) = REAP_QUEUE.lock().pop_front() {
            let Some(p) = get(pid) else { continue };
            // Closing handles signals EOF/EPIPE to pipe peers.
            p.fds.lock().clear();
            crate::gui::server::process_exited(pid);
            p.aspace.lock().take();
            let code = p.exit_code().unwrap_or(0);
            log!("proc", "pid {} '{}' exited with code {}", pid, p.name, code);
            let mut detail = String::new();
            let _ = core::fmt::Write::write_fmt(&mut detail, format_args!("{code}"));
            crate::telemetry::process(if code == CRASH_CODE { "crash" } else { "exit" }, pid, &p.name, &detail);
            // Nobody will wait for children of the kernel or of dead parents.
            let orphan = p.parent == 0 || get(p.parent).is_none_or(|pp| pp.exit_code().is_some());
            if orphan || p.waited.load(Ordering::Acquire) {
                TABLE.lock().remove(&pid);
            }
            // Children of this process become orphans; reap any that already exited.
            let zombies: Vec<Pid> = TABLE
                .lock()
                .values()
                .filter(|c| c.parent == pid && c.exit_code().is_some() && c.aspace.try_is_none())
                .map(|c| c.pid)
                .collect();
            for z in zombies {
                TABLE.lock().remove(&z);
            }
        }
    }
}

trait TryIsNone {
    fn try_is_none(&self) -> bool;
}

impl TryIsNone for Mutex<Option<AddressSpace>> {
    /// Non-blocking check used while holding the process table lock.
    fn try_is_none(&self) -> bool {
        self.try_lock().is_some_and(|g| g.is_none())
    }
}
