//! Vblank counting and DRM events.
//!
//! Devices with vblank interrupts count frames themselves
//! (`KmsDevice::vblank`). The others (virtio-gpu, the VGA-style ones) have
//! no vertical blank a program could see, so each CRTC gets a steady clock
//! at its mode's refresh rate: frame `n` begins at `n` periods after boot.
//! Page flips complete, and their events arrive, at the next frame, so a
//! program that flips on each event runs at the refresh rate, as it would
//! on Linux.
//!
//! Events wait in the file until due (a timer wakes `poll`, `epoll` and
//! blocking reads then) and are read as whole `struct drm_event`s.

use super::{Args, File};
use crate::drivers::gpu::{KmsDevice, ObjId};
use crate::proc::readiness;
use crate::time;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub const EVENT_VBLANK: u32 = 0x01;
pub const EVENT_FLIP_COMPLETE: u32 = 0x02;
pub const EVENT_CRTC_SEQUENCE: u32 = 0x03;

/// Linux allows a file this much unread event data.
const EVENT_SPACE: usize = 4096;

#[derive(Default)]
pub struct Events {
    /// Due events, oldest first.
    ready: VecDeque<Vec<u8>>,
    /// Events for later: (due, bytes), in due order.
    pending: Vec<(u64, Vec<u8>)>,
}

impl Events {
    /// Moves what is due by `now` to the ready queue; true if any is ready.
    pub fn promote(&mut self, now: u64) -> bool {
        while self.pending.first().is_some_and(|(due, _)| *due <= now) {
            let (_, e) = self.pending.remove(0);
            self.ready.push_back(e);
        }
        !self.ready.is_empty()
    }

    fn space(&self) -> usize {
        self.ready.iter().map(Vec::len).chain(self.pending.iter().map(|(_, e)| e.len())).sum()
    }

    /// The soonest pending event's due time.
    fn next_due(&self) -> Option<u64> {
        self.pending.first().map(|(d, _)| *d)
    }
}

/// Wakes whoever waits on the file with key `key` (from a timer).
fn wake(key: u64) {
    readiness::notify(key as usize);
}

/// Queues event `bytes` for `due` (monotonic ns).
fn queue(f: &File, due: u64, bytes: Vec<u8>) -> Result<(), isize> {
    let now = time::now_ns();
    {
        let mut ev = f.events.lock();
        if ev.space() + bytes.len() > EVENT_SPACE {
            return Err(ENOMEM);
        }
        let at = ev.pending.iter().position(|(d, _)| *d > due).unwrap_or(ev.pending.len());
        ev.pending.insert(at, (due, bytes));
    }
    let key = readiness::key(f as *const File) as u64;
    if due <= now {
        wake(key);
    } else {
        // At the frame's start to the nanosecond, not the millisecond after.
        crate::timer::add(due, crate::timer::Action::Call { f: wake, arg: key, name: "drm-event" });
    }
    Ok(())
}

/// Reads whole events into `out` (0 bytes if the first does not fit);
/// waits for one unless `nonblock`.
pub fn read(f: &File, out: &mut [u8], nonblock: bool) -> Result<usize, isize> {
    loop {
        let watch = (!nonblock).then(|| readiness::Watch::new(&[readiness::key(f as *const File)]));
        let now = time::now_ns();
        let next = {
            let mut ev = f.events.lock();
            if ev.promote(now) {
                let mut n = 0;
                while let Some(e) = ev.ready.front() {
                    if n + e.len() > out.len() {
                        break;
                    }
                    out[n..n + e.len()].copy_from_slice(e);
                    n += e.len();
                    ev.ready.pop_front();
                }
                return Ok(n);
            }
            ev.next_due()
        };
        if nonblock {
            return Err(EAGAIN);
        }
        if crate::proc::interrupted() {
            return Err(EINTR);
        }
        let ms = next.map_or(1000, |d| d.saturating_sub(now).div_ceil(1_000_000)).clamp(1, 1000);
        watch.unwrap().wait(ms);
    }
}

// ------------------------------------------------------------ counting

/// A CRTC's index and its frame period (ns); `None` if it does not exist.
fn crtc_info(dev: &dyn KmsDevice, crtc: ObjId) -> Option<(u32, u64, bool)> {
    let index = dev.crtcs().iter().find(|c| c.id == crtc)?.index;
    let (cs, _) = dev.state();
    let s = cs.iter().find(|s| s.crtc == crtc);
    let hz = s.and_then(|s| s.mode).map_or(60, |m| m.vrefresh).clamp(10, 1000) as u64;
    let active = s.is_some_and(|s| s.active);
    Some((index, 1_000_000_000 / hz, active))
}

/// The frame a CRTC is in now: (count, when it began).
pub fn now(dev: &dyn KmsDevice, crtc: ObjId) -> (u64, u64) {
    if dev.caps().vblank_irq {
        let v = dev.vblank(crtc);
        return (v.seq, v.time_ns);
    }
    let period = crtc_info(dev, crtc).map_or(16_666_667, |i| i.1);
    let t = time::now_ns();
    (t / period, t / period * period)
}

/// When frame `seq` of `crtc` begins (now, for devices that count frames).
fn frame_time(dev: &dyn KmsDevice, crtc: ObjId, seq: u64) -> u64 {
    if dev.caps().vblank_irq {
        // Not knowable ahead: poll for it (see `wait_frame`).
        return time::now_ns();
    }
    let period = crtc_info(dev, crtc).map_or(16_666_667, |i| i.1);
    seq * period
}

/// Waits until `crtc` reaches frame `seq`.
fn wait_frame(dev: &dyn KmsDevice, crtc: ObjId, seq: u64) -> Result<(), isize> {
    loop {
        let (cur, _) = now(dev, crtc);
        if cur >= seq {
            return Ok(());
        }
        if crate::proc::interrupted() {
            return Err(EINTR);
        }
        let wait = if dev.caps().vblank_irq {
            1
        } else {
            frame_time(dev, crtc, seq).saturating_sub(time::now_ns()).div_ceil(1_000_000).max(1)
        };
        crate::sched::wait_until(wait, crate::proc::interrupted);
    }
}

/// Busy CRTCs: a flip whose event is still to come (Linux's EBUSY).
static FLIPS: crate::sync::IrqMutex<Vec<(ObjId, u64)>> = crate::sync::IrqMutex::new(Vec::new());

/// True if a flip on `crtc` has not completed yet.
pub fn flip_pending(crtc: ObjId) -> bool {
    let now = time::now_ns();
    let mut f = FLIPS.lock();
    f.retain(|&(_, due)| due > now);
    f.iter().any(|&(c, _)| c == crtc)
}

/// Waits for a pending flip on `crtc` to complete (blocking commits).
pub fn wait_flip(crtc: ObjId) -> Result<(), isize> {
    loop {
        let due = FLIPS.lock().iter().filter(|&&(c, _)| c == crtc).map(|&(_, d)| d).max();
        let Some(due) = due.filter(|&d| d > time::now_ns()) else { return Ok(()) };
        if crate::proc::interrupted() {
            return Err(EINTR);
        }
        let ms = (due - time::now_ns()).div_ceil(1_000_000).max(1);
        crate::sched::wait_until(ms, crate::proc::interrupted);
    }
}

/// A commit changed what `crtc` shows: the flip completes at the next
/// frame (returned), with a `DRM_EVENT_FLIP_COMPLETE` for `user_data` if
/// `event`.
pub fn flipped(f: &File, crtc: ObjId, event: Option<u64>) -> Result<u64, isize> {
    let dev = &*f.dev;
    let (seq, _) = now(dev, crtc);
    let due = if dev.caps().vblank_irq { time::now_ns() } else { frame_time(dev, crtc, seq + 1) };
    FLIPS.lock().push((crtc, due));
    if let Some(user_data) = event {
        queue(f, due, vblank_event(EVENT_FLIP_COMPLETE, user_data, due, seq + 1, crtc))?;
    }
    Ok(due)
}

/// `struct drm_event_vblank` (32 bytes).
fn vblank_event(kind: u32, user_data: u64, ns: u64, seq: u64, crtc: ObjId) -> Vec<u8> {
    let mut b = alloc::vec![0u8; 32];
    b[0..4].copy_from_slice(&kind.to_le_bytes());
    b[4..8].copy_from_slice(&32u32.to_le_bytes());
    b[8..16].copy_from_slice(&user_data.to_le_bytes());
    b[16..20].copy_from_slice(&((ns / 1_000_000_000) as u32).to_le_bytes());
    b[20..24].copy_from_slice(&((ns / 1000 % 1_000_000) as u32).to_le_bytes());
    b[24..28].copy_from_slice(&(seq as u32).to_le_bytes());
    b[28..32].copy_from_slice(&crtc.to_le_bytes());
    b
}

// ------------------------------------------------------------ ioctls

/// The CRTC a `WAIT_VBLANK` request names: by index (`SECONDARY`, or the
/// "high CRTC" bits).
fn crtc_by_index(dev: &dyn KmsDevice, index: u32) -> Option<ObjId> {
    dev.crtcs().iter().find(|c| c.index == index).map(|c| c.id)
}

/// `union drm_wait_vblank`: request { type, sequence, signal }, reply
/// { type, sequence, tval_sec, tval_usec }.
pub fn wait_vblank(f: &File, a: &mut Args) -> Result<(), isize> {
    const RELATIVE: u32 = 0x1;
    const HIGH_CRTC_MASK: u32 = 0x3E;
    const EVENT: u32 = 0x0400_0000;
    const FLIP: u32 = 0x0800_0000;
    const NEXTONMISS: u32 = 0x1000_0000;
    const SECONDARY: u32 = 0x2000_0000;
    const SIGNAL: u32 = 0x4000_0000;
    let (kind, want, signal) = (a.u32(0), a.u32(4), a.u64(8));
    let flags = EVENT | FLIP | NEXTONMISS | SECONDARY | SIGNAL;
    if kind & !(RELATIVE | HIGH_CRTC_MASK | flags) != 0 || kind & SIGNAL != 0 {
        return Err(EINVAL);
    }
    let index = if kind & SECONDARY != 0 { 1 } else { (kind & HIGH_CRTC_MASK) >> 1 };
    let dev = &*f.dev;
    let crtc = crtc_by_index(dev, index).ok_or(EINVAL)?;
    if !crtc_info(dev, crtc).is_some_and(|i| i.2) {
        return Err(EINVAL);
    }
    let (cur, cur_ns) = now(dev, crtc);
    // Sequence numbers are 32 bits wide here; extend them near the counter.
    let mut target = if kind & RELATIVE != 0 { cur + want as u64 } else { widen(cur, want) };
    if kind & NEXTONMISS != 0 && target <= cur {
        target = cur + 1;
    }
    let reply = |a: &mut Args, seq: u64, ns: u64| {
        a.set32(4, seq as u32);
        a.set64(8, ns / 1_000_000_000);
        a.set64(16, ns / 1000 % 1_000_000);
    };
    if kind & EVENT != 0 {
        let due = if target <= cur { time::now_ns() } else { frame_time(dev, crtc, target) };
        queue(f, due, vblank_event(EVENT_VBLANK, signal, due.max(cur_ns), target.max(cur), crtc))?;
        reply(a, target, cur_ns);
        return Ok(());
    }
    if target > cur {
        wait_frame(dev, crtc, target)?;
    }
    let (seq, ns) = now(dev, crtc);
    reply(a, seq, ns);
    Ok(())
}

/// The 64-bit frame number nearest `cur` whose low 32 bits are `low`.
fn widen(cur: u64, low: u32) -> u64 {
    let base = cur & !0xFFFF_FFFF;
    let c = base | low as u64;
    if c + (1 << 31) < cur {
        c + (1 << 32)
    } else if c > cur + (1 << 31) && c >= 1 << 32 {
        c - (1 << 32)
    } else {
        c
    }
}

/// `struct drm_crtc_get_sequence`: crtc_id, active, sequence, sequence_ns.
pub fn get_sequence(f: &File, a: &mut Args) -> Result<(), isize> {
    let crtc = a.u32(0);
    let (_, _, active) = crtc_info(&*f.dev, crtc).ok_or(ENOENT)?;
    let (seq, ns) = now(&*f.dev, crtc);
    a.set32(4, active as u32);
    a.set64(8, seq);
    a.set64(16, ns);
    Ok(())
}

/// `struct drm_crtc_queue_sequence`: crtc_id, flags, sequence, user_data.
/// An event of type `DRM_EVENT_CRTC_SEQUENCE` arrives at that frame.
pub fn queue_sequence(f: &File, a: &mut Args) -> Result<(), isize> {
    const RELATIVE: u32 = 1;
    const NEXT_ON_MISS: u32 = 2;
    let (crtc, flags, want, user_data) = (a.u32(0), a.u32(4), a.u64(8), a.u64(16));
    if flags & !(RELATIVE | NEXT_ON_MISS) != 0 {
        return Err(EINVAL);
    }
    let dev = &*f.dev;
    let (_, _, active) = crtc_info(dev, crtc).ok_or(ENOENT)?;
    if !active {
        return Err(EINVAL);
    }
    let (cur, _) = now(dev, crtc);
    let mut target = if flags & RELATIVE != 0 { cur + want } else { want };
    if flags & NEXT_ON_MISS != 0 && target <= cur {
        target = cur + 1;
    }
    let due = if target <= cur { time::now_ns() } else { frame_time(dev, crtc, target) };
    // struct drm_event_crtc_sequence: base, user_data, time_ns, sequence.
    let mut b = alloc::vec![0u8; 32];
    b[0..4].copy_from_slice(&EVENT_CRTC_SEQUENCE.to_le_bytes());
    b[4..8].copy_from_slice(&32u32.to_le_bytes());
    b[8..16].copy_from_slice(&user_data.to_le_bytes());
    b[16..24].copy_from_slice(&due.to_le_bytes());
    b[24..32].copy_from_slice(&target.max(cur).to_le_bytes());
    queue(f, due, b)?;
    a.set64(8, target);
    Ok(())
}
