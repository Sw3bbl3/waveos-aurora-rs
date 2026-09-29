//! Memory management: physical frames, kernel heap, page tables.

pub mod frame;
pub mod heap;
pub mod paging;
pub mod vmm;

use bootinfo::{BootInfo, PHYS_OFFSET};

pub const PAGE_SIZE: u64 = 4096;

/// Translates a physical address into the kernel's physical-memory window.
#[inline]
pub fn phys_to_virt(phys: u64) -> u64 {
    phys + PHYS_OFFSET
}

pub fn init(boot_info: &'static BootInfo) {
    frame::init(unsafe { boot_info.memory_map() });
    heap::init();
    paging::init(boot_info);
    vmm::init();
}

pub struct MemStats {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub heap_size: u64,
    pub heap_used: u64,
}

pub fn stats() -> MemStats {
    let (total, free) = frame::counts();
    let (heap_size, heap_used) = heap::usage();
    MemStats { total_bytes: total * PAGE_SIZE, used_bytes: (total - free) * PAGE_SIZE, heap_size, heap_used }
}
