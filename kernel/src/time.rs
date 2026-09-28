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
