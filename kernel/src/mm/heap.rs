//! Kernel heap: a contiguous run of physical frames accessed through the
//! physical-memory window.
//!
//! Small allocations (4 KiB or less, which is nearly all of them) come from
//! free lists of power-of-two size classes, each with its own lock: taking
//! or giving back a block is a pointer swap. A class gets more blocks by
//! cutting a 64 KiB slab from the rest of the heap, which
//! `linked_list_allocator` manages, and which also serves the allocations
//! too big for a class. (Before, every allocation walked that allocator's
//! list of holes, which grew long as the heap fragmented: a third of the
//! kernel's time while Firefox started.)
//!
//! Large allocations (a megabyte or more: screen-sized buffers, which are
//! 33 MB each at 4K) don't come from the heap: they get their own frames,
//! mapped contiguously into the kernel virtual area.

use super::{frame, phys_to_virt, PAGE_SIZE};
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};
use linked_list_allocator::Heap;
use x86_64::instructions::interrupts::without_interrupts;

struct KernelAllocator {
    heap: spin::Mutex<Heap>,
    /// Free blocks of each size class (16 bytes to 4 KiB), as a list linked
    /// through the blocks themselves; 0 ends it.
    classes: [spin::Mutex<usize>; CLASSES],
}

/// Size classes: 16 << 0 ..= 16 << 8 (4 KiB).
const CLASSES: usize = 9;
const MIN_CLASS: usize = 16;
/// What a class takes from the heap at a time.
const SLAB: usize = 64 << 10;
/// Bytes sitting in the classes' free lists (counted as free in [`usage`]).
static CLASS_FREE: AtomicU64 = AtomicU64::new(0);

/// The class for `layout` (big enough and aligned enough), if it is small.
fn class_of(layout: Layout) -> Option<usize> {
    let size = layout.size().max(layout.align()).max(MIN_CLASS).next_power_of_two();
    let class = (size / MIN_CLASS).trailing_zeros() as usize;
    (class < CLASSES).then_some(class)
}

impl KernelAllocator {
    fn class_alloc(&self, class: usize) -> *mut u8 {
        let size = MIN_CLASS << class;
        let mut head = self.classes[class].lock();
        if *head == 0 {
            // A new slab, aligned to its blocks' size, cut into blocks.
            let layout = Layout::from_size_align(SLAB, size.max(PAGE_SIZE as usize)).unwrap();
            let Ok(slab) = self.heap.lock().allocate_first_fit(layout) else { return core::ptr::null_mut() };
            let base = slab.as_ptr() as usize;
            for block in (base..base + SLAB).step_by(size).rev() {
                unsafe { *(block as *mut usize) = *head };
                *head = block;
            }
            CLASS_FREE.fetch_add(SLAB as u64, Ordering::Relaxed);
        }
        let block = *head;
        *head = unsafe { *(block as *const usize) };
        CLASS_FREE.fetch_sub(size as u64, Ordering::Relaxed);
        block as *mut u8
    }

    fn class_free(&self, class: usize, ptr: *mut u8) {
        let mut head = self.classes[class].lock();
        unsafe { *(ptr as *mut usize) = *head };
        *head = ptr as usize;
        CLASS_FREE.fetch_add((MIN_CLASS << class) as u64, Ordering::Relaxed);
    }
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= LARGE && layout.align() <= PAGE_SIZE as usize && LARGE_READY.load(Ordering::Acquire) {
            if let Some(p) = large::alloc(layout.size()) {
                return p;
            }
        }
        if let Some(class) = class_of(layout) {
            return without_interrupts(|| self.class_alloc(class));
        }
        without_interrupts(|| self.heap.lock().allocate_first_fit(layout).map_or(core::ptr::null_mut(), |p| p.as_ptr()))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if large::owns(ptr) {
            return large::free(ptr, layout.size());
        }
        if let Some(class) = class_of(layout) {
            return without_interrupts(|| self.class_free(class, ptr));
        }
        without_interrupts(|| self.heap.lock().deallocate(core::ptr::NonNull::new_unchecked(ptr), layout))
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Growing or shrinking within a class keeps the block.
        let new = Layout::from_size_align_unchecked(new_size, layout.align());
        if !large::owns(ptr) && class_of(layout).is_some() && class_of(layout) == class_of(new) {
            return ptr;
        }
        let p = self.alloc(new);
        if !p.is_null() {
            core::ptr::copy_nonoverlapping(ptr, p, layout.size().min(new_size));
            self.dealloc(ptr, layout);
        }
        p
    }
}

/// Allocations of this size or more get frames of their own.
const LARGE: usize = 1 << 20;
/// The kernel virtual area is ready (set after `vmm::init`).
static LARGE_READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Lets large allocations use the kernel virtual area.
pub fn enable_large() {
    LARGE_READY.store(true, Ordering::Release);
}

mod large {
    use super::super::{frame, vmm, PAGE_SIZE};
    use alloc::vec::Vec;

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
            Some(va) => Some(va as *mut u8),
            None => {
                frames.iter().for_each(|&f| frame::free(f));
                None
            }
        }
    }

    pub fn free(p: *mut u8, size: usize) {
        let (base, pages) = (p as u64, (size as u64).div_ceil(PAGE_SIZE));
        if !x86_64::instructions::interrupts::are_enabled() {
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
        if !x86_64::instructions::interrupts::are_enabled() {
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

#[global_allocator]
static ALLOCATOR: KernelAllocator =
    KernelAllocator { heap: spin::Mutex::new(Heap::empty()), classes: [const { spin::Mutex::new(0) }; CLASSES] };

static HEAP_SIZE: AtomicU64 = AtomicU64::new(0);

pub fn init() {
    let (total, _) = frame::counts();
    // A quarter of RAM, between 16 and 128 MiB.
    let bytes = (total * PAGE_SIZE / 4).clamp(16 << 20, 128 << 20);
    let phys = frame::alloc_contiguous(bytes / PAGE_SIZE).expect("no room for the kernel heap");
    unsafe { ALLOCATOR.heap.lock().init(phys_to_virt(phys) as *mut u8, bytes as usize) };
    HEAP_SIZE.store(bytes, Ordering::Relaxed);
    log!("mm", "kernel heap {} MiB at phys {:#x}", bytes >> 20, phys);
}

/// (heap size, bytes in use)
pub fn usage() -> (u64, u64) {
    let used = without_interrupts(|| ALLOCATOR.heap.lock().used()) as u64;
    (HEAP_SIZE.load(Ordering::Relaxed), used.saturating_sub(CLASS_FREE.load(Ordering::Relaxed)))
}
