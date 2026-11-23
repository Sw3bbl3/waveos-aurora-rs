//! The virtio-gpu command layer: the control and cursor queues, request
//! slots, completion waits, fences and resource ids.
//!
//! Nothing here knows about displays: a 3D context implementation
//! (virgl/Venus `SUBMIT_3D`, blob resources) sends its commands through
//! the same [`Gpu::submit`]/[`Gpu::wait`] calls with a context id in the
//! header, takes ids from [`Gpu::alloc_resource_id`], and waits on fences
//! with [`Gpu::wait_fence`].
//!
//! Each control command occupies one slot of a DMA pool: the request at
//! the start, the device's response after it. A request may carry one
//! extra read-only buffer (a backing's page list), owned by the slot until
//! the command completes. Completions are reaped by whoever waits (the
//! MSI-X handler only wakes the waiters), so no lock is ever held across
//! a sleep.

use super::proto::{self, Cmd, Header, HEADER_LEN};
use crate::drivers::block::{wait_for, Dma};
use crate::drivers::gpu::{bo::Unbind, Error, Result};
use crate::drivers::pci;
use crate::drivers::virtio::{Transport, Virtqueue, F_VERSION_1};
use crate::sync::{IrqMutex, WaitQueue};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const SLOTS: usize = 32;
const SLOT_SIZE: usize = 2048;
/// Requests start a slot; responses start here.
const RESP_OFF: usize = 512;
const QUEUE_SIZE: u16 = 128;
const CURSOR_SLOTS: usize = 16;
const CURSOR_CMD: usize = 56;
/// How long a command may take before the device is considered stuck.
const TIMEOUT_MS: u64 = 3000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Slot {
    Free,
    /// In flight; `detached` slots are freed by the reaper (nobody waits).
    Busy {
        detached: bool,
    },
    /// Completed: bytes the device wrote.
    Done(u32),
}

struct Ring {
    q: Virtqueue,
    pool: Dma,
    slots: [Slot; SLOTS],
    /// Slot of each in-flight chain, by head descriptor.
    by_head: Vec<Option<u8>>,
    /// Extra buffers, kept until their command completes.
    extra: Vec<Option<Dma>>,
    /// Bumped whenever the device is reset: tickets from before are void.
    epoch: u32,
}

struct CursorRing {
    q: Virtqueue,
    pool: Dma,
    free: Vec<u8>,
    by_head: Vec<Option<u8>>,
}

/// A submitted command.
#[derive(Clone, Copy, Debug)]
pub struct Ticket {
    slot: u8,
    epoch: u32,
}

pub struct Gpu {
    t: Transport,
    pub features: u64,
    ctrl: IrqMutex<Ring>,
    cursor: IrqMutex<CursorRing>,
    /// Woken by the interrupt: command waiters.
    wake: WaitQueue,
    /// Woken by the interrupt when the device has an event to read.
    pub events: WaitQueue,
    pub event_pending: AtomicBool,
    has_irq: AtomicBool,
    next_fence: AtomicU64,
    done_fence: AtomicU64,
    ids: IrqMutex<(u32, Vec<u32>)>,
    pub num_scanouts: u32,
    pub num_capsets: u32,
}

fn interrupt(arg: usize) {
    let gpu = unsafe { &*(arg as *const Gpu) };
    gpu.wake.wake_all();
    // Configuration changed (a host window resized)? A register read, no lock.
    if gpu.read_events() != 0 {
        gpu.event_pending.store(true, Ordering::Release);
        gpu.events.wake_all();
    }
}

impl Gpu {
    /// Sets up the device: features, queues, interrupts.
    pub fn new(dev: &pci::Device) -> Option<Arc<Gpu>> {
        dev.enable();
        let t = Transport::probe(dev)?;
        let features = t.negotiate(F_VERSION_1 | proto::F_EDID, F_VERSION_1)?;
        let qsize = |i| t.queue_max(i).min(QUEUE_SIZE);
        let (cs, ks) = (qsize(0), qsize(1));
        if cs < 8 || ks < 2 {
            return None;
        }
        let ctrl = Ring {
            q: Virtqueue::new(cs)?,
            pool: Dma::new(SLOTS * SLOT_SIZE)?,
            slots: [Slot::Free; SLOTS],
            by_head: vec![None; cs as usize],
            extra: (0..SLOTS).map(|_| None).collect(),
            epoch: 0,
        };
        let cursor = CursorRing {
            q: Virtqueue::new(ks)?,
            pool: Dma::new(CURSOR_SLOTS * 64)?,
            free: (0..CURSOR_SLOTS as u8).rev().collect(),
            by_head: vec![None; ks as usize],
        };
        let config32 = |o: u64| unsafe { ((t.device + o) as *const u32).read_volatile() };
        let gpu = Arc::new(Gpu {
            t,
            features,
            ctrl: IrqMutex::new(ctrl),
            cursor: IrqMutex::new(cursor),
            wake: WaitQueue::new(),
            events: WaitQueue::new(),
            event_pending: AtomicBool::new(false),
            has_irq: AtomicBool::new(false),
            next_fence: AtomicU64::new(1),
            done_fence: AtomicU64::new(0),
            ids: IrqMutex::new((1, Vec::new())),
            num_scanouts: config32(proto::CFG_NUM_SCANOUTS).clamp(1, proto::MAX_SCANOUTS as u32),
            num_capsets: config32(proto::CFG_NUM_CAPSETS),
        });
        // The interrupt handler keeps a reference for good.
        let arg = Arc::into_raw(gpu.clone()) as usize;
        let msix = dev.enable_msi("virtio-gpu", interrupt, arg).is_some();
        let irq = gpu.start_queues(msix);
        gpu.has_irq.store(irq, Ordering::Release);
        Some(gpu)
    }

    pub fn has_irq(&self) -> bool {
        self.has_irq.load(Ordering::Acquire)
    }

    /// Installs both queues and starts the device. Returns whether
    /// completions interrupt (else waits poll).
    fn start_queues(&self, msix: bool) -> bool {
        let vector = msix.then_some(0);
        let a = self.t.attach_queue(0, &mut self.ctrl.lock().q, vector);
        let b = self.t.attach_queue(1, &mut self.cursor.lock().q, vector);
        if msix {
            self.t.config_interrupt(0);
        } else {
            self.t.no_config_interrupt();
        }
        self.t.driver_ok();
        a && b
    }

    /// After sleep the device was reset: negotiate again and start with
    /// empty queues. Commands in flight are lost (their waiters fail).
    pub fn reinit(&self) -> bool {
        if self.t.negotiate(self.features | F_VERSION_1, F_VERSION_1).is_none() {
            return false;
        }
        {
            let mut r = self.ctrl.lock();
            r.q.reset();
            r.slots = [Slot::Free; SLOTS];
            r.by_head.iter_mut().for_each(|h| *h = None);
            r.extra.iter_mut().for_each(|e| *e = None);
            r.epoch = r.epoch.wrapping_add(1);
        }
        {
            let mut c = self.cursor.lock();
            c.q.reset();
            c.free = (0..CURSOR_SLOTS as u8).rev().collect();
            c.by_head.iter_mut().for_each(|h| *h = None);
        }
        self.start_queues(self.has_irq());
        self.wake.wake_all();
        true
    }

    pub fn config32(&self, offset: u64) -> u32 {
        unsafe { ((self.t.device + offset) as *const u32).read_volatile() }
    }

    /// Pending device events (`proto::EVENT_DISPLAY`).
    pub fn read_events(&self) -> u32 {
        self.config32(proto::CFG_EVENTS_READ)
    }

    pub fn clear_events(&self, events: u32) {
        unsafe { ((self.t.device + proto::CFG_EVENTS_CLEAR) as *mut u32).write_volatile(events) };
    }

    // ------------------------------------------------------- submission

    /// Queues `cmd` (and `extra`, read after it) with room for a
    /// `resp_len`-byte response. With `notify` false the device isn't told
    /// yet: batch several commands, then [`Gpu::notify`]. A `detached`
    /// command is never waited for.
    pub fn submit(
        &self,
        cmd: &Cmd,
        extra: Option<Dma>,
        resp_len: usize,
        notify: bool,
        detached: bool,
    ) -> Result<Ticket> {
        assert!(cmd.len <= RESP_OFF && resp_len <= SLOT_SIZE - RESP_OFF);
        let deadline = crate::time::uptime_ms() + TIMEOUT_MS;
        let mut extra = extra;
        loop {
            {
                let mut r = self.ctrl.lock();
                self.reap_locked(&mut r);
                let need = 2 + extra.is_some() as usize;
                if let (Some(slot), true) = (r.slots.iter().position(|s| *s == Slot::Free), r.q.num_free() >= need) {
                    let base = slot * SLOT_SIZE;
                    r.pool.bytes_mut()[base..base + cmd.len].copy_from_slice(cmd.bytes());
                    r.pool.bytes_mut()[base + RESP_OFF..base + RESP_OFF + HEADER_LEN].fill(0);
                    let phys = r.pool.phys + base as u64;
                    let mut bufs = vec![(phys, cmd.len as u32, false)];
                    if let Some(e) = &extra {
                        bufs.push((e.phys, e.len() as u32, false));
                    }
                    bufs.push((phys + RESP_OFF as u64, resp_len.max(HEADER_LEN) as u32, true));
                    let head = r.q.add(&bufs).ok_or(Error::Io)?;
                    r.by_head[head as usize] = Some(slot as u8);
                    r.slots[slot] = Slot::Busy { detached };
                    r.extra[slot] = extra.take();
                    if notify {
                        r.q.notify(0);
                    }
                    return Ok(Ticket { slot: slot as u8, epoch: r.epoch });
                }
                // Full: make sure the device knows about what is queued.
                r.q.notify(0);
            }
            if crate::time::uptime_ms() > deadline {
                return Err(Error::Io);
            }
            self.wait_progress(10);
        }
    }

    /// Tells the device about queued commands.
    pub fn notify(&self) {
        self.ctrl.lock().q.notify(0);
    }

    /// Waits for `t`; returns the response header and copies the response
    /// (after the header) into `out`.
    pub fn wait(&self, t: Ticket, out: &mut [u8]) -> Result<Header> {
        let done = || {
            let mut r = self.ctrl.lock();
            self.reap_locked(&mut r);
            r.epoch != t.epoch || matches!(r.slots[t.slot as usize], Slot::Done(_))
        };
        let finished =
            if self.has_irq() { self.wake.wait(TIMEOUT_MS, done) } else { wait_for(TIMEOUT_MS, done).is_ok() };
        let mut r = self.ctrl.lock();
        if r.epoch != t.epoch {
            return Err(Error::Io);
        }
        let slot = t.slot as usize;
        let Slot::Done(len) = r.slots[slot] else {
            if finished {
                return Err(Error::Io);
            }
            // Stuck: let the reaper free the slot if it ever completes.
            r.slots[slot] = Slot::Busy { detached: true };
            log!("virtio-gpu", "command timed out");
            return Err(Error::Io);
        };
        let base = slot * SLOT_SIZE + RESP_OFF;
        let resp = &r.pool.bytes()[base..base + (len as usize).max(HEADER_LEN)];
        let h = Header::decode(resp);
        let n = out.len().min(resp.len().saturating_sub(HEADER_LEN));
        out[..n].copy_from_slice(&resp[HEADER_LEN..HEADER_LEN + n]);
        r.slots[slot] = Slot::Free;
        r.extra[slot] = None;
        Ok(h)
    }

    /// Sends a command and waits for an `OK_NODATA`.
    pub fn call(&self, cmd: &Cmd) -> Result<()> {
        let t = self.submit(cmd, None, HEADER_LEN, true, false)?;
        Self::ok(self.wait(t, &mut [])?)
    }

    pub fn ok(h: Header) -> Result<()> {
        match h.kind {
            proto::OK_NODATA => Ok(()),
            proto::ERR_OUT_OF_MEMORY => Err(Error::NoMemory),
            proto::ERR_INVALID_SCANOUT_ID | proto::ERR_INVALID_RESOURCE_ID | proto::ERR_INVALID_PARAMETER => {
                Err(Error::Invalid)
            }
            _ => Err(Error::Io),
        }
    }

    /// Takes finished chains off the used ring (lock held).
    fn reap_locked(&self, r: &mut Ring) {
        while let Some((head, len)) = r.q.pop_used() {
            let Some(slot) = r.by_head.get_mut(head as usize).and_then(|s| s.take()) else { continue };
            let slot = slot as usize;
            let resp = slot * SLOT_SIZE + RESP_OFF;
            let h = Header::decode(&r.pool.bytes()[resp..resp + HEADER_LEN]);
            if h.flags & proto::FLAG_FENCE != 0 {
                self.done_fence.fetch_max(h.fence, Ordering::AcqRel);
            }
            match r.slots[slot] {
                Slot::Busy { detached: true } => {
                    r.slots[slot] = Slot::Free;
                    // Dropping a DMA buffer only frees frames (IRQ-safe lock).
                    r.extra[slot] = None;
                }
                _ => r.slots[slot] = Slot::Done(len),
            }
        }
    }

    /// Waits up to `ms` for any completion.
    fn wait_progress(&self, ms: u64) {
        let has_used = || self.ctrl.lock().q.has_used();
        if self.has_irq() {
            self.wake.wait(ms, has_used);
        } else {
            let _ = wait_for(ms, has_used);
        }
    }

    // ----------------------------------------------------------- fences

    /// A new fence id for a command (see [`Cmd::set_fence`]).
    pub fn next_fence(&self) -> u64 {
        self.next_fence.fetch_add(1, Ordering::Relaxed)
    }

    pub fn fence_done(&self, fence: u64) -> bool {
        self.done_fence.load(Ordering::Acquire) >= fence
    }

    /// Waits until the host has signalled `fence`.
    pub fn wait_fence(&self, fence: u64) -> Result<()> {
        let done = || {
            self.reap_locked(&mut self.ctrl.lock());
            self.fence_done(fence)
        };
        let ok = if self.has_irq() { self.wake.wait(TIMEOUT_MS, done) } else { wait_for(TIMEOUT_MS, done).is_ok() };
        if ok {
            Ok(())
        } else {
            Err(Error::Io)
        }
    }

    // ------------------------------------------------------ resource ids

    pub fn alloc_resource_id(&self) -> u32 {
        let mut ids = self.ids.lock();
        if let Some(id) = ids.1.pop() {
            return id;
        }
        ids.0 += 1;
        ids.0 - 1
    }

    pub fn free_resource_id(&self, id: u32) {
        self.ids.lock().1.push(id);
    }

    // ------------------------------------------------------ cursor queue

    /// Sends a cursor command (no response; the device just takes it).
    pub fn cursor_cmd(&self, cmd: &Cmd) -> Result<()> {
        let deadline = crate::time::uptime_ms() + 200;
        loop {
            {
                let mut c = self.cursor.lock();
                while let Some((head, _)) = c.q.pop_used() {
                    if let Some(slot) = c.by_head.get_mut(head as usize).and_then(|s| s.take()) {
                        c.free.push(slot);
                    }
                }
                if let Some(slot) = c.free.pop() {
                    let base = slot as usize * 64;
                    let n = cmd.len.min(CURSOR_CMD);
                    c.pool.bytes_mut()[base..base + n].copy_from_slice(&cmd.bytes()[..n]);
                    let phys = c.pool.phys + base as u64;
                    match c.q.add(&[(phys, n as u32, false)]) {
                        Some(head) => {
                            c.by_head[head as usize] = Some(slot);
                            c.q.notify(1);
                            return Ok(());
                        }
                        None => c.free.push(slot),
                    }
                }
                c.q.notify(1);
            }
            if crate::time::uptime_ms() > deadline {
                return Err(Error::Io);
            }
            crate::sched::yield_now();
        }
    }

    // -------------------------------------------------- 2D conveniences

    pub fn get_display_info(&self) -> Result<[proto::DisplayOne; proto::MAX_SCANOUTS]> {
        let cmd = Cmd::new(Header::new(proto::GET_DISPLAY_INFO));
        let t = self.submit(&cmd, None, proto::DISPLAY_INFO_LEN, true, false)?;
        let mut out = [0u8; proto::DISPLAY_INFO_LEN - HEADER_LEN];
        let h = self.wait(t, &mut out)?;
        if h.kind != proto::OK_DISPLAY_INFO {
            return Err(Error::Io);
        }
        // `display_info` expects the header in front.
        let mut full = [0u8; proto::DISPLAY_INFO_LEN];
        full[HEADER_LEN..].copy_from_slice(&out);
        Ok(proto::display_info(&full))
    }

    /// The EDID of `scanout` (needs `F_EDID`).
    pub fn get_edid(&self, scanout: u32) -> Result<Vec<u8>> {
        if self.features & proto::F_EDID == 0 {
            return Err(Error::Unsupported);
        }
        let cmd = Cmd::new(Header::new(proto::GET_EDID)).u32(scanout).u32(0);
        let t = self.submit(&cmd, None, proto::EDID_LEN, true, false)?;
        let mut out = vec![0u8; proto::EDID_LEN - HEADER_LEN];
        let h = self.wait(t, &mut out)?;
        if h.kind != proto::OK_EDID {
            return Err(Error::Io);
        }
        let size = (proto::le32(&out, 0) as usize).min(1024);
        Ok(out[8..8 + size].to_vec())
    }

    pub fn resource_create_2d(&self, id: u32, format: u32, w: u32, h: u32) -> Result<()> {
        self.call(&Cmd::new(Header::new(proto::RESOURCE_CREATE_2D)).u32(id).u32(format).u32(w).u32(h))
    }

    /// Gives resource `id` its guest memory: `(physical address, bytes)` runs.
    pub fn attach_backing(&self, id: u32, chunks: &[(u64, usize)]) -> Result<()> {
        let mut list = Dma::new(chunks.len() * 16).ok_or(Error::NoMemory)?;
        for (i, &(addr, len)) in chunks.iter().enumerate() {
            let e = &mut list.bytes_mut()[i * 16..i * 16 + 16];
            e[..8].copy_from_slice(&addr.to_le_bytes());
            e[8..12].copy_from_slice(&(len as u32).to_le_bytes());
            e[12..].fill(0);
        }
        let cmd = Cmd::new(Header::new(proto::RESOURCE_ATTACH_BACKING)).u32(id).u32(chunks.len() as u32);
        // The list buffer is a whole number of pages; the device reads only `nr_entries`.
        let t = self.submit(&cmd, Some(list), HEADER_LEN, true, false)?;
        Self::ok(self.wait(t, &mut [])?)
    }

    pub fn detach_backing(&self, id: u32) -> Result<()> {
        self.call(&Cmd::new(Header::new(proto::RESOURCE_DETACH_BACKING)).u32(id).u32(0))
    }

    pub fn resource_unref(&self, id: u32) -> Result<()> {
        self.call(&Cmd::new(Header::new(proto::RESOURCE_UNREF)).u32(id).u32(0))
    }

    /// Shows `rect` of resource `id` on `scanout` (`id` 0 turns it off).
    pub fn set_scanout(&self, scanout: u32, id: u32, rect: proto::Rect) -> Result<()> {
        self.call(&Cmd::new(Header::new(proto::SET_SCANOUT)).rect(rect).u32(scanout).u32(id))
    }

    /// A transfer of `rect` from guest memory (rows `stride` bytes apart).
    pub fn transfer_cmd(id: u32, rect: proto::Rect, stride: u32) -> Cmd {
        let offset = rect.y as u64 * stride as u64 + rect.x as u64 * 4;
        Cmd::new(Header::new(proto::TRANSFER_TO_HOST_2D)).rect(rect).u64(offset).u32(id).u32(0)
    }

    pub fn flush_cmd(id: u32, rect: proto::Rect) -> Cmd {
        Cmd::new(Header::new(proto::RESOURCE_FLUSH)).rect(rect).u32(id).u32(0)
    }

    /// Copies `rects` of resource `id` to the host and shows them: all
    /// transfers, then all flushes (the last fenced), one notification,
    /// one wait.
    pub fn present(&self, id: u32, stride: u32, rects: &[proto::Rect]) -> Result<()> {
        if rects.is_empty() {
            return Ok(());
        }
        let mut tickets: Vec<Ticket> = Vec::with_capacity(rects.len() * 2);
        for r in rects {
            tickets.push(self.submit(&Self::transfer_cmd(id, *r, stride), None, HEADER_LEN, false, false)?);
        }
        for (i, r) in rects.iter().enumerate() {
            let mut cmd = Self::flush_cmd(id, *r);
            if i + 1 == rects.len() {
                cmd.set_fence(self.next_fence());
            }
            tickets.push(self.submit(&cmd, None, HEADER_LEN, false, false)?);
        }
        self.notify();
        let mut result = Ok(());
        for t in tickets {
            if let Err(e) = self.wait(t, &mut []).and_then(Self::ok) {
                result = Err(e);
            }
        }
        result
    }

    /// Copies `rect` of resource `id` to the host, without showing it.
    pub fn present_no_flush(&self, id: u32, stride: u32, rect: proto::Rect) -> Result<()> {
        let t = self.submit(&Self::transfer_cmd(id, rect, stride), None, HEADER_LEN, true, false)?;
        Self::ok(self.wait(t, &mut [])?)
    }

    /// Shows `rect` of resource `id` wherever it is scanned out.
    pub fn flush(&self, id: u32, rect: proto::Rect) -> Result<()> {
        self.call(&Self::flush_cmd(id, rect))
    }

    pub fn cursor_update(&self, scanout: u32, x: i32, y: i32, id: u32, hot: (u32, u32)) -> Result<()> {
        self.cursor_cmd(
            &Cmd::new(Header::new(proto::UPDATE_CURSOR))
                .u32(scanout)
                .u32(x.max(0) as u32)
                .u32(y.max(0) as u32)
                .u32(0)
                .u32(id)
                .u32(hot.0)
                .u32(hot.1)
                .u32(0),
        )
    }

    pub fn cursor_move(&self, scanout: u32, x: i32, y: i32) -> Result<()> {
        self.cursor_cmd(
            &Cmd::new(Header::new(proto::MOVE_CURSOR))
                .u32(scanout)
                .u32(x.max(0) as u32)
                .u32(y.max(0) as u32)
                .u32(0)
                .u32(0)
                .u32(0)
                .u32(0)
                .u32(0),
        )
    }
}

/// A buffer object bound to a host resource is going away: the host lets
/// go of its memory first.
impl Unbind for Gpu {
    fn unbind(&self, id: u32) {
        let _ = self.detach_backing(id);
        let _ = self.resource_unref(id);
        self.free_resource_id(id);
        super::forget_resource(id);
    }
}
