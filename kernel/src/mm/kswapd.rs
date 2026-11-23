//! Reclaim: the `kswapd` task, memory pressure, and the out-of-memory
//! killer.
//!
//! **Watermarks.** Below the *min* watermark (the kernel's reserve,
//! `frame::kernel_reserve`) programs get no more frames. When free memory
//! falls under *low* (min + 1/64 of memory) `kswapd` wakes, and works until
//! it is back over *high* (min + 1/32): first the kernel's caches give back
//! what they hold (`mm::reclaim`: the block caches, the mapped-file cache,
//! cached kernel stacks, idle heap slabs), then cached shared pages nobody
//! maps any more are written out (`swap::writeback`), then it goes round
//! the address spaces swapping out pages not used since it last passed
//! (`AddressSpace::reclaim`): those of programs in the background first,
//! the one in front last.
//!
//! **Who waits, and the rule against deadlock.** Only `kswapd` evicts
//! pages; no allocation ever does it itself. A user allocation that finds
//! memory down to the reserve ([`wait_for_memory`]) may wait for `kswapd`
//! only if it can sleep — interrupts on, so it holds no spinning lock —
//! and holds no address-space lock (allocations under one use
//! `frame::alloc_user_nowait`; page faults and `mmap` let go of the lock
//! and wait outside it, in `proc::with_memory`), and then for at most a
//! second. `kswapd` takes address-space locks only with `try_lock` (it
//! skips a busy one), and the locks it does wait for (a filesystem's, the
//! swap file's device) are never held by anyone waiting for memory for
//! longer than that second. So whatever a waiter holds, `kswapd` gets on,
//! or the waiter gives up: a failed `mmap` or system call (`ENOMEM`), or
//! for a page fault, the out-of-memory killer.
//!
//! **Out of memory.** When reclaim can't keep up — a page fault still
//! finds no memory after waiting, or `kswapd` went round everything
//! without freeing a page — [`out_of_memory`] ends one program, chosen by
//! [`badness`]: the memory killing it frees (pages in memory and swapped
//! out), a fraction of it for the program the user is using (1/8) and for
//! programs with windows (1/2), and nothing for system services (processes
//! marked `oom_protected`; the window server, the desktop and the rest of
//! the system run in the kernel and are never candidates), which are
//! chosen only if nothing else is left. One victim at a time: while it is on its way out,
//! others wait for it rather than kill again. The user gets a notification
//! naming it.
//!
//! **Pressure.** Time tasks spend waiting for memory is accounted as Linux
//! does for `/proc/pressure/memory` ("some": at least one task waiting),
//! as 10, 60 and 300 second averages; the level ([`level`]: none, warning,
//! critical) goes to every window as a `MEMORY_PRESSURE` event when it
//! changes.

use super::{frame, swap, swapfile, PAGE_SIZE};
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

static TASK: AtomicU64 = AtomicU64::new(u64::MAX);
static WANTED: AtomicBool = AtomicBool::new(false);
/// `kswapd`'s last round freed nothing and memory is still short: waiters
/// need not wait for it.
static FUTILE: AtomicBool = AtomicBool::new(false);
static ROUNDS: AtomicU64 = AtomicU64::new(0);
static OOM_KILLS: AtomicU64 = AtomicU64::new(0);
static LEVEL: AtomicU32 = AtomicU32::new(0);
/// When the level was last raised, or seen to be right.
static LEVEL_AT: AtomicU64 = AtomicU64::new(0);

pub fn low_watermark() -> u64 {
    let total = frame::counts().0;
    frame::kernel_reserve() + (total / 64).max(512)
}

pub fn high_watermark() -> u64 {
    let total = frame::counts().0;
    frame::kernel_reserve() + (total / 32).max(1024)
}

pub fn init() {
    swap::init();
    TASK.store(crate::sched::spawn("kswapd", run), Ordering::Relaxed);
}

/// Asks `kswapd` to make room (safe anywhere: interrupt handlers, with locks held).
pub fn wake() {
    if !WANTED.swap(true, Ordering::AcqRel) {
        let t = TASK.load(Ordering::Relaxed);
        if t != u64::MAX {
            crate::sched::wake(t);
        }
    }
}

/// Whether the running task may wait for memory: not with interrupts off
/// (it may hold a spinning lock), and not `kswapd` itself.
fn may_wait() -> bool {
    x86_64::instructions::interrupts::are_enabled() && crate::sched::current_id() != TASK.load(Ordering::Relaxed)
}

fn above_min() -> bool {
    frame::counts().1 > frame::kernel_reserve() + 64
}

/// Memory is down to the reserve: waits (at most a second) for `kswapd`
/// to make room. True if there is room now; false at once if the caller
/// may not wait or `kswapd` already found nothing to free.
pub fn wait_for_memory() -> bool {
    if !may_wait() {
        return false;
    }
    // (Even if the last round found nothing, there may be something now:
    // this waits for a round that started after it asked.)
    wake();
    stall_begin();
    let deadline = crate::time::uptime_ms() + 1000;
    let start_round = ROUNDS.load(Ordering::Relaxed);
    while !above_min() && crate::time::uptime_ms() < deadline {
        crate::sched::wait_until(10, || {
            above_min() || (FUTILE.load(Ordering::Relaxed) && ROUNDS.load(Ordering::Relaxed) >= start_round + 2)
        });
        if crate::proc::interrupted() {
            break;
        }
        if FUTILE.load(Ordering::Relaxed) && ROUNDS.load(Ordering::Relaxed) >= start_round + 2 {
            break;
        }
        wake();
    }
    stall_end();
    above_min()
}

fn run() {
    loop {
        crate::sched::wait_until(10_000, || WANTED.load(Ordering::Acquire));
        WANTED.store(false, Ordering::Release);
        let free = frame::counts().1;
        if free >= low_watermark() && !FUTILE.load(Ordering::Relaxed) {
            update_level();
            continue;
        }
        // The swap file, the first time memory runs short.
        swapfile::start();
        reclaim_to(high_watermark());
        ROUNDS.fetch_add(1, Ordering::Release);
        update_level();
    }
}

/// Frees memory until `target` frames are free, or nothing more can go.
fn reclaim_to(target: u64) {
    let free = || frame::counts().1;
    // The kernel's caches first: their pages cost nothing to drop.
    super::reclaim();
    let total = frame::counts().0;
    let mut idle_scanned = 0u64;
    let mut futile = false;
    let mut busy_rounds = 0;
    while free() < target {
        let want = target.saturating_sub(free()).clamp(64, 4096);
        let written = swap::writeback(want as usize) as u64;
        let (round, busy) = evict_round(want);
        if written + round.evicted > 0 {
            idle_scanned = 0;
            busy_rounds = 0;
        } else if busy > 0 {
            // Address spaces in use (a fault, a system call): soon free.
            // Never a reason to call it hopeless: after a while this round
            // ends, and waiters wait (or time out) for the next.
            busy_rounds += 1;
            if busy_rounds > 5000 {
                break;
            }
            crate::sched::yield_now();
            continue;
        } else {
            idle_scanned += round.scanned;
            // Twice round everything (once to clear the accessed bits,
            // once to find them still clear) without a page to show for it,
            // or swap is full: nothing more will come of it.
            if round.full || round.scanned == 0 || idle_scanned > 2 * total {
                futile = !swap::writeback_pending();
                if futile {
                    break;
                }
            }
        }
        crate::sched::yield_now();
    }
    FUTILE.store(futile && free() < target, Ordering::Release);
}

/// One pass over every address space, background programs (and bigger
/// ones) first, asking each for a share of `want` pages. Also returns how
/// many were busy (skipped).
fn evict_round(want: u64) -> (super::vmm::Reclaimed, u32) {
    // (Every allocation here may fail: memory is short.)
    let mut total = super::vmm::Reclaimed::default();
    let front = crate::gui::server::foreground();
    let Some(all) = crate::proc::try_all() else { return (total, 1) };
    let mut procs: Vec<(bool, u64, alloc::sync::Arc<crate::proc::Process>)> = Vec::new();
    if procs.try_reserve_exact(all.len()).is_err() {
        return (total, 1);
    }
    procs.extend(
        all.into_iter()
            .filter(|p| p.exit_code().is_none())
            .map(|p| (p.pid == front, p.resident_kib.load(Ordering::Relaxed), p)),
    );
    procs.sort_unstable_by_key(|(fg, kib, _)| (*fg, u64::MAX - kib));
    let mut full = false;
    let mut busy = 0;
    for (_, _, p) in procs {
        if total.evicted >= want {
            break;
        }
        // Never wait: whoever holds it may be waiting for memory.
        let Some(mut g) = p.aspace.try_lock() else {
            busy += 1;
            continue;
        };
        let Some(a) = g.as_mut() else { continue };
        let r = a.reclaim(4096, want - total.evicted);
        p.resident_kib.store(a.resident / 1024, Ordering::Relaxed);
        drop(g);
        total.scanned += r.scanned;
        total.evicted += r.evicted;
        total.failed += r.failed;
        full |= r.full;
    }
    total.full = full && total.evicted == 0;
    (total, busy)
}

/// Whether `kswapd` last found nothing it could free.
pub fn futile() -> bool {
    FUTILE.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------- pressure

struct Stall {
    waiting: u32,
    since_ns: u64,
    total_ns: u64,
    /// Averages over 10, 60 and 300 s, in hundredths of a percent, and when
    /// they were last brought up to date.
    avg: [u64; 3],
    total_at_avg: u64,
    avg_at_ns: u64,
}

static STALL: IrqMutex<Stall> =
    IrqMutex::new(Stall { waiting: 0, since_ns: 0, total_ns: 0, avg: [0; 3], total_at_avg: 0, avg_at_ns: 0 });

fn stall_begin() {
    let mut s = STALL.lock();
    if s.waiting == 0 {
        s.since_ns = crate::time::now_ns();
    }
    s.waiting += 1;
}

fn stall_end() {
    let mut s = STALL.lock();
    s.waiting = s.waiting.saturating_sub(1);
    if s.waiting == 0 {
        let now = crate::time::now_ns();
        s.total_ns += now.saturating_sub(s.since_ns);
    }
}

/// Time some task spent waiting for memory, and the 10, 60 and 300 second
/// averages of the share of time that was (hundredths of a percent).
pub fn pressure() -> (u64, [u64; 3]) {
    let mut s = STALL.lock();
    let now = crate::time::now_ns();
    let total = s.total_ns + if s.waiting > 0 { now.saturating_sub(s.since_ns) } else { 0 };
    // Averages move in 2-second steps, decaying as Linux's do: by
    // e^(-2/10), e^(-2/60) and e^(-2/300) a step (in 1/65536).
    const STEP: u64 = 2_000_000_000;
    const DECAY: [u64; 3] = [53_656, 63_388, 65_101];
    if s.avg_at_ns == 0 {
        s.avg_at_ns = now;
        s.total_at_avg = total;
    }
    let steps = (now.saturating_sub(s.avg_at_ns) / STEP).min(600);
    if steps > 0 {
        let stalled = total.saturating_sub(s.total_at_avg);
        // The share of the elapsed steps spent stalled, spread evenly.
        let share = (stalled * 10_000 / (steps * STEP)).min(10_000);
        for i in 0..3 {
            for _ in 0..steps {
                s.avg[i] = (s.avg[i] * DECAY[i] + share * (65_536 - DECAY[i])) / 65_536;
            }
        }
        s.avg_at_ns += steps * STEP;
        s.total_at_avg = total;
    }
    (total, s.avg)
}

/// Memory pressure: 0 none, 1 warning (memory is short and being
/// reclaimed, or swap is over half full), 2 critical (programs wait for
/// memory, or reclaim can't keep up).
pub fn level() -> u32 {
    LEVEL.load(Ordering::Relaxed)
}

fn update_level() {
    let free = frame::counts().1;
    let s = swap::stats();
    let (_, avg) = pressure();
    let seen = if FUTILE.load(Ordering::Relaxed) || avg[0] >= 1000 {
        2
    } else if free < low_watermark() || (s.zram_pages + s.disk_pages) * 2 > s.store_pages || avg[0] > 0 {
        1
    } else {
        0
    };
    // Up at once; down only after ten seconds without reason for the
    // higher level (programs shouldn't be told every second round).
    let now = crate::time::uptime_ms();
    let current = LEVEL.load(Ordering::Relaxed);
    let level = if seen >= current {
        LEVEL_AT.store(now, Ordering::Relaxed);
        seen
    } else if now >= LEVEL_AT.load(Ordering::Relaxed) + 10_000 {
        seen
    } else {
        current
    };
    if LEVEL.swap(level, Ordering::Relaxed) != level {
        log!("mm", "memory pressure: {}", ["none", "warning", "critical"][level as usize]);
        crate::gui::server::broadcast(aurora_abi::Event {
            kind: aurora_abi::event::MEMORY_PRESSURE,
            a: level,
            ..Default::default()
        });
    }
}

/// Out-of-memory kills since boot.
pub fn oom_kills() -> u64 {
    OOM_KILLS.load(Ordering::Relaxed)
}

// ------------------------------------------------------------ out of memory

/// The victim on its way out, and when it was chosen.
static VICTIM: IrqMutex<(u32, u64)> = IrqMutex::new((0, 0));
static OOM_LOCK: crate::sync::Mutex<()> = crate::sync::Mutex::new(());

/// How much ending process `p` is worth (see the module comment), and the
/// pages that would free.
pub fn badness(p: &crate::proc::Process, front: u32, windows: &[u32]) -> (u64, u64) {
    let pages = match p.aspace.try_lock() {
        Some(g) => g.as_ref().map_or(0, |a| {
            let c = a.page_counts();
            c.private + c.swapped
        }),
        // Busy: what it last said.
        None => p.resident_kib.load(Ordering::Relaxed) / 4,
    };
    let score = if p.oom_protected.load(Ordering::Relaxed) {
        0
    } else if p.pid == front {
        pages / 8
    } else if windows.contains(&p.pid) {
        pages / 2
    } else {
        pages
    };
    (score, pages)
}

/// Chooses the process to end: the highest [`badness`], the newest on a
/// tie; system services only if nothing else is left (then the biggest).
pub fn choose_victim() -> Option<(alloc::sync::Arc<crate::proc::Process>, u64)> {
    let front = crate::gui::server::foreground();
    let windows = crate::gui::server::window_pids();
    let mut best: Option<(u64, u64, alloc::sync::Arc<crate::proc::Process>)> = None;
    for p in crate::proc::try_all()? {
        if p.exit_code().is_some() || p.killed.load(Ordering::Acquire) {
            continue;
        }
        let (score, pages) = badness(&p, front, &windows);
        if pages == 0 {
            continue;
        }
        let key = (score, if score == 0 { pages } else { p.pid as u64 });
        if best.as_ref().is_none_or(|b| key > (b.0, b.1)) {
            best = Some((key.0, key.1, p));
        }
    }
    best.map(|(score, _, p)| (p, score))
}

/// Reclaim can't keep up: ends one program to free memory (or, if one is
/// already on its way out, waits for it). Call only where the caller may
/// wait (see [`wait_for_memory`]), holding no address-space lock.
pub fn out_of_memory() {
    if !may_wait() {
        return;
    }
    let _one = OOM_LOCK.lock();
    if above_min() {
        return;
    }
    let (pending, chosen_at) = *VICTIM.lock();
    let now = crate::time::uptime_ms();
    if pending != 0 && now < chosen_at + 10_000 && crate::proc::get(pending).is_some_and(|p| !p.is_reaped()) {
        wait_for_exit(pending);
        return;
    }
    let Some((victim, score)) = choose_victim() else {
        log!("mm", "out of memory, and no program to end");
        return;
    };
    let name = victim.name();
    log!(
        "mm",
        "out of memory: ending pid {} '{}' (score {}, {} KiB in memory)",
        victim.pid,
        name,
        score,
        victim.resident_kib.load(Ordering::Relaxed)
    );
    super::report(&mut super::Console);
    *VICTIM.lock() = (victim.pid, now);
    OOM_KILLS.fetch_add(1, Ordering::Relaxed);
    crate::proc::terminate_by_signal(&victim, 9);
    crate::telemetry::process("oom", victim.pid, &name, "killed to free memory");
    let mut body = String::from(name.as_str());
    body.push_str(" was closed because the computer ran out of memory.");
    crate::gui::notify::system("Out of memory", &body);
    let pid = victim.pid;
    drop(victim);
    wait_for_exit(pid);
}

/// Waits (at most a second) for process `pid` to be gone and its memory freed.
fn wait_for_exit(pid: u32) {
    let deadline = crate::time::uptime_ms() + 1000;
    while crate::time::uptime_ms() < deadline && crate::proc::get(pid).is_some_and(|p| !p.is_reaped()) {
        crate::sched::sleep_ms(10);
    }
    FUTILE.store(false, Ordering::Release);
}

// ------------------------------------------------------------------ figures

/// Memory figures for reports, `/proc/meminfo` and the system monitor.
pub struct MemInfo {
    pub total: u64,
    pub free: u64,
    /// Free plus what the kernel's caches would give back.
    pub available: u64,
    /// Bytes the compressed store holds, and the pages in it (uncompressed).
    pub compressed_bytes: u64,
    pub compressed_pages: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    pub disk_swap_total: u64,
    pub disk_swap_used: u64,
}

pub fn meminfo() -> MemInfo {
    let (total, free) = frame::counts();
    let s = swap::stats();
    let caches = crate::fs::cache::held_blocks() + crate::fs::mapcache::held_pages();
    let (zram_slots, _) = swap::limits();
    let disk = swapfile::pages();
    MemInfo {
        total: total * PAGE_SIZE,
        free: free * PAGE_SIZE,
        available: (free + caches).saturating_sub(frame::kernel_reserve()) * PAGE_SIZE,
        compressed_bytes: s.pool_frames * PAGE_SIZE,
        compressed_pages: s.zram_pages + s.same_pages,
        swap_total: (zram_slots + disk) * PAGE_SIZE,
        swap_used: (s.zram_pages + s.same_pages + s.disk_pages + s.cached_pages) * PAGE_SIZE,
        disk_swap_total: disk * PAGE_SIZE,
        disk_swap_used: s.disk_pages * PAGE_SIZE,
    }
}
