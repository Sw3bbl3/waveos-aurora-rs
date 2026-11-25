//! firstlight — the WaveOS Aurora UEFI bootloader.
//!
//! Boot flow:
//! 1. Pick a graphics mode through GOP.
//! 2. Choose the system to start (`slots`: the EFI partition's own, or an
//!    update in slot a or b on the home volume, going back to the previous
//!    one when an update fails), load its kernel and place its PT_LOAD
//!    segments in freshly allocated physical memory.
//! 3. Build a new 4-level page table: all physical memory at `PHYS_OFFSET`,
//!    an identity map (so this code survives the switch), and the kernel
//!    at its higher-half link address (`paging` on x86-64, `paging_aarch64`
//!    on Arm, which also holds the Arm hand-off).
//! 4. Exit boot services, translate the UEFI memory map into `BootInfo`,
//!    switch to the new address space and jump to the kernel.

#![no_std]
#![no_main]

extern crate alloc;

#[cfg(target_arch = "x86_64")]
mod paging;
#[cfg(target_arch = "aarch64")]
mod paging_aarch64;
mod slots;

use alloc::vec::Vec;
use bootinfo::{
    BootInfo, Framebuffer, MemoryKind, MemoryRegion, PixelFormat, VideoMode, BOOTINFO_MAGIC, BOOTINFO_VERSION,
    BOOT_DARK, BOOT_VERBOSE, KERNEL_BASE, MAX_MODES, PHYS_OFFSET,
};
use log::info;
use uefi::boot::{self, AllocateType, MemoryType};
use uefi::mem::memory_map::MemoryMap;
use uefi::proto::console::gop::{self, GraphicsOutput};
use uefi::table::cfg::ConfigTableEntry;
use uefi::{cstr16, prelude::*};

const PAGE: u64 = 4096;
const KERNEL_STACK_PAGES: usize = 128; // 512 KiB
/// Extra memory-map slots in case the map grows between our snapshot and exit.
const MEMMAP_SLACK: usize = 64;

#[entry]
fn main() -> Status {
    uefi::helpers::init().expect("uefi helpers");
    info!("firstlight {} starting", env!("CARGO_PKG_VERSION"));

    let conf = BootConf::read();
    let (framebuffer, modes, mode_count) = init_graphics(conf.resolution);
    info!(
        "graphics: {}x{} stride {} @ {:#x}",
        framebuffer.width, framebuffer.height, framebuffer.stride, framebuffer.phys_addr
    );

    let rsdp_phys = find_rsdp();
    info!("ACPI RSDP at {:#x}", rsdp_phys);

    // --- Choose and load the system ------------------------------------------------
    let (slot, system) = choose_system();
    let kernel_file = system.kernel;
    let image = aurora_elf::parse(&kernel_file).expect("kernel.elf is not an ELF for this processor");
    let span = image.virt_end - image.virt_start;
    let kernel_pages = (span.div_ceil(PAGE)) as usize;
    let kernel_phys = alloc_pages(kernel_pages);
    unsafe {
        core::ptr::write_bytes(kernel_phys as *mut u8, 0, kernel_pages * PAGE as usize);
        for seg in &image.segments {
            let dst = kernel_phys + (seg.vaddr - image.virt_start);
            let src = &kernel_file[seg.offset as usize..(seg.offset + seg.filesz) as usize];
            core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len());
        }
    }
    info!(
        "kernel: {} KiB at phys {:#x}, virt {:#x}, entry {:#x}",
        span / 1024,
        kernel_phys,
        image.virt_start,
        image.entry
    );
    assert!(image.virt_start >= KERNEL_BASE, "kernel must be linked in the higher half");
    drop(kernel_file);

    // --- System image (initrd) ----------------------------------------------------
    let system = Some(system.image).filter(|i| !i.is_empty());
    let (initrd_phys, initrd_len) = match system {
        Some(data) => {
            let phys = alloc_pages(data.len().div_ceil(PAGE as usize).max(1));
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), phys as *mut u8, data.len()) };
            info!("system image: {} KiB at phys {:#x}", data.len() / 1024, phys);
            (phys, data.len() as u64)
        }
        None => {
            log::warn!("no \\aurora\\system.tar found; booting without a system image");
            (0, 0)
        }
    };

    // --- A page below 1 MiB for real-mode start-up code (SMP, S3 resume) ----------
    let trampoline_phys = boot::allocate_pages(AllocateType::MaxAddress(0x9_F000), MemoryType::LOADER_DATA, 1)
        .map(|p| {
            unsafe { core::ptr::write_bytes(p.as_ptr(), 0, PAGE as usize) };
            p.as_ptr() as u64
        })
        .unwrap_or(0);
    info!("trampoline page at {:#x}", trampoline_phys);

    // --- Stack, BootInfo and memory map storage --------------------------------
    let stack_phys = alloc_pages(KERNEL_STACK_PAGES);
    let snapshot = boot::memory_map(MemoryType::LOADER_DATA).expect("memory map");
    let map_capacity = snapshot.len() + MEMMAP_SLACK;
    let map_bytes = map_capacity * core::mem::size_of::<MemoryRegion>();
    let info_pages = (core::mem::size_of::<BootInfo>() as u64 + map_bytes as u64).div_ceil(PAGE);
    let info_phys = alloc_pages(info_pages as usize);

    // Map everything that is RAM (plus the framebuffer and the low 4 GiB, which
    // holds the LAPIC/IOAPIC MMIO) through the physical-memory window.
    let mut phys_end: u64 = 4 << 30;
    #[cfg(target_arch = "aarch64")]
    let mut ram: Vec<(u64, u64)> = Vec::new();
    for d in snapshot.entries() {
        let end = d.phys_start + d.page_count * PAGE;
        let is_ram = matches!(
            d.ty,
            MemoryType::CONVENTIONAL
                | MemoryType::LOADER_CODE
                | MemoryType::LOADER_DATA
                | MemoryType::BOOT_SERVICES_CODE
                | MemoryType::BOOT_SERVICES_DATA
                | MemoryType::RUNTIME_SERVICES_CODE
                | MemoryType::RUNTIME_SERVICES_DATA
                | MemoryType::ACPI_RECLAIM
                | MemoryType::ACPI_NON_VOLATILE
        );
        if is_ram {
            phys_end = phys_end.max(end);
            #[cfg(target_arch = "aarch64")]
            ram.push((d.phys_start, end));
        }
    }
    phys_end = phys_end.max(framebuffer.phys_addr + framebuffer.size);
    phys_end = phys_end.next_multiple_of(1 << 30);
    drop(snapshot);

    // --- Page tables ------------------------------------------------------------
    #[cfg(target_arch = "x86_64")]
    let pml4_phys = {
        let mut pt = paging::PageTables::new(alloc_page);
        pt.map_huge_range(PHYS_OFFSET, 0, phys_end);
        pt.map_huge_range(0, 0, phys_end); // identity, dropped by the kernel later
        let mut off = 0;
        while off < span {
            pt.map_4k(image.virt_start + off, kernel_phys + off);
            off += PAGE;
        }
        pt.pml4()
    };
    // Arm: memory types matter. RAM is normal memory, a framebuffer
    // outside RAM non-cacheable, anything else (MMIO) device memory, per
    // 2 MiB block.
    #[cfg(target_arch = "aarch64")]
    let pml4_phys = {
        use paging_aarch64::Memory;
        let fb = (framebuffer.phys_addr, framebuffer.phys_addr + framebuffer.size);
        let kind = |p: u64| {
            let end = p + (2 << 20);
            // RAM first: a framebuffer in RAM (QEMU's ramfb) shares its
            // blocks with ordinary memory, which must stay cacheable (the
            // heap's atomics need it); the kernel maps the framebuffer
            // itself write-combining.
            if ram.iter().any(|&(s, e)| s < end && p < e) {
                Memory::Normal
            } else if fb.0 < end && p < fb.1 {
                Memory::WriteCombining
            } else {
                Memory::Device
            }
        };
        let mut pt = paging_aarch64::PageTables::new(alloc_page);
        pt.map_blocks(PHYS_OFFSET, 0, phys_end, false, kind);
        pt.map_blocks(0, 0, phys_end, true, kind); // identity, dropped by the kernel later
        let mut off = 0;
        while off < span {
            pt.map_4k(image.virt_start + off, kernel_phys + off);
            off += PAGE;
        }
        paging_aarch64::sync_icache(kernel_phys, span);
        pt.root()
    };
    info!("page tables ready, physical window covers {} GiB", phys_end >> 30);

    info!("exiting boot services, jumping to Aster kernel...");

    // --- Point of no return -----------------------------------------------------
    let uefi_map = unsafe { boot::exit_boot_services(None) };

    let map_ptr = (info_phys + core::mem::size_of::<BootInfo>() as u64).next_multiple_of(16);
    let regions = map_ptr as *mut MemoryRegion;
    let mut count = 0usize;
    for d in uefi_map.entries() {
        if count == map_capacity {
            break;
        }
        let region = MemoryRegion { start: d.phys_start, len: d.page_count * PAGE, kind: classify(d.ty) };
        // Merge with the previous region when contiguous and of the same kind.
        if count > 0 {
            let prev = unsafe { &mut *regions.add(count - 1) };
            if prev.kind == region.kind && prev.end() == region.start {
                prev.len += region.len;
                continue;
            }
        }
        unsafe { regions.add(count).write(region) };
        count += 1;
    }

    let boot_info = info_phys as *mut BootInfo;
    unsafe {
        boot_info.write(BootInfo {
            magic: BOOTINFO_MAGIC,
            version: BOOTINFO_VERSION,
            _pad: 0,
            framebuffer,
            memory_map: (map_ptr + PHYS_OFFSET) as *const MemoryRegion,
            memory_map_len: count as u64,
            rsdp_phys,
            phys_offset: PHYS_OFFSET,
            kernel_phys,
            kernel_size: kernel_pages as u64 * PAGE,
            pml4_phys,
            phys_mapped_end: phys_end,
            initrd_phys,
            initrd_len,
            modes,
            mode_count,
            flags: if conf.verbose { BOOT_VERBOSE } else { 0 }
                | if conf.light { 0 } else { BOOT_DARK }
                | slots::flag(&slot)
                | bootinfo::BOOT_SLOTS,
            trampoline_phys,
        });
    }

    let stack_top = PHYS_OFFSET + stack_phys + KERNEL_STACK_PAGES as u64 * PAGE;
    let boot_info_virt = PHYS_OFFSET + info_phys;
    #[cfg(target_arch = "aarch64")]
    unsafe {
        paging_aarch64::enter_kernel(pml4_phys, image.entry, stack_top, boot_info_virt)
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::asm!(
            "cli",
            "mov cr3, {pml4}",
            "mov rsp, {stack}",
            "xor ebp, ebp",
            "call {entry}",
            "2: hlt",
            "jmp 2b",
            pml4 = in(reg) pml4_phys,
            stack = in(reg) stack_top,
            entry = in(reg) image.entry,
            in("rdi") boot_info_virt,
            options(noreturn)
        );
    }
}

fn alloc_pages(count: usize) -> u64 {
    boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, count)
        .expect("out of memory while loading the kernel")
        .as_ptr() as u64
}

fn alloc_page() -> u64 {
    let p = alloc_pages(1);
    unsafe { core::ptr::write_bytes(p as *mut u8, 0, PAGE as usize) };
    p
}

fn classify(ty: MemoryType) -> MemoryKind {
    match ty {
        MemoryType::CONVENTIONAL | MemoryType::BOOT_SERVICES_CODE | MemoryType::BOOT_SERVICES_DATA => {
            MemoryKind::Usable
        }
        // Everything this loader allocated (kernel, stack, page tables, BootInfo)
        // is LOADER_DATA; keep it away from the kernel's frame allocator.
        MemoryType::LOADER_CODE | MemoryType::LOADER_DATA => MemoryKind::Bootloader,
        MemoryType::RUNTIME_SERVICES_CODE | MemoryType::RUNTIME_SERVICES_DATA => MemoryKind::UefiRuntime,
        MemoryType::ACPI_RECLAIM => MemoryKind::AcpiReclaimable,
        MemoryType::ACPI_NON_VOLATILE => MemoryKind::AcpiNvs,
        MemoryType::MMIO | MemoryType::MMIO_PORT_SPACE => MemoryKind::Mmio,
        MemoryType::UNUSABLE => MemoryKind::Bad,
        _ => MemoryKind::Reserved,
    }
}

/// Options from `\aurora\boot.conf` (`key=value` lines; Settings writes it).
#[derive(Default)]
struct BootConf {
    resolution: Option<(usize, usize)>,
    verbose: bool,
    /// `appearance=light`: a light boot screen (dark is the default).
    light: bool,
}

impl BootConf {
    fn read() -> BootConf {
        let mut conf = BootConf::default();
        let Some(data) = read_file(cstr16!("\\aurora\\boot.conf")) else { return conf };
        for line in core::str::from_utf8(&data).unwrap_or("").lines() {
            match line.trim().split_once('=') {
                Some(("resolution", v)) => {
                    conf.resolution =
                        v.trim().split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                }
                Some(("verbose", v)) => conf.verbose = v.trim() == "1",
                Some(("appearance", v)) => conf.light = v.trim() == "light",
                _ => {}
            }
        }
        conf
    }
}

/// Chooses a graphics mode: the one boot.conf asks for, else 1280x800 (the
/// Aurora design size) if offered, else the largest mode no wider than 1920.
/// Returns the framebuffer and the list of usable modes.
fn init_graphics(wanted: Option<(usize, usize)>) -> (Framebuffer, [VideoMode; MAX_MODES], u32) {
    let none = || (Framebuffer { phys_addr: 0, size: 0, width: 0, height: 0, stride: 0, format: PixelFormat::Bgr }, [VideoMode::default(); MAX_MODES], 0);
    let Ok(handle) = boot::get_handle_for_protocol::<GraphicsOutput>() else {
        log::warn!("no graphics output (GOP): booting without a framebuffer");
        return none();
    };
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle).expect("open GOP");

    let usable = |m: &gop::Mode| matches!(m.info().pixel_format(), gop::PixelFormat::Rgb | gop::PixelFormat::Bgr);
    let mut list = [VideoMode::default(); MAX_MODES];
    let mut count = 0;
    let mut best: Option<gop::Mode> = None;
    let mut exact: Option<gop::Mode> = None;
    let mut design: Option<gop::Mode> = None;
    for mode in gop.modes() {
        if !usable(&mode) {
            continue;
        }
        let (w, h) = mode.info().resolution();
        let vm = VideoMode { width: w as u32, height: h as u32 };
        if count < MAX_MODES && !list[..count].contains(&vm) {
            list[count] = vm;
            count += 1;
        }
        if Some((w, h)) == wanted && exact.is_none() {
            exact = Some(mode);
            continue;
        }
        if (w, h) == (1280, 800) && design.is_none() {
            design = Some(mode);
            continue;
        }
        if w > 1920 {
            continue;
        }
        let better = match &best {
            None => true,
            Some(b) => {
                let (bw, bh) = b.info().resolution();
                w * h > bw * bh
            }
        };
        if better {
            best = Some(mode);
        }
    }
    if let Some(mode) = exact.or(design).or(best) {
        gop.set_mode(&mode).expect("set GOP mode");
    } else if count == 0 {
        // Only Blt-only modes (EDK2's virtio-gpu on Arm): no framebuffer.
        log::warn!("the graphics output has no framebuffer: booting without one");
        return none();
    }
    list[..count].sort_by_key(|m| (m.width, m.height));

    let info = gop.current_mode_info();
    let (width, height) = info.resolution();
    let format = match info.pixel_format() {
        gop::PixelFormat::Rgb => PixelFormat::Rgb,
        _ => PixelFormat::Bgr,
    };
    let mut fb = gop.frame_buffer();
    let framebuffer = Framebuffer {
        phys_addr: fb.as_mut_ptr() as u64,
        size: fb.size() as u64,
        width: width as u32,
        height: height as u32,
        stride: info.stride() as u32,
        format,
    };
    (framebuffer, list, count as u32)
}

fn find_rsdp() -> u64 {
    uefi::system::with_config_table(|entries| {
        let find = |guid| entries.iter().find(|e| e.guid == guid).map(|e| e.address as u64);
        find(ConfigTableEntry::ACPI2_GUID).or_else(|| find(ConfigTableEntry::ACPI_GUID)).unwrap_or(0)
    })
}

fn read_file(path: &uefi::CStr16) -> Option<Vec<u8>> {
    let sfs = boot::get_image_file_system(boot::image_handle()).expect("boot volume");
    let mut fs = uefi::fs::FileSystem::new(sfs);
    fs.read(path).ok()
}

fn write_file(path: &uefi::CStr16, data: &[u8]) -> bool {
    let Ok(sfs) = boot::get_image_file_system(boot::image_handle()) else { return false };
    uefi::fs::FileSystem::new(sfs).write(path, data).is_ok()
}

/// The EFI partition's own system: `\aurora\kernel.elf` and the system
/// image (optical discs carry it compressed: their boot image holds 32 MiB
/// at most).
fn esp_system() -> slots::System {
    let kernel =
        read_file(cstr16!("\\aurora\\kernel.elf")).expect("could not read \\aurora\\kernel.elf from the boot volume");
    let image = read_file(cstr16!("\\aurora\\system.tar"))
        .or_else(|| {
            let packed = read_file(cstr16!("\\aurora\\system.tar.zlib"))?;
            let data = miniz_oxide::inflate::decompress_to_vec_zlib(&packed);
            if data.is_err() {
                log::warn!("\\aurora\\system.tar.zlib is damaged");
            }
            data.ok()
        })
        .unwrap_or_else(|| {
            log::warn!("no \\aurora\\system.tar found; booting without a system image");
            Vec::new()
        });
    slots::System { kernel, image }
}

/// Picks the system to start (see `slots`), taking a try from a slot on
/// trial and going back when one can't start. Returns the slot started.
fn choose_system() -> (alloc::string::String, slots::System) {
    use alloc::string::String;
    let raw = read_file(cstr16!("\\aurora\\boot.conf")).and_then(|b| String::from_utf8(b).ok()).unwrap_or_default();
    let choice = slots::Choice::parse(&raw);
    let save = |text: &str| {
        if !write_file(cstr16!("\\aurora\\boot.conf"), text.as_bytes()) {
            log::warn!("could not update \\aurora\\boot.conf");
        }
    };
    let mut failed: Option<String> = None;
    let mut first = choice.system.clone();
    if first != "esp" {
        match choice.tries {
            Some(0) => {
                log::warn!("system {first} did not start after its tries; going back to {}", choice.previous);
                failed = Some(first.clone());
                first = choice.previous.clone();
            }
            Some(n) => {
                // The try is taken before the attempt, so a crash still counts.
                save(&slots::edit(&raw, &[("system_tries", Some(&alloc::format!("{}", n - 1)))]));
                info!("starting system {first} on trial ({} tries left after this one)", n - 1);
            }
            None => {}
        }
    }
    let mut order = alloc::vec![first];
    for s in [choice.previous.clone(), String::from("esp")] {
        if !order.contains(&s) {
            order.push(s);
        }
    }
    for slot in order {
        let system = if slot == "esp" {
            Some(esp_system())
        } else {
            slots::load(&slot, choice.tries.is_some() && slot == choice.system)
        };
        let Some(system) = system else {
            log::warn!("system {slot} could not be read; trying the next one");
            failed.get_or_insert(slot);
            continue;
        };
        if slot != choice.system {
            // Went back: say so, and stop trying the one that failed.
            let conf = read_file(cstr16!("\\aurora\\boot.conf")).and_then(|b| String::from_utf8(b).ok()).unwrap_or(raw.clone());
            save(&slots::edit(
                &conf,
                &[("system", Some(&slot)), ("system_tries", None), ("system_failed", failed.as_deref())],
            ));
        }
        info!("starting system {slot}");
        return (slot, system);
    }
    unreachable!("the EFI partition's system always loads or panics")
}
