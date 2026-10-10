//! The process heap. Small blocks (up to 2 KiB, alignment up to 16) come
//! from per-size-class free lists refilled from 64 KiB slabs: O(1) to
//! allocate and free however fragmented the heap gets. Larger ones come
//! from a chain of first-fit arenas obtained from `mmap` on demand.

use crate::abi::nr;
use crate::sync::Mutex;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use linked_list_allocator::Heap;

const MAX_ARENAS: usize = 48;
const MIN_ARENA: usize = 1 << 20;

/// Small block sizes, all multiples of 16.
const CLASSES: [usize; 24] = [
    16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792,
    2048,
];
const SLAB: usize = 64 << 10;

/// The class for a small layout (None: use the arenas).
fn class_of(layout: &Layout) -> Option<usize> {
    if layout.align() > 16 || layout.size() > 2048 {
        return None;
    }
    let size = layout.size().max(1);
    Some(CLASSES.iter().position(|c| *c >= size).unwrap())
}

struct Arenas {
    heaps: [Heap; MAX_ARENAS],
    ranges: [(usize, usize); MAX_ARENAS],
    count: usize,
    /// Per class: freed blocks (each holds the next pointer), and the
    /// uncarved rest of its current slab (start, end).
    free: [usize; CLASSES.len()],
    carve: [(usize, usize); CLASSES.len()],
}

pub struct Allocator(Mutex<Arenas>);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator(Mutex::new(Arenas {
    heaps: [const { Heap::empty() }; MAX_ARENAS],
    ranges: [(0, 0); MAX_ARENAS],
    count: 0,
    free: [0; CLASSES.len()],
    carve: [(0, 0); CLASSES.len()],
}));

impl Arenas {
    fn grow(&mut self, need: usize) -> bool {
        if self.count == MAX_ARENAS {
            return false;
        }
        let size = (need * 2 + 4096).max(MIN_ARENA << self.count.min(4)).next_multiple_of(4096);
        let Ok(addr) = crate::sys::call(nr::MMAP, &[size as u64]) else { return false };
        unsafe { self.heaps[self.count].init(addr as *mut u8, size) };
        self.ranges[self.count] = (addr as usize, addr as usize + size);
        self.count += 1;
        true
    }
}

impl Arenas {
    unsafe fn large(&mut self, layout: Layout) -> *mut u8 {
        loop {
            for i in (0..self.count).rev() {
                if let Ok(p) = self.heaps[i].allocate_first_fit(layout) {
                    return p.as_ptr();
                }
            }
            if !self.grow(layout.size() + layout.align()) {
                return core::ptr::null_mut();
            }
        }
    }
}

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut a = self.0.lock();
        let Some(c) = class_of(&layout) else { return a.large(layout) };
        // A freed block of this size.
        let head = a.free[c];
        if head != 0 {
            a.free[c] = *(head as *const usize);
            return head as *mut u8;
        }
        // Else the next one from this class's slab (a new slab when it's used up).
        let size = CLASSES[c];
        if a.carve[c].1 - a.carve[c].0 < size {
            let slab = a.large(Layout::from_size_align_unchecked(SLAB, 16));
            if slab.is_null() {
                return slab;
            }
            a.carve[c] = (slab as usize, slab as usize + SLAB);
        }
        let p = a.carve[c].0;
        a.carve[c].0 += size;
        p as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut a = self.0.lock();
        if let Some(c) = class_of(&layout) {
            *(ptr as *mut usize) = a.free[c];
            a.free[c] = ptr as usize;
            return;
        }
        let p = ptr as usize;
        if let Some(i) = (0..a.count).find(|&i| p >= a.ranges[i].0 && p < a.ranges[i].1) {
            a.heaps[i].deallocate(NonNull::new_unchecked(ptr), layout);
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new = Layout::from_size_align_unchecked(new_size, layout.align());
        // Still the same block size: nothing to move.
        if class_of(&layout).is_some() && class_of(&layout) == class_of(&new) {
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
