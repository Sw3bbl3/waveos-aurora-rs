//! PSCI, the firmware interface for CPU start-up, power off and reset.
//!
//! The FADT's ARM_BOOT_ARCH flags say whether PSCI is there and whether to
//! call it with `hvc` (a hypervisor implements it: QEMU's `virt` without
//! EL2, HVF) or `smc` (secure firmware, or QEMU with `virtualization=on`).

use core::sync::atomic::{AtomicU8, Ordering};

pub const SYSTEM_OFF: u32 = 0x8400_0008;
pub const SYSTEM_RESET: u32 = 0x8400_0009;
pub const CPU_ON: u64 = 0xC400_0003;
pub const PSCI_VERSION: u32 = 0x8400_0000;

/// 0: unknown (try `hvc`), 1: `hvc`, 2: `smc`.
static CONDUIT: AtomicU8 = AtomicU8::new(0);

/// Takes the conduit from the FADT.
pub fn init(acpi: &crate::acpi::AcpiInfo) {
    let flags = acpi.arm_boot_arch;
    let conduit = if flags & 2 != 0 {
        1
    } else if flags & 1 != 0 {
        2
    } else {
        0
    };
    CONDUIT.store(conduit, Ordering::Relaxed);
    if flags & 1 != 0 {
        let v = call(PSCI_VERSION as u64, 0, 0, 0);
        log!("psci", "PSCI {}.{} via {}", v >> 16, v & 0xFFFF, if conduit == 2 { "smc" } else { "hvc" });
    } else {
        log!("psci", "the firmware describes no PSCI; trying hvc");
    }
}

/// Calls PSCI function `f`; returns x0.
pub fn call(f: u64, a: u64, b: u64, c: u64) -> u64 {
    let mut r = f;
    // SAFETY: an SMC Calling Convention call: x0-x3 in, x0-x3 out (x1-x17
    // may be clobbered), no memory touched by the firmware we gave it.
    unsafe {
        if CONDUIT.load(Ordering::Relaxed) == 2 {
            core::arch::asm!("smc #0", inout("x0") r, inout("x1") a => _, inout("x2") b => _, inout("x3") c => _,
                out("x4") _, out("x5") _, out("x6") _, out("x7") _, out("x8") _, out("x9") _, out("x10") _,
                out("x11") _, out("x12") _, out("x13") _, out("x14") _, out("x15") _, out("x16") _, out("x17") _,
                options(nostack));
        } else {
            core::arch::asm!("hvc #0", inout("x0") r, inout("x1") a => _, inout("x2") b => _, inout("x3") c => _,
                out("x4") _, out("x5") _, out("x6") _, out("x7") _, out("x8") _, out("x9") _, out("x10") _,
                out("x11") _, out("x12") _, out("x13") _, out("x14") _, out("x15") _, out("x16") _, out("x17") _,
                options(nostack));
        }
    }
    r
}
