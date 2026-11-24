//! Starting and stopping the other CPUs (PSCI `CPU_ON`, milestone 2).

/// Starts the other CPUs. Until PSCI start-up is in, only the boot CPU runs.
pub fn start_aps(cpus: &[u8], _trampoline: u64) {
    if cpus.len() > 1 {
        log!("smp", "{} CPUs described; starting the others is not done yet on Arm", cpus.len());
    }
}

/// Stops the other CPUs (panic): none run yet.
pub fn halt_others() {}

/// No real-mode trampoline page on Arm.
#[allow(dead_code)]
pub fn trampoline() -> u64 {
    0
}
