//! SD cards and eMMC behind an SD Host Controller (SDHCI).
//!
//! - [`host`]: the SD Host Controller Standard Specification (versions 1.00
//!   to 4.20): reset, power, clock, bus width, timing and signal voltage,
//!   commands, data by PIO, SDMA or ADMA2 (32- and 64-bit descriptors),
//!   tuning, and line resets after errors.
//! - [`card`]: the cards on top: SD memory cards (Physical Layer
//!   Specification 3.01 and later: SDSC, SDHC and SDXC, high speed and
//!   UHS-I) and eMMC (JEDEC JESD84 4.41 to 5.1: `EXT_CSD`, 4- and 8-bit
//!   buses, high speed, HS200 and HS400), with block reads and writes,
//!   erase and TRIM, cache flush, and retries that fall back to slower modes.
//! - [`regs`]: the controller's register map; [`card_regs`]: the cards'
//!   registers (CID, CSD, SCR, switch status, `EXT_CSD`).
//! - [`mock`]: a controller and cards in software, for tests: the host's and
//!   the kernel's self-test, which drive the real code through it, inject
//!   errors QEMU never produces, and run the eMMC and UHS paths QEMU lacks.
//!
//! No hardware access happens here: registers go through [`Regs`], time and
//! waiting through [`Platform`], and DMA memory through [`DmaBuf`].

#![no_std]

extern crate alloc;

pub mod card;
pub mod card_regs;
pub mod host;
pub mod mock;
pub mod regs;

pub use card::{CardInfo, CardKind, EraseKind, Limits, Slot};
pub use host::{Caps, Host, Timing, TransferMode};

/// Why an operation failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// No card in the slot (or it was pulled out).
    NoCard,
    /// The card did not answer a command.
    Timeout,
    /// A command's response was corrupt (CRC, end bit or index).
    Crc,
    /// Data did not arrive (or the card stayed busy) in time.
    DataTimeout,
    /// Data arrived corrupt (CRC or end bit).
    DataCrc,
    /// The controller's DMA engine stopped (ADMA error status in the value).
    Dma(u8),
    /// The card reported errors in its status (the R1 bits).
    Card(u32),
    /// The controller did not do what it was told (a reset or clock that
    /// never settled, a line that stayed busy).
    Controller,
    /// The card or controller can't do what was asked.
    Unsupported,
    /// The card is write protected.
    ReadOnly,
    /// Outside the card.
    OutOfRange,
    /// Sampling-point tuning failed.
    Tuning,
}

impl Error {
    /// Errors a slower bus or a fresh tuning may cure.
    pub fn is_signal(self) -> bool {
        matches!(self, Error::Crc | Error::DataCrc | Error::Timeout | Error::DataTimeout | Error::Tuning)
    }
}

pub type Result<T> = core::result::Result<T, Error>;

/// Register access at byte offsets, in the widths the specification gives.
pub trait Regs {
    fn read8(&self, offset: usize) -> u8;
    fn read16(&self, offset: usize) -> u16;
    fn read32(&self, offset: usize) -> u32;
    fn write8(&self, offset: usize, value: u8);
    fn write16(&self, offset: usize, value: u16);
    fn write32(&self, offset: usize, value: u32);
}

/// Time, waiting and logging, as the environment provides them.
pub trait Platform {
    /// Microseconds from any fixed point.
    fn now_us(&mut self) -> u64;
    /// Waits a short while (up to about a millisecond) for the controller's
    /// interrupt. May return early or spuriously.
    fn wait(&mut self);
    /// Waits at least `us` microseconds.
    fn delay_us(&mut self, us: u64);
    /// A line for the log (bring-up decisions, recoveries).
    fn note(&mut self, _msg: core::fmt::Arguments) {}
}

/// Memory the controller reaches by DMA: physically contiguous, at `phys`.
pub trait DmaBuf: Send {
    fn phys(&self) -> u64;
    fn bytes(&self) -> &[u8];
    fn bytes_mut(&mut self) -> &mut [u8];
}
