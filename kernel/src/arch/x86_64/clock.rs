//! The cycle counter as a clock: the TSC, calibrated at boot.

use super::cpu;
use super::io::Port;

/// What the clock code calls the counter.
pub const NAME: &str = "TSC";

/// The TSC's rate in Hz if it can be the clock: it runs at a constant rate
/// (invariant TSC, or under any hypervisor). Measured against the HPET, or
/// PIT channel 2 without one. Call once, early, with interrupts off.
pub fn counter_hz() -> Option<u64> {
    if !cpu::invariant_counter() && !cpu::hypervisor() {
        return None;
    }
    const MS: u64 = 20;
    Some(if crate::drivers::hpet::present() {
        hpet_tsc_hz()
    } else {
        let (t0, t1) = pit_wait(cpu::cycles, MS);
        (t1 - t0) * 1000 / MS
    })
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
            let t = cpu::cycles();
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
    // SAFETY: the PIT and port 0x61 belong to the kernel; channel 2 drives
    // only the speaker gate, which stays off.
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
