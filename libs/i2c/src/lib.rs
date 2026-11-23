//! I²C for WaveOS: the bus, a controller and the HID protocol on top.
//!
//! - [`Bus`]: one combined transfer (write, then read after a repeated
//!   start) to a 7- or 10-bit address; what every controller offers and
//!   every device driver needs.
//! - [`designware`]: the Synopsys DesignWare APB I²C master found in Intel
//!   LPSS (Haswell and later laptops) and AMD FCH chipsets, over any register
//!   access ([`designware::Regs`]) and wait strategy ([`designware::Waiter`]).
//! - [`hid`]: the HID over I²C protocol (Microsoft, version 1.0): the HID
//!   descriptor, commands (reset, power, get and set report) and input reports.
//! - [`mock`]: a software HID-over-I²C device on a software bus, which the
//!   kernel's self-test plays a Precision Touchpad through.
//!
//! No hardware access happens here, so all of it runs on the host in tests.

#![no_std]

extern crate alloc;

pub mod designware;
pub mod hid;
pub mod mock;

/// Why a transfer failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Nobody answered at the address (it may be asleep).
    AddressNack,
    /// The device refused a data byte.
    DataNack,
    /// Another master took the bus.
    ArbitrationLost,
    /// The transfer did not finish in time (it was aborted).
    Timeout,
    /// Received data was lost (the driver fell behind the FIFO).
    Overrun,
    /// The controller reported another abort reason (its raw bits).
    Abort(u32),
    /// The controller is not there or not responding.
    NoController,
    /// A transfer the controller cannot do (e.g. zero bytes each way).
    Unsupported,
}

/// An I²C master.
pub trait Bus {
    /// Writes `write` to the device at `addr`, then (with a repeated start,
    /// if both are non-empty) reads `read.len()` bytes, ending with a stop.
    /// `addr` above 0x7F is a 10-bit address.
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error>;
}

impl<B: Bus + ?Sized> Bus for &mut B {
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        (**self).transfer(addr, write, read)
    }
}
