//! Page-table management on top of the bootloader's initial tables.

use super::{frame, phys_to_virt, PAGE_SIZE};
use crate::arch::mmu::{self, pte};
use bootinfo::BootInfo;
use core::sync::atomic::{AtomicU64, Ordering};

static PHYS_MAPPED_END: AtomicU64 = AtomicU64::new(0);

fn table(phys: u64) -> &'static mut [u64; 512] {
    // SAFETY: page tables live in RAM, inside the physical window.
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

pub fn init(boot_info: &BootInfo) {
    PHYS_MAPPED_END.store(boot_info.phys_mapped_end, Ordering::Relaxed);
    // Drop the bootloader's identity map: from now on low addresses fault,
    // which turns null-pointer bugs into clean page faults.
    table(mmu::root())[0] = 0;
    mmu::flush_all();
    log!("mm", "identity map removed; physical window covers {} GiB", boot_info.phys_mapped_end >> 30);
}

/// The kernel's 4 KiB entry for `va`, making missing tables; `None` if a
/// large page covers it or memory ran out.
fn kernel_leaf(va: u64) -> Option<&'static mut u64> {
    let mut t = mmu::root();
    for level in (1..=3).rev() {
        let e = &mut table(t)[((va >> (12 + 9 * level)) & 0x1FF) as usize];
        if *e & pte::PRESENT == 0 {
            let f = frame::alloc()?;
            // SAFETY: a frame just allocated, in the physical window.
            unsafe { core::ptr::write_bytes(phys_to_virt(f) as *mut u8, 0, PAGE_SIZE as usize) };
            *e = f | pte::KERNEL_TABLE;
        } else if *e & pte::HUGE != 0 {
            return None;
        }
        t = *e & pte::ADDR;
    }
    Some(&mut table(t)[((va >> 12) & 0x1FF) as usize])
}

/// Returns a kernel virtual address for a device's MMIO range, mapping it
/// (uncached) if it lies beyond the bootloader's physical window.
#[allow(dead_code)]
pub fn map_mmio(phys: u64, size: u64) -> u64 {
    let virt = phys_to_virt(phys);
    if phys + size <= PHYS_MAPPED_END.load(Ordering::Relaxed) {
        return virt;
    }
    let start = phys & !(PAGE_SIZE - 1);
    let mut p = start;
    while p < phys + size {
        let va = phys_to_virt(p);
        if let Some(e) = kernel_leaf(va).filter(|e| **e & pte::PRESENT == 0) {
            *e = pte::leaf(p | pte::PRESENT | pte::WRITABLE | pte::NO_CACHE);
            mmu::flush_page(va);
        }
        p += PAGE_SIZE;
    }
    virt
}
