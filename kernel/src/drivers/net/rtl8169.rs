//! Realtek gigabit and 2.5G Ethernet: RTL8169/8110 (PCI), RTL8168/8111/8411
//! (PCIe; the most common wired adapter on PC motherboards) and RTL8125.
//! They share the "C+" descriptor rings: 16-byte descriptors that the card
//! owns while their OWN bit is set, one ring for receive and one (normal
//! priority) for transmit, 256 entries each with 2 KiB buffers.
//!
//! Written from the register descriptions in Realtek's datasheets and kept
//! deliberately conservative: reset, take the newer chips out of
//! out-of-band (management) mode, read the MAC address, set up both rings,
//! make sure the PHY is powered and negotiating, and watch the link. None of
//! the per-revision PHY tuning or firmware patches other drivers apply is
//! done; the chips work with their power-on PHY defaults, at reduced margins
//! on some revisions. QEMU cannot emulate these chips: untested on real
//! hardware.
//!
//! Interrupts: MSI-X or MSI on PCIe chips, the legacy INTx line on the PCI
//! RTL8169 (whose MSI is unreliable), else polled by the network task.

use crate::drivers::block::{wait_for, Dma};
use crate::drivers::pci;
use crate::net::{self, Nic};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicBool, AtomicU64, AtomicUsize, Ordering};

const IDR0: u64 = 0x00;
const MAR0: u64 = 0x08;
const TNPDS_LO: u64 = 0x20;
const TNPDS_HI: u64 = 0x24;
/// RTL8125: interrupt routing configuration (0 = one vector, no moderation).
const INT_CFG0_8125: u64 = 0x34;
const CR: u64 = 0x37;
/// RTL8169/8168 transmit poll (8 bits); RTL8125 uses [`TPPOLL_8125`].
const TPPOLL: u64 = 0x38;
/// RTL8125: 32-bit interrupt mask and status.
const IMR0_8125: u64 = 0x38;
const ISR0_8125: u64 = 0x3C;
/// RTL8169/8168: 16-bit interrupt mask and status.
const IMR: u64 = 0x3C;
const ISR: u64 = 0x3E;
const TCR: u64 = 0x40;
const RCR: u64 = 0x44;
const CR9346: u64 = 0x50;
const PHYAR: u64 = 0x60;
const PHYSTATUS: u64 = 0x6C;
const TPPOLL_8125: u64 = 0x90;
/// MAC-internal (OCP) register window, RTL8168G and later.
const OCPDR: u64 = 0xB0;
/// PHY (OCP) register window, RTL8168G and later.
const GPHY_OCP: u64 = 0xB8;
const MCU: u64 = 0xD3;
const RMS: u64 = 0xDA;
const INTR_MITIGATE: u64 = 0xE2;
const RDSAR_LO: u64 = 0xE4;
const RDSAR_HI: u64 = 0xE8;
const MTPS: u64 = 0xEC;
const MISC: u64 = 0xF0;

const CR_RST: u8 = 1 << 4;
const CR_RE: u8 = 1 << 3;
const CR_TE: u8 = 1 << 2;
const CR9346_UNLOCK: u8 = 0xC0;
const TPPOLL_NPQ: u8 = 1 << 6;
const PHYSTATUS_LINK: u8 = 1 << 1;
/// TCR: the transmit FIFO is empty (RTL8168G and later).
const TCR_TXCFG_EMPTY: u32 = 1 << 11;
/// TCR: automatic transmit-FIFO sizing (RTL8168E-VL and later).
const TCR_AUTO_FIFO: u32 = 1 << 7;
/// MCU: still in out-of-band (management firmware) mode.
const MCU_NOW_IS_OOB: u8 = 1 << 7;
/// MCU: both FIFOs empty.
const MCU_RXTX_EMPTY: u8 = 1 << 5 | 1 << 4;
/// MCU: the MAC's link list is ready (after an OCP handshake).
const MCU_LINK_LIST_READY: u8 = 1 << 1;
/// MISC: received data is held at the MAC's gate.
const MISC_RXDV_GATED: u32 = 1 << 19;

const IRQ_ROK: u32 = 1 << 0;
const IRQ_RER: u32 = 1 << 1;
const IRQ_RDU: u32 = 1 << 4;
const IRQ_LINKCHG: u32 = 1 << 5;
const IRQ_FOVW: u32 = 1 << 6;
const IRQ_SERR: u32 = 1 << 15;

// Descriptor opts1.
const DESC_OWN: u32 = 1 << 31;
const DESC_EOR: u32 = 1 << 30;
const DESC_FS: u32 = 1 << 29;
const DESC_LS: u32 = 1 << 28;
/// Receive: error summary.
const RX_RES: u32 = 1 << 21;

const RING: usize = 256;
const BUF: usize = 2048;
/// Receive frames longer than this are dropped (1518 plus a VLAN tag).
const RX_MAX: u16 = 1536;
const MIN_FRAME: usize = 60;

const MII_BMCR: u32 = 0;
const MII_BMSR: u32 = 1;
const BMCR_ANRESTART: u16 = 1 << 9;
const BMCR_ISOLATE: u16 = 1 << 10;
const BMCR_PDOWN: u16 = 1 << 11;
const BMCR_ANENABLE: u16 = 1 << 12;
const BMSR_LINK: u16 = 1 << 2;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Gen {
    /// RTL8169/8110 on conventional PCI.
    Pci,
    /// RTL8168/8111 B through F, RTL8411.
    Pcie,
    /// RTL8168G and later (8168H, 8168EP/FP, 8411B): OCP register windows.
    PcieG,
    /// RTL8125 (2.5 Gbit/s).
    R8125,
}

pub fn supported(vendor: u16, device: u16) -> bool {
    matches!(
        (vendor, device),
        (0x10EC, 0x8161 | 0x8167 | 0x8168 | 0x8169 | 0x8125 | 0x3000 | 0x2502 | 0x2600) | (0x1186, 0x4300 | 0x4302)
    )
}

pub fn model(device: u16) -> &'static str {
    match device {
        0x8125 | 0x3000 => "Realtek RTL8125",
        0x8169 | 0x8167 | 0x4300 | 0x4302 => "Realtek RTL8169/8110",
        _ => "Realtek RTL8168/8111",
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

pub struct Rtl8169 {
    regs: u64,
    device: u16,
    gen: Gen,
    /// The chip revision (TCR bits 30:20, masked), for the log.
    xid: u32,
    mac: [u8; 6],
    rx: IrqMutex<Rx>,
    tx: IrqMutex<Tx>,
    iface: AtomicUsize,
    has_irq: AtomicBool,
    intx: IrqMutex<Option<&'static pci::IntxLine>>,
    /// RTL8125: the PHY's link bit, read over OCP at most every 500 ms.
    link: AtomicBool,
    link_checked: AtomicU64,
    /// PHY register accesses are a two-step handshake: one at a time.
    phy: IrqMutex<()>,
}

impl Rtl8169 {
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

    fn ocp_windows(&self) -> bool {
        matches!(self.gen, Gen::PcieG | Gen::R8125)
    }

    fn irq_status(&self) -> u32 {
        if self.gen == Gen::R8125 {
            self.r32(ISR0_8125)
        } else {
            self.r16(ISR) as u32
        }
    }
    fn irq_ack(&self, bits: u32) {
        if self.gen == Gen::R8125 {
            self.w32(ISR0_8125, bits)
        } else {
            self.w16(ISR, bits as u16)
        }
    }
    fn irq_mask(&self, bits: u32) {
        if self.gen == Gen::R8125 {
            self.w32(IMR0_8125, bits)
        } else {
            self.w16(IMR, bits as u16)
        }
    }
    fn irq_enabled_mask(&self) -> u32 {
        let base = IRQ_ROK | IRQ_RER | IRQ_RDU | IRQ_LINKCHG;
        if self.gen == Gen::Pci {
            base | IRQ_FOVW | IRQ_SERR
        } else {
            base
        }
    }

    /// Spins until `done` or about `us` microseconds pass (for register
    /// handshakes that finish in microseconds; safe with interrupts off).
    fn spin_until(&self, us: u64, done: impl Fn() -> bool) -> bool {
        for _ in 0..us.div_ceil(5) {
            if done() {
                return true;
            }
            crate::drivers::hpet::spin_us(5);
        }
        done()
    }

    // ---------------------------------------------------- PHY access

    /// A standard MII register of the internal PHY.
    fn phy_read(&self, reg: u32) -> Option<u16> {
        let _one_at_a_time = self.phy.lock();
        if self.ocp_windows() {
            // Page 0 of the standard registers sits at OCP 0xA400.
            self.w32(GPHY_OCP, (0xA400 + reg * 2) << 15);
            self.spin_until(500, || self.r32(GPHY_OCP) & (1 << 31) != 0).then(|| self.r32(GPHY_OCP) as u16)
        } else {
            self.w32(PHYAR, (reg & 0x1F) << 16);
            let ok = self.spin_until(2000, || self.r32(PHYAR) & (1 << 31) != 0);
            let v = self.r32(PHYAR) as u16;
            // The chip needs a pause between PHY accesses.
            crate::drivers::hpet::spin_us(20);
            ok.then_some(v)
        }
    }

    fn phy_write(&self, reg: u32, value: u16) -> bool {
        let _one_at_a_time = self.phy.lock();
        if self.ocp_windows() {
            self.w32(GPHY_OCP, 1 << 31 | (0xA400 + reg * 2) << 15 | value as u32);
            self.spin_until(500, || self.r32(GPHY_OCP) & (1 << 31) == 0)
        } else {
            self.w32(PHYAR, 1 << 31 | (reg & 0x1F) << 16 | value as u32);
            let ok = self.spin_until(2000, || self.r32(PHYAR) & (1 << 31) == 0);
            crate::drivers::hpet::spin_us(20);
            ok
        }
    }

    fn mac_ocp_read(&self, reg: u32) -> u16 {
        self.w32(OCPDR, reg << 15);
        self.r32(OCPDR) as u16
    }

    fn mac_ocp_write(&self, reg: u32, value: u16) {
        self.w32(OCPDR, 1 << 31 | reg << 15 | value as u32);
    }

    // ---------------------------------------------------- bring-up

    /// RTL8168G and later can be left by the firmware in out-of-band mode,
    /// where the management engine owns the MAC. Hand it to the driver:
    /// quiesce, clear the OOB flag, and redo the link-list handshake.
    fn exit_oob(&self) {
        // Hold received data at the gate while the FIFOs drain ([`program`]
        // opens it again).
        self.w32(MISC, self.r32(MISC) | MISC_RXDV_GATED);
        let _ = self.spin_until(5000, || self.r32(TCR) & TCR_TXCFG_EMPTY != 0);
        let _ = self.spin_until(5000, || self.r8(MCU) & MCU_RXTX_EMPTY == MCU_RXTX_EMPTY);
        self.w8(CR, self.r8(CR) & !(CR_RE | CR_TE));
        crate::sched::sleep_ms(1);
        self.w8(MCU, self.r8(MCU) & !MCU_NOW_IS_OOB);
        self.mac_ocp_write(0xE8DE, self.mac_ocp_read(0xE8DE) & !(1 << 14));
        if !self.spin_until(50_000, || self.r8(MCU) & MCU_LINK_LIST_READY != 0) {
            log!("rtl8169", "link list not ready after leaving OOB mode");
        }
        if self.gen != Gen::R8125 {
            self.mac_ocp_write(0xE8DE, self.mac_ocp_read(0xE8DE) | 1 << 15);
            let _ = self.spin_until(50_000, || self.r8(MCU) & MCU_LINK_LIST_READY != 0);
        }
    }

    fn reset(&self) -> bool {
        self.irq_mask(0);
        if self.ocp_windows() {
            self.exit_oob();
        }
        self.w8(CR, CR_RST);
        if wait_for(100, || self.r8(CR) & CR_RST == 0).is_err() {
            log!("rtl8169", "reset timed out");
            return false;
        }
        self.irq_ack(u32::MAX);
        // The firmware may have left the PHY powered down; it must negotiate.
        if !self.ocp_windows() {
            self.phy_write(0x1F, 0); // standard register page
        }
        if let Some(bmcr) = self.phy_read(MII_BMCR) {
            if bmcr & BMCR_PDOWN != 0 || bmcr & BMCR_ANENABLE == 0 {
                let v = (bmcr & !(BMCR_PDOWN | BMCR_ISOLATE)) | BMCR_ANENABLE | BMCR_ANRESTART;
                self.phy_write(MII_BMCR, v);
            }
        }
        true
    }

    /// Both rings, filters and the receive and transmit configuration.
    fn program(&self) {
        {
            let mut rx = self.rx.lock();
            rx.ring.bytes_mut().fill(0);
            for i in 0..RING {
                let eor = if i == RING - 1 { DESC_EOR } else { 0 };
                rx.ring.write::<u64>(i * 16 + 8, rx.bufs.phys + (i * BUF) as u64);
                rx.ring.write::<u32>(i * 16 + 4, 0);
                rx.ring.write::<u32>(i * 16, DESC_OWN | eor | BUF as u32);
            }
            rx.next = 0;
        }
        {
            let mut tx = self.tx.lock();
            tx.ring.bytes_mut().fill(0);
            tx.ring.write::<u32>((RING - 1) * 16, DESC_EOR);
            tx.tail = 0;
        }
        fence(Ordering::SeqCst);
        let (rx_ring, tx_ring) = (self.rx.lock().ring.phys, self.tx.lock().ring.phys);

        self.w8(CR9346, CR9346_UNLOCK);
        if self.gen == Gen::R8125 {
            self.w8(INT_CFG0_8125, 0);
        } else {
            self.w16(INTR_MITIGATE, 0);
        }
        if self.gen == Gen::Pci {
            self.w8(MTPS, 0x3F); // largest transmit frame, in 128-byte units
        }
        self.w16(RMS, RX_MAX);
        // High halves first (some chips latch the address on the low write).
        self.w32(TNPDS_HI, (tx_ring >> 32) as u32);
        self.w32(TNPDS_LO, tx_ring as u32);
        self.w32(RDSAR_HI, (rx_ring >> 32) as u32);
        self.w32(RDSAR_LO, rx_ring as u32);
        self.w8(CR9346, 0);
        if self.ocp_windows() {
            // Let received data through the MAC's gate.
            self.w32(MISC, self.r32(MISC) & !MISC_RXDV_GATED);
        }
        self.w8(CR, CR_RE | CR_TE);

        // Accept broadcast, multicast and our address; unlimited DMA bursts.
        let accept = 1 << 3 | 1 << 2 | 1 << 1;
        let rcr = match self.gen {
            // No receive FIFO threshold.
            Gen::Pci => 7 << 13 | 7 << 8,
            // 128-byte receive interrupt.
            Gen::Pcie => 1 << 15 | 7 << 8,
            // The same, multiple descriptor fetch, no early-receive interrupts.
            Gen::PcieG => 1 << 15 | 1 << 14 | 1 << 11 | 7 << 8,
            // Default descriptor fetch count.
            Gen::R8125 => 8 << 27 | 7 << 8,
        };
        self.w32(RCR, rcr | accept);
        // Standard inter-frame gap, unlimited DMA bursts.
        let auto_fifo = match self.gen {
            Gen::Pci => false,
            Gen::Pcie => self.xid >= 0x480 || self.xid & 0x7C8 == 0x2C8,
            Gen::PcieG | Gen::R8125 => true,
        };
        self.w32(TCR, 3 << 24 | 7 << 8 | if auto_fifo { TCR_AUTO_FIFO } else { 0 });
        // All multicast (IPv6 neighbor discovery; the stack filters).
        self.w32(MAR0, u32::MAX);
        self.w32(MAR0 + 4, u32::MAX);
        self.irq_ack(u32::MAX);
    }

    fn enable_interrupts(&self) {
        if self.has_irq.load(Ordering::Relaxed) {
            self.irq_mask(self.irq_enabled_mask());
        }
    }

    fn collect(&self) {
        let mut frames = Vec::new();
        {
            let mut rx = self.rx.lock();
            for _ in 0..RING {
                let d = rx.next * 16;
                let opts1 = rx.ring.read::<u32>(d);
                if opts1 & DESC_OWN != 0 {
                    break;
                }
                fence(Ordering::Acquire);
                // Length includes the CRC; one frame per buffer.
                let len = (opts1 & 0x3FFF) as usize;
                let whole = opts1 & (DESC_FS | DESC_LS) == DESC_FS | DESC_LS;
                if whole && opts1 & RX_RES == 0 && (18..=BUF).contains(&len) {
                    let start = rx.next * BUF;
                    frames.push(rx.bufs.bytes()[start..start + len - 4].to_vec());
                }
                let eor = if rx.next == RING - 1 { DESC_EOR } else { 0 };
                let phys = rx.bufs.phys + (rx.next * BUF) as u64;
                rx.ring.write::<u64>(d + 8, phys);
                rx.ring.write::<u32>(d + 4, 0);
                fence(Ordering::SeqCst);
                rx.ring.write::<u32>(d, DESC_OWN | eor | BUF as u32);
                rx.next = (rx.next + 1) % RING;
            }
        }
        let iface = self.iface.load(Ordering::Acquire);
        for f in frames {
            net::receive(iface, f);
        }
    }

    /// Acknowledges what the card reports and notes link changes; `None`
    /// if it reported nothing (or is gone).
    fn events(&self) -> Option<u32> {
        let status = self.irq_status();
        let status = if self.gen == Gen::R8125 { status } else { status & 0xFFFF };
        if status == 0 || status == 0xFFFF || status == u32::MAX {
            return None;
        }
        self.irq_ack(status);
        if status & IRQ_LINKCHG != 0 {
            self.link_checked.store(0, Ordering::Relaxed);
            log!("rtl8169", "link {}", if self.link_up() { "up" } else { "down" });
        }
        if status & IRQ_SERR != 0 {
            log!("rtl8169", "PCI system error reported");
        }
        Some(status)
    }

    /// The interrupt: masks, handles, then unmasks, so anything that
    /// arrived meanwhile raises a fresh interrupt.
    fn service(&self) -> bool {
        self.irq_mask(0);
        let mine = self.events().is_some();
        if mine {
            self.collect();
        }
        self.irq_mask(self.irq_enabled_mask());
        mine
    }
}

fn msi_interrupt(arg: usize) {
    let n = unsafe { &*(arg as *const Rtl8169) };
    n.service();
}

fn intx_interrupt(arg: usize) -> bool {
    let n = unsafe { &*(arg as *const Rtl8169) };
    n.service()
}

impl Nic for Rtl8169 {
    fn driver(&self) -> String {
        String::from(model(self.device))
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn link_up(&self) -> bool {
        if self.gen != Gen::R8125 {
            return self.r8(PHYSTATUS) & PHYSTATUS_LINK != 0;
        }
        let now = crate::time::uptime_ms();
        let last = self.link_checked.load(Ordering::Relaxed);
        if last == 0 || now.saturating_sub(last) >= 500 {
            self.link_checked.store(now.max(1), Ordering::Relaxed);
            // The link bit latches low: the second read is the current state.
            let _ = self.phy_read(MII_BMSR);
            let up = self.phy_read(MII_BMSR).is_some_and(|s| s & BMSR_LINK != 0);
            self.link.store(up, Ordering::Relaxed);
        }
        self.link.load(Ordering::Relaxed)
    }
    fn send(&self, frame: &[u8]) -> bool {
        if frame.len() > BUF {
            return false;
        }
        let mut tx = self.tx.lock();
        let i = tx.tail;
        let d = i * 16;
        if tx.ring.read::<u32>(d) & DESC_OWN != 0 {
            return false; // the card hasn't sent what was here yet
        }
        let start = i * BUF;
        let len = frame.len().max(MIN_FRAME);
        let buf = &mut tx.bufs.bytes_mut()[start..start + len];
        buf[..frame.len()].copy_from_slice(frame);
        buf[frame.len()..].fill(0);
        let eor = if i == RING - 1 { DESC_EOR } else { 0 };
        tx.ring.write::<u64>(d + 8, tx.bufs.phys + start as u64);
        tx.ring.write::<u32>(d + 4, 0);
        fence(Ordering::SeqCst);
        tx.ring.write::<u32>(d, DESC_OWN | eor | DESC_FS | DESC_LS | len as u32);
        tx.tail = (i + 1) % RING;
        fence(Ordering::SeqCst);
        if self.gen == Gen::R8125 {
            self.w8(TPPOLL_8125, 1);
        } else {
            self.w8(TPPOLL, TPPOLL_NPQ);
        }
        true
    }
    fn poll(&self) {
        let intx_live = self.intx.lock().is_some_and(|l| l.live());
        let msi = self.has_irq.load(Ordering::Relaxed) && self.gen != Gen::Pci;
        if !msi && !intx_live {
            // Descriptors are the truth; the status register only says why.
            let _ = self.events();
            self.collect();
        }
    }
}

struct Handle(&'static Rtl8169);

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
        let msi = self.0.has_irq.load(Ordering::Relaxed) && self.0.gen != Gen::Pci;
        !msi && !self.0.intx.lock().is_some_and(|l| l.live())
    }
}

static CARDS: IrqMutex<Vec<&'static Rtl8169>> = IrqMutex::new(Vec::new());

/// After sleep: the cards lost their state.
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
    // The registers are the first memory BAR (BAR 1 on the RTL8169, BAR 2
    // on PCIe chips, whose BAR 0 is the same registers in I/O space).
    let Some(regs) = (0..6).find_map(|i| match dev.bars[i] {
        pci::Bar::Memory { .. } => dev.map_bar(i),
        _ => None,
    }) else {
        log!("rtl8169", "no memory-mapped registers");
        return;
    };
    let tcr = unsafe { ((regs + TCR) as *const u32).read_volatile() };
    if tcr == u32::MAX {
        log!("rtl8169", "{:04x}:{:04x} does not respond", dev.vendor, dev.device);
        return;
    }
    let xid = (tcr >> 20) & 0x7CF;
    let gen = match dev.device {
        0x8125 | 0x3000 => Gen::R8125,
        0x8169 | 0x8167 | 0x4300 | 0x4302 => Gen::Pci,
        _ if xid >= 0x4C0 => Gen::PcieG,
        _ => Gen::Pcie,
    };
    let (Some(rx_ring), Some(rx_bufs), Some(tx_ring), Some(tx_bufs)) =
        (Dma::new(RING * 16), Dma::new(RING * BUF), Dma::new(RING * 16), Dma::new(RING * BUF))
    else {
        return;
    };
    let mut n = Rtl8169 {
        regs,
        device: dev.device,
        gen,
        xid,
        mac: [0; 6],
        rx: IrqMutex::new(Rx { ring: rx_ring, bufs: rx_bufs, next: 0 }),
        tx: IrqMutex::new(Tx { ring: tx_ring, bufs: tx_bufs, tail: 0 }),
        iface: AtomicUsize::new(0),
        has_irq: AtomicBool::new(false),
        intx: IrqMutex::new(None),
        link: AtomicBool::new(false),
        link_checked: AtomicU64::new(0),
        phy: IrqMutex::new(()),
    };
    if !n.reset() {
        return;
    }
    // The chip loads its address from the EEPROM or eFuse at reset.
    let mac: [u8; 6] = core::array::from_fn(|i| n.r8(IDR0 + i as u64));
    if mac == [0; 6] || mac == [0xFF; 6] {
        log!("rtl8169", "no MAC address");
        return;
    }
    n.mac = mac;
    n.program();
    let nic: &'static Rtl8169 = Box::leak(Box::new(n));
    let index = net::register(Arc::new(Handle(nic)));
    nic.iface.store(index, Ordering::Release);
    CARDS.lock().push(nic);
    let arg = nic as *const Rtl8169 as usize;
    let how = if gen == Gen::Pci {
        let line = dev.enable_intx("rtl8169", intx_interrupt, arg);
        *nic.intx.lock() = line;
        if line.is_some() {
            nic.has_irq.store(true, Ordering::Relaxed);
            "interrupts by INTx once routed, polled until then"
        } else {
            "polled"
        }
    } else if dev.enable_msi("rtl8169", msi_interrupt, arg).is_some() {
        nic.has_irq.store(true, Ordering::Relaxed);
        "interrupts via MSI"
    } else {
        "no MSI; polled"
    };
    nic.enable_interrupts();
    log!(
        "rtl8169",
        "{} (chip {:#05x}, {:?}), {}; {} (untested on real hardware)",
        model(dev.device),
        xid,
        gen,
        net::wire::mac_str(mac),
        how
    );
}
