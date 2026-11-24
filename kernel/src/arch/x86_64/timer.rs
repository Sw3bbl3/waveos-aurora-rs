//! Each CPU's local APIC timer, programmed for one deadline at a time.
//!
//! The kernel's timer queue (`crate::timer`) asks for an interrupt at this
//! CPU's next deadline: the earliest timer, or the end of the running task's
//! time slice while others wait for the CPU. Three ways to get it:
//!
//! - **TSC-deadline** (CPUID.01H:ECX[24], and the TSC is the clock): the
//!   deadline is written as a TSC value to `IA32_TSC_DEADLINE` (SDM vol. 3A
//!   §11.5.4.1). No conversion error accumulates and no count can overflow.
//! - **One-shot**: the LAPIC counts the distance down (divide-by-16) once.
//!   Distances beyond its 32 bits fire early; the queue then re-arms.
//! - **Periodic** at 1 kHz, as before: when nothing better is possible (the
//!   clock is the tick itself) or `timer=periodic` is in `boot.conf`.
//!
//! Whether the timer keeps running in deep C-states is CPUID.06H:EAX[2]
//! (ARAT, "always running APIC timer"); without it the idle code stays in
//! states that keep it (see `power::idle`).

use super::apic;
use super::percpu::MAX_CPUS;
use core::arch::x86_64::{__cpuid, _rdtsc};
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use x86_64::registers::model_specific::Msr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Periodic,
    OneShot,
    TscDeadline,
}

const IA32_TSC_DEADLINE: u32 = 0x6E0;
const LVT_ONESHOT: u32 = 0;
const LVT_PERIODIC: u32 = 1 << 17;
const LVT_TSC_DEADLINE: u32 = 2 << 17;
const NONE: u64 = u64::MAX;

static MODE: AtomicU8 = AtomicU8::new(0);
/// The mode each CPU's timer was started in (it changes only when that CPU
/// restarts its timer, so a new `MODE` never meets an old LVT setup).
static LOCAL_MODE: [AtomicU8; MAX_CPUS] = [const { AtomicU8::new(0) }; MAX_CPUS];
static ARAT: AtomicBool = AtomicBool::new(false);
/// What each CPU's timer is armed for (ns of `time::now_ns`; `NONE`: nothing).
static ARMED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(NONE) }; MAX_CPUS];
/// Times each CPU's timer was written (the cost of tickless scheduling).
static WRITES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

fn decode(v: u8) -> Mode {
    match v {
        1 => Mode::OneShot,
        2 => Mode::TscDeadline,
        _ => Mode::Periodic,
    }
}

/// The mode chosen for the system.
pub fn mode() -> Mode {
    decode(MODE.load(Ordering::Relaxed))
}

/// The mode this CPU's timer runs in.
pub fn local_mode() -> Mode {
    decode(LOCAL_MODE.get(super::percpu::cpu_id()).map_or(0, |m| m.load(Ordering::Relaxed)))
}

/// Whether the LAPIC timer keeps counting in every C-state.
pub fn always_running() -> bool {
    ARAT.load(Ordering::Relaxed)
}

/// Chooses how timers are programmed (on the boot CPU, after calibration).
/// `choice` is the `timer=` boot option once the file system is up; at boot
/// it is `None` and the best available mode is used.
pub fn choose(choice: Option<&str>) {
    let deadline = __cpuid(1).ecx & (1 << 24) != 0 && crate::time::tsc_hz() != 0;
    let ticking_clock = crate::time::source() == crate::time::Source::Tick;
    ARAT.store(__cpuid(0).eax >= 6 && __cpuid(6).eax & (1 << 2) != 0, Ordering::Relaxed);
    let mode = match choice {
        _ if ticking_clock => Mode::Periodic,
        Some("periodic") => Mode::Periodic,
        Some("oneshot") => Mode::OneShot,
        _ if deadline => Mode::TscDeadline,
        _ => Mode::OneShot,
    };
    MODE.store(mode as u8, Ordering::Relaxed);
}

/// Starts this CPU's timer in the chosen mode, disarmed (periodic: ticking).
pub fn start_local() {
    let cpu = super::percpu::cpu_id();
    apic::lapic_write(apic::LAPIC_TIMER_DIVIDE, 0x3);
    let vector = super::idt::TIMER_VECTOR as u32;
    let mode = mode();
    if cpu < MAX_CPUS {
        LOCAL_MODE[cpu].store(mode as u8, Ordering::Relaxed);
    }
    if mode != Mode::TscDeadline {
        // Leaving TSC-deadline mode: disarm it (harmless otherwise when supported).
        if __cpuid(1).ecx & (1 << 24) != 0 {
            unsafe { Msr::new(IA32_TSC_DEADLINE).write(0) };
        }
    }
    match mode {
        Mode::Periodic => {
            apic::lapic_write(apic::LAPIC_LVT_TIMER, vector | LVT_PERIODIC);
            apic::lapic_write(apic::LAPIC_TIMER_INIT, apic::timer_count_per_ms());
        }
        Mode::OneShot => {
            apic::lapic_write(apic::LAPIC_LVT_TIMER, vector | LVT_ONESHOT);
            apic::lapic_write(apic::LAPIC_TIMER_INIT, 0);
        }
        Mode::TscDeadline => {
            apic::lapic_write(apic::LAPIC_LVT_TIMER, vector | LVT_TSC_DEADLINE);
            // The LVT write must be visible before the MSR write (SDM §11.5.4.1).
            unsafe {
                core::arch::asm!("mfence", options(nostack, preserves_flags));
                Msr::new(IA32_TSC_DEADLINE).write(0);
            }
        }
    }
    if cpu < MAX_CPUS {
        ARMED[cpu].store(NONE, Ordering::Relaxed);
    }
}

/// Arms this CPU's timer for `deadline` (ns of `time::now_ns`, `u64::MAX`:
/// none). Interrupts must be off. Does nothing in periodic mode.
pub fn program(deadline: u64) {
    let mode = local_mode();
    if mode == Mode::Periodic {
        return;
    }
    let cpu = super::percpu::cpu_id();
    if ARMED[cpu].load(Ordering::Relaxed) == deadline {
        return;
    }
    ARMED[cpu].store(deadline, Ordering::Relaxed);
    WRITES[cpu].fetch_add(1, Ordering::Relaxed);
    if deadline == NONE {
        match mode {
            Mode::TscDeadline => unsafe { Msr::new(IA32_TSC_DEADLINE).write(0) },
            _ => apic::lapic_write(apic::LAPIC_TIMER_INIT, 0),
        }
        return;
    }
    // Already due: as soon as possible (an interrupt, not a call, so the
    // handler runs in the usual context).
    let delta = deadline.saturating_sub(crate::time::now_ns()).max(1_000);
    match mode {
        Mode::TscDeadline => {
            let ticks = (delta as u128 * crate::time::tsc_hz() as u128 / 1_000_000_000) as u64;
            let target = unsafe { _rdtsc() }.saturating_add(ticks.max(1));
            unsafe {
                core::arch::asm!("mfence", options(nostack, preserves_flags));
                Msr::new(IA32_TSC_DEADLINE).write(target);
            }
        }
        _ => {
            let per_ms = apic::timer_count_per_ms() as u128;
            let count = (delta as u128 * per_ms / 1_000_000).clamp(1, u32::MAX as u128) as u32;
            apic::lapic_write(apic::LAPIC_TIMER_INIT, count);
        }
    }
}

/// The timer fired on this CPU: it is no longer armed.
pub fn fired() {
    let cpu = super::percpu::cpu_id();
    if cpu < MAX_CPUS && local_mode() != Mode::Periodic {
        ARMED[cpu].store(NONE, Ordering::Relaxed);
    }
}

/// How many times `cpu`'s timer has been written.
pub fn writes(cpu: usize) -> u64 {
    WRITES.get(cpu).map_or(0, |a| a.load(Ordering::Relaxed))
}

pub fn describe() -> &'static str {
    match mode() {
        Mode::Periodic => "periodic 1 kHz tick",
        Mode::OneShot => "one-shot LAPIC timer (tickless)",
        Mode::TscDeadline => "TSC-deadline LAPIC timer (tickless)",
    }
}
