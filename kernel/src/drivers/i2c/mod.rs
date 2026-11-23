//! I²C: laptops' built-in touchpads and touch screens (HID over I²C).
//!
//! Most laptops since about 2016 wire the touchpad to an I²C controller in
//! the chipset rather than to the PS/2 port, and only ACPI says where it is:
//!
//! 1. The ACPI task (`acpi::devices`) finds `PNP0C50` devices, their
//!    `I2cSerialBus` connection (controller, address, speed), their
//!    interrupt (`Interrupt` or `GpioInt`) and HID descriptor register
//!    (`_DSM`), and the controllers, and calls [`start`].
//! 2. The `i2c` task brings up each controller ([`designware`]: Intel LPSS
//!    PCI functions or ACPI-described Intel and AMD controllers).
//! 3. Each device gets a task ([`hid`]) that speaks HID over I²C and feeds
//!    the input queue through the same gesture engine as USB touchpads.
//!
//! The controller and device drivers are written from the public
//! specifications and have run only against simulations and QEMU's
//! software bus (the self-test); see docs/HARDWARE.md.

pub mod designware;
mod gpio;
pub mod hid;
#[cfg(feature = "ktest")]
pub mod mock;

use crate::acpi::devices::{Found, I2cController};
use crate::sync::{IrqMutex, Mutex};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_acpi_res::{self as res, Resource};
use aurora_i2c::designware::Speed;
pub use aurora_i2c::Error;

/// An I²C master the device drivers can use from any task.
pub trait I2cBus: Send + Sync {
    /// Write `write`, then (repeated start) read into `read`; see [`aurora_i2c::Bus`].
    fn transfer(&self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error>;
    /// After sleep: the controller lost its state; set it up again.
    fn resume(&self) {}
    fn describe(&self) -> String;
}

/// Lets the protocol code (generic over [`aurora_i2c::Bus`]) use a shared bus.
pub struct BusRef<'a>(pub &'a dyn I2cBus);

impl aurora_i2c::Bus for BusRef<'_> {
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        self.0.transfer(addr, write, read)
    }
}

/// Controllers by ACPI path.
static BUSES: Mutex<Vec<(String, Arc<dyn I2cBus>)>> = Mutex::new(Vec::new());
static FOUND: IrqMutex<Option<Found>> = IrqMutex::new(None);

/// Called by the ACPI task with what the namespace describes.
pub fn start(found: Found) {
    // The drivers are new to real hardware: `i2c=off` in boot.conf leaves
    // the controllers alone (the ACPI scan above still logs what is there).
    if crate::gui::prefs::boot_option("i2c").as_deref() == Some("off") {
        log!("i2c", "I2C drivers disabled (i2c=off)");
        return;
    }
    *FOUND.lock() = Some(found);
    crate::sched::spawn("i2c", task);
}

/// The fastest speed every device on controller `path` supports.
fn bus_speed(found: &Found, path: &str) -> Speed {
    let mut speed = Speed::Fast;
    for h in &found.hid {
        for r in &h.dev.resources {
            if let Some((source, _, hz)) = r.i2c() {
                if res::normalize_path(source) == path && Speed::for_hz(hz) == Speed::Standard {
                    speed = Speed::Standard;
                }
            }
        }
    }
    speed
}

fn open_controller(c: &I2cController, speed: Speed) -> Option<Arc<dyn I2cBus>> {
    #[cfg(feature = "ktest")]
    if c.dev.is(crate::acpi::devices::TEST_CONTROLLER) {
        return Some(mock::bus());
    }
    designware::open(c, speed)
}

fn task() {
    let Some(found) = FOUND.lock().take() else { return };
    designware::log_pci_controllers();
    for c in &found.controllers {
        let speed = bus_speed(&found, &c.dev.path);
        match open_controller(c, speed) {
            Some(bus) => {
                log!("i2c", "{}: {}", c.dev.path, bus.describe());
                BUSES.lock().push((c.dev.path.clone(), bus));
            }
            None => log!("i2c", "{}: no driver for this controller", c.dev.path),
        }
    }
    for h in &found.hid {
        let Some((source, addr, speed_hz)) = h.dev.resources.iter().find_map(Resource::i2c) else {
            log!("i2c", "{}: no I2cSerialBus resource", h.dev.path);
            continue;
        };
        let source = res::normalize_path(source);
        let Some(bus) = BUSES.lock().iter().find(|b| b.0 == source).map(|b| b.1.clone()) else {
            log!("i2c", "{}: its controller {} is not running", h.dev.path, source);
            continue;
        };
        let Some(desc_reg) = h.desc_reg else {
            log!("i2c", "{}: no HID descriptor register (_DSM); skipped", h.dev.path);
            continue;
        };
        let irq = hid::IrqSource::from_resources(&h.dev.resources, &found.gpio);
        hid::start(hid::Config { path: h.dev.path.clone(), controller: source, bus, addr, desc_reg, speed_hz, irq });
    }
}

/// After sleep: controllers first, then each device's task brings its device back.
pub fn resume() {
    let buses: Vec<Arc<dyn I2cBus>> = BUSES.lock().iter().map(|b| b.1.clone()).collect();
    for b in buses {
        b.resume();
    }
    hid::resume_all();
}

/// One line per device, for the System Report and the self-test.
pub fn list() -> Vec<String> {
    hid::list()
}
