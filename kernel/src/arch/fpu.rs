//! SIMD state for user programs.
//!
//! The kernel is built soft-float and never touches x87/SSE registers, so only
//! user tasks carry SIMD state. It is saved and restored eagerly on every
//! switch between user tasks (FXSAVE covers x87 and SSE, all user code uses).

use core::arch::asm;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// A 512-byte FXSAVE area.
#[repr(C, align(16))]
pub struct FpuState([u8; 512]);

impl FpuState {
    /// The power-on state: all exceptions masked, round to nearest.
    pub fn new() -> FpuState {
        let mut s = FpuState([0; 512]);
        s.0[0..2].copy_from_slice(&0x037Fu16.to_le_bytes()); // FCW
        s.0[24..28].copy_from_slice(&0x1F80u32.to_le_bytes()); // MXCSR
        s.0[28..32].copy_from_slice(&0xFFFFu32.to_le_bytes()); // MXCSR_MASK
        s
    }

    #[inline]
    pub fn save(&mut self) {
        unsafe { asm!("fxsave64 [{}]", in(reg) self.0.as_mut_ptr(), options(nostack)) };
    }

    #[inline]
    pub fn restore(&self) {
        unsafe { asm!("fxrstor64 [{}]", in(reg) self.0.as_ptr(), options(nostack)) };
    }
}

/// Enables x87/SSE for ring 3 on this CPU.
pub fn init() {
    unsafe {
        Cr0::update(|f| {
            f.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            f.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR);
        });
        Cr4::update(|f| f.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE));
        asm!("fninit", options(nomem, nostack));
    }
}
