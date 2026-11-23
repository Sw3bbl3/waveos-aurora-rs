//! `/dev/input/jsN`: the old joystick API (`struct js_event`, 8 bytes),
//! which some games still read: buttons and axes numbered from 0, axes
//! scaled to ±32767, and the opening events that give the whole state.

use super::super::chardev::{self, ioc_dir, ioc_nr, ioc_size, ioc_type, put_str, CharDev};
use super::tree::Dev;
use crate::drivers::evdev::JoyClient;
use crate::syscall::user;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

const JS_VERSION: u32 = 0x020100;
/// `struct js_corr`: eight coefficients, a precision and a type.
const CORR_SIZE: usize = 36;
const JS_CORR_BROKEN: u16 = 1;

pub struct JsFile {
    pub c: Arc<JoyClient>,
}

impl CharDev for JsFile {
    fn read(&self, out: &mut [u8], nonblock: bool) -> Result<usize, isize> {
        if out.len() < 8 {
            return Err(EINVAL);
        }
        let events = chardev::wait(self.c.key(), nonblock, || {
            if self.c.dev.gone() {
                return Err(ENODEV);
            }
            Ok(self.c.ready().then(|| self.c.take(out.len() / 8)))
        })?;
        for (i, e) in events.iter().enumerate() {
            let o = &mut out[i * 8..i * 8 + 8];
            o[0..4].copy_from_slice(&e.time.to_le_bytes());
            o[4..6].copy_from_slice(&e.value.to_le_bytes());
            o[6] = e.kind;
            o[7] = e.number;
        }
        Ok(events.len() * 8)
    }

    fn ioctl(&self, r: u32, arg: u64) -> Result<u64, isize> {
        if ioc_type(r) != b'j' as u32 {
            return Err(ENOTTY);
        }
        let (axes, buttons) = {
            let st = self.c.st.lock();
            (st.axes.clone(), st.buttons.clone())
        };
        let size = ioc_size(r);
        match (ioc_dir(r), ioc_nr(r)) {
            // JSIOCGVERSION, JSIOCGAXES, JSIOCGBUTTONS, JSIOCGNAME.
            (2, 0x01) => user::put(arg, &JS_VERSION).map(|_| 0),
            (2, 0x11) => user::put(arg, &(axes.len() as u8)).map(|_| 0),
            (2, 0x12) => user::put(arg, &(buttons.len() as u8)).map(|_| 0),
            (2, 0x13) => put_str(arg, size, &self.c.dev.name),
            // JSIOCSCORR / JSIOCGCORR: one js_corr per axis.
            (1, 0x21) => {
                let mut raw = alloc::vec![0u8; CORR_SIZE * axes.len()];
                user::get_bytes(arg, &mut raw)?;
                let corr: Vec<[i32; 4]> = raw
                    .as_chunks::<CORR_SIZE>()
                    .0
                    .iter()
                    .map(|c| core::array::from_fn(|i| i32::from_le_bytes(c[i * 4..i * 4 + 4].try_into().unwrap())))
                    .collect();
                self.c.set_corr(corr);
                Ok(0)
            }
            (2, 0x22) => {
                let corr = self.c.st.lock().corr.clone();
                let mut raw = Vec::with_capacity(CORR_SIZE * corr.len());
                for c in corr {
                    for v in c.iter().chain([0, 0, 0, 0].iter()) {
                        raw.extend_from_slice(&v.to_le_bytes());
                    }
                    raw.extend_from_slice(&0i16.to_le_bytes());
                    raw.extend_from_slice(&JS_CORR_BROKEN.to_le_bytes());
                }
                user::put_bytes(arg, &raw).map(|_| 0)
            }
            // JSIOCSAXMAP / JSIOCGAXMAP: axis number → abs code (u8 each).
            (1, 0x31) => {
                let mut raw = alloc::vec![0u8; axes.len()];
                user::get_bytes(arg, &mut raw)?;
                if raw.iter().any(|&a| a as usize >= crate::drivers::evdev::codes::ABS_CNT) {
                    return Err(EINVAL);
                }
                self.c.set_map(Some(raw.into_iter().map(|a| a as u16).collect()), None);
                Ok(0)
            }
            (2, 0x32) => {
                let mut raw = alloc::vec![0u8; size.min(64)];
                for (i, a) in axes.iter().enumerate().take(raw.len()) {
                    raw[i] = *a as u8;
                }
                user::put_bytes(arg, &raw).map(|_| raw.len() as u64)
            }
            // JSIOCSBTNMAP / JSIOCGBTNMAP: button number → key code (u16 each).
            (1, 0x33) => {
                let mut raw = alloc::vec![0u8; buttons.len() * 2];
                user::get_bytes(arg, &mut raw)?;
                let map: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|b| u16::from_le_bytes(*b)).collect();
                if map.iter().any(|&k| k > crate::drivers::evdev::codes::KEY_MAX) {
                    return Err(EINVAL);
                }
                self.c.set_map(None, Some(map));
                Ok(0)
            }
            (2, 0x34) => {
                let mut raw = alloc::vec![0u8; size.min(1024)];
                for (i, b) in buttons.iter().enumerate().take(raw.len() / 2) {
                    raw[i * 2..i * 2 + 2].copy_from_slice(&b.to_le_bytes());
                }
                user::put_bytes(arg, &raw).map(|_| raw.len() as u64)
            }
            _ => Err(EINVAL),
        }
    }

    fn readiness(&self) -> (bool, bool, bool) {
        if self.c.dev.gone() {
            return (true, false, true);
        }
        (self.c.ready(), false, false)
    }

    fn wait_key(&self) -> usize {
        self.c.key()
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        super::node_stat(Dev::Js(self.c.dev.js.unwrap_or(0)))
    }
}
