//! Starting the other CPUs (application processors).
//!
//! An AP wakes in 16-bit real mode at a page-aligned address below 1 MiB (the
//! "trampoline" page the bootloader reserved), named by the STARTUP IPI. The
//! trampoline climbs to 32-bit protected mode and then long mode, loads a
//! small page table (identity-mapped low memory plus the shared kernel half)
//! and jumps to `ap_entry` in the kernel on a fresh stack. From there the AP
//! sets up its own GDT/TSS, IDT, `syscall`, FPU and local APIC, and joins the
//! scheduler with its own idle task.

use super::percpu::{self, MAX_CPUS};
use crate::mm::{frame, phys_to_virt, vmm, PAGE_SIZE};
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const AP_STACK: usize = 64 * 1024;

global_asm!(
    r#"
    .section .text.aurora_trampoline, "ax"
    .global aurora_trampoline_start
    .global aurora_trampoline_end
    .global aurora_tr_gdtr
    .global aurora_tr_cr3
    .global aurora_tr_stack
    .global aurora_tr_entry
    .global aurora_tr_arg
    .code16
aurora_trampoline_start:
    cli
    cld
    movw %cs, %ax
    movw %ax, %ds
    movw %ax, %ss
    movw $0x1000, %sp                   # a small stack at the top of this page
    movzwl %ax, %ebx
    shll $4, %ebx                       # ebx = physical base of this page
    lgdtl (aurora_tr_gdtr - aurora_trampoline_start)
    movl %cr0, %eax
    orl $1, %eax
    movl %eax, %cr0
    leal (1f - aurora_trampoline_start)(%ebx), %eax
    pushl $0x08
    pushl %eax
    lretl
    .code32
1:
    movw $0x10, %ax
    movw %ax, %ds
    movw %ax, %es
    movw %ax, %ss
    leal 0x1000(%ebx), %esp
    movl %cr4, %eax
    orl $0x20, %eax                     # PAE
    movl %eax, %cr4
    movl (aurora_tr_cr3 - aurora_trampoline_start)(%ebx), %eax
    movl %eax, %cr3
    movl $0xC0000080, %ecx              # EFER: long mode + no-execute
    rdmsr
    orl $0x900, %eax
    wrmsr
    movl %cr0, %eax
    orl $0x80000001, %eax               # paging + protection
    movl %eax, %cr0
    leal (2f - aurora_trampoline_start)(%ebx), %eax
    pushl $0x18
    pushl %eax
    lretl
    .code64
2:
    movl %ebx, %ebx                     # zero-extend the base into rbx
    movq (aurora_tr_stack - aurora_trampoline_start)(%rbx), %rsp
    movq (aurora_tr_arg - aurora_trampoline_start)(%rbx), %rdi
    movq (aurora_tr_entry - aurora_trampoline_start)(%rbx), %rax
    jmpq *%rax
    .balign 8
aurora_tr_gdt:
    .quad 0
    .quad 0x00CF9A000000FFFF            # 0x08: 32-bit code
    .quad 0x00CF92000000FFFF            # 0x10: data
    .quad 0x00AF9A000000FFFF            # 0x18: 64-bit code
aurora_tr_gdtr:
    .word 31
    .long 0                             # base, patched
    .balign 8
aurora_tr_cr3:
    .quad 0
aurora_tr_stack:
    .quad 0
aurora_tr_entry:
    .quad 0
aurora_tr_arg:
    .quad 0
aurora_trampoline_end:
    .text
"#,
    options(att_syntax)
);

extern "C" {
    static aurora_trampoline_start: u8;
    static aurora_trampoline_end: u8;
    static aurora_tr_gdtr: u8;
    static aurora_tr_cr3: u8;
    static aurora_tr_stack: u8;
    static aurora_tr_entry: u8;
    static aurora_tr_arg: u8;
}

/// Set by an AP once it is fully up.
static AP_READY: AtomicBool = AtomicBool::new(false);
/// The page table APs start with (identity map of low memory + kernel half).
static AP_PML4: AtomicU64 = AtomicU64::new(0);
/// Physical address of the trampoline page (below 1 MiB), 0 if none.
static TRAMPOLINE: AtomicU64 = AtomicU64::new(0);

/// The trampoline page, also the waking vector after sleep.
pub fn trampoline() -> u64 {
    TRAMPOLINE.load(Ordering::Relaxed)
}

fn offset(sym: &u8) -> usize {
    sym as *const u8 as usize - unsafe { &aurora_trampoline_start as *const u8 as usize }
}

fn delay_us(us: u64) {
    let end = crate::time::now_ns() + us * 1000;
    while crate::time::now_ns() < end {
        core::hint::spin_loop();
    }
}

/// A page table for starting APs: the kernel half of the kernel's PML4 plus
/// low memory identity-mapped with one 2 MiB page. Must lie below 4 GiB,
/// because the trampoline loads CR3 in 32-bit mode.
fn boot_page_table() -> Option<u64> {
    const PRESENT: u64 = 1;
    const WRITABLE: u64 = 2;
    const HUGE: u64 = 1 << 7;
    let zeroed = || {
        let f = frame::alloc()?;
        unsafe { core::ptr::write_bytes(phys_to_virt(f) as *mut u8, 0, PAGE_SIZE as usize) };
        (f < 1 << 32).then_some(f)
    };
    let (pml4, pdpt, pd) = (zeroed()?, zeroed()?, zeroed()?);
    let table = |p: u64| unsafe { &mut *(phys_to_virt(p) as *mut [u64; 512]) };
    let kernel = table(vmm::kernel_pml4());
    let new = table(pml4);
    new[256..].copy_from_slice(&kernel[256..]);
    new[0] = pdpt | PRESENT | WRITABLE;
    table(pdpt)[0] = pd | PRESENT | WRITABLE;
    table(pd)[0] = PRESENT | WRITABLE | HUGE;
    Some(pml4)
}

/// Copies the trampoline into its page and points it at `entry`.
fn install(trampoline: u64, entry: u64) -> Option<*mut u8> {
    if trampoline == 0 || trampoline >= 0x10_0000 {
        return None;
    }
    let pml4 = match AP_PML4.load(Ordering::Relaxed) {
        0 => {
            let p = boot_page_table()?;
            AP_PML4.store(p, Ordering::Relaxed);
            p
        }
        p => p,
    };
    TRAMPOLINE.store(trampoline, Ordering::Relaxed);
    // Copy the trampoline and patch its absolute fields.
    let len = unsafe { &aurora_trampoline_end as *const u8 as usize - &aurora_trampoline_start as *const u8 as usize };
    let page = phys_to_virt(trampoline) as *mut u8;
    unsafe {
        core::ptr::copy_nonoverlapping(&aurora_trampoline_start as *const u8, page, len);
        let gdt_base = trampoline as u32 + offset(&aurora_tr_gdtr) as u32 - 32;
        (page.add(offset(&aurora_tr_gdtr) + 2) as *mut u32).write_unaligned(gdt_base);
        (page.add(offset(&aurora_tr_cr3)) as *mut u64).write(pml4);
        (page.add(offset(&aurora_tr_entry)) as *mut u64).write(entry);
    }
    Some(page)
}

/// Makes the trampoline the way back from sleep: the boot CPU arrives at
/// `entry(0)` in long mode on `stack`.
pub fn prepare_wake(entry: extern "sysv64" fn(u64) -> !, stack: u64) -> bool {
    let Some(page) = install(TRAMPOLINE.load(Ordering::Relaxed), entry as *const () as u64) else { return false };
    unsafe {
        (page.add(offset(&aurora_tr_stack)) as *mut u64).write_volatile(stack);
        (page.add(offset(&aurora_tr_arg)) as *mut u64).write_volatile(0);
    }
    true
}

/// Starts every other CPU listed in the ACPI MADT. Returns how many are up.
pub fn start_aps(apic_ids: &[u8], trampoline: u64) -> usize {
    let bsp = super::apic::lapic_id();
    let others: alloc::vec::Vec<u8> = apic_ids.iter().copied().filter(|&id| id as u32 != bsp).collect();
    TRAMPOLINE.store(trampoline, Ordering::Relaxed);
    if others.is_empty() {
        return 1;
    }
    let Some(page) = install(trampoline, ap_entry as *const () as u64) else {
        log!("smp", "no trampoline page below 1 MiB (or no memory below 4 GiB); running on one CPU");
        return 1;
    };

    let mut up = 1;
    for (i, &apic) in others.iter().enumerate() {
        let cpu = i + 1;
        if cpu >= MAX_CPUS {
            log!("smp", "more than {} CPUs; the rest stay parked", MAX_CPUS);
            break;
        }
        let stack = alloc::vec![0u8; AP_STACK].leak();
        let top = (stack.as_ptr() as u64 + AP_STACK as u64) & !0xF;
        unsafe {
            (page.add(offset(&aurora_tr_stack)) as *mut u64).write_volatile(top);
            (page.add(offset(&aurora_tr_arg)) as *mut u64).write_volatile(cpu as u64);
        }
        AP_READY.store(false, Ordering::Release);
        super::apic::start_ap(apic as u32, trampoline, delay_us);
        let deadline = crate::time::now_ns() + 200_000_000;
        while !AP_READY.load(Ordering::Acquire) && crate::time::now_ns() < deadline {
            core::hint::spin_loop();
        }
        if AP_READY.load(Ordering::Acquire) {
            up += 1;
            percpu::set_count(up);
        } else {
            log!("smp", "CPU with APIC id {} did not start", apic);
        }
    }
    log!("smp", "{} CPU(s) online", up);
    up
}

/// First Rust code on an application processor (on its own stack).
extern "sysv64" fn ap_entry(cpu: u64) -> ! {
    let cpu = cpu as usize;
    unsafe { vmm::load(vmm::kernel_pml4()) };
    super::gdt::init(cpu);
    super::idt::init();
    super::syscall::init();
    super::fpu::init();
    super::apic::init_ap();
    percpu::init(cpu);
    crate::sched::init_ap(cpu);
    log!("smp", "CPU {} up (APIC id {})", cpu, super::apic::lapic_id());
    AP_READY.store(true, Ordering::Release);
    crate::sched::idle();
}

/// Stops every other CPU (a kernel panic).
pub fn halt_others() {
    if percpu::count() > 1 {
        super::apic::broadcast_ipi(super::idt::HALT_VECTOR);
    }
}
