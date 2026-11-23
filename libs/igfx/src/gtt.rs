//! The global GTT: the display engine reads surfaces through it.
//!
//! Since Broadwell a GGTT entry is 64 bits: the page's physical address
//! (bits 38:12) and bit 0, present. The table sits half way into BAR 0;
//! its size comes from `GGC` (`GGMS`, 7:6: 2, 4 or 8 MiB of entries, so 1,
//! 2 or 4 GiB of address space). The firmware maps its stolen memory (and
//! the framebuffer in it) at the bottom; the driver never touches entries
//! below the end of what the firmware uses, and allocates above.
//!
//! Display engines prefetch past the end of a surface, so every mapping is
//! followed by guard entries pointing at a scratch page, and freed ranges
//! are pointed at the scratch page too (never left dangling).

use alloc::vec::Vec;

pub const PAGE: u64 = 4096;
pub const PRESENT: u64 = 1;
/// Guard pages after each mapping.
pub const GUARD_PAGES: u64 = 16;

/// A GGTT entry for the page at `phys`.
pub fn pte(phys: u64) -> u64 {
    (phys & 0x7F_FFFF_F000) | PRESENT
}

/// The physical page of an entry, if present.
pub fn pte_page(e: u64) -> Option<u64> {
    (e & PRESENT != 0).then_some(e & 0x7F_FFFF_F000)
}

/// GGTT address space in bytes for a `GGC` value (0 if none).
pub fn ggtt_size(ggc: u16) -> u64 {
    match (ggc >> 6) & 3 {
        0 => 0,
        n => (1u64 << n) * (1 << 20) / 8 * PAGE,
    }
}

/// Stolen (pre-allocated) graphics memory in bytes for a `GGC` value
/// (`GMS`, 15:8: 32 MiB units below 0xF0, 4 MiB units from 0xF0).
pub fn stolen_size(ggc: u16) -> u64 {
    let gms = (ggc >> 8) as u64;
    if gms >= 0xF0 {
        (gms - 0xF0 + 1) * (4 << 20)
    } else {
        gms * (32 << 20)
    }
}

/// The table itself: 64-bit entries, one per 4 KiB page of GGTT space.
pub trait Table {
    fn set(&self, index: u64, pte: u64);
    fn get(&self, index: u64) -> u64;
    /// After writing entries: make the GPU see them (`GFX_FLSH_CNTL`).
    fn flush(&self);
}

/// A first-fit allocator over GGTT addresses, [start, end).
#[derive(Clone, Debug)]
pub struct Space {
    pub start: u64,
    pub end: u64,
    /// Free ranges (start, end), sorted and coalesced.
    free: Vec<(u64, u64)>,
}

impl Space {
    pub fn new(start: u64, end: u64) -> Space {
        let start = start.next_multiple_of(PAGE);
        let end = end & !(PAGE - 1);
        Space { start, end, free: if start < end { alloc::vec![(start, end)] } else { Vec::new() } }
    }

    /// `size` bytes at an `align`-aligned address, plus the guard pages.
    pub fn alloc(&mut self, size: u64, align: u64) -> Option<u64> {
        let size = size.next_multiple_of(PAGE) + GUARD_PAGES * PAGE;
        for i in 0..self.free.len() {
            let (s, e) = self.free[i];
            let at = s.next_multiple_of(align);
            if at + size > e {
                continue;
            }
            // Split around [at, at + size).
            let mut repl = Vec::new();
            if s < at {
                repl.push((s, at));
            }
            if at + size < e {
                repl.push((at + size, e));
            }
            self.free.splice(i..=i, repl);
            return Some(at);
        }
        None
    }

    /// Frees what [`Space::alloc`] returned for `size`.
    pub fn free(&mut self, at: u64, size: u64) {
        let size = size.next_multiple_of(PAGE) + GUARD_PAGES * PAGE;
        let pos = self.free.partition_point(|r| r.0 < at);
        self.free.insert(pos, (at, at + size));
        // Coalesce neighbours.
        let mut out: Vec<(u64, u64)> = Vec::with_capacity(self.free.len());
        for &(s, e) in &self.free {
            match out.last_mut() {
                Some(last) if last.1 >= s => last.1 = last.1.max(e),
                _ => out.push((s, e)),
            }
        }
        self.free = out;
    }

    /// Bytes still free.
    pub fn available(&self) -> u64 {
        self.free.iter().map(|r| r.1 - r.0).sum()
    }
}
