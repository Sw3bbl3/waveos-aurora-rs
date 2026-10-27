//! UHCI host controller driver (USB 1.1: 12 and 1.5 Mb/s devices).
//!
//! Intel's USB 1.1 controller, programmed through I/O ports, and usually
//! paired with an EHCI on Intel chipsets of the 2000s (QEMU's ich9 USB
//! too). Each millisecond it walks one frame-list entry; ours all lead to
//! the same chain of queue heads: the interrupt endpoints' first, then
//! control transfers, then bulk. A transfer descriptor (TD) moves one
//! packet, so a transfer is a chain of TDs under its own queue head, which
//! leaves the chain once it is done. The data toggle (DATA0/DATA1) of each
//! endpoint is ours to keep. UHCI reports port changes only when asked, so
//! the bus polls its two ports.

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
use x86_64::instructions::port::Port;

// I/O registers.
const USBCMD: u16 = 0x00;
const USBSTS: u16 = 0x02;
const USBINTR: u16 = 0x04;
const FRNUM: u16 = 0x06;
const FRBASEADD: u16 = 0x08;
const SOFMOD: u16 = 0x0C;
const PORTSC: u16 = 0x10;

const CMD_RUN: u16 = 1 << 0;
const CMD_HCRESET: u16 = 1 << 1;
const CMD_GRESET: u16 = 1 << 2;
const CMD_CONFIGURED: u16 = 1 << 6;
const CMD_MAXP64: u16 = 1 << 7;

const STS_HSE: u16 = 1 << 3;
const STS_HCPE: u16 = 1 << 4;
const STS_HALTED: u16 = 1 << 5;
const STS_ACK: u16 = 0x1F;

const INTR_TIMEOUT_CRC: u16 = 1 << 0;
const INTR_IOC: u16 = 1 << 2;
const INTR_SHORT: u16 = 1 << 3;

const PORT_CCS: u16 = 1 << 0;
const PORT_CSC: u16 = 1 << 1;
const PORT_PE: u16 = 1 << 2;
const PORT_PEC: u16 = 1 << 3;
const PORT_LSDA: u16 = 1 << 8;
const PORT_RESET: u16 = 1 << 9;
/// Change bits (written back as 1 only to clear them).
const PORT_RWC: u16 = PORT_CSC | PORT_PEC;

/// PCI legacy support register: firmware's keyboard emulation and our IRQ.
const LEGSUP: u16 = 0xC0;
const LEGSUP_CLEAR: u16 = 0x8F00;
const LEGSUP_PIRQ: u16 = 0x2000;

// Link pointers.
const T: u32 = 1;
const Q: u32 = 1 << 1;
const DEPTH_FIRST: u32 = 1 << 2;

// QH dwords.
const QH_LINK: usize = 0;
const QH_ELEMENT: usize = 1;

// TD dwords.
const TD_LINK: usize = 0;
const TD_STATUS: usize = 1;
const TD_TOKEN: usize = 2;
const TD_BUFFER: usize = 3;

const ST_ACTIVE: u32 = 1 << 23;
const ST_STALLED: u32 = 1 << 22;
/// Stalled, data buffer error, babble, CRC/timeout, bitstuff (NAK is not a failure).
const ST_FAILED: u32 = 0x76 << 16;
const ST_IOC: u32 = 1 << 24;
const ST_LOW_SPEED: u32 = 1 << 26;
const ST_CERR3: u32 = 3 << 27;
const ST_SPD: u32 = 1 << 29;

const PID_IN: u32 = 0x69;
const PID_OUT: u32 = 0xE1;
const PID_SETUP: u32 = 0x2D;

/// Bytes a TD moved (the hardware stores n - 1).
fn actual(status: u32) -> usize {
    ((status + 1) & 0x7FF) as usize
}

struct Device {
    pos: Position,
    max_packet0: u16,
    /// Endpoint address → (max packet, next data toggle).
    eps: BTreeMap<u8, (u16, bool)>,
    busy: Arc<Mutex<()>>,
}

struct Pipe {
    dev: u8,
    qh: Block,
    td: Block,
    buf: Dma,
    size: usize,
    token: u32,
    status: u32,
    toggle: bool,
    handler: Arc<dyn PipeHandler>,
    errors: u32,
}

pub struct Uhci {
    index: usize,
    io: u16,
    ports: u8,
    pool: Pool,
    addresses: Addresses,
    events: Events,
    frame_list: Dma,
    /// Anchors of the chain every frame walks: interrupt → control → bulk.
    anchors: [Block; 3],
    /// Queue heads after each anchor, in link order.
    chains: Mutex<[Vec<Block>; 3]>,
    devices: IrqMutex<BTreeMap<u8, Device>>,
    pipes: IrqMutex<Vec<Pipe>>,
    /// Finished transfers' QHs and TDs, freed once the controller can no
    /// longer be looking at them (two frames after unlinking).
    retired: IrqMutex<Vec<(u64, Block, Dma)>>,
    status: AtomicU32,
    bus: spin::Once<&'static Bus>,
}

const INTERRUPT: usize = 0;
const CONTROL: usize = 1;
const BULK: usize = 2;

static COUNT: AtomicUsize = AtomicUsize::new(0);

fn interrupt(arg: usize) -> bool {
    let c = unsafe { &*(arg as *const Uhci) };
    let sts = c.r16(USBSTS) & STS_ACK;
    if sts == 0 {
        return false;
    }
    c.w16(USBSTS, sts);
    c.status.fetch_or(sts as u32, Ordering::AcqRel);
    c.events.signal();
    true
}

pub fn probe(dev: &pci::Device) {
    let io = match dev.bars[4] {
        pci::Bar::Io { port, .. } => port,
        _ => return,
    };
    dev.enable();
    // Stop the firmware's legacy emulation (SMIs) before touching anything.
    dev.write16(LEGSUP, LEGSUP_CLEAR);
    let pool = Pool::new();
    let (Some(frame_list), Some(a0), Some(a1), Some(a2)) = (Dma::new32(4096), pool.alloc(), pool.alloc(), pool.alloc())
    else {
        return;
    };
    let anchors = [a0, a1, a2];
    a0.write(QH_LINK, a1.phys | Q);
    a1.write(QH_LINK, a2.phys | Q);
    a2.write(QH_LINK, T);
    for a in anchors {
        a.write(QH_ELEMENT, T);
    }
    for i in 0..1024 {
        frame_list.write::<u32>(i * 4, a0.phys | Q);
    }
    let mut c = Uhci {
        index: COUNT.fetch_add(1, Ordering::Relaxed),
        io,
        ports: 0,
        pool,
        addresses: Addresses::new(),
        events: Events::new(),
        frame_list,
        anchors,
        chains: Mutex::new([Vec::new(), Vec::new(), Vec::new()]),
        devices: IrqMutex::new(BTreeMap::new()),
        pipes: IrqMutex::new(Vec::new()),
        retired: IrqMutex::new(Vec::new()),
        status: AtomicU32::new(0),
        bus: spin::Once::new(),
    };
    // Ports: as many consecutive registers as read back bit 7 set (usually 2).
    c.ports =
        (0..8u16).take_while(|&i| c.r16(PORTSC + i * 2) & 0x80 != 0 && c.r16(PORTSC + i * 2) != 0xFFFF).count() as u8;
    let c: &'static Uhci = Box::leak(Box::new(c));
    if !c.start() {
        log!("uhci", "controller {} did not start", c.index);
        return;
    }
    let arg = c as *const Uhci as usize;
    if let Some(line) = dev.enable_intx("uhci", interrupt, arg) {
        c.events.set_intx(line);
    }
    dev.write16(LEGSUP, LEGSUP_PIRQ);
    log!("uhci", "controller {}: {} ports", c.index, c.ports);
    c.bus.call_once(|| super::register(c));
    crate::sched::spawn_task("uhci-events", event_task_entry, arg as u64, 0, crate::mm::vmm::kernel_pml4());
}

fn event_task_entry(arg: u64) {
    unsafe { &*(arg as *const Uhci) }.event_task();
}

impl Uhci {
    fn r16(&self, reg: u16) -> u16 {
        unsafe { Port::<u16>::new(self.io + reg).read() }
    }
    fn w16(&self, reg: u16, v: u16) {
        unsafe { Port::<u16>::new(self.io + reg).write(v) }
    }
    fn w32(&self, reg: u16, v: u32) {
        unsafe { Port::<u32>::new(self.io + reg).write(v) }
    }
    fn portsc(&self, port: u8) -> u16 {
        self.r16(PORTSC + (port as u16 - 1) * 2)
    }
    fn set_portsc(&self, port: u8, v: u16) {
        self.w16(PORTSC + (port as u16 - 1) * 2, v);
    }

    fn start(&self) -> bool {
        self.w16(USBINTR, 0);
        self.w16(USBCMD, CMD_GRESET);
        crate::sched::sleep_ms(50);
        self.w16(USBCMD, 0);
        crate::sched::sleep_ms(10);
        self.w16(USBCMD, CMD_HCRESET);
        if crate::drivers::block::wait_for(50, || self.r16(USBCMD) & CMD_HCRESET == 0).is_err() {
            return false;
        }
        self.w16(USBSTS, STS_ACK);
        self.w32(FRBASEADD, self.frame_list.phys as u32);
        self.w16(FRNUM, 0);
        unsafe { Port::<u8>::new(self.io + SOFMOD).write(64) };
        self.w16(USBINTR, INTR_TIMEOUT_CRC | INTR_IOC | INTR_SHORT);
        self.w16(USBCMD, CMD_RUN | CMD_CONFIGURED | CMD_MAXP64);
        crate::drivers::block::wait_for(10, || self.r16(USBSTS) & STS_HALTED == 0).is_ok()
    }

    fn event_task(&'static self) {
        loop {
            self.events.idle(4, || self.status.load(Ordering::Acquire) != 0);
            if self.events.live() {
                // Whatever an interrupt that went missing would have said.
                interrupt(self as *const Uhci as usize);
            }
            let mut sts = self.status.swap(0, Ordering::AcqRel) as u16;
            if !self.events.live() {
                let polled = self.r16(USBSTS) & STS_ACK;
                self.w16(USBSTS, polled);
                sts |= polled;
            }
            if sts & (STS_HSE | STS_HCPE) != 0 {
                log!("uhci", "controller {}: host error {:#x}", self.index, sts);
            }
            self.poll_pipes();
            self.reap();
        }
    }

    // --------------------------------------------------------------- chains

    fn link(&self, chain: usize, qh: Block) {
        let mut chains = self.chains.lock();
        let anchor = self.anchors[chain];
        qh.write(QH_LINK, anchor.read(QH_LINK));
        fence(Ordering::SeqCst);
        anchor.write(QH_LINK, qh.phys | Q);
        chains[chain].insert(0, qh);
    }

    /// Takes a QH out of the chain and waits until it is safe to reuse.
    fn unlink(&self, chain: usize, qh: Block) {
        self.detach(chain, qh);
        crate::sched::sleep_ms(2);
    }

    /// Takes a QH out of the chain; the controller may still be on it until
    /// the next frame.
    fn detach(&self, chain: usize, qh: Block) {
        let mut chains = self.chains.lock();
        let Some(i) = chains[chain].iter().position(|q| *q == qh) else { return };
        let prev = if i == 0 { self.anchors[chain] } else { chains[chain][i - 1] };
        prev.write(QH_LINK, qh.read(QH_LINK));
        chains[chain].remove(i);
    }

    /// Frees what finished transfers left behind, once it is old enough.
    fn reap(&self) {
        let now = crate::time::uptime_ms();
        let done: Vec<(u64, Block, Dma)> = {
            let mut r = self.retired.lock();
            let (old, keep): (Vec<_>, Vec<_>) = r.drain(..).partition(|e| e.0 <= now);
            *r = keep;
            old
        };
        for (_, qh, tds) in done {
            self.pool.free(qh);
            drop(tds);
        }
    }

    fn status_bits(pos: &Position) -> u32 {
        ST_ACTIVE | ST_CERR3 | if pos.speed == Speed::Low { ST_LOW_SPEED } else { 0 }
    }

    /// Runs packets `(pid and target, phys, len, toggle)` as one chain under
    /// a fresh QH and waits. Returns the bytes moved by IN/OUT data packets
    /// and how many packets completed. A short IN packet ends the transfer,
    /// except in a `control` transfer, whose status packet still runs.
    fn run(
        &self,
        chain: usize,
        pos: &Position,
        packets: &[(u32, u64, usize, bool)],
        timeout_ms: u64,
        control: bool,
    ) -> (Result<usize, u8>, usize) {
        let Some(qh) = self.pool.alloc() else { return (Err(TIMEOUT), 0) };
        // TDs: 16 bytes each in one buffer (a bulk transfer can be a thousand packets).
        let Some(tds) = Dma::new32(packets.len() * 16) else {
            self.pool.free(qh);
            return (Err(TIMEOUT), 0);
        };
        let addr = |i: usize| tds.phys as u32 + (i * 16) as u32;
        let base = Self::status_bits(pos);
        let last = packets.len() - 1;
        for (i, &(pid, phys, len, toggle)) in packets.iter().enumerate() {
            let link = if i == last { T } else { addr(i + 1) | DEPTH_FIRST };
            let ioc = if i == last { ST_IOC } else { 0 };
            // A short packet stops the queue for bulk IN (the transfer ends there).
            let spd = if pid & 0xFF == PID_IN && !control { ST_SPD } else { 0 };
            tds.write::<u32>(i * 16, link);
            tds.write::<u32>(i * 16 + 4, base | ioc | spd);
            tds.write::<u32>(i * 16 + 8, pid | toggle_bits(toggle) | len_bits(len));
            tds.write::<u32>(i * 16 + 12, phys as u32);
        }
        qh.write(QH_LINK, T);
        qh.write(QH_ELEMENT, addr(0));
        self.link(chain, qh);
        let state = || -> Option<Result<(), u8>> {
            for (i, &(pid, _, len, _)) in packets.iter().enumerate() {
                let st = tds.read::<u32>(i * 16 + 4);
                if st & ST_ACTIVE != 0 {
                    return None;
                }
                if st & ST_FAILED != 0 {
                    // A STALL handshake alone, or errors until the retries ran out.
                    let stall = st & ST_FAILED == ST_STALLED;
                    return Some(Err(if stall { STALL } else { ERROR }));
                }
                // A short IN packet ends a bulk transfer (a control transfer's
                // status packet still runs).
                if pid & 0xFF == PID_IN && actual(st) < len && !control {
                    return Some(Ok(()));
                }
            }
            Some(Ok(()))
        };
        self.events.wait(timeout_ms, || state().is_some());
        let result = state().unwrap_or(Err(TIMEOUT));
        self.detach(chain, qh);
        let mut moved = 0;
        let mut done = 0;
        for (i, &(pid, _, _, _)) in packets.iter().enumerate() {
            let st = tds.read::<u32>(i * 16 + 4);
            if st & ST_ACTIVE != 0 || st & ST_FAILED != 0 {
                break;
            }
            done += 1;
            if pid & 0xFF != PID_SETUP {
                moved += actual(st);
            }
        }
        self.retired.lock().push((crate::time::uptime_ms() + 3, qh, tds));
        self.reap();
        (result.map(|_| moved), done)
    }

    fn device(&self, dev: u8) -> Option<(Position, u16, Arc<Mutex<()>>)> {
        self.devices.lock().get(&dev).map(|d| (d.pos, d.max_packet0, d.busy.clone()))
    }

    // ---------------------------------------------------------------- pipes

    fn arm_pipe(p: &mut Pipe) {
        p.td.write(TD_LINK, T);
        p.td.write(TD_STATUS, p.status);
        p.td.write(TD_TOKEN, p.token | toggle_bits(p.toggle));
        p.td.write(TD_BUFFER, p.buf.phys as u32);
        fence(Ordering::SeqCst);
        p.qh.write(QH_ELEMENT, p.td.phys);
    }

    fn poll_pipes(&self) {
        let mut ready = Vec::new();
        {
            let mut pipes = self.pipes.lock();
            for p in pipes.iter_mut() {
                let st = p.td.read(TD_STATUS);
                if st & ST_ACTIVE != 0 {
                    continue;
                }
                // The controller moves the QH's element pointer on just after
                // retiring the TD; re-arming before that would be overwritten.
                let mut spins = 0;
                while p.qh.read(QH_ELEMENT) & T == 0 && spins < 10_000 {
                    core::hint::spin_loop();
                    spins += 1;
                }
                if st & ST_FAILED != 0 {
                    p.errors += 1;
                    if p.errors < 100 {
                        Self::arm_pipe(p);
                    }
                    continue;
                }
                p.errors = 0;
                p.toggle = !p.toggle;
                let n = actual(st).min(p.size);
                ready.push((p.handler.clone(), p.buf.bytes()[..n].to_vec()));
                Self::arm_pipe(p);
            }
        }
        for (h, data) in ready {
            h.data(&data);
        }
    }
}

fn toggle_bits(toggle: bool) -> u32 {
    (toggle as u32) << 19
}

fn len_bits(len: usize) -> u32 {
    ((len as u32).wrapping_sub(1) & 0x7FF) << 21
}

/// The address and endpoint fields of a TD token.
fn target(addr: u8, ep: u8) -> u32 {
    (addr as u32) << 8 | (ep as u32 & 0xF) << 15
}

impl Hcd for Uhci {
    fn kind(&self) -> &'static str {
        "UHCI"
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
        sc & PORT_CCS != 0
    }

    fn port_reset(&self, port: u8) -> Option<Speed> {
        let sc = self.portsc(port);
        self.set_portsc(port, (sc & !PORT_RWC) | PORT_RESET);
        crate::sched::sleep_ms(50);
        let sc = self.portsc(port);
        self.set_portsc(port, sc & !(PORT_RWC | PORT_RESET));
        crate::sched::sleep_ms(1);
        // UHCI ports must be enabled by hand after a reset (sometimes twice).
        for _ in 0..10 {
            let sc = self.portsc(port);
            if sc & PORT_CCS == 0 {
                return None;
            }
            if sc & PORT_PE != 0 {
                return Some(if sc & PORT_LSDA != 0 { Speed::Low } else { Speed::Full });
            }
            self.set_portsc(port, (sc & !PORT_RWC) | (sc & PORT_RWC) | PORT_PE);
            crate::sched::sleep_ms(10);
        }
        log!("usb", "UHCI port {}: not enabled after reset", port);
        None
    }

    fn new_device(&self, pos: &Position) -> Option<u8> {
        let addr = self.addresses.alloc()?;
        self.devices
            .lock()
            .insert(addr, Device { pos: *pos, max_packet0: 8, eps: BTreeMap::new(), busy: Arc::new(Mutex::new(())) });
        // Until SET_ADDRESS, the device answers at 0: send it from there.
        let set_address = Setup { request_type: 0, request: 5, value: addr as u16, index: 0, length: 0 };
        if self.control_at(0, pos, 8, set_address, None).is_err() {
            log!("usb", "UHCI port {}: SET_ADDRESS failed", pos.root_port);
            self.release(addr);
            return None;
        }
        crate::sched::sleep_ms(2);
        Some(addr)
    }

    fn set_ep0_max_packet(&self, dev: u8, max_packet: u16) {
        if let Some(d) = self.devices.lock().get_mut(&dev) {
            d.max_packet0 = max_packet;
        }
    }

    fn control(&self, dev: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        let (pos, mps, busy) = self.device(dev).ok_or(TIMEOUT)?;
        let _busy = busy.lock();
        self.control_at(dev, &pos, mps, setup, data)
    }

    fn transfer(&self, dev: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8> {
        let (pos, _, busy) = self.device(dev).ok_or(TIMEOUT)?;
        let _busy = busy.lock();
        let (mps, toggle) = *self.devices.lock().get(&dev).and_then(|d| d.eps.get(&ep.address)).ok_or(TIMEOUT)?;
        let pid = if ep.is_in() { PID_IN } else { PID_OUT };
        let t = target(dev, ep.number());
        let mps = mps.max(8) as usize;
        let mut packets = Vec::new();
        let mut off = 0;
        let mut tg = toggle;
        loop {
            let n = (len - off).min(mps);
            packets.push((pid | t, phys + off as u64, n, tg));
            tg = !tg;
            off += n;
            if off >= len {
                break;
            }
        }
        let chain = if ep.kind == 3 { INTERRUPT } else { BULK };
        let (r, done) = self.run(chain, &pos, &packets, timeout_ms, false);
        // The toggle moves on by the packets that went through.
        let next = if done % 2 == 1 { !toggle } else { toggle };
        if let Some(e) = self.devices.lock().get_mut(&dev).and_then(|d| d.eps.get_mut(&ep.address)) {
            e.1 = next;
        }
        r
    }

    fn configure(&self, dev: u8, eps: &[Endpoint], _hub: Option<HubInfo>) -> bool {
        let mut devs = self.devices.lock();
        let Some(d) = devs.get_mut(&dev) else { return false };
        for e in eps {
            d.eps.entry(e.address).or_insert((e.max_packet, false));
        }
        true
    }

    fn open_pipe(&self, dev: u8, ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool {
        let Some((pos, _, _)) = self.device(dev) else { return false };
        let size = (ep.max_packet as usize).clamp(1, 64);
        let (Some(qh), Some(td), Some(buf)) = (self.pool.alloc(), self.pool.alloc(), Dma::new32(size)) else {
            return false;
        };
        let mut pipe = Pipe {
            dev,
            qh,
            td,
            buf,
            size,
            token: PID_IN | target(dev, ep.number()) | len_bits(size),
            status: Self::status_bits(&pos) | ST_IOC,
            toggle: false,
            handler,
            errors: 0,
        };
        qh.write(QH_LINK, T);
        Self::arm_pipe(&mut pipe);
        self.link(INTERRUPT, qh);
        self.pipes.lock().push(pipe);
        true
    }

    fn reset_toggle(&self, dev: u8, ep_address: u8) {
        if let Some(e) = self.devices.lock().get_mut(&dev).and_then(|d| d.eps.get_mut(&ep_address)) {
            e.1 = false;
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
            self.unlink(INTERRUPT, p.qh);
            self.pool.free(p.qh);
            self.pool.free(p.td);
        }
        if let Some(d) = self.devices.lock().remove(&dev) {
            // Wait for a transfer in flight to give up.
            drop(d.busy.lock());
        }
        self.addresses.free(dev);
    }

    fn restart(&self) -> bool {
        let devs: Vec<u8> = self.devices.lock().keys().copied().collect();
        for d in devs {
            self.release(d);
        }
        self.anchors[0].write(QH_LINK, self.anchors[1].phys | Q);
        self.anchors[1].write(QH_LINK, self.anchors[2].phys | Q);
        self.anchors[2].write(QH_LINK, T);
        *self.chains.lock() = [Vec::new(), Vec::new(), Vec::new()];
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

impl Uhci {
    /// A control transfer to the device at `addr` (0 before SET_ADDRESS).
    fn control_at(
        &self,
        addr: u8,
        pos: &Position,
        mps: u16,
        setup: Setup,
        data: Option<&mut [u8]>,
    ) -> Result<usize, u8> {
        let len = setup.length as usize;
        let mut buf = Dma::new32(8 + len.max(1)).ok_or(TIMEOUT)?;
        buf.bytes_mut()[..8].copy_from_slice(&setup.bytes().to_le_bytes());
        if let (Some(d), false) = (&data, setup.is_in()) {
            buf.bytes_mut()[8..8 + len].copy_from_slice(&d[..len]);
        }
        let t = target(addr, 0);
        let mut packets = alloc::vec![(PID_SETUP | t, buf.phys, 8, false)];
        let pid = if setup.is_in() { PID_IN } else { PID_OUT };
        let mps = mps.max(8) as usize;
        let mut off = 0;
        let mut toggle = true;
        while off < len {
            let n = (len - off).min(mps);
            packets.push((pid | t, buf.phys + 8 + off as u64, n, toggle));
            toggle = !toggle;
            off += n;
        }
        // The status stage runs opposite to the data (IN when there is none), DATA1.
        let status_pid = if len == 0 || !setup.is_in() { PID_IN } else { PID_OUT };
        packets.push((status_pid | t, 0, 0, true));
        let (r, _) = self.run(CONTROL, pos, &packets, 2000, true);
        let n = r?;
        if let (Some(d), true) = (data, setup.is_in()) {
            d[..n].copy_from_slice(&buf.bytes()[8..8 + n]);
        }
        Ok(n)
    }
}
