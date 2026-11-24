//! The Generic Interrupt Controller, version 3 (and 4, used as 3), as the
//! ACPI MADT describes it: the distributor (GICD) for shared peripheral
//! interrupts (SPIs, INTIDs 32..1020), one redistributor (GICR) per CPU for
//! its private ones (SGIs 0-15, the CPUs' messages to each other; PPIs
//! 16-31, such as the timer), and the CPU interface through system
//! registers (ICC_*). MSIs go through a GICv2m frame: a device writes an
//! SPI number to its doorbell register (QEMU `virt` with `msi=gicv2m`; HVF's
//! GIC has no ITS).
//!
//! Every interrupt is Group 1 (non-secure), priority 0xA0, routed to the
//! boot CPU (SPIs), and handled with EOI mode 0: one write to EOIR drops
//! the priority and deactivates.
//!
//! GICv2 (memory-mapped CPU interface) is not driven yet: on such a system
//! no interrupts are enabled and drivers poll.

use super::percpu::MAX_CPUS;
use super::regs::{self, read};
use crate::acpi::AcpiInfo;
use crate::mm::paging::map_mmio;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

/// SGIs the kernel sends between CPUs.
pub const SGI_RESCHEDULE: u32 = 0;
pub const SGI_TLB: u32 = 1;
pub const SGI_HALT: u32 = 2;
/// The EL1 virtual timer's PPI when the GTDT doesn't say.
const DEFAULT_VTIMER_PPI: u32 = 27;

const PRIORITY: u8 = 0xA0;
const MAX_INTID: usize = 1020;

// Distributor registers.
const GICD_CTLR: u64 = 0x0;
const GICD_TYPER: u64 = 0x4;
const GICD_IGROUPR: u64 = 0x80;
const GICD_ISENABLER: u64 = 0x100;
const GICD_ICENABLER: u64 = 0x180;
const GICD_ICPENDR: u64 = 0x280;
const GICD_IPRIORITYR: u64 = 0x400;
const GICD_ICFGR: u64 = 0xC00;
const GICD_IROUTER: u64 = 0x6000;
const CTLR_RWP: u32 = 1 << 31;
/// ARE (affinity routing) and EnableGrp1 (with one security state; with
/// two, as seen from the non-secure side, the same bits are ARE_NS and
/// EnableGrp1A).
const CTLR_ENABLE: u32 = 1 << 4 | 1 << 1;

// Redistributor registers (RD_base frame, then SGI_base 64 KiB on).
const GICR_TYPER: u64 = 0x8;
const GICR_WAKER: u64 = 0x14;
const SGI_BASE: u64 = 0x1_0000;
const WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
const WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

// GICv2m MSI frame registers.
const V2M_TYPER: u64 = 0x8;
const V2M_SETSPI: u64 = 0x40;

static GICD: AtomicU64 = AtomicU64::new(0);
static LINES: AtomicU32 = AtomicU32::new(0);
static RDIST: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static VTIMER: AtomicU32 = AtomicU32::new(DEFAULT_VTIMER_PPI);
static READY: AtomicBool = AtomicBool::new(false);
/// Each SPI's handler slot (`irq` vector; 0: none).
static SLOT: [AtomicU8; MAX_INTID] = [const { AtomicU8::new(0) }; MAX_INTID];
/// The GICv2m frame: physical doorbell base, first SPI, SPI count, next free.
static V2M_PHYS: AtomicU64 = AtomicU64::new(0);
static V2M_BASE: AtomicU32 = AtomicU32::new(0);
static V2M_COUNT: AtomicU32 = AtomicU32::new(0);
static V2M_NEXT: AtomicU32 = AtomicU32::new(0);
/// The SPI given to each slot's MSI (0: none yet).
static MSI_SPI: [AtomicU16; 256] = [const { AtomicU16::new(0) }; 256];
/// CPUs as the MADT lists them: MPIDR and redistributor base (0: search).
static CPU_MPIDR: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(u64::MAX) }; MAX_CPUS];
static CPU_GICR: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static GICR_RANGES: spin::Mutex<alloc::vec::Vec<(u64, u64)>> = spin::Mutex::new(alloc::vec::Vec::new());

fn rd32(base: u64, off: u64) -> u32 {
    // SAFETY: GIC registers, device-mapped by `map_mmio`.
    unsafe { ((base + off) as *const u32).read_volatile() }
}

fn wr32(base: u64, off: u64, v: u32) {
    // SAFETY: as `rd32`.
    unsafe { ((base + off) as *mut u32).write_volatile(v) }
}

fn rd64(base: u64, off: u64) -> u64 {
    // SAFETY: as `rd32`.
    unsafe { ((base + off) as *const u64).read_volatile() }
}

fn wr64(base: u64, off: u64, v: u64) {
    // SAFETY: as `rd32`.
    unsafe { ((base + off) as *mut u64).write_volatile(v) }
}

fn wr8(base: u64, off: u64, v: u8) {
    // SAFETY: as `rd32`; the priority registers are byte-accessible.
    unsafe { ((base + off) as *mut u8).write_volatile(v) }
}

fn wait_rwp(gicd: u64) {
    for _ in 0..1_000_000 {
        if rd32(gicd, GICD_CTLR) & CTLR_RWP == 0 {
            return;
        }
        core::hint::spin_loop();
    }
}

/// The routing value for MPIDR `mpidr` (Aff3 moved down next to Aff2..0).
fn affinity(mpidr: u64) -> u64 {
    (mpidr & 0xFF_FFFF) | ((mpidr >> 32) & 0xFF) << 32
}

/// Whether the GIC delivers interrupts (else drivers poll).
pub fn ready() -> bool {
    READY.load(Ordering::Acquire)
}

/// The timer PPI.
pub fn timer_intid() -> u32 {
    VTIMER.load(Ordering::Relaxed)
}

/// Sets the distributor up from the MADT and this CPU's redistributor.
/// False when there is no GICv3 to drive.
pub fn init(acpi: &AcpiInfo) -> bool {
    let g = &acpi.gic;
    if g.gicd == 0 || g.version < 3 {
        log!("gic", "no GICv3 described (version {}); interrupts stay off, drivers poll", g.version);
        return false;
    }
    let gicd = map_mmio(g.gicd, 0x1_0000);
    GICD.store(gicd, Ordering::Relaxed);
    for (i, &(mpidr, gicr, _)) in g.cpus.iter().take(MAX_CPUS).enumerate() {
        CPU_MPIDR[i].store(mpidr, Ordering::Relaxed);
        CPU_GICR[i].store(gicr, Ordering::Relaxed);
    }
    {
        let mut ranges = GICR_RANGES.lock();
        for &(base, len) in &g.gicr_ranges {
            ranges.push((map_mmio(base, len as u64), len as u64));
        }
    }
    if g.virtual_timer_gsi != 0 {
        VTIMER.store(g.virtual_timer_gsi, Ordering::Relaxed);
    }
    let lines = (((rd32(gicd, GICD_TYPER) & 0x1F) + 1) * 32).min(MAX_INTID as u32);
    LINES.store(lines, Ordering::Relaxed);

    wr32(gicd, GICD_CTLR, 0);
    wait_rwp(gicd);
    let me = affinity(super::cpu::mpidr());
    for i in (32..lines).step_by(32) {
        let r = (i / 32) as u64 * 4;
        wr32(gicd, GICD_IGROUPR + r, !0);
        wr32(gicd, GICD_ICENABLER + r, !0);
        wr32(gicd, GICD_ICPENDR + r, !0);
    }
    for i in 32..lines as u64 {
        wr8(gicd, GICD_IPRIORITYR + i, PRIORITY);
        wr64(gicd, GICD_IROUTER + 8 * i, me);
    }
    wait_rwp(gicd);
    wr32(gicd, GICD_CTLR, CTLR_ENABLE);
    wait_rwp(gicd);

    if let Some(&(base, spi, count)) = g.msi_frames.first() {
        let frame = map_mmio(base, 0x1000);
        let (spi, count) = if count != 0 {
            (spi as u32, count as u32)
        } else {
            let t = rd32(frame, V2M_TYPER);
            ((t >> 16) & 0x3FF, t & 0x3FF)
        };
        V2M_PHYS.store(base, Ordering::Relaxed);
        V2M_BASE.store(spi, Ordering::Relaxed);
        V2M_COUNT.store(count, Ordering::Relaxed);
    }
    if !init_cpu(0) {
        return false;
    }
    READY.store(true, Ordering::Release);
    log!(
        "gic",
        "GICv{} distributor at {:#x}, {} interrupt lines, {} CPU(s); MSI: {} SPIs from {} (GICv2m at {:#x}); timer PPI {}",
        g.version,
        g.gicd,
        lines,
        g.cpus.len(),
        V2M_COUNT.load(Ordering::Relaxed),
        V2M_BASE.load(Ordering::Relaxed),
        V2M_PHYS.load(Ordering::Relaxed),
        timer_intid()
    );
    true
}

/// Finds the redistributor of the CPU with MPIDR `mpidr`.
fn find_rdist(cpu: usize, mpidr: u64) -> Option<u64> {
    let want = (mpidr & 0xFF_FFFF) | ((mpidr >> 32) & 0xFF) << 24;
    let listed = CPU_GICR.get(cpu).map_or(0, |g| g.load(Ordering::Relaxed));
    if listed != 0 {
        return Some(map_mmio(listed, 0x2_0000));
    }
    for &(base, len) in GICR_RANGES.lock().iter() {
        let mut rd = base;
        while rd < base + len {
            let typer = rd64(rd, GICR_TYPER);
            if typer >> 32 == want {
                return Some(rd);
            }
            if typer & (1 << 4) != 0 {
                break; // Last
            }
            // GICv4 redistributors with virtual LPIs have two more frames.
            rd += if typer & (1 << 1) != 0 { 0x4_0000 } else { 0x2_0000 };
        }
    }
    None
}

/// Wakes this CPU's redistributor, sets up its SGIs and PPIs and the CPU
/// interface (every CPU, interrupts masked).
pub fn init_cpu(cpu: usize) -> bool {
    let mpidr = super::cpu::mpidr();
    let Some(rd) = find_rdist(cpu, mpidr) else {
        log!("gic", "CPU {}: no redistributor for MPIDR {:#x}", cpu, mpidr);
        return false;
    };
    RDIST[cpu].store(rd, Ordering::Relaxed);
    wr32(rd, GICR_WAKER, rd32(rd, GICR_WAKER) & !WAKER_PROCESSOR_SLEEP);
    for _ in 0..1_000_000 {
        if rd32(rd, GICR_WAKER) & WAKER_CHILDREN_ASLEEP == 0 {
            break;
        }
        core::hint::spin_loop();
    }
    let sgi = rd + SGI_BASE;
    wr32(sgi, GICD_IGROUPR, !0);
    wr32(sgi, GICD_ICENABLER, !0);
    wr32(sgi, GICD_ICPENDR, !0);
    for i in 0..32 {
        wr8(sgi, GICD_IPRIORITYR + i, PRIORITY);
    }
    // PPIs level-triggered (the timer).
    wr32(sgi, GICD_ICFGR + 4, 0);
    let enable = 1 << SGI_RESCHEDULE | 1 << SGI_TLB | 1 << SGI_HALT | 1 << timer_intid();
    wr32(sgi, GICD_ISENABLER, enable);
    // SAFETY: the CPU interface's own registers: system register access
    // (SRE) with bypass off, every priority unmasked, one group-priority
    // bit split, EOI mode 0, group 1 on.
    unsafe {
        let sre = read!("s3_0_c12_c12_5");
        regs::write!("s3_0_c12_c12_5", sre | 0b111);
        regs::isb();
        regs::write!("s3_0_c4_c6_0", 0xF0u64);
        regs::write!("s3_0_c12_c12_3", 0u64);
        regs::write!("s3_0_c12_c12_4", 0u64);
        regs::write!("s3_0_c12_c12_7", 1u64);
    }
    regs::isb();
    true
}

/// Acknowledges the highest-priority pending interrupt (its INTID; 1020 or
/// more: none).
fn acknowledge() -> u32 {
    let v = read!("s3_0_c12_c12_0") as u32 & 0xFF_FFFF;
    regs::dsb_sy();
    v
}

fn end(intid: u32) {
    // SAFETY: ends the interrupt this CPU acknowledged.
    unsafe { regs::write!("s3_0_c12_c12_1", intid as u64) };
    regs::isb();
}

/// An IRQ exception: handles one interrupt (a pending other one raises the
/// exception again once this returns).
pub fn handle() {
    let intid = acknowledge();
    if intid >= 1020 {
        return; // spurious
    }
    let timer = timer_intid();
    match intid {
        SGI_RESCHEDULE => {
            end(intid);
            crate::sched::on_reschedule_ipi();
        }
        SGI_TLB => {
            crate::mm::vmm::service_shootdown();
            end(intid);
        }
        SGI_HALT => super::cpu::halt_forever(),
        _ if intid == timer => {
            // Stop the level first, so it is not taken again once the
            // priority drops (the handler below may switch tasks).
            super::timer::fired();
            end(intid);
            crate::telemetry::irq(crate::telemetry::Irq::Timer);
            crate::timer::interrupt();
        }
        _ => {
            let slot = SLOT.get(intid as usize).map_or(0, |s| s.load(Ordering::Acquire));
            if slot != 0 {
                super::irq::dispatch(slot);
            }
            end(intid);
        }
    }
}

/// Configures SPI `intid` (edge or level) to run slot `vector`, and enables it.
pub fn route(intid: u32, vector: u8, edge: bool) -> bool {
    let gicd = GICD.load(Ordering::Relaxed);
    if !ready() || intid < 32 || intid >= LINES.load(Ordering::Relaxed) {
        return false;
    }
    SLOT[intid as usize].store(vector, Ordering::Release);
    let reg = GICD_ICFGR + (intid as u64 / 16) * 4;
    let shift = (intid % 16) * 2 + 1;
    let cfg = rd32(gicd, reg);
    wr32(gicd, reg, if edge { cfg | 1 << shift } else { cfg & !(1 << shift) });
    wr32(gicd, GICD_ISENABLER + (intid as u64 / 32) * 4, 1 << (intid % 32));
    true
}

/// Disables SPI `intid`.
pub fn mask(intid: u32) {
    let gicd = GICD.load(Ordering::Relaxed);
    if ready() && intid >= 32 && intid < LINES.load(Ordering::Relaxed) {
        wr32(gicd, GICD_ICENABLER + (intid as u64 / 32) * 4, 1 << (intid % 32));
    }
}

/// Whether devices can raise MSIs (a GICv2m frame).
pub fn msi_available() -> bool {
    ready() && V2M_COUNT.load(Ordering::Relaxed) != 0
}

/// The MSI (doorbell address, SPI number) for slot `vector`, giving it an
/// SPI of the v2m frame the first time.
pub fn msi(vector: u8) -> Option<(u64, u32)> {
    if !msi_available() {
        return None;
    }
    let slot = &MSI_SPI[vector as usize];
    let mut spi = slot.load(Ordering::Acquire) as u32;
    if spi == 0 {
        let n = V2M_NEXT.fetch_add(1, Ordering::Relaxed);
        if n >= V2M_COUNT.load(Ordering::Relaxed) {
            return None;
        }
        spi = V2M_BASE.load(Ordering::Relaxed) + n;
        route(spi, vector, true);
        slot.store(spi as u16, Ordering::Release);
    }
    Some((V2M_PHYS.load(Ordering::Relaxed) + V2M_SETSPI, spi))
}

/// Sends SGI `sgi` to the CPU with MPIDR `mpidr`.
pub fn send_sgi(mpidr: u64, sgi: u32) {
    let aff0 = mpidr & 0xFF;
    let v = (sgi as u64) << 24
        | ((mpidr >> 8) & 0xFF) << 16
        | ((mpidr >> 16) & 0xFF) << 32
        | ((mpidr >> 32) & 0xFF) << 48
        | (aff0 >> 4) << 44
        | 1 << (aff0 & 0xF);
    regs::dsb_ish();
    // SAFETY: ICC_SGI1R_EL1 only raises an SGI.
    unsafe { regs::write!("s3_0_c12_c11_5", v) };
    regs::isb();
}

/// Sends SGI `sgi` to every CPU but this one.
pub fn broadcast_sgi(sgi: u32) {
    regs::dsb_ish();
    // SAFETY: IRM = 1: all but self.
    unsafe { regs::write!("s3_0_c12_c11_5", (sgi as u64) << 24 | 1 << 40) };
    regs::isb();
}
