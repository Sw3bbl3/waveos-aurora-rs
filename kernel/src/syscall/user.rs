//! Safe access to user memory from system calls.
//!
//! Every pointer from user space is checked against the calling process's
//! page tables (present + user, and writable when written) before the kernel
//! touches it. Processes are single-threaded, so a validated range cannot be
//! unmapped while the syscall runs.

use crate::proc;
use alloc::string::String;
use aurora_abi::err::*;

const MAX_STR: u64 = 4096;
const MAX_BUF: u64 = 64 << 20;

fn check(ptr: u64, len: u64, write: bool) -> Result<(), isize> {
    if len > MAX_BUF {
        return Err(EINVAL);
    }
    let p = proc::current().ok_or(EPERM)?;
    let guard = p.aspace.lock();
    let aspace = guard.as_ref().ok_or(ESRCH)?;
    if aspace.check(ptr, len, write) {
        Ok(())
    } else {
        Err(EFAULT)
    }
}

pub fn slice<'a>(ptr: u64, len: u64) -> Result<&'a [u8], isize> {
    if len == 0 {
        return Ok(&[]);
    }
    check(ptr, len, false)?;
    Ok(unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) })
}

pub fn slice_mut<'a>(ptr: u64, len: u64) -> Result<&'a mut [u8], isize> {
    if len == 0 {
        return Ok(&mut []);
    }
    check(ptr, len, true)?;
    Ok(unsafe { core::slice::from_raw_parts_mut(ptr as *mut u8, len as usize) })
}

pub fn str<'a>(ptr: u64, len: u64) -> Result<&'a str, isize> {
    if len > MAX_STR {
        return Err(ENAMETOOLONG);
    }
    core::str::from_utf8(slice(ptr, len)?).map_err(|_| EINVAL)
}

/// A path argument, resolved against the process's working directory.
pub fn path(ptr: u64, len: u64) -> Result<String, isize> {
    let raw = str(ptr, len)?;
    if raw.is_empty() {
        return Err(ENOENT);
    }
    let cwd = proc::current().map(|p| p.cwd.lock().clone()).unwrap_or_else(|| String::from("/"));
    Ok(crate::fs::resolve(&cwd, raw))
}

/// Copies a plain-data value out to user memory.
pub fn put<T: Copy>(ptr: u64, value: &T) -> Result<(), isize> {
    let size = core::mem::size_of::<T>() as u64;
    let dst = slice_mut(ptr, size)?;
    let src = unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size as usize) };
    dst.copy_from_slice(src);
    Ok(())
}

/// Copies a plain-data value in from user memory.
pub fn get<T: Copy>(ptr: u64) -> Result<T, isize> {
    let size = core::mem::size_of::<T>() as u64;
    let src = slice(ptr, size)?;
    Ok(unsafe { (src.as_ptr() as *const T).read_unaligned() })
}
