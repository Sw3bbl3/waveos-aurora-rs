//! OHCI host controller driver (USB 1.1: 12 and 1.5 Mb/s devices).
//!
//! The controller runs three lists of endpoint descriptors (EDs): control
//! and bulk, which it serves as time allows, and the interrupt list, which
//! every frame of the 32-entry table in the shared HCCA leads to (so each
//! interrupt endpoint is polled every millisecond). Each ED holds a queue of
//! transfer descriptors (TDs) that ends in an empty "dummy" TD: to queue a
//! transfer we fill the dummy, chain new TDs after it, and move the ED's
//! tail to a fresh dummy. Retired TDs carry their completion code.
//!
//! OHCI is common on non-Intel chipsets, as EHCI's companion for slow
//! devices, and in virtual machines (VirtualBox's default USB controller).
//! It addresses only the first 4 GiB of memory.

use super::hcutil::{chunks, Addresses, Block, Events, Pool};
use super::{Bus, Endpoint, Hcd, HubInfo, PipeHandler, Position, Setup, Speed, ERROR, STALL, TIMEOUT};
use crate::drivers::block::Dma;
use crate::drivers::pci;
use crate::sync::{IrqMutex, Mutex};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicU32, AtomicUsize, Ordering};

// Registers.
const HC_CONTROL: u64 = 0x04;
const HC_COMMAND: u64 = 0x08;
const HC_INT_STATUS: u64 = 0x0C;
const HC_INT_ENABLE: u64 = 0x10;
const HC_INT_DISABLE: u64 = 0x14;
const HC_HCCA: u64 = 0x18;
const HC_CONTROL_HEAD: u64 = 0x20;
const HC_CONTROL_CURRENT: u64 = 0x24;
const HC_BULK_HEAD: u64 = 0x28;
const HC_BULK_CURRENT: u64 = 0x2C;
const HC_FM_INTERVAL: u64 = 0x34;
const HC_PERIODIC_START: u64 = 0x40;
const HC_LS_THRESHOLD: u64 = 0x44;
const HC_RH_DESC_A: u64 = 0x48;
const HC_RH_STATUS: u64 = 0x50;
const HC_RH_PORT: u64 = 0x54;

const CTL_CBSR: u32 = 3;
const CTL_PERIODIC: u32 = 1 << 2;
const CTL_CONTROL: u32 = 1 << 4;
const CTL_BULK: u32 = 1 << 5;
const CTL_OPERATIONAL: u32 = 2 << 6;
const CTL_STATE: u32 = 3 << 6;
/// Interrupts routed to SMM: firmware owns the controller.
const CTL_IR: u32 = 1 << 8;

const CMD_RESET: u32 = 1 << 0;
const CMD_CONTROL_FILLED: u32 = 1 << 1;
const CMD_BULK_FILLED: u32 = 1 << 2;
const CMD_OWNERSHIP: u32 = 1 << 3;

const INT_SO: u32 = 1 << 0;
const INT_WDH: u32 = 1 << 1;
const INT_UE: u32 = 1 << 4;
const INT_MIE: u32 = 1 << 31;
const INT_ALL: u32 = 0x4000_007F;

const RH_LPSC: u32 = 1 << 16;

const PORT_CCS: u32 = 1 << 0;
const PORT_PES: u32 = 1 << 1;
const PORT_PRS: u32 = 1 << 4;
const PORT_PPS: u32 = 1 << 8;
const PORT_LSDA: u32 = 1 << 9;
const PORT_CSC: u32 = 1 << 16;
const PORT_PRSC: u32 = 1 << 20;
const PORT_CHANGES: u32 = 0x1F << 16;

// ED dwords and bits.
const ED_INFO: usize = 0;
const ED_TAIL: usize = 1;
const ED_HEAD: usize = 2;
const ED_NEXT: usize = 3;
const ED_LOW_SPEED: u32 = 1 << 13;
const ED_SKIP: u32 = 1 << 14;
const HEAD_HALTED: u32 = 1 << 0;
const HEAD_CARRY: u32 = 1 << 1;

// TD dwords and bits.
const TD_INFO: usize = 0;
const TD_CBP: usize = 1;
const TD_NEXT: usize = 2;
const TD_BE: usize = 3;
const TD_ROUNDING: u32 = 1 << 18;
const TD_SETUP: u32 = 0 << 19;
const TD_OUT: u32 = 1 << 19;
const TD_IN: u32 = 2 << 19;
const TD_NO_INTERRUPT: u32 = 7 << 21;
const TD_DATA0: u32 = 2 << 24;
const TD_DATA1: u32 = 3 << 24;
const TD_NOT_ACCESSED: u32 = 0xF << 28;

const CC_OK: u32 = 0;
const CC_STALL: u32 = 4;
const CC_UNDERRUN: u32 = 9;

fn r32(a: u64) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn w32(a: u64, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}

fn cc(td: Block) -> u32 {
    td.read(TD_INFO) >> 28
}

/// An endpoint's ED and its current dummy TD; one transfer at a time.
struct Endp {
    ed: Block,
    bulk: bool,
    queue: Mutex<Block>,
}

struct Device {
    pos: Position,
    ep0: Arc<Endp>,
    eps: BTreeMap<u8, Arc<Endp>>,
}

struct Pipe {
    dev: u8,
    ed: Block,
    /// The TD in flight and the ED's current dummy.
    td: Block,
    dummy: Block,
    buf: Dma,
    size: usize,
    handler: Arc<dyn PipeHandler>,
    errors: u32,
}

pub struct Ohci {
    index: usize,
    regs: u64,
    ports: u8,
    pool: Pool,
    addresses: Addresses,
    events: Events,
    hcca: Dma,
    control_head: Block,
    bulk_head: Block,
    interrupt_head: Block,
    /// List members after their heads, in link order, for unlinking.
    lists: Mutex<[Vec<Block>; 3]>,
    devices: IrqMutex<BTreeMap<u8, Device>>,
    pipes: IrqMutex<Vec<Pipe>>,
    status: AtomicU32,
    bus: spin::Once<&'static Bus>,
}

const CONTROL: usize = 0;
const BULK: usize = 1;
const INTERRUPT: usize = 2;

static COUNT: AtomicUsize = AtomicUsize::new(0);

fn interrupt(arg: usize) -> bool {
    let c = unsafe { &*(arg as *const Ohci) };
    let sts = r32(c.regs + HC_INT_STATUS) & r32(c.regs + HC_INT_ENABLE) & !INT_MIE;
    if sts == 0 {
        return false;
    }
    w32(c.regs + HC_INT_STATUS, sts);
    c.status.fetch_or(sts, Ordering::AcqRel);
    c.events.signal();
    true
}

fn msi_interrupt(arg: usize) {
    interrupt(arg);
}

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(regs) = dev.map_bar(0) else { return };
    let pool = Pool::new();
    let (Some(hcca), Some(control_head), Some(bulk_head), Some(interrupt_head)) =
        (Dma::new32(256), pool.alloc(), pool.alloc(), pool.alloc())
    else {
        return;
    };
    // Each list starts with a skipped ED that never moves.
    for head in [control_head, bulk_head, interrupt_head] {
        head.write(ED_INFO, ED_SKIP);
    }
    for i in 0..32 {
        hcca.write::<u32>(i * 4, interrupt_head.phys);
    }
    let c: &'static Ohci = Box::leak(Box::new(Ohci {
        index: COUNT.fetch_add(1, Ordering::Relaxed),
        regs,
        ports: (r32(regs + HC_RH_DESC_A) & 0xFF).min(15) as u8,
        pool,
        addresses: Addresses::new(),
        events: Events::new(),
        hcca,
        control_head,
        bulk_head,
        interrupt_head,
        lists: Mutex::new([Vec::new(), Vec::new(), Vec::new()]),
        devices: IrqMutex::new(BTreeMap::new()),
        pipes: IrqMutex::new(Vec::new()),
        status: AtomicU32::new(0),
        bus: spin::Once::new(),
    }));
    if !c.start() {
        log!("ohci", "controller {} did not start", c.index);
        return;
    }
    let arg = c as *const Ohci as usize;
    if dev.enable_msi("ohci", msi_interrupt, arg).is_some() {
        c.events.set_msi();
    } else if let Some(line) = dev.enable_intx("ohci", interrupt, arg) {
        c.events.set_intx(line);
    }
    log!("ohci", "controller {}: {} ports", c.index, c.ports);
    c.bus.call_once(|| super::register(c));
    crate::sched::spawn_task("ohci-events", event_task_entry, arg as u64, 0, crate::mm::vmm::kernel_pml4());
}

fn event_task_entry(arg: u64) {
    unsafe { &*(arg as *const Ohci) }.event_task();
}

impl Ohci {
    fn port(&self, port: u8) -> u32 {
        r32(self.regs + HC_RH_PORT + (port as u64 - 1) * 4)
    }

    fn set_port(&self, port: u8, v: u32) {
        w32(self.regs + HC_RH_PORT + (port as u64 - 1) * 4, v);
    }

    /// Takes the controller from the firmware, resets it and runs it.
    fn start(&self) -> bool {
        let r = self.regs;
        if r32(r + HC_CONTROL) & CTL_IR != 0 {
            w32(r + HC_COMMAND, CMD_OWNERSHIP);
            if crate::drivers::block::wait_for(1000, || r32(r + HC_CONTROL) & CTL_IR == 0).is_err() {
                log!("ohci", "firmware did not release the controller; taking it anyway");
                w32(r + HC_CONTROL, r32(r + HC_CONTROL) & !CTL_IR);
            }
        }
        w32(r + HC_INT_DISABLE, INT_ALL | INT_MIE);
        let fm = r32(r + HC_FM_INTERVAL);
        w32(r + HC_COMMAND, CMD_RESET);
        if crate::drivers::block::wait_for(10, || r32(r + HC_COMMAND) & CMD_RESET == 0).is_err() {
            return false;
        }
        // Now suspended: we have 2 ms to make it operational.
        w32(r + HC_HCCA, self.hcca.phys as u32);
        w32(r + HC_CONTROL_HEAD, self.control_head.phys);
        w32(r + HC_CONTROL_CURRENT, 0);
        w32(r + HC_BULK_HEAD, self.bulk_head.phys);
        w32(r + HC_BULK_CURRENT, 0);
        let fi = match fm & 0x3FFF {
            0 => 11999,
            v => v,
        };
        let fsmps = 6 * (fi - 210) / 7;
        w32(r + HC_FM_INTERVAL, ((fm & 1 << 31) ^ 1 << 31) | fsmps << 16 | fi);
        w32(r + HC_PERIODIC_START, fi * 9 / 10);
        w32(r + HC_LS_THRESHOLD, 0x628);
        w32(r + HC_CONTROL, CTL_CBSR | CTL_PERIODIC | CTL_CONTROL | CTL_BULK | CTL_OPERATIONAL);
        if r32(r + HC_CONTROL) & CTL_STATE != CTL_OPERATIONAL {
            return false;
        }
        w32(r + HC_INT_STATUS, INT_ALL);
        w32(r + HC_INT_ENABLE, INT_MIE | INT_WDH | INT_UE | INT_SO);
        // Port power: globally, and per port where it switches separately.
        let desc = r32(r + HC_RH_DESC_A);
        w32(r + HC_RH_STATUS, RH_LPSC);
        for p in 1..=self.ports {
            self.set_port(p, PORT_PPS);
        }
        crate::sched::sleep_ms(((desc >> 24) as u64 * 2).max(20));
        true
    }

    fn event_task(&'static self) {
        loop {
            self.events.idle(4, || self.status.load(Ordering::Acquire) != 0);
            if self.events.live() {
                // Whatever an interrupt that went missing would have said.
                interrupt(self as *const Ohci as usize);
            }
            let mut sts = self.status.swap(0, Ordering::AcqRel);
            if !self.events.live() {
                let polled = r32(self.regs + HC_INT_STATUS) & (INT_WDH | INT_UE | INT_SO);
                w32(self.regs + HC_INT_STATUS, polled);
                sts |= polled;
            }
            if sts & INT_UE != 0 {
                log!("ohci", "controller {}: unrecoverable error", self.index);
            }
            self.poll_pipes();
        }
    }

    // -------------------------------------------------------------- lists

    fn head(&self, list: usize) -> Block {
        [self.control_head, self.bulk_head, self.interrupt_head][list]
    }

    fn link(&self, list: usize, ed: Block) {
        let mut lists = self.lists.lock();
        let head = self.head(list);
        ed.write(ED_NEXT, head.read(ED_NEXT));
        fence(Ordering::SeqCst);
        head.write(ED_NEXT, ed.phys);
        lists[list].insert(0, ed);
    }

    /// Takes an ED off its list; the controller may still be on it for this
    /// frame, so the caller frees it only after.
    fn unlink(&self, list: usize, ed: Block) {
        let mut lists = self.lists.lock();
        ed.write(ED_INFO, ed.read(ED_INFO) | ED_SKIP);
        let Some(i) = lists[list].iter().position(|e| *e == ed) else { return };
        let prev = if i == 0 { self.head(list) } else { lists[list][i - 1] };
        prev.write(ED_NEXT, ed.read(ED_NEXT));
        lists[list].remove(i);
        drop(lists);
        crate::sched::sleep_ms(2);
    }

    /// A new ED and its first dummy TD.
    fn new_ed(&self, addr: u8, pos: &Position, number: u8, max_packet: u16) -> Option<(Block, Block)> {
        let ed = self.pool.alloc()?;
        let Some(dummy) = self.pool.alloc() else {
            self.pool.free(ed);
            return None;
        };
        let low = if pos.speed == Speed::Low { ED_LOW_SPEED } else { 0 };
        ed.write(ED_INFO, addr as u32 | (number as u32) << 7 | low | (max_packet as u32 & 0x7FF) << 16);
        ed.write(ED_TAIL, dummy.phys);
        ed.write(ED_HEAD, dummy.phys);
        Some((ed, dummy))
    }

    // ---------------------------------------------------------- transfers

    fn fill_td(td: Block, flags: u32, phys: u64, len: usize) {
        td.write(TD_INFO, TD_NOT_ACCESSED | flags);
        if len == 0 {
            td.write(TD_CBP, 0);
            td.write(TD_BE, 0);
        } else {
            td.write(TD_CBP, phys as u32);
            td.write(TD_BE, (phys + len as u64 - 1) as u32);
        }
    }

    /// Queues `pieces` (TD flags, buffer, length) on an endpoint and waits.
    /// Returns the bytes the data pieces (`counted`) moved.
    fn run(&self, ep: &Endp, pieces: &[(u32, u64, usize)], counted: &[usize], timeout_ms: u64) -> Result<usize, u8> {
        let mut dummy = ep.queue.lock();
        let mut tds = Vec::with_capacity(pieces.len());
        tds.push(*dummy);
        for _ in 0..pieces.len() {
            match self.pool.alloc() {
                Some(b) => tds.push(b),
                None => {
                    for &b in &tds[1..] {
                        self.pool.free(b);
                    }
                    return Err(TIMEOUT);
                }
            }
        }
        let new_dummy = *tds.last().unwrap();
        tds.pop();
        for (i, &(flags, phys, len)) in pieces.iter().enumerate() {
            Self::fill_td(tds[i], flags, phys, len);
            let next = if i + 1 < tds.len() { tds[i + 1] } else { new_dummy };
            tds[i].write(TD_NEXT, next.phys);
        }
        fence(Ordering::SeqCst);
        ep.ed.write(ED_TAIL, new_dummy.phys);
        *dummy = new_dummy;
        w32(self.regs + HC_COMMAND, if ep.bulk { CMD_BULK_FILLED } else { CMD_CONTROL_FILLED });

        // The controller writes a retired TD a word at a time and moves the
        // ED's head on afterwards: the transfer is over once the head reaches
        // the dummy (or the ED halts), never on a TD's code alone.
        let state = || -> Option<Result<(), u8>> {
            let head = ep.ed.read(ED_HEAD);
            if head & !0xF != new_dummy.phys && head & HEAD_HALTED == 0 {
                return None;
            }
            for &td in &tds {
                match cc(td) {
                    14 | 15 => return None,
                    CC_OK => {}
                    // A short IN piece: the transfer ends there (the ED halts).
                    CC_UNDERRUN => return Some(Ok(())),
                    CC_STALL => return Some(Err(STALL)),
                    _ => return Some(Err(ERROR)),
                }
            }
            Some(Ok(()))
        };
        self.events.wait(timeout_ms, || state().is_some());
        let result = state().unwrap_or(Err(TIMEOUT));
        if result == Err(TIMEOUT) {
            let codes: Vec<u32> = tds.iter().map(|&t| cc(t)).collect();
            log!(
                "ohci",
                "transfer timed out: ED {:#x} head {:#x} tail {:#x}, TD codes {:?}, control {:#x}",
                ep.ed.read(ED_INFO),
                ep.ed.read(ED_HEAD),
                ep.ed.read(ED_TAIL),
                codes,
                r32(self.regs + HC_CONTROL)
            );
        }
        let mut moved = 0;
        for &i in counted {
            let td = tds[i];
            let (_, phys, len) = pieces[i];
            match cc(td) {
                CC_OK | CC_UNDERRUN => {
                    let cbp = td.read(TD_CBP);
                    moved += if cbp == 0 { len } else { (cbp as u64 - phys) as usize };
                }
                _ => {}
            }
        }
        // Whatever is left of the queue (after a short piece, an error or a
        // timeout): skip the ED, drop the TDs, keep the toggle, un-halt.
        if ep.ed.read(ED_HEAD) & !0xF != new_dummy.phys || result == Err(TIMEOUT) {
            ep.ed.write(ED_INFO, ep.ed.read(ED_INFO) | ED_SKIP);
            crate::sched::sleep_ms(2);
            let carry = ep.ed.read(ED_HEAD) & HEAD_CARRY;
            ep.ed.write(ED_HEAD, new_dummy.phys | carry);
            ep.ed.write(ED_INFO, ep.ed.read(ED_INFO) & !ED_SKIP);
        } else if ep.ed.read(ED_HEAD) & HEAD_HALTED != 0 {
            ep.ed.write(ED_HEAD, ep.ed.read(ED_HEAD) & !HEAD_HALTED);
        }
        for td in tds {
            self.pool.free(td);
        }
        result.map(|_| moved)
    }

    fn endpoint(&self, dev: u8, address: Option<u8>) -> Option<Arc<Endp>> {
        let devs = self.devices.lock();
        let d = devs.get(&dev)?;
        match address {
            None => Some(d.ep0.clone()),
            Some(a) => d.eps.get(&a).cloned(),
        }
    }

    // -------------------------------------------------------------- pipes

    fn arm_pipe(&self, p: &mut Pipe) {
        Self::fill_td(p.td, TD_IN | TD_ROUNDING, p.buf.phys, p.size);
        p.td.write(TD_NEXT, p.dummy.phys);
        fence(Ordering::SeqCst);
        p.ed.write(ED_TAIL, p.dummy.phys);
    }

    fn poll_pipes(&self) {
        let mut ready = Vec::new();
        {
            let mut pipes = self.pipes.lock();
            for p in pipes.iter_mut() {
                // Retired once the ED's head has moved on to the dummy (or halted).
                let head = p.ed.read(ED_HEAD);
                if head & !0xF != p.dummy.phys && head & HEAD_HALTED == 0 {
                    continue;
                }
                let code = cc(p.td);
                if code == CC_OK || code == CC_UNDERRUN {
                    let cbp = p.td.read(TD_CBP);
                    let n = if cbp == 0 { p.size } else { (cbp as u64 - p.buf.phys) as usize };
                    ready.push((p.handler.clone(), p.buf.bytes()[..n.min(p.size)].to_vec()));
                    p.errors = 0;
                } else {
                    p.errors += 1;
                }
                if p.ed.read(ED_HEAD) & HEAD_HALTED != 0 {
                    let carry = p.ed.read(ED_HEAD) & HEAD_CARRY;
                    p.ed.write(ED_HEAD, p.dummy.phys | carry);
                }
                if p.errors >= 100 {
                    continue;
                }
                // The retired TD becomes the next dummy; the dummy carries the next poll.
                core::mem::swap(&mut p.td, &mut p.dummy);
                self.arm_pipe(p);
            }
        }
        for (h, data) in ready {
            h.data(&data);
        }
    }
}

impl Hcd for Ohci {
    fn kind(&self) -> &'static str {
        "OHCI"
    }

    fn ports(&self) -> u8 {
        self.ports
    }

    fn changed_ports(&self) -> Vec<u8> {
        (1..=self.ports).filter(|&p| self.port(p) & PORT_CSC != 0).collect()
    }

    fn port_connected(&self, port: u8) -> bool {
        let sc = self.port(port);
        self.set_port(port, sc & PORT_CHANGES);
        sc & PORT_CCS != 0
    }

    fn port_reset(&self, port: u8) -> Option<Speed> {
        self.set_port(port, PORT_PRS);
        if crate::drivers::block::wait_for(100, || self.port(port) & PORT_PRSC != 0).is_err() {
            log!("usb", "OHCI port {}: reset timed out", port);
            return None;
        }
        self.set_port(port, PORT_PRSC);
        crate::sched::sleep_ms(2);
        let sc = self.port(port);
        if sc & PORT_PES == 0 {
            log!("usb", "OHCI port {}: not enabled after reset", port);
            return None;
        }
        Some(if sc & PORT_LSDA != 0 { Speed::Low } else { Speed::Full })
    }

    fn new_device(&self, pos: &Position) -> Option<u8> {
        let addr = self.addresses.alloc()?;
        let Some((ed, dummy)) = self.new_ed(0, pos, 0, 8) else {
            self.addresses.free(addr);
            return None;
        };
        self.link(CONTROL, ed);
        let ep0 = Arc::new(Endp { ed, bulk: false, queue: Mutex::new(dummy) });
        self.devices.lock().insert(addr, Device { pos: *pos, ep0, eps: BTreeMap::new() });
        let set_address = Setup { request_type: 0, request: 5, value: addr as u16, index: 0, length: 0 };
        if self.control(addr, set_address, None).is_err() {
            log!("usb", "OHCI port {}: SET_ADDRESS failed", pos.root_port);
            self.release(addr);
            return None;
        }
        crate::sched::sleep_ms(2);
        ed.write(ED_INFO, (ed.read(ED_INFO) & !0x7F) | addr as u32);
        Some(addr)
    }

    fn set_ep0_max_packet(&self, dev: u8, max_packet: u16) {
        if let Some(ep) = self.endpoint(dev, None) {
            let _q = ep.queue.lock();
            ep.ed.write(ED_INFO, (ep.ed.read(ED_INFO) & !(0x7FF << 16)) | (max_packet as u32 & 0x7FF) << 16);
        }
    }

    fn control(&self, dev: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        let ep = self.endpoint(dev, None).ok_or(TIMEOUT)?;
        let len = setup.length as usize;
        let mut buf = Dma::new32(8 + len.max(1)).ok_or(TIMEOUT)?;
        buf.bytes_mut()[..8].copy_from_slice(&setup.bytes().to_le_bytes());
        if let (Some(d), false) = (&data, setup.is_in()) {
            buf.bytes_mut()[8..8 + len].copy_from_slice(&d[..len]);
        }
        let (data_dir, status_dir) = if setup.is_in() { (TD_IN, TD_OUT) } else { (TD_OUT, TD_IN) };
        let status_dir = if len == 0 { TD_IN } else { status_dir };
        let mut pieces = alloc::vec![(TD_SETUP | TD_DATA0 | TD_NO_INTERRUPT, buf.phys, 8)];
        if len > 0 {
            pieces.push((data_dir | TD_DATA1 | TD_ROUNDING | TD_NO_INTERRUPT, buf.phys + 8, len));
        }
        pieces.push((status_dir | TD_DATA1, 0, 0));
        let counted: &[usize] = if len > 0 { &[1] } else { &[] };
        let n = self.run(&ep, &pieces, counted, 2000)?;
        if let (Some(d), true) = (data, setup.is_in()) {
            d[..n].copy_from_slice(&buf.bytes()[8..8 + n]);
        }
        Ok(n)
    }

    fn transfer(&self, dev: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8> {
        let endp = self.endpoint(dev, Some(ep.address)).ok_or(TIMEOUT)?;
        let dir = if ep.is_in() { TD_IN } else { TD_OUT };
        let parts = chunks(phys, len, 4096, 4096);
        let last = parts.len() - 1;
        let pieces: Vec<(u32, u64, usize)> = parts
            .iter()
            .enumerate()
            .map(|(i, &(p, n))| {
                // Only the last piece may come up short without halting the ED.
                let flags = if i == last { dir | TD_ROUNDING } else { dir | TD_NO_INTERRUPT };
                (flags, p, n)
            })
            .collect();
        let counted: Vec<usize> = (0..pieces.len()).collect();
        self.run(&endp, &pieces, &counted, timeout_ms)
    }

    fn configure(&self, dev: u8, eps: &[Endpoint], _hub: Option<HubInfo>) -> bool {
        let Some(pos) = self.devices.lock().get(&dev).map(|d| d.pos) else { return false };
        for e in eps.iter().filter(|e| e.kind == 2 || (e.kind == 3 && !e.is_in())) {
            if self.endpoint(dev, Some(e.address)).is_some() {
                continue;
            }
            let Some((ed, dummy)) = self.new_ed(dev, &pos, e.number(), e.max_packet) else { return false };
            let (list, bulk) = if e.kind == 2 { (BULK, true) } else { (INTERRUPT, false) };
            self.link(list, ed);
            let endp = Arc::new(Endp { ed, bulk, queue: Mutex::new(dummy) });
            if let Some(d) = self.devices.lock().get_mut(&dev) {
                d.eps.insert(e.address, endp);
            }
        }
        true
    }

    fn open_pipe(&self, dev: u8, ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool {
        let Some(pos) = self.devices.lock().get(&dev).map(|d| d.pos) else { return false };
        let size = (ep.max_packet as usize).clamp(8, 1023);
        let Some((ed, td)) = self.new_ed(dev, &pos, ep.number(), ep.max_packet) else { return false };
        let (Some(dummy), Some(buf)) = (self.pool.alloc(), Dma::new32(size)) else { return false };
        let mut pipe = Pipe { dev, ed, td, dummy, buf, size, handler, errors: 0 };
        self.arm_pipe(&mut pipe);
        self.link(INTERRUPT, ed);
        self.pipes.lock().push(pipe);
        true
    }

    fn reset_toggle(&self, dev: u8, ep_address: u8) {
        if let Some(ep) = self.endpoint(dev, Some(ep_address)) {
            let _q = ep.queue.lock();
            ep.ed.write(ED_HEAD, ep.ed.read(ED_HEAD) & !HEAD_CARRY);
        }
    }

    fn release(&self, dev: u8) {
        let pipes: Vec<Pipe> = {
            let mut all = self.pipes.lock();
            let (mine, rest): (Vec<_>, Vec<_>) = all.drain(..).partition(|p| p.dev == dev);
            *all = rest;
            mine
        };
        for p in pipes {
            self.unlink(INTERRUPT, p.ed);
            self.pool.free(p.td);
            self.pool.free(p.dummy);
            self.pool.free(p.ed);
        }
        if let Some(d) = self.devices.lock().remove(&dev) {
            for ep in core::iter::once(&d.ep0).chain(d.eps.values()) {
                let dummy = ep.queue.lock();
                let list = if ep.bulk {
                    BULK
                } else if Arc::ptr_eq(ep, &d.ep0) {
                    CONTROL
                } else {
                    INTERRUPT
                };
                self.unlink(list, ep.ed);
                self.pool.free(*dummy);
                self.pool.free(ep.ed);
            }
        }
        self.addresses.free(dev);
    }

    fn restart(&self) -> bool {
        let devs: Vec<u8> = self.devices.lock().keys().copied().collect();
        for d in devs {
            self.release(d);
        }
        for head in [self.control_head, self.bulk_head, self.interrupt_head] {
            head.write(ED_NEXT, 0);
        }
        *self.lists.lock() = [Vec::new(), Vec::new(), Vec::new()];
        self.addresses.clear();
        self.start()
    }

    fn dma(&self, bytes: usize) -> Option<Dma> {
        Dma::new32(bytes)
    }

    fn interrupts(&self) -> u64 {
        self.events.count.load(Ordering::Relaxed)
    }
}
