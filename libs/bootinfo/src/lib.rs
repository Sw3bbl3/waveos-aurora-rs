//! The handoff contract between `aurora-boot` (the UEFI loader) and the Tide kernel.
//!
//! Everything here is `#[repr(C)]` so both sides agree on layout regardless of
//! the compiler target (the bootloader is built for `x86_64-unknown-uefi`, the
//! kernel for `x86_64-unknown-none`).

#![no_std]

/// All physical memory is mapped at this virtual offset by the bootloader.
pub const PHYS_OFFSET: u64 = 0xFFFF_8000_0000_0000;

/// The kernel is linked to run at this virtual base (top 2 GiB, `-mcmodel=kernel`).
pub const KERNEL_BASE: u64 = 0xFFFF_FFFF_8000_0000;

/// "AURORA!!" — sanity check that the kernel was handed a real BootInfo.
pub const BOOTINFO_MAGIC: u64 = 0x2121_4152_4F52_5541;

/// Bumped whenever the layout of [`BootInfo`] changes.
pub const BOOTINFO_VERSION: u32 = 1;

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    /// Free RAM the kernel may use.
    Usable = 1,
    /// Firmware-reserved or otherwise untouchable.
    Reserved = 2,
    /// ACPI tables; reclaimable once parsed.
    AcpiReclaimable = 3,
    /// ACPI non-volatile storage; never touch.
    AcpiNvs = 4,
    /// Kernel image, boot stack, page tables and this BootInfo.
    Bootloader = 5,
    /// UEFI runtime services code/data; must be preserved.
    UefiRuntime = 6,
    /// Memory-mapped I/O.
    Mmio = 7,
    /// Faulty RAM.
    Bad = 8,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct MemoryRegion {
    pub start: u64,
    pub len: u64,
    pub kind: MemoryKind,
}

impl MemoryRegion {
    pub const fn end(&self) -> u64 {
        self.start + self.len
    }
}

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    /// Byte order R, G, B, X.
    Rgb = 0,
    /// Byte order B, G, R, X (the common case: a little-endian `0x00RRGGBB` u32).
    Bgr = 1,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct Framebuffer {
    /// Physical address of the linear framebuffer.
    pub phys_addr: u64,
    /// Size in bytes.
    pub size: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per scanline (may be larger than `width`).
    pub stride: u32,
    pub format: PixelFormat,
}

#[repr(C)]
#[derive(Debug)]
pub struct BootInfo {
    pub magic: u64,
    pub version: u32,
    pub _pad: u32,
    pub framebuffer: Framebuffer,
    /// Virtual pointer (through [`PHYS_OFFSET`]) to the memory map array.
    pub memory_map: *const MemoryRegion,
    pub memory_map_len: u64,
    /// Physical address of the ACPI RSDP, or 0 if the firmware had none.
    pub rsdp_phys: u64,
    /// Always [`PHYS_OFFSET`]; passed explicitly so the kernel never has to assume.
    pub phys_offset: u64,
    /// Physical address and size of the loaded kernel image.
    pub kernel_phys: u64,
    pub kernel_size: u64,
    /// Physical address of the PML4 the kernel is running on.
    pub pml4_phys: u64,
    /// Highest physical address covered by the physical-memory map.
    pub phys_mapped_end: u64,
}

impl BootInfo {
    /// # Safety
    /// `memory_map`/`memory_map_len` must describe a valid array (the bootloader guarantees this).
    pub unsafe fn memory_map(&self) -> &[MemoryRegion] {
        core::slice::from_raw_parts(self.memory_map, self.memory_map_len as usize)
    }
}

/// Signature of the kernel entry point (`_start`), System V ABI.
pub type KernelEntry = extern "sysv64" fn(&'static BootInfo) -> !;
