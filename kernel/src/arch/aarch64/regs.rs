//! System register access: `read!(NAME)` and `write!(NAME, value)`.
//!
//! Only this module and the assembly in this directory name system
//! registers; reading one has no side effect, writing one is `unsafe` at
//! the call site's discretion (the macros themselves wrap the `asm!`).

/// Reads system register `$reg` (e.g. `read!("mpidr_el1")`).
macro_rules! sysreg_read {
    ($reg:literal) => {{
        let v: u64;
        // SAFETY: reading a system register has no side effect.
        #[allow(unused_unsafe)]
        unsafe { core::arch::asm!(concat!("mrs {}, ", $reg), out(reg) v, options(nomem, nostack, preserves_flags)) };
        v
    }};
}

/// Writes system register `$reg`. The caller states why the write is safe.
macro_rules! sysreg_write {
    ($reg:literal, $v:expr) => {{
        let v: u64 = $v;
        core::arch::asm!(concat!("msr ", $reg, ", {}"), in(reg) v, options(nostack, preserves_flags));
    }};
}

pub(crate) use sysreg_read as read;
pub(crate) use sysreg_write as write;

/// Instruction synchronization barrier.
#[inline(always)]
pub fn isb() {
    // SAFETY: a barrier.
    unsafe { core::arch::asm!("isb", options(nostack, preserves_flags)) };
}

/// Data synchronization barrier over the inner shareable domain.
#[inline(always)]
pub fn dsb_ish() {
    // SAFETY: a barrier.
    unsafe { core::arch::asm!("dsb ish", options(nostack, preserves_flags)) };
}

/// Data synchronization barrier over the whole system.
#[inline(always)]
pub fn dsb_sy() {
    // SAFETY: a barrier.
    unsafe { core::arch::asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// The exception level the CPU runs at (1 for the kernel).
pub fn current_el() -> u64 {
    (read!("CurrentEL") >> 2) & 3
}
