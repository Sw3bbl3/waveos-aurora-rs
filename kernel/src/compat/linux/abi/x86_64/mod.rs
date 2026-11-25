//! x86-64 Linux programs: the layer's native ABI. Its call numbers are the
//! layer's own ([`nr`]), so a call goes straight to its implementation.
//! (`nr` is built on every architecture: it names the shared calls.)

pub mod nr;
#[cfg(target_arch = "x86_64")]
pub mod signal;

#[cfg(target_arch = "x86_64")]
pub use native::*;

#[cfg(target_arch = "x86_64")]
mod native {
    use super::super::Call;
    use crate::arch::syscall::SyscallFrame;
    use crate::proc::Process;
    use aurora_abi::err::*;
    pub use aurora_linuxabi::x86_64::{EPOLL_DATA_AT, EPOLL_EVENT_SIZE, MACHINE, MINSIGSTKSZ, O_DIRECTORY, O_NOFOLLOW};

    /// What call `n` is: the shared call with the same number.
    #[inline]
    pub fn lookup(n: u64) -> Call {
        Call::Shared(n)
    }

    /// The layer's number for native call `n` (the same).
    #[inline]
    pub fn canonical(n: u64) -> u64 {
        n
    }

    /// A call's name for logs: x86-64 has no table of names here.
    pub fn name(_n: u64) -> &'static str {
        ""
    }

    /// No call of x86-64's needs code of its own.
    pub fn own(_n: u64, _f: &mut SyscallFrame, _p: &alloc::sync::Arc<Process>) -> Result<u64, isize> {
        Err(ENOSYS)
    }

    /// `struct stat` at `ptr`.
    pub fn put_stat(ptr: u64, s: &aurora_linuxabi::Stat) -> Result<u64, isize> {
        crate::syscall::user::put(ptr, &aurora_linuxabi::x86_64::encode_stat(s)).map(|_| 0)
    }

    /// `AT_HWCAP`: `cpuid` leaf 1's EDX, as Linux gives it.
    pub fn hwcap() -> u64 {
        crate::arch::cpu::linux_hwcap()
    }

    /// Auxiliary vector entries beyond the common ones: none on x86-64.
    pub fn extra_auxv(_vdso: u64) -> alloc::vec::Vec<(u64, u64)> {
        alloc::vec::Vec::new()
    }
}
