//! Global Descriptor Table and Task State Segment.
//!
//! Layout (fixed so `syscall`/`sysret` work once user space lands in M2):
//!   0x08 kernel code, 0x10 kernel data, 0x18 user data, 0x20 user code, 0x28 TSS

use spin::Lazy;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST: u16 = 0;
pub const PAGE_FAULT_IST: u16 = 1;
const IST_STACK_SIZE: usize = 32 * 1024;

#[repr(align(16))]
#[allow(dead_code)] // only its address is used
struct Stack([u8; IST_STACK_SIZE]);

static mut DOUBLE_FAULT_STACK: Stack = Stack([0; IST_STACK_SIZE]);
static mut PAGE_FAULT_STACK: Stack = Stack([0; IST_STACK_SIZE]);

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_SS: u16 = 0x10;
pub const USER_SS: u16 = 0x18 | 3;
pub const USER_CS: u16 = 0x20 | 3;

/// Mutable because `rsp0` (the stack used when an interrupt arrives in ring 3)
/// changes on every switch to a user task. Single CPU: no concurrent access.
static mut TSS: TaskStateSegment = TaskStateSegment::new();

fn tss() -> &'static TaskStateSegment {
    unsafe { &*(&raw const TSS) }
}

/// Sets the kernel stack used for interrupts and exceptions taken in ring 3.
pub fn set_kernel_stack(top: u64) {
    unsafe { (*(&raw mut TSS)).privilege_stack_table[0] = VirtAddr::new(top) };
}

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    gdt.append(Descriptor::user_data_segment());
    gdt.append(Descriptor::user_code_segment());
    let tss = gdt.append(Descriptor::tss_segment(tss()));
    (gdt, Selectors { code, data, tss })
});

pub fn init() {
    let top = |s: *const Stack| VirtAddr::from_ptr(s) + IST_STACK_SIZE as u64;
    unsafe {
        let t = &mut *(&raw mut TSS);
        t.interrupt_stack_table[DOUBLE_FAULT_IST as usize] = top(&raw const DOUBLE_FAULT_STACK);
        t.interrupt_stack_table[PAGE_FAULT_IST as usize] = top(&raw const PAGE_FAULT_STACK);
    }
    GDT.0.load();
    unsafe {
        CS::set_reg(GDT.1.code);
        DS::set_reg(GDT.1.data);
        ES::set_reg(GDT.1.data);
        SS::set_reg(GDT.1.data);
        load_tss(GDT.1.tss);
    }
}
