//! Interrupt Descriptor Table: CPU exceptions and hardware IRQ vectors.

use super::{apic, gdt};
use spin::Lazy;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

pub const TIMER_VECTOR: u8 = 32;
pub const KEYBOARD_VECTOR: u8 = 33;
pub const MOUSE_VECTOR: u8 = 44;
/// The legacy 8259 PICs are remapped here and masked; stray IRQs land harmlessly.
pub const PIC_BASE_VECTOR: u8 = 0xE0;
pub const SPURIOUS_VECTOR: u8 = 0xFF;

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.divide_error.set_handler_fn(divide_error);
    idt.debug.set_handler_fn(debug);
    idt.non_maskable_interrupt.set_handler_fn(nmi);
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.overflow.set_handler_fn(overflow);
    idt.bound_range_exceeded.set_handler_fn(bound_range);
    idt.invalid_opcode.set_handler_fn(invalid_opcode);
    idt.device_not_available.set_handler_fn(device_not_available);
    idt.invalid_tss.set_handler_fn(invalid_tss);
    idt.segment_not_present.set_handler_fn(segment_not_present);
    idt.stack_segment_fault.set_handler_fn(stack_segment_fault);
    idt.general_protection_fault.set_handler_fn(general_protection);
    idt.x87_floating_point.set_handler_fn(x87_fp);
    idt.alignment_check.set_handler_fn(alignment_check);
    idt.simd_floating_point.set_handler_fn(simd_fp);
    unsafe {
        idt.double_fault.set_handler_fn(double_fault).set_stack_index(gdt::DOUBLE_FAULT_IST);
        idt.page_fault.set_handler_fn(page_fault).set_stack_index(gdt::PAGE_FAULT_IST);
    }
    idt.machine_check.set_handler_fn(machine_check);

    idt[TIMER_VECTOR].set_handler_fn(timer);
    idt[KEYBOARD_VECTOR].set_handler_fn(keyboard);
    idt[MOUSE_VECTOR].set_handler_fn(mouse);
    for v in PIC_BASE_VECTOR..PIC_BASE_VECTOR + 16 {
        idt[v].set_handler_fn(ignored_irq);
    }
    idt[SPURIOUS_VECTOR].set_handler_fn(ignored_irq);
    idt
});

pub fn init() {
    IDT.load();
}

fn fatal(name: &str, frame: &InterruptStackFrame, detail: core::fmt::Arguments) -> ! {
    if from_user(frame) {
        crate::proc::crash_current(format_args!(
            "{} at {:#x}{}{}",
            name,
            frame.instruction_pointer.as_u64(),
            if detail.as_str() == Some("") { "" } else { " — " },
            detail
        ));
    }
    panic!(
        "CPU exception: {}\n{}\nRIP={:#x} CS={:#x} RFLAGS={:#x} RSP={:#x}",
        name,
        detail,
        frame.instruction_pointer.as_u64(),
        frame.code_segment.0,
        frame.cpu_flags.bits(),
        frame.stack_pointer.as_u64()
    );
}

macro_rules! exception {
    ($name:ident, $label:expr) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            fatal($label, &frame, format_args!(""));
        }
    };
    ($name:ident, $label:expr, error) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame, code: u64) {
            fatal($label, &frame, format_args!("error code {:#x}", code));
        }
    };
}

exception!(divide_error, "Divide Error (#DE)");
exception!(overflow, "Overflow (#OF)");
exception!(bound_range, "Bound Range Exceeded (#BR)");
exception!(invalid_opcode, "Invalid Opcode (#UD)");
exception!(device_not_available, "Device Not Available (#NM)");
exception!(x87_fp, "x87 Floating Point (#MF)");
exception!(simd_fp, "SIMD Floating Point (#XM)");
exception!(invalid_tss, "Invalid TSS (#TS)", error);
exception!(segment_not_present, "Segment Not Present (#NP)", error);
exception!(stack_segment_fault, "Stack Segment Fault (#SS)", error);
exception!(general_protection, "General Protection Fault (#GP)", error);
exception!(alignment_check, "Alignment Check (#AC)", error);

extern "x86-interrupt" fn debug(_frame: InterruptStackFrame) {}

extern "x86-interrupt" fn nmi(frame: InterruptStackFrame) {
    fatal("Non-Maskable Interrupt", &frame, format_args!(""));
}

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    log!("int", "breakpoint at {:#x}", frame.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, code: u64) -> ! {
    fatal("Double Fault (#DF)", &frame, format_args!("error code {:#x}", code));
}

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    fatal("Machine Check (#MC)", &frame, format_args!(""));
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    let addr = Cr2::read_raw();
    fatal("Page Fault (#PF)", &frame, format_args!("address {:#x}, {:?}", addr, code));
}

extern "x86-interrupt" fn timer(frame: InterruptStackFrame) {
    crate::time::tick();
    apic::eoi();
    crate::sched::on_timer_tick();
    if from_user(&frame) {
        crate::proc::check_killed();
    }
}

fn from_user(frame: &InterruptStackFrame) -> bool {
    frame.code_segment.0 & 3 == 3
}

extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    crate::drivers::ps2::keyboard_irq();
    apic::eoi();
}

extern "x86-interrupt" fn mouse(_frame: InterruptStackFrame) {
    crate::drivers::ps2::mouse_irq();
    apic::eoi();
}

extern "x86-interrupt" fn ignored_irq(_frame: InterruptStackFrame) {}
