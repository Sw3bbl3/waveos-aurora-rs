//! Memory management: physical frames, kernel heap, page tables, and
//! what happens when memory runs short (`kswapd`, `swap`, `swapfile`).

pub mod aslr;
pub mod frame;
pub mod heap;
pub mod kstack;
pub mod kswapd;
pub mod paging;
#[cfg(feature = "ktest")]
pub mod selftest;
pub mod swap;
#[cfg(feature = "ktest")]
pub mod swaptest;
pub mod swapfile;
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
/// that `kstack` keeps for the next task count as free too, and so does
/// the swap slot table, which grows with the most swap ever used.
#[cfg(feature = "ktest")]
pub fn leak_check_counts() -> (u64, u64) {
    let (total, free) = frame::counts();
    (total, free + heap::held_frames() + kstack::cached_frames() + swap::stats().table_frames)
}

/// The allocators' details for a telemetry snapshot, as JSON members to
/// append inside its `mem` object: free frames (all, and below 4 GiB), the
/// frames in the CPU caches, failed allocations and ignored double frees,
/// frames the heap gave back under pressure,
/// the CPU caches' hit rates (per mille), the longest interrupts-off stretch,
/// and `[size, objects in use, slabs]` for every size class in use.
pub fn write_telemetry(o: &mut impl core::fmt::Write) {
    let f = frame::stats();
    let m = kswapd::meminfo();
    let s = swap::stats();
    let (hits, misses) = heap::cache_counts();
    let permille = |h: u64, m: u64| (h * 1000).checked_div(h + m).unwrap_or(1000);
    let _ = write!(
        o,
        concat!(
            ",\"framesFree\":{},\"dma32Free\":{},\"framesCached\":{},\"frameFailures\":{},\"doubleFrees\":{},",
            "\"heapReclaimed\":{},\"frameCacheHit\":{},\"heapCacheHit\":{},\"irqOffMaxNs\":{},",
            "\"compressedBytes\":{},\"compressedPages\":{},\"swapTotal\":{},\"swapUsed\":{},",
            "\"diskSwapUsed\":{},\"swapIns\":{},\"swapOuts\":{},\"pressure\":{},\"oomKills\":{},\"classes\":["
        ),
        f.free,
        f.dma32_free,
        f.cached,
        f.failures,
        f.double_frees,
        heap::reclaimed_frames(),
        permille(f.cache_hits, f.cache_misses),
        permille(hits, misses),
        irq_off_max_ns(),
        m.compressed_bytes,
        m.compressed_pages,
        m.swap_total,
        m.swap_used,
        m.disk_swap_used,
        s.swapins,
        s.swapouts,
        kswapd::level(),
        kswapd::oom_kills()
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

/// Where memory is, for the log: frames, the heap (and its biggest size
/// classes), kernel stacks, the kernel virtual area, shared frames, the
/// block and map caches, pipes and tasks. Allocates nothing and waits for
/// no lock (what is busy reads as 0), so it can run when memory is out.
pub fn report(o: &mut impl core::fmt::Write) {
    let f = frame::stats_nowait();
    let (heap_size, heap_used) = heap::usage_nowait();
    let (pages, large) = heap::run_bytes();
    let (live, live_heap, cached, unmapped) = kstack::counts();
    let (kva, shm) = vmm::usage();
    let (tasks, user, sleeping) = crate::sched::task_counts();
    let _ = writeln!(
        o,
        concat!(
            "mem: free {} of {} frames ({} in CPU caches, {} failures); heap {} KiB held, {} KiB used ",
            "({} KiB in runs, {} KiB mapped); kstacks {} live + {} on the heap, {} cached, {} slots unmapped; ",
            "kva {} pages; shared frames {}; block cache {} blocks; map cache {} pages; pipes {}; ",
            "tasks {} ({} user, {} of them asleep)"
        ),
        f.free,
        f.total,
        f.cached,
        f.failures,
        heap_size / 1024,
        heap_used / 1024,
        pages / 1024,
        large / 1024,
        live,
        live_heap,
        cached,
        unmapped,
        kva,
        shm,
        crate::fs::cache::held_blocks(),
        crate::fs::mapcache::held_pages(),
        crate::proc::pipe::live(),
        tasks,
        user,
        sleeping,
    );
    let s = swap::stats_nowait();
    let _ = writeln!(
        o,
        concat!(
            "mem: swap {} of {} slots ({} table frames): {} pages compressed to {} KiB in {} pool frames, ",
            "{} of one word, {} on disk (of {}), {} cached shared; {} out, {} in, {} written back, {} refused; ",
            "pressure {}, {} OOM kills"
        ),
        s.slots,
        s.capacity,
        s.table_frames,
        s.zram_pages,
        s.zram_bytes / 1024,
        s.pool_frames,
        s.same_pages,
        s.disk_pages,
        swapfile::pages(),
        s.cached_pages,
        s.swapouts,
        s.swapins,
        s.writebacks,
        s.refused,
        kswapd::level(),
        kswapd::oom_kills(),
    );
    let avg_us = |(n, ns): (u64, u64)| (ns / 1000).checked_div(n).unwrap_or(0);
    let (reads, writes) = swapfile::io_counts();
    let _ = write!(
        o,
        "mem: swap-in {} us compressed ({} pages), {} us from disk ({} pages); swap file {} reads, {} writes",
        avg_us(s.in_zram),
        s.in_zram.0,
        avg_us(s.in_disk),
        s.in_disk.0,
        reads,
        writes,
    );
    if let Some(e) = swapfile::failure() {
        let _ = write!(o, " (none: {})", aurora_abi::err::name(e));
    }
    let _ = writeln!(o, "; {} counted frames", s.counted);
    let _ = write!(o, "mem: classes (size:live/slab KiB)");
    for c in 0..heap::CLASSES {
        let s = heap::class_stats_nowait(c);
        if s.slab_bytes >= 256 * 1024 {
            let _ = write!(o, " {}:{}/{}", s.size, s.live, s.slab_bytes / 1024);
        }
    }
    let _ = writeln!(o);
}

/// Memory ran short: every cache the kernel keeps gives back what it can:
/// cached kernel stacks, the filesystems' block caches, the cache of mapped
/// files, the heap's idle slabs and the CPUs' cached frames. Run from a
/// task that may wait (the reaper, at most a second after the allocator
/// saw the shortage); the allocator itself only does the last two, which
/// never wait. Returns the frames it freed.
pub fn reclaim() -> u64 {
    let before = frame::counts().1;
    kstack::trim_to(0);
    crate::fs::shrink_caches();
    crate::fs::mapcache::shrink();
    heap::reclaim();
    frame::drain_caches();
    frame::counts().1.saturating_sub(before)
}

/// Writes to the kernel console (for [`report`]).
pub struct Console;

impl core::fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        crate::kprint!("{s}");
        Ok(())
    }
}

/// A kernel heap allocation is about to fail (the caller will most likely
/// panic): says where memory went, the first few times.
pub(crate) fn on_heap_failure(size: usize) {
    static REPORTS: AtomicU64 = AtomicU64::new(0);
    if REPORTS.fetch_add(1, Ordering::Relaxed) < 3 {
        crate::kprintln!("\nmm: a kernel heap allocation of {size} bytes failed (fatal unless its caller copes)");
        report(&mut Console);
    }
}

/// Every process's threads, resident memory, swapped-out pages, page
/// tables and descriptors (allocates a list: only where the heap still works).
#[cfg_attr(not(feature = "ktest"), allow(dead_code))] // memory reports in self-tests
pub fn report_processes(o: &mut impl core::fmt::Write) {
    let _ = write!(o, "mem: procs (pid:name:threads:resident KiB:swapped pages:table frames:fds)");
    for p in crate::proc::all() {
        let (res, swapped, tables) = match p.aspace.try_lock() {
            Some(g) => g.as_ref().map_or((0, 0, 0), |a| (a.resident / 1024, a.swapped, a.table_frames())),
            None => (0, 0, 0),
        };
        let fds = p.fds.try_lock().map_or(0, |f| f.iter().filter(|h| h.is_some()).count());
        let _ = write!(o, " {}:{}:{}:{}:{}:{}:{}", p.pid, p.name(), p.thread_count(), res, swapped, tables, fds);
    }
    let _ = writeln!(o);
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
