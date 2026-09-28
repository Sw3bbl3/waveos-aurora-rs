//! Builds the initial x86_64 4-level page tables handed to the kernel.
//!
//! While boot services are active, UEFI identity-maps memory, so physical
//! addresses of table pages can be dereferenced directly.

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const HUGE: u64 = 1 << 7;
const GLOBAL: u64 = 1 << 8;
const ADDR_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const TWO_MIB: u64 = 2 << 20;

pub struct PageTables {
    pml4: u64,
    alloc: fn() -> u64,
}

impl PageTables {
    pub fn new(alloc: fn() -> u64) -> Self {
        Self { pml4: alloc(), alloc }
    }

    pub fn pml4(&self) -> u64 {
        self.pml4
    }

    fn table(phys: u64) -> &'static mut [u64; 512] {
        unsafe { &mut *(phys as *mut [u64; 512]) }
    }

    /// Returns the next-level table behind `table[index]`, allocating it if needed.
    fn next(&mut self, table: u64, index: usize) -> u64 {
        let entries = Self::table(table);
        if entries[index] & PRESENT == 0 {
            entries[index] = (self.alloc)() | PRESENT | WRITABLE;
        }
        entries[index] & ADDR_MASK
    }

    fn idx(virt: u64, level: u32) -> usize {
        ((virt >> (12 + 9 * level)) & 0x1FF) as usize
    }

    /// Maps `[phys, phys+len)` at `virt` with 2 MiB pages.
    pub fn map_huge_range(&mut self, virt: u64, phys: u64, len: u64) {
        let mut off = 0;
        while off < len {
            let v = virt + off;
            let pdpt = self.next(self.pml4, Self::idx(v, 3));
            let pd = self.next(pdpt, Self::idx(v, 2));
            Self::table(pd)[Self::idx(v, 1)] = (phys + off) | PRESENT | WRITABLE | HUGE;
            off += TWO_MIB;
        }
    }

    /// Maps a single 4 KiB page.
    pub fn map_4k(&mut self, virt: u64, phys: u64) {
        let pdpt = self.next(self.pml4, Self::idx(virt, 3));
        let pd = self.next(pdpt, Self::idx(virt, 2));
        let pt = self.next(pd, Self::idx(virt, 1));
        Self::table(pt)[Self::idx(virt, 0)] = phys | PRESENT | WRITABLE | GLOBAL;
    }
}
