//! Reading the level of a device's interrupt pin, where we can do it
//! soundly from public documentation, so its driver need not poll the bus.
//!
//! Only AMD's FCH GPIO controller (`AMDI0030`/`AMD0030`) for now: one
//! 32-bit register per pin at its MMIO base + 4 × pin, whose bit 16
//! ("PinSts") is the pin's input level (AMD processor programming
//! references, GPIO bank registers). We only ever *read* it: nothing about
//! the pin's configuration changes.
//!
//! Intel's GPIO communities number and lay out pads differently on every
//! platform generation, so on Intel laptops the touchpad is polled instead
//! (see `hid`).

use crate::acpi::devices::AcpiDevice;
use aurora_acpi_res::resource::first_memory;

/// A pin whose input level can be read.
pub struct LevelPin {
    reg: u64,
    pub controller: &'static str,
}

impl LevelPin {
    /// The pin `pin` on GPIO controller `dev`, if we know how to read it.
    pub fn new(dev: &AcpiDevice, pin: u16) -> Option<LevelPin> {
        if !(dev.is("AMDI0030") || dev.is("AMD0030")) {
            return None;
        }
        let (base, len) = first_memory(&dev.resources)?;
        let offset = pin as u64 * 4;
        if offset + 4 > len {
            return None;
        }
        let virt = crate::mm::paging::map_mmio(base, len.max(0x1000));
        Some(LevelPin { reg: virt + offset, controller: "AMD FCH GPIO" })
    }

    /// The pin's input level (high = true).
    pub fn level(&self) -> bool {
        unsafe { (self.reg as *const u32).read_volatile() & (1 << 16) != 0 }
    }
}
