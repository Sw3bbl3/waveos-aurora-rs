//! Per-CPU identity and state.
//!
//! `cpu_id()` reads TPIDR_EL1, which holds the CPU's index: one register
//! read, valid anywhere (exception handlers included). The bootloader
//! zeroes it, so early code is CPU 0.

use super::regs::{self, read};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const MAX_CPUS: usize = 16;

/// Kept for the interface's sake: Arm needs no syscall scratch area (the
/// CPU switches to the kernel stack, SP_EL1, by itself).
#[repr(C)]
pub struct SyscallArea {
    pub kernel_rsp: AtomicU64,
    pub user_rsp: AtomicU64,
}

pub struct PerCpu {
    pub syscall: SyscallArea,
    /// The CPU's hardware id: its MPIDR_EL1 affinity (named for the x86
    /// field it stands in for, the local APIC id).
    pub lapic_id: AtomicU32,
    pub online: AtomicBool,
    /// Currently running its idle task (a woken task should send an IPI).
    pub idle: AtomicBool,
    /// Page table root loaded on this CPU (TTBR0; for TLB shootdowns).
    pub cr3: AtomicU64,
    /// TLB shootdown requests this CPU still has to acknowledge.
    pub tlb_pending: AtomicBool,
    /// The task running here, and its process (read without the scheduler lock).
    pub task: AtomicU64,
    pub pid: AtomicU32,
    /// The running task is a kernel task with its own saved SIMD state.
    pub simd: AtomicBool,
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
            simd: AtomicBool::new(false),
        }
    }
}

pub static CPUS: [PerCpu; MAX_CPUS] = [const { PerCpu::new() }; MAX_CPUS];

/// Number of CPUs brought up so far.
static COUNT: AtomicU32 = AtomicU32::new(1);

/// Index of the CPU executing this code (0 = the boot CPU).
#[inline]
pub fn cpu_id() -> usize {
    read!("tpidr_el1") as usize
}

/// Records this CPU's index in TPIDR_EL1 (first thing on every CPU).
pub fn set_this(cpu: usize) {
    // SAFETY: TPIDR_EL1 is the kernel's own per-CPU register.
    unsafe { regs::write!("tpidr_el1", cpu as u64) };
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

/// Marks this CPU online with its hardware id.
pub fn init(cpu: usize) {
    set_this(cpu);
    CPUS[cpu].lapic_id.store(super::cpu::mpidr() as u32, Ordering::Relaxed);
    CPUS[cpu].online.store(true, Ordering::Release);
}

/// Iterates the indices of CPUs that are up.
pub fn online() -> impl Iterator<Item = usize> {
    (0..MAX_CPUS).filter(|&i| CPUS[i].online.load(Ordering::Acquire))
}
