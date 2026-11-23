//! Address space layout randomization for user programs, from the kernel's
//! ChaCha20 generator (`random`).
//!
//! | What | Range | Entropy |
//! |---|---|---|
//! | `mmap` base (heap arenas, surfaces, Linux `mmap`) | `MMAP_BASE` + up to 1 TiB | 28 bits of pages |
//! | Stack top (WaveOS and Linux programs) | `STACK_TOP` − up to 4 GiB | 20 bits of pages, plus 8 bits of 16-byte offset for the first stack pointer |
//! | Linux position-independent programs | `0x1000_0000` + up to 64 GiB | 24 bits of pages |
//! | Linux dynamic linker (`ld.so`) | `0x7e00_0000_0000` + up to 512 GiB | 27 bits of pages |
//!
//! WaveOS programs themselves are still linked at a fixed address
//! (`layout::PROGRAM_BASE`); see docs/ARCHITECTURE.md ("Hardening") for the
//! plan to build them position-independent.
//!
//! `aslr=off` in `\aurora\boot.conf` turns it off (fixed addresses, for
//! debugging).

use super::PAGE_SIZE;
use aurora_abi::layout;
use core::sync::atomic::{AtomicU8, Ordering};

/// Where Linux programs built position-independent go, without ASLR.
pub const PIE_BASE: u64 = 0x1000_0000;
/// Where the dynamic linker goes, without ASLR: above the `mmap` region,
/// below the stack.
pub const INTERP_BASE: u64 = 0x7e00_0000_0000;

/// 0: not looked up yet, 1: on, 2: off.
static STATE: AtomicU8 = AtomicU8::new(0);

pub fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        // (Reading boot.conf may wait for the disk: not with interrupts off.)
        0 if !x86_64::instructions::interrupts::are_enabled() => true,
        0 => {
            // Only once the boot volume is there (the first program runs later).
            let off = crate::gui::prefs::boot_option("aslr").as_deref() == Some("off");
            if crate::fs::is_dir("/Boot") {
                STATE.store(if off { 2 } else { 1 }, Ordering::Relaxed);
            }
            !off
        }
        s => s == 1,
    }
}

/// A random multiple of the page size below `PAGE_SIZE << bits`.
fn pages(bits: u32) -> u64 {
    if !enabled() {
        return 0;
    }
    (crate::random::u64() & ((1 << bits) - 1)) * PAGE_SIZE
}

pub fn mmap_base() -> u64 {
    layout::MMAP_BASE + pages(28)
}

/// The top of the main thread's stack (its mapping ends here).
pub fn stack_top() -> u64 {
    layout::STACK_TOP - pages(20)
}

/// A 16-byte aligned offset below the stack top for the first stack frame.
pub fn stack_offset() -> u64 {
    if enabled() {
        (crate::random::u32() as u64 & 0xFF) * 16
    } else {
        0
    }
}

pub fn pie_base() -> u64 {
    PIE_BASE + pages(24)
}

pub fn interp_base() -> u64 {
    INTERP_BASE + pages(27)
}
