//! The process heap: a chain of arenas obtained from `mmap` on demand.

use crate::abi::nr;
use crate::sync::Mutex;
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use linked_list_allocator::Heap;

const MAX_ARENAS: usize = 48;
const MIN_ARENA: usize = 1 << 20;

struct Arenas {
    heaps: [Heap; MAX_ARENAS],
    ranges: [(usize, usize); MAX_ARENAS],
    count: usize,
}

pub struct Allocator(Mutex<Arenas>);

#[global_allocator]
static ALLOCATOR: Allocator = Allocator(Mutex::new(Arenas {
    heaps: [const { Heap::empty() }; MAX_ARENAS],
    ranges: [(0, 0); MAX_ARENAS],
    count: 0,
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

unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut a = self.0.lock();
        loop {
            for i in (0..a.count).rev() {
                if let Ok(p) = a.heaps[i].allocate_first_fit(layout) {
                    return p.as_ptr();
                }
            }
            if !a.grow(layout.size() + layout.align()) {
                return core::ptr::null_mut();
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut a = self.0.lock();
        let p = ptr as usize;
        if let Some(i) = (0..a.count).find(|&i| p >= a.ranges[i].0 && p < a.ranges[i].1) {
            a.heaps[i].deallocate(NonNull::new_unchecked(ptr), layout);
        }
    }
}
