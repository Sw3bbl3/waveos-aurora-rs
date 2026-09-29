//! xHCI host controller driver (USB 1.1, 2.0 and 3.x devices).
//!
//! The controller works from rings of 16-byte TRBs in memory: we queue
//! commands on the command ring and transfers on each endpoint's transfer
//! ring, ring a doorbell, and the controller reports completions and port
//! changes on the event ring (and raises an MSI). Device state lives in
//! contexts the controller owns (output) and that we fill in to change it
//! (input).

use super::{Configuration, DeviceDescriptor, Endpoint, Setup, Speed};
use crate::drivers::block::Dma;
use crate::drivers::pci;
use crate::sync::{IrqMutex, Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicBool, AtomicU64, Ordering};

// Capability registers.
const CAPLENGTH: u64 = 0x00;
const HCSPARAMS1: u64 = 0x04;
const HCSPARAMS2: u64 = 0x08;
const HCCPARAMS1: u64 = 0x10;
const DBOFF: u64 = 0x14;
const RTSOFF: u64 = 0x18;

// Operational registers.
const USBCMD: u64 = 0x00;
const USBSTS: u64 = 0x04;
const CRCR: u64 = 0x18;
const DCBAAP: u64 = 0x30;
const CONFIG: u64 = 0x38;
const PORTSC: u64 = 0x400;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_INTE: u32 = 1 << 2;
const STS_HALTED: u32 = 1 << 0;
const STS_EINT: u32 = 1 << 3;
const STS_NOT_READY: u32 = 1 << 11;

// Port status and control.
const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_RESET: u32 = 1 << 4;
const PORT_POWER: u32 = 1 << 9;
const PORT_CHANGES: u32 = 0x00FE_0000;
const PORT_PRC: u32 = 1 << 21;
/// Bits written back unchanged (read-only and read-write-preserve).
const PORT_KEEP: u32 = 0x01 | 0x08 | 0x3C00 | 1 << 24 | 1 << 30 | 0x200 | 0xC000 | 0x0E00_0000;

// Interrupter 0 (runtime registers + 0x20).
const IMAN: u64 = 0x20;
const IMOD: u64 = 0x24;
const ERSTSZ: u64 = 0x28;
const ERSTBA: u64 = 0x30;
const ERDP: u64 = 0x38;

// TRB types.
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_SET_DEQUEUE: u32 = 16;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_COMMAND_DONE: u32 = 33;
const TRB_PORT_CHANGE: u32 = 34;

const TRB_CYCLE: u32 = 1 << 0;
const TRB_ISP: u32 = 1 << 2;
const TRB_CHAIN: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;

pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT: u8 = 13;
/// Our own code for "no completion arrived in time".
pub const CC_TIMEOUT: u8 = 0xFF;

const RING_TRBS: usize = 256;
const EVENT_TRBS: usize = 256;

/// A producer ring (command or transfer), ending in a link TRB back to its start.
struct Ring {
    dma: Dma,
    enqueue: usize,
    cycle: bool,
}

impl Ring {
    fn new() -> Option<Ring> {
        let dma = Dma::new(RING_TRBS * 16)?;
        let link = (RING_TRBS - 1) * 16;
        dma.write::<u64>(link, dma.phys);
        dma.write::<u32>(link + 12, TRB_LINK << 10 | 1 << 1); // toggle cycle
        Some(Ring { dma, enqueue: 0, cycle: true })
    }

    fn phys(&self) -> u64 {
        self.dma.phys
    }

    /// Queues a TRB; returns its physical address.
    fn push(&mut self, param: u64, status: u32, control: u32) -> u64 {
        let off = self.enqueue * 16;
        self.dma.write::<u64>(off, param);
        self.dma.write::<u32>(off + 8, status);
        fence(Ordering::SeqCst);
        self.dma.write::<u32>(off + 12, control | self.cycle as u32);
        let phys = self.dma.phys + off as u64;
        self.enqueue += 1;
        if self.enqueue == RING_TRBS - 1 {
            // Hand the link TRB over and wrap.
            let link = (RING_TRBS - 1) * 16;
            let ctl = self.dma.read::<u32>(link + 12) & !TRB_CYCLE;
            self.dma.write::<u32>(link + 12, ctl | self.cycle as u32 | (control & TRB_CHAIN));
            self.enqueue = 0;
            self.cycle = !self.cycle;
        }
        phys
    }

    /// Where the controller should resume after an error.
    fn dequeue_pointer(&self) -> u64 {
        self.dma.phys + (self.enqueue * 16) as u64 | self.cycle as u64
    }
}

struct EventRing {
    segment: Dma,
    _table: Dma,
    dequeue: usize,
    cycle: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Completion {
    pub code: u8,
    pub residual: u32,
    pub slot: u8,
}

/// A driver fed by an interrupt IN endpoint (keyboard reports, hub changes).
pub trait PipeHandler: Send + Sync {
    fn data(&self, data: &[u8]);
    /// Periodic work (key repeat); returns ms until it wants to run again.
    fn tick(&self, _now_ms: u64) -> Option<u64> {
        None
    }
}

struct Pipe {
    handler: Arc<dyn PipeHandler>,
    buf: Dma,
    size: usize,
    /// Queued TRB → its slice of `buf`.
    trbs: BTreeMap<u64, usize>,
}

struct SlotState {
    output: Dma,
    input: Dma,
    rings: [Option<Ring>; 32],
}

/// What we know about an attached device (for listing and teardown).
#[derive(Clone)]
pub struct DeviceRecord {
    pub slot: u8,
    pub root_port: u8,
    pub route: u32,
    pub speed: Speed,
    pub desc: DeviceDescriptor,
    pub name: String,
    pub driver: String,
}

struct Inner {
    cmd: Ring,
    dcbaa: Dma,
    slots: BTreeMap<u8, SlotState>,
}

pub struct Controller {
    pub index: usize,
    op: u64,
    rt: u64,
    db: u64,
    ctx_size: usize,
    max_ports: u8,
    /// USB major revision of each root port (from the supported-protocol capabilities).
    port_major: [u8; 256],
    inner: Mutex<Inner>,
    events: spin::Mutex<EventRing>,
    completions: IrqMutex<BTreeMap<u64, Completion>>,
    done: WaitQueue,
    irq: WaitQueue,
    has_irq: AtomicBool,
    pipes: IrqMutex<BTreeMap<(u8, u8), Pipe>>,
    port_events: IrqMutex<VecDeque<u8>>,
    hub_events: IrqMutex<VecDeque<(u8, u32)>>,
    enum_wake: WaitQueue,
    devices: IrqMutex<Vec<DeviceRecord>>,
    /// Hooks run when a device goes away (storage unmounts, …).
    detach: IrqMutex<Vec<(u8, Box<dyn Fn() + Send + Sync>)>>,
    pub interrupts: AtomicU64,
    /// Set once the devices present at boot are set up: later ones are announced.
    settled: AtomicBool,
}

static CONTROLLERS: IrqMutex<Vec<&'static Controller>> = IrqMutex::new(Vec::new());

pub fn controllers() -> Vec<&'static Controller> {
    CONTROLLERS.lock().clone()
}

fn r32(a: u64) -> u32 {
    unsafe { (a as *const u32).read_volatile() }
}
fn w32(a: u64, v: u32) {
    unsafe { (a as *mut u32).write_volatile(v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

fn wait_until(ms: u64, cond: impl Fn() -> bool) -> bool {
    crate::drivers::block::wait_for(ms, cond).is_ok()
}

fn interrupt(arg: usize) {
    let c = unsafe { &*(arg as *const Controller) };
    // Acknowledge (IP is write-1-to-clear; keep IE set), then let the event task run.
    w32(c.rt + IMAN, 0b11);
    w32(c.op + USBSTS, STS_EINT);
    c.interrupts.fetch_add(1, Ordering::Relaxed);
    c.irq.wake_all();
}

/// Takes the controller from the firmware (USB legacy support) and reads the
/// supported-protocol capabilities.
fn extended_capabilities(cap: u64, port_major: &mut [u8; 256]) {
    let mut off = ((r32(cap + HCCPARAMS1) >> 16) as u64) << 2;
    let mut guard = 0;
    while off != 0 && guard < 64 {
        let addr = cap + off;
        let head = r32(addr);
        match head & 0xFF {
            1 => {
                // USBLEGSUP: request ownership, wait for the BIOS to let go.
                w32(addr, head | 1 << 24);
                if !wait_until(1000, || r32(addr) & (1 << 16) == 0) {
                    log!("xhci", "firmware did not release the controller; taking it anyway");
                    w32(addr, r32(addr) & !(1 << 16));
                }
                // No more SMIs for USB events.
                w32(addr + 4, 0xE000_0000);
            }
            2 => {
                let major = (head >> 24) as u8;
                let ports = r32(addr + 8);
                let (first, count) = ((ports & 0xFF) as usize, ((ports >> 8) & 0xFF) as usize);
                for p in first..(first + count).min(256) {
                    port_major[p] = major;
                }
            }
            _ => {}
        }
        let next = ((head >> 8) & 0xFF) as u64;
        if next == 0 {
            break;
        }
        off += next << 2;
        guard += 1;
    }
}

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(cap) = dev.map_bar(0) else { return };
    let op = cap + (r32(cap + CAPLENGTH) & 0xFF) as u64;
    let rt = cap + (r32(cap + RTSOFF) & !0x1F) as u64;
    let db = cap + (r32(cap + DBOFF) & !0x3) as u64;
    let hcs1 = r32(cap + HCSPARAMS1);
    let hcs2 = r32(cap + HCSPARAMS2);
    let max_slots = (hcs1 & 0xFF) as u8;
    let max_ports = (hcs1 >> 24) as u8;
    let ctx_size = if r32(cap + HCCPARAMS1) & (1 << 2) != 0 { 64 } else { 32 };
    let mut port_major = [0u8; 256];
    extended_capabilities(cap, &mut port_major);

    // Stop and reset.
    w32(op + USBCMD, r32(op + USBCMD) & !CMD_RUN);
    if !wait_until(100, || r32(op + USBSTS) & STS_HALTED != 0) {
        log!("xhci", "controller did not halt");
    }
    w32(op + USBCMD, CMD_RESET);
    if !wait_until(1000, || r32(op + USBCMD) & CMD_RESET == 0 && r32(op + USBSTS) & STS_NOT_READY == 0) {
        log!("xhci", "controller reset timed out");
        return;
    }

    w32(op + CONFIG, max_slots as u32);
    let Some(dcbaa) = Dma::new((max_slots as usize + 1) * 8) else { return };
    // Scratchpad buffers the controller may ask for.
    let scratch = ((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27) & 0x1F;
    if scratch > 0 {
        let Some(array) = Dma::new(scratch as usize * 8) else { return };
        for i in 0..scratch as usize {
            let Some(page) = Dma::new(4096) else { return };
            array.write::<u64>(i * 8, page.phys);
            core::mem::forget(page);
        }
        dcbaa.write::<u64>(0, array.phys);
        core::mem::forget(array);
    }
    w64(op + DCBAAP, dcbaa.phys);
    let Some(cmd) = Ring::new() else { return };
    w64(op + CRCR, cmd.phys() | 1);

    let (Some(segment), Some(table)) = (Dma::new(EVENT_TRBS * 16), Dma::new(64)) else { return };
    table.write::<u64>(0, segment.phys);
    table.write::<u32>(8, EVENT_TRBS as u32);
    w32(rt + ERSTSZ, 1);
    w64(rt + ERDP, segment.phys);
    w64(rt + ERSTBA, table.phys);

    let c: &'static Controller = Box::leak(Box::new(Controller {
        index: CONTROLLERS.lock().len(),
        op,
        rt,
        db,
        ctx_size,
        max_ports,
        port_major,
        inner: Mutex::new(Inner { cmd, dcbaa, slots: BTreeMap::new() }),
        events: spin::Mutex::new(EventRing { segment, _table: table, dequeue: 0, cycle: true }),
        completions: IrqMutex::new(BTreeMap::new()),
        done: WaitQueue::new(),
        irq: WaitQueue::new(),
        has_irq: AtomicBool::new(false),
        pipes: IrqMutex::new(BTreeMap::new()),
        port_events: IrqMutex::new(VecDeque::new()),
        hub_events: IrqMutex::new(VecDeque::new()),
        enum_wake: WaitQueue::new(),
        devices: IrqMutex::new(Vec::new()),
        detach: IrqMutex::new(Vec::new()),
        interrupts: AtomicU64::new(0),
        settled: AtomicBool::new(false),
    }));
    if dev.enable_msi("xhci", interrupt, c as *const Controller as usize).is_some() {
        c.has_irq.store(true, Ordering::Relaxed);
    }
    w32(rt + IMOD, 1000); // at most one interrupt per 250 µs
    w32(rt + IMAN, 0b11);
    w32(op + USBCMD, CMD_RUN | CMD_INTE);
    if !wait_until(100, || r32(op + USBSTS) & STS_HALTED == 0) {
        log!("xhci", "controller did not start");
        return;
    }
    log!(
        "xhci",
        "controller {}: {} ports, {} slots, {}-byte contexts, {} scratchpad pages, {}",
        c.index,
        max_ports,
        max_slots,
        ctx_size,
        scratch,
        if c.has_irq.load(Ordering::Relaxed) { "MSI" } else { "polling" }
    );
    CONTROLLERS.lock().push(c);
    // Power every port, then look at what is already plugged in.
    for p in 1..=max_ports {
        let sc = c.portsc(p);
        if sc & PORT_POWER == 0 {
            c.set_portsc(p, (sc & PORT_KEEP) | PORT_POWER);
        }
        c.port_events.lock().push_back(p);
    }
    let arg = c as *const Controller as u64;
    let kernel = crate::mm::vmm::kernel_pml4();
    crate::sched::spawn_task("xhci-events", event_task_entry, arg, 0, kernel);
    crate::sched::spawn_task("usb", enum_task_entry, arg, 0, kernel);
}

fn event_task_entry(arg: u64) {
    unsafe { &*(arg as *const Controller) }.event_task();
}

fn enum_task_entry(arg: u64) {
    unsafe { &*(arg as *const Controller) }.enum_task();
}

impl Controller {
    fn portsc(&self, port: u8) -> u32 {
        r32(self.op + PORTSC + (port as u64 - 1) * 0x10)
    }

    fn set_portsc(&self, port: u8, v: u32) {
        w32(self.op + PORTSC + (port as u64 - 1) * 0x10, v);
    }

    fn doorbell(&self, slot: u8, target: u8) {
        fence(Ordering::SeqCst);
        w32(self.db + slot as u64 * 4, target as u32);
    }

    // ---------------------------------------------------------------- events

    fn event_task(&'static self) {
        loop {
            let mut next = 1000;
            let now = crate::time::uptime_ms();
            let handlers: Vec<Arc<dyn PipeHandler>> = self.pipes.lock().values().map(|p| p.handler.clone()).collect();
            for h in handlers {
                if let Some(ms) = h.tick(now) {
                    next = next.min(ms.max(1));
                }
            }
            if self.has_irq.load(Ordering::Relaxed) {
                self.irq.wait(next, || self.event_pending());
            } else {
                crate::sched::sleep_ms(next.min(8));
            }
            self.drain_events();
        }
    }

    fn event_pending(&self) -> bool {
        let ev = self.events.lock();
        let ctl = ev.segment.read::<u32>(ev.dequeue * 16 + 12);
        (ctl & TRB_CYCLE != 0) == ev.cycle
    }

    fn drain_events(&self) {
        let mut any = false;
        loop {
            let trb = {
                let mut ev = self.events.lock();
                let off = ev.dequeue * 16;
                let control = ev.segment.read::<u32>(off + 12);
                if (control & TRB_CYCLE != 0) != ev.cycle {
                    break;
                }
                let t = (ev.segment.read::<u64>(off), ev.segment.read::<u32>(off + 8), control);
                ev.dequeue += 1;
                if ev.dequeue == EVENT_TRBS {
                    ev.dequeue = 0;
                    ev.cycle = !ev.cycle;
                }
                t
            };
            any = true;
            self.handle_event(trb.0, trb.1, trb.2);
        }
        if any {
            let ev = self.events.lock();
            w64(self.rt + ERDP, ev.segment.phys + (ev.dequeue * 16) as u64 | 1 << 3);
            drop(ev);
            self.done.wake_all();
        }
    }

    fn handle_event(&self, param: u64, status: u32, control: u32) {
        let code = (status >> 24) as u8;
        let slot = (control >> 24) as u8;
        match (control >> 10) & 0x3F {
            TRB_COMMAND_DONE => {
                self.completions.lock().insert(param, Completion { code, residual: 0, slot });
            }
            TRB_TRANSFER_EVENT => {
                let ep = ((control >> 16) & 0x1F) as u8;
                let residual = status & 0xFF_FFFF;
                if self.pipes.lock().contains_key(&(slot, ep)) {
                    self.pipe_event(slot, ep, param, code, residual);
                } else {
                    self.completions.lock().insert(param, Completion { code, residual, slot });
                }
            }
            TRB_PORT_CHANGE => {
                self.port_events.lock().push_back((param >> 24) as u8);
                self.enum_wake.wake_all();
            }
            _ => {}
        }
    }

    fn pipe_event(&self, slot: u8, ep: u8, trb: u64, code: u8, residual: u32) {
        let (handler, data) = {
            let mut pipes = self.pipes.lock();
            let Some(p) = pipes.get_mut(&(slot, ep)) else { return };
            let Some(off) = p.trbs.remove(&trb) else { return };
            let n = p.size.saturating_sub(residual as usize);
            let data = p.buf.bytes_mut()[off..off + n].to_vec();
            (p.handler.clone(), (off, data))
        };
        if code == CC_SUCCESS || code == CC_SHORT {
            handler.data(&data.1);
        }
        if code == CC_STALL {
            self.recover(slot, ep);
        }
        // Queue the buffer again (unless the device has gone).
        self.queue_pipe_buffer(slot, ep, data.0);
    }

    fn queue_pipe_buffer(&self, slot: u8, ep: u8, off: usize) {
        let (phys, size) = match self.pipes.lock().get(&(slot, ep)) {
            Some(p) => (p.buf.phys + off as u64, p.size),
            None => return,
        };
        let trb = {
            let mut inner = self.inner.lock();
            let Some(ring) = inner.slots.get_mut(&slot).and_then(|s| s.rings[ep as usize].as_mut()) else { return };
            ring.push(phys, size as u32, TRB_NORMAL << 10 | TRB_IOC | TRB_ISP)
        };
        if let Some(p) = self.pipes.lock().get_mut(&(slot, ep)) {
            p.trbs.insert(trb, off);
        }
        self.doorbell(slot, ep);
    }

    /// Starts polling an interrupt IN endpoint: `handler` gets every report.
    pub fn open_pipe(&self, slot: u8, ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool {
        const BUFFERS: usize = 4;
        let size = (ep.max_packet as usize).clamp(8, 1024);
        let Some(buf) = Dma::new(size * BUFFERS) else { return false };
        self.pipes.lock().insert((slot, ep.dci()), Pipe { handler, buf, size, trbs: BTreeMap::new() });
        for i in 0..BUFFERS {
            self.queue_pipe_buffer(slot, ep.dci(), i * size);
        }
        true
    }

    /// Waits for the completion of any of `trbs`: the last one, or an earlier
    /// one that failed. Returns it with the residual of the first short one.
    fn wait_completion(&self, trbs: &[u64], timeout_ms: u64) -> Completion {
        let last = *trbs.last().unwrap();
        let finished = || {
            let c = self.completions.lock();
            c.contains_key(&last)
                || trbs.iter().any(|t| c.get(t).is_some_and(|e| e.code != CC_SUCCESS && e.code != CC_SHORT))
        };
        let ok = if self.has_irq.load(Ordering::Relaxed) {
            self.done.wait(timeout_ms, finished)
        } else {
            let end = crate::time::uptime_ms() + timeout_ms;
            loop {
                self.drain_events();
                if finished() || crate::time::uptime_ms() > end {
                    break finished();
                }
                crate::sched::yield_now();
            }
        };
        let mut c = self.completions.lock();
        let events: Vec<Completion> = trbs.iter().filter_map(|t| c.remove(t)).collect();
        drop(c);
        if !ok {
            return Completion { code: CC_TIMEOUT, residual: 0, slot: 0 };
        }
        let failed = events.iter().find(|e| e.code != CC_SUCCESS && e.code != CC_SHORT);
        let short = events.iter().find(|e| e.code == CC_SHORT);
        match (failed, short) {
            (Some(f), _) => *f,
            (None, Some(s)) => Completion { code: CC_SUCCESS, ..*s },
            _ => events.last().copied().unwrap_or(Completion { code: CC_SUCCESS, residual: 0, slot: 0 }),
        }
    }

    // --------------------------------------------------------------- commands

    fn command(&self, param: u64, control: u32) -> Completion {
        let trb = {
            let mut inner = self.inner.lock();
            inner.cmd.push(param, 0, control)
        };
        self.doorbell(0, 0);
        self.wait_completion(&[trb], 2000)
    }

    /// Clears a halted endpoint (after a STALL) so it can be used again.
    fn recover(&self, slot: u8, dci: u8) {
        self.command(0, TRB_RESET_EP << 10 | (dci as u32) << 16 | (slot as u32) << 24);
        let dequeue = {
            let inner = self.inner.lock();
            inner.slots.get(&slot).and_then(|s| s.rings[dci as usize].as_ref()).map(|r| r.dequeue_pointer())
        };
        if let Some(d) = dequeue {
            self.command(d, TRB_SET_DEQUEUE << 10 | (dci as u32) << 16 | (slot as u32) << 24);
        }
    }

    // -------------------------------------------------------------- transfers

    /// A control transfer on endpoint 0; returns the bytes transferred.
    pub fn control(&self, slot: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        let len = setup.length as usize;
        let buf = if len > 0 { Some(Dma::new(len).ok_or(CC_TIMEOUT)?) } else { None };
        if let (Some(b), Some(d), false) = (&buf, &data, setup.is_in()) {
            b.bytes_mut()[..len].copy_from_slice(&d[..len]);
        }
        let trbs = {
            let mut inner = self.inner.lock();
            let ring = inner.slots.get_mut(&slot).and_then(|s| s.rings[1].as_mut()).ok_or(CC_TIMEOUT)?;
            let trt = match (len, setup.is_in()) {
                (0, _) => 0,
                (_, true) => 3,
                (_, false) => 2,
            };
            let mut trbs = vec![ring.push(setup.bytes(), 8, TRB_SETUP << 10 | TRB_IDT | trt << 16)];
            if let Some(b) = &buf {
                let dir = if setup.is_in() { 1 << 16 } else { 0 };
                trbs.push(ring.push(b.phys, len as u32, TRB_DATA << 10 | TRB_ISP | dir));
            }
            // The status stage runs opposite to the data (IN when there is none).
            let status_in = len == 0 || !setup.is_in();
            trbs.push(ring.push(0, 0, TRB_STATUS << 10 | TRB_IOC | if status_in { 1 << 16 } else { 0 }));
            trbs
        };
        self.doorbell(slot, 1);
        let c = self.wait_completion(&trbs, 2000);
        if c.code != CC_SUCCESS {
            if c.code == CC_STALL {
                self.recover(slot, 1);
            }
            return Err(c.code);
        }
        let n = len.saturating_sub(c.residual as usize);
        if let (Some(b), Some(d), true) = (&buf, data, setup.is_in()) {
            d[..n].copy_from_slice(&b.bytes_mut()[..n]);
        }
        Ok(n)
    }

    /// A bulk (or interrupt OUT) transfer of `len` bytes at `phys`, split at
    /// 64 KiB boundaries as TRBs require. Returns the bytes transferred.
    pub fn bulk(&self, slot: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8> {
        let dci = ep.dci();
        let trbs = {
            let mut inner = self.inner.lock();
            let ring = inner.slots.get_mut(&slot).and_then(|s| s.rings[dci as usize].as_mut()).ok_or(CC_TIMEOUT)?;
            let mut trbs = Vec::new();
            let mut done = 0usize;
            let max_packet = ep.max_packet.max(1) as usize;
            loop {
                let addr = phys + done as u64;
                let chunk = ((0x1_0000 - (addr & 0xFFFF)) as usize).min(len - done);
                let last = done + chunk >= len;
                // TD size: packets still to come after this TRB (capped at 31).
                let remaining = (len - done - chunk).div_ceil(max_packet).min(31) as u32;
                let flags = if last { TRB_IOC | TRB_ISP } else { TRB_CHAIN | TRB_ISP };
                trbs.push(ring.push(addr, chunk as u32 | remaining << 17, TRB_NORMAL << 10 | flags));
                done += chunk;
                if last {
                    break;
                }
            }
            trbs
        };
        self.doorbell(slot, dci);
        let c = self.wait_completion(&trbs, timeout_ms);
        if c.code != CC_SUCCESS {
            if c.code == CC_STALL {
                self.recover(slot, dci);
            }
            return Err(c.code);
        }
        Ok(len.saturating_sub(c.residual as usize))
    }

    // ------------------------------------------------------------ enumeration

    fn enum_task(&'static self) {
        loop {
            self.enum_wake.wait(1000, || !self.port_events.lock().is_empty() || !self.hub_events.lock().is_empty());
            // Each pop is its own statement, so the queue lock (interrupts
            // off) isn't held while a device is enumerated.
            loop {
                let Some(port) = self.port_events.lock().pop_front() else { break };
                self.root_port_changed(port);
            }
            loop {
                let Some((hub_slot, bitmap)) = self.hub_events.lock().pop_front() else { break };
                super::hub::changed(self, hub_slot, bitmap);
            }
            self.settled.store(true, Ordering::Release);
        }
    }

    /// Called by the hub driver's status pipe.
    pub fn hub_changed(&self, slot: u8, bitmap: u32) {
        self.hub_events.lock().push_back((slot, bitmap));
        self.enum_wake.wake_all();
    }

    fn root_port_changed(&'static self, port: u8) {
        if port == 0 || port > self.max_ports {
            return;
        }
        let sc = self.portsc(port);
        // Acknowledge the change bits.
        self.set_portsc(port, (sc & PORT_KEEP) | (sc & PORT_CHANGES));
        let attached = self.has_device(port, 0);
        if sc & PORT_CCS == 0 {
            if attached {
                self.detach_port(port, 0, 0);
            }
            return;
        }
        if attached {
            return;
        }
        // USB 2 ports need a reset to become enabled; USB 3 ports train on their own.
        if self.port_major[port as usize] != 3 || self.portsc(port) & PORT_PED == 0 {
            let sc = self.portsc(port);
            self.set_portsc(port, (sc & PORT_KEEP) | PORT_RESET);
            if !wait_until(500, || self.portsc(port) & PORT_PRC != 0) {
                log!("usb", "port {}: reset timed out", port);
                return;
            }
            let sc = self.portsc(port);
            self.set_portsc(port, (sc & PORT_KEEP) | PORT_PRC);
        }
        if !wait_until(100, || self.portsc(port) & PORT_PED != 0) {
            log!("usb", "port {}: not enabled after reset", port);
            return;
        }
        let speed = Speed::from_xhci((self.portsc(port) >> 10) & 0xF);
        crate::sched::sleep_ms(10); // reset recovery
        self.enumerate(port, 0, 0, speed, None);
    }

    /// Is there a device at this position (root port + route)?
    pub fn has_device(&self, root_port: u8, route: u32) -> bool {
        self.devices.lock().iter().any(|d| d.root_port == root_port && d.route == route)
    }

    /// The device at root `port` + `route` (whose route has `depth` tiers) and
    /// everything behind it are gone. `depth` 0 means the whole root port.
    pub fn detach_port(&self, port: u8, route: u32, depth: u32) {
        let mask = if depth == 0 { 0 } else { (1u32 << (4 * depth)) - 1 };
        let gone: Vec<DeviceRecord> = {
            let mut devs = self.devices.lock();
            let (gone, keep): (Vec<_>, Vec<_>) =
                devs.drain(..).partition(|d| d.root_port == port && d.route & mask == route & mask);
            *devs = keep;
            gone
        };
        for d in gone {
            log!("usb", "{} disconnected", d.name);
            let hooks: Vec<_> = {
                let mut all = self.detach.lock();
                let (mine, rest): (Vec<_>, Vec<_>) = all.drain(..).partition(|(s, _)| *s == d.slot);
                *all = rest;
                mine
            };
            for (_, hook) in hooks {
                hook();
            }
            self.pipes.lock().retain(|&(s, _), _| s != d.slot);
            self.command(0, TRB_DISABLE_SLOT << 10 | (d.slot as u32) << 24);
            let mut inner = self.inner.lock();
            inner.dcbaa.write::<u64>(d.slot as usize * 8, 0);
            inner.slots.remove(&d.slot);
            drop(inner);
            if d.driver != "hub" {
                crate::gui::notify::system("USB device removed", &d.name);
            }
        }
    }

    /// Plugged in while running (not present at boot): worth a notification.
    pub fn announce(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }

    /// Runs `hook` when the device in `slot` is unplugged.
    pub fn on_detach(&self, slot: u8, hook: Box<dyn Fn() + Send + Sync>) {
        self.detach.lock().push((slot, hook));
    }

    fn ep_ctx(&self, dci: u8) -> usize {
        self.ctx_size * (1 + dci as usize)
    }

    /// Addresses and configures a new device, then hands it to a class driver.
    pub fn enumerate(
        &'static self,
        root_port: u8,
        route: u32,
        depth: u32,
        speed: Speed,
        hub: Option<(u8, u8, bool)>,
    ) -> Option<u8> {
        let c = self.command(0, TRB_ENABLE_SLOT << 10);
        if c.code != CC_SUCCESS {
            log!("usb", "port {}: no free device slot ({})", root_port, c.code);
            return None;
        }
        let slot = c.slot;
        let (Some(output), Some(input), Some(ep0)) =
            (Dma::new(self.ctx_size * 32), Dma::new(self.ctx_size * 33), Ring::new())
        else {
            return None;
        };
        // Input control context: add the slot and endpoint 0.
        input.write::<u32>(4, 0b11);
        // Input context: the control context, then the slot context, then endpoints.
        let s = self.ctx_size;
        input.write::<u32>(s, route & 0xF_FFFF | (speed as u32) << 20 | 1 << 27);
        input.write::<u32>(s + 4, (root_port as u32) << 16);
        if let Some((hub_slot, hub_port, true)) = hub {
            // A full/low-speed device behind a high-speed hub: its transaction translator.
            input.write::<u32>(s + 8, hub_slot as u32 | (hub_port as u32) << 8);
        }
        let mps = speed.default_max_packet() as u32;
        let e = self.ep_ctx(1);
        input.write::<u32>(e + 4, 3 << 1 | 4 << 3 | mps << 16);
        input.write::<u64>(e + 8, ep0.phys() | 1);
        input.write::<u32>(e + 16, 8);
        let input_phys = input.phys;
        {
            let mut inner = self.inner.lock();
            inner.dcbaa.write::<u64>(slot as usize * 8, output.phys);
            let mut rings: [Option<Ring>; 32] = Default::default();
            rings[1] = Some(ep0);
            inner.slots.insert(slot, SlotState { output, input, rings });
        }
        let c = self.command(input_phys, TRB_ADDRESS_DEVICE << 10 | (slot as u32) << 24);
        if c.code != CC_SUCCESS {
            log!("usb", "port {}: address device failed ({})", root_port, c.code);
            self.release_slot(slot);
            return None;
        }

        // The first 8 bytes tell the real packet size of endpoint 0.
        let mut head = [0u8; 8];
        if self.control(slot, Setup::get_descriptor(super::DESC_DEVICE, 0, 0, 8), Some(&mut head)).is_err() {
            log!("usb", "port {}: device does not answer", root_port);
            self.release_slot(slot);
            return None;
        }
        let real = if speed as u8 >= Speed::Super as u8 { 1u32 << head[7].min(10) } else { head[7] as u32 };
        if real != mps && real >= 8 {
            {
                let inner = self.inner.lock();
                let st = &inner.slots[&slot];
                st.input.write::<u32>(0, 0);
                st.input.write::<u32>(4, 0b10);
                let e = self.ep_ctx(1);
                let old = st.input.read::<u32>(e + 4);
                st.input.write::<u32>(e + 4, (old & 0xFFFF) | real << 16);
            }
            self.command(input_phys, TRB_EVALUATE_CTX << 10 | (slot as u32) << 24);
        }
        let mut d = [0u8; 18];
        self.control(slot, Setup::get_descriptor(super::DESC_DEVICE, 0, 0, 18), Some(&mut d)).ok()?;
        let desc = DeviceDescriptor::parse(&d)?;
        let product = self.string(slot, desc.product_index);
        let maker = self.string(slot, desc.manufacturer_index);
        let name = match (maker.is_empty(), product.is_empty()) {
            (_, true) => format!("USB device {:04x}:{:04x}", desc.vendor, desc.product),
            (true, false) => product,
            (false, false) if product.starts_with(maker.as_str()) => product,
            (false, false) => format!("{maker} {product}"),
        };
        let mut head = [0u8; 9];
        self.control(slot, Setup::get_descriptor(super::DESC_CONFIG, 0, 0, 9), Some(&mut head)).ok()?;
        let total = u16::from_le_bytes([head[2], head[3]]).clamp(9, 4096);
        let mut full = vec![0u8; total as usize];
        let n = self.control(slot, Setup::get_descriptor(super::DESC_CONFIG, 0, 0, total), Some(&mut full)).ok()?;
        let config = Configuration::parse(&full[..n])?;
        self.control(slot, Setup::set_configuration(config.value), None).ok()?;

        let dev = super::UsbDevice {
            ctrl: self,
            slot,
            speed,
            root_port,
            route,
            depth,
            desc: desc.clone(),
            config,
            name: name.clone(),
        };
        // Recorded first, so an unplug during the driver's setup still finds it.
        self.devices.lock().push(DeviceRecord {
            slot,
            root_port,
            route,
            speed,
            desc,
            name: name.clone(),
            driver: String::new(),
        });
        let driver = super::attach(&dev);
        log!(
            "usb",
            "{} ({}, port {}{}) — {}",
            name,
            speed.name(),
            root_port,
            if route != 0 { format!(", route {:x}", route) } else { String::new() },
            driver
        );
        if let Some(d) = self.devices.lock().iter_mut().find(|d| d.slot == slot) {
            d.driver = driver.clone();
        }
        if driver != "hub" && self.announce() {
            crate::gui::notify::system("USB device connected", &name);
        }
        Some(slot)
    }

    fn release_slot(&self, slot: u8) {
        self.command(0, TRB_DISABLE_SLOT << 10 | (slot as u32) << 24);
        let mut inner = self.inner.lock();
        inner.dcbaa.write::<u64>(slot as usize * 8, 0);
        inner.slots.remove(&slot);
    }

    fn string(&self, slot: u8, index: u8) -> String {
        if index == 0 {
            return String::new();
        }
        let mut b = [0u8; 255];
        match self.control(slot, Setup::get_descriptor(super::DESC_STRING, index, 0x0409, 255), Some(&mut b)) {
            Ok(n) => super::parse_string(&b[..n]),
            Err(_) => String::new(),
        }
    }

    /// Configures endpoints (and hub properties) for `slot`, creating their rings.
    pub fn configure(&self, slot: u8, speed: Speed, eps: &[Endpoint], hub: Option<(u8, u32)>) -> bool {
        let input_phys = {
            let mut inner = self.inner.lock();
            let Some(st) = inner.slots.get_mut(&slot) else { return false };
            let max_dci = eps.iter().map(|e| e.dci()).max().unwrap_or(1).max(1);
            let mut add = 1u32; // the slot context
            for e in eps {
                add |= 1 << e.dci();
            }
            let input = &st.input;
            input.write::<u32>(0, 0);
            input.write::<u32>(4, add);
            // Start from the controller's view of the slot.
            for dw in 0..4 {
                let v = st.output.read::<u32>(dw * 4);
                input.write::<u32>(self.ctx_size + dw * 4, v);
            }
            let s = self.ctx_size;
            // Context entries: the highest endpoint in use (earlier drivers' included).
            let entries = (input.read::<u32>(s) >> 27).max(max_dci as u32);
            let mut dw0 = input.read::<u32>(s) & !(0x1F << 27);
            dw0 |= entries << 27;
            if let Some((ports, ttt)) = hub {
                dw0 |= 1 << 26;
                let dw1 = input.read::<u32>(s + 4) & 0x00FF_FFFF;
                input.write::<u32>(s + 4, dw1 | (ports as u32) << 24);
                let dw2 = input.read::<u32>(s + 8) & !(3 << 16);
                input.write::<u32>(s + 8, dw2 | (ttt & 3) << 16);
            }
            input.write::<u32>(s, dw0);
            for e in eps {
                let Some(ring) = Ring::new() else { return false };
                let off = self.ep_ctx(e.dci());
                let kind = match (e.kind, e.is_in()) {
                    (1, false) => 1,
                    (2, false) => 2,
                    (3, false) => 3,
                    (1, true) => 5,
                    (2, true) => 6,
                    _ => 7,
                };
                let interval = match (e.kind, speed) {
                    (3, Speed::Low | Speed::Full) => (e.interval.max(1) as u32 * 8).ilog2().clamp(3, 10),
                    (3 | 1, _) => (e.interval.clamp(1, 16) - 1) as u32,
                    _ => 0,
                };
                for i in 0..5 {
                    input.write::<u32>(off + i * 4, 0);
                }
                input.write::<u32>(off, interval << 16);
                input.write::<u32>(off + 4, 3 << 1 | kind << 3 | (e.burst as u32) << 8 | (e.max_packet as u32) << 16);
                input.write::<u64>(off + 8, ring.phys() | 1);
                let avg = if e.kind == 3 { e.max_packet as u32 } else { 3072 };
                let esit = if e.kind == 3 { e.max_packet as u32 * (e.burst as u32 + 1) } else { 0 };
                input.write::<u32>(off + 16, avg | esit << 16);
                st.rings[e.dci() as usize] = Some(ring);
            }
            input.phys
        };
        let c = self.command(input_phys, TRB_CONFIGURE_EP << 10 | (slot as u32) << 24);
        if c.code != CC_SUCCESS {
            log!("usb", "slot {}: configure endpoints failed ({})", slot, c.code);
        }
        c.code == CC_SUCCESS
    }

    pub fn describe_devices(&self) -> Vec<String> {
        self.devices
            .lock()
            .iter()
            .map(|d| {
                format!(
                    "Bus {:03} Port {:>2}{} {:04x}:{:04x} {} [{}, {}]",
                    self.index + 1,
                    d.root_port,
                    if d.route != 0 { format!(".{:x}", d.route) } else { String::new() },
                    d.desc.vendor,
                    d.desc.product,
                    d.name,
                    d.speed.name(),
                    d.driver
                )
            })
            .collect()
    }
}
