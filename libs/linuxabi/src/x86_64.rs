//! x86-64 Linux: the layer's native ABI. Its call numbers are the layer's
//! own (the kernel's `compat::linux::abi::x86_64::nr`); here are the
//! layouts and constants that differ from arm64.

use crate::{put, Stat};

/// `uname`'s `machine`, and `AT_PLATFORM`.
pub const MACHINE: &str = "x86_64";

/// Open flags that differ between architectures.
pub const O_DIRECT: u64 = 0o40000;
pub const O_LARGEFILE: u64 = 0o100000;
pub const O_DIRECTORY: u64 = 0o200000;
pub const O_NOFOLLOW: u64 = 0o400000;

/// The smallest alternate signal stack `sigaltstack` accepts.
pub const MINSIGSTKSZ: u64 = 2048;

/// `struct epoll_event` is packed on x86-64: `u32 events`, `u64 data`.
pub const EPOLL_EVENT_SIZE: usize = 12;
pub const EPOLL_DATA_AT: usize = 4;

/// `struct stat`.
pub const STAT_SIZE: usize = 144;

/// `s` as x86-64's `struct stat`: `st_nlink` and `st_blksize` are 64-bit,
/// mode before uid.
pub fn encode_stat(s: &Stat) -> [u8; STAT_SIZE] {
    let mut b = [0u8; STAT_SIZE];
    put(&mut b, 0, &s.dev.to_le_bytes());
    put(&mut b, 8, &s.ino.to_le_bytes());
    put(&mut b, 16, &(s.nlink as u64).to_le_bytes());
    put(&mut b, 24, &s.mode.to_le_bytes());
    put(&mut b, 28, &s.uid.to_le_bytes());
    put(&mut b, 32, &s.gid.to_le_bytes());
    put(&mut b, 40, &s.rdev.to_le_bytes());
    put(&mut b, 48, &s.size.to_le_bytes());
    put(&mut b, 56, &(s.blksize as u64).to_le_bytes());
    put(&mut b, 64, &s.blocks.to_le_bytes());
    for (i, (sec, nsec)) in [s.atime, s.mtime, s.ctime].into_iter().enumerate() {
        put(&mut b, 72 + i * 16, &sec.to_le_bytes());
        put(&mut b, 80 + i * 16, &(nsec as u64).to_le_bytes());
    }
    b
}
