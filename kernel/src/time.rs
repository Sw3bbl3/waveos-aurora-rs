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
    let (t0, t1) = if crate::drivers::hpet::present() {
        let t0 = unsafe { _rdtsc() };
        crate::drivers::hpet::spin_us(MS * 1000);
        (t0, unsafe { _rdtsc() })
    } else {
        pit_wait(|| unsafe { _rdtsc() }, MS)
    };
    let hz = (t1 - t0) * 1000 / MS;
    TSC_BASE.store(unsafe { _rdtsc() }, Ordering::Relaxed);
    TSC_HZ.store(hz, Ordering::Release);
    log!("time", "TSC at {}.{:02} GHz is the clock", hz / 1_000_000_000, hz / 10_000_000 % 100);
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
    init_wall_clock_at(uptime_ms());
}

fn raw_ns() -> u64 {
    let hz = TSC_HZ.load(Ordering::Relaxed);
    if hz != 0 {
        let d = unsafe { _rdtsc() }.wrapping_sub(TSC_BASE.load(Ordering::Relaxed));
        return (d as u128 * 1_000_000_000 / hz as u128) as u64;
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

static BOOT_WALL: AtomicU64 = AtomicU64::new(0);

/// Records the RTC time at boot so wall-clock time can be derived from uptime.
pub fn init_wall_clock() {
    init_wall_clock_at(0);
}

fn init_wall_clock_at(uptime_ms: u64) {
    let now = seconds_since_2000(&crate::drivers::rtc::now());
    BOOT_WALL.store(now.saturating_sub(uptime_ms / 1000), Ordering::Relaxed);
}

/// Sets the date and time (hardware clock and the kernel's wall clock).
pub fn set_wall_clock(d: &crate::drivers::rtc::DateTime) {
    crate::drivers::rtc::set(d);
    BOOT_WALL.store(seconds_since_2000(d).saturating_sub(uptime_ms() / 1000), Ordering::Relaxed);
}

/// Local wall-clock time in seconds since 2000-01-01 00:00.
pub fn wall_seconds() -> u64 {
    BOOT_WALL.load(Ordering::Relaxed) + uptime_ms() / 1000
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
