//! The cycle counter as a clock: the generic timer's virtual count, whose
//! rate the firmware set in CNTFRQ_EL0 (62.5 MHz under QEMU's TCG, 24 MHz
//! on Apple M-series hosts under HVF). Nothing to calibrate.

use super::regs::read;

/// What the clock code calls the counter.
pub const NAME: &str = "generic timer";

/// The counter's rate in Hz.
pub fn counter_hz() -> Option<u64> {
    let hz = read!("cntfrq_el0") & 0xFFFF_FFFF;
    (hz != 0).then_some(hz)
}
