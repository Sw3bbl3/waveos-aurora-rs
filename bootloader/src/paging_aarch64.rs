//! Builds the initial AArch64 translation tables handed to the kernel, and
//! the hand-off itself.
//!
//! 4 KiB granule, 48-bit addresses, four levels, one root for both halves
//! (see the kernel's `arch::aarch64::mmu`): TTBR1 uses entries 256..512 for
//! the kernel half, TTBR0 (47 bits) entries 0..256 for the identity map the
//! loader needs while it switches, which the kernel then drops. While boot
//! services are active UEFI identity-maps memory, so table pages can be
//! dereferenced at their physical addresses.

const VALID: u64 = 1 << 0;
/// Table descriptor (levels 0-2) or page descriptor (level 3).
const TABLE_OR_PAGE: u64 = 1 << 1;
const AF: u64 = 1 << 10;
const SH_INNER: u64 = 3 << 8;
const PXN: u64 = 1 << 53;
const UXN: u64 = 1 << 54;
/// Software bit marking a block descriptor (the kernel's `pte::HUGE`).
const SW_BLOCK: u64 = 1 << 58;
const ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;
const TWO_MIB: u64 = 2 << 20;

/// Memory types: indices into MAIR_EL1 (the kernel's `mmu::MAIR`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Memory {
    /// Normal, write-back (RAM).
    Normal = 0,
    /// Device nGnRE (MMIO).
    Device = 1,
    /// Normal, non-cacheable (framebuffers).
    WriteCombining = 2,
}

/// MAIR_EL1: 0 normal write-back read/write-allocate, 1 device nGnRE,
/// 2 normal non-cacheable, 3 device nGnRnE.
pub const MAIR: u64 = 0xFF | 0x04 << 8 | 0x44 << 16;

pub struct PageTables {
    root: u64,
    alloc: fn() -> u64,
}

impl PageTables {
    pub fn new(alloc: fn() -> u64) -> Self {
        Self { root: alloc(), alloc }
    }

    pub fn root(&self) -> u64 {
        self.root
    }

    fn table(phys: u64) -> &'static mut [u64; 512] {
        // SAFETY: a zeroed page from `alloc`, identity-mapped by UEFI.
        unsafe { &mut *(phys as *mut [u64; 512]) }
    }

    /// Returns the next-level table behind `table[index]`, allocating it if needed.
    fn next(&mut self, table: u64, index: usize) -> u64 {
        let entries = Self::table(table);
        if entries[index] & VALID == 0 {
            entries[index] = (self.alloc)() | VALID | TABLE_OR_PAGE;
        }
        entries[index] & ADDR_MASK
    }

    fn idx(virt: u64, level: u32) -> usize {
        ((virt >> (12 + 9 * level)) & 0x1FF) as usize
    }

    /// Maps `[phys, phys+len)` at `virt` with 2 MiB blocks, each of the
    /// memory type `kind(block's physical address)` says; never executable
    /// unless `exec` (the identity map, which the loader runs from while
    /// it switches).
    pub fn map_blocks(&mut self, virt: u64, phys: u64, len: u64, exec: bool, kind: impl Fn(u64) -> Memory) {
        let mut off = 0;
        while off < len {
            let v = virt + off;
            let p = phys + off;
            let l1 = self.next(self.root, Self::idx(v, 3));
            let l2 = self.next(l1, Self::idx(v, 2));
            let m = kind(p);
            let xn = if exec && m == Memory::Normal { UXN } else { PXN | UXN };
            Self::table(l2)[Self::idx(v, 1)] = p | VALID | AF | SH_INNER | (m as u64) << 2 | xn | SW_BLOCK;
            off += TWO_MIB;
        }
    }

    /// Maps a single 4 KiB page of the kernel image: normal memory,
    /// writable, executable at EL1 (the kernel tightens it), global.
    pub fn map_4k(&mut self, virt: u64, phys: u64) {
        let l1 = self.next(self.root, Self::idx(virt, 3));
        let l2 = self.next(l1, Self::idx(virt, 2));
        let l3 = self.next(l2, Self::idx(virt, 1));
        Self::table(l3)[Self::idx(virt, 0)] = phys | VALID | TABLE_OR_PAGE | AF | SH_INNER | UXN;
    }
}

/// TCR_EL1 for the tables: T0SZ 17 (47-bit user half), T1SZ 16, 4 KiB
/// granules, inner/outer write-back walks, inner shareable, 16-bit ASIDs,
/// and the physical address size the CPU reports (ID_AA64MMFR0_EL1).
pub fn tcr() -> u64 {
    let mmfr0: u64;
    // SAFETY: reads an ID register.
    unsafe { core::arch::asm!("mrs {}, id_aa64mmfr0_el1", out(reg) mmfr0) };
    let ips = (mmfr0 & 0x7).min(5); // up to 48 bits
    let t0sz = 17;
    let t1sz = 16 << 16;
    let walk0 = 1 << 8 | 1 << 10 | 3 << 12; // IRGN0, ORGN0 write-back; SH0 inner
    let walk1 = 1 << 24 | 1 << 26 | 3 << 28; // IRGN1, ORGN1; SH1
    let tg1 = 2 << 30; // TG1 = 4 KiB
    let asid16 = 1 << 36; // AS
    t0sz | t1sz | walk0 | walk1 | tg1 | ips << 32 | asid16
}

/// SCTLR_EL1 for the kernel: MMU, caches, stack alignment checks, EL0
/// access to the cache-type register, `dc zva` and cache maintenance, the
/// RES1 bits of ARMv8.0, little-endian.
pub const SCTLR: u64 = 0x34D5_D81D;

/// Writes the D-cache back to the point of unification for `[start,
/// start+len)` (the kernel image just copied) and invalidates the I-cache,
/// so instruction fetches see it.
pub fn sync_icache(start: u64, len: u64) {
    let ctr: u64;
    // SAFETY: reads CTR_EL0; cache maintenance by address of memory the
    // loader owns; barriers.
    unsafe {
        core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr);
        let line = 4u64 << ((ctr >> 16) & 0xF);
        let mut p = start & !(line - 1);
        while p < start + len {
            core::arch::asm!("dc cvau, {}", in(reg) p);
            p += line;
        }
        core::arch::asm!("dsb ish", "ic iallu", "dsb ish", "isb");
    }
}

/// Switches to the new tables and jumps to the kernel at EL1 with `x0 =
/// boot_info`, on `stack`. From EL2 it sets EL1 up and drops there with
/// `eret`; at EL1 it turns the MMU off for the few instructions it takes
/// to load the new registers (running identity-mapped, touching no memory).
///
/// # Safety
/// Boot services are gone; `root` maps the kernel, the stack, `boot_info`
/// and (identity) this code.
pub unsafe fn enter_kernel(root: u64, entry: u64, stack: u64, boot_info: u64) -> ! {
    core::arch::asm!(
        "msr daifset, #0xf",
        "mrs x9, CurrentEL",
        "lsr x9, x9, #2",
        "cmp x9, #2",
        "b.eq 2f",
        // EL1: MMU off, new registers, MMU on.
        "mrs x9, sctlr_el1",
        "bic x9, x9, #1",
        "msr sctlr_el1, x9",
        "isb",
        "msr mair_el1, x1",
        "msr tcr_el1, x2",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x3",
        "msr tpidr_el1, xzr",
        "tlbi vmalle1",
        "dsb nsh",
        "isb",
        "msr sctlr_el1, x4",
        "isb",
        "ic iallu",
        "dsb nsh",
        "isb",
        "mov sp, x5",
        "mov x29, xzr",
        "mov x30, xzr",
        "br x6",
        // EL2: EL1 in AArch64 without traps (E2H off first, so the _el1
        // registers below are EL1's), the timers and the GIC's system
        // registers open to EL1, then `eret` to the kernel with the MMU on.
        "2:",
        "mov x9, #0x80000000",
        "msr hcr_el2, x9",
        "isb",
        "mov x9, #3",
        "msr cnthctl_el2, x9",
        "msr cntvoff_el2, xzr",
        "mov x9, #0x33ff",
        "msr cptr_el2, x9",
        "msr hstr_el2, xzr",
        "mrs x9, id_aa64pfr0_el1",
        "ubfx x9, x9, #24, #4",
        "cbz x9, 3f",
        "mov x9, #0xf",
        "msr s3_4_c12_c9_5, x9", // ICC_SRE_EL2: SRE, Enable (and the bypasses off)
        "isb",
        "3:",
        "mrs x9, midr_el1",
        "msr vpidr_el2, x9",
        "mrs x9, mpidr_el1",
        "msr vmpidr_el2, x9",
        "msr mair_el1, x1",
        "msr tcr_el1, x2",
        "msr ttbr0_el1, x3",
        "msr ttbr1_el1, x3",
        "msr tpidr_el1, xzr",
        "msr sctlr_el1, x4",
        "msr sp_el1, x5",
        "msr elr_el2, x6",
        "mov x9, #0x3c5", // EL1h, DAIF masked
        "msr spsr_el2, x9",
        "tlbi vmalle1",
        "dsb nsh",
        "isb",
        "mov x29, xzr",
        "mov x30, xzr",
        "eret",
        // Fixed registers, so x9 is free for scratch.
        in("x0") boot_info,
        in("x1") MAIR,
        in("x2") tcr(),
        in("x3") root,
        in("x4") SCTLR,
        in("x5") stack,
        in("x6") entry,
        options(noreturn)
    );
}
