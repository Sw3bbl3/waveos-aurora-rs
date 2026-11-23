//! Memory management: physical frames, kernel heap, page tables.

pub mod aslr;
pub mod frame;
pub mod heap;
pub mod kstack;
pub mod paging;
#[cfg(feature = "ktest")]
pub mod selftest;
pub mod vmm;

use bootinfo::{BootInfo, PHYS_OFFSET};
use core::sync::atomic::{AtomicU64, Ordering};

pub const PAGE_SIZE: u64 = 4096;

/// Translates a physical address into the kernel's physical-memory window.
#[inline]
pub fn phys_to_virt(phys: u64) -> u64 {
    phys + PHYS_OFFSET
}

pub fn init(boot_info: &'static BootInfo) {
    frame::init(unsafe { boot_info.memory_map() });
    heap::init();
    paging::init(boot_info);
    vmm::init();
    crate::arch::protect::protect_kernel(vmm::kernel_pml4());
    heap::enable_large();
}

pub struct MemStats {
    pub total_bytes: u64,
    pub used_bytes: u64,
    pub heap_size: u64,
    pub heap_used: u64,
}

pub fn stats() -> MemStats {
    let (total, free) = frame::counts();
    let (heap_size, heap_used) = heap::usage();
    MemStats { total_bytes: total * PAGE_SIZE, used_bytes: (total - free) * PAGE_SIZE, heap_size, heap_used }
}

/// (total, free) frames for leak checks. Frames the heap holds for small
/// and mid-sized allocations count as free, as they did when the heap was
/// one region carved out at boot, so the heap growing on demand (or keeping
/// an empty slab) is not mistaken for a leak. The stacks of exited tasks
/// that `kstack` keeps for the next task count as free too.
#[cfg(feature = "ktest")]
pub fn leak_check_counts() -> (u64, u64) {
    let (total, free) = frame::counts();
    (total, free + heap::held_frames() + kstack::cached_frames())
}

/// The allocators' details for a telemetry snapshot, as JSON members to
/// append inside its `mem` object: free frames (all, and below 4 GiB), the
/// frames in the CPU caches, failed allocations and ignored double frees,
/// frames the heap gave back under pressure,
/// the CPU caches' hit rates (per mille), the longest interrupts-off stretch,
/// and `[size, objects in use, slabs]` for every size class in use.
pub fn write_telemetry(o: &mut impl core::fmt::Write) {
    let f = frame::stats();
    let (hits, misses) = heap::cache_counts();
    let permille = |h: u64, m: u64| (h * 1000).checked_div(h + m).unwrap_or(1000);
    let _ = write!(
        o,
        concat!(
            ",\"framesFree\":{},\"dma32Free\":{},\"framesCached\":{},\"frameFailures\":{},\"doubleFrees\":{},",
            "\"heapReclaimed\":{},\"frameCacheHit\":{},\"heapCacheHit\":{},\"irqOffMaxNs\":{},\"classes\":["
        ),
        f.free,
        f.dma32_free,
        f.cached,
        f.failures,
        f.double_frees,
        heap::reclaimed_frames(),
        permille(f.cache_hits, f.cache_misses),
        permille(hits, misses),
        irq_off_max_ns()
    );
    let mut first = true;
    for c in 0..heap::CLASSES {
        let s = heap::class_stats(c);
        if s.slabs > 0 || s.live > 0 {
            let _ = write!(o, "{}[{},{},{}]", if first { "" } else { "," }, s.size, s.live, s.slabs);
            first = false;
        }
    }
    let _ = write!(o, "]");
}

/// The longest stretch, in TSC cycles, an allocator kept interrupts off
/// (lock wait included).
static IRQ_OFF_MAX: AtomicU64 = AtomicU64::new(0);

#[inline]
pub(crate) fn tsc() -> u64 {
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// Records an interrupts-off stretch that began at `start` (a [`tsc`] value).
#[inline]
pub(crate) fn note_irq_off(start: u64) {
    let d = tsc().wrapping_sub(start);
    if d > IRQ_OFF_MAX.load(Ordering::Relaxed) && d < 1 << 40 {
        IRQ_OFF_MAX.fetch_max(d, Ordering::Relaxed);
    }
}

/// The longest interrupts-off stretch in the allocators, in nanoseconds
/// (0 before the TSC is calibrated).
pub fn irq_off_max_ns() -> u64 {
    let khz = crate::time::tsc_hz() / 1000;
    if khz == 0 {
        return 0;
    }
    IRQ_OFF_MAX.load(Ordering::Relaxed) * 1_000_000 / khz
}

#[cfg(feature = "ktest")]
pub fn reset_irq_off_max() {
    IRQ_OFF_MAX.store(0, Ordering::Relaxed);
}
