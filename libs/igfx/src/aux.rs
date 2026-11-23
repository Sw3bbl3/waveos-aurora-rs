//! The DisplayPort AUX channel: message framing (DisplayPort 1.4, §2.7)
//! and Intel's AUX controller (`DP_AUX_CH_CTL` and five data registers per
//! channel).
//!
//! A request is a header — command in the top nibble of byte 0, a 20-bit
//! address, and (unless it is an address-only I²C transaction) the length
//! minus one — then up to 16 data bytes. A reply's first byte carries the
//! native reply (5:4: ACK, NACK, DEFER) and the I²C-over-AUX reply (7:6).
//! The controller sends `send` bytes packed big-endian into the data
//! registers and leaves the reply there.

use crate::Mmio;
use aurora_i2c::Bus;

pub const NATIVE_WRITE: u8 = 0x8;
pub const NATIVE_READ: u8 = 0x9;
pub const I2C_WRITE: u8 = 0x0;
pub const I2C_READ: u8 = 0x1;
/// Middle of transaction: the I²C transaction continues (no stop).
pub const I2C_MOT: u8 = 0x4;

pub const MAX_PAYLOAD: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reply {
    Ack,
    Nack,
    Defer,
    I2cNack,
    I2cDefer,
}

pub fn decode_reply(b: u8) -> Reply {
    match (b >> 4 & 3, b >> 6 & 3) {
        (1, _) => Reply::Nack,
        (2, _) => Reply::Defer,
        (_, 1) => Reply::I2cNack,
        (_, 2) => Reply::I2cDefer,
        _ => Reply::Ack,
    }
}

/// The request header for `cmd` at `addr` moving `len` bytes. Returns the
/// header and its length: 4, or 3 for an address-only I²C transaction
/// (`len == 0` with an I²C command).
pub fn header(cmd: u8, addr: u32, len: usize) -> ([u8; 4], usize) {
    let h = [cmd << 4 | (addr >> 16 & 0xF) as u8, (addr >> 8) as u8, addr as u8, len.saturating_sub(1) as u8];
    let native = cmd & 0x8 != 0;
    (h, if len == 0 && !native { 3 } else { 4 })
}

/// Packs up to 20 bytes into the five data registers, big-endian.
pub fn pack(bytes: &[u8]) -> [u32; 5] {
    let mut out = [0u32; 5];
    for (i, b) in bytes.iter().take(20).enumerate() {
        out[i / 4] |= (*b as u32) << ((3 - i % 4) * 8);
    }
    out
}

pub fn unpack(regs: &[u32; 5], out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(20).enumerate() {
        *b = (regs[i / 4] >> ((3 - i % 4) * 8)) as u8;
    }
}

// ------------------------------------------------------- the controller

/// `DP_AUX_CH_CTL` of AUX channel `ch` (A = 0); data registers follow at +4…+0x14.
pub fn ctl(ch: u8) -> u32 {
    0x64010 + ch as u32 * 0x100
}

pub const SEND_BUSY: u32 = 1 << 31;
pub const DONE: u32 = 1 << 30;
pub const INTERRUPT: u32 = 1 << 29;
pub const TIME_OUT_ERROR: u32 = 1 << 28;
/// Time-out select 27:26: 3 is the longest (1.6 ms on Skylake). The value
/// varies per platform; 3 is the longest everywhere.
pub const TIME_OUT_MAX: u32 = 3 << 26;
pub const RECEIVE_ERROR: u32 = 1 << 25;
pub const SIZE_SHIFT: u32 = 20;
pub const SIZE_MASK: u32 = 0x1F << 20;
/// Display 9+: "FW sync pulse" 9:5 and "sync pulse" 4:0, each count − 1.
/// 32 of each, the values Intel's documentation gives for Skylake.
pub const SYNC_PULSES: u32 = 31 << 5 | 31;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuxError {
    /// No sink answered (nothing plugged in, or the channel is unpowered).
    Timeout,
    /// The controller stayed busy.
    Busy,
    /// Garbled replies, retries exhausted.
    Receive,
    /// The sink kept deferring.
    Defer,
    Nack,
    /// The reply was shorter than asked for.
    Short,
}

/// One AUX channel of the controller.
pub struct AuxCh<'a, M: Mmio> {
    pub mmio: &'a M,
    pub ch: u8,
}

impl<M: Mmio> AuxCh<'_, M> {
    /// Sends `send` (header and data) and reads the reply into `recv`;
    /// returns the reply's length. Retries receive errors (as the
    /// specification asks, at least three times) but not timeouts.
    pub fn raw(&self, send: &[u8], recv: &mut [u8; 20]) -> Result<usize, AuxError> {
        let c = ctl(self.ch);
        if !self.mmio.wait_for(c, SEND_BUSY, 0, 10_000) {
            return Err(AuxError::Busy);
        }
        for _ in 0..5 {
            let data = pack(send);
            for (i, d) in data.iter().enumerate().take(send.len().div_ceil(4)) {
                self.mmio.write(c + 4 + i as u32 * 4, *d);
            }
            // The status bits are write-one-to-clear.
            self.mmio.write(
                c,
                SEND_BUSY
                    | DONE
                    | TIME_OUT_ERROR
                    | TIME_OUT_MAX
                    | RECEIVE_ERROR
                    | (send.len() as u32) << SIZE_SHIFT
                    | SYNC_PULSES,
            );
            if !self.mmio.wait_for(c, SEND_BUSY, 0, 10_000) {
                return Err(AuxError::Busy);
            }
            let status = self.mmio.read(c);
            self.mmio.write(c, status | DONE | TIME_OUT_ERROR | RECEIVE_ERROR);
            if status & TIME_OUT_ERROR != 0 {
                return Err(AuxError::Timeout);
            }
            if status & RECEIVE_ERROR != 0 {
                self.mmio.delay_us(400);
                continue;
            }
            if status & DONE == 0 {
                continue;
            }
            let n = ((status & SIZE_MASK) >> SIZE_SHIFT) as usize;
            if n == 0 || n > 20 {
                continue;
            }
            let mut regs = [0u32; 5];
            for (i, r) in regs.iter_mut().enumerate().take(n.div_ceil(4)) {
                *r = self.mmio.read(c + 4 + i as u32 * 4);
            }
            unpack(&regs, &mut recv[..n]);
            return Ok(n);
        }
        Err(AuxError::Receive)
    }

    pub fn read(&self, addr: u32, out: &mut [u8]) -> Result<(), AuxError> {
        Transport::read(self, addr, out)
    }

    pub fn write(&self, addr: u32, data: &[u8]) -> Result<(), AuxError> {
        Transport::write(self, addr, data)
    }

    pub fn write_byte(&self, addr: u32, v: u8) -> Result<(), AuxError> {
        Transport::write_byte(self, addr, v)
    }

    pub fn read_byte(&self, addr: u32) -> Result<u8, AuxError> {
        Transport::read_byte(self, addr)
    }

    pub fn i2c(&self, addr: u8, write: &[u8], read: &mut [u8]) -> Result<(), AuxError> {
        Transport::i2c(self, addr, write, read)
    }

    pub fn read_edid_block(&self, block: u8, out: &mut [u8; 128]) -> Result<(), AuxError> {
        Transport::read_edid_block(self, block, out)
    }
}

impl<M: Mmio> Transport for AuxCh<'_, M> {
    fn raw(&self, send: &[u8], recv: &mut [u8; 20]) -> Result<usize, AuxError> {
        AuxCh::raw(self, send, recv)
    }

    fn delay_us(&self, us: u32) {
        self.mmio.delay_us(us)
    }
}

/// A controller that moves one AUX message (header and data, at most 20
/// bytes) and returns the reply: Intel's [`AuxCh`] here, AMD's AUX engine
/// in `aurora-amdgfx`. The DisplayPort side (DEFER retries, DPCD reads and
/// writes, I²C-over-AUX, EDID) is shared.
pub trait Transport {
    /// Sends `send` (header and data) and reads the reply into `recv`;
    /// returns the reply's length.
    fn raw(&self, send: &[u8], recv: &mut [u8; 20]) -> Result<usize, AuxError>;
    fn delay_us(&self, us: u32);

    /// One request with the specification's DEFER retries.
    fn request(&self, cmd: u8, addr: u32, data: &[u8], read_len: usize, out: &mut [u8]) -> Result<usize, AuxError> {
        let len = if cmd & 1 == 1 { read_len } else { data.len() };
        let (h, hl) = header(cmd, addr, len);
        let mut msg = [0u8; 20];
        msg[..hl].copy_from_slice(&h[..hl]);
        let write = cmd & 1 == 0;
        if write {
            msg[hl..hl + data.len()].copy_from_slice(data);
        }
        let n = hl + if write { data.len() } else { 0 };
        for _ in 0..32 {
            let mut reply = [0u8; 20];
            let got = self.raw(&msg[..n], &mut reply)?;
            match decode_reply(reply[0]) {
                Reply::Ack => {
                    let k = (got - 1).min(out.len());
                    out[..k].copy_from_slice(&reply[1..1 + k]);
                    return Ok(got - 1);
                }
                Reply::Nack | Reply::I2cNack => return Err(AuxError::Nack),
                Reply::Defer | Reply::I2cDefer => self.delay_us(500),
            }
        }
        Err(AuxError::Defer)
    }

    /// Reads DPCD registers starting at `addr`.
    fn read(&self, addr: u32, out: &mut [u8]) -> Result<(), AuxError> {
        let mut done = 0;
        while done < out.len() {
            let want = (out.len() - done).min(MAX_PAYLOAD);
            let got = self.request(NATIVE_READ, addr + done as u32, &[], want, &mut out[done..done + want])?;
            if got == 0 {
                return Err(AuxError::Short);
            }
            done += got.min(want);
        }
        Ok(())
    }

    /// Writes DPCD registers starting at `addr`.
    fn write(&self, addr: u32, data: &[u8]) -> Result<(), AuxError> {
        for (i, chunk) in data.chunks(MAX_PAYLOAD).enumerate() {
            self.request(NATIVE_WRITE, addr + (i * MAX_PAYLOAD) as u32, chunk, 0, &mut [])?;
        }
        Ok(())
    }

    fn write_byte(&self, addr: u32, v: u8) -> Result<(), AuxError> {
        self.write(addr, &[v])
    }

    fn read_byte(&self, addr: u32) -> Result<u8, AuxError> {
        let mut b = [0u8];
        self.read(addr, &mut b)?;
        Ok(b[0])
    }

    /// An I²C-over-AUX transfer: write, then read (each in 16-byte AUX
    /// transactions with the MOT bit), then an address-only stop.
    fn i2c(&self, addr: u8, write: &[u8], read: &mut [u8]) -> Result<(), AuxError> {
        let addr = addr as u32;
        let r = (|| {
            for chunk in write.chunks(MAX_PAYLOAD) {
                self.request(I2C_WRITE | I2C_MOT, addr, chunk, 0, &mut [])?;
            }
            let mut done = 0;
            while done < read.len() {
                let want = (read.len() - done).min(MAX_PAYLOAD);
                let got = self.request(I2C_READ | I2C_MOT, addr, &[], want, &mut read[done..done + want])?;
                if got == 0 {
                    return Err(AuxError::Short);
                }
                done += got.min(want);
            }
            Ok(())
        })();
        // Stop: an address-only transaction without MOT.
        let cmd = if read.is_empty() { I2C_WRITE } else { I2C_READ };
        let stop = self.request(cmd, addr, &[], 0, &mut []);
        r.and(stop.map(|_| ()))
    }

    /// Reads EDID block `block` over I²C-over-AUX (segment pointer for blocks ≥ 2).
    fn read_edid_block(&self, block: u8, out: &mut [u8; 128]) -> Result<(), AuxError> {
        if block >= 2 {
            self.request(I2C_WRITE | I2C_MOT, 0x30, &[block / 2], 0, &mut [])?;
        }
        self.i2c(0x50, &[(block % 2) * 128], out)
    }
}

impl<M: Mmio> Bus for AuxCh<'_, M> {
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), aurora_i2c::Error> {
        if addr > 0x7F {
            return Err(aurora_i2c::Error::Unsupported);
        }
        self.i2c(addr as u8, write, read).map_err(|e| match e {
            AuxError::Nack => aurora_i2c::Error::AddressNack,
            AuxError::Timeout | AuxError::Busy | AuxError::Defer => aurora_i2c::Error::Timeout,
            _ => aurora_i2c::Error::Abort(0),
        })
    }
}
