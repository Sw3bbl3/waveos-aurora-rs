//! The last steps of power off and restart: PSCI.

use super::psci;

/// Powers off (PSCI SYSTEM_OFF). Interrupts off.
pub fn off() -> ! {
    psci::call(psci::SYSTEM_OFF as u64, 0, 0, 0);
    super::cpu::halt_forever();
}

/// Restarts the machine (PSCI SYSTEM_RESET). The FADT's reset register is
/// an x86 I/O port, which Arm platforms don't have.
pub fn reset(_reset_port: Option<(u16, u8)>) -> ! {
    psci::call(psci::SYSTEM_RESET as u64, 0, 0, 0);
    super::cpu::halt_forever();
}

/// Ends a test run with `code`. QEMU's `virt` has no isa-debug-exit device:
/// the code goes to the console, where the test harness reads it, and the
/// machine powers off.
pub fn qemu_exit(code: u32) -> ! {
    crate::kprintln!("\n[aurora-exit {}]", code);
    off();
}
