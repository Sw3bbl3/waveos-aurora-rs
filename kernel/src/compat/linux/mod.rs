//! Linux programs on WaveOS.
//!
//! A program file without WaveOS's ELF note (see `corekit`) that looks like it
//! was built for Linux runs as a Linux process: it gets a System V start-up
//! stack ([`exec`]), and its system calls arrive here with Linux's numbers and
//! meanings and are carried out with the kernel's own files, processes,
//! memory and clocks. It sees a Linux-shaped file tree ([`fs`]): `/usr`,
//! `/lib` and `/etc` come from the read-only `/System/Linux`, `/home`, `/tmp`
//! and `/var` from the writable `/Linux`, and WaveOS folders such as
//! `/Documents` are where they always are.
//!
//! Calls not implemented yet return `ENOSYS`, and each is logged once, which
//! is how the next ones to add are found.
//!
//! Linux has no windows of its own, so a Linux program reaches WaveOS's
//! windows, events, sound and clipboard through call numbers from
//! [`WAVE_BASE`] up: `WAVE_BASE + n` is WaveOS's call `n`, with WaveOS's
//! arguments (`libs/linux/include/waveos.h` describes them for C).

pub mod abi;
pub mod chardev;
mod cpu;
pub mod drm;
mod events;
pub mod exec;
pub mod fs;
pub mod inotify;
pub mod input;
pub mod links;
mod misc;
pub(crate) use misc::realtime_ns;
mod mm;
mod net;
mod process;
pub mod shm;
pub mod signal;
mod syscount;
mod sysfs;
pub mod uevent;
pub mod unix;
pub mod vdso;

use crate::arch::syscall::SyscallFrame;
use crate::proc;
use crate::sync::Mutex;
use abi::nr;
use alloc::collections::BTreeMap;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicU64, Ordering};

pub use fs::Special;

/// What a Linux process keeps beyond a WaveOS one.
pub struct Linux {
    pub mm: Mutex<mm::Memory>,
    /// Each thread's `set_tid_address` word (by task), cleared and woken
    /// when the thread exits: how `pthread_join` learns of it.
    pub clear_tid: Mutex<BTreeMap<u64, u64>>,
    /// Descriptors closed by `exec` (one bit per descriptor; only open ones are set).
    pub cloexec: Mutex<[u64; 16]>,
    /// Descriptors in non-blocking mode (`O_NONBLOCK`), the same way.
    nonblock: Mutex<[u64; 16]>,
    pub signals: signal::Signals,
    /// Log every system call (`WAVEOS_STRACE=1` in the environment).
    pub trace: core::sync::atomic::AtomicBool,
    /// Count them (`WAVEOS_SYSCOUNT=1`).
    counts: syscount::Counts,
    /// Where the program's vDSO is (0: none): arm64 signal handlers
    /// return through it.
    vdso: AtomicU64,
    /// The program's auxiliary vector (`/proc/self/auxv`).
    auxv: Mutex<alloc::vec::Vec<u8>>,
}

impl Linux {
    pub fn new(brk: u64) -> Linux {
        Linux {
            mm: Mutex::new(mm::Memory::new(brk)),
            clear_tid: Mutex::new(BTreeMap::new()),
            cloexec: Mutex::new([0; 16]),
            nonblock: Mutex::new([0; 16]),
            signals: signal::Signals::new(),
            trace: core::sync::atomic::AtomicBool::new(false),
            counts: syscount::Counts::default(),
            vdso: AtomicU64::new(0),
            auxv: Mutex::new(alloc::vec::Vec::new()),
        }
    }

    /// Tracing and counting as the program's environment asks.
    pub fn configure(&self, env: &[alloc::string::String]) {
        self.trace.store(tracing(env), core::sync::atomic::Ordering::Relaxed);
        self.counts.reset(syscount::wanted(env));
    }

    /// A forked child's copy.
    pub fn for_child(&self) -> Linux {
        Linux {
            mm: Mutex::new(self.mm.lock().clone()),
            clear_tid: Mutex::new(BTreeMap::new()),
            cloexec: Mutex::new(*self.cloexec.lock()),
            nonblock: Mutex::new(*self.nonblock.lock()),
            signals: self.signals.for_child(),
            trace: core::sync::atomic::AtomicBool::new(self.trace.load(core::sync::atomic::Ordering::Relaxed)),
            counts: syscount::Counts::new(self.counts.on()),
            vdso: AtomicU64::new(self.vdso()),
            auxv: Mutex::new(self.auxv()),
        }
    }

    /// The program's vDSO (`AT_SYSINFO_EHDR`; 0: none).
    pub fn vdso(&self) -> u64 {
        self.vdso.load(Ordering::Relaxed)
    }

    /// The auxiliary vector the program started with.
    pub fn auxv(&self) -> alloc::vec::Vec<u8> {
        self.auxv.lock().clone()
    }

    /// Records what `exec` set up: where the vDSO went, the auxiliary vector.
    pub fn started(&self, start: &exec::Start) {
        self.vdso.store(start.vdso, Ordering::Relaxed);
        *self.auxv.lock() = start.auxv.clone();
    }

    /// A new program (`exec`) with its heap at `brk`.
    pub fn exec_reset(&self, brk: u64, env: &[alloc::string::String]) {
        self.configure(env);
        *self.mm.lock() = mm::Memory::new(brk);
        self.clear_tid.lock().clear();
        self.signals.exec_reset();
    }

    /// Marks descriptor `fd` close-on-exec, or not.
    pub fn set_cloexec(&self, fd: usize, on: bool) {
        if fd < 1024 {
            let mut bits = self.cloexec.lock();
            if on {
                bits[fd / 64] |= 1 << (fd % 64);
            } else {
                bits[fd / 64] &= !(1 << (fd % 64));
            }
        }
    }

    pub fn set_nonblock(&self, fd: usize, on: bool) {
        if fd < 1024 {
            let mut bits = self.nonblock.lock();
            if on {
                bits[fd / 64] |= 1 << (fd % 64);
            } else {
                bits[fd / 64] &= !(1 << (fd % 64));
            }
        }
    }

    pub fn nonblock(&self, fd: usize) -> bool {
        fd < 1024 && self.nonblock.lock()[fd / 64] & (1 << (fd % 64)) != 0
    }

    pub fn cloexec(&self, fd: usize) -> bool {
        fd < 1024 && self.cloexec.lock()[fd / 64] & (1 << (fd % 64)) != 0
    }
}

/// Should the program in `path` (with this ELF image) run as a Linux program?
/// WaveOS programs carry a note (see `corekit`); anything else is a Linux
/// program, except in the folders where WaveOS programs built before the
/// note existed may live (installed apps, the developer tools).
pub fn is_linux(image: &aurora_elf::Image, path: &str) -> bool {
    if image.waveos_note {
        return false;
    }
    let legacy = ["/Applications/", "/System/Apps/", "/System/Bin/", "/System/Developer/"];
    image.interp.is_some() || image.gnu_note || image.osabi == 3 || !legacy.iter().any(|d| path.starts_with(d))
}

/// Linux programs make WaveOS call `n` as `WAVE_BASE + n` ("WA" in ASCII).
pub const WAVE_BASE: u64 = 0x5741_0000;

/// The WaveOS calls Linux programs may make: windows, events, sound,
/// the clipboard, notifications, preferences, the screen's size, game
/// controllers and their sandbox (the open and save panel, bookmarks).
fn wave_call(p: &proc::Process, n: u64, a: [u64; 6]) -> Result<u64, isize> {
    use aurora_abi::nr as w;
    match n as usize {
        w::EVENT_FD => events::wave_event_fd(p, a[0]),
        w::WIN_CREATE
        | w::WIN_SURFACE
        | w::WIN_PRESENT
        | w::WIN_SET_TITLE
        | w::WIN_SET_CURSOR
        | w::WIN_SET_PROGRAM
        | w::WIN_BUFFER
        | w::WIN_BUFFER_DESTROY
        | w::WIN_ATTACH
        | w::WIN_SET_FULLSCREEN
        | w::WIN_CLOSE
        | w::WIN_RESIZE
        | w::NEXT_EVENT
        | w::AUDIO_OPEN
        | w::AUDIO_CONTROL
        | w::CLIPBOARD_SET
        | w::CLIPBOARD_GET
        | w::NOTIFY
        | w::PREF_GET
        | w::DISPLAY_MODES
        | w::UNIX_MS
        | w::TIME_MS
        | w::GAMEPADS
        | w::GAMEPAD_STATE
        | w::GAMEPAD_RUMBLE
        | w::SANDBOX => crate::syscall::handle(n as usize, a),
        _ => Err(ENOSYS),
    }
}

/// True if the environment asks for system calls to be logged.
pub fn tracing(env: &[alloc::string::String]) -> bool {
    env.iter().any(|e| e == "WAVEOS_STRACE=1")
}

/// The uid and gid of process `pid` (the session's if it is gone).
pub fn ids_of(pid: u32) -> (u32, u32) {
    let c = proc::get(pid).map(|p| *p.cred.lock()).unwrap_or_else(crate::users::session);
    (c.uid, c.gid)
}

/// Entry for a Linux process's system call (from `syscall::dispatch`).
pub fn dispatch(f: &mut SyscallFrame, p: &alloc::sync::Arc<proc::Process>) -> Result<u64, isize> {
    if let Some(l) = p.linux.as_ref().filter(|l| l.counts.on()) {
        let (nr, start) = (f.number(), crate::time::now_ns());
        let r = traced(f, p);
        l.counts.record(|| p.name(), nr, crate::time::now_ns() - start);
        return r;
    }
    traced(f, p)
}

/// A call by the number the program used: its ABI's table says which
/// shared call that is (`abi`).
fn native(f: &mut SyscallFrame, p: &alloc::sync::Arc<proc::Process>) -> Result<u64, isize> {
    let n = f.number();
    match abi::native::lookup(n) {
        abi::Call::Shared(nr) => call(f, p, nr),
        abi::Call::Own => abi::native::own(n, f, p),
        abi::Call::None if (WAVE_BASE..WAVE_BASE + 0x1000).contains(&n) => wave_call(p, n - WAVE_BASE, f.args()),
        abi::Call::None => {
            unimplemented(n);
            Err(ENOSYS)
        }
    }
}

fn traced(f: &mut SyscallFrame, p: &alloc::sync::Arc<proc::Process>) -> Result<u64, isize> {
    if !p.linux.as_ref().is_some_and(|l| l.trace.load(core::sync::atomic::Ordering::Relaxed)) {
        return native(f, p);
    }
    let (native_nr, a) = (f.number(), f.args());
    let nr = abi::native::canonical(native_nr);
    let r = native(f, p);
    let tid = misc::tid(p);
    // The path a call names, if it names one (which argument holds it).
    let path_arg = match nr {
        nr::OPEN | nr::STAT | nr::LSTAT | nr::ACCESS | nr::READLINK | nr::EXECVE | nr::MKDIR | nr::UNLINK => Some(0),
        nr::OPENAT
        | nr::NEWFSTATAT
        | nr::STATX
        | nr::FACCESSAT
        | nr::FACCESSAT2
        | nr::READLINKAT
        | nr::MKDIRAT
        | nr::INOTIFY_ADD_WATCH => Some(1),
        _ => None,
    };
    let path = path_arg.and_then(|i| crate::syscall::user::cstr(a[i]).ok()).unwrap_or_default();
    let result = match r {
        Ok(v) => alloc::format!("{v:#x}"),
        Err(e) => alloc::format!("-{}", e.abs()),
    };
    log!(
        "strace",
        "{tid} {native_nr}{}({:#x}, {:#x}, {:#x}, {:#x}, {:#x}, {:#x}) {path} = {result}",
        label(native_nr),
        a[0],
        a[1],
        a[2],
        a[3],
        a[4],
        a[5]
    );
    r
}

/// Call `number` (the layer's numbering, `abi::nr`) with the frame's arguments.
fn call(f: &mut SyscallFrame, p: &alloc::sync::Arc<proc::Process>, number: u64) -> Result<u64, isize> {
    const SIGCHLD: u64 = signal::SIGCHLD as u64;
    let a = f.args();
    let p = p.clone();
    match number {
        nr::READ => fs::read(&p, a[0], a[1], a[2]),
        nr::WRITE => fs::write(&p, a[0], a[1], a[2]),
        nr::OPEN => fs::openat(&p, fs::AT_FDCWD, a[0], a[1], a[2]),
        nr::CREAT => fs::openat(&p, fs::AT_FDCWD, a[0], fs::O_CREAT | fs::O_WRONLY | fs::O_TRUNC, a[1]),
        nr::OPENAT => fs::openat(&p, a[0], a[1], a[2], a[3]),
        nr::CLOSE => fs::close(&p, a[0]),
        nr::CLOSE_RANGE => fs::close_range(&p, a[0], a[1], a[2]),
        nr::STAT => fs::stat_path(&p, fs::AT_FDCWD, a[0], a[1], 0),
        nr::LSTAT => fs::stat_path(&p, fs::AT_FDCWD, a[0], a[1], fs::AT_SYMLINK_NOFOLLOW),
        // Hard links: WaveOS's file systems have none. EPERM is what Linux
        // says on such file systems (FAT), and what programs expect there:
        // fontconfig then locks its cache with a folder instead, and can
        // save the cache that spares each program a scan of every font.
        nr::LINK | nr::LINKAT => Err(EPERM),
        nr::SYMLINK => fs::symlinkat(&p, a[0], fs::AT_FDCWD, a[1]),
        nr::SYMLINKAT => fs::symlinkat(&p, a[0], a[1], a[2]),
        nr::FSTAT => fs::fstat(&p, a[0], a[1]),
        nr::NEWFSTATAT => fs::stat_path(&p, a[0], a[1], a[2], a[3]),
        nr::STATX => fs::statx(&p, a[0], a[1], a[2], a[4]),
        nr::LSEEK => fs::lseek(&p, a[0], a[1] as i64, a[2]),
        nr::PREAD64 => fs::pread(&p, a[0], a[1], a[2], a[3]),
        nr::PWRITE64 => fs::pwrite(&p, a[0], a[1], a[2], a[3]),
        nr::READV => fs::readv(&p, a[0], a[1], a[2]),
        nr::WRITEV => fs::writev(&p, a[0], a[1], a[2]),
        nr::GETDENTS64 => fs::getdents64(&p, a[0], a[1], a[2]),
        nr::IOCTL => fs::ioctl(&p, a[0], a[1], a[2]),
        nr::FCNTL => fs::fcntl(&p, a[0], a[1], a[2]),
        nr::DUP => fs::dup(&p, a[0]),
        nr::DUP2 => fs::dup3(&p, a[0], a[1], 0, true),
        nr::DUP3 => fs::dup3(&p, a[0], a[1], a[2], false),
        nr::PIPE => fs::pipe2(&p, a[0], 0),
        nr::PIPE2 => fs::pipe2(&p, a[0], a[1]),
        nr::ACCESS => fs::access(&p, fs::AT_FDCWD, a[0], a[1]),
        nr::FACCESSAT | nr::FACCESSAT2 => fs::access(&p, a[0], a[1], a[2]),
        nr::GETCWD => fs::getcwd(&p, a[0], a[1]),
        nr::CHDIR => fs::chdir(&p, a[0]),
        nr::FCHDIR => fs::fchdir(&p, a[0]),
        nr::MKDIR => fs::mkdirat(&p, fs::AT_FDCWD, a[0], a[1]),
        nr::MKDIRAT => fs::mkdirat(&p, a[0], a[1], a[2]),
        nr::RMDIR => fs::unlinkat(&p, fs::AT_FDCWD, a[0], fs::AT_REMOVEDIR),
        nr::UNLINK => fs::unlinkat(&p, fs::AT_FDCWD, a[0], 0),
        nr::UNLINKAT => fs::unlinkat(&p, a[0], a[1], a[2]),
        nr::RENAME => fs::renameat(&p, fs::AT_FDCWD, a[0], fs::AT_FDCWD, a[1]),
        nr::RENAMEAT | nr::RENAMEAT2 => fs::renameat(&p, a[0], a[1], a[2], a[3]),
        nr::READLINK => fs::readlinkat(&p, fs::AT_FDCWD, a[0], a[1], a[2]),
        nr::READLINKAT => fs::readlinkat(&p, a[0], a[1], a[2], a[3]),
        nr::TRUNCATE => fs::truncate(&p, a[0], a[1]),
        nr::FTRUNCATE => fs::ftruncate(&p, a[0], a[1]),
        nr::STATFS => fs::statfs_path(&p, a[0], a[1]),
        nr::FSTATFS => fs::fstatfs(&p, a[0], a[1]),
        nr::FALLOCATE => fs::fallocate(&p, a[0], a[1], a[2], a[3]),
        nr::FSYNC | nr::FDATASYNC | nr::SYNCFS => fs::fsync(&p, a[0]),
        nr::FLOCK | nr::FADVISE64 | nr::READAHEAD => {
            p.fd(a[0] as usize)?;
            Ok(0)
        }
        nr::SYNC => {
            crate::fs::sync_all();
            Ok(0)
        }
        // Owners and permission bits (volumes without them accept and keep nothing).
        nr::CHMOD => fs::chmod(&p, fs::AT_FDCWD, a[0], a[1]),
        nr::FCHMODAT => fs::chmod(&p, a[0], a[1], a[2]),
        nr::FCHMOD => fs::fchmod(&p, a[0], Some(a[1]), (0, 0)),
        nr::CHOWN | nr::LCHOWN => fs::chown(&p, fs::AT_FDCWD, a[0], a[1], a[2]),
        nr::FCHOWNAT => fs::chown(&p, a[0], a[1], a[2], a[3]),
        nr::FCHOWN => fs::fchmod(&p, a[0], None, (a[1], a[2])),
        nr::UTIMES | nr::UTIMENSAT => Ok(0),
        nr::UMASK => {
            let mut c = p.cred.lock();
            let old = c.umask;
            c.umask = (a[0] & 0o777) as u16;
            Ok(old as u64)
        }
        nr::POLL => fs::poll(&p, a[0], a[1], a[2] as i64),
        nr::PPOLL => fs::ppoll(&p, a[0], a[1], a[2], a[3]),
        nr::SELECT => fs::select(&p, a[0], [a[1], a[2], a[3]], fs::Timeout::Timeval(a[4]), 0),
        nr::PSELECT6 => fs::select(&p, a[0], [a[1], a[2], a[3]], fs::Timeout::Timespec(a[4]), a[5]),

        nr::SOCKET => net::socket(&p, a[0], a[1], a[2]),
        nr::SOCKETPAIR => net::socketpair(&p, a[0], a[1], a[2], a[3]),
        nr::BIND => net::bind(&p, a[0], a[1], a[2]),
        nr::LISTEN => net::listen(&p, a[0], a[1]),
        nr::CONNECT => net::connect(&p, a[0], a[1], a[2]),
        nr::ACCEPT => net::accept(&p, a[0], a[1], a[2], 0),
        nr::ACCEPT4 => net::accept(&p, a[0], a[1], a[2], a[3]),
        nr::SENDTO => net::sendto(&p, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::RECVFROM => net::recvfrom(&p, a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::SENDMSG => net::sendmsg(&p, a[0], a[1], a[2]),
        nr::RECVMSG => net::recvmsg(&p, a[0], a[1], a[2]),
        nr::SHUTDOWN => net::shutdown(&p, a[0], a[1]),
        nr::SENDMMSG => net::sendmmsg(&p, a[0], a[1], a[2], a[3]),
        nr::RECVMMSG => net::recvmmsg(&p, a[0], a[1], a[2], a[3]),
        nr::GETSOCKNAME => net::getsockname(&p, a[0], a[1], a[2]),
        nr::GETPEERNAME => net::getpeername(&p, a[0], a[1], a[2]),
        nr::SETSOCKOPT => net::setsockopt(&p, a[0], a[1], a[2], a[3], a[4]),
        nr::GETSOCKOPT => net::getsockopt(&p, a[0], a[1], a[2], a[3], a[4]),
        nr::MEMFD_CREATE => fs::memfd_create(&p, a[0], a[1]),
        nr::INOTIFY_INIT => inotify::init1(&p, 0),
        nr::INOTIFY_INIT1 => inotify::init1(&p, a[0]),
        nr::INOTIFY_ADD_WATCH => inotify::add_watch(&p, a[0], a[1], a[2]),
        nr::INOTIFY_RM_WATCH => inotify::rm_watch(&p, a[0], a[1]),
        nr::EVENTFD => events::eventfd(&p, a[0], 0),
        nr::EVENTFD2 => events::eventfd(&p, a[0], a[1]),
        nr::TIMERFD_CREATE => events::timerfd_create(&p, a[0], a[1]),
        nr::TIMERFD_SETTIME => events::timerfd_settime(&p, a[0], a[1], a[2], a[3]),
        nr::TIMERFD_GETTIME => events::timerfd_gettime(&p, a[0], a[1]),
        nr::EPOLL_CREATE => events::epoll_create(&p, 0),
        nr::EPOLL_CREATE1 => events::epoll_create(&p, a[0]),
        nr::EPOLL_CTL => events::epoll_ctl(&p, a[0], a[1], a[2], a[3]),
        nr::EPOLL_WAIT => events::epoll_wait(&p, a[0], a[1], a[2], a[3] as i32 as i64, 0),
        nr::EPOLL_PWAIT => events::epoll_wait(&p, a[0], a[1], a[2], a[3] as i32 as i64, a[4]),
        nr::EPOLL_PWAIT2 => {
            let ms = if a[3] == 0 {
                -1
            } else {
                let [sec, nsec]: [i64; 2] = crate::syscall::user::get(a[3])?;
                sec.max(0) * 1000 + nsec.max(0) / 1_000_000
            };
            events::epoll_wait(&p, a[0], a[1], a[2], ms, a[4])
        }

        nr::MMAP => mm::mmap(&p, a[0], a[1], a[2], a[3], a[4] as i64, a[5]),
        nr::MUNMAP => mm::munmap(&p, a[0], a[1]),
        nr::MPROTECT => mm::mprotect(&p, a[0], a[1], a[2]),
        nr::BRK => mm::brk(&p, a[0]),
        nr::MREMAP => mm::mremap(&p, a[0], a[1], a[2], a[3]),
        nr::MADVISE => mm::madvise(&p, a[0], a[1], a[2]),
        nr::MSYNC => Ok(0),
        nr::MLOCK | nr::MLOCK2 => mm::mlock(&p, a[0], a[1], true),
        nr::MUNLOCK => mm::mlock(&p, a[0], a[1], false),
        nr::MLOCKALL => mm::mlockall(&p, a[0], true),
        nr::MUNLOCKALL => mm::mlockall(&p, 0, false),
        nr::MINCORE => mm::mincore(&p, a[0], a[1], a[2]),

        nr::EXIT => misc::exit_thread(&p, a[0] as i64),
        nr::EXIT_GROUP => {
            if let Some(l) = &p.linux {
                l.counts.report();
            }
            proc::exit_current((a[0] & 0xFF) as i64)
        }
        nr::GETPID => Ok(p.pid as u64),
        nr::GETTID => Ok(misc::tid(&p)),
        nr::GETPPID => Ok(p.parent as u64),
        nr::GETUID | nr::GETEUID => Ok(p.cred.lock().uid as u64),
        nr::GETGID | nr::GETEGID => Ok(p.cred.lock().gid as u64),
        nr::GETRESUID | nr::GETRESGID => {
            let c = *p.cred.lock();
            let id = if number == nr::GETRESUID { c.uid } else { c.gid };
            for ptr in [a[0], a[1], a[2]] {
                crate::syscall::user::put(ptr, &id)?;
            }
            Ok(0)
        }
        // Becoming someone else takes being the system (no account is);
        // "becoming" who you already are is fine.
        nr::SETUID | nr::SETGID => {
            let mut c = p.cred.lock();
            let id = a[0] as u32;
            let same = if number == nr::SETUID { c.uid == id } else { c.gid == id };
            if same {
                Ok(0)
            } else if c.is_root() {
                if number == nr::SETUID {
                    c.uid = id;
                } else {
                    c.gid = id;
                }
                Ok(0)
            } else {
                Err(EPERM)
            }
        }
        nr::GETGROUPS => {
            let c = *p.cred.lock();
            let groups = c.groups();
            if a[0] == 0 {
                return Ok(groups.len() as u64);
            }
            if (a[0] as usize) < groups.len() {
                return Err(EINVAL);
            }
            for (i, g) in groups.iter().enumerate() {
                crate::syscall::user::put(a[1] + i as u64 * 4, g)?;
            }
            Ok(groups.len() as u64)
        }
        nr::GETPGRP => process::getpgid(&p, 0),
        nr::GETPGID => process::getpgid(&p, a[0]),
        nr::GETSID => process::getsid(&p, a[0]),
        nr::SETPGID => process::setpgid(&p, a[0], a[1]),
        nr::SETSID => process::setsid(&p),
        nr::SET_TID_ADDRESS => {
            if let Some(l) = &p.linux {
                l.clear_tid.lock().insert(crate::sched::current_id(), a[0]);
            }
            Ok(misc::tid(&p))
        }
        nr::CLONE => process::clone(&p, f, a[0], a[1], a[2], a[3], a[4]),
        nr::FORK => process::clone(&p, f, SIGCHLD, 0, 0, 0, 0),
        nr::VFORK => process::clone(&p, f, 0x4000 | 0x100 | SIGCHLD, 0, 0, 0, 0),
        nr::EXECVE => process::execve(&p, f, fs::AT_FDCWD, a[0], a[1], a[2]),
        nr::EXECVEAT => process::execve(&p, f, a[0], a[1], a[2], a[3]),
        nr::WAIT4 => process::wait4(&p, a[0] as i32 as i64, a[1], a[2], a[3]),
        nr::WAITID => process::waitid(&p, a[0], a[1], a[2], a[3]),
        nr::KILL => signal::kill(&p, a[0] as i32 as i64, a[1]),
        nr::SET_ROBUST_LIST => Ok(0),
        nr::RSEQ | nr::CLONE3 => Err(ENOSYS),
        nr::ARCH_PRCTL => misc::arch_prctl(a[0], a[1]),
        nr::PRCTL => misc::prctl(&p, a[0], a[1]),
        nr::FUTEX => misc::futex(a[0], a[1], a[2], a[3], a[4], a[5]),
        nr::SCHED_YIELD => {
            crate::sched::yield_now();
            Ok(0)
        }
        nr::SCHED_GETAFFINITY => misc::sched_getaffinity(a[1], a[2]),
        nr::SCHED_SETAFFINITY | nr::SCHED_GETPARAM | nr::SCHED_GETSCHEDULER | nr::SETPRIORITY => Ok(0),
        // Ordinary policies (OTHER, BATCH, IDLE: Mesa's worker threads ask
        // for IDLE) are all the scheduler's one; real-time ones need a
        // privilege programs don't have, as on Linux without RLIMIT_RTPRIO.
        // (SCHED_RESET_ON_FORK, 0x4000_0000, may come with any of them.)
        nr::SCHED_SETSCHEDULER => match a[1] & !0x4000_0000 {
            0 | 3 | 5 => Ok(0),
            1 | 2 => Err(EPERM),
            _ => Err(EINVAL),
        },
        // One priority for every policy (as SCHED_OTHER has on Linux).
        nr::SCHED_GET_PRIORITY_MAX | nr::SCHED_GET_PRIORITY_MIN => Ok(0),
        nr::SCHED_GETATTR => misc::sched_getattr(&p, a[0], a[1], a[2], a[3]),
        // As sched_setscheduler: the ordinary policies, nice values kept by nobody.
        nr::SCHED_SETATTR => {
            let [_size, policy]: [u32; 2] = crate::syscall::user::get(a[1])?;
            match policy {
                0 | 3 | 5 => Ok(0),
                1 | 2 | 6 => Err(EPERM),
                _ => Err(EINVAL),
            }
        }
        nr::GET_MEMPOLICY => misc::get_mempolicy(a[0], a[1], a[2], a[4]),
        nr::GETPRIORITY => Ok(20),
        nr::RT_SIGACTION => signal::sigaction(&p, a[0], a[1], a[2]),
        nr::RT_SIGPROCMASK => signal::sigprocmask(&p, a[0], a[1], a[2]),
        nr::RT_SIGRETURN => signal::sigreturn(&p, f),
        nr::SIGALTSTACK => signal::sigaltstack(&p, a[0], a[1]),
        nr::RT_SIGPENDING => signal::sigpending(&p, a[0]),
        nr::RT_SIGSUSPEND => signal::suspend(&p, Some(a[0])),
        nr::PAUSE => signal::suspend(&p, None),
        nr::RT_SIGTIMEDWAIT => signal::sigtimedwait(&p, a[0], a[1], a[2]),
        nr::TGKILL => signal::tgkill(&p, Some(a[0]), a[1], a[2]),
        nr::TKILL => signal::tgkill(&p, None, a[0], a[1]),

        nr::UNAME => misc::uname(a[0]),
        nr::SYSINFO => misc::sysinfo(a[0]),
        nr::GETRLIMIT => misc::prlimit(a[0], 0, a[1]),
        nr::PRLIMIT64 => misc::prlimit(a[1], a[2], a[3]),
        nr::SETRLIMIT => Ok(0),
        nr::GETRUSAGE => misc::getrusage(a[1]),
        nr::TIMES => misc::times(a[0]),
        nr::GETRANDOM => misc::getrandom(a[0], a[1]),
        nr::CLOCK_GETTIME => misc::clock_gettime(a[0], a[1]),
        nr::CLOCK_GETRES => misc::clock_getres(a[1]),
        nr::GETTIMEOFDAY => misc::gettimeofday(a[0]),
        nr::TIME => misc::time(a[0]),
        nr::NANOSLEEP => misc::nanosleep(a[0], a[1], false, 0),
        nr::CLOCK_NANOSLEEP => misc::nanosleep(a[2], a[3], a[1] & 1 != 0, a[0]),
        n if (WAVE_BASE..WAVE_BASE + 0x1000).contains(&n) => wave_call(&p, n - WAVE_BASE, a),
        _ => {
            unimplemented(f.number());
            Err(ENOSYS)
        }
    }
}

/// " (name)" of native call `n` for logs, where the ABI names its calls
/// (arm64); nothing on x86-64, whose logs show the number alone.
fn label(n: u64) -> alloc::string::String {
    match abi::native::name(n) {
        "" => alloc::string::String::new(),
        name => alloc::format!(" ({name})"),
    }
}

/// Logs a call Linux programs made that WaveOS doesn't handle, once per number.
fn unimplemented(n: u64) {
    static SEEN: Mutex<[u64; 8]> = Mutex::new([0; 8]);
    if n >= 512 {
        return;
    }
    let mut seen = SEEN.lock();
    let (word, bit) = ((n / 64) as usize, 1u64 << (n % 64));
    if seen[word] & bit == 0 {
        seen[word] |= bit;
        let name = proc::current().map(|p| p.name()).unwrap_or_default();
        log!("linux", "{name}: system call {n}{} is not supported yet (ENOSYS)", label(n));
    }
}
