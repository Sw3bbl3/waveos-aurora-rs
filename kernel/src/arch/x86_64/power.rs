//! The last steps of power off and restart, after ACPI (`power`) had its go.

use super::io::Port;

/// Powers off where ACPI could not: the emulators' own ports. Interrupts off.
pub fn off() -> ! {
    // SAFETY: writes to fixed emulator ports; on real hardware nothing
    // decodes them, and the machine is going down either way.
    unsafe {
        // Emulator fallbacks: QEMU (older), Bochs, VirtualBox.
        Port::<u16>::new(0x604).write(0x2000);
        Port::<u16>::new(0xB004).write(0x2000);
        Port::<u16>::new(0x4004).write(0x3400);
    }
    super::cpu::halt_forever();
}

/// Restarts the machine: the FADT reset register (`reset_port`: port and
/// value), port CF9, the keyboard controller, and at last a triple fault.
pub fn reset(reset_port: Option<(u16, u8)>) -> ! {
    // SAFETY: each step either resets the machine or does nothing; the
    // empty IDT makes the final `int3` a triple fault.
    unsafe {
        if let Some((port, value)) = reset_port {
            Port::<u8>::new(port).write(value);
        }
        Port::<u8>::new(0xCF9).write(0x06);
        // Keyboard controller pulse reset line.
        for _ in 0..10_000 {
            if Port::<u8>::new(0x64).read() & 0x02 == 0 {
                break;
            }
        }
        Port::<u8>::new(0x64).write(0xFE);
        // Last resort: triple fault with an empty IDT.
        let idt = x86_64::structures::DescriptorTablePointer { limit: 0, base: x86_64::VirtAddr::new(0) };
        x86_64::instructions::tables::lidt(&idt);
        core::arch::asm!("int3");
    }
    super::cpu::halt_forever();
}

/// Exits QEMU via the isa-debug-exit device (status = (code << 1) | 1).
pub fn qemu_exit(code: u32) -> ! {
    // SAFETY: the test machine's debug-exit port; elsewhere ignored.
    unsafe { Port::<u32>::new(0xF4).write(code) };
    super::cpu::halt_forever();
}
