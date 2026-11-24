//! Cache maintenance and store ordering, for memory a device reads.
//!
//! On QEMU's `virt` and on server-class Arm systems DMA is cache-coherent,
//! as on x86; what differs is ordering: stores to normal memory become
//! visible to a device only after a `dsb`.

/// Orders earlier stores to non-cacheable (write-combining) memory before
/// later stores (x86's `sfence`).
#[inline]
pub fn wc_fence() {
    // SAFETY: a barrier.
    unsafe { core::arch::asm!("dsb st", options(nostack, preserves_flags)) };
}

/// Orders every earlier memory access before later ones (x86's `mfence`).
#[inline]
pub fn full_fence() {
    super::regs::dsb_sy();
}

/// Cleans and invalidates the data cache line holding `p` to the point of
/// coherency (x86's `clflush`).
#[inline]
pub fn flush_line(p: *const u8) {
    // SAFETY: cache maintenance of a mapped address only affects caching.
    unsafe { core::arch::asm!("dc civac, {}", in(reg) p, options(nostack, preserves_flags)) };
}

/// Makes everything written so far visible to devices (x86's `wbinvd`).
/// Arm has no whole-cache clean by address range; set/way operations are
/// not safe with other CPUs running, and with coherent DMA a barrier is
/// what's needed.
#[inline]
pub fn write_back_all() {
    super::regs::dsb_sy();
}
