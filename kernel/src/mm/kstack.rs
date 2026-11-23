//! Kernel task stacks with guard pages.
//!
//! Each stack sits at the top of its own 256 KiB slot in a kernel area of
//! its own; the 128 KiB below it stay unmapped. A task overflowing its
//! stack therefore faults on a guard page — never writes into another
//! task's stack or the heap — and the page fault, unable to push its frame,
//! becomes a double fault on its own IST stack, which reports it.
//!
//! Stacks of exited tasks go back to a cache with their frames still mapped,
//! because they are freed right after a context switch (interrupts off,
//! no TLB shootdown possible there). [`trim`] gives the frames of surplus
//! cached stacks back from a context that can wait for other CPUs.
//!
//! If frames run out, a stack falls back to the kernel heap (no guard), as
//! before guarded stacks: spawning a task never fails on that account.

use super::{frame, vmm, PAGE_SIZE};
use crate::sync::IrqMutex;
use alloc::boxed::Box;
use alloc::vec::Vec;

pub const SIZE: u64 = 128 * 1024;
const SLOT: u64 = 256 * 1024;
/// One PML4 slot (512 GiB) of the kernel half, no-execute like all but the
/// image's (see `arch::protect`).
pub const BASE: u64 = 0xFFFF_D000_0000_0000;
const END: u64 = BASE + (512 << 30);
/// Stacks kept mapped for reuse after [`trim`].
const KEEP: usize = 32;

struct Pool {
    /// Next never-used slot.
    next: u64,
    /// Slots whose stack is mapped and unused.
    free: Vec<u64>,
    /// Slots given back by [`trim`]: unmapped everywhere, reusable.
    empty: Vec<u64>,
}

static POOL: IrqMutex<Pool> = IrqMutex::new(Pool { next: BASE, free: Vec::new(), empty: Vec::new() });

pub enum KStack {
    /// The slot at this base; the stack is its top [`SIZE`] bytes.
    Guarded(u64),
    Heap(Box<[u8]>),
}

impl KStack {
    pub fn new() -> KStack {
        Self::guarded().unwrap_or_else(|| KStack::Heap(alloc::vec![0u8; SIZE as usize].into_boxed_slice()))
    }

    fn guarded() -> Option<KStack> {
        let mut pool = POOL.lock();
        if let Some(slot) = pool.free.pop() {
            return Some(KStack::Guarded(slot));
        }
        let slot = match pool.empty.pop() {
            Some(s) => s,
            None if pool.next + SLOT <= END => {
                pool.next += SLOT;
                pool.next - SLOT
            }
            None => return None,
        };
        let bottom = slot + SLOT - SIZE;
        let mut mapped = Vec::with_capacity((SIZE / PAGE_SIZE) as usize);
        for i in 0..SIZE / PAGE_SIZE {
            let f = frame::alloc().filter(|&f| vmm::kmap_fixed(bottom + i * PAGE_SIZE, f));
            match f {
                Some(f) => mapped.push(f),
                None => {
                    // Out of memory: undo (these pages were never used, and the
                    // slot is not handed out, so no other CPU has them cached).
                    for (j, &f) in mapped.iter().enumerate() {
                        vmm::kunmap_fixed(bottom + j as u64 * PAGE_SIZE);
                        frame::free(f);
                    }
                    pool.empty.push(slot);
                    return None;
                }
            }
        }
        Some(KStack::Guarded(slot))
    }

    pub fn bottom(&self) -> u64 {
        match self {
            KStack::Guarded(slot) => slot + SLOT - SIZE,
            KStack::Heap(b) => b.as_ptr() as u64,
        }
    }

    pub fn top(&self) -> u64 {
        match self {
            KStack::Guarded(slot) => slot + SLOT,
            KStack::Heap(b) => b.as_ptr() as u64 + b.len() as u64,
        }
    }
}

impl Drop for KStack {
    fn drop(&mut self) {
        if let KStack::Guarded(slot) = *self {
            POOL.lock().free.push(slot);
        }
    }
}

/// Frames held by cached (unused) stacks: not leaked, but not free either.
#[cfg(feature = "ktest")]
pub fn cached_frames() -> u64 {
    POOL.lock().free.len() as u64 * (SIZE / PAGE_SIZE)
}

/// Whether `addr` is in the guard area of a kernel stack.
pub fn is_guard(addr: u64) -> bool {
    (BASE..END).contains(&addr) && (addr - BASE) % SLOT < SLOT - SIZE
}

/// Gives back the frames of cached stacks beyond the ones kept for reuse.
/// Must be called with interrupts on and no spinlock held (it waits for a
/// TLB shootdown); the reaper does after each process.
pub fn trim() {
    loop {
        let slot = {
            let mut pool = POOL.lock();
            if pool.free.len() <= KEEP {
                return;
            }
            // The lowest slots first, so the area stays compact-ish.
            let (i, _) = pool.free.iter().enumerate().min_by_key(|(_, &s)| s).unwrap();
            pool.free.swap_remove(i)
        };
        let bottom = slot + SLOT - SIZE;
        let frames: Vec<u64> = (0..SIZE / PAGE_SIZE).filter_map(|i| vmm::kunmap_fixed(bottom + i * PAGE_SIZE)).collect();
        vmm::shootdown(bottom, SIZE / PAGE_SIZE, None);
        for f in frames {
            frame::free(f);
        }
        // No CPU can hold a translation for it any more: reusable.
        POOL.lock().empty.push(slot);
    }
}
