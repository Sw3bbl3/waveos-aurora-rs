//! Hardware random numbers: RNDR and RNDRRS (FEAT_RNG), where present.

use super::regs::read;

fn has_rndr() -> bool {
    (read!("id_aa64isar0_el1") >> 60) & 0xF != 0
}

/// Reads RNDR (`rrs`: false) or RNDRRS (reseeded first: true). The
/// instruction sets NZCV to 0b0100 on failure.
#[inline(always)]
fn rndr(reseed: bool) -> Option<u64> {
    let (v, nzcv): (u64, u64);
    // SAFETY: used only where ID_AA64ISAR0_EL1.RNDR says it exists. The
    // registers are named by encoding (s3_3_c2_c4_0/1), which any
    // assembler accepts.
    unsafe {
        if reseed {
            core::arch::asm!("mrs {0}, s3_3_c2_c4_1", "mrs {1}, nzcv", out(reg) v, out(reg) nzcv, options(nomem, nostack));
        } else {
            core::arch::asm!("mrs {0}, s3_3_c2_c4_0", "mrs {1}, nzcv", out(reg) v, out(reg) nzcv, options(nomem, nostack));
        }
    }
    (nzcv & (1 << 30) == 0).then_some(v)
}

/// 64 bits from the CPU's random number generator (RNDR), if it has one.
pub fn random() -> Option<u64> {
    if !has_rndr() {
        return None;
    }
    (0..10).find_map(|_| rndr(false))
}

/// 64 bits of seed-grade entropy (RNDRRS), if the CPU has it.
pub fn seed() -> Option<u64> {
    if !has_rndr() {
        return None;
    }
    (0..10).find_map(|_| rndr(true))
}

/// A value for the stack protector's canary, before anything else runs:
/// RNDRRS where present, mixed with the counter. Registers only (no
/// address-taken locals, so no canary of its own).
#[inline(never)]
pub fn early_seed() -> u64 {
    let hw = has_rndr();
    let mut v = super::cpu::cycles().wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for _ in 0..4 {
        let r = if hw { rndr(true).unwrap_or(0) } else { 0 };
        let t = super::cpu::cycles();
        v = v.rotate_left(17) ^ r ^ t.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    }
    v
}
