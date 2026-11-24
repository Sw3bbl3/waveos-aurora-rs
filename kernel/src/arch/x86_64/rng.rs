//! Hardware random numbers: RDRAND and RDSEED.

use super::cpu::{cpuid, cpuid_count};

/// 64 bits from the CPU's random number generator (RDRAND), if it has one.
pub fn random() -> Option<u64> {
    if cpuid(1).ecx & (1 << 30) == 0 {
        return None;
    }
    for _ in 0..10 {
        let mut v = 0u64;
        // SAFETY: CPUID says RDRAND exists.
        if unsafe { core::arch::x86_64::_rdrand64_step(&mut v) } == 1 {
            return Some(v);
        }
    }
    None
}

/// 64 bits of seed-grade entropy (RDSEED), if the CPU has it.
pub fn seed() -> Option<u64> {
    if cpuid(7).ebx & (1 << 18) == 0 {
        return None;
    }
    for _ in 0..10 {
        let mut v = 0u64;
        // SAFETY: CPUID says RDSEED exists.
        if unsafe { core::arch::x86_64::_rdseed64_step(&mut v) } == 1 {
            return Some(v);
        }
    }
    None
}

/// A value for the stack protector's canary, computed before anything else
/// runs: RDSEED (or RDRAND) through registers only, mixed with the TSC. No
/// address-taken locals, so this function carries no canary of its own.
#[inline(never)]
pub fn early_seed() -> u64 {
    let leaf1 = cpuid(1).ecx;
    let leaf7 = cpuid_count(7, 0).ebx;
    let mut v = super::cpu::cycles().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for _ in 0..4 {
        let (hw, ok): (u64, u8);
        // SAFETY: each instruction is used only where CPUID reports it.
        unsafe {
            if leaf7 & (1 << 18) != 0 {
                core::arch::asm!("rdseed {0}", "setc {1}", out(reg) hw, out(reg_byte) ok, options(nomem, nostack));
            } else if leaf1 & (1 << 30) != 0 {
                core::arch::asm!("rdrand {0}", "setc {1}", out(reg) hw, out(reg_byte) ok, options(nomem, nostack));
            } else {
                (hw, ok) = (0, 0);
            }
        }
        let t = super::cpu::cycles();
        v = v.rotate_left(17) ^ if ok != 0 { hw } else { 0 } ^ t.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    }
    v
}
