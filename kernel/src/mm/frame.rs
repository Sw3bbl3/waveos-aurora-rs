//! Physical frame allocator.
//!
//! A binary buddy allocator over every usable region, in two zones: below
//! 4 GiB ("DMA32", for devices with 32-bit addresses) and above. Free blocks
//! of 1 to 1024 frames (4 KiB to 4 MiB), each aligned to its size, sit on
//! per-zone, per-order lists linked through the free frames themselves, so a
//! run of any size up to 4 MiB is found without scanning, and freeing merges
//! a block with its free buddy in O(log n). Longer runs, and runs that must
//! end below an arbitrary limit, fall back to a scan.
//!
//! Single frames, by far the most common request (page tables, user pages,
//! window surfaces), go through a small cache per CPU, refilled from and
//! drained to the buddy lists in batches, so most of them never touch the
//! shared lock. When memory runs out the caches are drained and the heap is
//! asked to give back what it can before an allocation fails.
//!
//! Two bitmaps cover every frame up to the top of usable RAM (including any
//! above 4 GiB): `USED` (handed out to a caller; changed with atomics outside
//! the lock, which also catches double frees) and `heads` (a free block
//! starts here; under the lock).
//!
//! Normal allocations take low memory first, as the old next-fit allocator
//! did: some callers still rely on that (see `smp::boot_page_table`, and
//! controllers given `Dma::new` buffers through 32-bit registers). Devices
//! that need it ask explicitly with [`alloc_contiguous_below`].

use super::{note_irq_off, phys_to_virt, tsc, PAGE_SIZE};
use crate::arch::percpu::{cpu_id, MAX_CPUS};
use crate::sync::IrqMutex;
use bootinfo::{MemoryKind, MemoryRegion};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use x86_64::instructions::interrupts;

/// The largest buddy block: 2^10 frames (4 MiB).
pub const MAX_ORDER: u32 = 10;
const ORDERS: usize = MAX_ORDER as usize + 1;
const NONE: u64 = u64::MAX;
/// The first frame above the DMA32 zone.
const DMA32_END: u64 = (1 << 32) / PAGE_SIZE;
const DMA32: usize = 0;
const HIGH: usize = 1;
/// The order normal allocations try the zones in (low memory first).
const PREFERENCE: [usize; 2] = [DMA32, HIGH];
/// Frames below 1 MiB are kept back (legacy BIOS data, the SMP trampoline).
const LOW_RESERVED: u64 = 0x10_0000;

/// Single frames each CPU keeps, and how many move to or from the buddy
/// lists at a time.
const CACHE_MAX: usize = 64;
const BATCH: usize = 32;

/// Written into the first frame of every free block.
#[repr(C)]
struct FreeBlock {
    next: u64,
    prev: u64,
    order: u64,
    magic: u64,
}
const BLOCK_MAGIC: u64 = 0x6b63_6f6c_4265_6572; // "reeBlock"

fn block(frame: u64) -> &'static mut FreeBlock {
    unsafe { &mut *(phys_to_virt(frame * PAGE_SIZE) as *mut FreeBlock) }
}

fn zone_of(frame: u64) -> usize {
    if frame < DMA32_END {
        DMA32
    } else {
        HIGH
    }
}

fn order_for(count: u64) -> u32 {
    count.next_power_of_two().trailing_zeros()
}

struct Buddy {
    /// Frames covered by the bitmaps (the top of usable RAM / 4 KiB).
    frames: u64,
    heads: &'static mut [u64],
    lists: [[u64; ORDERS]; 2],
    /// Frames on each zone's lists.
    free: [u64; 2],
}

impl Buddy {
    fn is_head(&self, f: u64) -> bool {
        self.heads[(f / 64) as usize] & (1 << (f % 64)) != 0
    }

    /// The order of the free block starting at `f`, if one does.
    fn order_at(&self, f: u64) -> Option<u32> {
        if f >= self.frames || !self.is_head(f) {
            return None;
        }
        let b = block(f);
        debug_assert_eq!(b.magic, BLOCK_MAGIC, "frame allocator: free block {f:#x} overwritten");
        Some(b.order as u32)
    }

    fn push(&mut self, f: u64, order: u32) {
        let z = zone_of(f);
        let head = self.lists[z][order as usize];
        *block(f) = FreeBlock { next: head, prev: NONE, order: order as u64, magic: BLOCK_MAGIC };
        if head != NONE {
            block(head).prev = f;
        }
        self.lists[z][order as usize] = f;
        self.heads[(f / 64) as usize] |= 1 << (f % 64);
        self.free[z] += 1 << order;
    }

    fn remove(&mut self, f: u64, order: u32) {
        let z = zone_of(f);
        let b = block(f);
        let (next, prev) = (b.next, b.prev);
        b.magic = 0;
        if prev != NONE {
            block(prev).next = next;
        } else {
            self.lists[z][order as usize] = next;
        }
        if next != NONE {
            block(next).prev = prev;
        }
        self.heads[(f / 64) as usize] &= !(1 << (f % 64));
        self.free[z] -= 1 << order;
    }

    /// Gives back the block of `2^order` frames at `f`, merging it with its
    /// buddy for as long as that is free too.
    fn release(&mut self, mut f: u64, mut order: u32) {
        while order < MAX_ORDER {
            let buddy = f ^ (1 << order);
            if self.order_at(buddy) != Some(order) {
                break;
            }
            self.remove(buddy, order);
            f &= !(1 << order);
            order += 1;
        }
        self.push(f, order);
    }

    /// Gives back frames `start..end` as the largest aligned blocks.
    fn release_range(&mut self, mut start: u64, end: u64) {
        while start < end {
            let mut order = MAX_ORDER.min(start.trailing_zeros());
            while start + (1 << order) > end {
                order -= 1;
            }
            self.release(start, order);
            start += 1 << order;
        }
    }

    /// Takes a block of `2^order` frames from zone `z` whose first `need`
    /// frames end at or below frame `limit`, splitting a larger one if needed.
    fn take(&mut self, z: usize, order: u32, need: u64, limit: u64) -> Option<u64> {
        for o in order..=MAX_ORDER {
            let mut f = self.lists[z][o as usize];
            while f != NONE && f + need > limit {
                f = block(f).next;
            }
            if f == NONE {
                continue;
            }
            self.remove(f, o);
            let mut o = o;
            while o > order {
                o -= 1;
                self.push(f + (1 << o), o);
            }
            return Some(f);
        }
        None
    }

    fn take_one(&mut self) -> Option<u64> {
        PREFERENCE.iter().find_map(|&z| self.take(z, 0, 1, NONE))
    }

    /// Fills `out` with free frames for a CPU cache, the lowest first in
    /// pop order. Small leftover blocks go first; otherwise whole blocks as
    /// big as fit, so a refill is a few list operations, not one per frame.
    fn take_batch(&mut self, out: &mut [u64]) -> usize {
        let mut n = 0;
        while n < out.len() {
            let room = (out.len() - n) as u64;
            let fits = 63 - room.leading_zeros();
            let found = PREFERENCE.iter().find_map(|&z| {
                let smallest = (0..=MAX_ORDER).find(|&o| self.lists[z][o as usize] != NONE)?;
                let o = smallest.min(fits);
                self.take(z, o, 1, NONE).map(|f| (f, o))
            });
            let Some((f, o)) = found else { break };
            for i in (0..1u64 << o).rev() {
                out[n] = f + i;
                n += 1;
            }
        }
        n
    }

    /// Gives back single frames, merging neighbours into runs first.
    fn release_frames(&mut self, frames: &mut [u64]) {
        frames.sort_unstable();
        let mut i = 0;
        while i < frames.len() {
            let mut j = i + 1;
            while j < frames.len() && frames[j] == frames[j - 1] + 1 {
                j += 1;
            }
            self.release_range(frames[i], frames[j - 1] + 1);
            i = j;
        }
    }

    /// `count` contiguous frames aligned to `2^align_order`, from the lists.
    fn alloc_run(&mut self, count: u64, align_order: u32, limit: u64) -> Option<u64> {
        let order = order_for(count).max(align_order);
        if order > MAX_ORDER {
            return None;
        }
        let zones: &[usize] = if limit <= DMA32_END { &[DMA32] } else { &PREFERENCE };
        for &z in zones {
            if let Some(f) = self.take(z, order, count, limit) {
                self.release_range(f + count, f + (1 << order));
                return Some(f);
            }
        }
        None
    }

    /// The free block containing frame `f`, as (first frame, order).
    fn containing(&self, f: u64) -> Option<(u64, u32)> {
        (0..=MAX_ORDER).find_map(|o| {
            let head = f & !((1 << o) - 1);
            self.order_at(head).filter(|&k| k >= o).map(|k| (head, k))
        })
    }

    /// The slow path: scans for `count` free frames in a row below `limit`
    /// (runs longer than the largest block, or a limit inside a zone).
    fn scan(&mut self, count: u64, align_order: u32, limit: u64) -> Option<u64> {
        let step = 1u64 << align_order;
        let end = self.frames.min(limit);
        let used = used();
        let mut s = (LOW_RESERVED / PAGE_SIZE).next_multiple_of(step);
        'candidates: while s + count <= end {
            let mut f = s;
            while f < s + count {
                match self.containing(f) {
                    Some((head, order)) => f = head + (1 << order),
                    None => {
                        // Skip whole words of frames in use.
                        let mut next = f + 1;
                        while next.is_multiple_of(64)
                            && next < end
                            && used[(next / 64) as usize].load(Ordering::Relaxed) == u64::MAX
                        {
                            next += 64;
                        }
                        s = next.next_multiple_of(step);
                        continue 'candidates;
                    }
                }
            }
            // Carve s..s+count out of the blocks that hold it.
            let mut f = s;
            while f < s + count {
                let (head, order) = self.containing(f).unwrap();
                self.remove(head, order);
                let block_end = head + (1 << order);
                if head < s {
                    self.release_range(head, s);
                }
                if block_end > s + count {
                    self.release_range(s + count, block_end);
                }
                f = block_end;
            }
            return Some(s);
        }
        None
    }
}

static BUDDY: IrqMutex<Option<Buddy>> = IrqMutex::new(None);
static TOTAL: AtomicU64 = AtomicU64::new(0);
static USED_PTR: AtomicUsize = AtomicUsize::new(0);
static USED_WORDS: AtomicUsize = AtomicUsize::new(0);
static DOUBLE_FREES: AtomicU64 = AtomicU64::new(0);
static FAILURES: AtomicU64 = AtomicU64::new(0);

/// One bit per frame: handed out to a caller.
fn used() -> &'static [AtomicU64] {
    let p = USED_PTR.load(Ordering::Acquire);
    if p == 0 {
        return &[];
    }
    unsafe { core::slice::from_raw_parts(p as *const AtomicU64, USED_WORDS.load(Ordering::Relaxed)) }
}

fn mark_used(start: u64, count: u64) {
    let used = used();
    for f in start..start + count {
        let old = used[(f / 64) as usize].fetch_or(1 << (f % 64), Ordering::Relaxed);
        debug_assert_eq!(old & (1 << (f % 64)), 0, "frame {f:#x} handed out twice");
    }
}

/// Clears a frame's used bit; false if it was not in use (a double free).
fn mark_free(f: u64) -> bool {
    let Some(word) = used().get((f / 64) as usize) else { return false };
    if f < LOW_RESERVED / PAGE_SIZE || word.fetch_and(!(1 << (f % 64)), Ordering::Relaxed) & (1 << (f % 64)) == 0 {
        DOUBLE_FREES.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    true
}

#[repr(align(64))]
struct Cache {
    frames: IrqMutex<([u64; CACHE_MAX], usize)>,
    len: AtomicU64,
    hits: AtomicU64,
    misses: AtomicU64,
}

static CACHES: [Cache; MAX_CPUS] = [const {
    Cache {
        frames: IrqMutex::new(([0; CACHE_MAX], 0)),
        len: AtomicU64::new(0),
        hits: AtomicU64::new(0),
        misses: AtomicU64::new(0),
    }
}; MAX_CPUS];

fn this_cache() -> &'static Cache {
    &CACHES[cpu_id().min(MAX_CPUS - 1)]
}

/// Adds one to a counter only its own CPU's cache lock holder writes.
fn bump(c: &AtomicU64) {
    c.store(c.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

/// Runs `f` with interrupts off, recording how long they were.
fn irq_off<R>(f: impl FnOnce() -> R) -> R {
    interrupts::without_interrupts(|| {
        let t = tsc();
        let r = f();
        note_irq_off(t);
        r
    })
}

pub fn init(map: &[MemoryRegion]) {
    let usable = |r: &&MemoryRegion| r.kind == MemoryKind::Usable;
    let top = map.iter().filter(usable).map(|r| r.end()).max().unwrap_or(0);
    let frames = top / PAGE_SIZE;
    let words = frames.div_ceil(64) as usize;
    // Two bitmaps: used, then heads.
    let bitmap_bytes = (words * 8 * 2) as u64;

    let host = map
        .iter()
        .filter(usable)
        .find(|r| r.start >= LOW_RESERVED && r.len >= bitmap_bytes + PAGE_SIZE)
        .expect("no region large enough for the frame bitmaps");
    let base = phys_to_virt(host.start);
    let used = unsafe { core::slice::from_raw_parts_mut(base as *mut u64, words) };
    used.fill(u64::MAX);
    let heads = unsafe { core::slice::from_raw_parts_mut((base + words as u64 * 8) as *mut u64, words) };
    heads.fill(0);
    USED_PTR.store(base as usize, Ordering::Release);
    USED_WORDS.store(words, Ordering::Release);

    let mut buddy = Buddy { frames, heads, lists: [[NONE; ORDERS]; 2], free: [0; 2] };
    let bitmap_start = host.start / PAGE_SIZE;
    let bitmap_end = bitmap_start + bitmap_bytes.div_ceil(PAGE_SIZE);
    let used = self::used();
    for r in map.iter().filter(usable) {
        let first = r.start.div_ceil(PAGE_SIZE).max(LOW_RESERVED / PAGE_SIZE);
        let last = r.end() / PAGE_SIZE;
        // The region, less the bitmaps.
        for (a, b) in [(first, last.min(bitmap_start)), (first.max(bitmap_end), last)] {
            if a < b {
                for f in a..b {
                    used[(f / 64) as usize].fetch_and(!(1 << (f % 64)), Ordering::Relaxed);
                }
                buddy.release_range(a, b);
            }
        }
    }
    let total = buddy.free[DMA32] + buddy.free[HIGH];
    TOTAL.store(total, Ordering::Relaxed);
    log!(
        "mm",
        "{} MiB usable RAM ({} frames, {} MiB above 4 GiB), bitmaps {} KiB",
        (total * PAGE_SIZE) >> 20,
        total,
        (buddy.free[HIGH] * PAGE_SIZE) >> 20,
        bitmap_bytes / 1024
    );
    *BUDDY.lock() = Some(buddy);
}

/// Allocates one frame (its contents are whatever was there); returns its
/// physical address.
pub fn alloc() -> Option<u64> {
    if let Some(f) = irq_off(alloc_cached) {
        return Some(f);
    }
    // Out of memory: take back what the other CPUs and the heap hold.
    for _ in 0..2 {
        relieve_pressure();
        if let Some(f) = irq_off(|| BUDDY.lock().as_mut()?.take_one()) {
            mark_used(f, 1);
            return Some(f * PAGE_SIZE);
        }
    }
    FAILURES.fetch_add(1, Ordering::Relaxed);
    None
}

fn alloc_cached() -> Option<u64> {
    let cache = this_cache();
    let mut c = cache.frames.lock();
    let (frames, n) = &mut *c;
    if *n == 0 {
        bump(&cache.misses);
        *n = BUDDY.lock().as_mut()?.take_batch(&mut frames[..BATCH]);
        if *n == 0 {
            return None;
        }
    } else {
        bump(&cache.hits);
    }
    *n -= 1;
    let f = frames[*n];
    cache.len.store(*n as u64, Ordering::Relaxed);
    mark_used(f, 1);
    Some(f * PAGE_SIZE)
}

/// Gives back one frame. Freeing a frame that is not allocated is ignored
/// (and counted).
pub fn free(phys: u64) {
    let f = phys / PAGE_SIZE;
    if !mark_free(f) {
        return;
    }
    irq_off(|| {
        let cache = this_cache();
        let mut c = cache.frames.lock();
        let (frames, n) = &mut *c;
        if *n == CACHE_MAX {
            BUDDY.lock().as_mut().unwrap().release_frames(&mut frames[CACHE_MAX - BATCH..]);
            *n -= BATCH;
        }
        frames[*n] = f;
        *n += 1;
        cache.len.store(*n as u64, Ordering::Relaxed);
    })
}

/// Allocates `count` physically contiguous frames.
pub fn alloc_contiguous(count: u64) -> Option<u64> {
    alloc_run(count, 0, u64::MAX)
}

/// Allocates `count` physically contiguous frames that end below `limit`
/// (for devices that address only the first 4 GiB).
pub fn alloc_contiguous_below(count: u64, limit: u64) -> Option<u64> {
    alloc_run(count, 0, limit)
}

/// Allocates `count` contiguous frames, the first aligned to
/// `2^align_order` frames, all below the physical address `limit`.
pub fn alloc_run(count: u64, align_order: u32, limit: u64) -> Option<u64> {
    if count == 0 {
        return None;
    }
    let limit = if limit == u64::MAX { NONE } else { limit / PAGE_SIZE };
    for attempt in 0..3 {
        let found = irq_off(|| {
            let mut b = BUDDY.lock();
            let b = b.as_mut()?;
            b.alloc_run(count, align_order, limit).or_else(|| b.scan(count, align_order, limit))
        });
        if let Some(f) = found {
            mark_used(f, count);
            return Some(f * PAGE_SIZE);
        }
        if attempt < 2 {
            relieve_pressure();
        }
    }
    FAILURES.fetch_add(1, Ordering::Relaxed);
    None
}

/// Gives back `count` contiguous frames from [`alloc_run`] (or any frames
/// that happen to be contiguous).
pub fn free_run(phys: u64, count: u64) {
    if count == 1 {
        return free(phys);
    }
    let start = phys / PAGE_SIZE;
    // Runs of frames that really were in use (normally all of them).
    let mut f = start;
    while f < start + count {
        if !mark_free(f) {
            f += 1;
            continue;
        }
        let run = f;
        f += 1;
        while f < start + count && mark_free(f) {
            f += 1;
        }
        let end = f;
        irq_off(|| BUDDY.lock().as_mut().unwrap().release_range(run, end));
        f += 1;
    }
}

/// Moves every CPU's cached frames back to the buddy lists (so they can
/// form contiguous runs again). A cache that is busy right now is skipped.
pub fn drain_caches() {
    for cache in CACHES.iter() {
        irq_off(|| {
            let Some(mut c) = cache.frames.try_lock() else { return };
            let (frames, n) = &mut *c;
            if *n == 0 {
                return;
            }
            let mut b = BUDDY.lock();
            let Some(b) = b.as_mut() else { return };
            b.release_frames(&mut frames[..*n]);
            *n = 0;
            cache.len.store(0, Ordering::Relaxed);
        });
    }
}

/// Memory is short: the heap returns its idle slabs and the CPUs their
/// cached frames.
fn relieve_pressure() {
    super::heap::reclaim();
    drain_caches();
}

/// Whether free memory is down to the part kept for the kernel (refreshed
/// every few user allocations: reading the exact count takes the allocator's
/// lock).
static LOW: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static USER_ALLOCS: AtomicU64 = AtomicU64::new(0);

/// Frames user programs can't take: 1/32 of memory, between 8 and 64 MiB.
/// The kernel's heap, page tables and drivers allocate on demand, so a
/// program that maps all of memory must fail before they do.
fn kernel_reserve() -> u64 {
    (TOTAL.load(Ordering::Relaxed) / 32).clamp(2048, 16384)
}

/// One frame for a user program's memory (or a window surface): like
/// [`alloc`], but fails once only the kernel's reserve is left.
pub fn alloc_user() -> Option<u64> {
    if USER_ALLOCS.fetch_add(1, Ordering::Relaxed).is_multiple_of(32) || LOW.load(Ordering::Relaxed) {
        LOW.store(counts().1 <= kernel_reserve(), Ordering::Relaxed);
    }
    if LOW.load(Ordering::Relaxed) {
        return None;
    }
    alloc()
}

/// (total usable frames, free frames)
pub fn counts() -> (u64, u64) {
    let s = stats();
    (s.total, s.free)
}

pub struct FrameStats {
    pub total: u64,
    /// Free frames, including those in the CPU caches.
    pub free: u64,
    /// Free frames below 4 GiB on the buddy lists.
    pub dma32_free: u64,
    /// Free frames in the CPU caches.
    pub cached: u64,
    /// Single-frame allocations served from a CPU cache, and refills.
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Frees of frames that were not allocated (ignored).
    pub double_frees: u64,
    /// Allocations that failed for want of memory.
    pub failures: u64,
}

pub fn stats() -> FrameStats {
    let (dma32, high) = BUDDY.lock().as_ref().map_or((0, 0), |b| (b.free[DMA32], b.free[HIGH]));
    let sum = |f: fn(&Cache) -> &AtomicU64| CACHES.iter().map(|c| f(c).load(Ordering::Relaxed)).sum::<u64>();
    let cached = sum(|c| &c.len);
    FrameStats {
        total: TOTAL.load(Ordering::Relaxed),
        free: dma32 + high + cached,
        dma32_free: dma32,
        cached,
        cache_hits: sum(|c| &c.hits),
        cache_misses: sum(|c| &c.misses),
        double_frees: DOUBLE_FREES.load(Ordering::Relaxed),
        failures: FAILURES.load(Ordering::Relaxed),
    }
}

/// Checks the buddy lists against the bitmaps (self-tests): every block is
/// aligned, in the right zone, marked as a head, not in use, and not left
/// unmerged with a free buddy; the lists add up to the free counts.
#[cfg(feature = "ktest")]
pub fn check() {
    irq_off(|| {
        let g = BUDDY.lock();
        let b = g.as_ref().unwrap();
        let used = used();
        let mut counted = [0u64; 2];
        for (z, lists) in b.lists.iter().enumerate() {
            for (o, &first) in lists.iter().enumerate() {
                let o = o as u32;
                let (mut f, mut prev) = (first, NONE);
                while f != NONE {
                    let blk = block(f);
                    assert_eq!(blk.magic, BLOCK_MAGIC, "free block {f:#x} overwritten");
                    assert_eq!(blk.order, o as u64);
                    assert_eq!(blk.prev, prev, "list links");
                    assert_eq!(f % (1 << o), 0, "block alignment");
                    assert_eq!(zone_of(f), z, "block zone");
                    assert!(b.is_head(f));
                    for g in f..f + (1 << o) {
                        let bit = used[(g / 64) as usize].load(Ordering::Relaxed) & (1 << (g % 64));
                        assert_eq!(bit, 0, "free frame {g:#x} marked in use");
                    }
                    if o < MAX_ORDER {
                        assert_ne!(b.order_at(f ^ (1 << o)), Some(o), "unmerged buddies at {f:#x}");
                    }
                    counted[z] += 1 << o;
                    prev = f;
                    f = blk.next;
                }
            }
        }
        assert_eq!(counted, b.free, "free counts");
    })
}
