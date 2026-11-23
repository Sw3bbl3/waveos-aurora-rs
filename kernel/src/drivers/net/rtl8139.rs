//! Realtek RTL8139 (10/100 Mbit/s; QEMU's `rtl8139`, and many older PCI
//! cards). The card writes received frames back to back into one ring
//! buffer, each behind a 4-byte header, and sends from four transmit slots
//! used in turn. It addresses only 32 bits, so every buffer sits below
//! 4 GiB. The chip predates MSI: it interrupts on its legacy INTx line once
//! ACPI has routed that, and is polled by the network task until then.

use crate::drivers::block::{wait_for, Dma};
use crate::drivers::pci;
use crate::net::{self, Nic};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicUsize, Ordering};

const IDR0: u64 = 0x00;
const MAR0: u64 = 0x08;
const TSD0: u64 = 0x10;
const TSAD0: u64 = 0x20;
const RBSTART: u64 = 0x30;
const CR: u64 = 0x37;
const CAPR: u64 = 0x38;
const IMR: u64 = 0x3C;
const ISR: u64 = 0x3E;
const TCR: u64 = 0x40;
const RCR: u64 = 0x44;
const CR9346: u64 = 0x50;
const CONFIG1: u64 = 0x52;
const MSR: u64 = 0x58;

const CR_RST: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const CR_BUFE: u8 = 1 << 0;
/// 9346CR: the configuration registers are writable while set.
const CR9346_UNLOCK: u8 = 0xC0;
/// Media status: link *failure*.
const MSR_LINKB: u8 = 1 << 2;

const ISR_ROK: u16 = 1 << 0;
const ISR_RER: u16 = 1 << 1;
const ISR_RXOVW: u16 = 1 << 4;
const ISR_LINKCHG: u16 = 1 << 5;
const ISR_FOVW: u16 = 1 << 6;
const ISR_SERR: u16 = 1 << 15;
const IRQ_MASK: u16 = ISR_ROK | ISR_RER | ISR_RXOVW | ISR_LINKCHG | ISR_FOVW | ISR_SERR;

/// Transmit status: the card has copied the slot's frame (it may be reused).
const TSD_OWN: u32 = 1 << 13;
/// Start sending once 256 bytes are in the FIFO (in units of 32 bytes).
const TSD_EARLY_TX: u32 = (256 / 32) << 16;

/// Accept broadcast, multicast and our own address; WRAP (a frame that
/// runs past the end of the ring continues linearly into the slack after it);
/// unlimited DMA bursts; a 32 KiB ring; no receive FIFO threshold.
const RCR_VALUE: u32 = 1 << 3 | 1 << 2 | 1 << 1 | 1 << 7 | 7 << 8 | 0b10 << 11 | 7 << 13;
/// Standard inter-frame gap, 1024-byte DMA bursts.
const TCR_VALUE: u32 = 3 << 24 | 6 << 8;

const RX_LEN: usize = 32 * 1024;
/// The ring plus the 16 bytes the card may use past it, plus room for one
/// frame written past the end (WRAP).
const RX_ALLOC: usize = RX_LEN + 16 + 2048;
const TX_SLOTS: usize = 4;
const TX_BUF: usize = 2048;
/// The largest frame a transmit slot takes.
const TX_MAX: usize = 1792;
/// Ethernet's minimum (without the CRC); the chip doesn't pad.
const MIN_FRAME: usize = 60;

pub fn supported(vendor: u16, device: u16) -> bool {
    matches!((vendor, device), (0x10EC, 0x8139) | (0x1186, 0x1300) | (0x1113, 0x1211))
}

struct Rx {
    buf: Dma,
    /// Read position: grows by each frame (header, length, rounded to 4);
    /// the ring offset is this modulo [`RX_LEN`].
    cur: u32,
}

struct Tx {
    bufs: Dma,
    next: usize,
    used: [bool; TX_SLOTS],
}

pub struct Rtl8139 {
    regs: u64,
    mac: [u8; 6],
    rx: IrqMutex<Rx>,
    tx: IrqMutex<Tx>,
    iface: AtomicUsize,
    intx: IrqMutex<Option<&'static pci::IntxLine>>,
}

impl Rtl8139 {
    fn r8(&self, reg: u64) -> u8 {
        unsafe { ((self.regs + reg) as *const u8).read_volatile() }
    }
    fn r16(&self, reg: u64) -> u16 {
        unsafe { ((self.regs + reg) as *const u16).read_volatile() }
    }
    fn r32(&self, reg: u64) -> u32 {
        unsafe { ((self.regs + reg) as *const u32).read_volatile() }
    }
    fn w8(&self, reg: u64, v: u8) {
        unsafe { ((self.regs + reg) as *mut u8).write_volatile(v) }
    }
    fn w16(&self, reg: u64, v: u16) {
        unsafe { ((self.regs + reg) as *mut u16).write_volatile(v) }
    }
    fn w32(&self, reg: u64, v: u32) {
        unsafe { ((self.regs + reg) as *mut u32).write_volatile(v) }
    }

    /// Powers the chip up and resets it, interrupts off.
    fn reset(&self) -> bool {
        // On the first RTL8139 revisions CONFIG1's low bits are SLEEP and
        // PWRDN; on later ones PME and VPD enables. Clearing them is safe on
        // both and wakes the old ones.
        self.w8(CR9346, CR9346_UNLOCK);
        self.w8(CONFIG1, self.r8(CONFIG1) & !0x03);
        self.w8(CR9346, 0);
        self.w16(IMR, 0);
        self.w8(CR, CR_RST);
        if wait_for(100, || self.r8(CR) & CR_RST == 0).is_err() {
            log!("rtl8139", "reset timed out");
            return false;
        }
        self.w16(ISR, 0xFFFF);
        true
    }

    /// The receive ring, the transmit slots, filters and interrupts.
    fn program(&self) {
        {
            let mut rx = self.rx.lock();
            rx.buf.bytes_mut().fill(0);
            rx.cur = 0;
            self.w32(RBSTART, rx.buf.phys as u32);
        }
        // Transmit and receive must be on before their configuration is written.
        self.w8(CR, CR_RE | CR_TE);
        self.w32(RCR, RCR_VALUE);
        self.w32(TCR, TCR_VALUE);
        self.w16(CAPR, 0u16.wrapping_sub(16));
        // All multicast (IPv6 neighbor discovery needs it; the stack filters).
        self.w32(MAR0, u32::MAX);
        self.w32(MAR0 + 4, u32::MAX);
        {
            let mut tx = self.tx.lock();
            tx.next = 0;
            tx.used = [false; TX_SLOTS];
            for i in 0..TX_SLOTS {
                self.w32(TSAD0 + 4 * i as u64, (tx.bufs.phys + (i * TX_BUF) as u64) as u32);
            }
        }
        if self.r8(CR) & (CR_RE | CR_TE) != CR_RE | CR_TE {
            self.w8(CR, CR_RE | CR_TE);
        }
        self.w16(ISR, 0xFFFF);
        self.w16(IMR, IRQ_MASK);
    }

    /// Restarts the receiver after a corrupt header (the datasheet's recovery).
    fn restart_rx(&self, rx: &mut Rx) {
        self.w8(CR, CR_TE);
        let _ = wait_for(10, || self.r8(CR) & CR_RE == 0);
        self.w32(RBSTART, rx.buf.phys as u32);
        self.w8(CR, CR_RE | CR_TE);
        self.w32(RCR, RCR_VALUE);
        rx.cur = 0;
        self.w16(CAPR, 0u16.wrapping_sub(16));
    }

    fn collect(&self) {
        let mut frames = Vec::new();
        {
            let mut rx = self.rx.lock();
            // Bounded, so a flood can't hold the lock forever.
            for _ in 0..256 {
                if self.r8(CR) & CR_BUFE != 0 {
                    break;
                }
                let off = rx.cur as usize % RX_LEN;
                let status = rx.buf.read::<u16>(off);
                let len = rx.buf.read::<u16>(off + 2) as usize;
                if len == 0xFFF0 {
                    break; // the card is still copying this frame in
                }
                // Length includes the 4-byte CRC.
                if status & 1 == 0 || !(18..=1600).contains(&len) {
                    log!("rtl8139", "bad receive header (status {:#06x}, length {}); restarting receiver", status, len);
                    self.restart_rx(&mut rx);
                    break;
                }
                frames.push(rx.buf.bytes()[off + 4..off + len].to_vec());
                rx.cur = (rx.cur + len as u32 + 4 + 3) & !3;
                // CAPR trails the read position by 16 (the chip's convention).
                self.w16(CAPR, rx.cur.wrapping_sub(16) as u16);
            }
        }
        let iface = self.iface.load(Ordering::Acquire);
        for f in frames {
            net::receive(iface, f);
        }
    }

    /// Acknowledges what the card reports; false if it reported nothing.
    fn service(&self) -> bool {
        let isr = self.r16(ISR);
        if isr == 0 || isr == 0xFFFF {
            return false;
        }
        self.w16(ISR, isr);
        if isr & ISR_LINKCHG != 0 {
            log!("rtl8139", "link {}", if self.link_up() { "up" } else { "down" });
        }
        if isr & ISR_SERR != 0 {
            log!("rtl8139", "PCI system error reported");
        }
        self.collect();
        true
    }
}

fn interrupt(arg: usize) -> bool {
    let n = unsafe { &*(arg as *const Rtl8139) };
    n.service()
}

impl Nic for Rtl8139 {
    fn driver(&self) -> String {
        String::from("Realtek RTL8139")
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        self.r8(MSR) & MSR_LINKB == 0
    }
    fn send(&self, frame: &[u8]) -> bool {
        if frame.len() > TX_MAX {
            return false;
        }
        let mut tx = self.tx.lock();
        let i = tx.next;
        let tsd = TSD0 + 4 * i as u64;
        // A slot is free until first used, then once the card has copied it.
        if tx.used[i] && self.r32(tsd) & TSD_OWN == 0 {
            return false;
        }
        let start = i * TX_BUF;
        let len = frame.len().max(MIN_FRAME);
        let buf = &mut tx.bufs.bytes_mut()[start..start + len];
        buf[..frame.len()].copy_from_slice(frame);
        buf[frame.len()..].fill(0);
        fence(Ordering::SeqCst);
        // Writing the size clears OWN, which hands the slot to the card.
        self.w32(tsd, len as u32 | TSD_EARLY_TX);
        tx.used[i] = true;
        tx.next = (i + 1) % TX_SLOTS;
        true
    }
    fn poll(&self) {
        let live = self.intx.lock().is_some_and(|l| l.live());
        if !live && !self.service() {
            // Nothing flagged; the ring's empty bit is the truth anyway.
            self.collect();
        }
    }
}

struct Handle(&'static Rtl8139);

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
    fn needs_polling(&self) -> bool {
        !self.0.intx.lock().is_some_and(|l| l.live())
    }
}

static CARDS: IrqMutex<Vec<&'static Rtl8139>> = IrqMutex::new(Vec::new());

/// After sleep: the cards lost their state.
pub fn resume() {
    let cards = CARDS.lock().clone();
    for c in cards {
        if c.reset() {
            c.program();
        }
    }
}

pub fn probe(dev: &pci::Device) {
    dev.enable();
    // BAR 1 maps the registers into memory (BAR 0 is the same in I/O space).
    let Some(regs) = dev.map_bar(1).or_else(|| dev.map_bar(0)) else {
        log!("rtl8139", "no memory-mapped registers");
        return;
    };
    let (Some(rx_buf), Some(tx_bufs)) = (Dma::new32(RX_ALLOC), Dma::new32(TX_SLOTS * TX_BUF)) else {
        return;
    };
    let mut n = Rtl8139 {
        regs,
        mac: [0; 6],
        rx: IrqMutex::new(Rx { buf: rx_buf, cur: 0 }),
        tx: IrqMutex::new(Tx { bufs: tx_bufs, next: 0, used: [false; TX_SLOTS] }),
        iface: AtomicUsize::new(0),
        intx: IrqMutex::new(None),
    };
    if !n.reset() {
        return;
    }
    // The chip loads its address from the EEPROM at reset.
    let mac: [u8; 6] = core::array::from_fn(|i| n.r8(IDR0 + i as u64));
    if mac == [0; 6] || mac == [0xFF; 6] {
        log!("rtl8139", "no MAC address");
        return;
    }
    n.mac = mac;
    n.program();
    let nic: &'static Rtl8139 = Box::leak(Box::new(n));
    let index = net::register(Arc::new(Handle(nic)));
    nic.iface.store(index, Ordering::Release);
    CARDS.lock().push(nic);
    let line = dev.enable_intx("rtl8139", interrupt, nic as *const Rtl8139 as usize);
    *nic.intx.lock() = line;
    let hw = (nic.r32(TCR) >> 26 & 0x1F) << 2 | (nic.r32(TCR) >> 22 & 0x03);
    log!(
        "rtl8139",
        "Realtek RTL8139 (hardware version {:#04x}), {}; {}",
        hw,
        net::wire::mac_str(mac),
        if line.is_some() { "interrupts by INTx once routed, polled until then" } else { "polled" }
    );
}
