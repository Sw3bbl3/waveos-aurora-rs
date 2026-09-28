//! Page-table management on top of the bootloader's initial tables.

use super::{frame, phys_to_virt, PAGE_SIZE};
use bootinfo::{BootInfo, PHYS_OFFSET};
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

static PHYS_MAPPED_END: AtomicU64 = AtomicU64::new(0);

struct Frames;

unsafe impl FrameAllocator<Size4KiB> for Frames {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        let phys = frame::alloc()?;
        unsafe { core::ptr::write_bytes(phys_to_virt(phys) as *mut u8, 0, PAGE_SIZE as usize) };
        Some(PhysFrame::containing_address(PhysAddr::new(phys)))
    }
}

fn active_table() -> OffsetPageTable<'static> {
    let (pml4, _) = Cr3::read();
    let table = phys_to_virt(pml4.start_address().as_u64()) as *mut PageTable;
    unsafe { OffsetPageTable::new(&mut *table, VirtAddr::new(PHYS_OFFSET)) }
}

pub fn init(boot_info: &BootInfo) {
    PHYS_MAPPED_END.store(boot_info.phys_mapped_end, Ordering::Relaxed);
    // Drop the bootloader's identity map: from now on low addresses fault,
    // which turns null-pointer bugs into clean page faults.
    let (pml4, _) = Cr3::read();
    let pml4 = unsafe { &mut *(phys_to_virt(pml4.start_address().as_u64()) as *mut PageTable) };
    pml4[0].set_unused();
    x86_64::instructions::tlb::flush_all();
    log!("mm", "identity map removed; physical window covers {} GiB", boot_info.phys_mapped_end >> 30);
}

/// Returns a kernel virtual address for a device's MMIO range, mapping it
/// (uncached) if it lies beyond the bootloader's physical window.
#[allow(dead_code)]
pub fn map_mmio(phys: u64, size: u64) -> u64 {
    let virt = phys_to_virt(phys);
    if phys + size <= PHYS_MAPPED_END.load(Ordering::Relaxed) {
        return virt;
    }
    let mut table = active_table();
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE;
    let start = phys & !(PAGE_SIZE - 1);
    let mut p = start;
    while p < phys + size {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(phys_to_virt(p)));
        let frame = PhysFrame::containing_address(PhysAddr::new(p));
        if let Ok(flush) = unsafe { table.map_to(page, frame, flags, &mut Frames) } {
            flush.flush();
        }
        p += PAGE_SIZE;
    }
    virt
}
