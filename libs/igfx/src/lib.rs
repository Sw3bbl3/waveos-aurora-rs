//! Intel integrated graphics: the display engine of Skylake through Raptor
//! Lake (display versions 9 to 13), as far as WaveOS drives it.
//!
//! Everything here is register programming expressed over [`Mmio`], so it
//! runs on real hardware in the kernel (`kernel/src/drivers/gpu/intel`) and
//! against a simulated register file on the host ([`mock`]):
//!
//! - [`platform`]: PCI device ids to platforms and display versions, and
//!   what each platform has (pipes, ports, south display, backlight).
//! - [`regs`]: register offsets and fields, by their names in Intel's
//!   Programmer's Reference Manuals.
//! - [`timing`]: transcoder timing registers to and from timings.
//! - [`takeover`]: reads back what the firmware (GOP) programmed — active
//!   pipes, transcoders, ports, timings, the primary plane — and decides
//!   whether the driver may take it over.
//! - [`plane`]: primary and cursor plane register values.
//! - [`ddb`]: display buffer (DDB) and watermark arithmetic for giving the
//!   cursor plane room next to a firmware-configured primary plane.
//! - [`gtt`]: the global GTT address-space allocator and PTE format.
//! - [`gmbus`]: the GMBUS I²C controller (DDC/EDID for HDMI and DVI).
//! - [`aux`]: the DisplayPort AUX channel: message framing and the
//!   controller, native and I²C-over-AUX transfers (EDID for DP/eDP).
//! - [`dp`]: DPCD, link rate and lane selection, M/N values, link training.
//! - [`dpll`]: Skylake-class DPLL divider computation (HDMI) and DP link
//!   rate codes.
//! - [`backlight`]: the PWM backlight of the internal panel.
//! - [`modeset`]: Skylake-class full mode set sequences (only used with the
//!   `intel=modeset` boot option).
//! - [`mock`]: a simulated display engine for tests.
//!
//! None of this has run on real hardware yet. Register details that could
//! not be confirmed from the public documentation are marked `UNCONFIRMED`
//! where they are defined.

#![no_std]

extern crate alloc;

pub mod aux;
pub mod backlight;
pub mod ddb;
pub mod dp;
pub mod dpll;
pub mod gmbus;
pub mod gtt;
pub mod mock;
pub mod modeset;
pub mod plane;
pub mod platform;
pub mod regs;
pub mod takeover;
pub mod timing;

/// Register access to the display engine (BAR 0 of the GPU), plus the
/// short waits programming sequences need.
pub trait Mmio {
    fn read(&self, reg: u32) -> u32;
    fn write(&self, reg: u32, value: u32);
    /// Busy-waits (or sleeps) at least `us` microseconds.
    fn delay_us(&self, us: u32);

    fn modify(&self, reg: u32, clear: u32, set: u32) {
        let v = self.read(reg);
        self.write(reg, (v & !clear) | set);
    }

    /// Polls until `(read(reg) & mask) == want`, for at most `timeout_us`.
    fn wait_for(&self, reg: u32, mask: u32, want: u32, timeout_us: u32) -> bool {
        let mut waited = 0;
        loop {
            if self.read(reg) & mask == want {
                return true;
            }
            if waited >= timeout_us {
                return false;
            }
            let step = if waited < 100 { 10 } else { 100 };
            self.delay_us(step);
            waited += step;
        }
    }
}

impl<M: Mmio + ?Sized> Mmio for &M {
    fn read(&self, reg: u32) -> u32 {
        (**self).read(reg)
    }
    fn write(&self, reg: u32, value: u32) {
        (**self).write(reg, value)
    }
    fn delay_us(&self, us: u32) {
        (**self).delay_us(us)
    }
}
