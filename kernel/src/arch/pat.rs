//! The page attribute table: lets a mapping ask for write-combining.
//!
//! Firmware usually marks the framebuffer uncached, so every pixel the
//! compositor copies there is a separate slow bus write. With slot 4 of the
//! PAT set to write-combining, a mapping that selects it (the PAT bit in a
//! 4 KiB page entry) gathers those writes into full bursts, as Linux's
//! `ioremap_wc` does. Every CPU must hold the same table, so this runs on
//! each one as it starts and again after waking from sleep.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::registers::model_specific::Msr;

const IA32_PAT: u32 = 0x277;
/// Slot 4 (PAT=1, PCD=0, PWT=0): write-back by default, never used otherwise.
const WC_SLOT: u64 = 4;
const WRITE_COMBINING: u64 = 0x01;

static SUPPORTED: AtomicBool = AtomicBool::new(false);

/// True once write-combining mappings are available.
pub fn write_combining() -> bool {
    SUPPORTED.load(Ordering::Relaxed)
}

/// Checks that write-combining mappings are what they claim (a kernel test):
/// slot 4 holds WC on this CPU, and a mapping made with it selects slot 4.
#[cfg(feature = "ktest")]
pub fn self_test() {
    if !write_combining() {
        return;
    }
    let pat = unsafe { Msr::new(IA32_PAT).read() };
    assert_eq!((pat >> (WC_SLOT * 8)) & 0xFF, WRITE_COMBINING, "PAT slot 4 is not write-combining");
    let frame = crate::mm::frame::alloc().expect("a frame");
    let va = crate::mm::vmm::kmap_wc(frame, 4096).expect("a write-combining mapping");
    let e = crate::mm::vmm::kernel_entry(va).expect("the mapping's entry");
    // PAT (bit 7) set, PCD (bit 4) and PWT (bit 3) clear: slot 4.
    assert_eq!(e & (1 << 7 | 1 << 4 | 1 << 3), 1 << 7);
    assert_eq!(e & 0x000F_FFFF_FFFF_F000, frame);
    // Only the entry is checked: touching RAM through two cache types at
    // once isn't coherent, so the page is never accessed.
    crate::mm::vmm::kunmap(va, 1);
    crate::mm::frame::free(frame);
}

/// Programs this CPU's PAT (the boot CPU first: it decides support).
pub fn init() {
    let edx = core::arch::x86_64::__cpuid(1).edx;
    if edx & (1 << 16) == 0 {
        return;
    }
    let mut msr = Msr::new(IA32_PAT);
    let old = unsafe { msr.read() };
    let new = (old & !(0xFF << (WC_SLOT * 8))) | WRITE_COMBINING << (WC_SLOT * 8);
    if new != old {
        // Intel's sequence: write back and invalidate caches, then load the table.
        unsafe {
            asm!("wbinvd", options(nostack));
            msr.write(new);
        }
        x86_64::instructions::tlb::flush_all();
    }
    SUPPORTED.store(true, Ordering::Relaxed);
}
