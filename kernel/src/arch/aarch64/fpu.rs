//! FP/SIMD state for user programs.
//!
//! The kernel is built soft-float and never touches v0-v31, so only user
//! tasks (and the few SIMD kernel tasks) carry FP/SIMD state: the 32
//! 128-bit registers, FPSR and FPCR, saved and restored eagerly on every
//! switch. The layout is Linux's `user_fpsimd_state` (what a signal frame
//! carries). SVE state is not kept: SVE stays disabled for EL0.

use core::arch::global_asm;

/// Bytes of state kept: 32 × 16 + FPSR + FPCR + 8 reserved.
pub const AREA: usize = 528;

#[repr(C, align(16))]
pub struct FpuState([u8; AREA]);

// The kernel's target has no FP/SIMD; these two routines alone use them.
global_asm!(
    ".arch_extension fp",
    ".arch_extension simd",
    ".global aurora_fpsimd_save",
    "aurora_fpsimd_save:",
    "stp q0, q1, [x0, #0]",
    "stp q2, q3, [x0, #32]",
    "stp q4, q5, [x0, #64]",
    "stp q6, q7, [x0, #96]",
    "stp q8, q9, [x0, #128]",
    "stp q10, q11, [x0, #160]",
    "stp q12, q13, [x0, #192]",
    "stp q14, q15, [x0, #224]",
    "stp q16, q17, [x0, #256]",
    "stp q18, q19, [x0, #288]",
    "stp q20, q21, [x0, #320]",
    "stp q22, q23, [x0, #352]",
    "stp q24, q25, [x0, #384]",
    "stp q26, q27, [x0, #416]",
    "stp q28, q29, [x0, #448]",
    "stp q30, q31, [x0, #480]",
    "mrs x1, fpsr",
    "str w1, [x0, #512]",
    "mrs x1, fpcr",
    "str w1, [x0, #516]",
    "ret",
    ".global aurora_fpsimd_restore",
    "aurora_fpsimd_restore:",
    "ldp q0, q1, [x0, #0]",
    "ldp q2, q3, [x0, #32]",
    "ldp q4, q5, [x0, #64]",
    "ldp q6, q7, [x0, #96]",
    "ldp q8, q9, [x0, #128]",
    "ldp q10, q11, [x0, #160]",
    "ldp q12, q13, [x0, #192]",
    "ldp q14, q15, [x0, #224]",
    "ldp q16, q17, [x0, #256]",
    "ldp q18, q19, [x0, #288]",
    "ldp q20, q21, [x0, #320]",
    "ldp q22, q23, [x0, #352]",
    "ldp q24, q25, [x0, #384]",
    "ldp q26, q27, [x0, #416]",
    "ldp q28, q29, [x0, #448]",
    "ldp q30, q31, [x0, #480]",
    "ldr w1, [x0, #512]",
    "msr fpsr, x1",
    "ldr w1, [x0, #516]",
    "msr fpcr, x1",
    "ret",
);

extern "C" {
    fn aurora_fpsimd_save(area: *mut u8);
    fn aurora_fpsimd_restore(area: *const u8);
}

impl FpuState {
    /// The reset state: all registers zero, round to nearest, no traps.
    pub fn new() -> FpuState {
        FpuState([0; AREA])
    }

    /// The bytes a Linux signal frame carries (`user_fpsimd_state`).
    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..size()]
    }

    /// A state from bytes a program supplied (a signal frame); FPCR's
    /// reserved bits cleared.
    pub fn from_bytes(bytes: &[u8]) -> FpuState {
        let mut s = FpuState::new();
        let n = bytes.len().min(size());
        s.0[..n].copy_from_slice(&bytes[..n]);
        // FPCR: AHP, DN, FZ, RMode, FZ16 and the trap enables are defined.
        let fpcr = u32::from_le_bytes(s.0[516..520].try_into().unwrap()) & 0x07F7_9F00;
        s.0[516..520].copy_from_slice(&fpcr.to_le_bytes());
        s
    }

    #[inline]
    pub fn save(&mut self) {
        // SAFETY: FP/SIMD access is enabled at EL1 (`init`); the area is
        // 528 bytes, 16-aligned.
        unsafe { aurora_fpsimd_save(self.0.as_mut_ptr()) }
    }

    #[inline]
    pub fn restore(&self) {
        // SAFETY: as `save`.
        unsafe { aurora_fpsimd_restore(self.0.as_ptr()) }
    }
}

impl Default for FpuState {
    fn default() -> FpuState {
        FpuState::new()
    }
}

/// Bytes of state saved.
pub fn size() -> usize {
    AREA
}

/// x86's XSAVE component mask has no Arm equivalent.
pub fn xfeatures() -> u64 {
    0
}

/// Lets EL0 and EL1 use FP/SIMD without trapping (CPACR_EL1.FPEN = 0b11).
/// The kernel itself does not (soft-float); user state is switched eagerly.
pub fn init() {
    use super::regs::{self, read};
    let cpacr = read!("cpacr_el1");
    // SAFETY: only enables FP/SIMD register access.
    unsafe { regs::write!("cpacr_el1", cpacr | 3 << 20) };
    regs::isb();
}
