//! Kernel heap: a contiguous run of physical frames accessed through the
//! physical-memory window, managed by `linked_list_allocator`.

use super::{frame, phys_to_virt, PAGE_SIZE};
use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, Ordering};
use linked_list_allocator::Heap;
use x86_64::instructions::interrupts::without_interrupts;

struct KernelAllocator {
    heap: spin::Mutex<Heap>,
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        without_interrupts(|| self.heap.lock().allocate_first_fit(layout).map_or(core::ptr::null_mut(), |p| p.as_ptr()))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        without_interrupts(|| self.heap.lock().deallocate(core::ptr::NonNull::new_unchecked(ptr), layout))
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator { heap: spin::Mutex::new(Heap::empty()) };

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
    let used = without_interrupts(|| ALLOCATOR.heap.lock().used());
    (HEAP_SIZE.load(Ordering::Relaxed), used as u64)
}
