//! Each CPU's generic timer (the EL1 virtual timer, CNTV), programmed for
//! one deadline at a time.
//!
//! The kernel's timer queue (`crate::timer`) asks for an interrupt at this
//! CPU's next deadline. The virtual timer compares CNTVCT_EL0 — the clock
//! itself (`time`) — with CNTV_CVAL_EL0, so a deadline is written as a
//! count, as with x86's TSC-deadline mode: nothing to calibrate, no range
//! limit. Its interrupt is PPI 27 (the GTDT says), level-triggered: it
//! stays asserted until the timer is re-armed or stopped ([`fired`]).
//! The architecture keeps the timer running in every idle state.

use super::percpu::MAX_CPUS;
use super::regs::{self, read};
use core::sync::atomic::{AtomicU64, AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    /// A 1 kHz tick (`timer=periodic`, or the tick is the clock).
    Periodic,
    /// One deadline at a time (tickless).
    OneShot,
}

const NONE: u64 = u64::MAX;
/// CNTV_CTL_EL0: enabled; IMASK (interrupt masked).
const CTL_ENABLE: u64 = 1;
const CTL_IMASK: u64 = 2;

static MODE: AtomicU8 = AtomicU8::new(1);
static ARMED: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(NONE) }; MAX_CPUS];
static WRITES: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];

fn decode(v: u8) -> Mode {
    match v {
        0 => Mode::Periodic,
        _ => Mode::OneShot,
    }
}

/// The mode chosen for the system.
pub fn mode() -> Mode {
    decode(MODE.load(Ordering::Relaxed))
}

/// The mode this CPU's timer runs in (the same on every CPU).
pub fn local_mode() -> Mode {
    mode()
}

/// The generic timer keeps counting in every idle state.
pub fn always_running() -> bool {
    true
}

/// Chooses how timers are programmed (`timer=` from `boot.conf`, or `None`).
pub fn choose(choice: Option<&str>) {
    let ticking_clock = crate::time::source() == crate::time::Source::Tick;
    let mode = match choice {
        _ if ticking_clock => Mode::Periodic,
        Some("periodic") => Mode::Periodic,
        _ => Mode::OneShot,
    };
    MODE.store(mode as u8, Ordering::Relaxed);
}

fn ctl(v: u64) {
    // SAFETY: this CPU's own virtual timer control.
    unsafe { regs::write!("cntv_ctl_el0", v) };
    regs::isb();
}

fn arm_ticks(ticks: u64) {
    let target = super::cpu::cycles().saturating_add(ticks.max(1));
    // SAFETY: this CPU's own virtual timer compare value.
    unsafe { regs::write!("cntv_cval_el0", target) };
    ctl(CTL_ENABLE);
}

fn ticks_per_ms() -> u64 {
    read!("cntfrq_el0") / 1000
}

/// CNTKCTL_EL1.EL0VCTEN: EL0 may read CNTVCT_EL0 and CNTFRQ_EL0 (the
/// Linux vDSO's clock, as on Linux).
const CNTKCTL_EL0VCTEN: u64 = 1 << 1;

/// Starts this CPU's timer in the chosen mode, disarmed (periodic: ticking).
pub fn start_local() {
    let cpu = super::percpu::cpu_id();
    // SAFETY: only opens the virtual count to user mode.
    unsafe { regs::write!("cntkctl_el1", read!("cntkctl_el1") | CNTKCTL_EL0VCTEN) };
    match mode() {
        Mode::Periodic => arm_ticks(ticks_per_ms()),
        Mode::OneShot => ctl(CTL_IMASK),
    }
    if cpu < MAX_CPUS {
        ARMED[cpu].store(NONE, Ordering::Relaxed);
    }
}

/// Arms this CPU's timer for `deadline` (ns of `time::now_ns`, `u64::MAX`:
/// none). Interrupts must be off. Does nothing in periodic mode.
pub fn program(deadline: u64) {
    if mode() == Mode::Periodic {
        return;
    }
    let cpu = super::percpu::cpu_id();
    if ARMED[cpu].load(Ordering::Relaxed) == deadline {
        return;
    }
    ARMED[cpu].store(deadline, Ordering::Relaxed);
    WRITES[cpu].fetch_add(1, Ordering::Relaxed);
    if deadline == NONE {
        ctl(CTL_IMASK);
        return;
    }
    // Already due: as soon as possible (an interrupt, not a call, so the
    // handler runs in the usual context).
    let delta = deadline.saturating_sub(crate::time::now_ns()).max(1_000);
    arm_ticks((delta as u128 * crate::time::tsc_hz() as u128 / 1_000_000_000) as u64);
}

/// The timer fired on this CPU: its level-triggered interrupt is cleared
/// by stopping it (one-shot) or re-arming it (periodic).
pub fn fired() {
    let cpu = super::percpu::cpu_id();
    match mode() {
        Mode::Periodic => arm_ticks(ticks_per_ms()),
        Mode::OneShot => {
            ctl(CTL_IMASK);
            if cpu < MAX_CPUS {
                ARMED[cpu].store(NONE, Ordering::Relaxed);
            }
        }
    }
}

/// How many times `cpu`'s timer has been written.
pub fn writes(cpu: usize) -> u64 {
    WRITES.get(cpu).map_or(0, |a| a.load(Ordering::Relaxed))
}

pub fn describe() -> &'static str {
    match mode() {
        Mode::Periodic => "periodic 1 kHz tick (generic timer)",
        Mode::OneShot => "generic timer deadline (tickless)",
    }
}
