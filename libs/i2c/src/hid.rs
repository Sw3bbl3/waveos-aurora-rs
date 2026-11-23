//! HID over I²C (Microsoft's "HID over I2C Protocol Specification", 1.0).
//!
//! A device has a few 16-bit registers, all named by its HID descriptor,
//! which the host reads from the register ACPI gives (`_DSM` function 1):
//!
//! - the **report descriptor** register: the ordinary HID report descriptor;
//! - the **input** register: the host reads it (a plain read, no register
//!   address) whenever the device's interrupt line is asserted; every report
//!   starts with its 16-bit length, which counts those two bytes, and a
//!   length of 0 means "nothing" (the device sends one after a reset);
//! - the **command** register: RESET, GET_REPORT, SET_REPORT, SET_POWER, …,
//!   written as the register, a byte with the report type and id, the
//!   opcode, and for report commands the **data** register followed by the
//!   data.
//!
//! All numbers are little-endian.

use crate::{Bus, Error as BusError};
use alloc::vec;
use alloc::vec::Vec;

/// The HID descriptor's size (version 1.00).
pub const DESCRIPTOR_LEN: usize = 30;

pub mod opcode {
    pub const RESET: u8 = 0x01;
    pub const GET_REPORT: u8 = 0x02;
    pub const SET_REPORT: u8 = 0x03;
    pub const SET_POWER: u8 = 0x08;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReportType {
    Input = 1,
    Output = 2,
    Feature = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Bus(BusError),
    /// The HID descriptor is not one (wrong length or version, impossible sizes).
    BadDescriptor,
    /// A report whose length field doesn't fit.
    BadLength(u16),
}

impl From<BusError> for Error {
    fn from(e: BusError) -> Error {
        Error::Bus(e)
    }
}

/// The HID descriptor (section 5.1.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HidDescriptor {
    pub bcd_version: u16,
    pub report_desc_len: u16,
    pub report_desc_reg: u16,
    pub input_reg: u16,
    /// The longest input report, its two length bytes included.
    pub max_input_len: u16,
    pub output_reg: u16,
    pub max_output_len: u16,
    pub command_reg: u16,
    pub data_reg: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

impl HidDescriptor {
    pub fn parse(b: &[u8]) -> Result<HidDescriptor, Error> {
        if b.len() < DESCRIPTOR_LEN {
            return Err(Error::BadDescriptor);
        }
        let w = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
        let d = HidDescriptor {
            bcd_version: w(2),
            report_desc_len: w(4),
            report_desc_reg: w(6),
            input_reg: w(8),
            max_input_len: w(10),
            output_reg: w(12),
            max_output_len: w(14),
            command_reg: w(16),
            data_reg: w(18),
            vendor: w(20),
            product: w(22),
            version: w(24),
        };
        if w(0) as usize != DESCRIPTOR_LEN
            || d.bcd_version >> 8 != 1
            || d.report_desc_len == 0
            || d.report_desc_len > 8192
            || !(3..=4096).contains(&d.max_input_len)
        {
            return Err(Error::BadDescriptor);
        }
        Ok(d)
    }

    pub fn to_bytes(&self) -> [u8; DESCRIPTOR_LEN] {
        let words = [
            DESCRIPTOR_LEN as u16,
            self.bcd_version,
            self.report_desc_len,
            self.report_desc_reg,
            self.input_reg,
            self.max_input_len,
            self.output_reg,
            self.max_output_len,
            self.command_reg,
            self.data_reg,
            self.vendor,
            self.product,
            self.version,
        ];
        let mut out = [0u8; DESCRIPTOR_LEN];
        for (i, v) in words.iter().enumerate() {
            out[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
        }
        out
    }
}

/// A command: register, then type and id, then opcode (ids from 15 up
/// take an extra byte, section 7.2).
pub fn command(command_reg: u16, op: u8, kind: Option<ReportType>, id: u8, low: u8) -> Vec<u8> {
    let mut v = command_reg.to_le_bytes().to_vec();
    let type_bits = kind.map_or(0, |k| (k as u8) << 4);
    let id_bits = if id >= 15 { 0x0F } else { id };
    v.push(type_bits | id_bits | (low & 0x0F));
    v.push(op & 0x0F);
    if id >= 15 {
        v.push(id);
    }
    v
}

/// What one read of the input register gave.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Input<'a> {
    /// Length 0: nothing pending, or the end of a reset.
    Empty,
    /// A report, with its report id first if the device numbers them.
    Report(&'a [u8]),
}

/// A HID over I²C device at `addr`.
#[derive(Clone, Debug)]
pub struct Device {
    pub addr: u16,
    pub desc: HidDescriptor,
}

impl Device {
    /// Reads the HID descriptor from register `desc_reg`.
    pub fn probe(bus: &mut dyn Bus, addr: u16, desc_reg: u16) -> Result<Device, Error> {
        let mut b = [0u8; DESCRIPTOR_LEN];
        // A sleeping device may miss the first transfer.
        let reg = desc_reg.to_le_bytes();
        if bus.transfer(addr, &reg, &mut b).is_err() {
            bus.transfer(addr, &reg, &mut b)?;
        }
        Ok(Device { addr, desc: HidDescriptor::parse(&b)? })
    }

    /// SET_POWER: on, or asleep. Tried twice: a device in deep sleep
    /// may not answer the first (section 7.2.8).
    pub fn set_power(&self, bus: &mut dyn Bus, on: bool) -> Result<(), Error> {
        let c = command(self.desc.command_reg, opcode::SET_POWER, None, 0, if on { 0 } else { 1 });
        if bus.transfer(self.addr, &c, &mut []).is_err() {
            bus.transfer(self.addr, &c, &mut [])?;
        }
        Ok(())
    }

    /// RESET. The device then interrupts and gives an empty input report;
    /// read it with [`Device::read_input`].
    pub fn reset(&self, bus: &mut dyn Bus) -> Result<(), Error> {
        let c = command(self.desc.command_reg, opcode::RESET, None, 0, 0);
        bus.transfer(self.addr, &c, &mut [])?;
        Ok(())
    }

    pub fn report_descriptor(&self, bus: &mut dyn Bus) -> Result<Vec<u8>, Error> {
        let mut d = vec![0u8; self.desc.report_desc_len as usize];
        bus.transfer(self.addr, &self.desc.report_desc_reg.to_le_bytes(), &mut d)?;
        Ok(d)
    }

    /// GET_REPORT: the report as the device returns it (id first if numbered),
    /// asking for at most `max_len` bytes of it.
    pub fn get_report(&self, bus: &mut dyn Bus, kind: ReportType, id: u8, max_len: usize) -> Result<Vec<u8>, Error> {
        let mut c = command(self.desc.command_reg, opcode::GET_REPORT, Some(kind), id, 0);
        c.extend_from_slice(&self.desc.data_reg.to_le_bytes());
        let mut b = vec![0u8; max_len + 2];
        bus.transfer(self.addr, &c, &mut b)?;
        let len = u16::from_le_bytes([b[0], b[1]]);
        if (len as usize) < 2 || len as usize > b.len() {
            return Err(Error::BadLength(len));
        }
        Ok(b[2..len as usize].to_vec())
    }

    /// SET_REPORT with `report` (id first if numbered; `id` is that id or 0).
    pub fn set_report(&self, bus: &mut dyn Bus, kind: ReportType, id: u8, report: &[u8]) -> Result<(), Error> {
        let mut c = command(self.desc.command_reg, opcode::SET_REPORT, Some(kind), id, 0);
        c.extend_from_slice(&self.desc.data_reg.to_le_bytes());
        c.extend_from_slice(&((report.len() + 2) as u16).to_le_bytes());
        c.extend_from_slice(report);
        bus.transfer(self.addr, &c, &mut [])?;
        Ok(())
    }

    /// A buffer for [`Device::read_input`].
    pub fn input_buffer(&self) -> Vec<u8> {
        vec![0u8; self.desc.max_input_len as usize]
    }

    /// Reads the input register once.
    pub fn read_input<'a>(&self, bus: &mut dyn Bus, buf: &'a mut [u8]) -> Result<Input<'a>, Error> {
        bus.transfer(self.addr, &[], buf)?;
        let len = u16::from_le_bytes([buf[0], buf[1]]);
        if len == 0 {
            return Ok(Input::Empty);
        }
        // Some devices send 0xFFFF while they have nothing (the bus idles high).
        if len < 2 || len as usize > buf.len() {
            return Err(Error::BadLength(len));
        }
        Ok(if len == 2 { Input::Empty } else { Input::Report(&buf[2..len as usize]) })
    }
}
