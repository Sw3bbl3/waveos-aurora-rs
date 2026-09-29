//! AHCI (SATA) host controller driver: one command slot per port. With MSI
//! the issuing task sleeps until the completion interrupt; without, it polls.

use super::{check_io, register, wait_for, wait_irq, BlockDevice, BlockResult, Dma};
use crate::drivers::pci;
use crate::sync::{Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicU32, Ordering};

// HBA registers
const GHC: usize = 0x04;
const IS: usize = 0x08;
const PI: usize = 0x0C;
const GHC_IE: u32 = 1 << 1;
const GHC_AE: u32 = 1 << 31;
const GHC_HR: u32 = 1 << 0;

// Port registers (offset from the port base)
const P_CLB: usize = 0x00;
const P_FB: usize = 0x08;
const P_IS: usize = 0x10;
const P_IE: usize = 0x14;
const P_CMD: usize = 0x18;
const P_TFD: usize = 0x20;
const P_SIG: usize = 0x24;
const P_SSTS: usize = 0x28;
const P_SCTL: usize = 0x2C;
const P_SERR: usize = 0x30;
const P_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_SUD: u32 = 1 << 1;
const CMD_POD: u32 = 1 << 2;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
const IS_TFES: u32 = 1 << 30;
/// Port interrupts we enable: D2H register / PIO setup / DMA setup / set
/// device bits FIS, descriptor processed, and every error condition.
const IE_MASK: u32 = 0xF | 1 << 5 | 1 << 27 | 1 << 28 | 1 << 29 | IS_TFES;
const TFD_BSY: u32 = 0x80;
const TFD_DRQ: u32 = 0x08;
const TFD_ERR: u32 = 0x01;

const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_EXT: u8 = 0xEA;

/// Bounce buffer size (sectors per command).
const MAX_SECTORS: usize = 128;

/// Controller state shared with the interrupt handler.
struct Hba {
    abar: u64,
    /// PxIS bits the interrupt handler cleared, per port, for the waiter to see.
    seen: [AtomicU32; 32],
    irq: WaitQueue,
    has_irq: bool,
}

impl Hba {
    fn r(&self, reg: usize) -> u32 {
        unsafe { ((self.abar + reg as u64) as *const u32).read_volatile() }
    }
    fn w(&self, reg: usize, v: u32) {
        unsafe { ((self.abar + reg as u64) as *mut u32).write_volatile(v) }
    }
}

/// MSI handler: acknowledges every port that raised an interrupt (PxIS, then
/// IS — otherwise no further message is sent) and wakes the waiting tasks.
fn interrupt(arg: usize) {
    let hba = unsafe { &*(arg as *const Hba) };
    let pending = hba.r(IS);
    for p in 0..32 {
        if pending & (1 << p) != 0 {
            let port_is = hba.abar + 0x100 + p as u64 * 0x80 + P_IS as u64;
            let bits = unsafe { (port_is as *const u32).read_volatile() };
            hba.seen[p].fetch_or(bits, Ordering::AcqRel);
            unsafe { (port_is as *mut u32).write_volatile(bits) };
        }
    }
    hba.w(IS, pending);
    hba.irq.wake_all();
}

struct Port {
    hba: &'static Hba,
    index: usize,
    regs: u64,
    /// Command list (1 KiB) + received FIS (256 B) + one command table.
    mem: Dma,
    buf: Dma,
}

const CLB_OFF: usize = 0;
const FB_OFF: usize = 1024;
const CT_OFF: usize = 2048;

impl Port {
    fn r(&self, reg: usize) -> u32 {
        unsafe { ((self.regs + reg as u64) as *const u32).read_volatile() }
    }
    fn w(&self, reg: usize, v: u32) {
        unsafe { ((self.regs + reg as u64) as *mut u32).write_volatile(v) }
    }

    /// Interrupt status: what is still set plus what the handler already cleared.
    fn status(&self) -> u32 {
        self.r(P_IS) | self.hba.seen[self.index].load(Ordering::Acquire)
    }

    fn stop(&self) -> BlockResult<()> {
        self.w(P_CMD, self.r(P_CMD) & !CMD_ST);
        wait_for(500, || self.r(P_CMD) & CMD_CR == 0)?;
        self.w(P_CMD, self.r(P_CMD) & !CMD_FRE);
        wait_for(500, || self.r(P_CMD) & CMD_FR == 0)
    }

    fn start(&self) -> BlockResult<()> {
        wait_for(500, || self.r(P_CMD) & CMD_CR == 0)?;
        self.w(P_CMD, self.r(P_CMD) | CMD_FRE);
        self.w(P_CMD, self.r(P_CMD) | CMD_ST);
        Ok(())
    }

    /// Issues one ATA command through slot 0 and waits for completion.
    fn command(&self, cmd: u8, lba: u64, count: u16, bytes: usize, write: bool) -> BlockResult<()> {
        wait_for(1000, || self.r(P_TFD) & (TFD_BSY | TFD_DRQ) == 0)?;
        self.w(P_IS, !0);
        self.hba.seen[self.index].store(0, Ordering::Release);
        // Command header 0: CFL = 5 dwords, W bit, PRDTL = 1 (or 0 for no data).
        let prdtl: u32 = if bytes > 0 { 1 } else { 0 };
        let flags = 5 | if write { 1 << 6 } else { 0 } | prdtl << 16;
        self.mem.write::<u32>(CLB_OFF, flags);
        self.mem.write::<u32>(CLB_OFF + 4, 0); // PRDBC
        let ct = self.mem.phys + CT_OFF as u64;
        self.mem.write::<u32>(CLB_OFF + 8, ct as u32);
        self.mem.write::<u32>(CLB_OFF + 12, (ct >> 32) as u32);
        // Command FIS (Register H2D).
        let fis = [
            0x27u8,
            0x80,
            cmd,
            0,
            lba as u8,
            (lba >> 8) as u8,
            (lba >> 16) as u8,
            0x40, // LBA mode
            (lba >> 24) as u8,
            (lba >> 32) as u8,
            (lba >> 40) as u8,
            0,
            count as u8,
            (count >> 8) as u8,
            0,
            0,
        ];
        for (i, b) in fis.iter().enumerate() {
            self.mem.write::<u8>(CT_OFF + i, *b);
        }
        if bytes > 0 {
            let prd = CT_OFF + 0x80;
            self.mem.write::<u32>(prd, self.buf.phys as u32);
            self.mem.write::<u32>(prd + 4, (self.buf.phys >> 32) as u32);
            self.mem.write::<u32>(prd + 8, 0);
            self.mem.write::<u32>(prd + 12, (bytes as u32 - 1) & 0x3F_FFFF);
        }
        self.w(P_CI, 1);
        let irq = self.hba.has_irq.then_some(&self.hba.irq);
        let r = wait_irq(irq, 5000, || self.r(P_CI) & 1 == 0 || self.status() & IS_TFES != 0);
        if r.is_err() || self.status() & IS_TFES != 0 || self.r(P_TFD) & TFD_ERR != 0 {
            log!("ahci", "command {:#x} failed: IS={:#x} TFD={:#x}", cmd, self.status(), self.r(P_TFD));
            return Err(EIO);
        }
        Ok(())
    }
}

pub struct AhciDisk {
    index: usize,
    port: Mutex<Port>,
    sectors: u64,
    model: String,
}

impl BlockDevice for AhciDisk {
    fn name(&self) -> String {
        format!("sata{}", self.index)
    }
    fn sector_size(&self) -> u32 {
        512
    }
    fn sectors(&self) -> u64 {
        self.sectors
    }
    fn describe(&self) -> String {
        format!("AHCI SATA — {}", self.model)
    }
    fn read(&self, lba: u64, buf: &mut [u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let port = self.port.lock();
        for (i, chunk) in buf.chunks_mut(MAX_SECTORS * 512).enumerate() {
            let n = chunk.len() / 512;
            port.command(ATA_READ_DMA_EXT, lba + (i * MAX_SECTORS) as u64, n as u16, chunk.len(), false)?;
            chunk.copy_from_slice(&port.buf.bytes_mut()[..chunk.len()]);
        }
        Ok(())
    }
    fn write(&self, lba: u64, buf: &[u8]) -> BlockResult<()> {
        check_io(self, lba, buf.len())?;
        let port = self.port.lock();
        for (i, chunk) in buf.chunks(MAX_SECTORS * 512).enumerate() {
            let n = chunk.len() / 512;
            port.buf.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
            port.command(ATA_WRITE_DMA_EXT, lba + (i * MAX_SECTORS) as u64, n as u16, chunk.len(), true)?;
        }
        Ok(())
    }
    fn flush(&self) -> BlockResult<()> {
        self.port.lock().command(ATA_FLUSH_EXT, 0, 0, 0, false)
    }
}

fn ata_string(id: &[u8], words: core::ops::Range<usize>) -> String {
    let mut s = String::new();
    for w in words {
        s.push(id[w * 2 + 1] as char);
        s.push(id[w * 2] as char);
    }
    String::from(s.trim())
}

/// Resets the HBA and switches it to AHCI mode.
fn reset_hba(hba: &Hba) -> bool {
    hba.w(GHC, hba.r(GHC) | GHC_AE);
    hba.w(GHC, hba.r(GHC) | GHC_HR);
    if wait_for(1000, || hba.r(GHC) & GHC_HR == 0).is_err() {
        log!("ahci", "HBA reset timed out");
        return false;
    }
    hba.w(GHC, hba.r(GHC) | GHC_AE);
    true
}

/// Brings a port's link up and starts its command engine; false if no ATA
/// disk answers.
fn start_port(port: &Port) -> bool {
    // Ports with nothing attached never establish a link; don't spend time resetting them.
    if wait_for(20, || port.r(P_SSTS) & 0xF != 0).is_err() || port.stop().is_err() {
        return false;
    }
    // Command list and FIS receive area first, so the device's signature FIS lands somewhere.
    let clb = port.mem.phys + CLB_OFF as u64;
    let fb = port.mem.phys + FB_OFF as u64;
    port.w(P_CLB, clb as u32);
    port.w(P_CLB + 4, (clb >> 32) as u32);
    port.w(P_FB, fb as u32);
    port.w(P_FB + 4, (fb >> 32) as u32);
    port.w(P_IE, 0);
    port.w(P_CMD, port.r(P_CMD) | CMD_FRE | CMD_POD | CMD_SUD);
    // Reset the link (COMRESET) and wait for the device to report in.
    port.w(P_SCTL, (port.r(P_SCTL) & !0xF) | 1);
    let t = crate::time::uptime_ms();
    while crate::time::uptime_ms() < t + 2 {
        core::hint::spin_loop();
    }
    port.w(P_SCTL, port.r(P_SCTL) & !0xF);
    let _ = wait_for(300, || port.r(P_SSTS) & 0xF == 3);
    let _ = wait_for(500, || port.r(P_TFD) & (TFD_BSY | TFD_DRQ) == 0);
    port.w(P_SERR, !0);
    port.w(P_IS, !0);
    if port.r(P_SSTS) & 0xF != 3 || port.r(P_SIG) != 0x0000_0101 {
        if port.r(P_SSTS) & 0xF != 0 {
            log!("ahci", "port {}: skipping (SSTS {:#x}, signature {:#x})", port.index, port.r(P_SSTS), port.r(P_SIG));
        }
        return false; // no device, or not an ATA disk (e.g. ATAPI CD-ROM)
    }
    if port.start().is_err() {
        return false;
    }
    if port.hba.has_irq {
        port.w(P_IE, IE_MASK);
    }
    true
}

fn enable_interrupts(hba: &Hba) {
    if hba.has_irq {
        hba.w(IS, !0);
        hba.w(GHC, hba.r(GHC) | GHC_IE);
    }
}

/// Every controller and its disks, for resume after sleep.
static CONTROLLERS: crate::sync::IrqMutex<alloc::vec::Vec<(&'static Hba, alloc::vec::Vec<Arc<AhciDisk>>)>> =
    crate::sync::IrqMutex::new(alloc::vec::Vec::new());

pub fn probe(dev: &pci::Device) {
    dev.enable();
    let Some(abar) = dev.map_bar(5) else {
        log!("ahci", "controller without ABAR");
        return;
    };
    // Interrupts: one MSI for the whole controller.
    let shared: &'static mut Hba = Box::leak(Box::new(Hba {
        abar,
        seen: [const { AtomicU32::new(0) }; 32],
        irq: WaitQueue::new(),
        has_irq: false,
    }));
    if !reset_hba(shared) {
        return;
    }
    let implemented = shared.r(PI);
    shared.has_irq = dev.enable_msi("ahci", interrupt, shared as *const Hba as usize).is_some();
    let shared: &'static Hba = shared;
    let mut disks = alloc::vec::Vec::new();
    for p in 0..32 {
        if implemented & (1 << p) == 0 {
            continue;
        }
        let regs = abar + 0x100 + p as u64 * 0x80;
        let (Some(mem), Some(buf)) = (Dma::new(4096), Dma::new(MAX_SECTORS * 512)) else { return };
        let port = Port { hba: shared, index: p, regs, mem, buf };
        if !start_port(&port) || port.command(ATA_IDENTIFY, 0, 0, 512, false).is_err() {
            continue;
        }
        let id = &port.buf.bytes_mut()[..512];
        let word = |w: usize| u16::from_le_bytes([id[w * 2], id[w * 2 + 1]]) as u64;
        let lba48 = word(83) & (1 << 10) != 0;
        let sectors = if lba48 {
            word(100) | word(101) << 16 | word(102) << 32 | word(103) << 48
        } else {
            word(60) | word(61) << 16
        };
        let model = ata_string(id, 27..47);
        if sectors == 0 {
            continue;
        }
        let disk = Arc::new(AhciDisk { index: disks.len(), port: Mutex::new(port), sectors, model });
        register(disk.clone());
        disks.push(disk);
    }
    enable_interrupts(shared);
    if shared.has_irq {
        log!("ahci", "interrupts via MSI");
    }
    CONTROLLERS.lock().push((shared, disks));
}

/// After sleep: the controller was reset, so bring it and its ports up again.
pub fn resume() {
    let controllers: alloc::vec::Vec<_> = CONTROLLERS.lock().iter().map(|(h, d)| (*h, d.clone())).collect();
    for (hba, disks) in controllers {
        if !reset_hba(hba) {
            continue;
        }
        for disk in disks {
            let port = disk.port.lock();
            if !start_port(&port) {
                log!("ahci", "port {}: disk did not come back after sleep", port.index);
            }
        }
        enable_interrupts(hba);
    }
}
