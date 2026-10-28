//! PCI/PCIe bus enumeration and configuration.
//!
//! Configuration space is accessed through ECAM (memory-mapped, from the ACPI
//! MCFG table) when available, else through the legacy 0xCF8/0xCFC ports.

use crate::mm::{paging, phys_to_virt};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::instructions::port::Port;

static ECAM: AtomicU64 = AtomicU64::new(0);
static DEVICES: IrqMutex<Vec<Device>> = IrqMutex::new(Vec::new());

/// An interrupt message a driver set up, re-applied after sleep.
#[derive(Clone, Copy)]
struct MsiRoute {
    dev: (u8, u8, u8),
    /// MSI-X table entry 0 (kernel virtual address), or `None` for MSI.
    msix_entry: Option<u64>,
    cap: u16,
    vector: u8,
}

static MSI_ROUTES: IrqMutex<Vec<MsiRoute>> = IrqMutex::new(Vec::new());

pub const CMD_IO: u16 = 1 << 0;
pub const CMD_MEMORY: u16 = 1 << 1;
pub const CMD_BUS_MASTER: u16 = 1 << 2;
pub const CMD_INTX_DISABLE: u16 = 1 << 10;

pub const CAP_MSI: u8 = 0x05;
pub const CAP_MSIX: u8 = 0x11;

#[derive(Clone, Copy, Debug)]
#[allow(dead_code)] // I/O BARs are recorded for completeness; no driver uses one yet
pub enum Bar {
    None,
    Memory { phys: u64, size: u64 },
    Io { port: u16, size: u32 },
}

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub bars: [Bar; 6],
}

fn legacy_addr(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
    0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (off as u32 & 0xFC)
}

fn ecam_ptr(bus: u8, dev: u8, func: u8, off: u16) -> Option<*mut u32> {
    let base = ECAM.load(Ordering::Relaxed);
    (base != 0).then(|| {
        (phys_to_virt(base + ((bus as u64) << 20 | (dev as u64) << 15 | (func as u64) << 12)) + (off as u64 & !3))
            as *mut u32
    })
}

pub fn read32(bus: u8, dev: u8, func: u8, off: u16) -> u32 {
    match ecam_ptr(bus, dev, func, off) {
        Some(p) => unsafe { p.read_volatile() },
        None => unsafe {
            x86_64::instructions::interrupts::without_interrupts(|| {
                Port::<u32>::new(0xCF8).write(legacy_addr(bus, dev, func, off));
                Port::<u32>::new(0xCFC).read()
            })
        },
    }
}

pub fn write32(bus: u8, dev: u8, func: u8, off: u16, value: u32) {
    match ecam_ptr(bus, dev, func, off) {
        Some(p) => unsafe { p.write_volatile(value) },
        None => unsafe {
            x86_64::instructions::interrupts::without_interrupts(|| {
                Port::<u32>::new(0xCF8).write(legacy_addr(bus, dev, func, off));
                Port::<u32>::new(0xCFC).write(value);
            })
        },
    }
}

impl Device {
    pub fn read32(&self, off: u16) -> u32 {
        read32(self.bus, self.dev, self.func, off)
    }
    pub fn write32(&self, off: u16, v: u32) {
        write32(self.bus, self.dev, self.func, off, v)
    }
    pub fn read16(&self, off: u16) -> u16 {
        (self.read32(off & !3) >> ((off & 2) * 8)) as u16
    }
    pub fn read8(&self, off: u16) -> u8 {
        (self.read32(off & !3) >> ((off & 3) * 8)) as u8
    }
    pub fn write16(&self, off: u16, v: u16) {
        let shift = (off & 2) * 8;
        let old = self.read32(off & !3);
        self.write32(off & !3, (old & !(0xFFFF << shift)) | (v as u32) << shift);
    }

    /// Enables memory decoding and bus mastering; masks legacy INTx (drivers
    /// use MSI/MSI-X, [`Device::enable_intx`], or poll).
    pub fn enable(&self) {
        let cmd = self.read16(0x04);
        self.write16(0x04, cmd | CMD_MEMORY | CMD_IO | CMD_BUS_MASTER | CMD_INTX_DISABLE);
    }

    /// The legacy interrupt pin (INTA–INTD as 1–4; 0 = none).
    pub fn interrupt_pin(&self) -> u8 {
        (self.read32(0x3C) >> 8) as u8
    }

    /// Asks for the device's legacy interrupt (INTx), for controllers without
    /// MSI. `handler(arg)` runs on every interrupt of the (possibly shared,
    /// level-triggered) line and returns whether its device raised it. Which
    /// interrupt-controller input the pin is wired to comes from ACPI (`_PRT`),
    /// which is read after the drivers start, so the line goes live later:
    /// drivers poll until [`IntxLine::live`] says otherwise. `None` when the
    /// device has no interrupt pin.
    pub fn enable_intx(&self, name: &str, handler: fn(usize) -> bool, arg: usize) -> Option<&'static IntxLine> {
        let pin = self.interrupt_pin();
        if !(1..=4).contains(&pin) {
            return None;
        }
        let line: &'static IntxLine = alloc::boxed::Box::leak(alloc::boxed::Box::new(IntxLine {
            live: AtomicBool::new(false),
            gsi: AtomicU32::new(u32::MAX),
        }));
        let cmd = self.read16(0x04);
        self.write16(0x04, cmd & !CMD_INTX_DISABLE);
        let req = IntxRequest {
            dev: (self.bus, self.dev, self.func),
            pin: pin - 1,
            name: alloc::string::String::from(name),
            handler,
            arg,
            line,
        };
        let resolver = *INTX_RESOLVER.lock();
        match resolver {
            Some(r) => route_intx(&req, r),
            None => INTX_PENDING.lock().push(req),
        }
        Some(line)
    }

    /// Maps a memory BAR (uncached) and returns its kernel virtual address.
    pub fn map_bar(&self, index: usize) -> Option<u64> {
        match self.bars[index] {
            Bar::Memory { phys, size } if phys != 0 => Some(paging::map_mmio(phys, size.max(4096))),
            _ => None,
        }
    }

    /// Sends the device's first interrupt message to a newly allocated vector
    /// that runs `handler(arg)` on the boot CPU, preferring MSI-X over MSI.
    /// Returns the vector, or `None` if the device supports neither (the
    /// driver then polls). Further MSI-X table entries stay masked.
    pub fn enable_msi(&self, name: &str, handler: fn(usize), arg: usize) -> Option<u8> {
        let caps = self.capabilities();
        let route = if let Some(&(_, cap)) = caps.iter().find(|c| c.0 == CAP_MSIX) {
            let table = self.read32(cap + 4);
            let base = self.map_bar((table & 7) as usize)?;
            let vector = crate::arch::irq::alloc(name, handler, arg)?;
            MsiRoute { dev: (self.bus, self.dev, self.func), msix_entry: Some(base + (table & !7) as u64), cap, vector }
        } else if let Some(&(_, cap)) = caps.iter().find(|c| c.0 == CAP_MSI) {
            let vector = crate::arch::irq::alloc(name, handler, arg)?;
            MsiRoute { dev: (self.bus, self.dev, self.func), msix_entry: None, cap, vector }
        } else {
            return None;
        };
        program_msi(&route);
        MSI_ROUTES.lock().push(route);
        Some(route.vector)
    }

    /// Iterates the capability list: (capability id, config offset).
    pub fn capabilities(&self) -> Vec<(u8, u16)> {
        let mut out = Vec::new();
        if self.read16(0x06) & (1 << 4) == 0 {
            return out;
        }
        let mut ptr = self.read8(0x34) as u16 & !3;
        let mut guard = 0;
        while ptr != 0 && guard < 48 {
            out.push((self.read8(ptr), ptr));
            ptr = self.read8(ptr + 1) as u16 & !3;
            guard += 1;
        }
        out
    }
}

// ------------------------------------------------------------ legacy INTx

/// A device's legacy interrupt, once routed.
pub struct IntxLine {
    live: AtomicBool,
    gsi: AtomicU32,
}

impl IntxLine {
    /// True once interrupts arrive (until then, and if routing fails, poll).
    pub fn live(&self) -> bool {
        self.live.load(Ordering::Acquire)
    }
}

struct IntxRequest {
    dev: (u8, u8, u8),
    /// 0–3 for INTA–INTD.
    pin: u8,
    name: alloc::string::String,
    handler: fn(usize) -> bool,
    arg: usize,
    line: &'static IntxLine,
}

/// Where a bus-0 device's pin goes: `(device, pin)` → `(gsi, level, active_low)`.
pub type IntxResolver = fn(u8, u8) -> Option<(u32, bool, bool)>;

static INTX_PENDING: IrqMutex<Vec<IntxRequest>> = IrqMutex::new(Vec::new());
static INTX_RESOLVER: IrqMutex<Option<IntxResolver>> = IrqMutex::new(None);

/// A driver's INTx handler: the function, its argument, and the line it watches.
type IntxHandler = (fn(usize) -> bool, usize, &'static IntxLine);

/// One interrupt-controller input shared by any number of INTx handlers.
struct SharedLine {
    gsi: u32,
    handlers: Vec<IntxHandler>,
    /// Interrupts in a row nobody claimed (a stuck line gets masked).
    unclaimed: u32,
}

static SHARED: IrqMutex<Vec<SharedLine>> = IrqMutex::new(Vec::new());

fn shared_dispatch(index: usize) {
    let mut lines = SHARED.lock();
    let Some(l) = lines.get_mut(index) else { return };
    let mut claimed = false;
    for &(h, arg, _) in &l.handlers {
        claimed |= h(arg);
    }
    if claimed {
        l.unclaimed = 0;
        return;
    }
    l.unclaimed += 1;
    if l.unclaimed == 100_000 {
        // Something we don't drive holds the line: mask it, and its drivers poll.
        crate::arch::apic::mask_gsi(l.gsi);
        for &(_, _, line) in &l.handlers {
            line.live.store(false, Ordering::Release);
        }
        log!("pci", "GSI {}: interrupt storm nobody claims; masked (drivers poll)", l.gsi);
    }
}

/// Follows a device's pin up through PCI-to-PCI bridges to bus 0 (the
/// standard swizzle), where the firmware's routing table applies.
fn root_pin(mut dev: (u8, u8, u8), mut pin: u8) -> Option<(u8, u8)> {
    for _ in 0..8 {
        if dev.0 == 0 {
            return Some((dev.1, pin));
        }
        pin = (pin + dev.1) % 4;
        let bridge = devices().into_iter().find(|b| {
            b.class == 0x06 && b.subclass == 0x04 && (read32(b.bus, b.dev, b.func, 0x18) >> 8) as u8 == dev.0
        })?;
        dev = (bridge.bus, bridge.dev, bridge.func);
    }
    None
}

fn route_intx(req: &IntxRequest, resolver: IntxResolver) {
    let Some((slot, pin)) = root_pin(req.dev, req.pin) else {
        log!("pci", "{}: no route to the root bus for its interrupt; polling", req.name);
        return;
    };
    let Some((gsi, level, active_low)) = resolver(slot, pin) else {
        log!("pci", "{}: the firmware gives no route for INT{}; polling", req.name, (b'A' + pin) as char);
        return;
    };
    let mut lines = SHARED.lock();
    let index = match lines.iter().position(|l| l.gsi == gsi) {
        Some(i) => i,
        None => {
            let index = lines.len();
            let Some(vector) = crate::arch::irq::alloc("pci-intx", shared_dispatch, index) else {
                log!("pci", "{}: no interrupt vector left; polling", req.name);
                return;
            };
            lines.push(SharedLine { gsi, handlers: Vec::new(), unclaimed: 0 });
            if !crate::arch::apic::route_gsi(gsi, vector, level, active_low) {
                log!("pci", "{}: GSI {} is not on the I/O APIC; polling", req.name, gsi);
                return;
            }
            index
        }
    };
    lines[index].handlers.push((req.handler, req.arg, req.line));
    drop(lines);
    req.line.gsi.store(gsi, Ordering::Relaxed);
    req.line.live.store(true, Ordering::Release);
    // A device already holding its line up from before (while the pin was
    // masked) would wait forever on some interrupt controllers, which only
    // see a new rising level: run its handler once so it lets go.
    x86_64::instructions::interrupts::without_interrupts(|| (req.handler)(req.arg));
    log!("pci", "{}: INT{} → GSI {}", req.name, (b'A' + req.pin) as char, gsi);
}

/// Called by the ACPI task once the firmware's PCI routing is known: routes
/// the lines drivers asked for so far, and later ones as they ask.
pub fn set_intx_resolver(resolver: IntxResolver) {
    *INTX_RESOLVER.lock() = Some(resolver);
    let pending: Vec<IntxRequest> = INTX_PENDING.lock().drain(..).collect();
    for req in &pending {
        route_intx(req, resolver);
    }
}

/// Points a device's interrupt message at its vector on the boot CPU.
fn program_msi(r: &MsiRoute) {
    let (b, d, f) = r.dev;
    let rd16 = |off: u16| (read32(b, d, f, off & !3) >> ((off & 2) * 8)) as u16;
    let wr16 = |off: u16, v: u16| {
        let shift = (off & 2) * 8;
        let old = read32(b, d, f, off & !3);
        write32(b, d, f, off & !3, (old & !(0xFFFF << shift)) | (v as u32) << shift);
    };
    let address = 0xFEE0_0000 | crate::arch::apic::boot_apic_id() << 12;
    let cap = r.cap;
    match r.msix_entry {
        Some(entry) => {
            let e = entry as *mut u32;
            unsafe {
                e.write_volatile(address);
                e.add(1).write_volatile(0);
                e.add(2).write_volatile(r.vector as u32);
                e.add(3).write_volatile(0); // unmasked
            }
            // Enable MSI-X and clear the function-wide mask.
            wr16(cap + 2, (rd16(cap + 2) | 1 << 15) & !(1 << 14));
        }
        None => {
            let control = rd16(cap + 2);
            write32(b, d, f, cap + 4, address);
            let data = if control & (1 << 7) != 0 {
                write32(b, d, f, cap + 8, 0); // upper address (64-bit capable)
                cap + 12
            } else {
                cap + 8
            };
            wr16(data, r.vector as u16);
            // One message (MME = 0), enabled.
            wr16(cap + 2, (control & !(0x7 << 4)) | 1);
        }
    }
}

/// A function's address (bus, device, function) and its first 64 bytes of configuration space.
pub type SavedConfig = ((u8, u8, u8), [u32; 16]);

/// The first 64 bytes of every device's configuration space (BARs, command,
/// interrupt line), saved before sleep: devices lose it when powered off.
pub fn save() -> Vec<SavedConfig> {
    devices()
        .iter()
        .map(|d| ((d.bus, d.dev, d.func), core::array::from_fn(|i| read32(d.bus, d.dev, d.func, i as u16 * 4))))
        .collect()
}

/// Restores what `save` recorded, then the interrupt messages.
pub fn restore(saved: &[SavedConfig]) {
    for &((b, d, f), regs) in saved {
        // BARs and the rest with decoding off, then the command register.
        write32(b, d, f, 0x04, regs[1] & !0x7);
        for i in [3usize, 4, 5, 6, 7, 8, 9, 15] {
            write32(b, d, f, i as u16 * 4, regs[i]);
        }
        write32(b, d, f, 0x04, regs[1] & 0xFFFF);
    }
    for r in MSI_ROUTES.lock().iter() {
        program_msi(r);
    }
}

fn probe_bars(bus: u8, dev: u8, func: u8) -> [Bar; 6] {
    let mut bars = [Bar::None; 6];
    let cmd = read32(bus, dev, func, 0x04);
    // Disable decoding while sizing BARs.
    write32(bus, dev, func, 0x04, cmd & !0x3);
    let mut i = 0;
    while i < 6 {
        let off = 0x10 + i as u16 * 4;
        let orig = read32(bus, dev, func, off);
        write32(bus, dev, func, off, 0xFFFF_FFFF);
        let mask = read32(bus, dev, func, off);
        write32(bus, dev, func, off, orig);
        if orig & 1 == 1 {
            let size = (!(mask & !0x3)).wrapping_add(1) & 0xFFFF;
            if mask != 0 {
                bars[i] = Bar::Io { port: (orig & !0x3) as u16, size };
            }
            i += 1;
            continue;
        }
        let is64 = (orig >> 1) & 0x3 == 0x2;
        let mut phys = (orig & !0xF) as u64;
        let mut size_mask = (mask & !0xF) as u64;
        if is64 && i < 5 {
            let off_hi = off + 4;
            let orig_hi = read32(bus, dev, func, off_hi);
            write32(bus, dev, func, off_hi, 0xFFFF_FFFF);
            let mask_hi = read32(bus, dev, func, off_hi);
            write32(bus, dev, func, off_hi, orig_hi);
            phys |= (orig_hi as u64) << 32;
            size_mask |= (mask_hi as u64) << 32;
        } else {
            size_mask |= 0xFFFF_FFFF_0000_0000;
        }
        if mask != 0 {
            bars[i] = Bar::Memory { phys, size: (!size_mask).wrapping_add(1) };
        }
        i += if is64 { 2 } else { 1 };
    }
    write32(bus, dev, func, 0x04, cmd);
    bars
}

pub fn init(ecam: Option<(u64, u8, u8)>) {
    let (first, last) = match ecam {
        Some((base, first, last)) => {
            let size = ((last as u64 - first as u64 + 1) << 20).max(1 << 20);
            // ECAM lives below 4 GiB on PCs, inside the physical window; map_mmio covers other cases.
            paging::map_mmio(base, size);
            ECAM.store(base - ((first as u64) << 20), Ordering::Relaxed);
            (first, last)
        }
        None => (0, 255),
    };
    let mut found = Vec::new();
    for bus in first..=last {
        for dev in 0..32u8 {
            for func in 0..8u8 {
                let id = read32(bus, dev, func, 0);
                if id & 0xFFFF == 0xFFFF {
                    if func == 0 {
                        break;
                    }
                    continue;
                }
                let class = read32(bus, dev, func, 0x08);
                let header = (read32(bus, dev, func, 0x0C) >> 16) as u8;
                let bars = if header & 0x7F == 0 { probe_bars(bus, dev, func) } else { [Bar::None; 6] };
                found.push(Device {
                    bus,
                    dev,
                    func,
                    vendor: id as u16,
                    device: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                    prog_if: (class >> 8) as u8,
                    bars,
                });
                if func == 0 && header & 0x80 == 0 {
                    break; // single-function device
                }
            }
        }
    }
    log!("pci", "{} device(s) via {}", found.len(), if ecam.is_some() { "ECAM" } else { "legacy config ports" });
    for d in &found {
        log!(
            "pci",
            "  {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x}.{:02x}",
            d.bus,
            d.dev,
            d.func,
            d.vendor,
            d.device,
            d.class,
            d.subclass,
            d.prog_if
        );
    }
    *DEVICES.lock() = found;
}

pub fn devices() -> Vec<Device> {
    DEVICES.lock().clone()
}
