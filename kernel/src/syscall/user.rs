//! Safe access to user memory from system calls.
//!
//! Every pointer from user space is checked against the calling process's
//! page tables (present + user, and writable when written), and the data is
//! then *copied* in or out by `user_copy`. Another thread of the process may
//! unmap the memory at any moment; if that makes the copy fault, the page
//! fault handler resumes `user_copy` at its fault exit (the fixup below) and
//! the system call returns `EFAULT` — the kernel never touches user memory
//! outside this one routine.

use crate::proc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::arch::global_asm;
use core::ops::{Deref, DerefMut};

const MAX_STR: u64 = 4096;
/// Largest single transfer; bigger reads and writes are shortened (callers loop).
pub const MAX_BUF: u64 = 1 << 20;

global_asm!(
    ".global aurora_user_copy",
    "aurora_user_copy:",
    "mov rcx, rdx",
    ".global aurora_user_copy_insn",
    "aurora_user_copy_insn:",
    "rep movsb",
    "xor eax, eax",
    "ret",
    ".global aurora_user_copy_fault",
    "aurora_user_copy_fault:",
    "mov eax, 1",
    "ret",
);

extern "sysv64" {
    /// Copies `len` bytes; returns 0, or 1 if a page fault cut it short.
    fn aurora_user_copy(dst: *mut u8, src: *const u8, len: usize) -> u32;
    static aurora_user_copy_insn: u8;
    static aurora_user_copy_fault: u8;
}

/// Called by the page fault handler for a kernel-mode fault at `rip`:
/// returns where to resume if the fault happened inside `user_copy`.
pub fn fixup(rip: u64, addr: u64) -> Option<u64> {
    let insn = unsafe { &aurora_user_copy_insn as *const u8 as u64 };
    (rip == insn && addr < aurora_abi::layout::USER_END).then(|| unsafe { &aurora_user_copy_fault as *const u8 as u64 })
}

fn check(ptr: u64, len: u64, write: bool) -> Result<(), isize> {
    let p = proc::current().ok_or(EPERM)?;
    let guard = p.aspace.lock();
    let aspace = guard.as_ref().ok_or(ESRCH)?;
    if aspace.check(ptr, len, write) {
        Ok(())
    } else {
        Err(EFAULT)
    }
}

fn copy_in(dst: &mut [u8], ptr: u64) -> Result<(), isize> {
    if dst.is_empty() {
        return Ok(());
    }
    check(ptr, dst.len() as u64, false)?;
    match unsafe { aurora_user_copy(dst.as_mut_ptr(), ptr as *const u8, dst.len()) } {
        0 => Ok(()),
        _ => Err(EFAULT),
    }
}

fn copy_out(ptr: u64, src: &[u8]) -> Result<(), isize> {
    if src.is_empty() {
        return Ok(());
    }
    check(ptr, src.len() as u64, true)?;
    match unsafe { aurora_user_copy(ptr as *mut u8, src.as_ptr(), src.len()) } {
        0 => Ok(()),
        _ => Err(EFAULT),
    }
}

/// Copies `len` bytes in from user memory (at most `MAX_BUF`).
pub fn slice(ptr: u64, len: u64) -> Result<Vec<u8>, isize> {
    let mut buf = vec![0u8; len.min(MAX_BUF) as usize];
    copy_in(&mut buf, ptr)?;
    Ok(buf)
}

/// A kernel buffer standing in for user memory the kernel fills; only the
/// part passed to [`UserOut::commit`] is copied out.
pub struct UserOut {
    ptr: u64,
    buf: Vec<u8>,
}

impl UserOut {
    pub fn commit(&self, n: usize) -> Result<(), isize> {
        copy_out(self.ptr, &self.buf[..n.min(self.buf.len())])
    }
}

impl Deref for UserOut {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.buf
    }
}

impl DerefMut for UserOut {
    fn deref_mut(&mut self) -> &mut [u8] {
        &mut self.buf
    }
}

/// A buffer of up to `len` bytes (at most `MAX_BUF`) to fill for user memory at `ptr`.
pub fn slice_mut(ptr: u64, len: u64) -> Result<UserOut, isize> {
    let len = len.min(MAX_BUF);
    check(ptr, len, true)?;
    Ok(UserOut { ptr, buf: vec![0u8; len as usize] })
}

/// Checks that `len` bytes at `ptr` are writable (without copying).
pub fn writable(ptr: u64, len: u64) -> Result<(), isize> {
    check(ptr, len, true)
}

pub fn str(ptr: u64, len: u64) -> Result<String, isize> {
    if len > MAX_STR {
        return Err(ENAMETOOLONG);
    }
    String::from_utf8(slice(ptr, len)?).map_err(|_| EINVAL)
}

/// A path argument, resolved against the process's working directory.
pub fn path(ptr: u64, len: u64) -> Result<String, isize> {
    let raw = str(ptr, len)?;
    if raw.is_empty() {
        return Err(ENOENT);
    }
    let cwd = proc::current().map(|p| p.cwd.lock().clone()).unwrap_or_else(|| String::from("/"));
    Ok(crate::fs::resolve(&cwd, &raw))
}

/// Copies a plain-data value out to user memory.
pub fn put<T: Copy>(ptr: u64, value: &T) -> Result<(), isize> {
    let size = core::mem::size_of::<T>();
    let src = unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size) };
    copy_out(ptr, src)
}

/// Copies a plain-data value in from user memory.
pub fn get<T: Copy>(ptr: u64) -> Result<T, isize> {
    let mut v = core::mem::MaybeUninit::<T>::uninit();
    let dst = unsafe { core::slice::from_raw_parts_mut(v.as_mut_ptr() as *mut u8, core::mem::size_of::<T>()) };
    copy_in(dst, ptr)?;
    Ok(unsafe { v.assume_init() })
}

/// Reads a user word that was validated earlier (e.g. a futex), without
/// taking the address-space lock — safe to call under a spinlock. A page
/// unmapped since the check yields `EFAULT`.
pub fn read_u32(addr: u64) -> Result<u32, isize> {
    if addr.checked_add(4).is_none_or(|end| end > aurora_abi::layout::USER_END) {
        return Err(EFAULT);
    }
    let mut v = 0u32;
    match unsafe { aurora_user_copy(&mut v as *mut u32 as *mut u8, addr as *const u8, 4) } {
        0 => Ok(v),
        _ => Err(EFAULT),
    }
}

/// Writes a user word that was validated earlier; see [`read_u32`].
pub fn write_u32(addr: u64, v: u32) -> Result<(), isize> {
    if addr.checked_add(4).is_none_or(|end| end > aurora_abi::layout::USER_END) {
        return Err(EFAULT);
    }
    match unsafe { aurora_user_copy(addr as *mut u8, &v as *const u32 as *const u8, 4) } {
        0 => Ok(()),
        _ => Err(EFAULT),
    }
}
