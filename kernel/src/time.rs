//! Monotonic kernel time, driven by the LAPIC timer tick.

use core::sync::atomic::{AtomicU64, Ordering};

/// Timer frequency in Hz.
pub const HZ: u64 = 1000;

static TICKS: AtomicU64 = AtomicU64::new(0);

pub fn tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

pub fn uptime_ms() -> u64 {
    ticks() * 1000 / HZ
}

static BOOT_WALL: AtomicU64 = AtomicU64::new(0);

/// Records the RTC time at boot so wall-clock time can be derived from uptime.
pub fn init_wall_clock() {
    BOOT_WALL.store(seconds_since_2000(&crate::drivers::rtc::now()), Ordering::Relaxed);
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
