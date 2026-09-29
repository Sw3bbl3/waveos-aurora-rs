//! Sleep (ACPI S3, suspend to RAM).
//!
//! Going to sleep: the other CPUs are parked, the firmware is told where to
//! resume (the FACS waking vector — our real-mode trampoline page), state the
//! hardware will lose is saved (IOAPIC routes, PCI configuration), the CPU's
//! registers are saved like `setjmp`, and SLP_TYP/SLP_EN powers down
//! everything but RAM.
//!
//! Waking: the firmware starts the boot CPU in real mode at the trampoline,
//! which enters long mode on a small stack and calls [`resume_entry`]. That
//! reloads the kernel's page tables, GDT/TSS, IDT, `syscall` and FPU setup,
//! then jumps back into [`suspend`] as if the save had just returned. From
//! there, in an ordinary task: clocks, interrupt controllers, PCI devices
//! and drivers come back, the other CPUs restart, and the desktop repaints.

use crate::arch::{apic, fpu, gdt, idt, percpu, smp, syscall};
use crate::mm::{phys_to_virt, vmm};
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::port::Port;

global_asm!(
    // aurora_s3_save(ctx): callee-saved registers, stack and return address; returns 0.
    ".global aurora_s3_save",
    "aurora_s3_save:",
    "mov [rdi], rbx",
    "mov [rdi + 8], rbp",
    "mov [rdi + 16], r12",
    "mov [rdi + 24], r13",
    "mov [rdi + 32], r14",
    "mov [rdi + 40], r15",
    "lea rax, [rsp + 8]",
    "mov [rdi + 48], rax",
    "mov rax, [rsp]",
    "mov [rdi + 56], rax",
    "xor eax, eax",
    "ret",
    // aurora_s3_restore(ctx): back into aurora_s3_save's caller, returning 1.
    ".global aurora_s3_restore",
    "aurora_s3_restore:",
    "mov rbx, [rdi]",
    "mov rbp, [rdi + 8]",
    "mov r12, [rdi + 16]",
    "mov r13, [rdi + 24]",
    "mov r14, [rdi + 32]",
    "mov r15, [rdi + 40]",
    "mov rsp, [rdi + 48]",
    "mov eax, 1",
    "jmp qword ptr [rdi + 56]",
);

extern "sysv64" {
    fn aurora_s3_save(ctx: *mut [u64; 8]) -> u64;
    fn aurora_s3_restore(ctx: *const [u64; 8]) -> !;
}

static mut CONTEXT: [u64; 8] = [0; 8];
static mut CR4: u64 = 0;

const RESUME_STACK: usize = 16 * 1024;
#[repr(align(16))]
#[allow(dead_code)] // only its address is used
struct Stack([u8; RESUME_STACK]);
static mut STACK: Stack = Stack([0; RESUME_STACK]);

static SLEEPING: AtomicBool = AtomicBool::new(false);

/// Whether this machine can sleep (AML gave `\_S3`, and we have a FACS and a trampoline page).
pub fn available() -> bool {
    crate::acpi::sleep_type(3).is_some() && crate::power::info().is_some_and(|a| a.facs != 0) && smp::trampoline() != 0
}

/// First code after waking, on the boot CPU with the trampoline's page tables.
extern "sysv64" fn resume_entry(_arg: u64) -> ! {
    unsafe {
        vmm::load(vmm::kernel_pml4());
        x86_64::registers::control::Cr4::write_raw((&raw const CR4).read());
    }
    gdt::reload(0);
    idt::init();
    syscall::init();
    fpu::init();
    percpu::init(0);
    unsafe { aurora_s3_restore(&raw const CONTEXT) }
}

fn inw(p: u16) -> u16 {
    unsafe { Port::<u16>::new(p).read() }
}
fn outw(p: u16, v: u16) {
    unsafe { Port::<u16>::new(p).write(v) }
}

/// Puts the machine to sleep and returns after it wakes (or if it could not
/// sleep). Must run on CPU 0 (see `sched::spawn_on_bsp`).
pub fn suspend() -> Result<(), &'static str> {
    if percpu::cpu_id() != 0 {
        return Err("sleep must be started on the boot CPU");
    }
    let info = crate::power::info().ok_or("no ACPI")?;
    let (typa, typb) = crate::acpi::sleep_type(3).ok_or("the firmware does not support sleep (S3)")?;
    if info.facs == 0 || smp::trampoline() == 0 {
        return Err("no waking vector");
    }
    if SLEEPING.swap(true, Ordering::AcqRel) {
        return Err("already going to sleep");
    }
    log!("power", "entering S3");
    crate::fs::sync_all();
    crate::acpi::prepare_sleep(3);
    let aps: alloc::vec::Vec<u8> =
        percpu::online().filter(|&c| c != 0).map(|c| percpu::CPUS[c].lapic_id.load(Ordering::Relaxed) as u8).collect();
    if !crate::sched::park_aps() {
        SLEEPING.store(false, Ordering::Release);
        crate::sched::unpark();
        return Err("could not stop the other processors");
    }

    let resumed = x86_64::instructions::interrupts::without_interrupts(|| {
        let ioapic = apic::save_ioapic();
        let pci = crate::drivers::pci::save();
        crate::time::suspend();
        unsafe { (&raw mut CR4).write(x86_64::registers::control::Cr4::read_raw()) };
        // Where the firmware jumps on wake: the trampoline, then resume_entry.
        let top = (&raw const STACK) as u64 + RESUME_STACK as u64;
        smp::prepare_wake(resume_entry, top & !0xF);
        let facs = phys_to_virt(info.facs);
        unsafe {
            ((facs + 12) as *mut u32).write_volatile(smp::trampoline() as u32);
            if ((facs + 4) as *const u32).read_volatile() >= 32 {
                ((facs + 24) as *mut u64).write_volatile(0); // no 64-bit waking vector
            }
        }
        if unsafe { aurora_s3_save(&raw mut CONTEXT) } == 0 {
            // Clear WAK_STS, flush caches, then SLP_TYP and SLP_EN.
            outw(info.pm1a_evt, 1 << 15);
            unsafe { core::arch::asm!("wbinvd") };
            for (port, typ) in [(info.pm1a_cnt, typa), (info.pm1b_cnt, typb)] {
                if port != 0 {
                    let v = inw(port) & !(0x7 << 10);
                    outw(port, v | typ << 10);
                    outw(port, v | typ << 10 | 1 << 13);
                }
            }
            // Still here: the platform did not sleep.
            for _ in 0..100_000 {
                core::hint::spin_loop();
            }
            crate::time::resume();
            apic::resume(&ioapic);
            return (false, pci);
        }
        // Awake. Interrupts stay off until the firmware's PIC setup (whose
        // vectors overlap CPU exceptions) is masked again.
        crate::time::resume();
        apic::resume(&ioapic);
        (true, pci)
    });
    let (slept, pci) = resumed;
    // Now in an ordinary task again: devices, AML, the other CPUs.
    crate::drivers::pci::restore(&pci);
    crate::drivers::resume();
    crate::acpi::finish_sleep(3);
    // Clear "parking" first, or the restarted CPUs would park again at once.
    crate::sched::unpark();
    smp::start_aps(&aps, smp::trampoline());
    SLEEPING.store(false, Ordering::Release);
    log!("power", "{}", if slept { "awake after S3" } else { "S3 was refused by the platform" });
    if slept {
        Ok(())
    } else {
        Err("the platform did not go to sleep")
    }
}
