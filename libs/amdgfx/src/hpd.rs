//! Hot-plug detection: the sense bit of each HPD pin (`DC_HPD_INT_STATUS.
//! DC_HPD_SENSE`), polled. Reading it changes nothing; the driver enables
//! no interrupt.
//!
//! Which connector a pin belongs to is in the VBIOS's object table, which
//! this driver does not parse; pins are reported by number (HPD 1–6, as
//! AMD numbers them), and HPD *n* normally pairs with AUX channel *n* − 1.

use crate::platform::{Info, Layout};
use crate::regs;
use crate::Mmio;

pub const PINS: u8 = 6;

/// Bit `i` set: something is plugged into HPD pin `i` (0-based).
pub fn sense(m: &impl Mmio, info: &Info) -> u8 {
    let pins = match info.block.layout() {
        Layout::Dcn(l) => l.hpd_status,
        Layout::Dce(l) => l.hpd_status,
    };
    let mut out = 0;
    for (i, &r) in pins.iter().enumerate() {
        if r == 0 {
            continue;
        }
        let v = m.read(r);
        if v != u32::MAX && v & regs::HPD_SENSE != 0 {
            out |= 1 << i;
        }
    }
    out
}
