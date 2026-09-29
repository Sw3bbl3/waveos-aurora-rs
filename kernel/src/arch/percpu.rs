//! Per-CPU identity and state.
//!
//! `cpu_id()` reads the task register: every CPU loaded its own TSS selector
//! (see `gdt`), so this works anywhere — in interrupt handlers, on any stack —
//! without relying on GS. The only GS use is the `syscall` entry stub, which
//! swaps GS for a few instructions to find this CPU's kernel stack.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use x86_64::registers::model_specific::KernelGsBase;
use x86_64::VirtAddr;

pub const MAX_CPUS: usize = 16;

/// What the `syscall` entry stub reads through GS (field offsets are ABI).
#[repr(C)]
pub struct SyscallArea {
    /// Top of the current task's kernel stack (offset 0).
    pub kernel_rsp: AtomicU64,
    /// Scratch slot for the user stack pointer during entry (offset 8).
    pub user_rsp: AtomicU64,
}

pub struct PerCpu {
    pub syscall: SyscallArea,
    pub lapic_id: AtomicU32,
    pub online: AtomicBool,
    /// Currently running its idle task (a woken task should send an IPI).
    pub idle: AtomicBool,
    /// Page table root loaded on this CPU (for TLB shootdowns).
    pub cr3: AtomicU64,
    /// TLB shootdown requests this CPU still has to acknowledge.
    pub tlb_pending: AtomicBool,
    /// The task running here, and its process (read without the scheduler lock).
    pub task: AtomicU64,
    pub pid: AtomicU32,
}

impl PerCpu {
    const fn new() -> Self {
        Self {
            syscall: SyscallArea { kernel_rsp: AtomicU64::new(0), user_rsp: AtomicU64::new(0) },
            lapic_id: AtomicU32::new(0),
            online: AtomicBool::new(false),
            idle: AtomicBool::new(false),
            cr3: AtomicU64::new(0),
            tlb_pending: AtomicBool::new(false),
            task: AtomicU64::new(0),
            pid: AtomicU32::new(0),
        }
    }
}

pub static CPUS: [PerCpu; MAX_CPUS] = [const { PerCpu::new() }; MAX_CPUS];

/// Number of CPUs brought up so far.
static COUNT: AtomicU32 = AtomicU32::new(1);

/// Index of the CPU executing this code (0 = the bootstrap processor).
#[inline]
pub fn cpu_id() -> usize {
    let sel: u16;
    unsafe { core::arch::asm!("str {0:x}", out(reg) sel, options(nomem, nostack, preserves_flags)) };
    if sel < super::gdt::FIRST_TSS {
        0 // before the TSS is loaded (early boot)
    } else {
        ((sel - super::gdt::FIRST_TSS) / 16) as usize
    }
}

pub fn this() -> &'static PerCpu {
    &CPUS[cpu_id()]
}

pub fn count() -> usize {
    COUNT.load(Ordering::Acquire) as usize
}

pub fn set_count(n: usize) {
    COUNT.store(n as u32, Ordering::Release);
}

/// Points KERNEL_GS_BASE at this CPU's syscall area (swapped in by `swapgs`).
pub fn init(cpu: usize) {
    let area = &CPUS[cpu].syscall as *const SyscallArea as u64;
    KernelGsBase::write(VirtAddr::new(area));
    CPUS[cpu].lapic_id.store(super::apic::lapic_id(), Ordering::Relaxed);
    CPUS[cpu].online.store(true, Ordering::Release);
}

/// Iterates the indices of CPUs that are up.
pub fn online() -> impl Iterator<Item = usize> {
    (0..MAX_CPUS).filter(|&i| CPUS[i].online.load(Ordering::Acquire))
}
