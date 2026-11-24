//! What an idle CPU does: picks an idle state and sleeps in it until an
//! interrupt (its timer, a device, or another CPU's IPI).
//!
//! **States.** Polling (spin on a flag; nothing to wake), `hlt` (C1), and
//! with MONITOR/MWAIT (CPUID.01H:ECX[3], leaf 5 ECX[0..1]) `mwait` with a
//! C-state hint: C1 always; on Intel, deeper ones from the sub-state counts
//! in CPUID leaf 5 EDX with conservative latencies; or the firmware's own
//! table from ACPI `_CST` (`acpi::processor`), which replaces them (FFH
//! entries are `mwait` hints, SystemIO ones a read of the P_LVLx port).
//! States deeper than C1 may stop the LAPIC timer and (without an invariant
//! TSC) the clock, so they are used only with ARAT (CPUID.06H:EAX[2]) and a
//! clock that survives them; there is no HPET broadcast fallback.
//! Under a hypervisor `hlt` is used, since `mwait` there usually spins
//! (QEMU's emulated `mwait` returns at once with more than one CPU).
//! `idle=halt|mwait|poll` in `boot.conf` overrides the choice.
//!
//! **Governor** (in the spirit of Linux's "menu"): the time to this CPU's
//! next timer, scaled by how long idle periods of that size really lasted
//! (other interrupts end them early), and capped by a repeating pattern in
//! the last eight idle periods, predicts the idle period; the deepest state
//! whose target residency fits it and whose exit latency the energy mode
//! allows is chosen.
//!
//! **Waking without an IPI.** A CPU that polls, or sleeps in `mwait` on its
//! own `need_resched` line, says so in `polling`; a waker that sets the flag
//! wakes it by that write alone (`sched::kick`).

use crate::arch::percpu::{self, MAX_CPUS};
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicU8, Ordering};
use crate::arch::interrupts;

pub const MAX_STATES: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Poll,
    Halt,
    /// `mwait` with this hint in EAX.
    Mwait(u32),
    /// A read of this I/O port (ACPI P_LVLx), then wake on interrupt.
    Port(u16),
}

#[derive(Clone, Debug)]
pub struct State {
    pub name: String,
    pub kind: Kind,
    /// Worst-case time to wake (ns).
    pub exit_latency: u64,
    /// Shortest stay that saves energy over the shallower state (ns).
    pub target_residency: u64,
    /// May stop the LAPIC timer (and a non-invariant TSC).
    pub deep: bool,
}

pub struct Table {
    pub states: Vec<State>,
    /// Where the states came from ("CPUID", "ACPI _CST", …).
    pub source: &'static str,
}

/// The current table; replaced once if ACPI `_CST` arrives (the old one is
/// leaked: idle CPUs may still be reading it).
static TABLE: AtomicPtr<Table> = AtomicPtr::new(core::ptr::null_mut());

/// Per CPU, on its own cache line: `mwait` monitors `need_resched`.
#[repr(C, align(128))]
pub struct IdleCpu {
    need_resched: AtomicBool,
    polling: AtomicBool,
    /// Entries and time (ns) per state.
    entries: [AtomicU64; MAX_STATES],
    residency: [AtomicU64; MAX_STATES],
    ipis: AtomicU64,
    ipis_avoided: AtomicU64,
    /// The governor's memory: recent idle periods (ns) and correction
    /// factors (actual / expected, ×1024) by expected length.
    history: [AtomicU64; 8],
    history_at: AtomicU8,
    correction: [AtomicU32; BUCKETS],
    /// Which `power::local_update` generation this CPU has applied.
    pub(super) applied: AtomicU64,
}

const BUCKETS: usize = 6;
const UNIT: u64 = 1024;

impl IdleCpu {
    const fn new() -> Self {
        Self {
            need_resched: AtomicBool::new(false),
            polling: AtomicBool::new(false),
            entries: [const { AtomicU64::new(0) }; MAX_STATES],
            residency: [const { AtomicU64::new(0) }; MAX_STATES],
            ipis: AtomicU64::new(0),
            ipis_avoided: AtomicU64::new(0),
            history: [const { AtomicU64::new(0) }; 8],
            history_at: AtomicU8::new(0),
            correction: [const { AtomicU32::new(UNIT as u32) }; BUCKETS],
            applied: AtomicU64::new(0),
        }
    }
}

pub static CPUS: [IdleCpu; MAX_CPUS] = [const { IdleCpu::new() }; MAX_CPUS];

/// `idle=` from boot.conf: 0 automatic, 1 halt only, 2 mwait (even in a VM), 3 poll.
static OVERRIDE: AtomicU8 = AtomicU8::new(0);
static MWAIT: AtomicBool = AtomicBool::new(false);
/// Exit latency the energy mode allows (ns).
static LATENCY_LIMIT: AtomicU64 = AtomicU64::new(u64::MAX);

fn hypervisor() -> bool {
    crate::arch::cpu::hypervisor()
}

/// MONITOR/MWAIT usable with interrupts as break events.
fn mwait_supported() -> bool {
    crate::arch::cpu::mwait_supported()
}

/// Whether states deeper than C1 keep time: the LAPIC timer keeps running
/// (ARAT) and the clock is not a TSC that might stop.
fn deep_allowed() -> bool {
    let invariant = crate::arch::cpu::invariant_counter();
    crate::arch::timer::always_running() && (invariant || crate::time::source() != crate::time::Source::Tsc)
}

/// Builds the state table from CPUID (call on the boot CPU, once the timer
/// mode is known).
pub fn init() {
    let hv = hypervisor();
    let ov = OVERRIDE.load(Ordering::Relaxed);
    let mwait = mwait_supported() && (ov == 2 || (!hv && ov != 1));
    MWAIT.store(mwait, Ordering::Relaxed);
    let mut states = alloc::vec![State {
        name: String::from("poll"),
        kind: Kind::Poll,
        exit_latency: 0,
        target_residency: 0,
        deep: false,
    }];
    // Waking from `hlt` in a VM is a round trip through the hypervisor.
    let (c1_exit, c1_target) = if hv { (10_000, 30_000) } else { (1_000, 2_000) };
    if ov != 3 {
        states.push(State {
            name: String::from(if mwait { "C1 (mwait)" } else { "C1 (hlt)" }),
            kind: if mwait { Kind::Mwait(0) } else { Kind::Halt },
            exit_latency: c1_exit,
            target_residency: c1_target,
            deep: false,
        });
    }
    if mwait && crate::arch::cpu::vendor() == "GenuineIntel" {
        let subs = crate::arch::cpu::mwait_substates();
        let count = |c: u32| (subs >> (4 * c)) & 0xF;
        if count(1) >= 2 {
            states.push(State {
                name: String::from("C1E (mwait 0x01)"),
                kind: Kind::Mwait(0x01),
                exit_latency: 10_000,
                target_residency: 20_000,
                deep: false,
            });
        }
        // MWAIT C-states 2..7 (hint (n-1) << 4): latencies are guesses on
        // the safe side; a firmware `_CST` replaces them.
        let guesses: [(u64, u64); 6] = [
            (80_000, 250_000),
            (120_000, 400_000),
            (200_000, 800_000),
            (300_000, 1_200_000),
            (500_000, 2_000_000),
            (800_000, 3_000_000),
        ];
        for n in 2..=7u32 {
            if count(n) == 0 || states.len() >= MAX_STATES {
                continue;
            }
            let (exit, target) = guesses[n as usize - 2];
            let hint = (n - 1) << 4;
            states.push(State {
                name: alloc::format!("C{} (mwait {:#04x})", n, hint),
                kind: Kind::Mwait(hint),
                exit_latency: exit,
                target_residency: target,
                deep: true,
            });
        }
    }
    install(Table { states, source: if mwait { "CPUID" } else { "hlt" } });
}

fn install(t: Table) {
    let p = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(t));
    TABLE.store(p, Ordering::Release);
}

pub fn table() -> Option<&'static Table> {
    let p = TABLE.load(Ordering::Acquire);
    (!p.is_null()).then(|| unsafe { &*p })
}

/// Replaces the states with the firmware's `_CST` (already filtered to
/// those this CPU can enter).
pub fn install_acpi(states: Vec<State>) {
    if states.is_empty() || OVERRIDE.load(Ordering::Relaxed) != 0 {
        return;
    }
    let mut all = alloc::vec![State {
        name: String::from("poll"),
        kind: Kind::Poll,
        exit_latency: 0,
        target_residency: 0,
        deep: false,
    }];
    all.extend(states.into_iter().take(MAX_STATES - 1));
    install(Table { states: all, source: "ACPI _CST" });
}

/// Reads `idle=` from boot.conf (once the file systems are up).
pub fn configure(option: Option<&str>) {
    let v = match option {
        Some("halt") | Some("hlt") => 1,
        Some("mwait") => 2,
        Some("poll") => 3,
        _ => 0,
    };
    if v != OVERRIDE.swap(v, Ordering::Relaxed) {
        init();
    }
}

/// The energy mode's limit on exit latency (ns).
pub fn set_latency_limit(ns: u64) {
    LATENCY_LIMIT.store(ns, Ordering::Relaxed);
}

pub fn uses_mwait() -> bool {
    MWAIT.load(Ordering::Relaxed)
}

// ------------------------------------------------------------- waking

pub fn set_need_resched(cpu: usize) {
    if let Some(c) = CPUS.get(cpu) {
        c.need_resched.store(true, Ordering::SeqCst);
    }
}

/// From `schedule`: the flag is seen. An interrupt taken while polling or in
/// `mwait` may switch this CPU to a task right away, so it no longer counts
/// as polling either (wakers must send IPIs again).
pub fn clear_need_resched(cpu: usize) {
    if let Some(c) = CPUS.get(cpu) {
        c.polling.store(false, Ordering::SeqCst);
        c.need_resched.store(false, Ordering::Relaxed);
    }
}

/// `cpu` is idle and watching its `need_resched` flag (no IPI needed).
pub fn polling(cpu: usize) -> bool {
    CPUS.get(cpu).is_some_and(|c| c.polling.load(Ordering::SeqCst))
}

pub fn ipi_avoided(cpu: usize) {
    if let Some(c) = CPUS.get(cpu) {
        c.ipis_avoided.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn ipi_received(cpu: usize) {
    if let Some(c) = CPUS.get(cpu) {
        c.ipis.fetch_add(1, Ordering::Relaxed);
    }
}

// ------------------------------------------------------------ governor

fn bucket(ns: u64) -> usize {
    match ns {
        0..10_000 => 0,
        10_000..100_000 => 1,
        100_000..1_000_000 => 2,
        1_000_000..10_000_000 => 3,
        10_000_000..100_000_000 => 4,
        _ => 5,
    }
}

/// A repeating idle period in the recent history (ns), if there is one:
/// the mean of the last eight, once outliers above it are dropped, if they
/// vary little (Linux menu's "typical interval").
fn typical_interval(c: &IdleCpu) -> Option<u64> {
    let mut v = [0u64; 8];
    for (d, h) in v.iter_mut().zip(&c.history) {
        *d = h.load(Ordering::Relaxed);
    }
    if v.contains(&0) {
        return None;
    }
    v.sort_unstable();
    let mut n = v.len();
    while n >= 6 {
        let w = &v[..n];
        let avg = w.iter().sum::<u64>() / n as u64;
        let var = w.iter().map(|&x| (x.abs_diff(avg) as u128).pow(2)).sum::<u128>() / n as u128;
        // Standard deviation within a sixth of the mean, or below 20 µs.
        if (avg as u128).pow(2) > 36 * var || var <= 400_000_000 {
            return Some(avg);
        }
        n -= 1; // drop the longest and look again
    }
    None
}

/// The deepest state whose target residency fits the predicted idle period
/// and whose exit latency is allowed; polling if none fits.
fn select(cpu: usize, t: &Table, until_timer: u64) -> usize {
    let c = &CPUS[cpu];
    let mut predicted = until_timer;
    if until_timer != u64::MAX {
        let corr = c.correction[bucket(until_timer)].load(Ordering::Relaxed) as u128;
        predicted = (until_timer as u128 * corr / UNIT as u128) as u64;
    }
    if let Some(typ) = typical_interval(c) {
        predicted = predicted.min(typ);
    }
    let limit = LATENCY_LIMIT.load(Ordering::Relaxed);
    let deep_ok = deep_allowed();
    let mut chosen = 0;
    for (i, s) in t.states.iter().enumerate().skip(1) {
        if (s.deep && !deep_ok) || s.exit_latency > limit {
            continue;
        }
        if s.target_residency <= predicted {
            chosen = i;
        }
    }
    chosen
}

fn record(cpu: usize, state: usize, slept: u64, until_timer: u64) {
    let c = &CPUS[cpu];
    if state < MAX_STATES {
        c.entries[state].fetch_add(1, Ordering::Relaxed);
        c.residency[state].fetch_add(slept, Ordering::Relaxed);
    }
    let at = c.history_at.load(Ordering::Relaxed) as usize % 8;
    c.history[at].store(slept.max(1), Ordering::Relaxed);
    c.history_at.store(((at + 1) % 8) as u8, Ordering::Relaxed);
    if until_timer != u64::MAX && until_timer > 0 {
        let b = bucket(until_timer);
        let ratio = (slept.min(until_timer) as u128 * UNIT as u128 / until_timer as u128) as u64;
        let old = c.correction[b].load(Ordering::Relaxed) as u64;
        c.correction[b].store(((old * 7 + ratio) / 8).max(1) as u32, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------- entry

/// How long a poll may spin before it gives up and halts after all.
fn poll_limit() -> u64 {
    if hypervisor() {
        50_000
    } else {
        10_000
    }
}

/// Puts this CPU into an idle state until an interrupt, unless work is
/// already queued for it. Returns with interrupts enabled.
pub fn enter() {
    interrupts::disable();
    let cpu = percpu::cpu_id();
    let me = &CPUS[cpu];
    if me.need_resched.load(Ordering::SeqCst) {
        interrupts::enable();
        return;
    }
    super::apply_local_updates(cpu);
    let Some(t) = table() else {
        interrupts::enable_and_hlt();
        return;
    };
    let now = crate::time::now_ns();
    let next = crate::timer::next_deadline(cpu).min(crate::sched::tick_deadline(cpu));
    let until_timer = if next == u64::MAX { u64::MAX } else { next.saturating_sub(now) };
    let i = select(cpu, t, until_timer);
    let state = &t.states[i];
    match state.kind {
        Kind::Poll => {
            me.polling.store(true, Ordering::SeqCst);
            interrupts::enable();
            let end = now.saturating_add(poll_limit().min(until_timer));
            while !me.need_resched.load(Ordering::Relaxed) && crate::time::now_ns() < end {
                core::hint::spin_loop();
            }
            me.polling.store(false, Ordering::SeqCst);
        }
        Kind::Halt => interrupts::enable_and_hlt(),
        Kind::Mwait(hint) => {
            me.polling.store(true, Ordering::SeqCst);
            let line = &me.need_resched as *const AtomicBool as *const u8;
            crate::arch::cpu::monitor_mwait(line, hint, || me.need_resched.load(Ordering::SeqCst));
            me.polling.store(false, Ordering::SeqCst);
        }
        Kind::Port(port) => {
            // The chipset stops the clock on the read; any interrupt resumes
            // it (with IF still clear), then it is taken.
            unsafe { crate::arch::io::Port::<u8>::new(port).read() };
            interrupts::enable();
        }
    }
    let slept = crate::time::now_ns().saturating_sub(now);
    record(cpu, i, slept, until_timer);
}

// --------------------------------------------------------------- report

pub struct Residency {
    pub name: String,
    pub entries: u64,
    pub ns: u64,
}

/// Per idle state of `cpu`: how often it was entered and for how long.
pub fn residency(cpu: usize) -> Vec<Residency> {
    let Some(t) = table() else { return Vec::new() };
    let c = &CPUS[cpu];
    t.states
        .iter()
        .enumerate()
        .take(MAX_STATES)
        .map(|(i, s)| Residency {
            name: s.name.clone(),
            entries: c.entries[i].load(Ordering::Relaxed),
            ns: c.residency[i].load(Ordering::Relaxed),
        })
        .collect()
}

/// (IPIs received, IPIs avoided because the CPU was polling).
pub fn ipi_counts(cpu: usize) -> (u64, u64) {
    let c = &CPUS[cpu];
    (c.ipis.load(Ordering::Relaxed), c.ipis_avoided.load(Ordering::Relaxed))
}

/// Total time `cpu` spent in idle states (ns).
pub fn idle_ns(cpu: usize) -> u64 {
    CPUS[cpu].residency.iter().map(|r| r.load(Ordering::Relaxed)).sum()
}
