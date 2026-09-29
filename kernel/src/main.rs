//! Tide — the WaveOS Aurora kernel.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
// The test build skips the desktop, which leaves parts of it unused.
#![cfg_attr(feature = "ktest", allow(dead_code))]

extern crate alloc;

#[macro_use]
mod drivers;
mod acpi;
mod arch;
mod fs;
mod gui;
mod mm;
mod power;
mod proc;
mod sched;
mod sync;
mod syscall;
mod telemetry;
#[cfg(feature = "ktest")]
mod tests;
mod time;

use bootinfo::{BootInfo, BOOTINFO_MAGIC};
use core::panic::PanicInfo;
use drivers::input;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[no_mangle]
#[link_section = ".text._start"]
pub extern "sysv64" fn _start(boot_info: &'static BootInfo) -> ! {
    drivers::serial::init();
    telemetry::init();
    telemetry::stage("kernel", "Tide kernel entry");
    kprintln!("\nWaveOS Aurora — Tide kernel {}", VERSION);
    assert_eq!(boot_info.magic, BOOTINFO_MAGIC, "bad BootInfo magic");
    assert_eq!(boot_info.version, bootinfo::BOOTINFO_VERSION, "bootloader/kernel version mismatch");

    telemetry::stage("cpu", "CPU tables: GDT, TSS, IDT, syscall");
    arch::gdt::init();
    arch::idt::init();
    arch::syscall::init();
    log!("boot", "GDT, TSS and IDT loaded");

    telemetry::stage("memory", "Memory: frames, heap, page tables");
    mm::init(boot_info);

    telemetry::stage("interrupts", "ACPI, APIC timer and interrupt routing");
    let acpi = acpi::parse(boot_info.rsdp_phys);
    arch::apic::init(&acpi);

    let fb = boot_info.framebuffer;
    input::set_screen_size(fb.width as i32, fb.height as i32);
    telemetry::stage("input", "Keyboard and mouse");
    drivers::ps2::init();
    arch::apic::route_isa_irq(&acpi, 1, arch::idt::KEYBOARD_VECTOR);
    arch::apic::route_isa_irq(&acpi, 12, arch::idt::MOUSE_VECTOR);
    let ecam = acpi.ecam;
    power::init(acpi);

    time::init_wall_clock();
    telemetry::stage("vfs", "Virtual filesystem and system image");
    fs::init(initrd(boot_info));
    gui::init(fb);

    telemetry::stage("scheduler", "Scheduler and process manager");
    sched::init();
    proc::init();
    x86_64::instructions::interrupts::enable();
    log!("boot", "interrupts enabled; CPU: {}", arch::cpu::brand());

    // Storage needs a running clock (timeouts) and scheduler (drivers yield while polling).
    telemetry::stage("storage", "PCI, disk drivers and filesystems");
    drivers::pci::init(ecam);
    drivers::block::init();
    fs::mount_disks();
    telemetry::start();

    #[cfg(feature = "ktest")]
    sched::spawn("ktest", tests::run);
    #[cfg(not(feature = "ktest"))]
    gui::start();

    sched::idle();
}

/// The system image loaded by the bootloader (empty if none).
fn initrd(boot_info: &BootInfo) -> &'static [u8] {
    if boot_info.initrd_len == 0 {
        return &[];
    }
    let ptr = mm::phys_to_virt(boot_info.initrd_phys) as *const u8;
    unsafe { core::slice::from_raw_parts(ptr, boot_info.initrd_len as usize) }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();
    unsafe { drivers::serial::force_unlock() };
    kprintln!("\n*** KERNEL PANIC: {}", info);
    if cfg!(feature = "ktest") {
        power::qemu_exit(0x11);
    }
    let mut text = gui::StackText::<2048>::new();
    let _ = core::fmt::Write::write_fmt(&mut text, format_args!("{}", info));
    gui::panic_screen(text.as_str());
    arch::cpu::halt_forever();
}
