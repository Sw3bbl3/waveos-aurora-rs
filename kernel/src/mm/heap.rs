//! Kernel heap: size-class slabs with per-CPU caches, on frames taken from
//! the frame allocator as it grows.
//!
//! - **Small** allocations (up to 32 KiB, nearly all of them) round up to
//!   one of 40 size classes, four per power of two (16, 32, 48, ... 1024,
//!   1280, 1536, 1792, 2048, ... 32 KiB). Each CPU keeps a short stack of
//!   free objects per class, so an allocation or a free is a push or a pop
//!   under that CPU's own lock with interrupts off for a few instructions.
//!   When a stack runs dry or overflows, a batch moves to or from the class's
//!   slabs under the class lock.
//! - **Slabs** are naturally aligned runs of 16 to 128 frames from the buddy
//!   allocator, used through the physical window (no mappings), with a
//!   header at the start. Partially used slabs are filled first; a slab that
//!   empties goes back to the frame allocator, but for one kept per class.
//!   When frames run out, [`reclaim`] empties the CPU stacks and gives back
//!   every idle slab.
//! - **Mid-sized** allocations (above 32 KiB, or aligned beyond a page) are
//!   runs of frames straight from the buddy allocator.
//! - **Large** ones (a megabyte or more, once the kernel virtual area
//!   exists: screen-sized buffers) get frames of their own, mapped
//!   contiguously into the kernel virtual area.
//!
//! So everything below a megabyte is physically contiguous, as it was when
//! the heap was one fixed region (some drivers hand heap buffers to devices).
//! The heap has no fixed size: it grows as long as there are frames.
//!
//! With the `heap-debug` feature, every small object carries a red zone
//! after it, checked when it is freed (overflows), and free objects are
//! filled with a poison pattern, checked when they are handed out again
//! (writes after free).

use super::{frame, note_irq_off, phys_to_virt, tsc, PAGE_SIZE};
use crate::arch::percpu::{cpu_id, MAX_CPUS};
use crate::sync::IrqMutex;
use bootinfo::PHYS_OFFSET;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::null_mut;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::arch::interrupts;

const PAGE: usize = PAGE_SIZE as usize;
const NCLASSES: usize = 40;
const SIZES: [usize; NCLASSES] = [
    16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792,
    2048, 2560, 3072, 3584, 4096, 5120, 6144, 7168, 8192, 10240, 12288, 14336, 16384, 20480, 24576, 28672, 32768,
];
/// The largest small allocation.
pub const MAX_SMALL: usize = SIZES[NCLASSES - 1];
/// Room for the slab header at the start of every slab.
const HEADER: usize = 64;
/// Empty slabs a class keeps rather than giving back at once.
const KEEP_EMPTY: u32 = 1;
/// Allocations of this size or more get frames of their own.
const LARGE: usize = 1 << 20;

#[cfg(feature = "heap-debug")]
const REDZONE: usize = 16;
#[cfg(not(feature = "heap-debug"))]
const REDZONE: usize = 0;

/// The alignment every object of a class has (its size's largest power-of-
/// two factor, up to a page).
const fn obj_align(size: usize) -> usize {
    let a = size.isolate_lowest_one();
    if a > PAGE {
        PAGE
    } else {
        a
    }
}

/// Where a class's first object starts in its slab (after the header).
const fn first_offset(size: usize) -> usize {
    HEADER.next_multiple_of(obj_align(size))
}

/// Slab size (as a frame order): at least 64 KiB and eight objects.
const fn slab_order(size: usize) -> u32 {
    let mut o = 4;
    while ((PAGE << o) - first_offset(size)) / size < 8 {
        o += 1;
    }
    o
}

/// Objects moved between a CPU's stack and the slabs at a time; a CPU's
/// stack for the class holds twice that.
const fn batch(size: usize) -> usize {
    if size <= 1024 {
        16
    } else if size <= 4096 {
        8
    } else {
        2
    }
}
const MAX_BATCH: usize = 16;

struct ClassInfo {
    order: u32,
    first: usize,
    objects: u32,
    batch: usize,
    /// Where this class's stack starts in a CPU's slot array.
    slots: usize,
}

const fn class_info() -> [ClassInfo; NCLASSES] {
    let mut r = [const { ClassInfo { order: 0, first: 0, objects: 0, batch: 0, slots: 0 } }; NCLASSES];
    let (mut c, mut slots) = (0, 0);
    while c < NCLASSES {
        let size = SIZES[c];
        let order = slab_order(size);
        r[c] = ClassInfo {
            order,
            first: first_offset(size),
            objects: (((PAGE << order) - first_offset(size)) / size) as u32,
            batch: batch(size),
            slots,
        };
        slots += 2 * batch(size);
        c += 1;
    }
    r
}
const CLASS: [ClassInfo; NCLASSES] = class_info();
const SLOTS: usize = CLASS[NCLASSES - 1].slots + 2 * CLASS[NCLASSES - 1].batch;

/// The smallest class at least `size` bytes (1 ..= `MAX_SMALL`).
fn class_index(size: usize) -> usize {
    if size <= 128 {
        return size.saturating_sub(1) / 16;
    }
    // Four classes per power of two above 128.
    let p = usize::BITS - 1 - (size - 1).leading_zeros();
    let step = 1 << (p - 2);
    let within = (size - (1 << p)).div_ceil(step) - 1;
    8 + (p as usize - 7) * 4 + within
}

/// The class for `layout`, if it is small: big enough (with the red zone)
/// and aligned enough.
fn class_of(layout: Layout) -> Option<usize> {
    let size = layout.size() + REDZONE;
    if size > MAX_SMALL || layout.align() > PAGE {
        return None;
    }
    let mut c = class_index(size);
    while obj_align(SIZES[c]) < layout.align() {
        c += 1;
    }
    Some(c)
}

// --- Slabs -------------------------------------------------------------

#[repr(C)]
struct Slab {
    magic: u32,
    class: u8,
    state: u8,
    inuse: u32,
    /// Objects never handed out start at this index.
    bump: u32,
    /// Free objects, linked through their first word (0 ends it).
    free: usize,
    /// The class's partial (or empty) list.
    next: *mut Slab,
    prev: *mut Slab,
}
const SLAB_MAGIC: u32 = 0x534c_4142; // "SLAB"
const FULL: u8 = 0;
const PARTIAL: u8 = 1;
const EMPTY: u8 = 2;

fn slab_bytes(c: usize) -> usize {
    PAGE << CLASS[c].order
}

fn slab_of(obj: usize, c: usize) -> *mut Slab {
    (obj & !(slab_bytes(c) - 1)) as *mut Slab
}

/// A class's slabs with free objects: `partial` (some in use) is doubly
/// linked; `empty` (none in use) is a stack.
struct Lists {
    partial: *mut Slab,
    empty: *mut Slab,
    empties: u32,
    slabs: u64,
}
unsafe impl Send for Lists {}

static LISTS: [IrqMutex<Lists>; NCLASSES] =
    [const { IrqMutex::new(Lists { partial: null_mut(), empty: null_mut(), empties: 0, slabs: 0 }) }; NCLASSES];

impl Lists {
    unsafe fn push_partial(&mut self, s: *mut Slab) {
        (*s).state = PARTIAL;
        (*s).prev = null_mut();
        (*s).next = self.partial;
        if !self.partial.is_null() {
            (*self.partial).prev = s;
        }
        self.partial = s;
    }

    unsafe fn unlink_partial(&mut self, s: *mut Slab) {
        if (*s).prev.is_null() {
            self.partial = (*s).next;
        } else {
            (*(*s).prev).next = (*s).next;
        }
        if !(*s).next.is_null() {
            (*(*s).next).prev = (*s).prev;
        }
    }

    /// Takes up to `out.len()` objects; returns how many it got.
    unsafe fn take(&mut self, c: usize, out: &mut [usize]) -> usize {
        let info = &CLASS[c];
        let mut got = 0;
        while got < out.len() {
            let s = if !self.partial.is_null() {
                self.partial
            } else if !self.empty.is_null() {
                let s = self.empty;
                self.empty = (*s).next;
                self.empties -= 1;
                self.push_partial(s);
                s
            } else {
                break;
            };
            while got < out.len() {
                let obj = if (*s).free != 0 {
                    let o = (*s).free;
                    let next = *(o as *const usize);
                    debug::check_link(s as usize, c, next);
                    (*s).free = next;
                    debug::reused(o);
                    o
                } else if (*s).bump < info.objects {
                    let o = s as usize + info.first + (*s).bump as usize * SIZES[c];
                    (*s).bump += 1;
                    debug::fresh(o, SIZES[c]);
                    o
                } else {
                    break;
                };
                (*s).inuse += 1;
                out[got] = obj;
                got += 1;
            }
            if (*s).inuse == info.objects {
                self.unlink_partial(s);
                (*s).state = FULL;
            }
        }
        got
    }

    /// Returns an object to its slab. A slab that empties beyond the ones
    /// kept is added to `release` (linked through `next`).
    unsafe fn put(&mut self, c: usize, obj: usize, release: &mut *mut Slab) {
        let s = slab_of(obj, c);
        assert!(
            (*s).magic == SLAB_MAGIC && (*s).class as usize == c && (*s).inuse > 0,
            "heap: freeing {obj:#x}, which is not an allocated {}-byte object",
            SIZES[c]
        );
        *(obj as *mut usize) = (*s).free;
        (*s).free = obj;
        (*s).inuse -= 1;
        if (*s).state == FULL {
            self.push_partial(s);
        }
        if (*s).inuse == 0 {
            self.unlink_partial(s);
            (*s).state = EMPTY;
            if self.empties < KEEP_EMPTY {
                (*s).next = self.empty;
                self.empty = s;
                self.empties += 1;
            } else {
                self.slabs -= 1;
                (*s).magic = 0;
                (*s).next = *release;
                *release = s;
            }
        }
    }
}

/// Frames [`reclaim`] has given back.
static RECLAIMED: AtomicU64 = AtomicU64::new(0);

/// Gives slabs back to the frame allocator.
unsafe fn release_slabs(c: usize, mut s: *mut Slab) {
    while !s.is_null() {
        let next = (*s).next;
        frame::free_run(s as u64 - PHYS_OFFSET, 1 << CLASS[c].order);
        s = next;
    }
}

/// Runs `f` on class `c`'s lists with interrupts off (timed).
fn with_lists<R>(c: usize, f: impl FnOnce(&mut Lists) -> R) -> R {
    interrupts::without_interrupts(|| {
        let t = tsc();
        let r = f(&mut LISTS[c].lock());
        note_irq_off(t);
        r
    })
}

/// Fills `out` with objects of class `c` (from the slabs, or a new slab);
/// returns how many it got (0 when memory is out).
unsafe fn refill(c: usize, out: &mut [usize]) -> usize {
    let got = with_lists(c, |l| l.take(c, out));
    if got > 0 {
        return got;
    }
    // A new slab, taken with no lock held (the frame allocator may call
    // `reclaim` when memory is short).
    let order = CLASS[c].order;
    let Some(phys) = frame::alloc_run(1 << order, order, u64::MAX) else { return 0 };
    let s = phys_to_virt(phys) as *mut Slab;
    s.write(Slab {
        magic: SLAB_MAGIC,
        class: c as u8,
        state: PARTIAL,
        inuse: 0,
        bump: 0,
        free: 0,
        next: null_mut(),
        prev: null_mut(),
    });
    with_lists(c, |l| {
        l.slabs += 1;
        l.push_partial(s);
        l.take(c, out)
    })
}

/// Returns objects to their slabs.
unsafe fn flush(c: usize, objs: &[usize]) {
    let mut release = null_mut();
    with_lists(c, |l| {
        for &o in objs {
            l.put(c, o, &mut release);
        }
    });
    release_slabs(c, release);
}

// --- Per-CPU stacks -----------------------------------------------------

struct Stacks {
    len: [u8; NCLASSES],
    slots: [usize; SLOTS],
}

#[repr(align(64))]
struct CpuCache {
    stacks: spin::Mutex<Stacks>,
    /// Counters written only under `stacks` by the CPU that owns it.
    allocs: [AtomicU64; NCLASSES],
    frees: [AtomicU64; NCLASSES],
    hits: AtomicU64,
    misses: AtomicU64,
}

static CPUS: [CpuCache; MAX_CPUS] = [const {
    CpuCache {
        stacks: spin::Mutex::new(Stacks { len: [0; NCLASSES], slots: [0; SLOTS] }),
        allocs: [const { AtomicU64::new(0) }; NCLASSES],
        frees: [const { AtomicU64::new(0) }; NCLASSES],
        hits: AtomicU64::new(0),
        misses: AtomicU64::new(0),
    }
}; MAX_CPUS];

fn bump(c: &AtomicU64) {
    c.store(c.load(Ordering::Relaxed).wrapping_add(1), Ordering::Relaxed);
}

/// Runs `f` on this CPU's stacks with interrupts off (so the task stays on
/// this CPU and no handler here can interrupt it). This fast path is timed
/// only in self-test builds; the slow paths always are.
#[inline]
fn with_cpu<R>(f: impl FnOnce(&CpuCache, &mut Stacks) -> R) -> R {
    let enabled = interrupts::are_enabled();
    interrupts::disable();
    #[cfg(feature = "ktest")]
    let t = tsc();
    let cpu = &CPUS[cpu_id().min(MAX_CPUS - 1)];
    let r = f(cpu, &mut cpu.stacks.lock());
    #[cfg(feature = "ktest")]
    note_irq_off(t);
    if enabled {
        interrupts::enable();
    }
    r
}

unsafe fn alloc_small(c: usize, size: usize) -> *mut u8 {
    let base = CLASS[c].slots;
    let mut p = with_cpu(|cpu, s| {
        let n = s.len[c] as usize;
        if n == 0 {
            return 0;
        }
        s.len[c] -= 1;
        bump(&cpu.allocs[c]);
        bump(&cpu.hits);
        s.slots[base + n - 1]
    });
    if p == 0 {
        p = refill_and_take(c);
        if p == 0 {
            return null_mut();
        }
    }
    debug::on_alloc(p, SIZES[c], size);
    p as *mut u8
}

#[cold]
unsafe fn refill_and_take(c: usize) -> usize {
    let mut batch = [0usize; MAX_BATCH];
    let got = refill(c, &mut batch[..CLASS[c].batch]);
    if got == 0 {
        return 0;
    }
    let (p, rest) = (batch[got - 1], &batch[..got - 1]);
    let base = CLASS[c].slots;
    let stored = with_cpu(|cpu, s| {
        bump(&cpu.allocs[c]);
        bump(&cpu.misses);
        let mut i = 0;
        while i < rest.len() && (s.len[c] as usize) < 2 * CLASS[c].batch {
            s.slots[base + s.len[c] as usize] = rest[i];
            s.len[c] += 1;
            i += 1;
        }
        i
    });
    // (A handler on this CPU may have filled the stack meanwhile.)
    if stored < rest.len() {
        flush(c, &rest[stored..]);
    }
    p
}

unsafe fn free_small(p: usize, c: usize, size: usize) {
    debug::on_free(p, SIZES[c], size);
    let base = CLASS[c].slots;
    let mut out = [0usize; MAX_BATCH];
    let spilled = with_cpu(|cpu, s| {
        bump(&cpu.frees[c]);
        let mut k = 0;
        if s.len[c] as usize == 2 * CLASS[c].batch {
            k = CLASS[c].batch;
            let from = base + s.len[c] as usize - k;
            out[..k].copy_from_slice(&s.slots[from..from + k]);
            s.len[c] -= k as u8;
        }
        s.slots[base + s.len[c] as usize] = p;
        s.len[c] += 1;
        k
    });
    if spilled > 0 {
        flush(c, &out[..spilled]);
    }
}

/// Memory is short: empties every CPU's stacks into the slabs and gives
/// every empty slab back to the frame allocator. Never waits for a lock
/// (it runs from inside the frame allocator, on any path): anything busy
/// right now is skipped.
pub fn reclaim() {
    for cpu in CPUS.iter() {
        for c in 0..NCLASSES {
            let mut out = [0usize; 2 * MAX_BATCH];
            let n = interrupts::without_interrupts(|| {
                let Some(mut s) = cpu.stacks.try_lock() else { return 0 };
                let n = s.len[c] as usize;
                let base = CLASS[c].slots;
                out[..n].copy_from_slice(&s.slots[base..base + n]);
                s.len[c] = 0;
                n
            });
            if n > 0 {
                unsafe { flush(c, &out[..n]) };
            }
        }
    }
    for c in 0..NCLASSES {
        let idle = interrupts::without_interrupts(|| {
            let Some(mut l) = LISTS[c].try_lock() else { return null_mut() };
            let s = core::mem::replace(&mut l.empty, null_mut());
            l.slabs -= l.empties as u64;
            l.empties = 0;
            s
        });
        let mut s = idle;
        while !s.is_null() {
            RECLAIMED.fetch_add(1 << CLASS[c].order, Ordering::Relaxed);
            s = unsafe { (*s).next };
        }
        unsafe { release_slabs(c, idle) };
    }
}

/// Frames given back under memory pressure so far.
pub fn reclaimed_frames() -> u64 {
    RECLAIMED.load(Ordering::Relaxed)
}

// --- Pages --------------------------------------------------------------

static PAGE_BYTES: AtomicU64 = AtomicU64::new(0);
static LARGE_BYTES: AtomicU64 = AtomicU64::new(0);

fn pages(size: usize) -> u64 {
    (size as u64).div_ceil(PAGE_SIZE).max(1)
}

unsafe fn alloc_pages(layout: Layout) -> *mut u8 {
    let n = pages(layout.size());
    let align_order = (layout.align() / PAGE).max(1).trailing_zeros();
    match frame::alloc_run(n, align_order, u64::MAX) {
        Some(phys) => {
            PAGE_BYTES.fetch_add(n * PAGE_SIZE, Ordering::Relaxed);
            phys_to_virt(phys) as *mut u8
        }
        None => null_mut(),
    }
}

unsafe fn free_pages(p: *mut u8, size: usize) {
    let n = pages(size);
    PAGE_BYTES.fetch_sub(n * PAGE_SIZE, Ordering::Relaxed);
    frame::free_run(p as u64 - PHYS_OFFSET, n);
}

// --- The allocator ------------------------------------------------------

struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = if let Some(c) = class_of(layout) {
            alloc_small(c, layout.size())
        } else if let Some(p) =
            (layout.size() >= LARGE && layout.align() <= PAGE && LARGE_READY.load(Ordering::Acquire))
                .then(|| large::alloc(layout.size()))
                .flatten()
        {
            p
        } else {
            alloc_pages(layout)
        };
        if p.is_null() {
            super::on_heap_failure(layout.size());
        }
        p
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if let Some(c) = class_of(layout) {
            return free_small(ptr as usize, c, layout.size());
        }
        if large::owns(ptr) {
            return large::free(ptr, layout.size());
        }
        free_pages(ptr, layout.size())
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = Layout::from_size_align_unchecked(new_size, layout.align());
        match (class_of(layout), class_of(new)) {
            // Growing or shrinking within a class keeps the block.
            (Some(a), Some(b)) if a == b => {
                debug::resize(ptr as usize, SIZES[a], layout.size(), new_size);
                return ptr;
            }
            // A run of frames shrinks in place, or keeps its page count.
            (None, None) if !large::owns(ptr) && new_size < LARGE => {
                let (old, want) = (pages(layout.size()), pages(new_size));
                if want == old {
                    return ptr;
                }
                if want < old {
                    PAGE_BYTES.fetch_sub((old - want) * PAGE_SIZE, Ordering::Relaxed);
                    frame::free_run(ptr as u64 - PHYS_OFFSET + want * PAGE_SIZE, old - want);
                    return ptr;
                }
            }
            _ => {}
        }
        let p = self.alloc(new);
        if !p.is_null() {
            core::ptr::copy_nonoverlapping(ptr, p, layout.size().min(new_size));
            self.dealloc(ptr, layout);
        }
        p
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator;

/// The kernel virtual area is ready (set after `vmm::init`).
static LARGE_READY: AtomicBool = AtomicBool::new(false);

/// Lets large allocations use the kernel virtual area.
pub fn enable_large() {
    LARGE_READY.store(true, Ordering::Release);
}

mod large {
    use super::super::{frame, vmm, PAGE_SIZE};
    use super::LARGE_BYTES;
    use alloc::vec::Vec;
    use core::sync::atomic::Ordering;

    /// Ranges freed with interrupts off, unmapped at the next chance (an
    /// unmap waits for the other CPUs, which needs interrupts on).
    static DEFERRED: spin::Mutex<[(u64, u64); 32]> = spin::Mutex::new([(0, 0); 32]);

    pub fn owns(p: *mut u8) -> bool {
        (vmm::KVA_BASE..vmm::KVA_BASE + vmm::KVA_SIZE).contains(&(p as u64))
    }

    pub fn alloc(size: usize) -> Option<*mut u8> {
        release_deferred();
        let pages = (size as u64).div_ceil(PAGE_SIZE);
        let mut frames: Vec<u64> = Vec::new();
        frames.try_reserve_exact(pages as usize).ok()?;
        for _ in 0..pages {
            match frame::alloc() {
                Some(f) => frames.push(f),
                None => {
                    frames.iter().for_each(|&f| frame::free(f));
                    return None;
                }
            }
        }
        match vmm::kmap(&frames) {
            Some(va) => {
                LARGE_BYTES.fetch_add(pages * PAGE_SIZE, Ordering::Relaxed);
                Some(va as *mut u8)
            }
            None => {
                frames.iter().for_each(|&f| frame::free(f));
                None
            }
        }
    }

    pub fn free(p: *mut u8, size: usize) {
        let (base, pages) = (p as u64, (size as u64).div_ceil(PAGE_SIZE));
        LARGE_BYTES.fetch_sub(pages * PAGE_SIZE, Ordering::Relaxed);
        if !crate::arch::interrupts::are_enabled() {
            let mut d = DEFERRED.lock();
            if let Some(slot) = d.iter_mut().find(|s| s.1 == 0) {
                *slot = (base, pages);
            }
            // (With every slot taken the range leaks rather than deadlocks.)
            return;
        }
        unmap(base, pages);
        release_deferred();
    }

    fn release_deferred() {
        if !crate::arch::interrupts::are_enabled() {
            return;
        }
        loop {
            let next = {
                let mut d = DEFERRED.lock();
                d.iter_mut().find(|s| s.1 != 0).map(core::mem::take)
            };
            match next {
                Some((base, pages)) => unmap(base, pages),
                None => break,
            }
        }
    }

    fn unmap(base: u64, pages: u64) {
        for i in 0..pages {
            if let Some(phys) = vmm::translate(base + i * PAGE_SIZE) {
                frame::free(phys & !(PAGE_SIZE - 1));
            }
        }
        vmm::kunmap(base, pages);
    }
}

pub fn init() {
    log!(
        "mm",
        "kernel heap: {} size classes up to {} KiB on demand, per-CPU caches{}",
        NCLASSES,
        MAX_SMALL >> 10,
        if cfg!(feature = "heap-debug") { ", red zones and poisoning" } else { "" }
    );
}

// --- Statistics ---------------------------------------------------------

/// One size class: object size, objects in use, slabs, bytes in slabs.
pub struct ClassStats {
    pub size: usize,
    pub live: u64,
    pub slabs: u64,
    pub slab_bytes: u64,
}

pub fn class_stats(c: usize) -> ClassStats {
    class_stats_with(c, LISTS[c].lock().slabs)
}

/// [`class_stats`] without waiting for the class's lock (no slabs if busy).
pub fn class_stats_nowait(c: usize) -> ClassStats {
    class_stats_with(c, LISTS[c].try_lock().map_or(0, |l| l.slabs))
}

/// (bytes in runs of frames for mid-sized allocations, bytes of large mapped ones)
pub fn run_bytes() -> (u64, u64) {
    (PAGE_BYTES.load(Ordering::Relaxed), LARGE_BYTES.load(Ordering::Relaxed))
}

/// [`usage`] without waiting for locks.
pub fn usage_nowait() -> (u64, u64) {
    usage_with(class_stats_nowait)
}

fn class_stats_with(c: usize, slabs: u64) -> ClassStats {
    let allocs = CPUS.iter().fold(0u64, |a, cpu| a.wrapping_add(cpu.allocs[c].load(Ordering::Relaxed)));
    let frees = CPUS.iter().fold(0u64, |a, cpu| a.wrapping_add(cpu.frees[c].load(Ordering::Relaxed)));
    ClassStats {
        size: SIZES[c],
        live: (allocs.wrapping_sub(frees) as i64).max(0) as u64,
        slabs,
        slab_bytes: slabs * slab_bytes(c) as u64,
    }
}

pub const CLASSES: usize = NCLASSES;

/// (small allocations served from a CPU's stack, ones that had to refill it)
pub fn cache_counts() -> (u64, u64) {
    CPUS.iter()
        .fold((0, 0), |(h, m), cpu| (h + cpu.hits.load(Ordering::Relaxed), m + cpu.misses.load(Ordering::Relaxed)))
}

/// Frames in slabs and in runs for mid-sized allocations (not the large,
/// mapped ones).
#[cfg(feature = "ktest")]
pub fn held_frames() -> u64 {
    let slabs: u64 = (0..NCLASSES).map(|c| class_stats(c).slab_bytes).sum();
    (slabs + PAGE_BYTES.load(Ordering::Relaxed)) / PAGE_SIZE
}

/// (bytes the heap holds, bytes handed out)
pub fn usage() -> (u64, u64) {
    usage_with(class_stats)
}

fn usage_with(stats: fn(usize) -> ClassStats) -> (u64, u64) {
    let (mut size, mut used) = (0, 0);
    for c in 0..NCLASSES {
        let s = stats(c);
        size += s.slab_bytes;
        used += s.live * s.size as u64;
    }
    let other = PAGE_BYTES.load(Ordering::Relaxed) + LARGE_BYTES.load(Ordering::Relaxed);
    (size + other, used + other)
}

// --- Debug checks -------------------------------------------------------

#[cfg(feature = "heap-debug")]
mod debug {
    const POISON: u8 = 0x6b;
    const RED: u8 = 0xcc;

    unsafe fn bytes<'a>(p: usize, from: usize, to: usize) -> &'a mut [u8] {
        core::slice::from_raw_parts_mut((p + from) as *mut u8, to - from)
    }

    /// A free-list link must point into the same slab (or end the list).
    pub unsafe fn check_link(slab: usize, c: usize, next: usize) {
        let end = slab + super::slab_bytes(c);
        assert!(
            next == 0 || (slab..end).contains(&next),
            "heap: free list of a {}-byte slab corrupted",
            super::SIZES[c]
        );
    }
    /// An object leaving a slab's free list: its link word becomes poison again.
    pub unsafe fn reused(p: usize) {
        bytes(p, 0, 8).fill(POISON);
    }
    pub unsafe fn fresh(p: usize, class: usize) {
        bytes(p, 0, class).fill(POISON);
    }
    pub unsafe fn on_alloc(p: usize, class: usize, size: usize) {
        if let Some(i) = bytes(p, 0, class).iter().position(|&b| b != POISON) {
            panic!("heap: {p:#x} ({class}-byte class) was written after being freed (byte {i})");
        }
        bytes(p, size, class).fill(RED);
    }
    pub unsafe fn on_free(p: usize, class: usize, size: usize) {
        if let Some(i) = bytes(p, size, class).iter().position(|&b| b != RED) {
            panic!("heap: write past the end of {p:#x} ({size} bytes; {} bytes past)", i + 1);
        }
        bytes(p, 0, class).fill(POISON);
    }
    pub unsafe fn resize(p: usize, class: usize, old: usize, new: usize) {
        if let Some(i) = bytes(p, old, class).iter().position(|&b| b != RED) {
            panic!("heap: write past the end of {p:#x} ({old} bytes; {} bytes past)", i + 1);
        }
        bytes(p, new, class).fill(RED);
    }
}

#[cfg(not(feature = "heap-debug"))]
mod debug {
    #[inline(always)]
    pub unsafe fn check_link(_: usize, _: usize, _: usize) {}
    #[inline(always)]
    pub unsafe fn reused(_: usize) {}
    #[inline(always)]
    pub unsafe fn fresh(_: usize, _: usize) {}
    #[inline(always)]
    pub unsafe fn on_alloc(_: usize, _: usize, _: usize) {}
    #[inline(always)]
    pub unsafe fn on_free(_: usize, _: usize, _: usize) {}
    #[inline(always)]
    pub unsafe fn resize(_: usize, _: usize, _: usize, _: usize) {}
}

/// Self-test helpers.
#[cfg(feature = "ktest")]
pub mod testing {
    use super::*;

    /// Every size maps to the smallest class that fits it, aligned enough.
    pub fn check_classes() {
        for size in 1..=MAX_SMALL - REDZONE {
            for align in [1, 8, 16, 64, 256, 4096] {
                let l = Layout::from_size_align(size, align).unwrap();
                let c = class_of(l).unwrap();
                assert!(SIZES[c] >= size + REDZONE, "class for {size}");
                assert_eq!(SIZES[c] % align.min(PAGE), 0, "alignment of class for {size}/{align}");
                if align <= 16 && c > 0 {
                    assert!(SIZES[c - 1] < size + REDZONE, "class for {size} not the smallest");
                }
            }
        }
        for (c, info) in CLASS.iter().enumerate() {
            assert!(info.objects >= 8, "class {c} slab too small");
            assert!(info.first >= HEADER && info.first % obj_align(SIZES[c]) == 0);
            assert!(info.first + info.objects as usize * SIZES[c] <= slab_bytes(c));
            assert!(info.batch <= MAX_BATCH);
        }
    }
}
