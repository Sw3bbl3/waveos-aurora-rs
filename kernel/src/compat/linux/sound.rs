//! ALSA, the way Linux programs play sound: `/dev/snd/controlC0` and
//! `/dev/snd/pcmC0D0p`, with the kernel's ioctls, so the ALSA library
//! (`libasound`) and everything over it (Firefox's cubeb, GStreamer's
//! `alsasink`, SDL, PortAudio, Wine) play through WaveOS's mixer
//! unchanged. There is one card with one playback device: the mixer. Any
//! number of programs may open it at once, each getting a stream of its
//! own, so nothing like `dmix` is needed; `/etc/asound.conf` makes the
//! default device `plug` over it, which converts what the card doesn't take
//! (other rates, more channels).
//!
//! The card takes 48 kHz, one or two channels, 16-bit, 32-bit or float
//! samples, written (`WRITEI_FRAMES`, `write`) or through the mapped
//! buffer (`mmap` at offset 0, committed by moving the application
//! pointer with `SYNC_PTR` or `FORWARD`). The status and control pages
//! can't be mapped, so libasound uses `SYNC_PTR` for them, as it does on
//! architectures without them.
//!
//! Positions, as a sound card reports them: the hardware pointer is what
//! the mixer has taken from the stream (it moves in steps, once per mixer
//! cycle, hence `INFO_BATCH`), and the delay adds what is mixed into the
//! device's ring but not heard yet ([`audio::latency_frames`]), so a
//! player that follows `snd_pcm_delay` keeps pictures in step with the
//! sound. Underruns are real: a stream the mixer finds empty while running
//! goes to `XRUN` when its stop threshold says so.

use super::chardev::{ioc_nr, ioc_size, ioc_type, CharDev};
use super::shm::ShmFile;
use crate::drivers::audio;
use crate::mm::PAGE_SIZE;
use crate::proc::readiness;
use crate::sync::Mutex;
use crate::syscall::user;
use crate::time;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub const MAJOR: u64 = 116;
pub const CONTROL_MINOR: u64 = 0;
pub const PCM_MINOR: u64 = 16;
const EBADFD: isize = 77;

const PCM_VERSION: u32 = 2 << 16 | 15;
const CTL_VERSION: u32 = 2 << 16 | 8;

const STATE_OPEN: u32 = 0;
const STATE_SETUP: u32 = 1;
const STATE_PREPARED: u32 = 2;
const STATE_RUNNING: u32 = 3;
const STATE_XRUN: u32 = 4;
const STATE_DRAINING: u32 = 5;
const STATE_PAUSED: u32 = 6;

const ACCESS_MMAP_INTERLEAVED: u32 = 0;
const ACCESS_RW_INTERLEAVED: u32 = 3;
const FORMAT_S16_LE: u32 = 2;
const FORMAT_S32_LE: u32 = 10;
const FORMAT_FLOAT_LE: u32 = 14;

// SNDRV_PCM_INFO_*: mappable, interleaved, pausable; the pointer moves in
// steps (BATCH).
const INFO: u32 = 0x1 | 0x2 | 0x10 | 0x100 | 0x10000 | 0x80000;

/// The device names in `/dev/snd`, if there is a sound device.
pub fn node_names() -> Vec<String> {
    if !audio::present() {
        return Vec::new();
    }
    vec![String::from("controlC0"), String::from("pcmC0D0p")]
}

/// `st_rdev` of a `/dev/snd` node.
pub fn node_rdev(guest: &str) -> Option<u64> {
    let minor = match guest {
        "/dev/snd/controlC0" => CONTROL_MINOR,
        "/dev/snd/pcmC0D0p" => PCM_MINOR,
        _ => return None,
    };
    audio::present().then(|| super::fs::makedev(MAJOR, minor))
}

/// Opens a `/dev/snd` node (None: not one).
pub fn open(guest: &str) -> Option<Result<Arc<dyn CharDev>, isize>> {
    match guest {
        "/dev/snd/controlC0" => Some(if audio::present() { Ok(Arc::new(Control) as Arc<dyn CharDev>) } else { Err(ENODEV) }),
        "/dev/snd/pcmC0D0p" => Some((|| {
            crate::sandbox::require(aurora_abi::sandbox::AUDIO_OUT)?;
            let stream = audio::open()?;
            // Nothing plays until the program starts the stream.
            stream.set_paused(true);
            let pcm = Arc::new(Pcm { stream, st: Mutex::new(State::default()) });
            pcm.stream.set_wake_key(readiness::key(Arc::as_ptr(&pcm)));
            Ok(pcm as Arc<dyn CharDev>)
        })()),
        _ => None,
    }
}

/// The mapped buffer of the PCM open as descriptor `fd`, for `mmap` at
/// `offset` (None: not a PCM).
pub fn mmap_buffer(dev: &dyn CharDev, offset: u64) -> Option<Result<Arc<ShmFile>, isize>> {
    let pcm = dev.pcm()?;
    if offset != 0 {
        // The status and control pages (0x8000_0000, 0x8100_0000): not
        // mappable, so libasound falls back to SYNC_PTR.
        return Some(Err(ENXIO));
    }
    Some(pcm.st.lock().buf.clone().ok_or(EBADFD))
}

/// The ALSA view of `/proc/asound`.
pub fn proc_file(name: &str) -> Option<String> {
    if !audio::present() {
        return None;
    }
    Some(String::from(match name {
        "cards" => " 0 [WaveOS         ]: WaveOS - WaveOS\n                      WaveOS sound mixer\n",
        "devices" => "  0: [ 0]   : control\n 16: [ 0- 0]: digital audio playback\n",
        "pcm" => "00-00: WaveOS : WaveOS mixer : playback 1\n",
        "version" => "Advanced Linux Sound Architecture Driver Version k6.1.0.\n",
        _ => return None,
    }))
}

fn put_cstr(out: &mut [u8], s: &str) {
    let n = s.len().min(out.len() - 1);
    out[..n].copy_from_slice(&s.as_bytes()[..n]);
}

fn get_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}
fn get_u64(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(b[off..off + 8].try_into().unwrap())
}
fn set_u32(b: &mut [u8], off: usize, v: u32) {
    b[off..off + 4].copy_from_slice(&v.to_le_bytes());
}
fn set_u64(b: &mut [u8], off: usize, v: u64) {
    b[off..off + 8].copy_from_slice(&v.to_le_bytes());
}
fn set_timespec(b: &mut [u8], off: usize, ns: u64) {
    set_u64(b, off, ns / 1_000_000_000);
    set_u64(b, off + 8, ns % 1_000_000_000);
}

/// `struct snd_pcm_info` (288 bytes) of the one playback device.
fn pcm_info(b: &mut [u8]) {
    b.fill(0);
    // device, subdevice, stream (playback), card: all 0.
    put_cstr(&mut b[16..80], "WaveOS");
    put_cstr(&mut b[80..160], "WaveOS mixer");
    put_cstr(&mut b[160..192], "subdevice #0");
    set_u32(b, 200, 1); // subdevices_count
    set_u32(b, 204, 1); // subdevices_avail
}

// ------------------------------------------------------------------ control

/// `/dev/snd/controlC0`: the card's description and its one PCM. It has
/// no mixer controls (WaveOS's volume is the system's).
struct Control;

impl CharDev for Control {
    fn read(&self, _out: &mut [u8], _nonblock: bool) -> Result<usize, isize> {
        // No control events ever come.
        Err(EAGAIN)
    }

    fn ioctl(&self, r: u32, arg: u64) -> Result<u64, isize> {
        if ioc_type(r) != b'U' as u32 {
            return Err(ENOTTY);
        }
        match ioc_nr(r) {
            // PVERSION
            0x00 => user::put(arg, &CTL_VERSION).map(|_| 0),
            // CARD_INFO (struct snd_ctl_card_info, 376 bytes)
            0x01 => {
                let mut b = vec![0u8; 376];
                put_cstr(&mut b[8..24], "WaveOS");
                put_cstr(&mut b[24..40], "WaveOS");
                put_cstr(&mut b[40..72], "WaveOS");
                put_cstr(&mut b[72..152], "WaveOS sound mixer");
                put_cstr(&mut b[168..248], "WaveOS");
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // ELEM_LIST: no controls (offset, space, used, count).
            0x10 => {
                let mut b = [0u8; 16];
                user::get_bytes(arg, &mut b)?;
                set_u32(&mut b, 8, 0);
                set_u32(&mut b, 12, 0);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // ELEM_INFO, ELEM_READ, ELEM_WRITE, LOCK, UNLOCK: no such control.
            0x11..=0x15 => Err(ENOENT),
            // SUBSCRIBE_EVENTS: nothing to subscribe to, but accepted.
            0x16 => {
                if ioc_size(r) == 4 && r >> 30 & 2 != 0 {
                    user::put(arg, &0i32)?;
                }
                Ok(0)
            }
            // HWDEP_NEXT_DEVICE, RAWMIDI_NEXT_DEVICE: none.
            0x20 | 0x40 => user::put(arg, &-1i32).map(|_| 0),
            // PCM_NEXT_DEVICE: device 0, then none.
            0x30 => {
                let cur: i32 = user::get(arg)?;
                user::put(arg, &if cur < 0 { 0i32 } else { -1 }).map(|_| 0)
            }
            // PCM_INFO: the playback device (no capture).
            0x31 => {
                let mut b = vec![0u8; 288];
                user::get_bytes(arg, &mut b)?;
                let (device, sub, stream) = (get_u32(&b, 0), get_u32(&b, 4) as i32, get_u32(&b, 8));
                if device != 0 || stream != 0 || sub > 0 {
                    return Err(ENOENT);
                }
                pcm_info(&mut b);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // PCM_PREFER_SUBDEVICE, RAWMIDI_PREFER_SUBDEVICE, POWER.
            0x32 | 0x42 | 0xD0 => Ok(0),
            // POWER_STATE: D0.
            0xD1 => user::put(arg, &0i32).map(|_| 0),
            _ => Err(ENOTTY),
        }
    }

    fn readiness(&self) -> (bool, bool, bool) {
        (false, true, false)
    }

    fn wait_key(&self) -> usize {
        readiness::key(self as *const Self)
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        (0o020666, super::fs::makedev(MAJOR, CONTROL_MINOR), None)
    }
}

// --------------------------------------------------------- hw_params rules

/// `struct snd_interval`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Iv {
    min: u32,
    max: u32,
    openmin: bool,
    openmax: bool,
    integer: bool,
    empty: bool,
}

impl Iv {
    const fn closed(min: u32, max: u32, integer: bool) -> Iv {
        Iv { min, max, openmin: false, openmax: false, integer, empty: false }
    }

    fn read(b: &[u8], off: usize) -> Iv {
        let bits = get_u32(b, off + 8);
        Iv {
            min: get_u32(b, off),
            max: get_u32(b, off + 4),
            openmin: bits & 1 != 0,
            openmax: bits & 2 != 0,
            integer: bits & 4 != 0,
            empty: bits & 8 != 0,
        }
    }

    fn write(&self, b: &mut [u8], off: usize) {
        set_u32(b, off, self.min);
        set_u32(b, off + 4, self.max);
        let bits = self.openmin as u32 | (self.openmax as u32) << 1 | (self.integer as u32) << 2 | (self.empty as u32) << 3;
        set_u32(b, off + 8, bits);
    }

    fn none() -> Iv {
        Iv { empty: true, ..Iv::closed(0, 0, false) }
    }

    fn check_empty(&self) -> bool {
        self.min > self.max || (self.min == self.max && (self.openmin || self.openmax))
    }

    fn single(&self) -> bool {
        !self.empty && (self.min == self.max || (self.min.wrapping_add(1) == self.max && (self.openmin || self.openmax)))
    }

    fn value(&self) -> u32 {
        if self.openmin && !self.openmax {
            self.max
        } else {
            self.min
        }
    }

    fn test(&self, v: u32) -> bool {
        !(self.min > v || (self.min == v && self.openmin) || self.max < v || (self.max == v && self.openmax))
    }

    /// `snd_interval_refine`: narrows to `v`; whether it changed.
    fn refine(&mut self, v: &Iv) -> Result<bool, isize> {
        if self.empty || v.empty {
            *self = Iv::none();
            return Err(EINVAL);
        }
        let mut changed = false;
        if self.min < v.min {
            self.min = v.min;
            self.openmin = v.openmin;
            changed = true;
        } else if self.min == v.min && !self.openmin && v.openmin {
            self.openmin = true;
            changed = true;
        }
        if self.max > v.max {
            self.max = v.max;
            self.openmax = v.openmax;
            changed = true;
        } else if self.max == v.max && !self.openmax && v.openmax {
            self.openmax = true;
            changed = true;
        }
        if !self.integer && v.integer {
            self.integer = true;
            changed = true;
        }
        if self.integer {
            if self.openmin {
                self.min = self.min.saturating_add(1);
                self.openmin = false;
            }
            if self.openmax {
                self.max = self.max.saturating_sub(1);
                self.openmax = false;
            }
        } else if !self.openmin && !self.openmax && self.min == self.max {
            self.integer = true;
        }
        if self.check_empty() {
            *self = Iv::none();
            return Err(EINVAL);
        }
        Ok(changed)
    }

    /// Narrows to its first (smallest) value (`snd_interval_refine_first`:
    /// an interval that holds one value already, such as (21333, 21334)
    /// for 1024 frames at 48 kHz in microseconds, stays as it is).
    fn first(&mut self) -> Result<bool, isize> {
        if self.empty {
            return Err(EINVAL);
        }
        if self.single() {
            return Ok(false);
        }
        let last_max = self.max;
        self.max = if self.openmin { self.min + 1 } else { self.min };
        // Only exclude the end if it was excluded before.
        self.openmax = self.openmax && self.max >= last_max;
        Ok(true)
    }

    /// Narrows to its last (largest) value (`snd_interval_refine_last`).
    fn last(&mut self) -> Result<bool, isize> {
        if self.empty {
            return Err(EINVAL);
        }
        if self.single() {
            return Ok(false);
        }
        let last_min = self.min;
        self.min = if self.openmax { self.max - 1 } else { self.max };
        self.openmin = self.openmin && self.min <= last_min;
        Ok(true)
    }
}

fn mul32(a: u32, b: u32) -> u32 {
    a.saturating_mul(b)
}

/// (a / b, remainder); b = 0 gives the largest value.
fn div32(a: u32, b: u32) -> (u32, u32) {
    match (a.checked_div(b), a.checked_rem(b)) {
        (Some(q), Some(r)) => (q, r),
        _ => (u32::MAX, 0),
    }
}

/// (a * b / c, remainder), saturating.
fn muldiv32(a: u32, b: u32, c: u32) -> (u32, u32) {
    if c == 0 {
        return (u32::MAX, 0);
    }
    let n = a as u64 * b as u64;
    let q = n / c as u64;
    if q > u32::MAX as u64 {
        (u32::MAX, 0)
    } else {
        (q as u32, (n % c as u64) as u32)
    }
}

/// `snd_interval_mul`: a × b.
fn iv_mul(a: &Iv, b: &Iv) -> Iv {
    if a.empty || b.empty {
        return Iv::none();
    }
    Iv {
        min: mul32(a.min, b.min),
        openmin: a.openmin || b.openmin,
        max: mul32(a.max, b.max),
        openmax: a.openmax || b.openmax,
        integer: a.integer && b.integer,
        empty: false,
    }
}

/// `snd_interval_div`: a ÷ b.
fn iv_div(a: &Iv, b: &Iv) -> Iv {
    if a.empty || b.empty {
        return Iv::none();
    }
    let (min, r) = div32(a.min, b.max);
    let mut c = Iv { min, openmin: r != 0 || a.openmin || b.openmax, ..Iv::closed(0, 0, false) };
    if b.min > 0 {
        let (max, r) = div32(a.max, b.min);
        c.max = max;
        if r != 0 {
            c.max = c.max.saturating_add(1);
            c.openmax = true;
        } else {
            c.openmax = a.openmax || b.openmin;
        }
    } else {
        c.max = u32::MAX;
        c.openmax = false;
    }
    c
}

/// `snd_interval_muldivk`: a × b ÷ k.
fn iv_muldivk(a: &Iv, b: &Iv, k: u32) -> Iv {
    if a.empty || b.empty {
        return Iv::none();
    }
    let (min, r) = muldiv32(a.min, b.min, k);
    let mut c = Iv { min, openmin: r != 0 || a.openmin || b.openmin, ..Iv::closed(0, 0, false) };
    let (max, r) = muldiv32(a.max, b.max, k);
    c.max = max;
    if r != 0 {
        c.max = c.max.saturating_add(1);
        c.openmax = true;
    } else {
        c.openmax = a.openmax || b.openmax;
    }
    c
}

/// `snd_interval_mulkdiv`: a × k ÷ b.
fn iv_mulkdiv(a: &Iv, k: u32, b: &Iv) -> Iv {
    if a.empty || b.empty {
        return Iv::none();
    }
    let (min, r) = muldiv32(a.min, k, b.max);
    let mut c = Iv { min, openmin: r != 0 || a.openmin || b.openmax, ..Iv::closed(0, 0, false) };
    if b.min > 0 {
        let (max, r) = muldiv32(a.max, k, b.min);
        c.max = max;
        if r != 0 {
            c.max = c.max.saturating_add(1);
            c.openmax = true;
        } else {
            c.openmax = a.openmax || b.openmin;
        }
    } else {
        c.max = u32::MAX;
        c.openmax = false;
    }
    c
}

// Parameter numbers (masks 0–2, intervals 8–19).
const ACCESS: usize = 0;
const FORMAT: usize = 1;
const SUBFORMAT: usize = 2;
const SAMPLE_BITS: usize = 8;
const FRAME_BITS: usize = 9;
const CHANNELS: usize = 10;
const RATE: usize = 11;
const PERIOD_TIME: usize = 12;
const PERIOD_SIZE: usize = 13;
const PERIOD_BYTES: usize = 14;
const PERIODS: usize = 15;
const BUFFER_TIME: usize = 16;
const BUFFER_SIZE: usize = 17;
const BUFFER_BYTES: usize = 18;

const HW_PARAMS_SIZE: usize = 608;

/// `struct snd_pcm_hw_params`: the masks (first word of each: no
/// parameter of ours has values past 31) and the intervals.
struct HwParams {
    raw: Vec<u8>,
    masks: [u32; 3],
    iv: [Iv; 12],
    changed: u32,
}

fn width(format: u32) -> u32 {
    match format {
        FORMAT_S16_LE => 16,
        FORMAT_S32_LE | FORMAT_FLOAT_LE => 32,
        _ => 0,
    }
}

impl HwParams {
    fn read(arg: u64) -> Result<HwParams, isize> {
        let mut raw = vec![0u8; HW_PARAMS_SIZE];
        user::get_bytes(arg, &mut raw)?;
        let masks = core::array::from_fn(|i| get_u32(&raw, 4 + i * 32));
        let iv = core::array::from_fn(|i| Iv::read(&raw, 260 + i * 12));
        Ok(HwParams { raw, masks, iv, changed: 0 })
    }

    fn write(mut self, arg: u64) -> Result<(), isize> {
        for (i, m) in self.masks.iter().enumerate() {
            let at = 4 + i * 32;
            self.raw[at..at + 32].fill(0);
            set_u32(&mut self.raw, at, *m);
        }
        for (i, v) in self.iv.iter().enumerate() {
            v.write(&mut self.raw, 260 + i * 12);
        }
        set_u32(&mut self.raw, 512, 0); // rmask
        set_u32(&mut self.raw, 516, self.changed); // cmask
        set_u32(&mut self.raw, 520, INFO);
        if get_u32(&self.raw, 524) == 0 && self.iv(SAMPLE_BITS).single() {
            let bits = self.iv(SAMPLE_BITS).value();
            set_u32(&mut self.raw, 524, bits);
        }
        if get_u32(&self.raw, 532) == 0 && self.iv(RATE).single() {
            let rate = self.iv(RATE).value();
            set_u32(&mut self.raw, 528, rate);
            set_u32(&mut self.raw, 532, 1);
        }
        set_u64(&mut self.raw, 536, 0); // fifo_size
        user::put_bytes(arg, &self.raw)
    }

    fn iv(&self, p: usize) -> &Iv {
        &self.iv[p - SAMPLE_BITS]
    }

    fn set(&mut self, p: usize, v: Iv) -> Result<(), isize> {
        if self.iv[p - SAMPLE_BITS].refine(&v)? {
            self.changed |= 1 << p;
        }
        Ok(())
    }

    fn mask(&mut self, p: usize, allowed: u32) -> Result<(), isize> {
        let m = self.masks[p] & allowed;
        if m == 0 {
            return Err(EINVAL);
        }
        if m != self.masks[p] {
            self.masks[p] = m;
            self.changed |= 1 << p;
        }
        Ok(())
    }

    /// What the card can do, then every relation between the parameters
    /// (the kernel's rules) until nothing changes.
    fn refine(&mut self) -> Result<(), isize> {
        self.mask(ACCESS, 1 << ACCESS_MMAP_INTERLEAVED | 1 << ACCESS_RW_INTERLEAVED)?;
        self.mask(FORMAT, 1 << FORMAT_S16_LE | 1 << FORMAT_S32_LE | 1 << FORMAT_FLOAT_LE)?;
        self.mask(SUBFORMAT, 1)?;
        self.set(SAMPLE_BITS, Iv::closed(16, 32, true))?;
        self.set(FRAME_BITS, Iv::closed(16, 64, true))?;
        self.set(CHANNELS, Iv::closed(1, 2, true))?;
        self.set(RATE, Iv::closed(audio::RATE as u32, audio::RATE as u32, true))?;
        self.set(PERIOD_SIZE, Iv::closed(32, 16384, true))?;
        self.set(PERIODS, Iv::closed(2, 1024, true))?;
        self.set(BUFFER_SIZE, Iv::closed(1024, 32768, true))?;
        for _ in 0..64 {
            let before = (self.masks, self.iv);
            self.rules()?;
            if (self.masks, self.iv) == before {
                return Ok(());
            }
        }
        Ok(())
    }

    fn rules(&mut self) -> Result<(), isize> {
        // FORMAT from SAMPLE_BITS, and back.
        let bits = *self.iv(SAMPLE_BITS);
        let mut formats = self.masks[FORMAT];
        for f in [FORMAT_S16_LE, FORMAT_S32_LE, FORMAT_FLOAT_LE] {
            if formats & 1 << f != 0 && !bits.test(width(f)) {
                formats &= !(1 << f);
            }
        }
        self.mask(FORMAT, formats)?;
        let widths = (0..32).filter(|f| self.masks[FORMAT] & 1 << f != 0).map(width).filter(|&w| w > 0);
        let (lo, hi) = widths.fold((u32::MAX, 0), |(lo, hi), w| (lo.min(w), hi.max(w)));
        self.set(SAMPLE_BITS, Iv::closed(lo, hi, true))?;
        let iv = |s: &Self, p: usize| *s.iv(p);
        let v = iv_div(&iv(self, FRAME_BITS), &iv(self, CHANNELS));
        self.set(SAMPLE_BITS, v)?;
        let v = iv_mul(&iv(self, SAMPLE_BITS), &iv(self, CHANNELS));
        self.set(FRAME_BITS, v)?;
        let v = iv_mulkdiv(&iv(self, PERIOD_BYTES), 8, &iv(self, PERIOD_SIZE));
        self.set(FRAME_BITS, v)?;
        let v = iv_mulkdiv(&iv(self, BUFFER_BYTES), 8, &iv(self, BUFFER_SIZE));
        self.set(FRAME_BITS, v)?;
        let v = iv_div(&iv(self, FRAME_BITS), &iv(self, SAMPLE_BITS));
        self.set(CHANNELS, v)?;
        let v = iv_mulkdiv(&iv(self, PERIOD_SIZE), 1_000_000, &iv(self, PERIOD_TIME));
        self.set(RATE, v)?;
        let v = iv_mulkdiv(&iv(self, BUFFER_SIZE), 1_000_000, &iv(self, BUFFER_TIME));
        self.set(RATE, v)?;
        let v = iv_div(&iv(self, BUFFER_SIZE), &iv(self, PERIOD_SIZE));
        self.set(PERIODS, v)?;
        let v = iv_div(&iv(self, BUFFER_SIZE), &iv(self, PERIODS));
        self.set(PERIOD_SIZE, v)?;
        let v = iv_mulkdiv(&iv(self, PERIOD_BYTES), 8, &iv(self, FRAME_BITS));
        self.set(PERIOD_SIZE, v)?;
        let v = iv_muldivk(&iv(self, PERIOD_TIME), &iv(self, RATE), 1_000_000);
        self.set(PERIOD_SIZE, v)?;
        let v = iv_mul(&iv(self, PERIOD_SIZE), &iv(self, PERIODS));
        self.set(BUFFER_SIZE, v)?;
        let v = iv_mulkdiv(&iv(self, BUFFER_BYTES), 8, &iv(self, FRAME_BITS));
        self.set(BUFFER_SIZE, v)?;
        let v = iv_muldivk(&iv(self, BUFFER_TIME), &iv(self, RATE), 1_000_000);
        self.set(BUFFER_SIZE, v)?;
        let v = iv_muldivk(&iv(self, PERIOD_SIZE), &iv(self, FRAME_BITS), 8);
        self.set(PERIOD_BYTES, v)?;
        let v = iv_muldivk(&iv(self, BUFFER_SIZE), &iv(self, FRAME_BITS), 8);
        self.set(BUFFER_BYTES, v)?;
        let v = iv_mulkdiv(&iv(self, PERIOD_SIZE), 1_000_000, &iv(self, RATE));
        self.set(PERIOD_TIME, v)?;
        let v = iv_mulkdiv(&iv(self, BUFFER_SIZE), 1_000_000, &iv(self, RATE));
        self.set(BUFFER_TIME, v)?;
        Ok(())
    }

    /// `snd_pcm_hw_params_choose`: one value for each parameter left open.
    fn choose(&mut self) -> Result<Config, isize> {
        for p in [ACCESS, FORMAT, SUBFORMAT] {
            let m = self.masks[p];
            self.masks[p] = m.isolate_lowest_one();
            self.refine()?;
        }
        for p in [CHANNELS, RATE, PERIOD_TIME, BUFFER_SIZE] {
            let mut v = *self.iv(p);
            if p == BUFFER_SIZE {
                v.last()?;
            } else {
                v.first()?;
            }
            self.set(p, v)?;
            self.refine()?;
        }
        for p in [CHANNELS, PERIOD_SIZE, BUFFER_SIZE, PERIODS] {
            if !self.iv(p).single() {
                return Err(EINVAL);
            }
        }
        let format = self.masks[FORMAT].trailing_zeros();
        let channels = self.iv(CHANNELS).value();
        let buffer = self.iv(BUFFER_SIZE).value() as u64;
        let mut boundary = buffer;
        while boundary * 2 <= i64::MAX as u64 - buffer {
            boundary *= 2;
        }
        Ok(Config {
            format,
            channels,
            frame_bytes: (width(format) / 8 * channels) as usize,
            period: self.iv(PERIOD_SIZE).value() as u64,
            buffer,
            boundary,
        })
    }
}

// ---------------------------------------------------------------------- pcm

#[derive(Clone, Copy)]
struct Config {
    format: u32,
    channels: u32,
    frame_bytes: usize,
    period: u64,
    buffer: u64,
    /// Where the pointers wrap (as the kernel computes it).
    boundary: u64,
}

#[derive(Clone, Copy, Default)]
struct Sw {
    avail_min: u64,
    start_threshold: u64,
    stop_threshold: u64,
}

#[derive(Default)]
struct State {
    state: u32,
    cfg: Option<Config>,
    sw: Sw,
    /// Frames the program has written since it opened the device
    /// (unwrapped: the ioctls see it from `base`, modulo the boundary).
    appl: u64,
    /// The stream's position when the hardware parameters were set: the
    /// pointers the program sees start from 0 there, as a freshly set up
    /// card's do.
    base: u64,
    /// The mapped buffer.
    buf: Option<Arc<ShmFile>>,
    trigger_ns: u64,
    avail_max: u64,
    /// Starts of `XRUN`s, for the log.
    xruns: u64,
}

pub struct Pcm {
    stream: Arc<audio::Stream>,
    st: Mutex<State>,
}

impl Pcm {
    /// The mixer's position in the stream: frames it has taken.
    fn hw(&self) -> u64 {
        self.stream.progress().0
    }

    /// Frames the program may write now (and the hardware pointer).
    fn avail(&self, st: &State) -> (u64, u64) {
        let hw = self.hw();
        let buffer = st.cfg.map_or(0, |c| c.buffer);
        (buffer.saturating_sub(st.appl.saturating_sub(hw)), hw)
    }

    /// Goes to `XRUN` if the stream ran dry while running (as the kernel
    /// does when the stop threshold is reached).
    fn check_xrun(&self, st: &mut State) {
        if matches!(st.state, STATE_RUNNING | STATE_DRAINING) {
            let (avail, _) = self.avail(st);
            st.avail_max = st.avail_max.max(avail);
            let Some(cfg) = st.cfg else { return };
            if st.sw.stop_threshold < cfg.boundary && avail >= st.sw.stop_threshold {
                if st.state == STATE_DRAINING {
                    st.state = STATE_SETUP;
                } else {
                    st.state = STATE_XRUN;
                    st.xruns += 1;
                }
                st.trigger_ns = time::now_ns();
                self.stream.set_paused(true);
            }
        }
    }

    fn start(&self, st: &mut State) {
        st.state = STATE_RUNNING;
        st.trigger_ns = time::now_ns();
        self.stream.set_paused(false);
    }

    /// Drops what hasn't been mixed: the application pointer comes back to
    /// the hardware pointer.
    fn drop_queued(&self, st: &mut State) {
        self.stream.drop_queued();
        st.appl = self.hw();
    }

    /// The delay the program should reckon with: what it wrote and the
    /// mixer hasn't taken, and what the mixer took and isn't heard yet.
    fn delay(&self, st: &State) -> u64 {
        let queued = st.appl.saturating_sub(self.hw());
        match st.state {
            STATE_RUNNING | STATE_DRAINING => queued + audio::latency_frames(),
            STATE_PREPARED | STATE_PAUSED => queued,
            _ => 0,
        }
    }

    /// Converts `frames` frames of the program's samples to the mixer's
    /// (16-bit stereo) and queues them.
    fn queue(&self, cfg: &Config, data: &[u8]) {
        let frames = data.len() / cfg.frame_bytes;
        let mut out = Vec::with_capacity(frames * 2);
        let ch = cfg.channels as usize;
        let sample = |i: usize| -> i16 {
            match cfg.format {
                FORMAT_S16_LE => i16::from_le_bytes([data[i * 2], data[i * 2 + 1]]),
                FORMAT_S32_LE => (i32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()) >> 16) as i16,
                _ => {
                    let f = f32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap());
                    (f * 32767.0).clamp(-32768.0, 32767.0) as i16
                }
            }
        };
        for f in 0..frames {
            let l = sample(f * ch);
            let r = if ch > 1 { sample(f * ch + 1) } else { l };
            out.push(l);
            out.push(r);
        }
        self.stream.push(&out);
    }

    /// Queues `frames` frames from the mapped buffer, starting at the
    /// application pointer (an `mmap` program committing what it wrote).
    fn commit(&self, st: &mut State, frames: u64) {
        let (Some(cfg), Some(buf)) = (st.cfg, st.buf.clone()) else { return };
        let mut left = frames;
        while left > 0 {
            let at = (st.appl - st.base) % cfg.buffer;
            let n = left.min(cfg.buffer - at);
            let mut data = vec![0u8; n as usize * cfg.frame_bytes];
            buf.read_at(at * cfg.frame_bytes as u64, &mut data);
            self.queue(&cfg, &data);
            st.appl += n;
            left -= n;
        }
    }

    /// The program moved the application pointer to `to` (modulo the
    /// boundary): forward is a commit, back a rewind.
    fn move_appl(&self, st: &mut State, to: u64) {
        let Some(cfg) = st.cfg else { return };
        let cur = (st.appl - st.base) % cfg.boundary;
        if to == cur || to >= cfg.boundary {
            return;
        }
        let ahead = (to + cfg.boundary - cur) % cfg.boundary;
        let (avail, _) = self.avail(st);
        if ahead <= avail {
            self.commit(st, ahead);
            self.maybe_start(st);
        } else {
            let back = cfg.boundary - ahead;
            let n = self.stream.unqueue(back as usize * 2) as u64 / 2;
            st.appl -= n;
        }
    }

    fn maybe_start(&self, st: &mut State) {
        if st.state == STATE_PREPARED && st.appl.saturating_sub(self.hw()) >= st.sw.start_threshold.max(1) {
            self.start(st);
        }
    }

    /// `WRITEI_FRAMES` and `write`: queues up to `frames` frames, which
    /// `get(offset, len)` reads, waiting for room unless `nonblock`.
    fn write_frames(
        &self,
        frames: u64,
        nonblock: bool,
        get: impl Fn(usize, usize) -> Result<Vec<u8>, isize>,
    ) -> Result<u64, isize> {
        let mut done = 0u64;
        loop {
            let watch = readiness::Watch::new(&[self.wait_key()]);
            {
                let mut st = self.st.lock();
                self.check_xrun(&mut st);
                let cfg = match st.state {
                    STATE_PREPARED | STATE_RUNNING | STATE_PAUSED => st.cfg.ok_or(EBADFD)?,
                    STATE_XRUN => return if done > 0 { Ok(done) } else { Err(EPIPE) },
                    _ => return Err(EBADFD),
                };
                let (avail, _) = self.avail(&st);
                let n = avail.min(frames - done);
                if n > 0 {
                    let data = get(done as usize * cfg.frame_bytes, n as usize * cfg.frame_bytes)?;
                    self.queue(&cfg, &data);
                    st.appl += n;
                    done += n;
                    self.maybe_start(&mut st);
                }
                if done == frames {
                    return Ok(done);
                }
                if nonblock {
                    return if done > 0 { Ok(done) } else { Err(EAGAIN) };
                }
            }
            if crate::proc::interrupted() {
                return if done > 0 { Ok(done) } else { Err(EINTR) };
            }
            watch.wait(1000);
        }
    }

    /// `DRAIN`: plays out what is queued, then stops.
    fn drain(&self) -> Result<u64, isize> {
        {
            let mut st = self.st.lock();
            match st.state {
                STATE_OPEN | STATE_SETUP => return Err(EBADFD),
                STATE_XRUN => {
                    st.state = STATE_SETUP;
                    return Ok(0);
                }
                STATE_PREPARED => {
                    if st.appl == self.hw() {
                        st.state = STATE_SETUP;
                        return Ok(0);
                    }
                    self.start(&mut st);
                }
                STATE_PAUSED => self.start(&mut st),
                _ => {}
            }
            st.state = STATE_DRAINING;
        }
        loop {
            let watch = readiness::Watch::new(&[self.wait_key()]);
            if self.stream.queued() == 0 || self.st.lock().state != STATE_DRAINING {
                break;
            }
            if crate::proc::interrupted() {
                return Err(EINTR);
            }
            watch.wait(100);
        }
        // What the mixer took is heard within its lead.
        let left_ms = audio::latency_frames() * 1000 / audio::RATE as u64;
        crate::sched::sleep_ms(left_ms);
        let mut st = self.st.lock();
        if st.state == STATE_DRAINING {
            st.state = STATE_SETUP;
            self.stream.set_paused(true);
        }
        Ok(0)
    }

    /// `HWSYNC` (alone or in `SYNC_PTR`): whether the state allows it.
    fn hwsync(&self, st: &mut State) -> Result<(), isize> {
        self.check_xrun(st);
        match st.state {
            STATE_RUNNING | STATE_DRAINING | STATE_PREPARED | STATE_PAUSED => Ok(()),
            STATE_XRUN => Err(EPIPE),
            _ => Err(EBADFD),
        }
    }

    /// `struct snd_pcm_status` (152 bytes).
    fn status(&self, b: &mut [u8]) {
        let mut st = self.st.lock();
        self.check_xrun(&mut st);
        let boundary = st.cfg.map_or(u64::MAX, |c| c.boundary);
        let (avail, hw) = self.avail(&st);
        let now = time::now_ns();
        b.fill(0);
        set_u32(b, 0, st.state);
        set_timespec(b, 8, st.trigger_ns);
        set_timespec(b, 24, now);
        set_u64(b, 40, (st.appl - st.base) % boundary);
        set_u64(b, 48, hw.saturating_sub(st.base) % boundary);
        set_u64(b, 56, self.delay(&st));
        set_u64(b, 64, avail);
        set_u64(b, 72, st.avail_max.max(avail));
        st.avail_max = 0;
        set_timespec(b, 112, now);
    }

    fn sync_ptr(&self, arg: u64) -> Result<u64, isize> {
        const HWSYNC: u32 = 1;
        const APPL: u32 = 2;
        const AVAIL_MIN: u32 = 4;
        let mut b = [0u8; 136];
        user::get_bytes(arg, &mut b)?;
        let flags = get_u32(&b, 0);
        let mut st = self.st.lock();
        if flags & HWSYNC != 0 {
            self.hwsync(&mut st)?;
        }
        if flags & APPL == 0 {
            self.move_appl(&mut st, get_u64(&b, 72));
        }
        if flags & AVAIL_MIN == 0 {
            st.sw.avail_min = get_u64(&b, 80).max(1);
        }
        self.check_xrun(&mut st);
        let boundary = st.cfg.map_or(u64::MAX, |c| c.boundary);
        let hw = self.hw().saturating_sub(st.base);
        set_u32(&mut b, 8, st.state);
        set_u64(&mut b, 16, hw % boundary);
        set_timespec(&mut b, 24, time::now_ns());
        set_u32(&mut b, 40, 0);
        set_u64(&mut b, 72, (st.appl - st.base) % boundary);
        set_u64(&mut b, 80, st.sw.avail_min);
        drop(st);
        user::put_bytes(arg, &b).map(|_| 0)
    }

    fn hw_params(&self, arg: u64) -> Result<u64, isize> {
        let mut p = HwParams::read(arg)?;
        p.refine()?;
        let cfg = p.choose()?;
        let bytes = (cfg.buffer as usize * cfg.frame_bytes) as u64;
        let buf = ShmFile::new(bytes.next_multiple_of(PAGE_SIZE))?;
        {
            let mut st = self.st.lock();
            if !matches!(st.state, STATE_OPEN | STATE_SETUP | STATE_PREPARED) {
                return Err(EBADFD);
            }
            self.drop_queued(&mut st);
            st.base = st.appl;
            // Room for the whole buffer and a period more, as stereo samples.
            self.stream.set_capacity((cfg.buffer + cfg.period) as usize * 2);
            st.cfg = Some(cfg);
            st.buf = Some(buf);
            st.sw = Sw { avail_min: cfg.period, start_threshold: 1, stop_threshold: cfg.buffer };
            st.state = STATE_SETUP;
        }
        p.write(arg).map(|_| 0)
    }

    fn sw_params(&self, arg: u64) -> Result<u64, isize> {
        let mut b = [0u8; 136];
        user::get_bytes(arg, &mut b)?;
        let mut st = self.st.lock();
        let cfg = st.cfg.ok_or(EBADFD)?;
        // (Silence filling isn't needed: the mixer plays silence where a
        // stream has nothing. Timestamps are always CLOCK_MONOTONIC's.)
        let (tstamp_mode, silence_size) = (get_u32(&b, 0), get_u64(&b, 56));
        if tstamp_mode > 1 || silence_size > cfg.boundary {
            return Err(EINVAL);
        }
        let sw = Sw {
            avail_min: get_u64(&b, 16).max(1),
            start_threshold: get_u64(&b, 32),
            stop_threshold: get_u64(&b, 40),
        };
        st.sw = sw;
        set_u64(&mut b, 64, cfg.boundary);
        drop(st);
        user::put_bytes(arg, &b).map(|_| 0)
    }
}

impl CharDev for Pcm {
    fn read(&self, _out: &mut [u8], _nonblock: bool) -> Result<usize, isize> {
        Err(EINVAL)
    }

    fn write(&self, data: &[u8], nonblock: bool) -> Result<usize, isize> {
        // `write` takes whole frames, as WRITEI_FRAMES does.
        let frame = self.st.lock().cfg.ok_or(EBADFD)?.frame_bytes;
        let n = self.write_frames((data.len() / frame) as u64, nonblock, |at, len| Ok(data[at..at + len].to_vec()))?;
        Ok(n as usize * frame)
    }

    fn ioctl(&self, r: u32, arg: u64) -> Result<u64, isize> {
        self.ioctl_nb(r, arg, false)
    }

    fn ioctl_nb(&self, r: u32, arg: u64, nonblock: bool) -> Result<u64, isize> {
        if ioc_type(r) != b'A' as u32 {
            return Err(ENOTTY);
        }
        match ioc_nr(r) {
            // PVERSION
            0x00 => user::put(arg, &PCM_VERSION).map(|_| 0),
            // INFO
            0x01 => {
                let mut b = vec![0u8; 288];
                pcm_info(&mut b);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // TSTAMP, TTSTAMP, USER_PVERSION: timestamps are always
            // CLOCK_MONOTONIC's.
            0x02..=0x04 => Ok(0),
            // HW_REFINE
            0x10 => {
                if ioc_size(r) != HW_PARAMS_SIZE {
                    return Err(EINVAL);
                }
                let mut p = HwParams::read(arg)?;
                p.refine()?;
                p.write(arg).map(|_| 0)
            }
            // HW_PARAMS
            0x11 => {
                if ioc_size(r) != HW_PARAMS_SIZE {
                    return Err(EINVAL);
                }
                self.hw_params(arg)
            }
            // HW_FREE
            0x12 => {
                let mut st = self.st.lock();
                if matches!(st.state, STATE_RUNNING | STATE_DRAINING | STATE_PAUSED) {
                    return Err(EBADFD);
                }
                self.drop_queued(&mut st);
                st.cfg = None;
                st.buf = None;
                st.state = STATE_OPEN;
                Ok(0)
            }
            // SW_PARAMS
            0x13 => self.sw_params(arg),
            // STATUS, STATUS_EXT
            0x20 | 0x24 => {
                let mut b = [0u8; 152];
                self.status(&mut b);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // DELAY
            0x21 => {
                let mut st = self.st.lock();
                self.hwsync(&mut st)?;
                let d = self.delay(&st) as i64;
                drop(st);
                user::put(arg, &d).map(|_| 0)
            }
            // HWSYNC
            0x22 => self.hwsync(&mut self.st.lock()).map(|_| 0),
            // SYNC_PTR
            0x23 => self.sync_ptr(arg),
            // CHANNEL_INFO: where a channel's samples are in the mapped
            // buffer (interleaved).
            0x32 => {
                let mut b = [0u8; 24];
                user::get_bytes(arg, &mut b)?;
                let cfg = self.st.lock().cfg.ok_or(EBADFD)?;
                let ch = get_u32(&b, 0);
                if ch >= cfg.channels {
                    return Err(EINVAL);
                }
                let bits = width(cfg.format);
                set_u64(&mut b, 8, 0);
                set_u32(&mut b, 16, ch * bits);
                set_u32(&mut b, 20, bits * cfg.channels);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            // PREPARE
            0x40 => {
                let mut st = self.st.lock();
                if st.state == STATE_OPEN {
                    return Err(EBADFD);
                }
                self.drop_queued(&mut st);
                self.stream.set_paused(true);
                st.state = STATE_PREPARED;
                Ok(0)
            }
            // RESET
            0x41 => {
                let mut st = self.st.lock();
                self.hwsync(&mut st)?;
                self.drop_queued(&mut st);
                Ok(0)
            }
            // START
            0x42 => {
                let mut st = self.st.lock();
                if st.state != STATE_PREPARED {
                    return Err(EBADFD);
                }
                if st.appl == self.hw() && st.sw.stop_threshold < st.cfg.map_or(0, |c| c.boundary) {
                    return Err(EPIPE);
                }
                self.start(&mut st);
                Ok(0)
            }
            // DROP
            0x43 => {
                let mut st = self.st.lock();
                if st.state == STATE_OPEN {
                    return Err(EBADFD);
                }
                self.drop_queued(&mut st);
                self.stream.set_paused(true);
                st.state = STATE_SETUP;
                Ok(0)
            }
            // DRAIN
            0x44 => self.drain(),
            // PAUSE
            0x45 => {
                let on = arg != 0;
                let mut st = self.st.lock();
                match (st.state, on) {
                    (STATE_RUNNING, true) => {
                        st.state = STATE_PAUSED;
                        self.stream.set_paused(true);
                    }
                    (STATE_PAUSED, false) => {
                        st.state = STATE_RUNNING;
                        self.stream.set_paused(false);
                    }
                    _ => return Err(EBADFD),
                }
                st.trigger_ns = time::now_ns();
                Ok(0)
            }
            // REWIND: takes back frames not mixed yet.
            0x46 => {
                let want: u64 = user::get(arg)?;
                let mut st = self.st.lock();
                self.hwsync(&mut st)?;
                let n = self.stream.unqueue(want.min(u32::MAX as u64) as usize * 2) as u64 / 2;
                st.appl -= n;
                drop(st);
                user::put(arg, &n).map(|_| 0)
            }
            // RESUME (never suspended), XRUN.
            0x47 => Err(ENOSYS),
            0x48 => {
                let mut st = self.st.lock();
                if matches!(st.state, STATE_RUNNING | STATE_PREPARED | STATE_PAUSED) {
                    st.state = STATE_XRUN;
                    self.stream.set_paused(true);
                    Ok(0)
                } else {
                    Err(EBADFD)
                }
            }
            // FORWARD: commits frames of the mapped buffer.
            0x49 => {
                let want: u64 = user::get(arg)?;
                let mut st = self.st.lock();
                self.hwsync(&mut st)?;
                let (avail, _) = self.avail(&st);
                let n = want.min(avail);
                self.commit(&mut st, n);
                drop(st);
                user::put(arg, &n).map(|_| 0)
            }
            // WRITEI_FRAMES (struct snd_xferi: result, buf, frames).
            0x50 => {
                let [_, buf, frames]: [u64; 3] = user::get(arg)?;
                let n = self.write_frames(frames, nonblock, |at, len| user::slice(buf + at as u64, len as u64))?;
                user::put(arg, &n).map(|_| 0)
            }
            // LINK, UNLINK: no linked streams.
            0x60 | 0x61 => Err(EINVAL),
            _ => Err(ENOTTY),
        }
    }

    fn readiness(&self) -> (bool, bool, bool) {
        let mut st = self.st.lock();
        self.check_xrun(&mut st);
        let writable = match st.state {
            STATE_PREPARED | STATE_RUNNING | STATE_DRAINING => self.avail(&st).0 >= st.sw.avail_min.max(1),
            STATE_PAUSED => false,
            // Errors (XRUN) and stopped streams wake poll at once.
            _ => true,
        };
        (false, writable, false)
    }

    fn wait_key(&self) -> usize {
        readiness::key(self as *const Self)
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        (0o020666, super::fs::makedev(MAJOR, PCM_MINOR), None)
    }

    fn pcm(&self) -> Option<&Pcm> {
        Some(self)
    }
}

impl Drop for Pcm {
    fn drop(&mut self) {
        // Closing drops what wasn't played, as on Linux.
        self.stream.drop_queued();
        self.stream.close();
        let (frames, underruns) = self.stream.progress();
        let xruns = self.st.lock().xruns;
        if frames > 0 {
            let name = crate::proc::current().map(|p| p.name()).unwrap_or_default();
            log!(
                "linux",
                "{name}: ALSA playback closed: {} ms played, {underruns} underruns, {xruns} xruns",
                frames * 1000 / audio::RATE as u64
            );
        }
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    // The rules, as libasound's snd_pcm_set_params uses them: float
    // stereo at 48 kHz with 100 ms of latency.
    let mut p = HwParams { raw: vec![0u8; HW_PARAMS_SIZE], masks: [u32::MAX; 3], iv: [Iv::closed(0, u32::MAX, false); 12], changed: 0 };
    p.refine().unwrap();
    assert_eq!(p.masks[FORMAT], 1 << FORMAT_S16_LE | 1 << FORMAT_S32_LE | 1 << FORMAT_FLOAT_LE);
    assert_eq!(p.iv(RATE).value(), 48000);
    p.masks[FORMAT] = 1 << FORMAT_FLOAT_LE;
    p.set(CHANNELS, Iv::closed(2, 2, true)).unwrap();
    p.set(BUFFER_TIME, Iv::closed(100_000, 100_000, false)).unwrap();
    p.refine().unwrap();
    assert_eq!(p.iv(FRAME_BITS).value(), 64);
    assert!(p.iv(BUFFER_SIZE).single() && p.iv(BUFFER_SIZE).value() == 4800, "{:?}", p.iv(BUFFER_SIZE));
    p.set(PERIOD_TIME, Iv::closed(25_000, 25_000, false)).unwrap();
    p.refine().unwrap();
    let cfg = p.choose().unwrap();
    assert_eq!((cfg.period, cfg.buffer, cfg.frame_bytes, cfg.channels), (1200, 4800, 8, 2));
    assert_eq!(p.iv(PERIODS).value(), 4);
    assert!(cfg.boundary > 1 << 60 && cfg.boundary.is_multiple_of(cfg.buffer));
    // Something the card can't do is refused.
    let mut q = HwParams { raw: vec![0u8; HW_PARAMS_SIZE], masks: [u32::MAX; 3], iv: [Iv::closed(0, u32::MAX, false); 12], changed: 0 };
    q.set(RATE, Iv::closed(44100, 44100, true)).unwrap();
    assert!(q.refine().is_err());
    log!("ktest", "alsa: hw_params rules ok");
}
