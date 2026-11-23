//! The self-test's I²C controller: a software bus with one HID over I²C
//! Precision Touchpad on it (`aurora_i2c::mock`), at the address and HID
//! descriptor register xtask's test SSDT describes (`\_SB.I2CT.TPD0`). The
//! real ACPI scan, HID over I²C driver and gesture engine run against it.

use super::{Error, I2cBus};
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_i2c::mock::{precision_touchpad, HidTarget};
use aurora_i2c::Bus;

/// Where the test SSDT puts the touchpad.
pub const ADDRESS: u16 = 0x2C;
pub const DESCRIPTOR_REGISTER: u16 = 0x20;

static TARGET: IrqMutex<Option<HidTarget>> = IrqMutex::new(None);

struct MockBus;

impl I2cBus for MockBus {
    fn transfer(&self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        let mut t = TARGET.lock();
        let t = t.as_mut().ok_or(Error::NoController)?;
        t.transfer(addr, write, read)
    }

    fn describe(&self) -> String {
        String::from("software I2C bus (self-test) with a Precision Touchpad at 0x2c")
    }
}

pub fn bus() -> Arc<dyn I2cBus> {
    *TARGET.lock() = Some(HidTarget::new(ADDRESS, DESCRIPTOR_REGISTER, precision_touchpad(), 32));
    Arc::new(MockBus)
}

/// Runs `f` on the touchpad (to queue reports, or look at what the driver set).
pub fn with_target<R>(f: impl FnOnce(&mut HidTarget) -> R) -> Option<R> {
    TARGET.lock().as_mut().map(f)
}
