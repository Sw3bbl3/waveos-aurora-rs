//! Cache maintenance and store ordering, for memory a device reads.
//!
//! x86 caches are coherent with DMA, so only memory the CPU maps uncached
//! or write-combining (framebuffers, GPU tables) needs explicit care.

/// Orders earlier write-combining stores before later stores (`sfence`).
#[inline]
pub fn wc_fence() {
    // SAFETY: a fence has no other effect.
    unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
}

/// Orders every earlier load and store before later ones (`mfence`).
#[inline]
pub fn full_fence() {
    // SAFETY: a fence has no other effect.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
}

/// Writes back and evicts the cache line holding `p` (`clflush`).
#[inline]
pub fn flush_line(p: *const u8) {
    // SAFETY: `clflush` of any mapped address only affects caching.
    unsafe { core::arch::asm!("clflush [{}]", in(reg) p, options(nostack, preserves_flags)) };
}

/// Writes back and invalidates every cache of this CPU (`wbinvd`): slow,
/// for before sleep or changing memory types.
#[inline]
pub fn write_back_all() {
    // SAFETY: only affects caching; ring 0.
    unsafe { core::arch::asm!("wbinvd", options(nostack)) };
}
