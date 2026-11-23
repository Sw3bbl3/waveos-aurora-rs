//! Kernel timers: a queue of deadlines per CPU, and the timer interrupt.
//!
//! Every CPU keeps its pending timers ordered by deadline (nanoseconds of
//! `time::now_ns`) and arms its local APIC timer (`arch::timer`) for the
//! earliest of them — or for the end of the running task's time slice while
//! other tasks wait for the CPU (`sched::tick_deadline`). There is no
//! periodic tick: a CPU with nothing due sleeps until its next deadline or
//! an interrupt. (The 1 kHz tick remains as a fallback when the only clock
//! is the tick itself, or with `timer=periodic` in `boot.conf`.)
//!
//! A timer is armed on the CPU that asks for it, so arming never needs
//! another CPU's hardware. It either wakes a sleeping task (every
//! `sched::sleep_ms` and `wait_until` timeout is one) or calls a function in
//! interrupt context. Cancelling works from any CPU.

use crate::arch::percpu::{self, MAX_CPUS};
use crate::sched::TaskId;
use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::instructions::interrupts;

pub const NONE: u64 = u64::MAX;

pub enum Action {
    /// Wakes a sleeping task; the number tells this sleep from later ones.
    Wake(TaskId, u64),
    /// Calls `f(arg)` with interrupts off on the CPU that armed it. It must
    /// be short and must not block. `name` counts its wakeups.
    Call { f: fn(u64), arg: u64, name: &'static str },
}

/// Identifies a pending timer, for [`cancel`].
#[derive(Clone, Copy, Debug)]
pub struct Handle {
    cpu: u16,
    deadline: u64,
    id: u64,
}

type Queue = BTreeMap<(u64, u64), Action>;

static QUEUES: [IrqMutex<Queue>; MAX_CPUS] = [const { IrqMutex::new(BTreeMap::new()) }; MAX_CPUS];
/// Each CPU's earliest deadline (read without the lock by the idle governor).
static NEXT: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(NONE) }; MAX_CPUS];
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Per-CPU counters: what woke each CPU, for `cpuinfo` and the telemetry.
pub struct CpuStats {
    /// Timer interrupts taken.
    pub interrupts: AtomicU64,
    /// Timers that expired (tasks woken, functions called).
    pub expired: AtomicU64,
    /// Interrupts for the end of a time slice (another task was waiting).
    pub slices: AtomicU64,
    /// Interrupts with nothing to do (armed early: a distance beyond the
    /// counter's range, or a timer cancelled after it was armed).
    pub empty: AtomicU64,
}

pub static STATS: [CpuStats; MAX_CPUS] = [const {
    CpuStats {
        interrupts: AtomicU64::new(0),
        expired: AtomicU64::new(0),
        slices: AtomicU64::new(0),
        empty: AtomicU64::new(0),
    }
}; MAX_CPUS];

/// Expirations of function timers, by name.
static CALLS: IrqMutex<[(&str, u64); 16]> = IrqMutex::new([("", 0); 16]);

fn front(q: &Queue) -> u64 {
    q.keys().next().map_or(NONE, |k| k.0)
}

/// Arms a timer on this CPU for `deadline` (ns of `time::now_ns`).
pub fn add(deadline: u64, action: Action) -> Handle {
    interrupts::without_interrupts(|| {
        let cpu = percpu::cpu_id();
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let earliest = {
            let mut q = QUEUES[cpu].lock();
            q.insert((deadline, id), action);
            let f = front(&q);
            NEXT[cpu].store(f, Ordering::Relaxed);
            f
        };
        if earliest == deadline {
            reprogram();
        }
        Handle { cpu: cpu as u16, deadline, id }
    })
}

/// Calls `f(arg)` in `ms` milliseconds, in interrupt context on this CPU.
pub fn call_after_ms(ms: u64, f: fn(u64), arg: u64, name: &'static str) -> Handle {
    let deadline = crate::time::now_ns().saturating_add(ms.saturating_mul(1_000_000));
    add(deadline, Action::Call { f, arg, name })
}

/// Removes a timer that has not fired yet (from any CPU). Its CPU's timer
/// may still interrupt once for it, finding nothing due.
pub fn cancel(h: Handle) -> bool {
    let key = (h.deadline, h.id);
    let remove = |cpu: usize| {
        let mut q = QUEUES[cpu].lock();
        let found = q.remove(&key).is_some();
        if found {
            NEXT[cpu].store(front(&q), Ordering::Relaxed);
        }
        found
    };
    if remove(h.cpu as usize) {
        return true;
    }
    // Moved when its CPU was parked (or it is firing right now).
    (0..MAX_CPUS).filter(|&c| c != h.cpu as usize).any(remove)
}

/// This CPU's earliest timer (`NONE`: none).
pub fn next_deadline(cpu: usize) -> u64 {
    NEXT.get(cpu).map_or(NONE, |n| n.load(Ordering::Relaxed))
}

/// Arms this CPU's timer for its next event: the earliest timer, or the end
/// of the running task's slice if others wait.
pub fn reprogram() {
    interrupts::without_interrupts(|| {
        let cpu = percpu::cpu_id();
        let next = next_deadline(cpu).min(crate::sched::tick_deadline(cpu));
        crate::arch::timer::program(next);
    });
}

/// Moves every timer of `from` (a CPU being parked) to `to`, then re-arms
/// this CPU (which must be `to`).
pub fn migrate(from: usize, to: usize) {
    if from == to {
        return;
    }
    interrupts::without_interrupts(|| {
        let moved = core::mem::take(&mut *QUEUES[from].lock());
        NEXT[from].store(NONE, Ordering::Relaxed);
        let mut q = QUEUES[to].lock();
        q.extend(moved);
        NEXT[to].store(front(&q), Ordering::Relaxed);
    });
    reprogram();
}

/// The timer interrupt (interrupts are off): runs what is due, then lets the
/// scheduler preempt, which re-arms the timer.
pub fn interrupt() {
    let cpu = percpu::cpu_id();
    crate::arch::timer::fired();
    let stats = &STATS[cpu];
    stats.interrupts.fetch_add(1, Ordering::Relaxed);
    let periodic = crate::arch::timer::local_mode() == crate::arch::timer::Mode::Periodic;
    if periodic && cpu == 0 {
        crate::time::tick();
    }
    let now = crate::time::now_ns();
    let mut ran = 0;
    loop {
        let due = {
            let mut q = QUEUES[cpu].lock();
            let due = q.first_key_value().is_some_and(|(k, _)| k.0 <= now);
            let entry = if due { q.pop_first() } else { None };
            NEXT[cpu].store(front(&q), Ordering::Relaxed);
            entry
        };
        let Some((_, action)) = due else { break };
        ran += 1;
        match action {
            Action::Wake(task, seq) => crate::sched::timer_expired(task, seq),
            Action::Call { f, arg, name } => {
                count_call(name);
                f(arg);
            }
        }
    }
    stats.expired.fetch_add(ran, Ordering::Relaxed);
    let slice = crate::sched::tick_deadline(cpu) <= now;
    if slice {
        stats.slices.fetch_add(1, Ordering::Relaxed);
    }
    if ran == 0 && !slice && !periodic {
        stats.empty.fetch_add(1, Ordering::Relaxed);
    }
    crate::sched::on_timer_tick();
}

fn count_call(name: &'static str) {
    let mut t = CALLS.lock();
    if let Some(e) = t.iter_mut().find(|e| e.0 == name || e.0.is_empty()) {
        e.0 = name;
        e.1 += 1;
    }
}

/// Function timers' expirations by name.
pub fn call_counts() -> alloc::vec::Vec<(&'static str, u64)> {
    CALLS.lock().iter().filter(|e| !e.0.is_empty()).copied().collect()
}

/// Pending timers on `cpu`.
pub fn pending(cpu: usize) -> usize {
    QUEUES.get(cpu).map_or(0, |q| q.lock().len())
}
