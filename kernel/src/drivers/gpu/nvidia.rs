//! Nvidia GeForce (Maxwell to Ada): detection only.
//!
//! There is no native Nvidia display driver. The display engine of these
//! GPUs is driven through method channels in memory (EVO on Maxwell and
//! Pascal, NVDisplay from Volta on), and from Turing on Nvidia's own
//! driver runs it through the GSP firmware; a takeover that can't harm the
//! machine needs more than the public documentation gives (see
//! docs/developer/nvidia-display.md for what a real driver needs). So the
//! firmware framebuffer stays in charge, with the display core's software
//! pointer, and this only says in the log what the GPU is: the device id's
//! architecture, and `NV_PMC_BOOT_0` (BAR 0 offset 0, the identification
//! register, read once) for the exact chip.
//!
//! `nvidia=off` in boot.conf skips even that read.

use aurora_nvgfx as nv;

/// Logs every Nvidia display controller. Never binds one.
pub fn probe(boot_fb: Option<bootinfo::Framebuffer>) {
    let option = crate::gui::prefs::boot_option("nvidia");
    for d in crate::drivers::pci::devices()
        .into_iter()
        .filter(|d| d.vendor == nv::VENDOR_NVIDIA && d.class == nv::CLASS_DISPLAY)
    {
        let where_ = alloc::format!("{:02x}:{:02x}.{} [10de:{:04x}]", d.bus, d.dev, d.func, d.device);
        let by_id = nv::arch_of_device(d.device);
        let bar0 = match d.bars[0] {
            crate::drivers::pci::Bar::Memory { phys, size } => Some((phys, size)),
            _ => None,
        };
        // The VRAM aperture is BAR 1.
        let scans_gop = boot_fb.is_some_and(|f| {
            matches!(d.bars[1], crate::drivers::pci::Bar::Memory { phys, size } if f.phys_addr >= phys && f.phys_addr < phys + size)
        });
        // The identification register, read once (only with memory
        // decoding already on, as the firmware leaves a GPU it uses).
        let boot0 = match bar0 {
            Some((phys, size)) if option.as_deref() != Some("off") && size >= 0x1000 && d.read16(0x04) & 0x2 != 0 => {
                let va = crate::mm::paging::map_mmio(phys, 0x1000);
                Some(unsafe { (va as *const u32).read_volatile() })
            }
            _ => None,
        };
        let chip = boot0.and_then(nv::decode_boot0).and_then(|(c, rev)| nv::chipset(c).map(|(a, n)| (a, n, c, rev)));
        let arch = chip.map(|c| c.0).or(by_id);
        let what = match (arch, chip) {
            (Some(a), Some((_, name, c, rev))) => {
                alloc::format!("{} {} ({}, chipset {:#x} rev {:#x})", a.name(), name, a.products(), c, rev)
            }
            (Some(a), None) => alloc::format!("{} ({}; from the device id)", a.name(), a.products()),
            (None, _) => alloc::format!("an architecture this table doesn't know (BOOT_0 {:#x?})", boot0),
        };
        log!(
            "nvidia",
            "{}: {}; no native display driver{}{}",
            where_,
            what,
            if arch.is_some_and(|a| a.gsp()) { " (its display needs the GSP firmware)" } else { "" },
            if scans_gop {
                "; the firmware framebuffer stays, with a software pointer"
            } else {
                "; it doesn't scan the firmware framebuffer"
            }
        );
    }
}
