//! Kernel stack-smashing protection (`-Z stack-protector=strong`, set in
//! `.cargo/config.toml` together with `--cfg aurora_ssp`).
//!
//! Functions with arrays or address-taken locals store [`__stack_chk_guard`]
//! below their return address on entry and compare it on return; a
//! mismatch calls [`__stack_chk_fail`]. The guard is a per-boot random
//! value, written once by [`init`] as the very first thing `_start` does —
//! before any protected function that will return has been entered.
//! (`_start` itself never returns, so its own check never runs.)

use core::sync::atomic::{AtomicU64, Ordering};

/// The canary. An atomic so the kernel can write it once without
/// `static mut`; LLVM reads it as a plain 64-bit global.
#[no_mangle]
#[used]
#[allow(non_upper_case_globals)]
pub static __stack_chk_guard: AtomicU64 = AtomicU64::new(0x2f8e_57a1_c3d0_9b46);

/// Called by a protected function whose canary was overwritten.
#[no_mangle]
pub extern "C" fn __stack_chk_fail() -> ! {
    panic!("kernel stack smashing detected (stack protector canary overwritten)");
}

/// Whether the kernel was built with stack protection.
pub fn active() -> bool {
    cfg!(aurora_ssp)
}

/// Sets the canary from the CPU's random number generator and cycle
/// counter (`arch::rng::early_seed`, which keeps to registers, so neither
/// function carries a canary of its own).
#[inline(never)]
pub fn init() {
    let v = crate::arch::rng::early_seed();
    // A zero byte first stops string overruns from copying the canary.
    __stack_chk_guard.store(v & !0xFF, Ordering::Relaxed);
}

#[cfg(feature = "ktest")]
pub fn guard() -> u64 {
    __stack_chk_guard.load(Ordering::Relaxed)
}
