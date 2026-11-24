//! The MMU: page-table root, TLB maintenance and the page-table entry format.
//!
//! Page tables are four levels of 512 eight-byte entries (4 KiB pages,
//! 48-bit addresses); `mm::vmm` walks them with `(va >> (12 + 9 * level))
//! & 0x1FF`, level 3 being the root. The root holds both halves: entries
//! 256..512 (the kernel half) are the same in every address space.

use x86_64::registers::control::Cr3;
use x86_64::structures::paging::PhysFrame;
use x86_64::{PhysAddr, VirtAddr};

/// Page-table entry bits (`mm::vmm`, `mm::paging`).
pub mod pte {
    pub const PRESENT: u64 = 1 << 0;
    pub const WRITABLE: u64 = 1 << 1;
    pub const USER: u64 = 1 << 2;
    /// Uncached (device memory).
    pub const NO_CACHE: u64 = 1 << 4;
    /// Set by the CPU when the page is used, and when it is written.
    pub const ACCESSED: u64 = 1 << 5;
    pub const DIRTY: u64 = 1 << 6;
    /// A 2 MiB or 1 GiB page (in a level 1 or 2 entry).
    pub const HUGE: u64 = 1 << 7;
    /// In a 4 KiB page entry the same bit selects the PAT slot of
    /// write-combining memory (see `arch::pat`).
    pub const WRITE_COMBINING: u64 = 1 << 7;
    /// Bits the hardware ignores, for the kernel's own use: three low
    /// (9-11) and three high (52-54), and one in non-present entries (62).
    pub const SOFT_0: u64 = 1 << 9;
    pub const SOFT_1: u64 = 1 << 10;
    pub const SOFT_2: u64 = 1 << 11;
    pub const SOFT_3: u64 = 1 << 52;
    pub const SOFT_4: u64 = 1 << 53;
    pub const SOFT_5: u64 = 1 << 54;
    /// Only in an entry that is not present.
    pub const SOFT_NOT_PRESENT: u64 = 1 << 62;
    pub const NO_EXECUTE: u64 = 1 << 63;
    /// The frame (or next table) address.
    pub const ADDR: u64 = 0x000F_FFFF_FFFF_F000;
    /// What an entry pointing at a next-level kernel table holds besides the address.
    pub const KERNEL_TABLE: u64 = PRESENT | WRITABLE;
    /// … at a next-level table of user mappings.
    pub const USER_TABLE: u64 = PRESENT | WRITABLE | USER;

    /// A leaf entry as written: the x86 format needs nothing more (on Arm
    /// this derives the hardware permission bits from the software ones).
    #[inline(always)]
    pub const fn leaf(e: u64) -> u64 {
        e
    }
}

/// The page-table root (physical address) the CPU runs on (CR3).
#[inline]
pub fn root() -> u64 {
    Cr3::read().0.start_address().as_u64()
}

/// Switches to the page tables at `root`.
///
/// # Safety
/// `root` must be a valid top-level table sharing the kernel half.
#[inline]
pub unsafe fn load_root(root: u64) {
    let (_, flags) = Cr3::read();
    Cr3::write(PhysFrame::containing_address(PhysAddr::new(root)), flags);
}

/// Drops this CPU's TLB entry for the page at `va`.
#[inline]
pub fn flush_page(va: u64) {
    x86_64::instructions::tlb::flush(VirtAddr::new(va));
}

/// Drops this CPU's TLB entries for every non-global page.
#[inline]
pub fn flush_all() {
    x86_64::instructions::tlb::flush_all();
}

/// Turns on what the entry format needs before user mappings are made:
/// the no-execute bit (EFER.NXE). Boot CPU; the AP trampoline sets it on
/// the others.
pub fn enable_features() {
    use x86_64::registers::model_specific::{Efer, EferFlags};
    // SAFETY: NXE only makes bit 63 of entries meaningful; nothing sets it
    // before this runs.
    unsafe { Efer::update(|f| f.insert(EferFlags::NO_EXECUTE_ENABLE)) };
}

/// Whether write-combining mappings ([`pte::WRITE_COMBINING`]) are available.
pub fn write_combining() -> bool {
    super::pat::write_combining()
}

/// Whether the no-execute bit is enabled (tests).
#[cfg(feature = "ktest")]
pub fn no_execute_enabled() -> bool {
    use x86_64::registers::model_specific::{Efer, EferFlags};
    Efer::read().contains(EferFlags::NO_EXECUTE_ENABLE)
}
