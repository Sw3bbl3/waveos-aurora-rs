//! A HID over I²C device in software, on a bus of its own: the far end of
//! the protocol for tests (the host's and the kernel's self-test, which plays
//! a Precision Touchpad through the real driver with it).
//!
//! It behaves as the specification says a device does: it answers at one
//! address; its HID descriptor and report descriptor are at their
//! registers; RESET leaves one empty input report pending; SET_REPORT
//! stores feature reports, which GET_REPORT returns; reading the input
//! register returns the oldest queued report, or length 0 when none is.
//! Asleep (after SET_POWER sleep), it misses the first transfer, as real
//! devices may.

use crate::hid::{opcode, HidDescriptor, DESCRIPTOR_LEN};
use crate::{Bus, Error};
use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub struct HidTarget {
    pub addr: u16,
    pub desc_reg: u16,
    pub desc: HidDescriptor,
    pub report_desc: Vec<u8>,
    /// Feature reports by id, as last set.
    pub features: Vec<(u8, Vec<u8>)>,
    pub queue: VecDeque<Vec<u8>>,
    pub reset_pending: bool,
    pub resets: u32,
    pub asleep: bool,
    /// Transfers seen, for tests.
    pub transfers: u32,
}

impl HidTarget {
    pub fn new(addr: u16, desc_reg: u16, report_desc: Vec<u8>, max_input_len: u16) -> HidTarget {
        HidTarget {
            addr,
            desc_reg,
            desc: HidDescriptor {
                bcd_version: 0x0100,
                report_desc_len: report_desc.len() as u16,
                report_desc_reg: 0x0002,
                input_reg: 0x0003,
                max_input_len,
                output_reg: 0x0004,
                max_output_len: 0,
                command_reg: 0x0005,
                data_reg: 0x0006,
                vendor: 0x1234,
                product: 0x5678,
                version: 1,
            },
            report_desc,
            features: Vec::new(),
            queue: VecDeque::new(),
            reset_pending: false,
            resets: 0,
            asleep: true,
            transfers: 0,
        }
    }

    /// Queues an input report (id first if numbered).
    pub fn push_input(&mut self, report: &[u8]) {
        self.queue.push_back(report.to_vec());
    }

    pub fn feature(&self, id: u8) -> Option<&[u8]> {
        self.features.iter().find(|f| f.0 == id).map(|f| f.1.as_slice())
    }

    fn reply(read: &mut [u8], data: &[u8]) {
        let n = data.len().min(read.len());
        read[..n].copy_from_slice(&data[..n]);
        read[n..].fill(0);
    }

    fn handle(&mut self, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        let reg = |w: &[u8]| u16::from_le_bytes([w[0], w[1]]);
        // A plain read: the input register.
        if write.is_empty() {
            let report = if self.reset_pending {
                self.reset_pending = false;
                Vec::new()
            } else {
                self.queue.pop_front().unwrap_or_default()
            };
            let len = if report.is_empty() { 0 } else { report.len() as u16 + 2 };
            let mut out = len.to_le_bytes().to_vec();
            out.extend_from_slice(&report);
            Self::reply(read, &out);
            return Ok(());
        }
        if write.len() < 2 {
            return Err(Error::DataNack);
        }
        let r = reg(write);
        if write.len() == 2 {
            if r == self.desc_reg {
                Self::reply(read, &self.desc.to_bytes()[..DESCRIPTOR_LEN]);
            } else if r == self.desc.report_desc_reg {
                let d = self.report_desc.clone();
                Self::reply(read, &d);
            } else {
                return Err(Error::DataNack);
            }
            return Ok(());
        }
        if r != self.desc.command_reg || write.len() < 4 {
            return Err(Error::DataNack);
        }
        let kind_id = write[2];
        let op = write[3] & 0x0F;
        let mut rest = &write[4..];
        let mut id = kind_id & 0x0F;
        if id == 0x0F && op != opcode::SET_POWER && op != opcode::RESET {
            id = *rest.first().ok_or(Error::DataNack)?;
            rest = &rest[1..];
        }
        match op {
            opcode::RESET => {
                self.reset_pending = true;
                self.resets += 1;
                self.features.clear();
                self.queue.clear();
                self.asleep = false;
            }
            opcode::SET_POWER => self.asleep = kind_id & 3 == 1,
            opcode::GET_REPORT => {
                if rest.len() < 2 || reg(rest) != self.desc.data_reg {
                    return Err(Error::DataNack);
                }
                let data = self.feature(id).map(|f| f.to_vec()).unwrap_or_else(|| alloc::vec![id]);
                let mut out = ((data.len() + 2) as u16).to_le_bytes().to_vec();
                out.extend_from_slice(&data);
                Self::reply(read, &out);
            }
            opcode::SET_REPORT => {
                if rest.len() < 4 || reg(rest) != self.desc.data_reg {
                    return Err(Error::DataNack);
                }
                let len = u16::from_le_bytes([rest[2], rest[3]]) as usize;
                let data = rest.get(4..2 + len).ok_or(Error::DataNack)?.to_vec();
                match self.features.iter_mut().find(|f| f.0 == id) {
                    Some(f) => f.1 = data,
                    None => self.features.push((id, data)),
                }
            }
            _ => return Err(Error::DataNack),
        }
        Ok(())
    }
}

/// A Precision Touchpad's report descriptor (after Microsoft's sample, the
/// same layout as aurora-hid's tests): report 1 holds two fingers (confidence,
/// tip, 3-bit contact id, 16-bit X 0..10000 over 100 mm and Y 0..6000 over
/// 60 mm), scan time, contact count and the button; feature 2 the maximum
/// contacts and pad type; feature 3 the Input Mode; feature 4 surface and
/// button switches; report 5 the mouse it acts as until switched.
pub fn precision_touchpad() -> Vec<u8> {
    let finger: &[u8] = &[
        0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, // Finger, logical collection
        0x15, 0x00, 0x25, 0x01, 0x09, 0x47, 0x09, 0x42, 0x95, 0x02, 0x75, 0x01, 0x81, 0x02, // confidence, tip
        0x95, 0x01, 0x75, 0x03, 0x25, 0x05, 0x09, 0x51, 0x81, 0x02, // contact id
        0x75, 0x01, 0x95, 0x03, 0x81, 0x03, // padding
        0x05, 0x01, 0x15, 0x00, 0x26, 0x10, 0x27, 0x75, 0x10, 0x55, 0x0E, 0x65, 0x11, // 0..10000, cm × 10^-2
        0x09, 0x30, 0x35, 0x00, 0x46, 0xE8, 0x03, 0x95, 0x01, 0x81, 0x02, // X: 10.00 cm
        0x26, 0x70, 0x17, 0x46, 0x58, 0x02, 0x09, 0x31, 0x81, 0x02, // Y: 0..6000, 6.00 cm
        0xC0,
    ];
    let mut d = alloc::vec![0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01];
    d.extend_from_slice(finger);
    d.extend_from_slice(finger);
    d.extend_from_slice(&[
        0x05, 0x0D, 0x55, 0x0C, 0x66, 0x01, 0x10, 0x47, 0xFF, 0xFF, 0x00, 0x00, 0x27, 0xFF, 0xFF, 0x00, 0x00, 0x75,
        0x10, 0x95, 0x01, 0x09, 0x56, 0x81, 0x02, // scan time
        0x09, 0x54, 0x25, 0x7F, 0x95, 0x01, 0x75, 0x08, 0x81, 0x02, // contact count
        0x05, 0x09, 0x09, 0x01, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, // button
        0x05, 0x0D, 0x85, 0x02, 0x09, 0x55, 0x09, 0x59, 0x75, 0x04, 0x95, 0x02, 0x25, 0x0F, 0xB1,
        0x02, // max, pad type
        0xC0, //
        0x05, 0x0D, 0x09, 0x0E, 0xA1, 0x01, 0x85, 0x03, 0x09, 0x22, 0xA1, 0x02, // configuration
        0x09, 0x52, 0x15, 0x00, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01, 0xB1, 0x02, 0xC0, // input mode
        0x09, 0x22, 0xA1, 0x00, 0x85, 0x04, 0x09, 0x57, 0x09, 0x58, 0x75, 0x01, 0x95, 0x02, 0x25, 0x01, 0xB1, 0x02,
        0x95, 0x06, 0xB1, 0x03, 0xC0, 0xC0, // selective reporting
        0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x05, 0x09, 0x01, 0xA1, 0x00, // legacy mouse, report 5
        0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03,
        0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xC0, 0xC0,
    ]);
    d
}

/// One input report of [`precision_touchpad`]: up to two contacts
/// `(id, touching, x, y)`, the frame's contact count and the button.
pub fn touch_report(contacts: &[(u8, bool, u16, u16)], count: u8, button: bool) -> Vec<u8> {
    let mut r = alloc::vec![0u8; 15];
    r[0] = 1;
    for (slot, &(id, tip, x, y)) in contacts.iter().take(2).enumerate() {
        let base = 1 + slot * 5;
        r[base] = 0x01 | (tip as u8) << 1 | (id & 7) << 2;
        r[base + 1..base + 3].copy_from_slice(&x.to_le_bytes());
        r[base + 3..base + 5].copy_from_slice(&y.to_le_bytes());
    }
    r[13] = count;
    r[14] = button as u8;
    r
}

impl Bus for HidTarget {
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        self.transfers += 1;
        if addr != self.addr {
            return Err(Error::AddressNack);
        }
        if self.asleep {
            // Woken by being addressed; this transfer is lost.
            self.asleep = false;
            return Err(Error::AddressNack);
        }
        self.handle(write, read)
    }
}
