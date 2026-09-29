//! High Precision Event Timer: a fixed-rate counter (usually 10–25 MHz) that
//! serves as the reference clock for calibrating the TSC and the LAPIC timer.

use core::sync::atomic::{AtomicU64, Ordering};

const REG_CAPS: u64 = 0x000;
const REG_CONFIG: u64 = 0x010;
const REG_COUNTER: u64 = 0x0F0;

static BASE: AtomicU64 = AtomicU64::new(0);
/// Counter period in femtoseconds.
static PERIOD_FS: AtomicU64 = AtomicU64::new(0);

fn read(reg: u64) -> u64 {
    unsafe { ((BASE.load(Ordering::Relaxed) + reg) as *const u64).read_volatile() }
}

fn write(reg: u64, v: u64) {
    unsafe { ((BASE.load(Ordering::Relaxed) + reg) as *mut u64).write_volatile(v) }
}

pub fn init(phys: Option<u64>) {
    let Some(phys) = phys.filter(|&p| p != 0) else {
        log!("hpet", "no HPET; timing falls back to the PIT");
        return;
    };
    BASE.store(crate::mm::paging::map_mmio(phys, 0x400), Ordering::Relaxed);
    let period = read(REG_CAPS) >> 32;
    if period == 0 || period > 100_000_000 {
        BASE.store(0, Ordering::Relaxed);
        log!("hpet", "HPET at {:#x} reports an invalid period; ignoring it", phys);
        return;
    }
    PERIOD_FS.store(period, Ordering::Relaxed);
    write(REG_CONFIG, read(REG_CONFIG) | 1); // start the main counter
    log!("hpet", "HPET at {:#x}, {} MHz", phys, 1_000_000_000_000_000 / period / 1_000_000);
}

pub fn present() -> bool {
    PERIOD_FS.load(Ordering::Relaxed) != 0
}

/// Nanoseconds since the counter started.
pub fn nanos() -> u64 {
    ((read(REG_COUNTER) as u128 * PERIOD_FS.load(Ordering::Relaxed) as u128) / 1_000_000) as u64
}

/// Busy-waits `us` microseconds (needs `present()`).
pub fn spin_us(us: u64) {
    let end = nanos() + us * 1000;
    while nanos() < end {
        core::hint::spin_loop();
    }
}
