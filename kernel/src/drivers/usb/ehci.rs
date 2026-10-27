//! EHCI host controller driver (USB 2.0, 480 Mb/s).
//!
//! The controller walks two schedules in memory. The asynchronous schedule
//! is a ring of queue heads (QHs), one per control or bulk endpoint, run as
//! bandwidth allows; the periodic schedule is a frame list whose every entry
//! leads to the interrupt endpoints' QHs, polled each millisecond. A QH
//! carries a queue of transfer descriptors (qTDs) that always ends in an
//! inactive "dummy": to queue a transfer we fill the dummy, chain the rest
//! and a new dummy after it, and make the first qTD active last. The
//! controller owns the QH's overlay; we write it only while the QH is out
//! of the schedule or halted.
//!
//! EHCI drives only high-speed devices on its own ports: a slower device on
//! a root port is handed to a companion controller (OHCI or UHCI), which
//! sees it connect. Slow devices behind a high-speed hub stay here, reached
//! through the hub's transaction translator with split transactions — which
//! is how newer chipsets without companions (an integrated "rate matching"
//! hub on each port) run keyboards and mice.

use super::hcutil::{Addresses, Block, Events, Pool};
use super::{Bus, Endpoint, Hcd, HubInfo, PipeHandler, Position, Setup, Speed, ERROR, STALL, TIMEOUT};
use crate::drivers::block::Dma;
use crate::drivers::pci;
use crate::sync::{IrqMutex, Mutex};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicU32, AtomicUsize, Ordering};

// Capability registers.
const CAPLENGTH: u64 = 0x00;
const HCSPARAMS: u64 = 0x04;
const HCCPARAMS: u64 = 0x08;

// Operational registers.
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const USBINTR: u64 = 0x08;
const FRINDEX: u64 = 0x0C;
const CTRLDSSEGMENT: u64 = 0x10;
const PERIODICLISTBASE: u64 = 0x14;
const ASYNCLISTADDR: u64 = 0x18;
const CONFIGFLAG: u64 = 0x40;
const PORTSC: u64 = 0x44;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_PERIODIC: u32 = 1 << 4;
const CMD_ASYNC: u32 = 1 << 5;
const CMD_IAA_DOORBELL: u32 = 1 << 6;
/// Interrupt threshold: at most one interrupt per microframe (125 µs).
const CMD_ITC_1: u32 = 1 << 16;

const STS_INT: u32 = 1 << 0;
const STS_ERR: u32 = 1 << 1;
const STS_PORT: u32 = 1 << 2;
const STS_HSE: u32 = 1 << 4;
const STS_IAA: u32 = 1 << 5;
const STS_HALTED: u32 = 1 << 12;
/// The write-1-to-clear status bits.
const STS_ACK: u32 = 0x3F;

const PORT_CCS: u32 = 1 << 0;
const PORT_CSC: u32 = 1 << 1;
const PORT_PE: u32 = 1 << 2;
const PORT_PEC: u32 = 1 << 3;
const PORT_OCC: u32 = 1 << 5;
const PORT_RESET: u32 = 1 << 8;
const PORT_POWER: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;
/// Change bits (written back as 1 only to clear them).
const PORT_RWC: u32 = PORT_CSC | PORT_PEC | PORT_OCC;

// Link pointers.
const T: u32 = 1;
const TYPE_QH: u32 = 1 << 1;

// Queue head dwords.
const QH_LINK: usize = 0;
const QH_INFO1: usize = 1;
const QH_INFO2: usize = 2;
const QH_NEXT: usize = 4;
const QH_ALT: usize = 5;
const QH_TOKEN: usize = 6;

// qTD dwords.
const TD_NEXT: usize = 0;
const TD_ALT: usize = 1;
const TD_TOKEN: usize = 2;
const TD_BUF: usize = 3;

// qTD token.
const TOK_ACTIVE: u32 = 1 << 7;
const TOK_HALTED: u32 = 1 << 6;
const TOK_ERRORS: u32 = 0x38; // data buffer error, babble, transaction error
const PID_OUT: u32 = 0 << 8;
const PID_IN: u32 = 1 << 8;
const PID_SETUP: u32 = 2 << 8;
const TOK_CERR: u32 = 3 << 10;
const TOK_IOC: u32 = 1 << 15;
const TOK_TOGGLE: u32 = 1 << 31;

/// Bytes per qTD: a multiple of every packet size that fits in its five
/// page pointers wherever the buffer starts.
const TD_BYTES: usize = 16 * 1024;

fn r32(a: u64) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn w32(a: u64, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}

/// An endpoint's queue head and its dummy qTD; one transfer at a time.
struct Endp {
    qh: Block,
    periodic: bool,
    dummy: Mutex<Block>,
}

struct Device {
    pos: Position,
    ep0: Arc<Endp>,
    eps: BTreeMap<u8, Arc<Endp>>,
}

/// An interrupt IN endpoint being polled for reports.
struct Pipe {
    dev: u8,
    qh: Block,
    /// The qTD in flight, and the dummy after it.
    td: Block,
    dummy: Block,
    buf: Dma,
    size: usize,
    handler: Arc<dyn PipeHandler>,
    errors: u32,
}

pub struct Ehci {
    index: usize,
    op: u64,
    ports: u8,
    companions: u8,
    pool: Pool,
    addresses: Addresses,
    events: Events,
    frame_list: Dma,
    async_head: Block,
    periodic_head: Block,
    /// The schedules' QHs in link order (after their heads), for unlinking.
    async_ring: Mutex<Vec<Block>>,
    periodic_chain: Mutex<Vec<Block>>,
    devices: IrqMutex<BTreeMap<u8, Device>>,
    pipes: IrqMutex<Vec<Pipe>>,
    /// Status bits the interrupt handler acknowledged.
    status: AtomicU32,
    bus: spin::Once<&'static Bus>,
}

static COUNT: AtomicUsize = AtomicUsize::new(0);

fn interrupt(arg: usize) -> bool {
    let c = unsafe { &*(arg as *const Ehci) };
    let sts = r32(c.op + USBSTS) & STS_ACK;
    if sts == 0 {
        return false;
    }
    w32(c.op + USBSTS, sts);
    c.status.fetch_or(sts, Ordering::AcqRel);
    c.events.signal();
    true
}

fn msi_interrupt(arg: usize) {
    interrupt(arg);
}

/// Takes the controller from the firmware (which may be running it for a
/// USB keyboard in "legacy support" mode).
fn take_ownership(dev: &pci::Device, eecp: u16) {
    let mut off = eecp;
    let mut guard = 0;
    while off >= 0x40 && guard < 16 {
        let cap = dev.read32(off);
        if cap & 0xFF == 1 {
            dev.write32(off, cap | 1 << 24);
            let released = crate::drivers::block::wait_for(1000, || dev.read32(off) & (1 << 16) == 0).is_ok();
            if !released {
                log!("ehci", "firmware did not release the controller; taking it anyway");
                dev.write32(off, dev.read32(off) & !(1 << 16));
            }
            // No more SMIs; clear the ones pending.
            dev.write32(off + 4, 0xE000_0000);
        }
        off = ((cap >> 8) & 0xFF) as u16;
        guard += 1;
    }
}

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(cap) = dev.map_bar(0) else { return };
    let op = cap + (r32(cap + CAPLENGTH) & 0xFF) as u64;
    let hcs = r32(cap + HCSPARAMS);
    let hcc = r32(cap + HCCPARAMS);
    take_ownership(dev, ((hcc >> 8) & 0xFF) as u16);

    // Stop and reset.
    w32(op + USBCMD, r32(op + USBCMD) & !CMD_RUN);
    if crate::drivers::block::wait_for(20, || r32(op + USBSTS) & STS_HALTED != 0).is_err() {
        log!("ehci", "controller did not halt");
    }
    w32(op + USBCMD, CMD_RESET);
    if crate::drivers::block::wait_for(250, || r32(op + USBCMD) & CMD_RESET == 0).is_err() {
        log!("ehci", "controller reset timed out");
        return;
    }

    let pool = Pool::new();
    let (Some(frame_list), Some(async_head), Some(periodic_head)) = (Dma::new32(4096), pool.alloc(), pool.alloc())
    else {
        return;
    };
    // The async ring's head: points at itself, never runs (halted overlay).
    async_head.write(QH_LINK, async_head.phys | TYPE_QH);
    async_head.write(QH_INFO1, 1 << 15 | 2 << 12);
    async_head.write(QH_INFO2, 1 << 30);
    async_head.write(QH_NEXT, T);
    async_head.write(QH_ALT, T);
    async_head.write(QH_TOKEN, TOK_HALTED);
    // The periodic schedule's anchor, which every frame leads to.
    periodic_head.write(QH_LINK, T);
    periodic_head.write(QH_INFO1, 2 << 12);
    periodic_head.write(QH_INFO2, 1 << 30 | 1);
    periodic_head.write(QH_NEXT, T);
    periodic_head.write(QH_ALT, T);
    for i in 0..1024 {
        frame_list.write::<u32>(i * 4, periodic_head.phys | TYPE_QH);
    }

    let c: &'static Ehci = Box::leak(Box::new(Ehci {
        index: COUNT.fetch_add(1, Ordering::Relaxed),
        op,
        ports: (hcs & 0xF) as u8,
        companions: ((hcs >> 12) & 0xF) as u8,
        pool,
        addresses: Addresses::new(),
        events: Events::new(),
        frame_list,
        async_head,
        periodic_head,
        async_ring: Mutex::new(Vec::new()),
        periodic_chain: Mutex::new(Vec::new()),
        devices: IrqMutex::new(BTreeMap::new()),
        pipes: IrqMutex::new(Vec::new()),
        status: AtomicU32::new(0),
        bus: spin::Once::new(),
    }));
    let arg = c as *const Ehci as usize;
    if dev.enable_msi("ehci", msi_interrupt, arg).is_some() {
        c.events.set_msi();
    } else if let Some(line) = dev.enable_intx("ehci", interrupt, arg) {
        c.events.set_intx(line);
    }
    if !c.start(hcs & (1 << 4) != 0) {
        log!("ehci", "controller {} did not start", c.index);
        return;
    }
    log!(
        "ehci",
        "controller {}: {} ports, {} companion controller(s), {}",
        c.index,
        c.ports,
        c.companions,
        if c.events.live() { "MSI" } else { "INTx (polling until routed)" }
    );
    c.bus.call_once(|| super::register(c));
    crate::sched::spawn_task("ehci-events", event_task_entry, arg as u64, 0, crate::mm::vmm::kernel_pml4());
}

fn event_task_entry(arg: u64) {
    unsafe { &*(arg as *const Ehci) }.event_task();
}

impl Ehci {
    fn portsc(&self, port: u8) -> u32 {
        r32(self.op + PORTSC + (port as u64 - 1) * 4)
    }

    fn set_portsc(&self, port: u8, v: u32) {
        w32(self.op + PORTSC + (port as u64 - 1) * 4, v);
    }

    /// Programs the (reset) controller and runs it; takes every port.
    fn start(&self, port_power: bool) -> bool {
        let op = self.op;
        w32(op + USBINTR, 0);
        w32(op + CTRLDSSEGMENT, 0);
        w32(op + PERIODICLISTBASE, self.frame_list.phys as u32);
        w32(op + ASYNCLISTADDR, self.async_head.phys);
        w32(op + FRINDEX, 0);
        w32(op + USBSTS, STS_ACK);
        w32(op + USBCMD, CMD_RUN | CMD_PERIODIC | CMD_ASYNC | CMD_ITC_1);
        if crate::drivers::block::wait_for(20, || r32(op + USBSTS) & STS_HALTED == 0).is_err() {
            return false;
        }
        // Route every port to EHCI (companions get slow devices back below).
        w32(op + CONFIGFLAG, 1);
        crate::sched::sleep_ms(5);
        if port_power {
            for p in 1..=self.ports {
                let sc = self.portsc(p);
                self.set_portsc(p, (sc & !PORT_RWC) | PORT_POWER);
            }
            crate::sched::sleep_ms(20);
        }
        w32(op + USBINTR, STS_INT | STS_ERR | STS_PORT | STS_HSE | STS_IAA);
        true
    }

    fn event_task(&'static self) {
        loop {
            self.events.idle(4, || self.status.load(Ordering::Acquire) != 0);
            if self.events.live() {
                // Whatever an interrupt that went missing would have said.
                interrupt(self as *const Ehci as usize);
            }
            let mut sts = self.status.swap(0, Ordering::AcqRel);
            if !self.events.live() {
                let polled = r32(self.op + USBSTS) & STS_ACK;
                w32(self.op + USBSTS, polled & !STS_IAA);
                sts |= polled & !STS_IAA;
            }
            if sts & STS_PORT != 0 {
                if let Some(bus) = self.bus.get() {
                    bus.poke();
                }
            }
            if sts & STS_HSE != 0 {
                log!("ehci", "controller {}: host system error", self.index);
            }
            self.poll_pipes();
        }
    }

    // ----------------------------------------------------------- queue heads

    /// A queue head for endpoint `number` of the device at `addr`, with its
    /// first dummy qTD.
    fn new_qh(
        &self,
        addr: u8,
        pos: &Position,
        number: u8,
        max_packet: u16,
        control: bool,
        periodic: bool,
    ) -> Option<(Block, Block)> {
        let qh = self.pool.alloc()?;
        let Some(dummy) = self.pool.alloc() else {
            self.pool.free(qh);
            return None;
        };
        dummy.write(TD_NEXT, T);
        dummy.write(TD_ALT, T);
        let eps = match pos.speed {
            Speed::Full => 0,
            Speed::Low => 1,
            _ => 2,
        };
        let slow = pos.speed != Speed::High;
        let info1 = addr as u32
            | (number as u32) << 8
            | eps << 12
            | (control as u32) << 14 // control: toggles come from the qTDs
            | (max_packet as u32 & 0x7FF) << 16
            | ((control && slow) as u32) << 27;
        let mut info2 = 1 << 30;
        if let (true, Some(tt)) = (slow, pos.tt) {
            info2 |= (tt.hub as u32) << 16 | (tt.port as u32) << 23;
        }
        if periodic {
            // Poll in microframe 0; slow devices: start split there, complete splits in 2–4.
            info2 |= if slow { 0x1C << 8 | 0x01 } else { 0x01 };
        }
        qh.write(QH_INFO1, info1);
        qh.write(QH_INFO2, info2);
        qh.write(QH_NEXT, dummy.phys);
        qh.write(QH_ALT, T);
        Some((qh, dummy))
    }

    fn link_async(&self, qh: Block) {
        let mut ring = self.async_ring.lock();
        qh.write(QH_LINK, self.async_head.read(QH_LINK));
        fence(Ordering::SeqCst);
        self.async_head.write(QH_LINK, qh.phys | TYPE_QH);
        ring.insert(0, qh);
    }

    /// Takes a QH out of the async ring and waits until the controller has
    /// forgotten it (the "interrupt on async advance" handshake).
    fn unlink_async(&self, qh: Block) {
        let mut ring = self.async_ring.lock();
        let Some(i) = ring.iter().position(|q| *q == qh) else { return };
        let prev = if i == 0 { self.async_head } else { ring[i - 1] };
        prev.write(QH_LINK, qh.read(QH_LINK));
        ring.remove(i);
        fence(Ordering::SeqCst);
        self.async_advance();
    }

    fn async_advance(&self) {
        if r32(self.op + USBSTS) & STS_HALTED != 0 {
            return;
        }
        self.status.fetch_and(!STS_IAA, Ordering::AcqRel);
        w32(self.op + USBCMD, r32(self.op + USBCMD) | CMD_IAA_DOORBELL);
        let seen = self.events.wait(100, || {
            if self.status.load(Ordering::Acquire) & STS_IAA != 0 {
                return true;
            }
            let sts = r32(self.op + USBSTS);
            if sts & STS_IAA != 0 {
                w32(self.op + USBSTS, STS_IAA);
                return true;
            }
            false
        });
        if !seen {
            crate::sched::sleep_ms(2);
        }
    }

    fn link_periodic(&self, qh: Block) {
        let mut chain = self.periodic_chain.lock();
        qh.write(QH_LINK, self.periodic_head.read(QH_LINK));
        fence(Ordering::SeqCst);
        self.periodic_head.write(QH_LINK, qh.phys | TYPE_QH);
        chain.insert(0, qh);
    }

    fn unlink_periodic(&self, qh: Block) {
        let mut chain = self.periodic_chain.lock();
        let Some(i) = chain.iter().position(|q| *q == qh) else { return };
        let prev = if i == 0 { self.periodic_head } else { chain[i - 1] };
        prev.write(QH_LINK, qh.read(QH_LINK));
        chain.remove(i);
        drop(chain);
        // The controller may hold it for the rest of this frame.
        crate::sched::sleep_ms(2);
    }

    /// Changes an async QH's characteristics (address, packet size) safely: out
    /// of the schedule, rewritten, back in.
    fn requalify(&self, qh: Block, f: impl Fn(u32) -> u32) {
        self.unlink_async(qh);
        qh.write(QH_INFO1, f(qh.read(QH_INFO1)));
        self.link_async(qh);
    }

    // -------------------------------------------------------------- transfers

    /// Fills a qTD for `len` bytes at `phys` (at most [`TD_BYTES`]), leading
    /// to `next` (also after a short packet). It stays inactive; returns the
    /// token that [`go`] makes it active with.
    fn fill_td(td: Block, pid: u32, phys: u64, len: usize, toggle: Option<bool>, ioc: bool, next: Block) -> u32 {
        td.write(TD_TOKEN, 0);
        td.write(TD_NEXT, next.phys);
        td.write(TD_ALT, next.phys);
        let page = phys & !0xFFF;
        for i in 0..5 {
            let p = if i == 0 { phys } else { page + i as u64 * 4096 };
            td.write(TD_BUF + i, p as u32);
        }
        let toggle = match toggle {
            Some(true) => TOK_TOGGLE,
            _ => 0,
        };
        // The upper halves of the buffer pointers (64-bit controllers).
        for i in 8..13 {
            td.write(i, 0);
        }
        TOK_CERR | pid | (len as u32) << 16 | if ioc { TOK_IOC } else { 0 } | toggle
    }

    /// Makes a filled qTD active (the controller may start on it at once).
    fn go(td: Block, token: u32) {
        fence(Ordering::SeqCst);
        td.write(TD_TOKEN, token | TOK_ACTIVE);
    }

    /// Runs a chain of filled qTDs on `ep` (the first is the endpoint's old
    /// dummy; `new_dummy` ends the chain) and waits. `data` lists the qTDs
    /// that move data (their lengths), for the count. Returns the bytes moved.
    fn run(
        &self,
        ep: &Endp,
        tds: &[(Block, u32)],
        new_dummy: Block,
        data: &[(usize, usize)],
        timeout_ms: u64,
    ) -> Result<usize, u8> {
        // The first qTD last: the controller may be looking at it already.
        for &(td, token) in tds[1..].iter().chain(&tds[..1]) {
            Self::go(td, token);
        }
        let tds: Vec<Block> = tds.iter().map(|t| t.0).collect();
        let state = || -> Option<Result<(), u8>> {
            for td in &tds {
                let t = td.read(TD_TOKEN);
                if t & TOK_ACTIVE != 0 {
                    return None;
                }
                if t & TOK_HALTED != 0 {
                    return Some(Err(if t & TOK_ERRORS != 0 { ERROR } else { STALL }));
                }
            }
            Some(Ok(()))
        };
        self.events.wait(timeout_ms, || state().is_some());
        let result = state().unwrap_or(Err(TIMEOUT));
        if result == Err(TIMEOUT) {
            let tokens: Vec<u32> = tds.iter().map(|t| t.read(TD_TOKEN)).collect();
            log!(
                "ehci",
                "transfer timed out: QH {:#x} {:#x} overlay next {:#x} alt {:#x} token {:#x}, qTDs {:x?} at {:x?}, USBSTS {:#x}",
                ep.qh.read(QH_INFO1),
                ep.qh.read(QH_INFO2),
                ep.qh.read(QH_NEXT),
                ep.qh.read(QH_ALT),
                ep.qh.read(QH_TOKEN),
                tokens,
                tds.iter().map(|t| t.phys).collect::<Vec<_>>(),
                r32(self.op + USBSTS)
            );
        }
        if result.is_err() {
            // The queue stopped (halted) or never finished: point it at the
            // new dummy, out of the schedule if it may still be running.
            let halted = result != Err(TIMEOUT);
            if !halted {
                if ep.periodic {
                    self.unlink_periodic(ep.qh);
                } else {
                    self.unlink_async(ep.qh);
                }
            }
            ep.qh.write(QH_NEXT, new_dummy.phys);
            ep.qh.write(QH_ALT, T);
            fence(Ordering::SeqCst);
            ep.qh.write(QH_TOKEN, ep.qh.read(QH_TOKEN) & TOK_TOGGLE);
            if !halted {
                if ep.periodic {
                    self.link_periodic(ep.qh);
                } else {
                    self.link_async(ep.qh);
                }
            }
        }
        let mut moved = 0;
        for &(i, len) in data {
            let t = tds[i].read(TD_TOKEN);
            if t & TOK_ACTIVE == 0 {
                moved += len - ((t >> 16) & 0x7FFF) as usize;
            }
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

    fn free_tds(&self, tds: &[Block]) {
        for &td in tds {
            self.pool.free(td);
        }
    }

    // ---------------------------------------------------------------- pipes

    fn arm_pipe(p: &Pipe) {
        let token = Self::fill_td(p.td, PID_IN, p.buf.phys, p.size, None, true, p.dummy);
        p.dummy.write(TD_TOKEN, 0);
        p.dummy.write(TD_NEXT, T);
        p.dummy.write(TD_ALT, T);
        Self::go(p.td, token);
    }

    fn poll_pipes(&self) {
        let mut ready = Vec::new();
        {
            let mut pipes = self.pipes.lock();
            for p in pipes.iter_mut() {
                let t = p.td.read(TD_TOKEN);
                if t & TOK_ACTIVE != 0 {
                    continue;
                }
                if t & TOK_HALTED == 0 {
                    p.errors = 0;
                    let n = p.size.saturating_sub(((t >> 16) & 0x7FFF) as usize);
                    ready.push((p.handler.clone(), p.buf.bytes()[..n].to_vec()));
                } else {
                    p.errors += 1;
                    if p.errors >= 100 {
                        continue;
                    }
                }
                // The queue has moved on to the dummy: it carries the next poll,
                // and the finished qTD becomes the dummy.
                core::mem::swap(&mut p.td, &mut p.dummy);
                Self::arm_pipe(p);
                if t & TOK_HALTED != 0 {
                    // Halted on the old qTD: un-halt onto the new one.
                    p.qh.write(QH_NEXT, p.td.phys);
                    fence(Ordering::SeqCst);
                    p.qh.write(QH_TOKEN, p.qh.read(QH_TOKEN) & TOK_TOGGLE);
                }
            }
        }
        for (h, data) in ready {
            h.data(&data);
        }
    }
}

impl Hcd for Ehci {
    fn kind(&self) -> &'static str {
        "EHCI"
    }

    fn ports(&self) -> u8 {
        self.ports
    }

    fn changed_ports(&self) -> Vec<u8> {
        (1..=self.ports).filter(|&p| self.portsc(p) & PORT_CSC != 0).collect()
    }

    fn port_connected(&self, port: u8) -> bool {
        let sc = self.portsc(port);
        self.set_portsc(port, (sc & !PORT_RWC) | (sc & PORT_RWC));
        sc & PORT_CCS != 0 && sc & PORT_OWNER == 0
    }

    fn port_reset(&self, port: u8) -> Option<Speed> {
        let sc = self.portsc(port);
        if sc & PORT_OWNER != 0 {
            return None;
        }
        let hand_off = |why: &str| {
            if self.companions == 0 {
                log!("usb", "EHCI port {}: {} device and no companion controller", port, why);
            } else {
                let sc = self.portsc(port);
                self.set_portsc(port, (sc & !PORT_RWC) | PORT_OWNER);
            }
            None
        };
        // Line state K: a low-speed device.
        if (sc >> 10) & 3 == 1 {
            return hand_off("low-speed");
        }
        self.set_portsc(port, (sc & !(PORT_RWC | PORT_PE)) | PORT_RESET);
        crate::sched::sleep_ms(50);
        let sc = self.portsc(port);
        self.set_portsc(port, sc & !(PORT_RWC | PORT_RESET));
        if crate::drivers::block::wait_for(10, || self.portsc(port) & PORT_RESET == 0).is_err() {
            log!("usb", "EHCI port {}: reset did not finish", port);
            return None;
        }
        crate::sched::sleep_ms(2);
        if self.portsc(port) & PORT_PE != 0 {
            Some(Speed::High)
        } else {
            // Not enabled after reset: a full-speed device.
            hand_off("full-speed")
        }
    }

    fn new_device(&self, pos: &Position) -> Option<u8> {
        let addr = self.addresses.alloc()?;
        let mps = pos.speed.default_max_packet();
        let Some((qh, dummy)) = self.new_qh(0, pos, 0, mps, true, false) else {
            self.addresses.free(addr);
            return None;
        };
        self.link_async(qh);
        let ep0 = Arc::new(Endp { qh, periodic: false, dummy: Mutex::new(dummy) });
        self.devices.lock().insert(addr, Device { pos: *pos, ep0, eps: BTreeMap::new() });
        let set_address = Setup { request_type: 0, request: 5, value: addr as u16, index: 0, length: 0 };
        if self.control(addr, set_address, None).is_err() {
            log!("usb", "EHCI port {}: SET_ADDRESS failed", pos.root_port);
            self.release(addr);
            return None;
        }
        crate::sched::sleep_ms(2);
        self.requalify(qh, |info| (info & !0x7F) | addr as u32);
        Some(addr)
    }

    fn set_ep0_max_packet(&self, dev: u8, max_packet: u16) {
        if let Some(ep) = self.endpoint(dev, None) {
            self.requalify(ep.qh, |info| (info & !(0x7FF << 16)) | (max_packet as u32 & 0x7FF) << 16);
        }
    }

    fn control(&self, dev: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        let ep = self.endpoint(dev, None).ok_or(TIMEOUT)?;
        let mut dummy = ep.dummy.lock();
        let len = setup.length as usize;
        let mut buf = Dma::new32(8 + len.max(1)).ok_or(TIMEOUT)?;
        buf.bytes_mut()[..8].copy_from_slice(&setup.bytes().to_le_bytes());
        if let (Some(d), false) = (&data, setup.is_in()) {
            buf.bytes_mut()[8..8 + len].copy_from_slice(&d[..len]);
        }
        // Setup, data (if any) and status: the old dummy and new qTDs, then a new dummy.
        let count = if len > 0 { 3 } else { 2 };
        let mut tds = alloc::vec![*dummy];
        for _ in 0..count {
            match self.pool.alloc() {
                Some(td) => tds.push(td),
                None => {
                    self.free_tds(&tds[1..]);
                    return Err(TIMEOUT);
                }
            }
        }
        let new_dummy = tds.pop().unwrap();
        new_dummy.write(TD_NEXT, T);
        new_dummy.write(TD_ALT, T);
        let status = *tds.last().unwrap();
        let mut queued =
            alloc::vec![(tds[0], Self::fill_td(tds[0], PID_SETUP, buf.phys, 8, Some(false), false, tds[1]))];
        let data_pid = if setup.is_in() { PID_IN } else { PID_OUT };
        if len > 0 {
            // A short IN data stage goes on to the status stage either way.
            queued.push((tds[1], Self::fill_td(tds[1], data_pid, buf.phys + 8, len, Some(true), false, status)));
        }
        // The status stage runs opposite to the data (IN when there is none).
        let status_pid = if len == 0 || !setup.is_in() { PID_IN } else { PID_OUT };
        queued.push((status, Self::fill_td(status, status_pid, 0, 0, Some(true), true, new_dummy)));
        let counted: &[(usize, usize)] = if len > 0 { &[(1, len)] } else { &[] };
        let r = self.run(&ep, &queued, new_dummy, counted, 2000);
        *dummy = new_dummy;
        self.free_tds(&tds);
        let n = r?;
        if let (Some(d), true) = (data, setup.is_in()) {
            d[..n].copy_from_slice(&buf.bytes()[8..8 + n]);
        }
        Ok(n)
    }

    /// One qTD at a time (up to 16 KiB): a short packet then simply ends the
    /// transfer, with nothing left queued behind it.
    fn transfer(&self, dev: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8> {
        let endp = self.endpoint(dev, Some(ep.address)).ok_or(TIMEOUT)?;
        let mut dummy = endp.dummy.lock();
        let pid = if ep.is_in() { PID_IN } else { PID_OUT };
        let mut moved = 0;
        loop {
            let n = (len - moved).min(TD_BYTES);
            let new_dummy = self.pool.alloc().ok_or(TIMEOUT)?;
            new_dummy.write(TD_NEXT, T);
            new_dummy.write(TD_ALT, T);
            let td = *dummy;
            let token = Self::fill_td(td, pid, phys + moved as u64, n, None, true, new_dummy);
            let r = self.run(&endp, &[(td, token)], new_dummy, &[(0, n)], timeout_ms);
            *dummy = new_dummy;
            self.pool.free(td);
            let got = r?;
            moved += got;
            if got < n || moved >= len {
                return Ok(moved);
            }
        }
    }

    fn configure(&self, dev: u8, eps: &[Endpoint], _hub: Option<HubInfo>) -> bool {
        let Some(pos) = self.devices.lock().get(&dev).map(|d| d.pos) else { return false };
        for e in eps.iter().filter(|e| e.kind == 2 || (e.kind == 3 && !e.is_in())) {
            if self.endpoint(dev, Some(e.address)).is_some() {
                continue;
            }
            let periodic = e.kind == 3;
            let Some((qh, dummy)) = self.new_qh(dev, &pos, e.number(), e.max_packet, false, periodic) else {
                return false;
            };
            if periodic {
                self.link_periodic(qh);
            } else {
                self.link_async(qh);
            }
            let endp = Arc::new(Endp { qh, periodic, dummy: Mutex::new(dummy) });
            if let Some(d) = self.devices.lock().get_mut(&dev) {
                d.eps.insert(e.address, endp);
            }
        }
        true
    }

    fn open_pipe(&self, dev: u8, ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool {
        let Some(pos) = self.devices.lock().get(&dev).map(|d| d.pos) else { return false };
        let size = (ep.max_packet as usize).clamp(8, 1024);
        let (Some((qh, td)), Some(dummy), Some(buf)) =
            (self.new_qh(dev, &pos, ep.number(), ep.max_packet, false, true), self.pool.alloc(), Dma::new32(size))
        else {
            return false;
        };
        let pipe = Pipe { dev, qh, td, dummy, buf, size, handler, errors: 0 };
        Self::arm_pipe(&pipe);
        self.link_periodic(qh);
        self.pipes.lock().push(pipe);
        true
    }

    fn reset_toggle(&self, dev: u8, ep_address: u8) {
        if let Some(ep) = self.endpoint(dev, Some(ep_address)) {
            let _queue = ep.dummy.lock();
            ep.qh.write(QH_TOKEN, ep.qh.read(QH_TOKEN) & !TOK_TOGGLE);
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
            self.unlink_periodic(p.qh);
            self.free_tds(&[p.qh, p.td, p.dummy]);
        }
        if let Some(d) = self.devices.lock().remove(&dev) {
            for ep in core::iter::once(&d.ep0).chain(d.eps.values()) {
                let dummy = ep.dummy.lock();
                if ep.periodic {
                    self.unlink_periodic(ep.qh);
                } else {
                    self.unlink_async(ep.qh);
                }
                self.free_tds(&[ep.qh, *dummy]);
            }
        }
        self.addresses.free(dev);
    }

    fn restart(&self) -> bool {
        let devs: Vec<u8> = self.devices.lock().keys().copied().collect();
        for d in devs {
            self.release(d);
        }
        let op = self.op;
        w32(op + USBCMD, r32(op + USBCMD) & !CMD_RUN);
        let _ = crate::drivers::block::wait_for(20, || r32(op + USBSTS) & STS_HALTED != 0);
        w32(op + USBCMD, CMD_RESET);
        if crate::drivers::block::wait_for(250, || r32(op + USBCMD) & CMD_RESET == 0).is_err() {
            return false;
        }
        self.async_head.write(QH_LINK, self.async_head.phys | TYPE_QH);
        self.periodic_head.write(QH_LINK, T);
        self.async_ring.lock().clear();
        self.periodic_chain.lock().clear();
        self.addresses.clear();
        self.start(true)
    }

    fn dma(&self, bytes: usize) -> Option<Dma> {
        Dma::new32(bytes)
    }

    fn interrupts(&self) -> u64 {
        self.events.count.load(Ordering::Relaxed)
    }
}
