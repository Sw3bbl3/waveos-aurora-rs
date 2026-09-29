//! Intel PRO/1000 (82540EM "e1000", 82545/82546, 82574L "e1000e"): legacy
//! descriptor rings of 256 entries for receive and transmit, 2 KiB buffers.
//! Interrupts by MSI when the card has it, otherwise the stack polls.

use crate::drivers::block::{wait_for, Dma};
use crate::drivers::pci;
use crate::net::{self, Nic};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const CTRL: u64 = 0x0000;
const STATUS: u64 = 0x0008;
const EERD: u64 = 0x0014;
const ICR: u64 = 0x00C0;
const IMS: u64 = 0x00D0;
const IMC: u64 = 0x00D8;
/// 82574: which MSI-X vector each interrupt cause uses.
const IVAR: u64 = 0x00E4;
const RCTL: u64 = 0x0100;
const TCTL: u64 = 0x0400;
const TIPG: u64 = 0x0410;
const RDBAL: u64 = 0x2800;
const RDBAH: u64 = 0x2804;
const RDLEN: u64 = 0x2808;
const RDH: u64 = 0x2810;
const RDT: u64 = 0x2818;
const TDBAL: u64 = 0x3800;
const TDBAH: u64 = 0x3804;
const TDLEN: u64 = 0x3808;
const TDH: u64 = 0x3810;
const TDT: u64 = 0x3818;
const MTA: u64 = 0x5200;
const RAL: u64 = 0x5400;
const RAH: u64 = 0x5404;

const CTRL_ASDE: u32 = 1 << 5;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_RST: u32 = 1 << 26;
const STATUS_LU: u32 = 1 << 1;
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
/// Receive timer, overrun, descriptors low, link status change.
const IRQ_MASK: u32 = 0x80 | 0x40 | 0x10 | 0x04;

const RING: usize = 256;
const BUF: usize = 2048;

/// Device ids this driver knows.
pub fn supported(id: u16) -> bool {
    matches!(id, 0x100E | 0x100F | 0x1010 | 0x1011 | 0x1015 | 0x1026 | 0x1027 | 0x1028 | 0x107C | 0x10D3 | 0x10F6)
}

fn model(id: u16) -> &'static str {
    match id {
        0x10D3 | 0x10F6 => "Intel 82574L (e1000e)",
        0x100E => "Intel 82540EM (e1000)",
        _ => "Intel PRO/1000",
    }
}

struct Rx {
    ring: Dma,
    bufs: Dma,
    next: usize,
}

struct Tx {
    ring: Dma,
    bufs: Dma,
    tail: usize,
}

pub struct E1000 {
    regs: u64,
    device: u16,
    mac: [u8; 6],
    rx: IrqMutex<Rx>,
    tx: IrqMutex<Tx>,
    iface: AtomicUsize,
    has_irq: AtomicBool,
}

impl E1000 {
    fn r(&self, reg: u64) -> u32 {
        unsafe { ((self.regs + reg) as *const u32).read_volatile() }
    }
    fn w(&self, reg: u64, v: u32) {
        unsafe { ((self.regs + reg) as *mut u32).write_volatile(v) }
    }

    /// Reads a 16-bit word from the EEPROM (82540 and 82574 differ in layout).
    fn eeprom(&self, word: u32) -> Option<u16> {
        let e1000e = matches!(self.device, 0x10D3 | 0x10F6);
        let (shift, done) = if e1000e { (2, 1 << 1) } else { (8, 1 << 4) };
        self.w(EERD, 1 | word << shift);
        wait_for(10, || self.r(EERD) & done != 0).ok()?;
        Some((self.r(EERD) >> 16) as u16)
    }

    fn collect(&self) {
        let mut frames = Vec::new();
        {
            let mut rx = self.rx.lock();
            for _ in 0..RING {
                let d = rx.next * 16;
                let status = rx.ring.read::<u8>(d + 12);
                if status & 1 == 0 {
                    break; // not done
                }
                let len = rx.ring.read::<u16>(d + 8) as usize;
                let errors = rx.ring.read::<u8>(d + 13);
                // End of packet, no errors: one frame per buffer (no jumbo frames).
                if status & 2 != 0 && errors == 0 && len <= BUF {
                    let start = rx.next * BUF;
                    frames.push(rx.bufs.bytes_mut()[start..start + len].to_vec());
                }
                rx.ring.write::<u8>(d + 12, 0);
                let done = rx.next;
                rx.next = (rx.next + 1) % RING;
                self.w(RDT, done as u32);
            }
        }
        let iface = self.iface.load(Ordering::Acquire);
        for f in frames {
            net::receive(iface, f);
        }
    }
}

fn interrupt(arg: usize) {
    let n = unsafe { &*(arg as *const E1000) };
    let cause = n.r(ICR); // reading clears it
    if cause & 0x04 != 0 {
        log!("e1000", "link {}", if n.r(STATUS) & STATUS_LU != 0 { "up" } else { "down" });
    }
    n.collect();
}

impl Nic for E1000 {
    fn driver(&self) -> String {
        String::from(model(self.device))
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        self.r(STATUS) & STATUS_LU != 0
    }
    fn send(&self, frame: &[u8]) -> bool {
        if frame.len() > BUF {
            return false;
        }
        let mut tx = self.tx.lock();
        let i = tx.tail;
        let d = i * 16;
        // The slot is free once the card marked it done (or it was never used).
        let cmd = tx.ring.read::<u8>(d + 11);
        if cmd != 0 && tx.ring.read::<u8>(d + 12) & 1 == 0 {
            return false;
        }
        let start = i * BUF;
        tx.bufs.bytes_mut()[start..start + frame.len()].copy_from_slice(frame);
        let phys = tx.bufs.phys + start as u64;
        tx.ring.write::<u64>(d, phys);
        tx.ring.write::<u16>(d + 8, frame.len() as u16);
        tx.ring.write::<u8>(d + 10, 0);
        tx.ring.write::<u8>(d + 12, 0);
        // End of packet, insert FCS, report status.
        tx.ring.write::<u8>(d + 11, 0x01 | 0x02 | 0x08);
        tx.tail = (i + 1) % RING;
        core::sync::atomic::fence(Ordering::SeqCst);
        self.w(TDT, tx.tail as u32);
        true
    }
    fn poll(&self) {
        if !self.has_irq.load(Ordering::Relaxed) {
            self.collect();
        }
    }
}

struct Handle(&'static E1000);

impl Nic for Handle {
    fn driver(&self) -> String {
        self.0.driver()
    }
    fn mac(&self) -> [u8; 6] {
        self.0.mac()
    }
    fn link_up(&self) -> bool {
        self.0.link_up()
    }
    fn send(&self, frame: &[u8]) -> bool {
        self.0.send(frame)
    }
    fn poll(&self) {
        self.0.poll()
    }
}

impl E1000 {
    /// Resets the card: interrupts off, link up with auto-speed.
    fn reset(&self) -> bool {
        // Reset, interrupts off, link up with auto-speed.
        self.w(IMC, u32::MAX);
        self.w(CTRL, self.r(CTRL) | CTRL_RST);
        crate::sched::sleep_ms(2);
        if wait_for(100, || self.r(CTRL) & CTRL_RST == 0).is_err() {
            log!("e1000", "reset timed out");
            return false;
        }
        self.w(IMC, u32::MAX);
        let _ = self.r(ICR);
        self.w(CTRL, (self.r(CTRL) | CTRL_SLU | CTRL_ASDE) & !(1 << 3 | 1 << 31));
        true
    }

    /// The card's address: from the receive-address registers the firmware
    /// loaded, or the EEPROM.
    fn read_mac(&self) -> Option<[u8; 6]> {
        let (lo, hi) = (self.r(RAL), self.r(RAH));
        if hi & (1 << 31) != 0 {
            return Some([lo as u8, (lo >> 8) as u8, (lo >> 16) as u8, (lo >> 24) as u8, hi as u8, (hi >> 8) as u8]);
        }
        let w: Vec<u16> = (0..3).filter_map(|i| self.eeprom(i)).collect();
        (w.len() == 3)
            .then(|| [w[0] as u8, (w[0] >> 8) as u8, w[1] as u8, (w[1] >> 8) as u8, w[2] as u8, (w[2] >> 8) as u8])
    }

    /// Address filter and both rings.
    fn program(&self) {
        let m = self.mac;
        self.w(RAL, u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
        self.w(RAH, u16::from_le_bytes([m[4], m[5]]) as u32 | 1 << 31);
        for i in 0..128 {
            self.w(MTA + i * 4, 0);
        }
        // Receive ring: every descriptor owns a buffer.
        {
            let mut rx = self.rx.lock();
            rx.ring.bytes_mut().fill(0);
            for i in 0..RING {
                rx.ring.write::<u64>(i * 16, rx.bufs.phys + (i * BUF) as u64);
            }
            rx.next = 0;
            self.w(RDBAL, rx.ring.phys as u32);
            self.w(RDBAH, (rx.ring.phys >> 32) as u32);
        }
        self.w(RDLEN, (RING * 16) as u32);
        self.w(RDH, 0);
        self.w(RDT, (RING - 1) as u32);
        self.w(RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
        {
            let mut tx = self.tx.lock();
            tx.ring.bytes_mut().fill(0);
            tx.tail = 0;
            self.w(TDBAL, tx.ring.phys as u32);
            self.w(TDBAH, (tx.ring.phys >> 32) as u32);
        }
        self.w(TDLEN, (RING * 16) as u32);
        self.w(TDH, 0);
        self.w(TDT, 0);
        self.w(TCTL, TCTL_EN | TCTL_PSP | 0x0F << 4 | 0x40 << 12);
        self.w(TIPG, 0x0060_200A);
    }

    fn enable_interrupts(&self) {
        if !self.has_irq.load(Ordering::Relaxed) {
            return;
        }
        if matches!(self.device, 0x10D3 | 0x10F6) {
            // With MSI-X the 82574 routes each cause through IVAR: all to
            // vector 0 (receive and transmit queues 0 and 1, and "other").
            self.w(IVAR, 0x0008_8888);
        }
        // Legacy causes, plus the 82574's per-queue ones (receive queue 0, other).
        self.w(IMS, IRQ_MASK | 1 << 20 | 1 << 24);
    }
}

static CARDS: IrqMutex<Vec<&'static E1000>> = IrqMutex::new(Vec::new());

/// After sleep: the cards were reset.
pub fn resume() {
    let cards = CARDS.lock().clone();
    for c in cards {
        if c.reset() {
            c.program();
            c.enable_interrupts();
        }
    }
}

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(regs) = dev.map_bar(0) else { return };
    let (Some(rx_ring), Some(rx_bufs), Some(tx_ring), Some(tx_bufs)) =
        (Dma::new(RING * 16), Dma::new(RING * BUF), Dma::new(RING * 16), Dma::new(RING * BUF))
    else {
        return;
    };
    let mut n = E1000 {
        regs,
        device: dev.device,
        mac: [0; 6],
        rx: IrqMutex::new(Rx { ring: rx_ring, bufs: rx_bufs, next: 0 }),
        tx: IrqMutex::new(Tx { ring: tx_ring, bufs: tx_bufs, tail: 0 }),
        iface: AtomicUsize::new(0),
        has_irq: AtomicBool::new(false),
    };
    if !n.reset() {
        return;
    }
    let Some(mac) = n.read_mac() else {
        log!("e1000", "no MAC address");
        return;
    };
    n.mac = mac;
    n.program();
    let nic: &'static E1000 = Box::leak(Box::new(n));
    let index = net::register(Arc::new(Handle(nic)));
    nic.iface.store(index, Ordering::Release);
    CARDS.lock().push(nic);
    if dev.enable_msi("e1000", interrupt, nic as *const E1000 as usize).is_some() {
        nic.has_irq.store(true, Ordering::Relaxed);
        nic.enable_interrupts();
        log!("e1000", "{}: interrupts via MSI", model(nic.device));
    } else {
        log!("e1000", "{}: no MSI; polled", model(nic.device));
    }
}
