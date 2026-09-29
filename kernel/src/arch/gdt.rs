//! Global Descriptor Table and per-CPU Task State Segments.
//!
//! One GDT is shared by every CPU. Its fixed part is laid out for
//! `syscall`/`sysret` (0x08 kernel code, 0x10 kernel data, 0x18 user data,
//! 0x20 user code); then come one TSS descriptor per CPU, 16 bytes each,
//! from 0x28. A CPU loads its own TSS, so `str` (the TSS selector it loaded)
//! tells any code — even an exception handler — which CPU it runs on.

use super::percpu::MAX_CPUS;
use spin::Once;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

pub const DOUBLE_FAULT_IST: u16 = 0;
pub const PAGE_FAULT_IST: u16 = 1;
pub const NMI_IST: u16 = 2;
const IST_STACK_SIZE: usize = 32 * 1024;

pub const KERNEL_CS: u16 = 0x08;
pub const KERNEL_SS: u16 = 0x10;
pub const USER_SS: u16 = 0x18 | 3;
pub const USER_CS: u16 = 0x20 | 3;
/// Selector of CPU 0's TSS; CPU n's is `FIRST_TSS + 16 * n`.
pub const FIRST_TSS: u16 = 0x28;

const GDT_ENTRIES: usize = 5 + 2 * MAX_CPUS;

#[repr(align(16))]
#[allow(dead_code)] // only its address is used
struct Stack([u8; IST_STACK_SIZE]);

static mut BSP_DOUBLE_FAULT: Stack = Stack([0; IST_STACK_SIZE]);
static mut BSP_PAGE_FAULT: Stack = Stack([0; IST_STACK_SIZE]);
static mut BSP_NMI: Stack = Stack([0; IST_STACK_SIZE]);

/// Mutable because `rsp0` (the stack used when an interrupt arrives in ring
/// 3) changes on every switch to a user task. Each CPU only touches its own.
static mut TSS: [TaskStateSegment; MAX_CPUS] = [const { TaskStateSegment::new() }; MAX_CPUS];

struct Gdt {
    table: GlobalDescriptorTable<GDT_ENTRIES>,
    code: SegmentSelector,
    data: SegmentSelector,
    tss: [SegmentSelector; MAX_CPUS],
}

static GDT: Once<Gdt> = Once::new();

fn tss(cpu: usize) -> &'static TaskStateSegment {
    unsafe { &(*(&raw const TSS))[cpu] }
}

fn build() -> Gdt {
    let mut table = GlobalDescriptorTable::<GDT_ENTRIES>::empty();
    let code = table.append(Descriptor::kernel_code_segment());
    let data = table.append(Descriptor::kernel_data_segment());
    table.append(Descriptor::user_data_segment());
    table.append(Descriptor::user_code_segment());
    let tss = core::array::from_fn(|cpu| table.append(Descriptor::tss_segment(tss(cpu))));
    Gdt { table, code, data, tss }
}

fn ist_top(stack: *const u8) -> VirtAddr {
    VirtAddr::from_ptr(stack) + IST_STACK_SIZE as u64
}

/// Sets up CPU `cpu`'s TSS (interrupt stacks) and loads the GDT and TSS on
/// the calling CPU.
pub fn init(cpu: usize) {
    let tops = if cpu == 0 {
        [
            ist_top(&raw const BSP_DOUBLE_FAULT as *const u8),
            ist_top(&raw const BSP_PAGE_FAULT as *const u8),
            ist_top(&raw const BSP_NMI as *const u8),
        ]
    } else {
        let alloc = || {
            let s = alloc::vec![0u8; IST_STACK_SIZE].leak();
            ist_top(s.as_ptr())
        };
        [alloc(), alloc(), alloc()]
    };
    unsafe {
        let t = &mut (*(&raw mut TSS))[cpu];
        t.interrupt_stack_table[DOUBLE_FAULT_IST as usize] = tops[0];
        t.interrupt_stack_table[PAGE_FAULT_IST as usize] = tops[1];
        t.interrupt_stack_table[NMI_IST as usize] = tops[2];
    }
    let gdt = GDT.call_once(build);
    gdt.table.load();
    unsafe {
        CS::set_reg(gdt.code);
        DS::set_reg(gdt.data);
        ES::set_reg(gdt.data);
        SS::set_reg(gdt.data);
        load_tss(gdt.tss[cpu]);
    }
}

/// Sets the kernel stack used for interrupts taken in ring 3 on this CPU.
pub fn set_kernel_stack(top: u64) {
    let cpu = super::percpu::cpu_id();
    unsafe { (*(&raw mut TSS))[cpu].privilege_stack_table[0] = VirtAddr::new(top) };
}
