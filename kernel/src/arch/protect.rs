//! CPU protection features and the kernel's own page permissions.
//!
//! Per CPU (the boot CPU, every application processor, and the boot CPU
//! again after waking from S3), each where CPUID offers it:
//! - **SMEP**: the kernel never executes a user page.
//! - **SMAP**: the kernel never reads or writes a user page, except inside
//!   the user-copy routine (`syscall::user`), which opens access with `stac`
//!   and closes it with `clac`. Every trap entry closes it again.
//! - **UMIP**: `sgdt`, `sidt`, `sldt`, `smsw` and `str` fault in ring 3
//!   instead of revealing kernel addresses.
//! - **CR0.WP** (set in `fpu::init`): the kernel can't write read-only pages.
//! - **EFER.NXE** (set in `vmm::init` and by the AP trampoline).
//!
//! Once, at boot ([`protect_kernel`]): the kernel image is mapped W^X —
//! `.text` read-only and executable, `.rodata` (with `.got`) read-only and
//! no-execute, `.data`/`.bss` writable and no-execute — and every other
//! kernel PML4 slot (the physical-memory window, the kernel virtual area,
//! kernel stacks) is no-execute, so no kernel mapping is both writable and
//! executable.
//!
//! Speculation: no IBRS/IBPB/STIBP or KPTI is set up (see
//! docs/ARCHITECTURE.md, "Hardening"); [`report`] states what the CPU has.

use crate::mm::phys_to_virt;
use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// Set once SMAP is on: the user-copy routine and the trap entry use
/// `stac`/`clac` only then (they are invalid instructions without SMAP).
/// Read by assembly (`arch::trap`), so a plain byte.
#[no_mangle]
pub static AURORA_SMAP: AtomicBool = AtomicBool::new(false);

static SMEP_ON: AtomicBool = AtomicBool::new(false);
/// CR4 as each CPU left `init` (and how many times it ran there: boot,
/// then once more per wake from S3).
static CR4_OF: [AtomicU64; super::percpu::MAX_CPUS] = [const { AtomicU64::new(0) }; super::percpu::MAX_CPUS];
static INITS_OF: [AtomicU64; super::percpu::MAX_CPUS] = [const { AtomicU64::new(0) }; super::percpu::MAX_CPUS];
static UMIP_ON: AtomicBool = AtomicBool::new(false);
/// Pages of the kernel image whose permissions were tightened (for the report).
static IMAGE_PAGES: AtomicU64 = AtomicU64::new(0);

const CR4_UMIP: u64 = 1 << 11;

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
    pub smep: bool,
    pub smap: bool,
    pub umip: bool,
    pub nx: bool,
    pub ibrs: bool,
    pub stibp: bool,
    pub ssbd: bool,
    pub md_clear: bool,
}

/// What this CPU offers (CPUID).
pub fn features() -> Features {
    let max = __cpuid(0).eax;
    let (b7, c7, d7) = if max >= 7 {
        let r = __cpuid_count(7, 0);
        (r.ebx, r.ecx, r.edx)
    } else {
        (0, 0, 0)
    };
    let nx = __cpuid(0x8000_0000).eax >= 0x8000_0001 && __cpuid(0x8000_0001).edx & (1 << 20) != 0;
    Features {
        smep: b7 & (1 << 7) != 0,
        smap: b7 & (1 << 20) != 0,
        umip: c7 & (1 << 2) != 0,
        nx,
        ibrs: d7 & (1 << 26) != 0,
        stibp: d7 & (1 << 27) != 0,
        ssbd: d7 & (1 << 31) != 0,
        md_clear: d7 & (1 << 10) != 0,
    }
}

/// Turns on SMEP, SMAP and UMIP on the calling CPU, where available.
/// Every CPU runs this (after `fpu::init`, which sets CR0.WP).
pub fn init() {
    let f = features();
    unsafe {
        Cr0::update(|c| c.insert(Cr0Flags::WRITE_PROTECT));
        let mut cr4 = Cr4::read_raw();
        if f.smep {
            cr4 |= Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION.bits();
        }
        if f.smap {
            cr4 |= Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION.bits();
        }
        if f.umip {
            cr4 |= CR4_UMIP;
        }
        Cr4::write_raw(cr4);
    }
    let cpu = super::percpu::cpu_id().min(super::percpu::MAX_CPUS - 1);
    CR4_OF[cpu].store(Cr4::read_raw(), Ordering::Relaxed);
    INITS_OF[cpu].fetch_add(1, Ordering::Relaxed);
    // All CPUs of a machine report the same features; the flags only say
    // what the first one found (they are never turned off again).
    SMEP_ON.fetch_or(f.smep, Ordering::Relaxed);
    UMIP_ON.fetch_or(f.umip, Ordering::Relaxed);
    if f.smap {
        AURORA_SMAP.store(true, Ordering::Release);
    }
}

/// Whether every online CPU turned on what CPUID offers (tests), and how
/// many times `init` ran on CPU 0 (once more after each wake from sleep).
#[cfg(feature = "ktest")]
pub fn every_cpu() -> (bool, u64) {
    let want = wanted_cr4();
    let ok = super::percpu::online().all(|c| CR4_OF[c].load(Ordering::Relaxed) & want == want);
    (ok, INITS_OF[0].load(Ordering::Relaxed))
}

/// The CR4 protection bits this CPU should have on (tests).
#[cfg(feature = "ktest")]
pub fn wanted_cr4() -> u64 {
    let f = features();
    (if f.smep { Cr4Flags::SUPERVISOR_MODE_EXECUTION_PROTECTION.bits() } else { 0 })
        | if f.smap { Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION.bits() } else { 0 }
        | if f.umip { CR4_UMIP } else { 0 }
}

pub fn smap() -> bool {
    AURORA_SMAP.load(Ordering::Relaxed)
}

pub fn smep() -> bool {
    SMEP_ON.load(Ordering::Relaxed)
}

pub fn umip() -> bool {
    UMIP_ON.load(Ordering::Relaxed)
}

// ------------------------------------------------- kernel image permissions

const PRESENT: u64 = 1;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

extern "C" {
    static __kernel_start: u8;
    static __text_end: u8;
    static __rodata_end: u8;
    static __kernel_end: u8;
}

fn sym(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// The 4 KiB page-table entry for kernel address `va` in the tables at `pml4`.
fn leaf(pml4: u64, va: u64) -> Option<&'static mut u64> {
    let mut t = pml4;
    for level in (1..=3).rev() {
        let e = unsafe { *(phys_to_virt(t) as *const u64).add(((va >> (12 + 9 * level)) & 0x1FF) as usize) };
        if e & PRESENT == 0 || e & HUGE != 0 {
            return None;
        }
        t = e & ADDR;
    }
    Some(unsafe { &mut *(phys_to_virt(t) as *mut u64).add(((va >> 12) & 0x1FF) as usize) })
}

/// Maps the kernel image W^X and makes every other kernel PML4 slot
/// no-execute. Runs once on the boot CPU, after `vmm::init` (EFER.NXE on,
/// the kernel half pre-populated) and before any other CPU or address space
/// copies the kernel half.
pub fn protect_kernel(pml4: u64) {
    let (start, text_end, ro_end, end) = unsafe {
        (sym(&__kernel_start), sym(&__text_end), sym(&__rodata_end), sym(&__kernel_end))
    };
    let mut pages = 0;
    let mut va = start;
    while va < end {
        if let Some(e) = leaf(pml4, va) {
            *e = if va < text_end {
                *e & !(WRITABLE | NO_EXECUTE)
            } else if va < ro_end {
                (*e & !WRITABLE) | NO_EXECUTE
            } else {
                *e | NO_EXECUTE
            };
            // The image is mapped global: a CR3 reload would not drop it.
            x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
            pages += 1;
        }
        va += 4096;
    }
    IMAGE_PAGES.store(pages, Ordering::Relaxed);
    // Everything but the last slot (the kernel image, at -2 GiB) is data:
    // the physical window (an alias of all RAM, kernel code included),
    // MMIO, window surfaces and kernel stacks.
    let kernel_slot = ((start >> 39) & 0x1FF) as usize;
    let top = unsafe { &mut *(phys_to_virt(pml4) as *mut [u64; 512]) };
    for (i, e) in top.iter_mut().enumerate().skip(256) {
        if i != kernel_slot && *e & PRESENT != 0 {
            *e |= NO_EXECUTE;
        }
    }
    // Large and global pages of the physical window: drop them all.
    unsafe {
        let cr4 = Cr4::read_raw();
        Cr4::write_raw(cr4 & !Cr4Flags::PAGE_GLOBAL.bits());
        Cr4::write_raw(cr4);
    }
    x86_64::instructions::tlb::flush_all();
}

/// One line for the boot log: what is on, what the CPU lacks.
pub fn report() -> alloc::string::String {
    let f = features();
    let on = |b: bool| if b { "on" } else { "unavailable" };
    let wp = Cr0::read().contains(Cr0Flags::WRITE_PROTECT);
    alloc::format!(
        "SMEP {}, SMAP {}, UMIP {}, NX {}, WP {}, kernel W^X ({} pages), stack protector {}; \
         speculation: IBRS {}, STIBP {}, SSBD {}, MD_CLEAR {} (none enabled)",
        on(smep()),
        on(smap()),
        on(umip()),
        on(f.nx),
        if wp { "on" } else { "off" },
        IMAGE_PAGES.load(Ordering::Relaxed),
        if crate::stackguard::active() { "on" } else { "off" },
        if f.ibrs { "available" } else { "absent" },
        if f.stibp { "available" } else { "absent" },
        if f.ssbd { "available" } else { "absent" },
        if f.md_clear { "available" } else { "absent" },
    )
}
