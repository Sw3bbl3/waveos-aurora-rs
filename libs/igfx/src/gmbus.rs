//! GMBUS: the display engine's I²C controller, for the DDC lines of HDMI
//! and DVI ports (EDID at address 0x50, the E-DDC segment pointer at 0x30).
//!
//! Registers (south display): `GMBUS0` selects the pin pair and rate,
//! `GMBUS1` starts a cycle (software ready, cycle type, byte count, index,
//! address, direction), `GMBUS2` is the status, `GMBUS3` the 4-byte data
//! window (little-endian), `GMBUS4` the interrupt mask, `GMBUS5` the 2-byte
//! index. One cycle moves at most [`MAX_BYTES`].

use crate::Mmio;
use aurora_i2c::{Bus, Error};

pub const GMBUS0: u32 = 0xC5100;
pub const GMBUS1: u32 = 0xC5104;
pub const GMBUS2: u32 = 0xC5108;
pub const GMBUS3: u32 = 0xC510C;
pub const GMBUS4: u32 = 0xC5110;
pub const GMBUS5: u32 = 0xC5120;

/// `GMBUS0` rate field 10:8: 0 = 100 kHz.
pub const RATE_100K: u32 = 0 << 8;

pub const SW_CLR_INT: u32 = 1 << 31;
pub const SW_RDY: u32 = 1 << 30;
/// Enable the hardware timeout.
pub const ENT: u32 = 1 << 29;
pub const CYCLE_WAIT: u32 = 1 << 25;
pub const CYCLE_INDEX: u32 = 2 << 25;
pub const CYCLE_STOP: u32 = 4 << 25;
pub const BYTE_COUNT_SHIFT: u32 = 16;
pub const INDEX_SHIFT: u32 = 8;
pub const READ: u32 = 1;

pub const HW_WAIT_PHASE: u32 = 1 << 14;
pub const STALL_TIMEOUT: u32 = 1 << 13;
pub const HW_RDY: u32 = 1 << 11;
/// Slave acknowledge timeout or error: the device did not answer.
pub const SATOER: u32 = 1 << 10;
pub const ACTIVE: u32 = 1 << 9;

/// Bytes per cycle the driver asks for (the field holds 9 bits; some
/// controllers handle no more than 256).
pub const MAX_BYTES: usize = 256;
const TIMEOUT_US: u32 = 50_000;

/// One pin pair of the controller.
pub struct Gmbus<'a, M: Mmio> {
    pub mmio: &'a M,
    pub pin: u32,
}

impl<M: Mmio> Gmbus<'_, M> {
    /// Waits for `HW_RDY` (or the wait phase), failing on a NAK or stall.
    fn wait(&self, bit: u32) -> Result<(), Error> {
        let mut waited = 0;
        loop {
            let s = self.mmio.read(GMBUS2);
            if s & SATOER != 0 {
                return Err(Error::AddressNack);
            }
            if s & STALL_TIMEOUT != 0 {
                return Err(Error::Timeout);
            }
            if s & bit != 0 {
                return Ok(());
            }
            if waited >= TIMEOUT_US {
                return Err(Error::Timeout);
            }
            self.mmio.delay_us(50);
            waited += 50;
        }
    }

    /// Ends the transaction with a stop and waits for the bus to go idle.
    fn stop(&self) {
        self.mmio.write(GMBUS1, SW_RDY | CYCLE_STOP);
        self.mmio.wait_for(GMBUS2, ACTIVE, 0, TIMEOUT_US);
    }

    /// After a NAK: clear the error, release the bus.
    fn recover(&self) {
        self.mmio.write(GMBUS1, SW_CLR_INT);
        self.mmio.write(GMBUS1, 0);
        self.mmio.wait_for(GMBUS2, ACTIVE, 0, TIMEOUT_US);
    }

    /// Reads `buf` (after writing a one-byte `index`, in the same cycle) and
    /// stays in the wait phase.
    fn read_cycle(&self, addr: u16, index: Option<u8>, buf: &mut [u8]) -> Result<(), Error> {
        let mut cmd =
            SW_RDY | ENT | CYCLE_WAIT | (buf.len() as u32) << BYTE_COUNT_SHIFT | (addr as u32 & 0x7F) << 1 | READ;
        if let Some(i) = index {
            self.mmio.write(GMBUS5, 0);
            cmd |= CYCLE_INDEX | (i as u32) << INDEX_SHIFT;
        }
        self.mmio.write(GMBUS1, cmd);
        for chunk in buf.chunks_mut(4) {
            self.wait(HW_RDY)?;
            let v = self.mmio.read(GMBUS3).to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
        Ok(())
    }

    /// Writes `data` and stays in the wait phase (no stop): a read or a stop follows.
    fn write_cycle(&self, addr: u16, data: &[u8]) -> Result<(), Error> {
        let word = |c: &[u8]| {
            let mut b = [0u8; 4];
            b[..c.len()].copy_from_slice(c);
            u32::from_le_bytes(b)
        };
        let mut chunks = data.chunks(4);
        self.mmio.write(GMBUS3, chunks.next().map_or(0, word));
        self.mmio.write(
            GMBUS1,
            SW_RDY | ENT | CYCLE_WAIT | (data.len() as u32) << BYTE_COUNT_SHIFT | (addr as u32 & 0x7F) << 1,
        );
        for c in chunks {
            self.wait(HW_RDY)?;
            self.mmio.write(GMBUS3, word(c));
        }
        self.wait(HW_WAIT_PHASE)
    }

    fn run(&self, f: impl FnOnce() -> Result<(), Error>) -> Result<(), Error> {
        self.mmio.write(GMBUS0, RATE_100K | self.pin);
        self.mmio.write(GMBUS4, 0);
        let r = f();
        match r {
            Ok(()) => {
                self.mmio.wait_for(GMBUS2, ACTIVE, 0, TIMEOUT_US);
            }
            Err(_) => self.recover(),
        }
        self.mmio.write(GMBUS0, 0);
        r
    }

    /// Reads EDID block `block` (128 bytes) from address 0x50, writing the
    /// E-DDC segment pointer first for blocks 2 and up.
    pub fn read_edid_block(&self, block: u8, out: &mut [u8; 128]) -> Result<(), Error> {
        self.run(|| {
            if block >= 2 {
                self.write_cycle(0x30, &[block / 2])?;
            }
            let offset = (block % 2) * 128;
            self.read_cycle(0x50, Some(offset), out)?;
            self.wait(HW_WAIT_PHASE)?;
            self.stop();
            Ok(())
        })
    }
}

impl<M: Mmio> Bus for Gmbus<'_, M> {
    /// A one-byte write then a read is an indexed read (one cycle); other
    /// shapes are a write, then a read after a repeated start.
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        if addr > 0x7F || write.len() > MAX_BYTES || read.len() > MAX_BYTES || (write.is_empty() && read.is_empty()) {
            return Err(Error::Unsupported);
        }
        self.run(|| {
            if write.len() == 1 && !read.is_empty() {
                self.read_cycle(addr, Some(write[0]), read)?;
            } else {
                if !write.is_empty() {
                    self.write_cycle(addr, write)?;
                }
                if !read.is_empty() {
                    self.read_cycle(addr, None, read)?;
                }
            }
            self.wait(HW_WAIT_PHASE)?;
            self.stop();
            Ok(())
        })
    }
}
