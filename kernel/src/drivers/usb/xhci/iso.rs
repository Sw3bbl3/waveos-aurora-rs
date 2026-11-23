//! xHCI isochronous streams.
//!
//! Each service interval is one transfer descriptor (TD): an Isoch TRB, plus
//! a Normal TRB when its buffer crosses a 64 KiB boundary. Every TD is
//! queued with "Start Isoch ASAP" (SIA), so the controller schedules it in
//! the interval after the previous one, or as soon as it can after the ring
//! ran dry, and interrupts on completion (IOC). A stream keeps `depth` TDs
//! on the ring: as each completes, the event task hands IN data to the
//! handler and queues the next TD (asking the handler to fill OUT data).
//!
//! The ring is FIFO, so an event for TD *k* retires every TD queued before
//! it, too: those were missed without an event of their own. A ring
//! underrun (OUT) or overrun (IN) means the controller found the ring empty,
//! so everything we thought was queued has been retired.

use super::super::iso::{IsoError, IsoHandler, IsoStats, MISSED, OVERRUN, UNDERRUN};
use super::super::{Endpoint, Speed};
use super::TRB_SET_DEQUEUE;
use super::{Controller, CC_SHORT, CC_SUCCESS, TRB_CHAIN, TRB_CONFIGURE_EP, TRB_IOC, TRB_ISP, TRB_NORMAL};
use crate::drivers::block::Dma;
use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

const TRB_ISOCH: u32 = 5;
const TRB_STOP_EP: u32 = 15;
/// Start Isoch ASAP: schedule in the next free interval (no frame ID).
const TRB_SIA: u32 = 1 << 31;

pub(super) const CC_STOPPED: u8 = 26;
pub(super) const CC_STOPPED_LENGTH: u8 = 27;

/// One queued TD: its buffer index and TRBs (address, length).
struct Td {
    index: usize,
    trbs: [(u64, u32); 2],
    count: usize,
}

pub(super) struct Stream {
    handler: Arc<dyn IsoHandler>,
    pub(super) stats: Arc<IsoStats>,
    dir_in: bool,
    max_packet: usize,
    /// Packets per burst, minus one (SuperSpeed).
    burst: usize,
    super_speed: bool,
    /// Largest payload per interval (each TD's buffer size).
    td_size: usize,
    /// Each TD's buffer: a DMA block and the offset in it.
    bufs: Vec<(Arc<Dma>, usize)>,
    free: Vec<usize>,
    queued: VecDeque<Td>,
    closing: bool,
}

/// What a completed TD hands its handler.
struct Done {
    index: usize,
    len: usize,
    status: u8,
}

impl Controller {
    pub(super) fn open_iso_inner(
        &self,
        slot: u8,
        ep: &Endpoint,
        depth: usize,
        handler: Arc<dyn IsoHandler>,
    ) -> Result<(), IsoError> {
        let dci = ep.dci();
        let speed = {
            let inner = self.inner.lock();
            let st = inner.slots.get(&slot).ok_or(IsoError::NotConfigured)?;
            if st.rings[dci as usize].is_none() {
                return Err(IsoError::NotConfigured);
            }
            st.speed
        };
        if self.isos.lock().contains_key(&(slot, dci)) {
            return Err(IsoError::Busy);
        }
        let depth = depth.clamp(2, 64);
        let td_size = super::super::iso::esit_payload(ep).max(1);
        // Small TDs share pages (a power-of-two stride never crosses one);
        // big ones get their own block.
        let stride = td_size.next_power_of_two().max(64);
        let mut bufs = Vec::with_capacity(depth);
        if stride <= 4096 {
            let block = Arc::new(Dma::new(stride * depth).ok_or(IsoError::NoMemory)?);
            for i in 0..depth {
                bufs.push((block.clone(), i * stride));
            }
        } else {
            for _ in 0..depth {
                bufs.push((Arc::new(Dma::new(td_size).ok_or(IsoError::NoMemory)?), 0));
            }
        }
        let stream = Stream {
            handler,
            stats: Arc::new(IsoStats::default()),
            dir_in: ep.is_in(),
            max_packet: (ep.max_packet as usize).max(1),
            burst: ep.burst as usize,
            super_speed: matches!(speed, Speed::Super | Speed::SuperPlus),
            td_size,
            bufs,
            free: (0..depth).rev().collect(),
            queued: VecDeque::new(),
            closing: false,
        };
        self.isos.lock().insert((slot, dci), stream);
        self.iso_refill(slot, dci);
        Ok(())
    }

    /// Stops the endpoint, empties its ring and ends the stream.
    pub(super) fn close_iso_inner(&self, slot: u8, ep_address: u8) {
        let dci = (ep_address & 0x0F) * 2 + (ep_address >> 7);
        match self.isos.lock().get_mut(&(slot, dci)) {
            Some(s) => s.closing = true,
            None => return,
        }
        self.command(0, TRB_STOP_EP << 10 | (dci as u32) << 16 | (slot as u32) << 24);
        // Point the controller past everything still queued.
        let dequeue = {
            let inner = self.inner.lock();
            inner.slots.get(&slot).and_then(|s| s.rings[dci as usize].as_ref()).map(|r| r.dequeue_pointer())
        };
        if let Some(d) = dequeue {
            self.command(d, TRB_SET_DEQUEUE << 10 | (dci as u32) << 16 | (slot as u32) << 24);
        }
        let stream = self.isos.lock().remove(&(slot, dci));
        if let Some(s) = stream {
            s.handler.stopped();
        }
    }

    /// Ends streams without touching the hardware (the device or the
    /// controller is gone or being reset).
    pub(super) fn stop_isos(&self, which: impl Fn((u8, u8)) -> bool) {
        let gone: Vec<Stream> = {
            let mut isos = self.isos.lock();
            let keys: Vec<(u8, u8)> = isos.keys().copied().filter(|&k| which(k)).collect();
            keys.iter().filter_map(|k| isos.remove(k)).collect()
        };
        for s in gone {
            s.handler.stopped();
        }
    }

    /// Takes endpoints off the schedule (Configure Endpoint with drop flags).
    pub(super) fn drop_endpoints_inner(&self, slot: u8, eps: &[Endpoint]) -> bool {
        for e in eps {
            self.close_iso_inner(slot, e.address);
        }
        let input_phys = {
            let inner = self.inner.lock();
            let Some(st) = inner.slots.get(&slot) else { return false };
            let mut drop = 0u32;
            for e in eps.iter().filter(|e| st.rings[e.dci() as usize].is_some()) {
                drop |= 1 << e.dci();
            }
            if drop == 0 {
                return true;
            }
            st.input.write::<u32>(0, drop);
            st.input.write::<u32>(4, 1); // the slot context, unchanged
            for dw in 0..4 {
                let v = st.output.read::<u32>(dw * 4);
                st.input.write::<u32>(self.ctx_size + dw * 4, v);
            }
            st.input.phys
        };
        let c = self.command(input_phys, TRB_CONFIGURE_EP << 10 | (slot as u32) << 24);
        let mut inner = self.inner.lock();
        if let Some(st) = inner.slots.get_mut(&slot) {
            st.input.write::<u32>(0, 0);
            if c.code == CC_SUCCESS {
                for e in eps {
                    st.rings[e.dci() as usize] = None;
                }
            }
        }
        if c.code != CC_SUCCESS {
            log!("usb", "slot {}: dropping endpoints failed ({})", slot, c.code);
        }
        c.code == CC_SUCCESS
    }

    /// A transfer event on a stream's endpoint.
    pub(super) fn iso_event(&self, slot: u8, dci: u8, trb: u64, code: u8, residual: u32) {
        let (handler, dir_in, done, bufs) = {
            // Held so a TD can't complete between being queued and recorded.
            let _ring = self.inner.lock();
            let mut isos = self.isos.lock();
            let Some(s) = isos.get_mut(&(slot, dci)) else { return };
            if s.closing {
                return;
            }
            let mut done = Vec::new();
            if code == UNDERRUN || code == OVERRUN {
                s.stats.dry.fetch_add(1, Ordering::Relaxed);
                while let Some(td) = s.queued.pop_front() {
                    s.stats.missed.fetch_add(1, Ordering::Relaxed);
                    done.push(Done { index: td.index, len: 0, status: MISSED });
                }
            } else {
                let Some(pos) = s.queued.iter().position(|td| td.trbs[..td.count].iter().any(|t| t.0 == trb)) else {
                    // A second event for a TD already retired (short packet then IOC).
                    return;
                };
                for _ in 0..pos {
                    let td = s.queued.pop_front().unwrap();
                    s.stats.missed.fetch_add(1, Ordering::Relaxed);
                    done.push(Done { index: td.index, len: 0, status: MISSED });
                }
                let td = s.queued.pop_front().unwrap();
                let (len, status) = match code {
                    CC_SUCCESS | CC_SHORT => {
                        // Bytes up to the TRB that reported, less what it left.
                        let at = td.trbs[..td.count].iter().position(|t| t.0 == trb).unwrap_or(0);
                        let before: u32 = td.trbs[..at].iter().map(|t| t.1).sum();
                        let len = before + td.trbs[at].1.saturating_sub(residual);
                        s.stats.completed.fetch_add(1, Ordering::Relaxed);
                        (len as usize, super::super::OK)
                    }
                    MISSED => {
                        s.stats.missed.fetch_add(1, Ordering::Relaxed);
                        (0, MISSED)
                    }
                    _ => {
                        s.stats.errors.fetch_add(1, Ordering::Relaxed);
                        (0, code)
                    }
                };
                done.push(Done { index: td.index, len, status });
            }
            let bufs: Vec<(Arc<Dma>, usize)> = done.iter().map(|d| s.bufs[d.index].clone()).collect();
            (s.handler.clone(), s.dir_in, done, bufs)
        };
        if dir_in {
            for (d, (buf, off)) in done.iter().zip(&bufs) {
                let data = &buf.bytes()[*off..*off + d.len];
                handler.data(data, d.status);
            }
        }
        if let Some(s) = self.isos.lock().get_mut(&(slot, dci)) {
            s.free.extend(done.iter().map(|d| d.index));
        }
        self.iso_refill(slot, dci);
    }

    /// Queues a TD for every free buffer, then rings the doorbell.
    fn iso_refill(&self, slot: u8, dci: u8) {
        let mut queued = false;
        loop {
            let (index, buf, off, td_size, handler, dir_in) = {
                let mut isos = self.isos.lock();
                let Some(s) = isos.get_mut(&(slot, dci)) else { break };
                if s.closing {
                    break;
                }
                let Some(index) = s.free.pop() else { break };
                let (buf, off) = s.bufs[index].clone();
                (index, buf, off, s.td_size, s.handler.clone(), s.dir_in)
            };
            let len = if dir_in {
                td_size
            } else {
                // The buffer is ours until the TD is queued.
                let bytes = unsafe { core::slice::from_raw_parts_mut((buf.virt() as usize + off) as *mut u8, td_size) };
                handler.fill(bytes).min(td_size)
            };
            let phys = buf.phys + off as u64;
            let mut inner = self.inner.lock();
            let mut isos = self.isos.lock();
            let Some(s) = isos.get_mut(&(slot, dci)) else { break };
            let Some(ring) = inner.slots.get_mut(&slot).and_then(|st| st.rings[dci as usize].as_mut()) else {
                s.free.push(index);
                break;
            };
            // Packets in the TD, and how they group into bursts.
            let packets = len.div_ceil(s.max_packet).max(1);
            let (tbc, tlbpc) = if s.super_speed {
                let per = s.burst + 1;
                let rem = packets % per;
                (packets.div_ceil(per) - 1, if rem == 0 { per - 1 } else { rem - 1 })
            } else {
                (0, packets - 1)
            };
            let first = ((0x1_0000 - (phys & 0xFFFF)) as usize).min(len);
            let split = first < len;
            let base = TRB_SIA | (tbc.min(3) as u32) << 7 | (tlbpc.min(15) as u32) << 16;
            let isp = if s.dir_in { TRB_ISP } else { 0 };
            let mut td = Td { index, trbs: [(0, 0); 2], count: 1 };
            if split {
                let rest = len - first;
                let left = rest.div_ceil(s.max_packet).min(31) as u32;
                let a = ring.push(phys, first as u32 | left << 17, TRB_ISOCH << 10 | base | isp | TRB_CHAIN);
                let b = ring.push(phys + first as u64, rest as u32, TRB_NORMAL << 10 | isp | TRB_IOC);
                td.trbs = [(a, first as u32), (b, rest as u32)];
                td.count = 2;
            } else {
                let a = ring.push(phys, len as u32, TRB_ISOCH << 10 | base | isp | TRB_IOC);
                td.trbs[0] = (a, len as u32);
            }
            s.queued.push_back(td);
            queued = true;
        }
        if queued {
            self.doorbell(slot, dci);
        }
    }
}
