//! PCI/PCIe bus enumeration and configuration.
//!
//! Configuration space is accessed through ECAM (memory-mapped, from the ACPI
//! MCFG table) when available, else through the legacy 0xCF8/0xCFC ports.

use crate::mm::{paging, phys_to_virt};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::port::Port;

static ECAM: AtomicU64 = AtomicU64::new(0);
static DEVICES: IrqMutex<Vec<Device>> = IrqMutex::new(Vec::new());

pub const CMD_IO: u16 = 1 << 0;
pub const CMD_MEMORY: u16 = 1 << 1;
pub const CMD_BUS_MASTER: u16 = 1 << 2;
pub const CMD_INTX_DISABLE: u16 = 1 << 10;

#[derive(Clone, Copy, Debug)]
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

    /// Enables memory decoding and bus mastering; masks legacy INTx (we poll).
    pub fn enable(&self) {
        let cmd = self.read16(0x04);
        self.write16(0x04, cmd | CMD_MEMORY | CMD_IO | CMD_BUS_MASTER | CMD_INTX_DISABLE);
    }

    /// Maps a memory BAR (uncached) and returns its kernel virtual address.
    pub fn map_bar(&self, index: usize) -> Option<u64> {
        match self.bars[index] {
            Bar::Memory { phys, size } if phys != 0 => Some(paging::map_mmio(phys, size.max(4096))),
            _ => None,
        }
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
