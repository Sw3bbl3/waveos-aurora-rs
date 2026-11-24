//! arm64 Linux: the system call table ([`table`]), the signal frame
//! ([`signal`]), CPU features and ID-register reads ([`cpu`]), and the
//! layouts and constants that differ from x86-64.
//!
//! The register convention at a system call: the number in x8, the
//! arguments in x0-x5, the result in x0; `svc #0` leaves the return address
//! in ELR, so making a call again means going back 4 bytes with the first
//! argument (`orig_x0`) put back in x0. Threads find their TLS through
//! TPIDR_EL0, which `clone` sets with `CLONE_SETTLS` (there is no
//! `arch_prctl`).

pub mod cpu;
pub mod signal;
pub mod table;

use crate::{put, Stat};

pub use table::TABLE;

/// `uname`'s `machine`, and `AT_PLATFORM`.
pub const MACHINE: &str = "aarch64";

/// Open flags that differ from x86-64 (the others are the same).
pub const O_DIRECTORY: u64 = 0o40000;
pub const O_NOFOLLOW: u64 = 0o100000;
pub const O_DIRECT: u64 = 0o200000;
pub const O_LARGEFILE: u64 = 0o400000;

/// `mmap`/`mprotect` protection bits only arm64 has: branch-target
/// guarded pages and memory tagging. The layer accepts and ignores both
/// (the C library asks for `PROT_BTI` only when `AT_HWCAP2` has BTI).
pub const PROT_BTI: u64 = 0x10;
pub const PROT_MTE: u64 = 0x20;

/// The smallest alternate signal stack `sigaltstack` accepts, and the
/// suggested size (a signal frame is about 4.6 KiB here).
pub const MINSIGSTKSZ: u64 = 5120;
pub const SIGSTKSZ: u64 = 16384;

/// `struct epoll_event` has its natural alignment on arm64.
pub const EPOLL_EVENT_SIZE: usize = 16;
pub const EPOLL_DATA_AT: usize = 8;

/// Auxiliary vector entries arm64 programs get beyond x86-64's.
pub const AT_HWCAP2: u64 = 26;
pub const AT_MINSIGSTKSZ: u64 = 51;

/// `struct stat` (the generic one).
pub const STAT_SIZE: usize = 128;

/// `s` as arm64's `struct stat`: 32-bit `st_mode`, `st_nlink` and
/// `st_blksize`, mode before nlink.
pub fn encode_stat(s: &Stat) -> [u8; STAT_SIZE] {
    let mut b = [0u8; STAT_SIZE];
    put(&mut b, 0, &s.dev.to_le_bytes());
    put(&mut b, 8, &s.ino.to_le_bytes());
    put(&mut b, 16, &s.mode.to_le_bytes());
    put(&mut b, 20, &s.nlink.to_le_bytes());
    put(&mut b, 24, &s.uid.to_le_bytes());
    put(&mut b, 28, &s.gid.to_le_bytes());
    put(&mut b, 32, &s.rdev.to_le_bytes());
    put(&mut b, 48, &s.size.to_le_bytes());
    put(&mut b, 56, &s.blksize.to_le_bytes());
    put(&mut b, 64, &s.blocks.to_le_bytes());
    for (i, (sec, nsec)) in [s.atime, s.mtime, s.ctime].into_iter().enumerate() {
        put(&mut b, 72 + i * 16, &sec.to_le_bytes());
        put(&mut b, 80 + i * 16, &(nsec as u64).to_le_bytes());
    }
    b
}

/// `clone`'s arguments as x86-64 orders them: arm64 passes `(flags, stack,
/// parent_tid, tls, child_tid)`, x86-64 `(flags, stack, parent_tid,
/// child_tid, tls)`.
pub fn clone_args(a: [u64; 6]) -> [u64; 5] {
    [a[0], a[1], a[2], a[4], a[3]]
}
