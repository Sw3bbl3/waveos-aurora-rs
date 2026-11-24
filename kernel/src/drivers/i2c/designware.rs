//! DesignWare I²C controllers: finding their registers and interrupt,
//! powering them, and running `aurora_i2c::designware` on them.
//!
//! Two kinds:
//!
//! - **Intel LPSS PCI functions** (Skylake and later; class 0x0C80): the
//!   ACPI node has only `_ADR`, so the registers are BAR 0. LPSS adds
//!   private registers at BAR 0 + 0x200; its RESETS register (+0x04) must
//!   read 0b11 in bits 1:0 (out of reset) for the controller to work. The
//!   function may be in D3 (PCI power management) and is put in D0.
//! - **ACPI-described controllers** (Intel Haswell/Broadwell LPSS in ACPI
//!   mode, Bay Trail, Cherry Trail; AMD `AMDI0010`): the registers and
//!   interrupt are in `_CRS`; Intel's private registers sit at +0x800.
//!
//! Bus timings come from the controller's `SSCN`/`FMCN` when the firmware
//! gives them (they are for its real input clock). Otherwise they are
//! computed for a 216 MHz clock, the fastest any of these controllers uses
//! (Intel's are 100 to 216 MHz, AMD's 150 MHz): on a slower clock the bus
//! just runs slower than 400 kHz, never faster.
//!
//! Interrupts: MSI if the function has it, else its INTx line (PCI), or the
//! `_CRS` interrupt (ACPI); the transfer loop sleeps on it. Without one, it
//! polls (a transfer takes a millisecond or two of a CPU).

use super::{Error, I2cBus};
use crate::acpi::devices::I2cController;
use crate::drivers::pci::{self, Bar};
use crate::sync::{Mutex, WaitQueue};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_acpi_res::{resource::first_memory, Polarity, Resource, Trigger};
use aurora_i2c::designware::{self as dw, reg, timing_for, Config, Controller, Regs, Speed, Timing, Waiter};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

/// The fastest input clock of any supported controller (see the module docs).
const SAFE_CLOCK_HZ: u64 = 216_000_000;
/// LPSS private registers (offset from the controller's registers) and RESETS within them.
const LPSS_PRIVATE_PCI: u64 = 0x200;
const LPSS_PRIVATE_ACPI: u64 = 0x800;
const LPSS_RESETS: u64 = 0x04;

#[derive(Clone, Copy)]
pub struct Mmio(u64);

impl Regs for Mmio {
    fn read(&self, offset: usize) -> u32 {
        unsafe { ((self.0 + offset as u64) as *const u32).read_volatile() }
    }
    fn write(&self, offset: usize, value: u32) {
        unsafe { ((self.0 + offset as u64) as *mut u32).write_volatile(value) }
    }
}

/// What the interrupt handler and the waiting task share.
struct Shared {
    regs: Mmio,
    fired: AtomicBool,
    wq: WaitQueue,
    /// 0 polled, 1 MSI or ACPI interrupt, 2 INTx (live once routed).
    mode: AtomicU8,
    /// The INTx line (a `&'static IntxLine`), once asked for.
    intx: AtomicUsize,
}

impl Shared {
    fn irq_live(&self) -> bool {
        match self.mode.load(Ordering::Acquire) {
            1 => true,
            2 => {
                let p = self.intx.load(Ordering::Acquire) as *const pci::IntxLine;
                !p.is_null() && unsafe { (*p).live() }
            }
            _ => false,
        }
    }
}

fn on_interrupt(arg: usize) {
    let s = unsafe { &*(arg as *const Shared) };
    if Controller::<Mmio>::interrupt(&s.regs) {
        s.fired.store(true, Ordering::Release);
        s.wq.wake_all();
    }
}

fn on_intx(arg: usize) -> bool {
    let s = unsafe { &*(arg as *const Shared) };
    let mine = Controller::<Mmio>::interrupt(&s.regs);
    if mine {
        s.fired.store(true, Ordering::Release);
        s.wq.wake_all();
    }
    mine
}

/// Sleeps on the controller's interrupt, or polls.
struct KernelWaiter<'a> {
    shared: &'a Shared,
    spins: u32,
}

impl Waiter for KernelWaiter<'_> {
    fn now_us(&mut self) -> u64 {
        crate::time::now_ns() / 1000
    }

    fn wait(&mut self, regs: &dyn Regs, events: u32) {
        if events != 0 && self.shared.irq_live() {
            self.shared.fired.store(false, Ordering::Release);
            regs.write(reg::INTR_MASK, events);
            // Already true? Then the handler may have run before the flag was cleared.
            if regs.read(reg::RAW_INTR_STAT) & events != 0 {
                regs.write(reg::INTR_MASK, 0);
                return;
            }
            let s = self.shared;
            s.wq.wait(2, || s.fired.load(Ordering::Acquire));
            return;
        }
        // Polled: a byte takes ~25 µs at 400 kHz; spin briefly, then let others run.
        self.spins += 1;
        if self.spins < 64 {
            for _ in 0..200 {
                core::hint::spin_loop();
            }
        } else {
            crate::sched::yield_now();
        }
    }
}

struct DwBus {
    ctrl: Mutex<Controller<Mmio>>,
    shared: &'static Shared,
    /// Intel LPSS private registers (to take the controller out of reset).
    lpss: Option<Mmio>,
    pci: Option<pci::Device>,
    needs_init: AtomicBool,
    what: String,
}

impl DwBus {
    fn power_up(&self) {
        if let Some(dev) = self.pci {
            set_d0(&dev);
            dev.enable();
            if self.shared.mode.load(Ordering::Relaxed) == 2 {
                // `enable` masks INTx again.
                let cmd = dev.read16(0x04);
                dev.write16(0x04, cmd & !pci::CMD_INTX_DISABLE);
            }
        }
        if let Some(p) = self.lpss {
            let resets = p.read(LPSS_RESETS as usize);
            if resets & 0x3 != 0x3 {
                // Functional and APB resets off, and the DMA's.
                p.write(LPSS_RESETS as usize, 0x7);
            }
        }
    }
}

impl I2cBus for DwBus {
    fn transfer(&self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        let mut c = self.ctrl.lock();
        let mut w = KernelWaiter { shared: self.shared, spins: 0 };
        if self.needs_init.swap(false, Ordering::AcqRel) {
            self.power_up();
            if let Err(e) = c.init(&mut w) {
                self.needs_init.store(true, Ordering::Release);
                return Err(e);
            }
        }
        c.transfer(&mut w, addr, write, read)
    }

    fn resume(&self) {
        self.needs_init.store(true, Ordering::Release);
    }

    fn describe(&self) -> String {
        self.what.clone()
    }
}

/// PCI power management: into D0 if the function is in D3.
fn set_d0(dev: &pci::Device) {
    let Some(&(_, cap)) = dev.capabilities().iter().find(|c| c.0 == 0x01) else { return };
    let pmcsr = dev.read16(cap + 4);
    if pmcsr & 3 != 0 {
        dev.write16(cap + 4, pmcsr & !3);
        crate::sched::sleep_ms(10); // D3hot → D0 recovery time
    }
}

fn timings(c: &I2cController) -> (Timing, Timing, &'static str) {
    let from = |t: [u32; 3]| Timing { hcnt: t[0] as u16, lcnt: t[1] as u16, sda_hold: t[2] };
    match (c.sscn, c.fmcn) {
        (Some(s), Some(f)) => (from(s), from(f), "SSCN/FMCN"),
        (s, f) => (
            s.map_or_else(|| timing_for(SAFE_CLOCK_HZ, Speed::Standard), from),
            f.map_or_else(|| timing_for(SAFE_CLOCK_HZ, Speed::Fast), from),
            if s.is_some() || f.is_some() { "partly SSCN/FMCN" } else { "computed for 216 MHz" },
        ),
    }
}

/// Finds the controller's registers, powers it and sets it up.
pub fn open(c: &I2cController, speed: Speed) -> Option<alloc::sync::Arc<dyn I2cBus>> {
    let (standard, fast, timing_src) = timings(c);
    let cfg = Config { speed, standard, fast, timeout_us: 100_000 };
    // ACPI registers, or a PCI function on bus 0 from `_ADR`.
    let (base, pci_dev, private) = if let Some((phys, len)) = first_memory(&c.dev.resources) {
        let intel = (c.dev.hid.starts_with("INT") || c.dev.hid.starts_with("8086")) && len >= 0x1000;
        let virt = crate::mm::paging::map_mmio(phys, len.max(0x1000));
        (virt, None, intel.then_some(LPSS_PRIVATE_ACPI))
    } else if let Some(adr) = c.dev.adr {
        let (d, f) = ((adr >> 16) as u8, (adr & 0xFFFF) as u8);
        let dev = pci::devices().into_iter().find(|p| p.bus == 0 && p.dev == d && p.func == f)?;
        set_d0(&dev);
        dev.enable();
        let virt = dev.map_bar(0)?;
        let intel = dev.vendor == 0x8086;
        (virt, Some(dev), intel.then_some(LPSS_PRIVATE_PCI))
    } else {
        log!("i2c", "{}: no registers in _CRS and no _ADR", c.dev.path);
        return None;
    };
    let regs = Mmio(base);
    let lpss = private.map(|off| Mmio(base + off));
    if let Some(p) = lpss {
        let resets = p.read(LPSS_RESETS as usize);
        if resets & 0x3 != 0x3 {
            log!("i2c", "{}: LPSS was in reset ({:#x}); releasing it", c.dev.path, resets);
            p.write(LPSS_RESETS as usize, 0x7);
        }
    }
    let Some(id) = Controller::identify(&regs) else {
        log!(
            "i2c",
            "{}: not a DesignWare I2C controller (component type {:#x})",
            c.dev.path,
            regs.read(reg::COMP_TYPE)
        );
        return None;
    };
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        regs,
        fired: AtomicBool::new(false),
        wq: WaitQueue::new(),
        mode: AtomicU8::new(0),
        intx: AtomicUsize::new(0),
    }));
    let arg = shared as *const Shared as usize;
    let irq = match pci_dev {
        Some(dev) => {
            if let Some(v) = dev.enable_msi("i2c", on_interrupt, arg) {
                shared.mode.store(1, Ordering::Release);
                format!("MSI vector {:#x}", v)
            } else if let Some(line) = dev.enable_intx("i2c", on_intx, arg) {
                shared.intx.store(line as *const pci::IntxLine as usize, Ordering::Release);
                shared.mode.store(2, Ordering::Release);
                String::from("INTx")
            } else {
                String::from("polled")
            }
        }
        None => acpi_interrupt(&c.dev.resources, arg).map_or_else(
            || String::from("polled"),
            |(gsi, v)| {
                shared.mode.store(1, Ordering::Release);
                format!("GSI {} vector {:#x}", gsi, v)
            },
        ),
    };
    let mut ctrl = Controller::new(regs, cfg).ok()?;
    let mut w = KernelWaiter { shared, spins: 0 };
    if let Err(e) = ctrl.init(&mut w) {
        log!("i2c", "{}: the controller did not initialize: {:?}", c.dev.path, e);
        return None;
    }
    let what = format!(
        "DesignWare I2C {} v{:#x}, FIFO {}/{}, {} mode, timings {} (SS {}/{}, FS {}/{}), {}",
        match pci_dev {
            Some(d) => format!("PCI {:02x}:{:02x}.{} {:04x}:{:04x}", d.bus, d.dev, d.func, d.vendor, d.device),
            None => format!("{} at MMIO", c.dev.hid),
        },
        id.version,
        id.tx_depth,
        id.rx_depth,
        if speed == Speed::Fast { "fast (400 kHz)" } else { "standard (100 kHz)" },
        timing_src,
        standard.hcnt,
        standard.lcnt,
        fast.hcnt,
        fast.lcnt,
        irq
    );
    Some(Arc::new(DwBus {
        ctrl: Mutex::new(ctrl),
        shared,
        lpss,
        pci: pci_dev,
        needs_init: AtomicBool::new(false),
        what,
    }))
}

/// Routes an ACPI-described controller's interrupt (`Interrupt`, or a legacy `IRQ`).
fn acpi_interrupt(resources: &[Resource], arg: usize) -> Option<(u32, u8)> {
    let (gsi, level, low) = resources.iter().find_map(|r| match r {
        Resource::Interrupt { interrupts, trigger, polarity, source: None, .. } => {
            Some((*interrupts.first()?, *trigger == Trigger::Level, *polarity == Polarity::Low))
        }
        Resource::Irq { irqs, trigger, polarity, .. } => {
            Some((*irqs.first()? as u32, *trigger == Trigger::Level, *polarity == Polarity::Low))
        }
        _ => None,
    })?;
    let vector = crate::arch::irq::alloc("i2c", on_interrupt, arg)?;
    crate::arch::irq::route_gsi(gsi, vector, level, low).then_some((gsi, vector))
}

/// Logs the PCI functions that are DesignWare I²C controllers (whether or
/// not ACPI describes a device on them), for the boot log.
pub fn log_pci_controllers() {
    for d in pci::devices().into_iter().filter(|d| d.class == 0x0C && d.subclass == 0x80 && d.vendor == 0x8086) {
        let Bar::Memory { phys, .. } = d.bars[0] else { continue };
        if phys == 0 {
            continue;
        }
        let Some(virt) = d.map_bar(0) else { continue };
        let comp = Mmio(virt).read(reg::COMP_TYPE);
        if comp == dw::COMP_TYPE {
            log!(
                "i2c",
                "PCI {:02x}:{:02x}.{} {:04x}:{:04x}: Intel LPSS I2C at {:#x}",
                d.bus,
                d.dev,
                d.func,
                d.vendor,
                d.device,
                phys
            );
        }
    }
}
