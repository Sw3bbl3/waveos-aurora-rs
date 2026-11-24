//! CPU identification, the cycle counter and small CPU operations.

use super::regs::{self, read};
use alloc::string::String;
use alloc::vec::Vec;

/// MIDR_EL1: implementer, variant, architecture, part number, revision.
fn midr() -> u64 {
    read!("midr_el1")
}

fn implementer_name(code: u64) -> &'static str {
    match code {
        0x00 => "QEMU",
        0x41 => "Arm",
        0x42 => "Broadcom",
        0x43 => "Cavium",
        0x46 => "Fujitsu",
        0x48 => "HiSilicon",
        0x4E => "Nvidia",
        0x50 => "Applied Micro",
        0x51 => "Qualcomm",
        0x61 => "Apple",
        0x63 => "Arm China",
        0x6D => "Microsoft",
        0xC0 => "Ampere",
        _ => "unknown",
    }
}

/// A name for the processor, from MIDR_EL1 (Arm has no brand string).
pub fn brand() -> String {
    let m = midr();
    let part = (m >> 4) & 0xFFF;
    let name = match ((m >> 24) & 0xFF, part) {
        (0x41, 0xD03) => "Cortex-A53",
        (0x41, 0xD07) => "Cortex-A57",
        (0x41, 0xD08) => "Cortex-A72",
        (0x41, 0xD0B) => "Cortex-A76",
        (0x41, 0xD0C) => "Neoverse-N1",
        (0x41, 0xD40) => "Neoverse-V1",
        (0x41, 0xD49) => "Neoverse-N2",
        (0x00, 0x051) => "QEMU max CPU",
        (0x61, 0x022 | 0x024 | 0x028) => "Apple M1 (Icestorm)",
        (0x61, 0x023 | 0x025 | 0x029) => "Apple M1 (Firestorm)",
        (0x61, 0x032 | 0x034 | 0x038) => "Apple M2 (Blizzard)",
        (0x61, 0x033 | 0x035 | 0x039) => "Apple M2 (Avalanche)",
        _ => "",
    };
    if name.is_empty() {
        alloc::format!("{} part {:#05x}", vendor(), part)
    } else {
        alloc::format!("{} {}", vendor(), name)
    }
}

pub fn vendor() -> String {
    String::from(implementer_name((midr() >> 24) & 0xFF))
}

/// Part, variant and revision, for `cpuinfo`.
pub fn model() -> String {
    let m = midr();
    alloc::format!("part {:#05x}, variant {}, revision {}", (m >> 4) & 0xFFF, (m >> 20) & 0xF, m & 0xF)
}

/// Feature fields of the ID registers (4 bits each).
fn field(reg: u64, shift: u32) -> u64 {
    (reg >> shift) & 0xF
}

/// Notable features this CPU has, for `cpuinfo`.
pub fn feature_names() -> Vec<&'static str> {
    let isar0 = read!("id_aa64isar0_el1");
    let isar1 = read!("id_aa64isar1_el1");
    let pfr0 = read!("id_aa64pfr0_el1");
    let pfr1 = read!("id_aa64pfr1_el1");
    let mmfr1 = read!("id_aa64mmfr1_el1");
    [
        (field(isar0, 4) >= 1, "aes"),
        (field(isar0, 4) >= 2, "pmull"),
        (field(isar0, 8) >= 1, "sha1"),
        (field(isar0, 12) >= 1, "sha256"),
        (field(isar0, 16) >= 1, "crc32"),
        (field(isar0, 20) >= 2, "atomics"),
        (field(isar0, 60) >= 1, "rndr"),
        (field(isar1, 4) != 0 || field(isar1, 8) != 0, "pauth"),
        (field(pfr0, 16) != 0xF, "fp"),
        (field(pfr0, 20) != 0xF, "asimd"),
        (field(pfr0, 32) != 0, "sve"),
        (field(pfr1, 0) != 0, "bti"),
        (field(mmfr1, 0) >= 1, "hafdbs"),
        (field(mmfr1, 20) >= 1, "pan"),
    ]
    .iter()
    .filter(|f| f.0)
    .map(|f| f.1)
    .collect()
}

/// MIDR_EL1's implementer, variant, part number and revision.
pub fn midr_fields() -> (u64, u64, u64, u64) {
    let m = midr();
    ((m >> 24) & 0xFF, (m >> 20) & 0xF, (m >> 4) & 0xFFF, m & 0xF)
}

/// Linux's arm64 AT_HWCAP bits and `/proc/cpuinfo` names, by what the ID
/// registers report (Linux's `arch/arm64/include/uapi/asm/hwcap.h` numbering).
fn hwcaps() -> [(bool, u32, &'static str); 12] {
    let isar0 = read!("id_aa64isar0_el1");
    let isar1 = read!("id_aa64isar1_el1");
    let pfr0 = read!("id_aa64pfr0_el1");
    [
        (field(pfr0, 16) != 0xF, 0, "fp"),
        (field(pfr0, 20) != 0xF, 1, "asimd"),
        (field(isar0, 4) >= 1, 3, "aes"),
        (field(isar0, 4) >= 2, 4, "pmull"),
        (field(isar0, 8) >= 1, 5, "sha1"),
        (field(isar0, 12) >= 1, 6, "sha2"),
        (field(isar0, 16) >= 1, 7, "crc32"),
        (field(isar0, 20) >= 2, 8, "atomics"),
        (field(pfr0, 16) == 1, 9, "fphp"),
        (field(pfr0, 20) == 1, 10, "asimdhp"),
        (field(isar0, 28) >= 1, 12, "asimdrdm"),
        (field(isar1, 0) >= 1, 16, "dcpop"),
    ]
}

/// What Linux programs get as AT_HWCAP.
pub fn linux_hwcap() -> u64 {
    hwcaps().iter().filter(|h| h.0).fold(0, |a, h| a | 1 << h.1)
}

/// The `Features` line of `/proc/cpuinfo`.
pub fn linux_features() -> String {
    let names: Vec<&str> = hwcaps().iter().filter(|h| h.0).map(|h| h.2).collect();
    names.join(" ")
}

/// Whether the kernel runs under a hypervisor. Arm has no architectural
/// bit for it; the kernel assumes it might (QEMU under HVF or TCG is the
/// only platform so far), which only makes idle polling a little longer.
pub fn hypervisor() -> bool {
    true
}

/// The generic timer's count runs at a constant rate (CNTFRQ_EL0) in
/// every state, by the architecture.
pub fn invariant_counter() -> bool {
    true
}

/// The cycle counter: the generic timer's virtual count (CNTVCT_EL0).
#[inline]
pub fn cycles() -> u64 {
    read!("cntvct_el0")
}

/// The count, read only once earlier instructions are done (`isb`).
#[cfg(feature = "ktest")]
#[inline]
pub fn cycles_ordered() -> u64 {
    regs::isb();
    read!("cntvct_el0")
}

/// Waits for the next interrupt (`wfi`). With interrupts masked it still
/// returns when one is pending.
#[inline]
pub fn halt() {
    // SAFETY: waits; no memory effect.
    unsafe { core::arch::asm!("wfi", options(nomem, nostack, preserves_flags)) };
}

pub fn halt_forever() -> ! {
    super::interrupts::disable();
    loop {
        halt();
    }
}

/// The caller's frame pointer (x29): the kernel keeps frame pointers, and a
/// frame record is (previous frame pointer, return address), as on x86-64.
#[inline(always)]
pub fn frame_pointer() -> u64 {
    let fp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, x29", out(reg) fp, options(nomem, nostack, preserves_flags)) };
    fp
}

/// Sets the user thread pointer (TPIDR_EL0) of the calling CPU.
pub fn set_thread_pointer(base: u64) {
    // SAFETY: TPIDR_EL0 is the user's thread register; the kernel never uses it.
    unsafe { regs::write!("tpidr_el0", base) };
}

/// Arm has no MONITOR/MWAIT: `wfi` (and `wfe` with an event stream) only.
pub fn mwait_supported() -> bool {
    false
}

pub fn mwait_substates() -> u32 {
    0
}

/// Never used (no MONITOR/MWAIT): waits for an interrupt unless `cancel()`.
pub fn monitor_mwait(_line: *const u8, _hint: u32, cancel: impl Fn() -> bool) {
    if !cancel() {
        halt();
    }
    super::interrupts::enable();
}

/// The CPU's affinity (MPIDR_EL1 Aff3..Aff0), the hardware CPU id that
/// PSCI and the GIC use.
pub fn mpidr() -> u64 {
    read!("mpidr_el1") & 0xFF_00FF_FFFF
}
