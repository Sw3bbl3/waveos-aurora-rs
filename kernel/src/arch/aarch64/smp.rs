//! Starting and stopping the other CPUs (PSCI `CPU_ON`).

use super::percpu::MAX_CPUS;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// The CPUs the MADT lists (MPIDRs), the boot CPU among them.
static MPIDRS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static LISTED: AtomicUsize = AtomicUsize::new(0);

/// Records the CPUs from the MADT: (MPIDR, GICR, GICC).
pub fn set_cpus(cpus: &[(u64, u64, u64)]) {
    for (i, c) in cpus.iter().take(MAX_CPUS).enumerate() {
        MPIDRS[i].store(c.0, Ordering::Relaxed);
    }
    LISTED.store(cpus.len().min(MAX_CPUS), Ordering::Relaxed);
}

/// Starts the other CPUs. (Not yet: only the boot CPU runs.)
pub fn start_aps(_cpus: &[u8], _trampoline: u64) {
    let n = LISTED.load(Ordering::Relaxed);
    if n > 1 {
        log!("smp", "{} CPUs described; starting the others is not done yet on Arm", n);
    }
}

/// Stops the other CPUs (panic).
pub fn halt_others() {
    if super::gic::ready() {
        super::gic::broadcast_sgi(super::gic::SGI_HALT);
    }
}

/// No real-mode trampoline page on Arm.
#[allow(dead_code)]
pub fn trampoline() -> u64 {
    0
}
