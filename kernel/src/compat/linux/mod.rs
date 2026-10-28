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

pub mod exec;
pub mod fs;
mod misc;
mod mm;

use crate::arch::syscall::SyscallFrame;
use crate::proc;
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use aurora_abi::err::*;

pub use fs::Special;

/// What a Linux process keeps beyond a WaveOS one.
pub struct Linux {
    pub mm: Mutex<mm::Memory>,
    /// Each thread's `set_tid_address` word (by task), cleared and woken
    /// when the thread exits: how `pthread_join` learns of it.
    pub clear_tid: Mutex<BTreeMap<u64, u64>>,
}

impl Linux {
    pub fn new(brk: u64) -> Linux {
        Linux { mm: Mutex::new(mm::Memory::new(brk)), clear_tid: Mutex::new(BTreeMap::new()) }
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

/// Linux system call numbers (x86-64).
#[allow(dead_code)] // the table grows milestone by milestone
mod nr {
    pub const READ: u64 = 0;
    pub const WRITE: u64 = 1;
    pub const OPEN: u64 = 2;
    pub const CLOSE: u64 = 3;
    pub const STAT: u64 = 4;
    pub const FSTAT: u64 = 5;
    pub const LSTAT: u64 = 6;
    pub const POLL: u64 = 7;
    pub const LSEEK: u64 = 8;
    pub const MMAP: u64 = 9;
    pub const MPROTECT: u64 = 10;
    pub const MUNMAP: u64 = 11;
    pub const BRK: u64 = 12;
    pub const RT_SIGACTION: u64 = 13;
    pub const RT_SIGPROCMASK: u64 = 14;
    pub const IOCTL: u64 = 16;
    pub const PREAD64: u64 = 17;
    pub const PWRITE64: u64 = 18;
    pub const READV: u64 = 19;
    pub const WRITEV: u64 = 20;
    pub const ACCESS: u64 = 21;
    pub const PIPE: u64 = 22;
    pub const SCHED_YIELD: u64 = 24;
    pub const MREMAP: u64 = 25;
    pub const MSYNC: u64 = 26;
    pub const MADVISE: u64 = 28;
    pub const DUP: u64 = 32;
    pub const DUP2: u64 = 33;
    pub const NANOSLEEP: u64 = 35;
    pub const GETPID: u64 = 39;
    pub const CLONE: u64 = 56;
    pub const EXIT: u64 = 60;
    pub const UNAME: u64 = 63;
    pub const FCNTL: u64 = 72;
    pub const FLOCK: u64 = 73;
    pub const FSYNC: u64 = 74;
    pub const FDATASYNC: u64 = 75;
    pub const TRUNCATE: u64 = 76;
    pub const FTRUNCATE: u64 = 77;
    pub const GETCWD: u64 = 79;
    pub const CHDIR: u64 = 80;
    pub const FCHDIR: u64 = 81;
    pub const RENAME: u64 = 82;
    pub const MKDIR: u64 = 83;
    pub const RMDIR: u64 = 84;
    pub const CREAT: u64 = 85;
    pub const UNLINK: u64 = 87;
    pub const READLINK: u64 = 89;
    pub const CHMOD: u64 = 90;
    pub const FCHMOD: u64 = 91;
    pub const CHOWN: u64 = 92;
    pub const FCHOWN: u64 = 93;
    pub const LCHOWN: u64 = 94;
    pub const UMASK: u64 = 95;
    pub const GETTIMEOFDAY: u64 = 96;
    pub const GETRLIMIT: u64 = 97;
    pub const GETRUSAGE: u64 = 98;
    pub const SYSINFO: u64 = 99;
    pub const TIMES: u64 = 100;
    pub const GETUID: u64 = 102;
    pub const GETGID: u64 = 104;
    pub const SETUID: u64 = 105;
    pub const SETGID: u64 = 106;
    pub const GETEUID: u64 = 107;
    pub const GETEGID: u64 = 108;
    pub const SETPGID: u64 = 109;
    pub const GETPPID: u64 = 110;
    pub const GETPGRP: u64 = 111;
    pub const SETSID: u64 = 112;
    pub const GETGROUPS: u64 = 115;
    pub const GETRESUID: u64 = 118;
    pub const GETRESGID: u64 = 120;
    pub const GETPGID: u64 = 121;
    pub const GETSID: u64 = 124;
    pub const SIGALTSTACK: u64 = 131;
    pub const STATFS: u64 = 137;
    pub const FSTATFS: u64 = 138;
    pub const GETPRIORITY: u64 = 140;
    pub const SETPRIORITY: u64 = 141;
    pub const SCHED_GETPARAM: u64 = 143;
    pub const SCHED_GETSCHEDULER: u64 = 145;
    pub const PRCTL: u64 = 157;
    pub const ARCH_PRCTL: u64 = 158;
    pub const SETRLIMIT: u64 = 160;
    pub const SYNC: u64 = 162;
    pub const GETTID: u64 = 186;
    pub const TIME: u64 = 201;
    pub const FUTEX: u64 = 202;
    pub const SCHED_SETAFFINITY: u64 = 203;
    pub const SCHED_GETAFFINITY: u64 = 204;
    pub const GETDENTS64: u64 = 217;
    pub const SET_TID_ADDRESS: u64 = 218;
    pub const FADVISE64: u64 = 221;
    pub const CLOCK_GETTIME: u64 = 228;
    pub const CLOCK_GETRES: u64 = 229;
    pub const CLOCK_NANOSLEEP: u64 = 230;
    pub const EXIT_GROUP: u64 = 231;
    pub const TGKILL: u64 = 234;
    pub const UTIMES: u64 = 235;
    pub const OPENAT: u64 = 257;
    pub const MKDIRAT: u64 = 258;
    pub const FCHOWNAT: u64 = 260;
    pub const NEWFSTATAT: u64 = 262;
    pub const UNLINKAT: u64 = 263;
    pub const RENAMEAT: u64 = 264;
    pub const READLINKAT: u64 = 267;
    pub const FCHMODAT: u64 = 268;
    pub const FACCESSAT: u64 = 269;
    pub const PPOLL: u64 = 271;
    pub const SET_ROBUST_LIST: u64 = 273;
    pub const UTIMENSAT: u64 = 280;
    pub const FALLOCATE: u64 = 285;
    pub const DUP3: u64 = 292;
    pub const PIPE2: u64 = 293;
    pub const PRLIMIT64: u64 = 302;
    pub const RENAMEAT2: u64 = 316;
    pub const GETRANDOM: u64 = 318;
    pub const STATX: u64 = 332;
    pub const RSEQ: u64 = 334;
    pub const CLONE3: u64 = 435;
    pub const FACCESSAT2: u64 = 439;
}

/// The uid and gid every Linux program runs as.
pub const UID: u64 = 1000;

/// Entry for a Linux process's system call (from `syscall::dispatch`).
pub fn dispatch(f: &mut SyscallFrame) -> Result<u64, isize> {
    let a = [f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9];
    let p = proc::current().ok_or(EPERM)?;
    match f.rax {
        nr::READ => fs::read(&p, a[0], a[1], a[2]),
        nr::WRITE => fs::write(&p, a[0], a[1], a[2]),
        nr::OPEN => fs::openat(&p, fs::AT_FDCWD, a[0], a[1], a[2]),
        nr::CREAT => fs::openat(&p, fs::AT_FDCWD, a[0], fs::O_CREAT | fs::O_WRONLY | fs::O_TRUNC, a[1]),
        nr::OPENAT => fs::openat(&p, a[0], a[1], a[2], a[3]),
        nr::CLOSE => fs::close(&p, a[0]),
        nr::STAT | nr::LSTAT => fs::stat_path(&p, fs::AT_FDCWD, a[0], a[1], 0),
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
        nr::MKDIR => fs::mkdirat(&p, fs::AT_FDCWD, a[0]),
        nr::MKDIRAT => fs::mkdirat(&p, a[0], a[1]),
        nr::RMDIR => fs::unlinkat(&p, fs::AT_FDCWD, a[0], fs::AT_REMOVEDIR),
        nr::UNLINK => fs::unlinkat(&p, fs::AT_FDCWD, a[0], 0),
        nr::UNLINKAT => fs::unlinkat(&p, a[0], a[1], a[2]),
        nr::RENAME => fs::renameat(&p, fs::AT_FDCWD, a[0], fs::AT_FDCWD, a[1]),
        nr::RENAMEAT | nr::RENAMEAT2 => fs::renameat(&p, a[0], a[1], a[2], a[3]),
        nr::READLINK => fs::readlinkat(&p, fs::AT_FDCWD, a[0], a[1], a[2]),
        nr::READLINKAT => fs::readlinkat(&p, a[0], a[1], a[2], a[3]),
        nr::TRUNCATE => fs::truncate(&p, a[0], a[1]),
        nr::FTRUNCATE => fs::ftruncate(&p, a[0], a[1]),
        nr::STATFS => fs::statfs(a[1]),
        nr::FSTATFS => fs::statfs(a[1]),
        nr::FSYNC | nr::FDATASYNC | nr::FLOCK | nr::FADVISE64 | nr::FALLOCATE => {
            p.fd(a[0] as usize)?;
            Ok(0)
        }
        nr::SYNC => {
            crate::fs::sync_all();
            Ok(0)
        }
        // Files have no owners or permission bits on WaveOS: accepted, and nothing changes.
        nr::CHMOD | nr::FCHMOD | nr::CHOWN | nr::FCHOWN | nr::LCHOWN | nr::FCHOWNAT | nr::FCHMODAT => Ok(0),
        nr::UTIMES | nr::UTIMENSAT => Ok(0),
        nr::UMASK => Ok(0o022),
        nr::POLL => fs::poll(&p, a[0], a[1], a[2] as i64),
        nr::PPOLL => fs::poll(&p, a[0], a[1], if a[2] == 0 { -1 } else { 0 }),

        nr::MMAP => mm::mmap(&p, a[0], a[1], a[2], a[3], a[4] as i64, a[5]),
        nr::MUNMAP => mm::munmap(&p, a[0], a[1]),
        nr::MPROTECT => mm::mprotect(&p, a[0], a[1], a[2]),
        nr::BRK => mm::brk(&p, a[0]),
        nr::MREMAP => mm::mremap(&p, a[0], a[1], a[2], a[3]),
        nr::MADVISE => mm::madvise(&p, a[0], a[1], a[2]),
        nr::MSYNC => Ok(0),

        nr::EXIT => misc::exit_thread(&p, a[0] as i64),
        nr::EXIT_GROUP => proc::exit_current((a[0] & 0xFF) as i64),
        nr::GETPID => Ok(p.pid as u64),
        nr::GETTID => Ok(misc::tid(&p)),
        nr::GETPPID => Ok(p.parent as u64),
        nr::GETUID | nr::GETEUID | nr::GETGID | nr::GETEGID => Ok(UID),
        nr::GETRESUID | nr::GETRESGID => {
            for ptr in [a[0], a[1], a[2]] {
                crate::syscall::user::put(ptr, &(UID as u32))?;
            }
            Ok(0)
        }
        nr::SETUID | nr::SETGID => Ok(0),
        nr::GETGROUPS => Ok(0),
        nr::GETPGRP | nr::GETPGID | nr::GETSID => Ok(p.pid as u64),
        nr::SETPGID => Ok(0),
        nr::SETSID => Ok(p.pid as u64),
        nr::SET_TID_ADDRESS => {
            if let Some(l) = &p.linux {
                l.clear_tid.lock().insert(crate::sched::current_id(), a[0]);
            }
            Ok(misc::tid(&p))
        }
        nr::CLONE => misc::clone(&p, f, a[0], a[1], a[2], a[3], a[4]),
        nr::SET_ROBUST_LIST => Ok(0),
        nr::RSEQ | nr::CLONE3 => Err(ENOSYS),
        nr::ARCH_PRCTL => misc::arch_prctl(a[0], a[1]),
        nr::PRCTL => misc::prctl(&p, a[0], a[1]),
        nr::FUTEX => misc::futex(a[0], a[1], a[2], a[3]),
        nr::SCHED_YIELD => {
            crate::sched::yield_now();
            Ok(0)
        }
        nr::SCHED_GETAFFINITY => misc::sched_getaffinity(a[1], a[2]),
        nr::SCHED_SETAFFINITY | nr::SCHED_GETPARAM | nr::SCHED_GETSCHEDULER | nr::SETPRIORITY => Ok(0),
        nr::GETPRIORITY => Ok(20),
        nr::RT_SIGACTION | nr::RT_SIGPROCMASK | nr::SIGALTSTACK => misc::signals_stub(a),
        nr::TGKILL => misc::tgkill(&p, a[1], a[2]),

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
        n => {
            unimplemented(n);
            Err(ENOSYS)
        }
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
        let name = proc::current().map(|p| p.name.clone()).unwrap_or_default();
        log!("linux", "{name}: system call {n} is not supported yet (ENOSYS)");
    }
}
