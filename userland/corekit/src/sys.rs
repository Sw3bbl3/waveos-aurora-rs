//! Raw system calls.

use crate::{Error, Result};
use core::arch::asm;

#[inline(always)]
pub unsafe fn syscall6(n: usize, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        in("rdi") a1,
        in("rsi") a2,
        in("rdx") a3,
        in("r10") a4,
        in("r8") a5,
        in("r9") a6,
        lateout("rcx") _,
        lateout("r11") _,
        options(nostack)
    );
    ret
}

#[inline(always)]
pub fn call(n: usize, args: &[u64]) -> Result<u64> {
    let mut a = [0u64; 6];
    a[..args.len()].copy_from_slice(args);
    let r = unsafe { syscall6(n, a[0], a[1], a[2], a[3], a[4], a[5]) };
    if r < 0 {
        Err(Error(r))
    } else {
        Ok(r as u64)
    }
}

pub fn str_args(s: &str) -> [u64; 2] {
    [s.as_ptr() as u64, s.len() as u64]
}

/// A system report as text (`aurora_abi::report`: PCI, USB, CPU, LOG).
pub fn report(kind: u64) -> alloc::string::String {
    let mut buf = alloc::vec![0u8; 16 * 1024];
    loop {
        let Ok(n) = call(crate::abi::nr::SYS_REPORT, &[kind, buf.as_mut_ptr() as u64, buf.len() as u64]) else {
            return alloc::string::String::new();
        };
        if n as usize <= buf.len() {
            buf.truncate(n as usize);
            return alloc::string::String::from_utf8_lossy(&buf).into_owned();
        }
        buf.resize(n as usize, 0);
    }
}
