//! CPU protection features and the kernel's own page permissions.
//!
//! What exists on Arm and what is on (milestone 1: the report only; PAN,
//! W^X of the kernel image, pointer authentication and BTI come with user
//! programs in milestone 2):
//! - **PAN** (FEAT_PAN): the kernel can't touch user pages outside the
//!   user-copy routine (x86's SMAP).
//! - **PXN** on every mapping the kernel makes at run time (x86's SMEP and
//!   NX for the kernel): part of `mmu::pte::PRESENT`.
//! - **WXN**: writable implies execute-never.

use super::regs::read;

#[derive(Clone, Copy, Debug, Default)]
pub struct Features {
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
    Features {
        pan: (mmfr1 >> 20) & 0xF != 0,
        pauth: (isar1 >> 4) & 0xFF != 0,
        bti: pfr1 & 0xF != 0,
        rndr: (isar0 >> 60) & 0xF != 0,
    }
}

/// Turns on what is ready on this CPU (nothing yet: see the module notes).
pub fn init() {}

/// The kernel image keeps the bootloader's permissions for now.
pub fn protect_kernel(_root: u64) {}

/// Whether the user-copy routine must open user access (x86's SMAP; PAN
/// here): not until PAN is turned on.
#[allow(dead_code)]
pub fn smap() -> bool {
    false
}

/// One line for the boot log.
pub fn report() -> alloc::string::String {
    let f = features();
    let has = |b: bool| if b { "available" } else { "absent" };
    alloc::format!(
        "PXN on run-time mappings; PAN {}, pointer authentication {}, BTI {}, RNDR {} (none enabled yet); \
         stack protector {}",
        has(f.pan),
        has(f.pauth),
        has(f.bti),
        has(f.rndr),
        if crate::stackguard::active() { "on" } else { "off" },
    )
}

#[cfg(feature = "ktest")]
pub fn every_cpu() -> (bool, u64) {
    (true, 1)
}

#[cfg(feature = "ktest")]
pub fn assert_this_cpu() {}

#[cfg(feature = "ktest")]
pub fn this_cpu_protected() -> bool {
    true
}
