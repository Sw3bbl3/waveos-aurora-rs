//! 64-bit Arm (AArch64) architecture support, for QEMU's `virt` machine
//! under HVF or TCG and, later, hardware with UEFI and ACPI.
//!
//! The kernel runs at EL1 (the bootloader drops from EL2 when the firmware
//! ran there), with 4 KiB pages and 48-bit addresses: TTBR1 holds the
//! kernel half (the same layout as on x86-64), TTBR0 the running process's
//! user half. See docs/developer/porting.md.

pub mod cache;
pub mod clock;
pub mod cpu;
pub mod fpu;
pub mod interrupts;
pub mod io;
pub mod irq;
pub mod mmu;
pub mod percpu;
pub mod power;
pub mod protect;
pub mod psci;
pub mod regs;
pub mod rng;
pub mod smp;
pub mod switch;
pub mod syscall;
pub mod timer;
pub mod trap;
pub mod uaccess;

/// What the boot log says once [`early_init`] is done.
pub const EARLY_INIT_DONE: &str = "exception vectors and FP/SIMD access set up";

/// Early set-up of the boot CPU: per-CPU register, exception vectors,
/// FP/SIMD access for user state, protections. Runs right after the
/// serial console, before memory management.
pub fn early_init() {
    percpu::set_this(0);
    trap::init();
    fpu::init();
    protect::init();
}

/// The interrupt controller and the timer interrupt, once ACPI is parsed
/// and the clock runs; then this CPU's per-CPU data.
pub fn init_interrupts(acpi: &crate::acpi::AcpiInfo) {
    psci::init(acpi);
    percpu::init(0);
}

/// Where Arm bring-up stops for now (milestone 1): before interrupts are
/// enabled, which needs the GIC driver. Reports what runs and powers off.
pub fn bringup_done() -> ! {
    log!("boot", "CPU: {} ({}), EL{}", cpu::brand(), cpu::model(), regs::current_el());
    log!("boot", "features: {}", cpu::feature_names().join(" "));
    log!("boot", "protections: {}", protect::report());
    let (total, free) = crate::mm::frame::counts();
    log!("boot", "memory: {} MiB, {} MiB free", total * 4096 >> 20, free * 4096 >> 20);
    log!("boot", "aarch64 bring-up complete up to the scheduler; interrupts (GIC) come next. Powering off.");
    power::off();
}

/// No legacy (ISA) input devices on Arm platforms.
pub fn route_legacy_input(_acpi: &crate::acpi::AcpiInfo) {}
