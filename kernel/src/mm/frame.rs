//! Bitmap physical frame allocator.
//!
//! One bit per 4 KiB frame covering every usable region the bootloader
//! reported. The bitmap itself is carved out of the first usable region large
//! enough to hold it.

use super::{phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use bootinfo::{MemoryKind, MemoryRegion};

struct FrameAllocator {
    bitmap: &'static mut [u64],
    frames: u64,
    total: u64,
    free: u64,
    next: u64,
}

static FRAMES: IrqMutex<Option<FrameAllocator>> = IrqMutex::new(None);

/// Frames below 1 MiB are kept back (legacy BIOS data, future SMP trampoline).
const LOW_RESERVED: u64 = 0x10_0000;

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

    fn alloc(&mut self) -> Option<u64> {
        for i in 0..self.frames {
            let f = (self.next + i) % self.frames;
            if self.is_free(f) {
                self.set_used(f);
                self.free -= 1;
                self.next = f + 1;
                return Some(f * PAGE_SIZE);
            }
        }
        None
    }

    fn alloc_contiguous(&mut self, count: u64) -> Option<u64> {
        let mut run = 0;
        for f in 0..self.frames {
            if self.is_free(f) {
                run += 1;
                if run == count {
                    let start = f + 1 - count;
                    for g in start..=f {
                        self.set_used(g);
                    }
                    self.free -= count;
                    return Some(start * PAGE_SIZE);
                }
            } else {
                run = 0;
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

    let mut fa = FrameAllocator { bitmap, frames, total: 0, free: 0, next: 0 };
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
    fa.next = LOW_RESERVED / PAGE_SIZE;
    log!(
        "mm",
        "{} MiB usable RAM ({} frames), bitmap {} KiB",
        fa.total * PAGE_SIZE >> 20,
        fa.total,
        bitmap_bytes / 1024
    );
    *FRAMES.lock() = Some(fa);
}

/// Allocates one zeroed-on-demand-free frame; returns its physical address.
pub fn alloc() -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc()
}

/// Allocates `count` physically contiguous frames.
pub fn alloc_contiguous(count: u64) -> Option<u64> {
    FRAMES.lock().as_mut()?.alloc_contiguous(count)
}

#[allow(dead_code)] // used by the self-tests; the GUI never frees frames yet
pub fn free(phys: u64) {
    if let Some(fa) = FRAMES.lock().as_mut() {
        let f = phys / PAGE_SIZE;
        if !fa.is_free(f) {
            fa.set_free(f);
            fa.free += 1;
        }
    }
}

/// (total usable frames, free frames)
pub fn counts() -> (u64, u64) {
    FRAMES.lock().as_ref().map(|f| (f.total, f.free)).unwrap_or((0, 0))
}
