//! Bitmap physical frame allocator.
//!
//! One bit per 4 KiB frame covering every usable region the bootloader
//! reported. The bitmap itself is carved out of the first usable region large
//! enough to hold it.
//!
//! Memory below 4 GiB is kept for what needs a 32-bit address: DMA for
//! controllers that can't reach higher (USB 1.1 and some EHCI), the page
//! table CPUs start on, and contiguous device buffers, which come first-fit
//! from the bottom. Single frames (process memory, page tables, the large
//! heap) come from above 4 GiB first and only move down once that is used up.

use super::{phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use bootinfo::{MemoryKind, MemoryRegion};

struct FrameAllocator {
    bitmap: &'static mut [u64],
    frames: u64,
    total: u64,
    free: u64,
    next: u64,
    /// Where single-frame allocations start: the first frame above 4 GiB,
    /// or the bottom when there is no memory up there.
    first: u64,
}

static FRAMES: IrqMutex<Option<FrameAllocator>> = IrqMutex::new(None);

/// Frames below 1 MiB are kept back (legacy BIOS data, future SMP trampoline).
const LOW_RESERVED: u64 = 0x10_0000;
/// The first address a device or CPU with 32-bit addressing can't reach.
pub const LIMIT_32BIT: u64 = 1 << 32;

impl FrameAllocator {
    fn is_free(&self, frame: u64) -> bool {
        self.bitmap[(frame / 64) as usize] & (1 << (frame % 64)) == 0
    }
    fn set_used(&mut self, frame: u64) {
        self.bitmap[(frame / 64) as usize] |= 1 << (frame % 64);
    }
    fn set_free(&mut self, frame: u64) {
        self.bitmap[(frame / 64) as usize] &= !(1 << (frame % 64));
    }

    /// The first free frame in `[from, to)`, skipping full words.
    fn find_free(&self, from: u64, to: u64) -> Option<u64> {
        let mut f = from;
        while f < to {
            if f.is_multiple_of(64) && self.bitmap[(f / 64) as usize] == u64::MAX {
                f += 64;
            } else if self.is_free(f) {
                return Some(f);
            } else {
                f += 1;
            }
        }
        None
    }

    /// Next-fit from where the last one left off: above 4 GiB first, then
    /// (only once that is full) below.
    fn alloc(&mut self) -> Option<u64> {
        let f = self
            .find_free(self.next, self.frames)
            .or_else(|| self.find_free(self.first, self.next))
            .or_else(|| self.find_free(0, self.first))?;
        self.set_used(f);
        self.free -= 1;
        // Once in low memory, start over from the top next time: frames
        // freed up there are the better choice.
        self.next = if f + 1 >= self.first { f + 1 } else { self.first };
        Some(f * PAGE_SIZE)
    }

    /// First-fit from the bottom: `count` free frames in a row, ending below `limit`.
    fn alloc_contiguous(&mut self, count: u64, limit: u64) -> Option<u64> {
        let end = self.frames.min(limit / PAGE_SIZE);
        let mut start = 0;
        while let Some(f) = self.find_free(start, end) {
            if f + count > end {
                return None;
            }
            match (f..f + count).find(|&g| !self.is_free(g)) {
                Some(used) => start = used + 1,
                None => {
                    for g in f..f + count {
                        self.set_used(g);
                    }
                    self.free -= count;
                    return Some(f * PAGE_SIZE);
                }
            }
        }
        None
    }
}

pub fn init(map: &[MemoryRegion]) {
    let usable = |r: &&MemoryRegion| r.kind == MemoryKind::Usable;
    let top = map.iter().filter(usable).map(|r| r.end()).max().unwrap_or(0);
    let frames = top / PAGE_SIZE;
    let words = frames.div_ceil(64) as usize;
    let bitmap_bytes = (words * 8) as u64;

    let host = map
        .iter()
        .filter(usable)
        .find(|r| r.start >= LOW_RESERVED && r.len >= bitmap_bytes + PAGE_SIZE)
        .expect("no region large enough for the frame bitmap");
    let bitmap = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(host.start) as *mut u64, words) };
    bitmap.fill(u64::MAX);

    let mut fa = FrameAllocator { bitmap, frames, total: 0, free: 0, next: 0, first: 0 };
    for r in map.iter().filter(usable) {
        let first = r.start.div_ceil(PAGE_SIZE).max(LOW_RESERVED / PAGE_SIZE);
        let last = r.end() / PAGE_SIZE;
        for f in first..last {
            fa.set_free(f);
            fa.free += 1;
        }
    }
    let bitmap_frames = bitmap_bytes.div_ceil(PAGE_SIZE);
    for f in host.start / PAGE_SIZE..host.start / PAGE_SIZE + bitmap_frames {
        fa.set_used(f);
        fa.free -= 1;
    }
    fa.total = fa.free;
    let high = map.iter().filter(usable).map(|r| r.end().saturating_sub(r.start.max(LIMIT_32BIT))).sum::<u64>();
    fa.first = if high >= PAGE_SIZE { LIMIT_32BIT / PAGE_SIZE } else { 0 };
    fa.next = fa.first;
    log!(
        "mm",
        "{} MiB usable RAM ({} frames, {} MiB above 4 GiB), bitmap {} KiB",
        (fa.total * PAGE_SIZE) >> 20,
        fa.total,
        high >> 20,
        bitmap_bytes / 1024
    );
    *FRAMES.lock() = Some(fa);
}

/// Allocates one zeroed-on-demand-free frame; returns its physical address.
pub fn alloc() -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc()
}

/// Allocates one frame below `limit` (for structures a device or a CPU in a
/// 32-bit mode reaches with a short address); returns its physical address.
pub fn alloc_below(limit: u64) -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc_contiguous(1, limit)
}

/// Allocates `count` physically contiguous frames.
pub fn alloc_contiguous(count: u64) -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc_contiguous(count, u64::MAX)
}

/// Allocates `count` physically contiguous frames that end below `limit`
/// (for devices that address only the first 4 GiB).
pub fn alloc_contiguous_below(count: u64, limit: u64) -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc_contiguous(count, limit)
}

#[allow(dead_code)] // used by the self-tests; the GUI never frees frames yet
pub fn free(phys: u64) {
    if let Some(fa) = FRAMES.lock().as_mut() {
        let f = phys / PAGE_SIZE;
        if f < fa.frames && !fa.is_free(f) {
            fa.set_free(f);
            fa.free += 1;
        }
    }
}

/// The end of usable RAM (a kernel test checks memory up to it).
#[cfg(feature = "ktest")]
pub fn top() -> u64 {
    FRAMES.lock().as_ref().map_or(0, |f| f.frames * PAGE_SIZE)
}

/// (total usable frames, free frames)
pub fn counts() -> (u64, u64) {
    FRAMES.lock().as_ref().map(|f| (f.total, f.free)).unwrap_or((0, 0))
}
