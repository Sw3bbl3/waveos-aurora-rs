//! Aster — the WaveOS Aurora kernel.

#![no_std]
#![no_main]
#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]
// The test build skips the desktop, which leaves parts of it unused.
#![cfg_attr(feature = "ktest", allow(dead_code))]
// Arm bring-up stops before the device drivers start (`arch::bringup_done`),
// which leaves much of the kernel unused there for now.
#![cfg_attr(target_arch = "aarch64", allow(dead_code, unreachable_code, unused_imports, unused_variables))]

extern crate alloc;

#[macro_use]
mod drivers;
mod acpi;
mod arch;
mod compat;
mod fs;
mod gui;
mod linux;
mod mm;
mod net;
mod power;
mod proc;
mod random;
mod report;
mod sandbox;
mod sched;
mod stackguard;
mod sync;
mod syscall;
mod telemetry;
#[cfg(feature = "ktest")]
mod tests;
mod time;
mod timer;
mod users;

use bootinfo::{BootInfo, BOOTINFO_MAGIC};
use core::panic::PanicInfo;
use drivers::input;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The kernel's entry from the bootloader (`bootinfo::KernelEntry`), on
/// the boot stack with interrupts off and the bootloader's page tables.
#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[link_section = ".text._start"]
pub extern "sysv64" fn _start(boot_info: &'static BootInfo) -> ! {
    kernel_main(boot_info)
}

#[cfg(target_arch = "aarch64")]
#[no_mangle]
#[link_section = ".text._start"]
pub extern "C" fn _start(boot_info: &'static BootInfo) -> ! {
    kernel_main(boot_info)
}

#[inline(always)]
fn kernel_main(boot_info: &'static BootInfo) -> ! {
    // First of all: nothing protected has been entered yet that will return.
    stackguard::init();
    drivers::serial::init();
    telemetry::init();
    telemetry::stage("kernel", "Aster kernel entry");
    kprintln!("\nWaveOS Aurora — Aster kernel {}", VERSION);
    assert_eq!(boot_info.magic, BOOTINFO_MAGIC, "bad BootInfo magic");
    assert_eq!(boot_info.version, bootinfo::BOOTINFO_VERSION, "bootloader/kernel version mismatch");

    telemetry::stage("cpu", "CPU tables: GDT, TSS, IDT, syscall");
    arch::early_init();
    log!("boot", "{}", arch::EARLY_INIT_DONE);

    telemetry::stage("memory", "Memory: frames, heap, page tables");
    mm::init(boot_info);

    telemetry::stage("interrupts", "ACPI, clocks, APIC timer and interrupt routing");
    let acpi = acpi::parse(boot_info.rsdp_phys);
    #[cfg(target_arch = "x86_64")]
    drivers::hpet::init(acpi.hpet);
    time::init_clock();
    arch::init_interrupts(&acpi);
    let cpu_ids = acpi.cpu_apic_ids.clone();

    let fb = boot_info.framebuffer;
    input::set_screen_size(fb.width as i32, fb.height as i32);
    telemetry::stage("input", "Keyboard and mouse");
    drivers::keymap::validate();
    #[cfg(target_arch = "x86_64")]
    drivers::ps2::init();
    arch::route_legacy_input(&acpi);
    let ecam = acpi.ecam;
    power::init(acpi);

    time::init_wall_clock();
    telemetry::stage("vfs", "Virtual filesystem and system image");
    fs::init(initrd(boot_info));
    gui::init(fb, boot_info.flags & bootinfo::BOOT_DARK != 0);
    gui::sysupdate::init(boot_info.flags);

    telemetry::stage("scheduler", "Scheduler and process manager");
    sched::init();
    proc::init();
    sandbox::init();
    mm::kswapd::init();
    // Arm bring-up: no interrupt controller driver yet, so it ends here.
    #[cfg(target_arch = "aarch64")]
    arch::bringup_done();
    arch::interrupts::enable();
    log!("boot", "interrupts enabled; CPU: {}", arch::cpu::brand());
    log!("boot", "protections: {}", arch::protect::report());

    telemetry::stage("smp", "Starting the other CPUs");
    arch::smp::start_aps(&cpu_ids, boot_info.trampoline_phys);

    // Storage needs a running clock (timeouts) and scheduler (drivers yield while polling).
    telemetry::stage("storage", "PCI, disk drivers and filesystems");
    drivers::pci::init(ecam);
    drivers::display::init(boot_info);
    drivers::block::init();
    fs::mount_disks();
    // boot.conf is readable now: SD options, and removable cards' volumes.
    drivers::block::sdhci::start();
    // After the disks: boot.conf chooses the virtio-gpu driver.
    telemetry::stage("gpu", "GPU drivers");
    drivers::display::gpu_start();
    telemetry::stage("network", "Network cards and the TCP/IP stack");
    net::init();
    drivers::net::init();
    drivers::wifi::init();
    telemetry::stage("usb", "USB controllers and devices");
    drivers::input::start();
    drivers::usb::init();
    if !fs::root_persistent() {
        // Perhaps we booted from a USB stick that holds the home volume.
        drivers::usb::settle(4000);
    }
    telemetry::stage("sound", "Sound output");
    drivers::audio::init();
    telemetry::stage("acpi", "ACPI runtime (AML, power button, battery)");
    acpi::start(boot_info.rsdp_phys);
    power::start();
    telemetry::start();
    // Accounts and file owners, once the home volume is there.
    users::init();

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
    arch::interrupts::disable();
    // The AML interpreter runs on its own task; if it fails, only ACPI stops.
    if acpi::is_acpi_task() {
        unsafe { drivers::serial::force_unlock() };
        kprintln!("\n*** ACPI task panicked: {}", info);
        acpi::on_panic();
    }
    arch::smp::halt_others();
    unsafe { drivers::serial::force_unlock() };
    kprintln!("\n*** KERNEL PANIC: {}", info);
    if cfg!(feature = "ktest") {
        sched::dump_stacks();
        power::qemu_exit(0x11);
    }
    let mut text = gui::StackText::<2048>::new();
    let _ = core::fmt::Write::write_fmt(&mut text, format_args!("{}", info));
    gui::panic_screen(text.as_str());
    arch::cpu::halt_forever();
}
