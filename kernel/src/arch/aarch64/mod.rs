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
pub mod gic;
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
    cpu::init_el0();
    protect::init();
}

/// The interrupt controller and the timer interrupt, once ACPI is parsed
/// and the clock runs; then this CPU's per-CPU data.
pub fn init_interrupts(acpi: &crate::acpi::AcpiInfo) {
    psci::init(acpi);
    percpu::init(0);
    let _ = cpu::id_regs(); // read once, on the boot CPU
    log!(
        "boot",
        "CPU: {} ({}), entered at EL{}, CTR_EL0 {:#x}",
        cpu::brand(),
        cpu::model(),
        regs::current_el(),
        regs::read!("ctr_el0")
    );
    smp::set_cpus(&acpi.gic.cpus);
    if gic::init(acpi) {
        timer::choose(None);
        log!("timer", "{}", timer::describe());
        timer::start_local();
        crate::timer::reprogram();
    }
}

/// No legacy (ISA) input devices on Arm platforms.
pub fn route_legacy_input(_acpi: &crate::acpi::AcpiInfo) {}
