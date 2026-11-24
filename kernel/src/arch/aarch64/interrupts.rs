//! Turning interrupts on and off on the calling CPU (PSTATE.I; FIQs stay
//! masked: the kernel uses none).

use super::regs::read;

/// Whether IRQs are unmasked.
#[inline]
pub fn are_enabled() -> bool {
    read!("daif") & (1 << 7) == 0
}

#[inline]
pub fn enable() {
    // SAFETY: unmasking IRQs is what the caller asked for; the compiler
    // barrier (no `nomem`) keeps memory accesses from moving across it.
    unsafe { core::arch::asm!("msr daifclr, #2", options(nostack, preserves_flags)) };
}

#[inline]
pub fn disable() {
    // SAFETY: as `enable`.
    unsafe { core::arch::asm!("msr daifset, #2", options(nostack, preserves_flags)) };
}

/// Runs `f` with interrupts off, restoring the previous state after.
#[inline]
pub fn without_interrupts<F: FnOnce() -> R, R>(f: F) -> R {
    let was = are_enabled();
    if was {
        disable();
    }
    let r = f();
    if was {
        enable();
    }
    r
}

/// Waits for an interrupt, then enables interrupts so it is taken. `wfi`
/// returns when an interrupt is pending even while masked, so none can be
/// missed between the two.
#[inline]
pub fn enable_and_hlt() {
    // SAFETY: as `enable`.
    unsafe { core::arch::asm!("wfi", "msr daifclr, #2", options(nostack, preserves_flags)) };
}
