//! Shutdown, restart, and the QEMU test-exit device.

use crate::acpi::AcpiInfo;
use spin::Once;
use x86_64::instructions::port::Port;

static ACPI: Once<AcpiInfo> = Once::new();

pub fn init(info: AcpiInfo) {
    ACPI.call_once(|| info);
}

pub fn shutdown() -> ! {
    log!("power", "shutting down");
    x86_64::instructions::interrupts::disable();
    if let Some(a) = ACPI.get() {
        if a.pm1a_cnt != 0 {
            unsafe {
                let mut cnt = Port::<u16>::new(a.pm1a_cnt);
                // Switch to ACPI mode first if firmware left us in legacy mode.
                if cnt.read() & 1 == 0 && a.smi_cmd != 0 && a.acpi_enable != 0 {
                    Port::<u8>::new(a.smi_cmd).write(a.acpi_enable);
                    for _ in 0..1_000_000 {
                        if cnt.read() & 1 != 0 {
                            break;
                        }
                    }
                }
                cnt.write((a.slp_typa << 10) | (1 << 13));
                if a.pm1b_cnt != 0 {
                    Port::<u16>::new(a.pm1b_cnt).write((a.slp_typb << 10) | (1 << 13));
                }
            }
        }
    }
    unsafe {
        // Emulator fallbacks: QEMU (older), Bochs, VirtualBox.
        Port::<u16>::new(0x604).write(0x2000);
        Port::<u16>::new(0xB004).write(0x2000);
        Port::<u16>::new(0x4004).write(0x3400);
    }
    crate::arch::cpu::halt_forever();
}

pub fn reboot() -> ! {
    log!("power", "restarting");
    x86_64::instructions::interrupts::disable();
    unsafe {
        if let Some((port, value)) = ACPI.get().and_then(|a| a.reset_port) {
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
    crate::arch::cpu::halt_forever();
}

/// Exits QEMU via the isa-debug-exit device (status = (code << 1) | 1).
#[allow(dead_code)]
pub fn qemu_exit(code: u32) -> ! {
    unsafe { Port::<u32>::new(0xF4).write(code) };
    crate::arch::cpu::halt_forever();
}
