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

/// RDRAND/RDSEED through registers only (no address-taken locals, so this
/// function carries no canary of its own), mixed with the TSC.
#[inline(never)]
pub fn init() {
    let leaf1 = core::arch::x86_64::__cpuid(1).ecx;
    let leaf7 = core::arch::x86_64::__cpuid_count(7, 0).ebx;
    let mut v = unsafe { core::arch::x86_64::_rdtsc() }.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for _ in 0..4 {
        let (hw, ok): (u64, u8);
        unsafe {
            if leaf7 & (1 << 18) != 0 {
                core::arch::asm!("rdseed {0}", "setc {1}", out(reg) hw, out(reg_byte) ok, options(nomem, nostack));
            } else if leaf1 & (1 << 30) != 0 {
                core::arch::asm!("rdrand {0}", "setc {1}", out(reg) hw, out(reg_byte) ok, options(nomem, nostack));
            } else {
                (hw, ok) = (0, 0);
            }
        }
        let t = unsafe { core::arch::x86_64::_rdtsc() };
        v = v.rotate_left(17) ^ if ok != 0 { hw } else { 0 } ^ t.wrapping_mul(0xD6E8_FEB8_6659_FD93);
    }
    // A zero byte first stops string overruns from copying the canary.
    __stack_chk_guard.store(v & !0xFF, Ordering::Relaxed);
}

#[cfg(feature = "ktest")]
pub fn guard() -> u64 {
    __stack_chk_guard.load(Ordering::Relaxed)
}
