//! Intel 82576, 82580, I350, I210 and I211 ("igb", 1 Gbit/s; QEMU's `igb`
//! is the 82576) and I225/I226 ("igc", 2.5 Gbit/s). One receive and one
//! transmit queue of 256 advanced descriptors with 2 KiB buffers. Interrupts
//! by MSI-X (the queues and link changes all on vector 0) or MSI; cards
//! without either are polled by the network task.
//!
//! Copper ports only: fibre and SerDes variants would need the PCS set up.

use crate::drivers::block::{wait_for, Dma};
use crate::drivers::pci;
use crate::net::{self, Nic};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicBool, AtomicUsize, Ordering};

const CTRL: u64 = 0x0000;
const STATUS: u64 = 0x0008;
const EECD: u64 = 0x0010;
const EERD: u64 = 0x0014;
const CTRL_EXT: u64 = 0x0018;
const MDIC: u64 = 0x0020;
const ICR: u64 = 0x00C0;
const IMS: u64 = 0x00D0;
const IMC: u64 = 0x00D8;
const RCTL: u64 = 0x0100;
const TCTL: u64 = 0x0400;
const GPIE: u64 = 0x1514;
const EIMS: u64 = 0x1524;
const EIMC: u64 = 0x1528;
const EIAC: u64 = 0x152C;
const EICR: u64 = 0x1580;
const IVAR0: u64 = 0x1700;
const IVAR_MISC: u64 = 0x1740;
const MTA: u64 = 0x5200;
const RAL0: u64 = 0x5400;
const RAH0: u64 = 0x5404;
// Receive and transmit queue 0.
const RDBAL: u64 = 0xC000;
const RDBAH: u64 = 0xC004;
const RDLEN: u64 = 0xC008;
const SRRCTL: u64 = 0xC00C;
const RDH: u64 = 0xC010;
const RDT: u64 = 0xC018;
const RXDCTL: u64 = 0xC028;
const TDBAL: u64 = 0xE000;
const TDBAH: u64 = 0xE004;
const TDLEN: u64 = 0xE008;
const TDH: u64 = 0xE010;
const TDT: u64 = 0xE018;
const TXDCTL: u64 = 0xE028;

const CTRL_GIO_MASTER_DISABLE: u32 = 1 << 2;
const CTRL_SLU: u32 = 1 << 6;
const CTRL_FRCSPD: u32 = 1 << 11;
const CTRL_FRCDPX: u32 = 1 << 12;
const CTRL_RST: u32 = 1 << 26;
/// I225/I226: the whole-device reset (its CTRL.RST bit is reserved).
const CTRL_DEV_RST: u32 = 1 << 29;
const STATUS_LU: u32 = 1 << 1;
const STATUS_GIO_MASTER_ENABLE: u32 = 1 << 19;
const EECD_AUTO_RD: u32 = 1 << 9;
const CTRL_EXT_DRV_LOAD: u32 = 1 << 28;
const MII_BMCR: u32 = 0;
const BMCR_ANRESTART: u16 = 1 << 9;
const BMCR_ISOLATE: u16 = 1 << 10;
const BMCR_PDOWN: u16 = 1 << 11;
const BMCR_ANENABLE: u16 = 1 << 12;
const MDIC_READY: u32 = 1 << 28;
const MDIC_ERROR: u32 = 1 << 30;
const GPIE_NSICR: u32 = 1 << 0;
const GPIE_MSIX_MODE: u32 = 1 << 4;
const GPIE_PBA: u32 = 1 << 31;
const ICR_LSC: u32 = 1 << 2;
const ICR_RXDMT0: u32 = 1 << 4;
const ICR_RXO: u32 = 1 << 6;
const ICR_RXT0: u32 = 1 << 7;
const RCTL_EN: u32 = 1 << 1;
const RCTL_MPE: u32 = 1 << 4;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
const TCTL_CT: u32 = 0x0F << 4;
/// Collision distance for full duplex (igb only; reserved on igc).
const TCTL_COLD: u32 = 0x40 << 12;
const TCTL_RTLC: u32 = 1 << 24;
const QUEUE_ENABLE: u32 = 1 << 25;
/// SRRCTL: 2 KiB buffers (in KiB), advanced one-buffer descriptors.
const SRRCTL_VALUE: u32 = 2 | 1 << 25;

// Advanced receive write-back: status and errors at byte 8, length at 12.
const RX_DD: u32 = 1 << 0;
const RX_EOP: u32 = 1 << 1;
/// CRC, symbol, sequence, carrier-extension and data errors.
const RX_ERRORS: u32 = 0x9700_0000;
// Advanced transmit data descriptor: command, type and length at byte 8.
const TX_DTYP_DATA: u32 = 3 << 20;
const TX_EOP: u32 = 1 << 24;
const TX_IFCS: u32 = 1 << 25;
const TX_RS: u32 = 1 << 27;
const TX_DEXT: u32 = 1 << 29;

const RING: usize = 256;
const BUF: usize = 2048;

#[derive(Clone, Copy, PartialEq)]
enum Family {
    Igb,
    Igc,
}

fn family(id: u16) -> Option<(Family, &'static str)> {
    Some(match id {
        0x10C9 | 0x10E8 | 0x1526 | 0x150A => (Family::Igb, "Intel 82576 (igb)"),
        0x150E => (Family::Igb, "Intel 82580 (igb)"),
        0x1521 => (Family::Igb, "Intel I350 (igb)"),
        0x1533 | 0x157B => (Family::Igb, "Intel I210 (igb)"),
        0x1539 => (Family::Igb, "Intel I211 (igb)"),
        0x15F2 | 0x15F3 | 0x15F8 | 0x0D9F => (Family::Igc, "Intel I225 (igc)"),
        0x125B..=0x125D => (Family::Igc, "Intel I226 (igc)"),
        _ => return None,
    })
}

/// Device ids this driver knows (vendor 8086).
pub fn supported(id: u16) -> bool {
    family(id).is_some()
}

pub fn model(id: u16) -> &'static str {
    family(id).map_or("Intel igb", |f| f.1)
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

pub struct Igb {
    regs: u64,
    device: u16,
    family: Family,
    mac: [u8; 6],
    rx: IrqMutex<Rx>,
    tx: IrqMutex<Tx>,
    iface: AtomicUsize,
    has_irq: AtomicBool,
    msix: AtomicBool,
}

impl Igb {
    fn r(&self, reg: u64) -> u32 {
        unsafe { ((self.regs + reg) as *const u32).read_volatile() }
    }
    fn w(&self, reg: u64, v: u32) {
        unsafe { ((self.regs + reg) as *mut u32).write_volatile(v) }
    }

    /// Reads a register of the internal PHY (address 1) over MDIO.
    fn phy_read(&self, reg: u32) -> Option<u16> {
        self.w(MDIC, reg << 16 | 1 << 21 | 2 << 26);
        self.mdic_wait().map(|v| v as u16)
    }

    fn phy_write(&self, reg: u32, value: u16) -> bool {
        self.w(MDIC, value as u32 | reg << 16 | 1 << 21 | 1 << 26);
        self.mdic_wait().is_some()
    }

    fn mdic_wait(&self) -> Option<u32> {
        for _ in 0..200 {
            crate::drivers::hpet::spin_us(10);
            let v = self.r(MDIC);
            if v & MDIC_READY != 0 {
                return (v & MDIC_ERROR == 0).then_some(v);
            }
        }
        None
    }

    /// Reads a 16-bit word from the NVM (82576/82580/I350 layout).
    fn eeprom(&self, word: u32) -> Option<u16> {
        self.w(EERD, 1 | word << 2);
        wait_for(10, || self.r(EERD) & (1 << 1) != 0).ok()?;
        Some((self.r(EERD) >> 16) as u16)
    }

    /// Resets the MAC (the datasheet's sequence: stop DMA, mask
    /// interrupts, reset, wait for the NVM to reload), then lets the PHY
    /// negotiate the link.
    fn reset(&self) -> bool {
        self.w(CTRL, self.r(CTRL) | CTRL_GIO_MASTER_DISABLE);
        let _ = wait_for(10, || self.r(STATUS) & STATUS_GIO_MASTER_ENABLE == 0);
        self.w(IMC, u32::MAX);
        self.w(EIMC, u32::MAX);
        self.w(RCTL, 0);
        self.w(TCTL, TCTL_PSP);
        crate::sched::sleep_ms(10);
        let bit = if self.family == Family::Igc { CTRL_DEV_RST } else { CTRL_RST };
        self.w(CTRL, self.r(CTRL) | bit);
        crate::sched::sleep_ms(2);
        if wait_for(100, || self.r(CTRL) & bit == 0).is_err() {
            log!("igb", "reset timed out");
            return false;
        }
        // The NVM reloads the address and defaults. (QEMU never sets this
        // bit; real cards do within milliseconds. Either way, go on.)
        let _ = wait_for(100, || self.r(EECD) & EECD_AUTO_RD != 0);
        self.w(IMC, u32::MAX);
        self.w(EIMC, u32::MAX);
        let _ = self.r(ICR);
        // Tell the management firmware a driver is in charge.
        self.w(CTRL_EXT, self.r(CTRL_EXT) | CTRL_EXT_DRV_LOAD);
        // Link up, speed and duplex from auto-negotiation.
        self.w(CTRL, (self.r(CTRL) | CTRL_SLU) & !(CTRL_FRCSPD | CTRL_FRCDPX | CTRL_GIO_MASTER_DISABLE));
        if self.family == Family::Igb {
            // The MAC reset leaves the PHY alone. If firmware powered it down,
            // or the MAC sees no link through it, power it up and (re)start
            // auto-negotiation. (QEMU's 82576 only reports the link in
            // STATUS once a negotiation has run.)
            let link = wait_for(100, || self.r(STATUS) & STATUS_LU != 0).is_ok();
            if let Some(bmcr) = self.phy_read(MII_BMCR) {
                if bmcr & BMCR_PDOWN != 0 || !link {
                    let v = (bmcr & !(BMCR_PDOWN | BMCR_ISOLATE)) | BMCR_ANENABLE | BMCR_ANRESTART;
                    self.phy_write(MII_BMCR, v);
                }
            }
        }
        true
    }

    fn read_mac(&self) -> Option<[u8; 6]> {
        let (lo, hi) = (self.r(RAL0), self.r(RAH0));
        let m = [lo as u8, (lo >> 8) as u8, (lo >> 16) as u8, (lo >> 24) as u8, hi as u8, (hi >> 8) as u8];
        if hi & (1 << 31) != 0 && m != [0; 6] {
            return Some(m);
        }
        if self.family == Family::Igc {
            return None;
        }
        let w: Vec<u16> = (0..3).filter_map(|i| self.eeprom(i)).collect();
        (w.len() == 3 && w.iter().any(|&x| x != 0 && x != 0xFFFF))
            .then(|| [w[0] as u8, (w[0] >> 8) as u8, w[1] as u8, (w[1] >> 8) as u8, w[2] as u8, (w[2] >> 8) as u8])
    }

    /// Address filter and both queues.
    fn program(&self) {
        let m = self.mac;
        self.w(RAL0, u32::from_le_bytes([m[0], m[1], m[2], m[3]]));
        self.w(RAH0, u16::from_le_bytes([m[4], m[5]]) as u32 | 1 << 31);
        for i in 0..128 {
            self.w(MTA + i * 4, 0);
        }
        // Receive queue 0: every descriptor owns a buffer.
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
        self.w(SRRCTL, SRRCTL_VALUE);
        self.w(RDH, 0);
        self.w(RDT, 0);
        self.w(RXDCTL, self.r(RXDCTL) | QUEUE_ENABLE);
        if wait_for(10, || self.r(RXDCTL) & QUEUE_ENABLE != 0).is_err() {
            log!("igb", "receive queue did not enable");
        }
        self.w(RDT, (RING - 1) as u32);
        // All multicast (IPv6 neighbor discovery; the stack filters).
        self.w(RCTL, RCTL_EN | RCTL_MPE | RCTL_BAM | RCTL_SECRC);
        // Transmit queue 0.
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
        self.w(TXDCTL, self.r(TXDCTL) | QUEUE_ENABLE);
        if wait_for(10, || self.r(TXDCTL) & QUEUE_ENABLE != 0).is_err() {
            log!("igb", "transmit queue did not enable");
        }
        let cold = if self.family == Family::Igb { TCTL_COLD } else { 0 };
        self.w(TCTL, TCTL_EN | TCTL_PSP | TCTL_CT | cold | TCTL_RTLC);
    }

    fn enable_interrupts(&self) {
        if !self.has_irq.load(Ordering::Relaxed) {
            return;
        }
        if self.msix.load(Ordering::Relaxed) {
            // Extended (MSI-X) mode: queue causes in EICR, auto-cleared when
            // the message is sent; the other causes (link) through ICR.
            self.w(GPIE, GPIE_NSICR | GPIE_MSIX_MODE | GPIE_PBA);
            self.w(EIAC, 1);
            self.w(IVAR_MISC, 0x80 << 8);
        }
        // Receive and transmit queue 0 on vector 0.
        self.w(IVAR0, 0x8080);
        self.w(EIMS, 1);
        self.w(IMS, ICR_LSC | ICR_RXT0 | ICR_RXDMT0 | ICR_RXO);
    }

    fn collect(&self) {
        let mut frames = Vec::new();
        {
            let mut rx = self.rx.lock();
            for _ in 0..RING {
                let d = rx.next * 16;
                let status = rx.ring.read::<u32>(d + 8);
                if status & RX_DD == 0 {
                    break;
                }
                fence(Ordering::Acquire);
                let len = rx.ring.read::<u16>(d + 12) as usize;
                // One frame per buffer (no jumbo frames).
                if status & RX_EOP != 0 && status & RX_ERRORS == 0 && len <= BUF {
                    let start = rx.next * BUF;
                    frames.push(rx.bufs.bytes()[start..start + len].to_vec());
                }
                // The write-back replaced the buffer address: give it back.
                let phys = rx.bufs.phys + (rx.next * BUF) as u64;
                rx.ring.write::<u64>(d, phys);
                rx.ring.write::<u64>(d + 8, 0);
                let done = rx.next;
                rx.next = (rx.next + 1) % RING;
                fence(Ordering::SeqCst);
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
    let n = unsafe { &*(arg as *const Igb) };
    let cause = n.r(ICR); // reading clears it
    if !n.msix.load(Ordering::Relaxed) {
        n.w(EICR, n.r(EICR));
    }
    if cause & ICR_LSC != 0 {
        log!("igb", "link {}", if n.r(STATUS) & STATUS_LU != 0 { "up" } else { "down" });
    }
    n.collect();
}

impl Nic for Igb {
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
        // Free once the card reported it done (or never used).
        if tx.ring.read::<u32>(d + 8) != 0 && tx.ring.read::<u32>(d + 12) & 1 == 0 {
            return false;
        }
        let start = i * BUF;
        tx.bufs.bytes_mut()[start..start + frame.len()].copy_from_slice(frame);
        let phys = tx.bufs.phys + start as u64;
        let len = frame.len() as u32;
        tx.ring.write::<u64>(d, phys);
        tx.ring.write::<u32>(d + 12, len << 14);
        tx.ring.write::<u32>(d + 8, len | TX_DTYP_DATA | TX_EOP | TX_IFCS | TX_RS | TX_DEXT);
        tx.tail = (i + 1) % RING;
        fence(Ordering::SeqCst);
        self.w(TDT, tx.tail as u32);
        true
    }
    fn poll(&self) {
        if !self.has_irq.load(Ordering::Relaxed) {
            self.collect();
        }
    }
}

struct Handle(&'static Igb);

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

static CARDS: IrqMutex<Vec<&'static Igb>> = IrqMutex::new(Vec::new());

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
    let Some((family, name)) = family(dev.device) else { return };
    dev.enable();
    let Some(regs) = dev.map_bar(0) else { return };
    let (Some(rx_ring), Some(rx_bufs), Some(tx_ring), Some(tx_bufs)) =
        (Dma::new(RING * 16), Dma::new(RING * BUF), Dma::new(RING * 16), Dma::new(RING * BUF))
    else {
        return;
    };
    let mut n = Igb {
        regs,
        device: dev.device,
        family,
        mac: [0; 6],
        rx: IrqMutex::new(Rx { ring: rx_ring, bufs: rx_bufs, next: 0 }),
        tx: IrqMutex::new(Tx { ring: tx_ring, bufs: tx_bufs, tail: 0 }),
        iface: AtomicUsize::new(0),
        has_irq: AtomicBool::new(false),
        msix: AtomicBool::new(false),
    };
    if !n.reset() {
        return;
    }
    let Some(mac) = n.read_mac() else {
        log!("igb", "{}: no MAC address", name);
        return;
    };
    n.mac = mac;
    n.program();
    let nic: &'static Igb = Box::leak(Box::new(n));
    let index = net::register(Arc::new(Handle(nic)));
    nic.iface.store(index, Ordering::Release);
    CARDS.lock().push(nic);
    let msix = dev.capabilities().iter().any(|c| c.0 == pci::CAP_MSIX);
    let how = if dev.enable_msi("igb", interrupt, nic as *const Igb as usize).is_some() {
        nic.msix.store(msix, Ordering::Relaxed);
        nic.has_irq.store(true, Ordering::Relaxed);
        nic.enable_interrupts();
        if msix {
            "interrupts via MSI-X"
        } else {
            "interrupts via MSI"
        }
    } else {
        "no MSI; polled"
    };
    log!("igb", "{}, {}; {}", name, net::wire::mac_str(mac), how);
}
