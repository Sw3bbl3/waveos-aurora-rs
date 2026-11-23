//! `/dev/input/eventN`: reading `struct input_event`s, writing events back
//! (force feedback, lights), and evdev's `ioctl`s.

use super::super::chardev::{self, ioc_dir, ioc_nr, ioc_size, ioc_type, put_str, CharDev};
use super::tree::Dev;
use crate::drivers::evdev::{codes::*, AbsInfo, Client, Effect, Event, Location};
use crate::sync::IrqMutex;
use crate::syscall::user;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, Ordering};

/// `sizeof(struct input_event)` on x86-64.
pub const EVENT_SIZE: usize = 24;
const EV_VERSION: i32 = 0x010001;

const CLOCK_REALTIME: u32 = 0;
const CLOCK_MONOTONIC: u32 = 1;
const CLOCK_BOOTTIME: u32 = 7;

pub struct EvdevFile {
    pub c: Arc<Client>,
    clock: IrqMutex<u32>,
    revoked: AtomicBool,
}

impl EvdevFile {
    pub fn new(c: Arc<Client>) -> EvdevFile {
        EvdevFile { c, clock: IrqMutex::new(CLOCK_REALTIME), revoked: AtomicBool::new(false) }
    }

    fn gone(&self) -> bool {
        self.revoked.load(Ordering::Acquire) || self.c.dev.gone()
    }

    /// An event as `struct input_event`, timed by this reader's clock.
    fn encode(&self, e: &Event, out: &mut [u8]) {
        let ns = match *self.clock.lock() {
            CLOCK_REALTIME => {
                let offset = crate::time::realtime_ns() as i128 - crate::time::now_ns() as i128;
                (e.ns as i128 + offset).max(0) as u64
            }
            _ => e.ns,
        };
        out[0..8].copy_from_slice(&((ns / 1_000_000_000) as i64).to_le_bytes());
        out[8..16].copy_from_slice(&(((ns % 1_000_000_000) / 1000) as i64).to_le_bytes());
        out[16..18].copy_from_slice(&e.ty.to_le_bytes());
        out[18..20].copy_from_slice(&e.code.to_le_bytes());
        out[20..24].copy_from_slice(&e.value.to_le_bytes());
    }

    fn focused(&self) -> bool {
        self.c.focused.load(Ordering::Acquire)
    }
}

/// Bits as Linux's `bits_to_user` gives them: whole longs, at most `max` bytes.
fn put_bits(arg: u64, max: usize, words: &[u64]) -> Result<u64, isize> {
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).take(max).collect();
    user::put_bytes(arg, &bytes)?;
    Ok(bytes.len() as u64)
}

fn get_abs(arg: u64) -> Result<AbsInfo, isize> {
    let v: [i32; 6] = user::get(arg)?;
    Ok(AbsInfo { value: v[0], minimum: v[1], maximum: v[2], fuzz: v[3], flat: v[4], resolution: v[5] })
}

impl CharDev for EvdevFile {
    fn read(&self, out: &mut [u8], nonblock: bool) -> Result<usize, isize> {
        if out.len() < EVENT_SIZE {
            return Err(EINVAL);
        }
        let events = chardev::wait(self.c.key(), nonblock, || {
            if self.gone() {
                return Err(ENODEV);
            }
            let mut q = self.c.q.lock();
            Ok(q.ready().then(|| q.take(out.len() / EVENT_SIZE)))
        })?;
        for (i, e) in events.iter().enumerate() {
            self.encode(e, &mut out[i * EVENT_SIZE..(i + 1) * EVENT_SIZE]);
        }
        Ok(events.len() * EVENT_SIZE)
    }

    fn write(&self, data: &[u8], _nonblock: bool) -> Result<usize, isize> {
        if self.gone() {
            return Err(ENODEV);
        }
        if !data.is_empty() && data.len() < EVENT_SIZE {
            return Err(EINVAL);
        }
        let mut done = 0;
        for e in data.as_chunks::<EVENT_SIZE>().0 {
            let ty = u16::from_le_bytes([e[16], e[17]]);
            let code = u16::from_le_bytes([e[18], e[19]]);
            let value = i32::from_le_bytes([e[20], e[21], e[22], e[23]]);
            self.c.dev.inject(&self.c, ty, code, value);
            done += EVENT_SIZE;
        }
        Ok(done)
    }

    fn ioctl(&self, r: u32, arg: u64) -> Result<u64, isize> {
        if ioc_type(r) != b'E' as u32 {
            return Err(ENOTTY);
        }
        if self.gone() {
            return Err(ENODEV);
        }
        let d = &self.c.dev;
        let (nr, size, dir) = (ioc_nr(r), ioc_size(r), ioc_dir(r));
        let put_int = |v: i32| user::put(arg, &v).map(|_| 0);
        match (dir, nr) {
            // EVIOCGVERSION.
            (2, 0x01) => put_int(EV_VERSION),
            // EVIOCGID.
            (2, 0x02) => user::put(arg, &[d.id.bustype, d.id.vendor, d.id.product, d.id.version]).map(|_| 0),
            // EVIOCGREP / EVIOCSREP.
            (2, 0x03) if d.caps.ev & (1 << EV_REP) != 0 => user::put(arg, &d.repeat_rate()).map(|_| 0),
            (1, 0x03) if d.caps.ev & (1 << EV_REP) != 0 => {
                let rep: [u32; 2] = user::get(arg)?;
                d.set_repeat_rate(rep);
                Ok(0)
            }
            (_, 0x03) => Err(ENOSYS),
            // EVIOCGKEYCODE(_V2) / EVIOCSKEYCODE(_V2): no scan code tables.
            (_, 0x04) => Err(EINVAL),
            // EVIOCGNAME / EVIOCGPHYS / EVIOCGUNIQ.
            (2, 0x06) => put_str(arg, size, &d.name),
            (2, 0x07 | 0x08) => {
                let s = if nr == 0x07 { &d.phys } else { &d.uniq };
                // A device made in software without one has none (Linux: ENOENT).
                if s.is_empty() && matches!(d.location, Location::Virtual) {
                    return Err(ENOENT);
                }
                put_str(arg, size, s)
            }
            // EVIOCGPROP.
            (2, 0x09) => put_bits(arg, size, &[d.caps.prop]),
            // EVIOCGMTSLOTS: { code; values[] }.
            (2, 0x0a) => {
                let code: u32 = user::get(arg)?;
                if code < ABS_MT_FIRST as u32 || code > ABS_MT_TRACKING_ID as u32 || !d.has(EV_ABS, code as u16) {
                    return Err(EINVAL);
                }
                let values = d.mt_values(code as u16);
                let n = (size.saturating_sub(4) / 4).min(values.len());
                let bytes: Vec<u8> = values[..n].iter().flat_map(|v| v.to_le_bytes()).collect();
                user::put_bytes(arg + 4, &bytes)?;
                Ok(0)
            }
            // EVIOCGKEY / EVIOCGLED / EVIOCGSND / EVIOCGSW: the state, and
            // no queued events of the type (the state replaces them).
            (2, 0x18) => {
                self.c.flush(EV_KEY);
                put_bits(arg, size, &d.key_bits(self.focused()))
            }
            (2, 0x19) => {
                self.c.flush(EV_LED);
                put_bits(arg, size, &[d.led_bits()])
            }
            (2, 0x1a) => put_bits(arg, size, &[0]),
            (2, 0x1b) => {
                self.c.flush(EV_SW);
                put_bits(arg, size, &[d.sw_bits()])
            }
            // EVIOCGBIT(type).
            (2, 0x20..=0x3f) => {
                let ty = (nr - 0x20) as u16;
                if !matches!(ty, 0 | EV_KEY | EV_REL | EV_ABS | EV_MSC | EV_LED | EV_SND | EV_FF | EV_SW) {
                    return Err(EINVAL);
                }
                put_bits(arg, size, &super::tree::words(d, ty))
            }
            // EVIOCGABS(axis).
            (2, 0x40..=0x7f) => {
                let code = (nr - 0x40) as u16;
                if !d.has(EV_ABS, code) {
                    return Err(EINVAL);
                }
                let a = d.abs_info(code, self.focused());
                let v = [a.value, a.minimum, a.maximum, a.fuzz, a.flat, a.resolution];
                let bytes: Vec<u8> = v.iter().flat_map(|x| x.to_le_bytes()).take(size.min(24)).collect();
                user::put_bytes(arg, &bytes).map(|_| 0)
            }
            // EVIOCSABS(axis).
            (1, 0xc0..=0xff) => {
                let code = (nr - 0xc0) as u16;
                if !d.has(EV_ABS, code) || code == ABS_MT_SLOT {
                    return Err(EINVAL);
                }
                d.set_abs_info(code, get_abs(arg)?);
                Ok(0)
            }
            // EVIOCSFF: the effect's id is written back.
            (1, 0x80) => {
                let raw: [u8; 48] = user::get(arg)?;
                let id = d.upload(&self.c, Effect { raw })?;
                user::put(arg + 2, &id).map(|_| 0)
            }
            // EVIOCRMFF.
            (1, 0x81) => d.erase(&self.c, arg as i32).map(|_| 0),
            // EVIOCGEFFECTS.
            (2, 0x84) => put_int(d.ff_max as i32),
            // EVIOCGRAB.
            (1, 0x90) => d.grab(&self.c, arg != 0).map(|_| 0),
            // EVIOCREVOKE: this descriptor reads nothing more.
            (1, 0x91) => {
                if arg != 0 {
                    return Err(EINVAL);
                }
                self.revoked.store(true, Ordering::Release);
                let _ = d.grab(&self.c, false);
                crate::proc::readiness::notify(self.c.key());
                Ok(0)
            }
            // EVIOCSCLOCKID: what was queued (timed by the old clock) goes.
            (1, 0xa0) => {
                let clock: i32 = user::get(arg)?;
                let clock = clock as u32;
                if !matches!(clock, CLOCK_REALTIME | CLOCK_MONOTONIC | CLOCK_BOOTTIME) {
                    return Err(EINVAL);
                }
                let mut c = self.clock.lock();
                if *c != clock {
                    *c = clock;
                    let mut q = self.c.q.lock();
                    if q.ready() {
                        q.take(usize::MAX);
                    }
                }
                Ok(0)
            }
            _ => Err(EINVAL),
        }
    }

    fn readiness(&self) -> (bool, bool, bool) {
        if self.gone() {
            return (true, false, true);
        }
        (self.c.q.lock().ready(), true, false)
    }

    fn wait_key(&self) -> usize {
        self.c.key()
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        super::node_stat(Dev::Event(self.c.dev.num))
    }
}
