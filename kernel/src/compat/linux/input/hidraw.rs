//! `/dev/hidrawN`: a HID device's reports as they are. `read` gives one
//! input report (with its id first, when the device numbers them),
//! `write` sends an output report, and the `ioctl`s give the report
//! descriptor, the device's name and ids, and feature reports.

use super::super::chardev::{self, ioc_dir, ioc_nr, ioc_size, ioc_type, put_str, CharDev};
use super::tree::Dev;
use crate::drivers::evdev::hidraw::{Client, FEATURE, INPUT, OUTPUT};
use crate::syscall::user;
use alloc::sync::Arc;
use aurora_abi::err::*;
use core::sync::atomic::Ordering;

/// `HID_MAX_DESCRIPTOR_SIZE` and the largest report Linux passes.
const MAX_DESCRIPTOR: usize = 4096;
const MAX_REPORT: usize = 16384;

pub struct HidrawFile {
    pub c: Arc<Client>,
}

impl HidrawFile {
    fn gone(&self) -> bool {
        self.c.gone.load(Ordering::Acquire)
    }

    /// A GET_REPORT of `kind` into the caller's buffer (report id first).
    fn get(&self, kind: u8, arg: u64, size: usize) -> Result<u64, isize> {
        let size = size.min(MAX_REPORT);
        if size == 0 {
            return Err(EINVAL);
        }
        let mut buf = alloc::vec![0u8; size];
        user::get_bytes(arg, &mut buf[..1])?;
        let n = self.c.dev.io.get_report(kind, &mut buf)?;
        user::put_bytes(arg, &buf[..n.min(size)])?;
        Ok(n.min(size) as u64)
    }

    /// A SET_REPORT of `kind` from the caller's buffer.
    fn set(&self, kind: u8, arg: u64, size: usize) -> Result<u64, isize> {
        if size == 0 || size > MAX_REPORT {
            return Err(EINVAL);
        }
        let data = user::slice(arg, size as u64)?;
        self.c.dev.io.set_report(kind, &data).map(|n| n as u64)
    }
}

impl CharDev for HidrawFile {
    fn read(&self, out: &mut [u8], nonblock: bool) -> Result<usize, isize> {
        let report = chardev::wait(self.c.key(), nonblock, || {
            if self.gone() {
                return Err(EIO);
            }
            Ok(self.c.take())
        })?;
        let n = report.len().min(out.len());
        out[..n].copy_from_slice(&report[..n]);
        Ok(n)
    }

    fn write(&self, data: &[u8], _nonblock: bool) -> Result<usize, isize> {
        if self.gone() {
            return Err(ENODEV);
        }
        if data.is_empty() || data.len() > MAX_REPORT {
            return Err(EINVAL);
        }
        self.c.dev.io.output(data)
    }

    fn ioctl(&self, r: u32, arg: u64) -> Result<u64, isize> {
        if ioc_type(r) != b'H' as u32 {
            return Err(ENOTTY);
        }
        if self.gone() {
            return Err(ENODEV);
        }
        let h = &self.c.dev;
        let size = ioc_size(r);
        match (ioc_dir(r), ioc_nr(r)) {
            // HIDIOCGRDESCSIZE.
            (2, 0x01) => user::put(arg, &(h.descriptor.len() as i32)).map(|_| 0),
            // HIDIOCGRDESC: { u32 size; u8 value[4096] }, `size` asked for.
            (2, 0x02) => {
                let want: u32 = user::get(arg)?;
                if want as usize > MAX_DESCRIPTOR {
                    return Err(EINVAL);
                }
                let n = (want as usize).min(h.descriptor.len());
                user::put_bytes(arg + 4, &h.descriptor[..n]).map(|_| 0)
            }
            // HIDIOCGRAWINFO: { u32 bustype; i16 vendor; i16 product }.
            (2, 0x03) => {
                let mut b = [0u8; 8];
                b[0..4].copy_from_slice(&(h.bustype as u32).to_le_bytes());
                b[4..6].copy_from_slice(&h.vendor.to_le_bytes());
                b[6..8].copy_from_slice(&h.product.to_le_bytes());
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // HIDIOCGRAWNAME, HIDIOCGRAWPHYS, HIDIOCGRAWUNIQ.
            (2, 0x04) => put_str(arg, size, &h.name),
            (2, 0x05) => put_str(arg, size, &h.phys),
            (2, 0x08) => put_str(arg, size, &h.uniq),
            // HIDIOCSFEATURE / HIDIOCGFEATURE (read-write requests).
            (3, 0x06) => self.set(FEATURE, arg, size),
            (3, 0x07) => self.get(FEATURE, arg, size),
            // HIDIOCSINPUT / HIDIOCGINPUT / HIDIOCSOUTPUT / HIDIOCGOUTPUT.
            (3, 0x09) => self.set(INPUT, arg, size),
            (3, 0x0A) => self.get(INPUT, arg, size),
            (3, 0x0B) => self.set(OUTPUT, arg, size),
            (3, 0x0C) => self.get(OUTPUT, arg, size),
            _ => Err(EINVAL),
        }
    }

    fn readiness(&self) -> (bool, bool, bool) {
        if self.gone() {
            return (true, false, true);
        }
        (self.c.ready(), true, false)
    }

    fn wait_key(&self) -> usize {
        self.c.key()
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        super::node_stat(Dev::Hidraw(self.c.dev.num))
    }
}
