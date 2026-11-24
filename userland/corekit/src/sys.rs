//! Raw system calls (the conventions are in `aurora_abi`'s documentation).

use crate::{Error, Result};
use core::arch::asm;

/// Makes system call `n`.
///
/// # Safety
///
/// Arguments that the call treats as pointers must be valid for what it does
/// with them (see `aurora_abi`).
#[cfg(target_arch = "aarch64")]
#[inline(always)]
pub unsafe fn syscall6(n: usize, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: u64) -> isize {
    // The number in x8, arguments in x0-x5, the result in x0; the kernel
    // preserves every other register.
    let ret: isize;
    asm!(
        "svc #0",
        in("x8") n,
        inlateout("x0") a1 as isize => ret,
        in("x1") a2,
        in("x2") a3,
        in("x3") a4,
        in("x4") a5,
        in("x5") a6,
        options(nostack)
    );
    ret
}

/// Makes system call `n`.
///
/// # Safety
///
/// Arguments that the call treats as pointers must be valid for what it does
/// with them (see `aurora_abi`).
#[cfg(target_arch = "x86_64")]
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

/// Whether the processor has `feature`, named as `#[target_feature]` names
/// it (`"aes"` is AES with PMULL, `"sha2"` SHA-256): the RustCrypto crates
/// ask through `cpufeatures`, which `rt::start` points here. The kernel's
/// CPU report is read once, on the first question, and only by programs
/// that ask (those that use TLS).
#[cfg(target_arch = "aarch64")]
pub fn cpu_has(feature: &str) -> bool {
    use core::sync::atomic::{AtomicU32, Ordering};
    const KNOWN: u32 = 1 << 31;
    const FEATURES: [(&str, &[&str]); 4] =
        [("aes", &["aes", "pmull"]), ("sha2", &["sha256"]), ("sha3", &["sha3"]), ("crc", &["crc32"])];
    static BITS: AtomicU32 = AtomicU32::new(0);
    let mut bits = BITS.load(Ordering::Relaxed);
    if bits & KNOWN == 0 {
        let cpu = report(crate::abi::report::CPU);
        bits = KNOWN;
        for (i, (_, names)) in FEATURES.iter().enumerate() {
            if crate::abi::report::has_features(&cpu, names) {
                bits |= 1 << i;
            }
        }
        BITS.store(bits, Ordering::Relaxed);
    }
    FEATURES.iter().position(|(f, _)| *f == feature).is_some_and(|i| bits & 1 << i != 0)
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
