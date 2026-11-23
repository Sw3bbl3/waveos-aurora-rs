//! Monotonic kernel time.
//!
//! The clock is the CPU's time-stamp counter, calibrated against the HPET
//! (or the PIT) at boot, when the TSC runs at a constant rate (invariant TSC,
//! or any hypervisor). Otherwise the HPET counter itself, and as a last
//! resort the 1 kHz scheduler tick. "Ticks" are milliseconds.

use core::arch::x86_64::{__cpuid, _rdtsc};
use core::sync::atomic::{AtomicU64, Ordering};

/// Scheduler tick frequency in Hz (ticks are milliseconds).
pub const HZ: u64 = 1000;

static TICKS: AtomicU64 = AtomicU64::new(0);
/// TSC frequency in Hz (0 = not used as the clock).
static TSC_HZ: AtomicU64 = AtomicU64::new(0);
/// Nanoseconds per TSC tick, times 2^32 (the vDSO computes time the same way).
static TSC_MULT: AtomicU64 = AtomicU64::new(0);
static TSC_BASE: AtomicU64 = AtomicU64::new(0);
/// Added to the hardware clock, so time continues across sleep (the TSC or
/// HPET may restart from zero when the machine wakes).
static OFFSET_NS: core::sync::atomic::AtomicI64 = core::sync::atomic::AtomicI64::new(0);
static SUSPENDED_AT: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    Tsc,
    Hpet,
    Tick,
}

/// The fallback clock: counted by CPU 0's timer interrupt.
pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

/// Calibrates the TSC against the HPET or the PIT (call once, early, with interrupts off).
pub fn init_clock() {
    let invariant = __cpuid(0x8000_0000).eax >= 0x8000_0007 && __cpuid(0x8000_0007).edx & (1 << 8) != 0;
    let hypervisor = __cpuid(1).ecx & (1 << 31) != 0;
    if !invariant && !hypervisor {
        log!("time", "TSC not invariant; using {:?} as the clock", source());
        return;
    }
    const MS: u64 = 20;
    let hz = if crate::drivers::hpet::present() {
        hpet_tsc_hz()
    } else {
        let (t0, t1) = pit_wait(|| unsafe { _rdtsc() }, MS);
        (t1 - t0) * 1000 / MS
    };
    TSC_BASE.store(unsafe { _rdtsc() }, Ordering::Relaxed);
    TSC_MULT.store(((1_000_000_000u128 << 32) / hz as u128) as u64, Ordering::Relaxed);
    TSC_HZ.store(hz, Ordering::Release);
    log!("time", "TSC at {}.{:02} GHz is the clock", hz / 1_000_000_000, hz / 10_000_000 % 100);
}

/// The TSC frequency measured against the HPET: the median of several short
/// windows. Under a hypervisor the host can pause this CPU at any point, which
/// stretches one window or separates a TSC read from its HPET read; the median
/// discards such windows, and each sample keeps the TSC read that sat in the
/// tightest pair of HPET reads.
fn hpet_tsc_hz() -> u64 {
    use crate::drivers::hpet;
    let sample = || {
        let mut best = (u64::MAX, 0u64, 0u64);
        for _ in 0..8 {
            let h0 = hpet::nanos();
            let t = unsafe { _rdtsc() };
            let h1 = hpet::nanos();
            if h1 - h0 < best.0 {
                best = (h1 - h0, t, h0 + (h1 - h0) / 2);
            }
        }
        (best.1, best.2)
    };
    let mut rates = [0u64; 5];
    for rate in rates.iter_mut() {
        let (t0, h0) = sample();
        hpet::spin_us(8000);
        let (t1, h1) = sample();
        *rate = ((t1 - t0) as u128 * 1_000_000_000 / (h1 - h0).max(1) as u128) as u64;
    }
    rates.sort_unstable();
    rates[rates.len() / 2]
}

/// Reads `f` before and after `ms` milliseconds measured by PIT channel 2.
fn pit_wait(f: impl Fn() -> u64, ms: u64) -> (u64, u64) {
    use x86_64::instructions::port::Port;
    unsafe {
        let mut ctl = Port::<u8>::new(0x61);
        let mut mode = Port::<u8>::new(0x43);
        let mut ch2 = Port::<u8>::new(0x42);
        let v = ctl.read();
        ctl.write((v & !0x02) | 0x01);
        mode.write(0b1011_0000);
        let count = (1_193_182 * ms / 1000) as u16;
        ch2.write(count as u8);
        let a = f();
        ch2.write((count >> 8) as u8);
        while ctl.read() & 0x20 == 0 {}
        (a, f())
    }
}

/// The calibrated TSC frequency (0 if the TSC is not the clock).
pub fn tsc_hz() -> u64 {
    TSC_HZ.load(Ordering::Relaxed)
}

pub fn source() -> Source {
    if TSC_HZ.load(Ordering::Relaxed) != 0 {
        Source::Tsc
    } else if crate::drivers::hpet::present() {
        Source::Hpet
    } else {
        Source::Tick
    }
}

/// Nanoseconds since the clock started (sleep excluded).
pub fn now_ns() -> u64 {
    (raw_ns() as i64 + OFFSET_NS.load(Ordering::Relaxed)).max(0) as u64
}

/// Before sleeping: remember where time stands.
pub fn suspend() {
    SUSPENDED_AT.store(now_ns(), Ordering::Release);
}

/// After waking: restart the counters and continue from where time stood.
pub fn resume() {
    crate::drivers::hpet::resume();
    if TSC_HZ.load(Ordering::Relaxed) != 0 {
        TSC_BASE.store(unsafe { _rdtsc() }, Ordering::Relaxed);
    }
    let at = SUSPENDED_AT.load(Ordering::Acquire) as i64;
    OFFSET_NS.store(at - raw_ns() as i64, Ordering::Release);
    // The date moved on while asleep.
    resume_wall_clock();
    crate::compat::linux::vdso::publish();
}

/// What the vDSO needs to compute `now_ns` itself: whether the TSC is the
/// clock, where it started, nanoseconds per tick (times 2^32) and the offset.
pub fn tsc_params() -> (bool, u64, u64, i64) {
    (
        TSC_HZ.load(Ordering::Acquire) != 0,
        TSC_BASE.load(Ordering::Relaxed),
        TSC_MULT.load(Ordering::Relaxed),
        OFFSET_NS.load(Ordering::Relaxed),
    )
}

fn raw_ns() -> u64 {
    let hz = TSC_HZ.load(Ordering::Relaxed);
    if hz != 0 {
        let d = unsafe { _rdtsc() }.wrapping_sub(TSC_BASE.load(Ordering::Relaxed));
        return ((d as u128 * TSC_MULT.load(Ordering::Relaxed) as u128) >> 32) as u64;
    }
    if crate::drivers::hpet::present() {
        return crate::drivers::hpet::nanos();
    }
    TICKS.load(Ordering::Relaxed) * 1_000_000
}

/// Milliseconds since boot (the scheduler's unit).
pub fn ticks() -> u64 {
    now_ns() / 1_000_000
}

pub fn uptime_ms() -> u64 {
    ticks()
}

// ------------------------------------------------------------ wall clock
//
// Real time (UTC) is the monotonic clock plus one offset, `REAL_OFFSET_NS`,
// and nothing else: `realtime_ns` adds it to one reading of `now_ns`, and
// the vDSO adds the very same value (published with the TSC's parameters)
// to its own reading, so a program's clocks agree with the kernel's by
// construction. The offset changes only when the clock is set, when the
// zone the RTC keeps is learnt, and after sleep; each change is published
// at once. Local time (what the RTC holds, and file times) is real time
// plus the zone (`utc_offset`, minutes east of UTC).

use core::sync::atomic::AtomicI64;

/// Seconds from 1970-01-01 (Unix time) to 2000-01-01.
const UNIX_2000: i64 = 946_684_800;
const NS: i64 = 1_000_000_000;

/// UTC in nanoseconds since 1970, minus [`now_ns`].
static REAL_OFFSET_NS: AtomicI64 = AtomicI64::new(0);
/// The zone of local time (and of the RTC), in minutes east of UTC.
static ZONE_MIN: AtomicI64 = AtomicI64::new(0);
/// Changes to the clock, one at a time (so the vDSO's copy is the last one).
static WALL_LOCK: spin::Mutex<()> = spin::Mutex::new(());

/// The real-time offset that makes local time `local` (seconds since 2000) now.
fn offset_for_local(local: u64) -> i64 {
    (local as i64 + UNIX_2000 - ZONE_MIN.load(Ordering::Relaxed) * 60) * NS - now_ns() as i64
}

/// Reads the RTC (local time) at boot.
pub fn init_wall_clock() {
    let rtc = seconds_since_2000(&crate::drivers::rtc::now());
    REAL_OFFSET_NS.store(offset_for_local(rtc), Ordering::Release);
}

/// After sleep: the RTC tells how long it was. It counts whole seconds, so
/// unless it disagrees by more than that the clock just goes on (a
/// fraction of a second either way would make real time jump back).
fn resume_wall_clock() {
    let _g = WALL_LOCK.lock();
    let rtc = seconds_since_2000(&crate::drivers::rtc::now());
    if rtc.abs_diff(wall_seconds()) > 1 {
        REAL_OFFSET_NS.store(offset_for_local(rtc), Ordering::Release);
    }
}

/// UTC in nanoseconds since 1970 (`CLOCK_REALTIME`).
pub fn realtime_ns() -> u64 {
    (now_ns() as i64 + REAL_OFFSET_NS.load(Ordering::Acquire)).max(0) as u64
}

/// What [`realtime_ns`] adds to [`now_ns`] (for the vDSO).
pub fn real_offset_ns() -> i64 {
    REAL_OFFSET_NS.load(Ordering::Acquire)
}

/// Sets the date and time, as local time (hardware clock and the kernel's
/// wall clock); Linux programs see the change at once.
pub fn set_wall_clock(d: &crate::drivers::rtc::DateTime) {
    {
        let _g = WALL_LOCK.lock();
        crate::drivers::rtc::set(d);
        REAL_OFFSET_NS.store(offset_for_local(seconds_since_2000(d)), Ordering::Release);
    }
    crate::compat::linux::vdso::publish();
}

/// The zone the user chose (`utc_offset`): the instant stays, local time
/// moves, and the RTC (which keeps local time) is rewritten.
pub fn change_zone(minutes: i64) {
    let _g = WALL_LOCK.lock();
    ZONE_MIN.store(minutes, Ordering::Relaxed);
    let d = aurora_abi::calendar::from_seconds_since_2000(wall_seconds());
    crate::drivers::rtc::set(&crate::drivers::rtc::DateTime {
        year: d.year,
        month: d.month,
        day: d.day,
        hour: d.hour,
        minute: d.minute,
        second: d.second,
    });
}

/// The zone the RTC's time was in (read from the settings after boot):
/// local time stays, the instant it stands for moves.
pub fn learn_zone(minutes: i64) {
    {
        let _g = WALL_LOCK.lock();
        let old = ZONE_MIN.swap(minutes, Ordering::Relaxed);
        REAL_OFFSET_NS.fetch_sub((minutes - old) * 60 * NS, Ordering::AcqRel);
    }
    crate::compat::linux::vdso::publish();
}

/// The zone of local time, in minutes east of UTC.
pub fn zone_minutes() -> i64 {
    ZONE_MIN.load(Ordering::Relaxed)
}

/// Local wall-clock time in seconds since 2000-01-01 00:00.
pub fn wall_seconds() -> u64 {
    let local = realtime_ns() as i64 / NS - UNIX_2000 + ZONE_MIN.load(Ordering::Relaxed) * 60;
    local.max(0) as u64
}

pub fn seconds_since_2000(d: &crate::drivers::rtc::DateTime) -> u64 {
    // Days from civil date (Howard Hinnant's algorithm), relative to 2000-03-01 epoch shift.
    let (y, m) =
        if d.month <= 2 { (d.year as i64 - 1, d.month as i64 + 9) } else { (d.year as i64, d.month as i64 - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d.day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 730425; // 730425 = days from 0000-03-01 to 2000-01-01
    (days.max(0) as u64) * 86400 + d.hour as u64 * 3600 + d.minute as u64 * 60 + d.second as u64
}
