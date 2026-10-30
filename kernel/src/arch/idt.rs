//! Interrupt Descriptor Table: CPU exceptions and hardware IRQ vectors.

use super::trap::TrapFrame;
use super::{apic, gdt};
use spin::Lazy;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::PrivilegeLevel;

pub const TIMER_VECTOR: u8 = 32;
pub const KEYBOARD_VECTOR: u8 = 33;
pub const MOUSE_VECTOR: u8 = 44;
/// Inter-processor interrupts.
pub const RESCHEDULE_VECTOR: u8 = 0xF0;
pub const TLB_VECTOR: u8 = 0xF1;
pub const HALT_VECTOR: u8 = 0xF2;
/// The legacy 8259 PICs are remapped here and masked; stray IRQs land harmlessly.
pub const PIC_BASE_VECTOR: u8 = 0xE0;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    // The exceptions a program can cause, and the interrupts after which a
    // signal may be delivered, go through `trap` with every register saved
    // (`arch::trap`). `int3` and `into` may be used by programs.
    unsafe {
        idt.divide_error.set_handler_addr(addr(0));
        idt.breakpoint.set_handler_addr(addr(3)).set_privilege_level(PrivilegeLevel::Ring3);
        idt.overflow.set_handler_addr(addr(4)).set_privilege_level(PrivilegeLevel::Ring3);
        idt.bound_range_exceeded.set_handler_addr(addr(5));
        idt.invalid_opcode.set_handler_addr(addr(6));
        idt.general_protection_fault.set_handler_addr(addr(13));
        idt.x87_floating_point.set_handler_addr(addr(16));
        idt.alignment_check.set_handler_addr(addr(17));
        idt.simd_floating_point.set_handler_addr(addr(19));
        // On the faulting task's own stack, not a stack of the CPU's:
        // resolving a write to a page shared since `fork` may wait (and the
        // CPU run other tasks, which may fault too). A kernel stack
        // overflowing still ends in a double fault, on its own stack.
        idt.page_fault.set_handler_addr(addr(14));
        idt[TIMER_VECTOR].set_handler_addr(addr(TIMER_VECTOR));
        idt[RESCHEDULE_VECTOR].set_handler_addr(addr(RESCHEDULE_VECTOR));
    }
    idt.debug.set_handler_fn(debug);
    unsafe {
        idt.non_maskable_interrupt.set_handler_fn(nmi).set_stack_index(gdt::NMI_IST);
    }
    idt.device_not_available.set_handler_fn(device_not_available);
    idt.invalid_tss.set_handler_fn(invalid_tss);
    idt.segment_not_present.set_handler_fn(segment_not_present);
    idt.stack_segment_fault.set_handler_fn(stack_segment_fault);
    unsafe {
        idt.double_fault.set_handler_fn(double_fault).set_stack_index(gdt::DOUBLE_FAULT_IST);
    }
    idt.machine_check.set_handler_fn(machine_check);

    idt[KEYBOARD_VECTOR].set_handler_fn(keyboard);
    idt[MOUSE_VECTOR].set_handler_fn(mouse);
    for v in PIC_BASE_VECTOR..PIC_BASE_VECTOR + 16 {
        idt[v].set_handler_fn(ignored_irq);
    }
    idt[SPURIOUS_VECTOR].set_handler_fn(ignored_irq);
    idt[TLB_VECTOR].set_handler_fn(tlb_ipi);
    idt[HALT_VECTOR].set_handler_fn(halt_ipi);
    super::irq::install(&mut idt);
    idt
});

fn addr(vector: u8) -> x86_64::VirtAddr {
    x86_64::VirtAddr::new(super::trap::stub(vector))
}

/// Loads the IDT on the calling CPU (every CPU shares it).
pub fn init() {
    IDT.load();
}

/// Return addresses up the interrupted kernel code's frame-pointer chain
/// (the kernel is built with frame pointers). The walk starts where it is
/// printed, goes up through the panic and the handler (on this stack), and
/// prints the frames of the interrupted stack, near its stack pointer.
struct Callers(u64);

impl core::fmt::Display for Callers {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let (mut rbp, here): (u64, u64);
        unsafe { core::arch::asm!("mov {}, rbp", "mov {}, rsp", out(reg) rbp, out(reg) here) };
        let near = |p: u64, base: u64| p >= base && p < base.saturating_add(64 * 1024) && p.is_multiple_of(8);
        let interrupted = self.0;
        for _ in 0..48 {
            if near(rbp, interrupted) {
                let ret = unsafe { *((rbp + 8) as *const u64) };
                write!(f, " {ret:#x}")?;
            } else if !near(rbp, here) {
                break;
            }
            rbp = unsafe { *(rbp as *const u64) };
        }
        Ok(())
    }
}

/// The 16 bytes of code at a user address, for crash reports (what an
/// invalid instruction was, say).
struct CodeBytes(u64);

impl core::fmt::Display for CodeBytes {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let mut code = [0u8; 16];
        if crate::syscall::user::get_bytes(self.0, &mut code).is_err() {
            return Ok(());
        }
        write!(f, " (code:")?;
        for b in code {
            write!(f, " {b:02x}")?;
        }
        write!(f, ")")
    }
}

/// Where an exception happened: enough to report it.
struct At {
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
}

impl From<&InterruptStackFrame> for At {
    fn from(f: &InterruptStackFrame) -> At {
        At {
            rip: f.instruction_pointer.as_u64(),
            cs: f.code_segment.0 as u64,
            rflags: f.cpu_flags.bits(),
            rsp: f.stack_pointer.as_u64(),
        }
    }
}

impl From<&TrapFrame> for At {
    fn from(f: &TrapFrame) -> At {
        At { rip: f.rip, cs: f.cs, rflags: f.rflags, rsp: f.rsp }
    }
}

fn fatal(name: &str, at: At, detail: core::fmt::Arguments) -> ! {
    if at.cs & 3 == 3 {
        crate::proc::crash_current(format_args!(
            "{} at {:#x}{}{}{}",
            name,
            at.rip,
            if detail.as_str() == Some("") { "" } else { " — " },
            detail,
            CodeBytes(at.rip)
        ));
    }
    panic!(
        "CPU exception: {}\n{}\nRIP={:#x} CS={:#x} RFLAGS={:#x} RSP={:#x}\ncalled from:{}",
        name,
        detail,
        at.rip,
        at.cs,
        at.rflags,
        at.rsp,
        Callers(at.rsp)
    );
}

macro_rules! exception {
    ($name:ident, $label:expr) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            fatal($label, (&frame).into(), format_args!(""));
        }
    };
    ($name:ident, $label:expr, error) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame, code: u64) {
            fatal($label, (&frame).into(), format_args!("error code {:#x}", code));
        }
    };
}

exception!(device_not_available, "Device Not Available (#NM)");
exception!(invalid_tss, "Invalid TSS (#TS)", error);
exception!(segment_not_present, "Segment Not Present (#NP)", error);
exception!(stack_segment_fault, "Stack Segment Fault (#SS)", error);

extern "x86-interrupt" fn debug(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    fatal("Non-Maskable Interrupt", (&frame).into(), format_args!(""));
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    fatal("Double Fault (#DF)", (&frame).into(), format_args!("error code {:#x}", code));
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    fatal("Machine Check (#MC)", (&frame).into(), format_args!(""));
}

/// Signal numbers and `si_code` values for faults (Linux's).
const SIGILL: u32 = 4;
const SIGTRAP: u32 = 5;
const SIGBUS: u32 = 7;
const SIGFPE: u32 = 8;
const SIGSEGV: u32 = 11;
const SEGV_MAPERR: i32 = 1;
const SEGV_ACCERR: i32 = 2;
const ILL_ILLOPN: i32 = 2;
const FPE_INTDIV: i32 = 1;
const FPE_FLTINV: i32 = 7;
const BUS_ADRALN: i32 = 1;
const SI_KERNEL: i32 = 0x80;

/// Every vector with a trap stub comes here (see `arch::trap`).
pub extern "sysv64" fn trap(f: &mut TrapFrame) {
    match f.vector as u8 {
        TIMER_VECTOR => {
            crate::telemetry::irq(crate::telemetry::Irq::Timer);
            apic::eoi();
            crate::sched::on_timer_tick();
        }
        RESCHEDULE_VECTOR => {
            apic::eoi();
            crate::sched::on_reschedule_ipi();
        }
        14 => page_fault(f),
        3 if !f.user_mode() => log!("int", "breakpoint at {:#x}", f.rip),
        v => {
            let (name, sig, code, addr) = match v {
                0 => ("Divide Error (#DE)", SIGFPE, FPE_INTDIV, f.rip),
                3 => ("Breakpoint (#BP)", SIGTRAP, SI_KERNEL, 0),
                4 => ("Overflow (#OF)", SIGSEGV, SI_KERNEL, 0),
                5 => ("Bound Range Exceeded (#BR)", SIGSEGV, SI_KERNEL, 0),
                6 => ("Invalid Opcode (#UD)", SIGILL, ILL_ILLOPN, f.rip),
                13 => ("General Protection Fault (#GP)", SIGSEGV, SI_KERNEL, 0),
                16 => ("x87 Floating Point (#MF)", SIGFPE, FPE_FLTINV, f.rip),
                17 => ("Alignment Check (#AC)", SIGBUS, BUS_ADRALN, f.rip),
                _ => ("SIMD Floating Point (#XM)", SIGFPE, FPE_FLTINV, f.rip),
            };
            if !user_fault(f, sig, code, addr) {
                if matches!(v, 13 | 17) {
                    fatal(name, (&*f).into(), format_args!("error code {:#x}", f.error));
                }
                fatal(name, (&*f).into(), format_args!(""));
            }
        }
    }
    if f.user_mode() {
        crate::proc::check_killed();
        crate::compat::linux::signal::on_trap_return(f);
    }
}

/// A fault in a Linux program with a handler for `sig` (that doesn't block
/// it): true once the frame enters the handler. Otherwise the caller
/// reports the crash.
fn user_fault(f: &mut TrapFrame, sig: u32, code: i32, addr: u64) -> bool {
    if !f.user_mode() {
        return false;
    }
    let Some(p) = crate::proc::current().filter(|p| p.linux.is_some()) else { return false };
    let info = crate::compat::linux::signal::Info { code, addr, ..Default::default() };
    crate::compat::linux::signal::force(&p, sig, info, f)
}

fn page_fault(f: &mut TrapFrame) {
    let addr = Cr2::read_raw();
    let code = PageFaultErrorCode::from_bits_truncate(f.error);
    // A first touch of lazy memory, or a write to a page shared since
    // `fork` (by the program, or by a system call copying to it): the page
    // is made (or becomes the writer's own) and the access goes again. Only
    // where interrupts were on (a program, or a system call holding no
    // spinning lock): this may wait for the process's memory lock and for
    // other CPUs.
    let write = code.contains(PageFaultErrorCode::CAUSED_BY_WRITE);
    let fixable = !code.contains(PageFaultErrorCode::PROTECTION_VIOLATION)
        || (write && !code.contains(PageFaultErrorCode::INSTRUCTION_FETCH));
    let interrupts_on = f.rflags & x86_64::registers::rflags::RFlags::INTERRUPT_FLAG.bits() != 0;
    if fixable
        && addr < aurora_abi::layout::USER_END
        && interrupts_on
        && (f.user_mode() || crate::syscall::user::fixup(f.rip, addr).is_some())
    {
        x86_64::instructions::interrupts::enable();
        let resolved = crate::proc::resolve_fault(addr, write);
        x86_64::instructions::interrupts::disable();
        if resolved {
            return;
        }
    }
    if !f.user_mode() {
        // A user copy hitting memory another thread just unmapped: resume
        // at the copy routine's fault exit, which reports EFAULT.
        if let Some(resume) = crate::syscall::user::fixup(f.rip, addr) {
            f.rip = resume;
            return;
        }
        let walk = crate::mm::vmm::entries_for(addr);
        fatal(
            "Page Fault (#PF)",
            (&*f).into(),
            format_args!("address {:#x}, {:?}, page tables {:x?}", addr, code, walk),
        );
    }
    // Mapped but not allowed (written while read-only, say), or not mapped.
    x86_64::instructions::interrupts::enable();
    let mapped = code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) || crate::proc::is_mapped(addr);
    x86_64::instructions::interrupts::disable();
    let si_code = if mapped { SEGV_ACCERR } else { SEGV_MAPERR };
    if !user_fault(f, SIGSEGV, si_code, addr) {
        fatal("Page Fault (#PF)", (&*f).into(), format_args!("address {:#x}, {:?}", addr, code));
    }
}

extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    crate::telemetry::irq(crate::telemetry::Irq::Keyboard);
    crate::drivers::ps2::keyboard_irq();
    apic::eoi();
}

extern "x86-interrupt" fn mouse(_frame: InterruptStackFrame) {
    crate::telemetry::irq(crate::telemetry::Irq::Mouse);
    crate::drivers::ps2::mouse_irq();
    apic::eoi();
}

extern "x86-interrupt" fn ignored_irq(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn tlb_ipi(_frame: InterruptStackFrame) {
    crate::mm::vmm::service_shootdown();
    apic::eoi();
}

extern "x86-interrupt" fn halt_ipi(_frame: InterruptStackFrame) {
    crate::arch::cpu::halt_forever();
}
