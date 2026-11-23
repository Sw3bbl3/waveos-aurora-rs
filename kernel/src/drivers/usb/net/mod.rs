//! USB network adapters: Ethernet dongles, USB-C docks and phone tethering.
//!
//! Class drivers for the standard functions — CDC-ECM, CDC-NCM, CDC-EEM
//! ([`cdc`]) and RNDIS ([`rndis`]) — and vendor drivers for ASIX's AX88772
//! and AX88179 ([`asix`]). Realtek's RTL8152/8153/8156 run in their CDC
//! configuration, which [`choose`] selects when the device is enumerated.
//! The wire formats live in `aurora_usbnet` (host-tested and fuzzed); this
//! module moves the bytes and ties a device to the network stack.
//!
//! Each adapter is a [`UsbNic`], registered with `net` like any card:
//!
//! - **Receive**: the bulk IN endpoint is kept busy with several queued
//!   transfers (`Bus::open_stream`, on xHCI); each completed one is split
//!   into frames in place and the buffer queued again. Controllers that
//!   can't stream get a task looping over blocking transfers instead.
//! - **Transmit**: `send` (from the network task) packs each frame straight
//!   into a ring of DMA buffers, aggregating frames into one transfer
//!   where the format allows (NCM, RNDIS, EEM); the adapter's task sends
//!   the buffers in order. While one transfer is on the bus the next one
//!   fills, so batches grow with the load.
//! - **Status**: the interrupt endpoint's notifications (link, speed;
//!   RNDIS's "response available") are handed to the same task, which may
//!   then talk to the device on its control endpoint.
//! - **Unplug**: the device's detach hook marks it gone and removes the
//!   interface (`net::unregister`), which fails its connections at once and
//!   drops its routes; the task and the streams end, and the buffers go
//!   with the last reference.

pub mod asix;
pub mod cdc;
pub mod rndis;
#[cfg(feature = "ktest")]
pub mod sim;

use super::{Bus, DeviceDescriptor, Endpoint, PipeHandler, Setup, UsbDevice, STALL, TIMEOUT};
use crate::drivers::block::Dma;
use crate::sync::{IrqMutex, WaitQueue};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_usbnet::cdc::Function;
use aurora_usbnet::select::{self, Chip, Iface, Prefer};
use aurora_usbnet::{eem, ncm};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// How the bytes of a transfer are laid out.
pub enum Framing {
    /// One frame per transfer (CDC-ECM).
    Plain,
    /// CDC-NCM transfer blocks.
    Ncm { layout: ncm::Layout, max_frame: usize },
    /// CDC-EEM's stream of headed packets.
    Eem(IrqMutex<eem::Receiver>),
    /// RNDIS PACKET messages, `max_packets` to a transfer.
    Rndis { rx: IrqMutex<aurora_usbnet::rndis::Receiver>, shift: u32, max_packets: usize },
    /// ASIX AX88772: 4-byte headers on a stream.
    Ax772(IrqMutex<aurora_usbnet::asix::Ax772Receiver>),
    /// ASIX AX88179: 8-byte headers out, an aggregate with a trailer in.
    Ax179,
}

impl Framing {
    /// Whether several frames may share a transfer we send.
    fn aggregates(&self) -> bool {
        match self {
            Framing::Ncm { .. } | Framing::Eem(_) => true,
            Framing::Rndis { max_packets, .. } => *max_packets > 1,
            _ => false,
        }
    }
}

/// What the interrupt endpoint means, and how to follow up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Cdc(Function),
    Chip(Chip),
}

/// Counters for the System Report and the throughput tests.
#[derive(Default)]
pub struct Stats {
    pub rx_transfers: AtomicU64,
    pub rx_frames: AtomicU64,
    pub rx_bytes: AtomicU64,
    pub tx_transfers: AtomicU64,
    /// Times the controller was handed transfers (several at once when they queue up).
    pub tx_batches: AtomicU64,
    pub tx_frames: AtomicU64,
    pub tx_bytes: AtomicU64,
    /// Frames the transmit ring had no room for.
    pub tx_dropped: AtomicU64,
    /// Receive transfers that made no sense (in part or whole).
    pub rx_errors: AtomicU64,
    /// Transmit transfers that failed.
    pub tx_errors: AtomicU64,
}

/// Transmit buffers of `size` bytes, carved out of 64 KiB regions (a
/// hundred one-frame buffers don't need a hundred pages).
struct Slots {
    regions: Vec<Dma>,
    size: usize,
    per_region: usize,
    count: usize,
}

impl Slots {
    fn new(bus: &Bus, size: usize, count: usize) -> Option<Slots> {
        let size = size.next_multiple_of(64);
        let per_region = (64 * 1024 / size).max(1);
        let mut regions = Vec::new();
        for _ in 0..count.div_ceil(per_region) {
            regions.push(bus.dma(size * per_region)?);
        }
        Some(Slots { regions, size, per_region, count })
    }

    fn len(&self) -> usize {
        self.count
    }

    fn phys(&self, i: usize) -> u64 {
        self.regions[i / self.per_region].phys + ((i % self.per_region) * self.size) as u64
    }

    fn bytes_mut(&mut self, i: usize) -> &mut [u8] {
        let at = (i % self.per_region) * self.size;
        &mut self.regions[i / self.per_region].bytes_mut()[at..at + self.size]
    }
}

/// Transmit buffers: `fill` is being packed by `send`, `head` is the next
/// (or the current) one on the bus; those between are waiting.
struct Tx {
    bufs: Slots,
    lens: Vec<usize>,
    frames: Vec<usize>,
    head: usize,
    fill: usize,
    /// The block being built in `fill` (NCM).
    ncm: Option<ncm::Builder>,
    sequence: u16,
}

pub struct UsbNic {
    pub bus: &'static Bus,
    pub dev: u8,
    /// What `net` shows as the driver ("USB CDC-ECM", "ASIX AX88179 (USB)").
    pub label: String,
    pub kind: Kind,
    pub framing: Framing,
    /// The communication (or vendor) interface, for class requests.
    pub control_iface: u8,
    pub bulk_in: Endpoint,
    pub bulk_out: Endpoint,
    pub mac: [u8; 6],
    /// `eth` for adapters, `usb` for links like a tethered phone.
    pub prefix: &'static str,
    /// Receive transfer size.
    pub rx_size: usize,
    /// The bus speed the device runs at.
    pub usb_speed: super::Speed,
    link: AtomicBool,
    speed: AtomicU32,
    gone: AtomicBool,
    index: AtomicUsize,
    tx: IrqMutex<Tx>,
    wake: WaitQueue,
    /// Interrupt-endpoint news for the task (a copy of the last report).
    status: IrqMutex<Option<Vec<u8>>>,
    /// The interrupt endpoint has said something (else, see `tick`).
    heard: AtomicBool,
    started: u64,
    pub stats: Stats,
    /// Driver-specific state the task keeps (RNDIS request IDs, PHY address).
    pub aux: AtomicU32,
}

/// Every adapter, for the System Report and tests.
static NICS: IrqMutex<Vec<Arc<UsbNic>>> = IrqMutex::new(Vec::new());

/// What a class or vendor driver found while bringing a device up.
pub struct Setup2 {
    pub label: String,
    pub kind: Kind,
    pub framing: Framing,
    pub control_iface: u8,
    pub bulk_in: Endpoint,
    pub bulk_out: Endpoint,
    pub interrupt: Option<Endpoint>,
    pub mac: [u8; 6],
    pub link: bool,
    pub speed_mbps: u32,
    pub rx_size: usize,
    /// Bytes per transmit buffer.
    pub tx_size: usize,
    pub aux: u32,
}

impl UsbNic {
    pub fn link(&self) -> bool {
        self.link.load(Ordering::Acquire)
    }

    pub fn gone(&self) -> bool {
        self.gone.load(Ordering::Acquire)
    }

    pub fn set_speed(&self, mbps: u32) {
        self.speed.store(mbps, Ordering::Release);
    }

    /// The link changed: `net` restarts DHCP on it (or drops its addresses).
    pub fn set_link(&self, up: bool) {
        if self.link.swap(up, Ordering::AcqRel) != up {
            let index = self.index.load(Ordering::Acquire);
            if index != usize::MAX && !self.gone() {
                crate::net::link_changed(index, up);
            }
        }
    }

    /// A class request to the control interface.
    pub fn class_out(&self, request: u8, value: u16, data: &mut [u8]) -> Result<usize, u8> {
        let setup = Setup::class_interface(request, value, self.control_iface, data.len() as u16, false);
        self.bus.control(self.dev, setup, if data.is_empty() { None } else { Some(data) })
    }

    pub fn class_in(&self, request: u8, value: u16, data: &mut [u8]) -> Result<usize, u8> {
        let setup = Setup::class_interface(request, value, self.control_iface, data.len() as u16, true);
        self.bus.control(self.dev, setup, Some(data))
    }

    // ------------------------------------------------------------ receive

    /// One completed receive transfer (from the controller's event task or
    /// the receive task): its frames go to the network stack.
    fn received(&self, data: &[u8]) {
        if data.is_empty() || self.gone() {
            return;
        }
        let index = self.index.load(Ordering::Acquire);
        if index == usize::MAX {
            return;
        }
        self.stats.rx_transfers.fetch_add(1, Ordering::Relaxed);
        let mut frames = 0u64;
        let mut bytes = 0u64;
        let mut deliver = |f: &[u8]| {
            frames += 1;
            bytes += f.len() as u64;
            crate::net::receive(index, f.to_vec());
        };
        let bad = match &self.framing {
            Framing::Plain => {
                if (aurora_usbnet::MIN_FRAME..=aurora_usbnet::MAX_FRAME + 4).contains(&data.len()) {
                    deliver(data);
                    false
                } else {
                    true
                }
            }
            Framing::Ncm { max_frame, .. } => match ncm::parse(data, *max_frame) {
                Ok(ranges) => {
                    for r in ranges {
                        deliver(&data[r]);
                    }
                    false
                }
                Err(_) => true,
            },
            Framing::Eem(rx) => {
                let mut r = eem::Received::default();
                rx.lock().feed(data, &mut r, &mut deliver);
                // Echo requests are answered on the transmit path.
                for reply in r.replies {
                    self.queue_raw(&reply);
                }
                r.errors + r.bad_crc > 0
            }
            Framing::Rndis { rx, .. } => {
                let mut rx = rx.lock();
                let before = rx.errors;
                rx.feed(data, &mut deliver);
                rx.errors != before
            }
            Framing::Ax772(rx) => {
                let mut rx = rx.lock();
                let before = rx.errors;
                rx.feed(data, &mut deliver);
                rx.errors != before
            }
            Framing::Ax179 => aurora_usbnet::asix::ax179_rx(data, &mut deliver).is_err(),
        };
        if bad {
            self.stats.rx_errors.fetch_add(1, Ordering::Relaxed);
        }
        self.stats.rx_frames.fetch_add(frames, Ordering::Relaxed);
        self.stats.rx_bytes.fetch_add(bytes, Ordering::Relaxed);
    }

    // ----------------------------------------------------------- transmit

    /// Seals the buffer being filled (an NCM block gets its headers).
    fn seal(&self, tx: &mut Tx) {
        let fill = tx.fill;
        if let Some(b) = tx.ncm.as_mut() {
            if !b.is_empty() {
                let seq = tx.sequence;
                tx.sequence = tx.sequence.wrapping_add(1);
                let n = b.finish(tx.bufs.bytes_mut(fill), seq);
                tx.lens[fill] = n;
            }
        }
    }

    /// Packs one frame into the transmit ring. False when the ring is full.
    fn pack(&self, frame: &[u8]) -> bool {
        let mut tx = self.tx.lock();
        let tx = &mut *tx;
        let size = tx.bufs.size;
        let mps = self.bulk_out.max_packet.max(8) as usize;
        let need = match &self.framing {
            Framing::Plain => frame.len(),
            Framing::Ncm { .. } => 0,
            Framing::Eem(_) => eem::packet_len(frame.len()),
            Framing::Rndis { shift, .. } => aurora_usbnet::rndis::packet_len(frame.len(), *shift),
            Framing::Ax772(_) => aurora_usbnet::asix::ax772_tx_len(frame.len(), mps),
            Framing::Ax179 => 8 + frame.len(),
        };
        if need > size {
            return false;
        }
        // The buffer being filled is never the one on the bus (`flush` moves
        // filling on before it sends that one). Room left in it?
        let fill = tx.fill;
        let fits = match &self.framing {
            Framing::Ncm { .. } => tx.ncm.as_ref().is_some_and(|b| b.fits(frame.len())),
            Framing::Rndis { max_packets, .. } => tx.lens[fill] + need <= size && tx.frames[fill] < *max_packets,
            Framing::Eem(_) => tx.lens[fill] + need + eem::ZLP.len() <= size,
            _ => tx.lens[fill] == 0,
        };
        if !fits {
            if tx.lens[fill] == 0 && tx.frames[fill] == 0 {
                // Doesn't fit an empty buffer either.
                return false;
            }
            let next = (fill + 1) % tx.bufs.len();
            if next == tx.head {
                return false;
            }
            self.seal(tx);
            tx.fill = next;
            tx.lens[next] = 0;
            tx.frames[next] = 0;
            if let Framing::Ncm { layout, .. } = &self.framing {
                tx.ncm = Some(ncm::Builder::new(*layout));
            }
        }
        let fill = tx.fill;
        let at = tx.lens[fill];
        let buf = tx.bufs.bytes_mut(fill);
        let n = match &self.framing {
            Framing::Plain => {
                buf[..frame.len()].copy_from_slice(frame);
                frame.len()
            }
            Framing::Ncm { .. } => {
                let Some(b) = tx.ncm.as_mut() else { return false };
                if !b.push(buf, frame) {
                    return false;
                }
                0
            }
            Framing::Eem(_) => eem::write_packet(&mut buf[at..], frame),
            Framing::Rndis { shift, .. } => aurora_usbnet::rndis::write_packet(&mut buf[at..], frame, *shift),
            Framing::Ax772(_) => aurora_usbnet::asix::ax772_tx(buf, frame, mps),
            Framing::Ax179 => aurora_usbnet::asix::ax179_tx(buf, frame, mps),
        };
        tx.lens[fill] += n;
        tx.frames[fill] += 1;
        true
    }

    /// Queues bytes that are already framed (an EEM echo response).
    fn queue_raw(&self, bytes: &[u8]) {
        {
            let mut tx = self.tx.lock();
            let tx = &mut *tx;
            let fill = tx.fill;
            let at = tx.lens[fill];
            if at + bytes.len() + eem::ZLP.len() > tx.bufs.size {
                return;
            }
            tx.bufs.bytes_mut(fill)[at..at + bytes.len()].copy_from_slice(bytes);
            tx.lens[fill] += bytes.len();
        }
        self.wake.wake_all();
    }

    /// Sends what is waiting in the ring, oldest first: every buffer ready
    /// goes to the controller at once (one doorbell, one wait), so the
    /// per-transfer cost is paid once per batch.
    fn flush(&self) {
        const BATCH: usize = 32;
        loop {
            let (first, ready) = {
                let mut tx = self.tx.lock();
                let tx = &mut *tx;
                let count = tx.bufs.len();
                let mut ready = (tx.fill + count - tx.head) % count;
                // The buffer being filled goes too, if there is room to move on.
                let open = tx.lens[tx.fill] != 0 || tx.ncm.as_ref().is_some_and(|b| !b.is_empty());
                if open && ready < BATCH && (tx.fill + 1) % count != tx.head {
                    self.seal(tx);
                    let next = (tx.fill + 1) % count;
                    tx.fill = next;
                    tx.lens[next] = 0;
                    tx.frames[next] = 0;
                    if let Framing::Ncm { layout, .. } = &self.framing {
                        tx.ncm = Some(ncm::Builder::new(*layout));
                    }
                    ready += 1;
                }
                if ready == 0 {
                    return;
                }
                let ready = ready.min(BATCH);
                let parts: Vec<(u64, usize, usize)> = (0..ready)
                    .map(|k| {
                        let i = (tx.head + k) % count;
                        (tx.bufs.phys(i), tx.lens[i], tx.frames[i])
                    })
                    .collect();
                (tx.head, parts)
            };
            self.transmit(&ready);
            let mut tx = self.tx.lock();
            let count = tx.bufs.len();
            for k in 0..ready.len() {
                let i = (first + k) % count;
                tx.lens[i] = 0;
                tx.frames[i] = 0;
            }
            tx.head = (first + ready.len()) % count;
        }
    }

    /// Transfers on the bulk OUT endpoint, each ended by a short packet.
    fn transmit(&self, bufs: &[(u64, usize, usize)]) {
        if self.gone() {
            return;
        }
        let mps = self.bulk_out.max_packet.max(8) as usize;
        // A transfer that fills whole packets needs a zero-length one to end
        // it (ASIX pads instead; an NCM block of the largest size ends itself).
        let zlp = |len: usize| {
            len.is_multiple_of(mps)
                && match &self.framing {
                    Framing::Ax772(_) | Framing::Ax179 => false,
                    Framing::Ncm { layout, .. } => len < layout.max,
                    _ => true,
                }
        };
        let mut parts = Vec::with_capacity(bufs.len() + 2);
        for &(phys, len, _) in bufs.iter().filter(|b| b.1 > 0) {
            parts.push((phys, len));
            if zlp(len) {
                parts.push((phys, 0));
            }
        }
        if parts.is_empty() {
            return;
        }
        match self.bus.transfer_batch(self.dev, &self.bulk_out, &parts, 1000) {
            Ok(_) => {
                let frames: usize = bufs.iter().map(|b| b.2).sum();
                let bytes: usize = bufs.iter().map(|b| b.1).sum();
                self.stats.tx_transfers.fetch_add(bufs.len() as u64, Ordering::Relaxed);
                self.stats.tx_batches.fetch_add(1, Ordering::Relaxed);
                self.stats.tx_frames.fetch_add(frames as u64, Ordering::Relaxed);
                self.stats.tx_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
            }
            Err(code) => {
                self.stats.tx_errors.fetch_add(1, Ordering::Relaxed);
                if code == STALL {
                    self.clear_halt(&self.bulk_out);
                }
            }
        }
    }

    fn clear_halt(&self, ep: &Endpoint) {
        let clear = Setup { request_type: 0x02, request: 1, value: 0, index: ep.address as u16, length: 0 };
        let _ = self.bus.control(self.dev, clear, None);
    }

    // -------------------------------------------------------------- task

    /// The adapter's task: transmit, and act on what the interrupt endpoint said.
    fn run(self: Arc<Self>) {
        let mut last_tick = crate::time::uptime_ms();
        while !self.gone() {
            self.wake.wait(1000, || {
                self.gone() || self.status.lock().is_some() || {
                    let tx = self.tx.lock();
                    tx.lens[tx.head] != 0 || tx.head != tx.fill || tx.ncm.as_ref().is_some_and(|b| !b.is_empty())
                }
            });
            if self.gone() {
                break;
            }
            let report = self.status.lock().take();
            if let Some(r) = report {
                self.on_status(&r);
            }
            self.flush();
            let now = crate::time::uptime_ms();
            if now - last_tick >= 1000 {
                last_tick = now;
                self.tick();
            }
        }
        log!("usbnet", "{}: stopped", self.label);
    }

    /// A report from the interrupt endpoint (in the adapter's task).
    fn on_status(&self, report: &[u8]) {
        self.heard.store(true, Ordering::Release);
        match self.kind {
            Kind::Cdc(Function::Rndis) => rndis::status(self, report),
            Kind::Cdc(_) => cdc::status(self, report),
            Kind::Chip(chip) => asix::status(self, chip, report),
        }
    }

    /// Once a second: devices without link interrupts are asked; an ECM or
    /// NCM function that never reports its link (some don't, despite the
    /// specification) is taken to be connected after a few seconds.
    fn tick(&self) {
        match self.kind {
            Kind::Chip(chip) => asix::tick(self, chip),
            Kind::Cdc(Function::Ecm | Function::Ncm)
                if !self.link()
                    && !self.heard.load(Ordering::Acquire)
                    && crate::time::uptime_ms() - self.started > 3000 =>
            {
                log!("usbnet", "{}: no link notification; assuming connected", self.label);
                self.heard.store(true, Ordering::Release);
                self.set_link(true);
            }
            _ => {}
        }
    }
}

impl crate::net::Nic for UsbNic {
    fn driver(&self) -> String {
        self.label.clone()
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        self.link() && !self.gone()
    }
    fn send(&self, frame: &[u8]) -> bool {
        if self.gone() || !self.link() {
            return false;
        }
        let ok = self.pack(frame);
        if !ok {
            self.stats.tx_dropped.fetch_add(1, Ordering::Relaxed);
        }
        self.wake.wake_all();
        ok
    }
    fn needs_polling(&self) -> bool {
        false
    }
    fn name_prefix(&self) -> &'static str {
        self.prefix
    }
    fn metric(&self) -> u32 {
        if self.prefix == "eth" {
            110
        } else {
            120
        }
    }
    fn speed_mbps(&self) -> u32 {
        self.speed.load(Ordering::Acquire)
    }
}

struct RxPipe(Arc<UsbNic>);

impl PipeHandler for RxPipe {
    fn data(&self, data: &[u8]) {
        self.0.received(data);
    }
}

struct StatusPipe(Arc<UsbNic>);

impl PipeHandler for StatusPipe {
    fn data(&self, data: &[u8]) {
        if data.is_empty() || self.0.gone() {
            return;
        }
        *self.0.status.lock() = Some(data.to_vec());
        self.0.wake.wake_all();
    }
}

fn task_entry(arg: u64) {
    let nic = unsafe { Arc::from_raw(arg as *const UsbNic) };
    nic.run();
}

fn rx_task_entry(arg: u64) {
    let nic = unsafe { Arc::from_raw(arg as *const UsbNic) };
    let Some(buf) = nic.bus.dma(nic.rx_size) else { return };
    let mut failures = 0u32;
    while !nic.gone() {
        match nic.bus.transfer(nic.dev, &nic.bulk_in, buf.phys, nic.rx_size, 5000) {
            Ok(n) => {
                failures = 0;
                nic.received(&buf.bytes()[..n]);
            }
            Err(TIMEOUT) => {}
            Err(code) => {
                nic.stats.rx_errors.fetch_add(1, Ordering::Relaxed);
                if code == STALL {
                    nic.clear_halt(&nic.bulk_in);
                }
                // A device going away fails quickly: don't spin on it.
                failures += 1;
                crate::sched::sleep_ms((10 * failures as u64).min(500));
            }
        }
    }
}

/// Starts an adapter a driver has set up: streams, its task, the interface.
pub fn start(dev: &UsbDevice, s: Setup2) -> Option<&'static str> {
    // One-frame-per-transfer formats get more buffers: a TCP burst is a
    // whole window of frames, and a frame without room is lost.
    let tx_count = if s.framing.aggregates() { 8 } else { 128 };
    let bufs = Slots::new(dev.bus, s.tx_size, tx_count)?;
    let ncm = match &s.framing {
        Framing::Ncm { layout, .. } => Some(ncm::Builder::new(*layout)),
        _ => None,
    };
    // Phones and gadgets make up their (locally administered) address;
    // adapters with a vendor's address are named like other wired cards.
    let prefix = match s.kind {
        Kind::Chip(_) => "eth",
        Kind::Cdc(_) if aurora_usbnet::locally_administered(&s.mac) => "usb",
        Kind::Cdc(_) => "eth",
    };
    let nic = Arc::new(UsbNic {
        bus: dev.bus,
        dev: dev.dev,
        label: s.label,
        kind: s.kind,
        framing: s.framing,
        control_iface: s.control_iface,
        bulk_in: s.bulk_in,
        bulk_out: s.bulk_out,
        mac: s.mac,
        prefix,
        rx_size: s.rx_size,
        usb_speed: dev.pos.speed,
        link: AtomicBool::new(s.link),
        speed: AtomicU32::new(s.speed_mbps),
        gone: AtomicBool::new(false),
        index: AtomicUsize::new(usize::MAX),
        tx: IrqMutex::new(Tx {
            lens: alloc::vec![0; tx_count],
            frames: alloc::vec![0; tx_count],
            bufs,
            head: 0,
            fill: 0,
            ncm,
            sequence: 0,
        }),
        wake: WaitQueue::new(),
        status: IrqMutex::new(None),
        heard: AtomicBool::new(false),
        started: crate::time::uptime_ms(),
        stats: Stats::default(),
        aux: AtomicU32::new(s.aux),
    });
    let index = crate::net::register(nic.clone());
    nic.index.store(index, Ordering::Release);
    // Receive: queued transfers where the controller streams, else a task.
    let rx_count = (64 * 1024 / s.rx_size).clamp(4, 16);
    let streaming = dev.bus.open_stream(dev.dev, &nic.bulk_in, s.rx_size, rx_count, Arc::new(RxPipe(nic.clone())));
    let kernel = crate::mm::vmm::kernel_pml4();
    if !streaming {
        crate::sched::spawn_task("usbnet-rx", rx_task_entry, Arc::into_raw(nic.clone()) as u64, 0, kernel);
    }
    if let Some(ep) = s.interrupt {
        if dev.bus.configure(dev.dev, &[ep], None) {
            dev.bus.open_pipe(dev.dev, &ep, Arc::new(StatusPipe(nic.clone())));
        }
    }
    crate::sched::spawn_task("usbnet", task_entry, Arc::into_raw(nic.clone()) as u64, 0, kernel);
    NICS.lock().push(nic.clone());
    log!(
        "usbnet",
        "{} on {} ({}): {}, link {}, receive {} × {} bytes{}",
        nic.label,
        dev.name,
        dev.pos.speed.name(),
        crate::net::wire::mac_str(nic.mac),
        if nic.link() { "up" } else { "down" },
        if streaming { rx_count } else { 1 },
        nic.rx_size,
        if streaming { "" } else { " (polled)" }
    );
    let gone = nic.clone();
    dev.bus.on_detach(
        dev.dev,
        Box::new(move || {
            gone.gone.store(true, Ordering::Release);
            gone.wake.wake_all();
            crate::net::unregister(gone.index.load(Ordering::Acquire));
            NICS.lock().retain(|n| !Arc::ptr_eq(n, &gone));
        }),
    );
    Some("network")
}

// ------------------------------------------------------------- matching

/// The network function a communication interface carries.
pub fn function(class: u8, subclass: u8, protocol: u8) -> Option<Function> {
    Function::of(class, subclass, protocol)
}

/// A vendor chip we drive.
pub fn chip(dev: &UsbDevice) -> Option<Chip> {
    select::chip(dev.desc.vendor, dev.desc.product)
}

/// Starts the class driver for a communication interface.
pub fn attach(dev: &UsbDevice, iface: &super::Interface) -> Option<&'static str> {
    let f = function(iface.class, iface.subclass, iface.protocol)?;
    let setup = match f {
        Function::Rndis => rndis::bring_up(dev, iface),
        _ => cdc::bring_up(dev, iface, f),
    };
    match setup {
        Some(s) => start(dev, s),
        None => {
            log!("usbnet", "{}: {} did not come up", dev.name, f.name());
            Some("network (failed)")
        }
    }
}

/// Starts a vendor chip's driver on its (vendor-class) interface.
pub fn attach_chip(dev: &UsbDevice, iface: &super::Interface) -> Option<&'static str> {
    let c = chip(dev)?;
    match asix::bring_up(dev, iface, c) {
        Some(s) => start(dev, s),
        None => {
            log!("usbnet", "{}: {} did not come up", dev.name, c.name());
            Some("network (failed)")
        }
    }
}

/// Which configuration of a newly found device to use (index into
/// `configs`): see `aurora_usbnet::select`. The `usbnet` boot option
/// (`ecm`, `ncm`, `eem`, `rndis` or `vendor`) prefers one when offered.
pub fn choose(desc: &DeviceDescriptor, configs: &[super::Configuration]) -> usize {
    if configs.len() < 2 {
        return 0;
    }
    let summaries: Vec<Vec<Iface>> = configs
        .iter()
        .map(|c| {
            c.interfaces
                .iter()
                .filter(|i| i.alternate == 0)
                .map(|i| Iface { class: i.class, subclass: i.subclass, protocol: i.protocol })
                .collect()
        })
        .collect();
    let refs: Vec<&[Iface]> = summaries.iter().map(|v| v.as_slice()).collect();
    let prefer = crate::gui::prefs::boot_option("usbnet").and_then(|p| Prefer::parse(p.trim()));
    select::configuration(desc.vendor, desc.product, &refs, prefer)
}

/// A MAC address made up for a device that has none (CDC-EEM) or gives a
/// broken one: random, locally administered, unicast.
pub fn random_mac() -> [u8; 6] {
    let mut mac = [0u8; 6];
    crate::random::fill(&mut mac);
    mac[0] = (mac[0] & 0xFE) | 0x02;
    mac
}

/// The Realtek chip behind a CDC function, for the label.
pub fn realtek_name(desc: &DeviceDescriptor) -> Option<&'static str> {
    select::REALTEK_CDC_ONLY.iter().find(|r| r.0 == desc.vendor && r.1 == desc.product).map(|r| r.2)
}

/// One line per adapter, for the System Report and `lsusb`-style listings.
pub fn describe() -> Vec<String> {
    NICS.lock()
        .iter()
        .map(|n| {
            let s = &n.stats;
            let rx_t = s.rx_transfers.load(Ordering::Relaxed);
            let tx_t = s.tx_transfers.load(Ordering::Relaxed);
            let per = |frames: u64, transfers: u64| (frames * 10).checked_div(transfers).unwrap_or(0);
            let (rx_per, tx_per) =
                (per(s.rx_frames.load(Ordering::Relaxed), rx_t), per(s.tx_frames.load(Ordering::Relaxed), tx_t));
            alloc::format!(
                "{} {}: link {}, {} Mb/s; rx {} frames in {} transfers ({}.{}/transfer), tx {} in {} ({}.{}/transfer, {} batches), {} dropped, {}/{} rx/tx errors",
                n.label,
                crate::net::wire::mac_str(n.mac),
                if n.link() { "up" } else { "down" },
                n.speed.load(Ordering::Relaxed),
                s.rx_frames.load(Ordering::Relaxed),
                rx_t,
                rx_per / 10,
                rx_per % 10,
                s.tx_frames.load(Ordering::Relaxed),
                tx_t,
                tx_per / 10,
                tx_per % 10,
                s.tx_batches.load(Ordering::Relaxed),
                s.tx_dropped.load(Ordering::Relaxed),
                s.rx_errors.load(Ordering::Relaxed),
                s.tx_errors.load(Ordering::Relaxed),
            )
        })
        .collect()
}

/// The adapters present (for tests).
#[cfg(feature = "ktest")]
pub fn adapters() -> Vec<Arc<UsbNic>> {
    NICS.lock().clone()
}
