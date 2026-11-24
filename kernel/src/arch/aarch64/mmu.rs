//! The MMU: translation table roots, TLB maintenance and the descriptor
//! format, laid out so `mm::vmm` walks Arm tables as it walks x86 ones.
//!
//! 4 KiB granule, four levels of 512 eight-byte descriptors, index
//! `(va >> (12 + 9 * level)) & 0x1FF`, level 3 the root. One root page
//! serves both halves, as on x86-64: TTBR1 (T1SZ = 16) walks its entries
//! 256..512 for kernel addresses `0xffff_8000_0000_0000..`, and TTBR0
//! (T0SZ = 17, a 47-bit user space ending at `USER_END`) walks only its
//! entries 0..256. So the kernel half copied into every process's root is
//! simply ignored by the hardware there, and [`load_root`] only changes
//! TTBR0; TTBR1 stays on the kernel's root.
//!
//! Memory types (MAIR_EL1, set by the bootloader): index 0 normal
//! write-back, 1 device nGnRE, 2 normal non-cacheable, 3 device nGnRnE.

use super::regs::{self, read};

/// Descriptor bits, named as `mm::vmm` uses them (`arch::x86_64::mmu::pte`
/// for the meanings). Bits the hardware ignores carry the kernel's own
/// flags: 55-58, 63 and, with hardware attributes (FEAT_HPDS2) off, 59-62.
///
/// **Write permission and dirtiness are kept in software**, as on Arm
/// processors without hardware dirty-bit management (FEAT_HAFDBS; Apple's
/// cores under HVF have none): [`WRITABLE`] and [`DIRTY`] are software
/// bits, and [`leaf`] derives the hardware's read-only bit (AP[2]) from
/// them whenever `vmm` writes an entry: a user page is writable in the
/// hardware only once it is [`DIRTY`]. The first write to a clean one is a
/// permission fault, which `trap` resolves by setting [`DIRTY`] (a
/// compare-and-swap, as a CPU would) and clearing AP[2]. A cleared access
/// flag likewise faults once and is set again there.
pub mod pte {
    /// Valid, and a page (level 0) or table (levels 1-3) descriptor, inner
    /// shareable, normal memory, never executable at EL1 (the kernel runs
    /// no code it maps at run time). The bits are set and cleared together,
    /// so `e & (PRESENT | USER) == PRESENT | USER` tests work as on x86.
    pub const PRESENT: u64 = 0b11 | SH_INNER | PXN;
    /// Software: the page may be written (see the module documentation).
    pub const WRITABLE: u64 = 1 << 63;
    /// AP[1]: EL0 may access.
    pub const USER: u64 = 1 << 6;
    /// AttrIndx 1: device nGnRE.
    pub const NO_CACHE: u64 = 1 << 2;
    /// The access flag (AF): set by [`leaf`]; when the reclaim scan clears
    /// it, the next access faults and `trap` sets it again.
    pub const ACCESSED: u64 = 1 << 10;
    /// Software: the page was written since this bit was last cleared.
    /// (Bit 62 serves as [`SOFT_NOT_PRESENT`] in invalid entries.)
    pub const DIRTY: u64 = 1 << 62;
    /// A block (2 MiB or 1 GiB) descriptor: the bootloader marks the
    /// physical window's blocks with this software bit, as their low bits
    /// (0b01) cannot be tested with a mask.
    pub const HUGE: u64 = 1 << 58;
    /// AttrIndx 2: normal non-cacheable (framebuffers).
    pub const WRITE_COMBINING: u64 = 1 << 3;
    pub const SOFT_0: u64 = 1 << 55;
    pub const SOFT_1: u64 = 1 << 56;
    pub const SOFT_2: u64 = 1 << 57;
    pub const SOFT_3: u64 = 1 << 59;
    pub const SOFT_4: u64 = 1 << 60;
    pub const SOFT_5: u64 = 1 << 61;
    /// Only in an invalid descriptor, where every bit but 0 is free.
    pub const SOFT_NOT_PRESENT: u64 = 1 << 62;
    /// UXN: EL0 may not execute (PXN is always set, in [`PRESENT`]).
    pub const NO_EXECUTE: u64 = 1 << 54;
    /// The output address (48 bits).
    pub const ADDR: u64 = 0x0000_FFFF_FFFF_F000;
    /// Table descriptors: no software bits (bit 63 is NSTable there).
    pub const KERNEL_TABLE: u64 = PRESENT;
    pub const USER_TABLE: u64 = PRESENT | USER;

    /// SH = inner shareable.
    pub const SH_INNER: u64 = 3 << 8;
    /// PXN: EL1 may not execute.
    pub const PXN: u64 = 1 << 53;
    /// AP[2]: read-only.
    pub const READ_ONLY: u64 = 1 << 7;
    /// nG: the TLB entry belongs to one address space (ASID).
    pub const NOT_GLOBAL: u64 = 1 << 11;

    /// A leaf entry as written: the access flag set, and AP[2] (read-only)
    /// unless the page is writable and — for user pages — dirty. Invalid
    /// entries (swap entries) pass unchanged.
    #[inline]
    pub const fn leaf(e: u64) -> u64 {
        if e & 1 == 0 {
            return e;
        }
        let e = e | ACCESSED;
        if e & WRITABLE != 0 && (e & DIRTY != 0 || e & USER == 0) {
            e & !READ_ONLY
        } else {
            e | READ_ONLY
        }
    }
}

/// Resolves a fault the software bits cause rather than the page's
/// absence or protection: a cleared access flag, or the first write to a
/// writable page that is not yet dirty. `root` is the table the faulting
/// address belongs to. True if the access can go again.
///
/// Runs with interrupts masked, so no TLB shootdown — and no freeing of a
/// page table, which waits for one — completes on this CPU's address
/// spaces meanwhile; the entry is changed with a compare-and-swap, as a
/// CPU managing these bits itself would.
pub fn fix_soft_fault(root: u64, va: u64, write: bool) -> bool {
    use core::sync::atomic::{AtomicU64, Ordering};
    let mut t = root;
    for level in (1..=3).rev() {
        let e = unsafe { *(crate::mm::phys_to_virt(t) as *const u64).add(((va >> (12 + 9 * level)) & 0x1FF) as usize) };
        if e & 0b11 != 0b11 {
            return false; // invalid, or a block (the physical window: never soft)
        }
        t = e & pte::ADDR;
    }
    // SAFETY: an aligned entry in a live page table (see above).
    let entry = unsafe { &*((crate::mm::phys_to_virt(t) as *const AtomicU64).add(((va >> 12) & 0x1FF) as usize)) };
    loop {
        let old = entry.load(Ordering::Acquire);
        if old & 1 == 0 {
            return false;
        }
        let mut new = old | pte::ACCESSED;
        if write {
            if old & pte::WRITABLE == 0 {
                return false;
            }
            new |= pte::DIRTY;
        }
        let new = pte::leaf(new);
        if new == old {
            // Another CPU fixed it already (or this TLB entry is stale).
            flush_page(va);
            return true;
        }
        if entry.compare_exchange(old, new, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            flush_page(va);
            return true;
        }
    }
}

/// MAIR_EL1 as the bootloader sets it (see the module documentation).
pub const MAIR: u64 = 0xFF | 0x04 << 8 | 0x44 << 16;

/// The root (physical address) of the user half the CPU runs on (TTBR0).
#[inline]
pub fn root() -> u64 {
    read!("ttbr0_el1") & pte::ADDR
}

/// The kernel's root (TTBR1).
pub fn kernel_root() -> u64 {
    read!("ttbr1_el1") & pte::ADDR
}

/// Switches the user half to the tables at `root`, dropping the TLB's
/// entries for the old one (no ASIDs yet).
///
/// # Safety
/// `root` must be a valid top-level table whose user half the running code
/// may lose (it is never running from user addresses).
#[inline]
pub unsafe fn load_root(root: u64) {
    regs::dsb_ish();
    regs::write!("ttbr0_el1", root);
    regs::isb();
    flush_all();
}

/// Drops the TLB entries for the page at `va` on every CPU (inner
/// shareable broadcast: Arm needs no IPI for this).
#[inline]
pub fn flush_page(va: u64) {
    // SAFETY: TLB maintenance only; the barriers order it after the
    // descriptor write and before later accesses.
    unsafe {
        core::arch::asm!(
            "dsb ishst",
            "tlbi vaae1is, {}",
            "dsb ish",
            "isb",
            in(reg) (va >> 12) & 0xFFF_FFFF_FFFF,
            options(nostack, preserves_flags)
        )
    };
}

/// Drops every TLB entry of the EL1&0 regime on every CPU.
#[inline]
pub fn flush_all() {
    // SAFETY: TLB maintenance only.
    unsafe { core::arch::asm!("dsb ishst", "tlbi vmalle1is", "dsb ish", "isb", options(nostack, preserves_flags)) };
}

/// The physical address `va` maps to for EL1 reads (`at s1e1r`), if any.
pub fn virt_to_phys(va: u64) -> Option<u64> {
    let par: u64;
    // SAFETY: an address translation query; PAR_EL1 is only ours.
    unsafe {
        core::arch::asm!("at s1e1r, {}", "isb", "mrs {}, par_el1", in(reg) va, out(reg) par, options(nostack));
    }
    (par & 1 == 0).then_some((par & pte::ADDR) | (va & 0xFFF))
}

/// Nothing to turn on: execute-never is part of the format.
pub fn enable_features() {}

/// Normal non-cacheable memory is always available.
pub fn write_combining() -> bool {
    true
}

#[cfg(feature = "ktest")]
pub fn no_execute_enabled() -> bool {
    true
}
