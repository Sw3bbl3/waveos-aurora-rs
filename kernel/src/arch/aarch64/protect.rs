//! CPU protection features and the kernel's own page permissions.
//!
//! Per CPU, where the ID registers offer it:
//! - **PAN** (FEAT_PAN, x86's SMAP): the kernel can't read or write a user
//!   page except inside the user-copy routine (`uaccess`), which clears
//!   PSTATE.PAN around its loop. SCTLR_EL1.SPAN is cleared, so every
//!   exception into the kernel sets PAN again.
//! - **PXN** on every user page and every page the kernel maps at run time
//!   (x86's SMEP and NX for kernel data): part of `mmu::pte::PRESENT`.
//! - **Pointer authentication for user code** (FEAT_PAuth): the APIA key
//!   is set (one random key per boot, the same on every CPU and in every
//!   process) and SCTLR_EL1.EnIA turned on, so the `paciasp`/`autiasp`
//!   pairs user programs are built with sign and check return addresses.
//!   (The kernel itself is built without them.) BTI landing pads are in the
//!   programs too, but not enforced: no page is mapped guarded (GP).
//!
//! Once, at boot ([`protect_kernel`]): the kernel image W^X — `.text`
//! read-only and executable, `.rodata` read-only, `.data`/`.bss` writable,
//! both never executable. The bootloader maps the physical window, an alias
//! of all RAM, never executable.

use super::regs::{self, read};
use crate::mm::phys_to_virt;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    /// x86's names, for the tests: SMEP is PXN (always), SMAP is PAN, UMIP
    /// has no counterpart (EL0 can't read the kernel's system registers).
    pub smep: bool,
    pub smap: bool,
    pub umip: bool,
    pub pan: bool,
    pub pauth: bool,
    pub bti: bool,
    pub rndr: bool,
}

/// What this CPU offers (ID registers).
pub fn features() -> Features {
    let mmfr1 = read!("id_aa64mmfr1_el1");
    let isar0 = read!("id_aa64isar0_el1");
    let isar1 = read!("id_aa64isar1_el1");
    let pfr1 = read!("id_aa64pfr1_el1");
    let pan = (mmfr1 >> 20) & 0xF != 0;
    Features {
        smep: true,
        smap: pan,
        umip: false,
        pan,
        // APA (QARMA) or API (implementation defined) for the A key.
        pauth: (isar1 >> 4) & 0xFF != 0,
        bti: pfr1 & 0xF != 0,
        rndr: (isar0 >> 60) & 0xF != 0,
    }
}

/// Set once PAN is on: the user-copy routine then opens user access.
static PAN_ON: AtomicBool = AtomicBool::new(false);
static PAUTH_ON: AtomicBool = AtomicBool::new(false);
/// The APIA key, made on the boot CPU and given to the others.
static KEY: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
/// Pages of the kernel image whose permissions were tightened.
static IMAGE_PAGES: AtomicU64 = AtomicU64::new(0);

const SCTLR_SPAN: u64 = 1 << 23;
const SCTLR_ENIA: u64 = 1 << 31;

/// Turns PAN and pointer authentication on, on the calling CPU (every CPU).
pub fn init() {
    let f = features();
    let mut sctlr = read!("sctlr_el1");
    if f.pauth {
        if KEY[0].load(Ordering::Relaxed) == 0 && KEY[1].load(Ordering::Relaxed) == 0 {
            KEY[0].store(super::rng::early_seed() | 1, Ordering::Relaxed);
            KEY[1].store(super::rng::early_seed(), Ordering::Relaxed);
        }
        // SAFETY: the APIA key and its enable only affect `pacia`/`autia`,
        // which the kernel never executes.
        unsafe {
            regs::write!("s3_0_c2_c1_0", KEY[0].load(Ordering::Relaxed));
            regs::write!("s3_0_c2_c1_1", KEY[1].load(Ordering::Relaxed));
        }
        sctlr |= SCTLR_ENIA;
        PAUTH_ON.store(true, Ordering::Relaxed);
    }
    if f.pan {
        sctlr &= !SCTLR_SPAN;
    }
    // SAFETY: SPAN and EnIA as described above; the rest unchanged.
    unsafe { regs::write!("sctlr_el1", sctlr) };
    regs::isb();
    if f.pan {
        // SAFETY: `msr pan, #1` (encoded: the kernel's target lacks v8.1).
        unsafe { core::arch::asm!(".inst 0xd500419f", options(nostack, preserves_flags)) };
        PAN_ON.store(true, Ordering::Release);
    }
}

/// Whether PAN is on (the user-copy routine must open user access).
pub fn smap() -> bool {
    PAN_ON.load(Ordering::Relaxed)
}

extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_end: u8;
    static __kernel_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// The 4 KiB entry for kernel address `va` in the tables at `root`.
fn leaf(root: u64, va: u64) -> Option<&'static mut u64> {
    let mut t = root;
    for level in (1..=3).rev() {
        // SAFETY: page tables live in RAM, in the physical window.
        let e = unsafe { *(phys_to_virt(t) as *const u64).add(((va >> (12 + 9 * level)) & 0x1FF) as usize) };
        if e & 0b11 != 0b11 {
            return None;
        }
        t = e & super::mmu::pte::ADDR;
    }
    // SAFETY: as above; the entry is the kernel's own.
    Some(unsafe { &mut *(phys_to_virt(t) as *mut u64).add(((va >> 12) & 0x1FF) as usize) })
}

/// Maps the kernel image W^X. Runs once on the boot CPU, after `vmm::init`.
pub fn protect_kernel(root: u64) {
    use super::mmu::pte::{PXN, READ_ONLY};
    // SAFETY: only the symbols' addresses are taken.
    let (start, text_end, ro_end, end) =
        unsafe { (sym(&__kernel_start), sym(&__text_end), sym(&__rodata_end), sym(&__kernel_end)) };
    let mut pages = 0;
    let mut va = start;
    while va < end {
        if let Some(e) = leaf(root, va) {
            *e = if va < text_end {
                (*e | READ_ONLY) & !PXN
            } else if va < ro_end {
                *e | READ_ONLY | PXN
            } else {
                (*e & !READ_ONLY) | PXN
            };
            pages += 1;
        }
        va += 4096;
    }
    super::mmu::flush_all();
    IMAGE_PAGES.store(pages, Ordering::Relaxed);
}

/// One line for the boot log.
pub fn report() -> alloc::string::String {
    let f = features();
    let on = |b: bool| if b { "on" } else { "unavailable" };
    alloc::format!(
        "PAN {}, PXN on user and run-time mappings, kernel W^X ({} pages), user pointer authentication {}, \
         BTI {} (not enforced), RNDR {}, stack protector {}",
        on(PAN_ON.load(Ordering::Relaxed)),
        IMAGE_PAGES.load(Ordering::Relaxed),
        on(PAUTH_ON.load(Ordering::Relaxed)),
        if f.bti { "available" } else { "absent" },
        if f.rndr { "available" } else { "absent" },
        if crate::stackguard::active() { "on" } else { "off" },
    )
}

#[cfg(feature = "ktest")]
pub fn every_cpu() -> (bool, u64) {
    (true, 1)
}

/// Asserts that the calling CPU has on what it offers (tests).
#[cfg(feature = "ktest")]
pub fn assert_this_cpu() {
    let f = features();
    assert!(this_cpu_protected(), "PAN, SPAN and EnIA as the ID registers say (pan {}, pauth {})", f.pan, f.pauth);
}

/// Whether the calling CPU's protections are on now.
#[cfg(feature = "ktest")]
pub fn this_cpu_protected() -> bool {
    let f = features();
    let sctlr = read!("sctlr_el1");
    (!f.pan || sctlr & SCTLR_SPAN == 0) && (!f.pauth || sctlr & SCTLR_ENIA != 0)
}
