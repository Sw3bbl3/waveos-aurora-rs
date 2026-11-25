//! Model-specific registers, for x86-only drivers (`power::cpufreq`).

/// Reads MSR `m`.
pub fn read(m: u32) -> u64 {
    // SAFETY: callers read only MSRs that CPUID says exist.
    unsafe { x86_64::registers::model_specific::Msr::new(m).read() }
}

/// Writes MSR `m`.
pub fn write(m: u32, v: u64) {
    // SAFETY: callers write only performance-control MSRs that CPUID says
    // exist, with values from their capability registers.
    unsafe { x86_64::registers::model_specific::Msr::new(m).write(v) }
}
