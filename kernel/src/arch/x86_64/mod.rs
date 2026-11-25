//! x86_64 architecture support.

pub mod apic;
pub mod cache;
pub mod clock;
pub mod cpu;
pub mod fpu;
pub mod gdt;
pub mod idt;
pub mod interrupts;
pub mod io;
pub mod irq;
pub mod mmu;
pub mod msr;
pub mod pat;
pub mod percpu;
pub mod power;
pub mod protect;
pub mod rng;
pub mod s3;
pub mod smp;
pub mod switch;
pub mod syscall;
pub mod timer;
pub mod trap;
pub mod uaccess;

/// What the boot log says once [`early_init`] is done.
pub const EARLY_INIT_DONE: &str = "GDT, TSS and IDT loaded";

/// Early, per-boot-CPU set-up of the processor's own tables: GDT and TSS,
/// IDT, `syscall`, FPU, protections and PAT. Runs right after the serial
/// console, before memory management.
pub fn early_init() {
    gdt::init(0);
    idt::init();
    syscall::init();
    fpu::init();
    protect::init();
    pat::init();
}

/// The interrupt controller and the timer interrupt, once ACPI is parsed
/// and the clock runs; then this CPU's per-CPU data.
pub fn init_interrupts(acpi: &crate::acpi::AcpiInfo) {
    apic::init(acpi);
    percpu::init(0);
}

/// The platform's legacy input interrupts (PS/2 keyboard and mouse).
pub fn route_legacy_input(acpi: &crate::acpi::AcpiInfo) {
    apic::route_isa_irq(acpi, 1, idt::KEYBOARD_VECTOR);
    apic::route_isa_irq(acpi, 12, idt::MOUSE_VECTOR);
}
