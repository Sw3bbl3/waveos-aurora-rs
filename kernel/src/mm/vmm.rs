//! Virtual memory: per-process address spaces and kernel virtual allocations.
//!
//! Every address space shares the kernel half (PML4 entries 256..512). At boot
//! all 256 kernel PML4 slots are pre-populated with (possibly empty) PDPTs, so
//! any later kernel mapping — MMIO, window surfaces — lands in page-table pages
//! that every address space already references.

use super::{frame, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use aurora_abi::layout;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::registers::model_specific::{Efer, EferFlags};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
const HUGE: u64 = 1 << 7;
/// Software bit: the frame is owned elsewhere (e.g. a shared surface); don't free it.
const SHARED: u64 = 1 << 9;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Kernel virtual area for window surfaces and other dynamic kernel mappings.
const KVA_BASE: u64 = 0xFFFF_C000_0000_0000;
const KVA_SIZE: u64 = 64 << 30;

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

pub fn kernel_pml4() -> u64 {
    KERNEL_PML4.load(Ordering::Relaxed)
}

fn table(phys: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

fn zeroed_frame() -> Option<u64> {
    let f = frame::alloc()?;
    unsafe { core::ptr::write_bytes(phys_to_virt(f) as *mut u8, 0, PAGE_SIZE as usize) };
    Some(f)
}

fn idx(va: u64, level: u32) -> usize {
    ((va >> (12 + 9 * level)) & 0x1FF) as usize
}

pub fn init() {
    let (pml4, _) = Cr3::read();
    let pml4 = pml4.start_address().as_u64();
    KERNEL_PML4.store(pml4, Ordering::Relaxed);
    let t = table(pml4);
    let mut added = 0;
    for e in t.iter_mut().skip(256) {
        if *e & PRESENT == 0 {
            *e = zeroed_frame().expect("out of memory for kernel page tables") | PRESENT | WRITABLE;
            added += 1;
        }
    }
    // Enable the no-execute bit for user data pages.
    unsafe { Efer::update(|f| f.insert(EferFlags::NO_EXECUTE_ENABLE)) };
    x86_64::instructions::tlb::flush_all();
    log!("vmm", "kernel half pre-populated ({} new PDPTs), NX enabled", added);
}

/// Walks to the leaf entry for `va`, optionally creating intermediate tables.
fn walk(pml4: u64, va: u64, create: bool, user: bool) -> Option<&'static mut u64> {
    let mut t = pml4;
    for level in (1..=3).rev() {
        let e = &mut table(t)[idx(va, level)];
        if *e & PRESENT == 0 {
            if !create {
                return None;
            }
            *e = zeroed_frame()? | PRESENT | WRITABLE | if user { USER } else { 0 };
        } else if *e & HUGE != 0 {
            return None; // 2 MiB/1 GiB pages are only used by the physical window
        }
        t = *e & ADDR;
    }
    Some(&mut table(t)[idx(va, 0)])
}

// --------------------------------------------------------- kernel mappings

struct Kva {
    next: u64,
    free: Vec<(u64, u64)>, // (base, pages)
}

static KVA: IrqMutex<Kva> = IrqMutex::new(Kva { next: KVA_BASE, free: Vec::new() });

/// Maps `frames` contiguously into the kernel virtual area; returns the address.
pub fn kmap(frames: &[u64]) -> Option<u64> {
    let pages = frames.len() as u64;
    let base = {
        let mut k = KVA.lock();
        if let Some(i) = k.free.iter().position(|&(_, p)| p == pages) {
            k.free.swap_remove(i).0
        } else {
            let b = k.next;
            // One unmapped guard page between allocations.
            k.next += (pages + 1) * PAGE_SIZE;
            if k.next > KVA_BASE + KVA_SIZE {
                return None;
            }
            b
        }
    };
    let pml4 = kernel_pml4();
    for (i, &f) in frames.iter().enumerate() {
        let va = base + i as u64 * PAGE_SIZE;
        let e = walk(pml4, va, true, false)?;
        *e = f | PRESENT | WRITABLE | NO_EXECUTE;
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
    }
    Some(base)
}

/// Unmaps a `kmap` range on every CPU. Must not be called while holding an
/// `IrqMutex` (other CPUs have to take the shootdown IPI).
pub fn kunmap(base: u64, pages: u64) {
    let pml4 = kernel_pml4();
    for i in 0..pages {
        let va = base + i * PAGE_SIZE;
        if let Some(e) = walk(pml4, va, false, false) {
            *e = 0;
            x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
        }
    }
    shootdown(base, pages, None);
    KVA.lock().free.push((base, pages));
}

// ----------------------------------------------------------- TLB shootdown

/// One shootdown at a time; `PENDING` counts CPUs yet to flush.
static SHOOTDOWN: spin::Mutex<()> = spin::Mutex::new(());
static REQ_START: AtomicU64 = AtomicU64::new(0);
static REQ_PAGES: AtomicU64 = AtomicU64::new(0);
static PENDING: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Makes other CPUs drop stale translations of `[start, start + pages)`: all
/// of them for kernel addresses, or those running address space `cr3`.
/// While waiting it also serves requests aimed at this CPU, so two CPUs
/// shooting at each other can't deadlock even with interrupts off.
pub fn shootdown(start: u64, pages: u64, cr3: Option<u64>) {
    use crate::arch::percpu;
    if percpu::count() <= 1 {
        return;
    }
    let me = percpu::cpu_id();
    let targets: Vec<usize> = percpu::online()
        .filter(|&c| c != me && cr3.is_none_or(|r| percpu::CPUS[c].cr3.load(Ordering::Relaxed) == r))
        .collect();
    if targets.is_empty() {
        return;
    }
    let guard = loop {
        if let Some(g) = SHOOTDOWN.try_lock() {
            break g;
        }
        service_shootdown();
        core::hint::spin_loop();
    };
    REQ_START.store(start, Ordering::Relaxed);
    REQ_PAGES.store(pages, Ordering::Relaxed);
    PENDING.store(targets.len() as u32, Ordering::Release);
    for &c in &targets {
        percpu::CPUS[c].tlb_pending.store(true, Ordering::Release);
        crate::arch::apic::send_ipi(percpu::CPUS[c].lapic_id.load(Ordering::Relaxed), crate::arch::idt::TLB_VECTOR);
    }
    while PENDING.load(Ordering::Acquire) != 0 {
        service_shootdown();
        core::hint::spin_loop();
    }
    drop(guard);
}

/// Flushes what a shootdown asked of this CPU (from the IPI, or while waiting).
pub fn service_shootdown() {
    let me = &crate::arch::percpu::this().tlb_pending;
    if !me.swap(false, Ordering::AcqRel) {
        return;
    }
    let (start, pages) = (REQ_START.load(Ordering::Relaxed), REQ_PAGES.load(Ordering::Relaxed));
    if pages > 64 {
        x86_64::instructions::tlb::flush_all();
    } else {
        for i in 0..pages {
            x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(start + i * PAGE_SIZE));
        }
    }
    PENDING.fetch_sub(1, Ordering::AcqRel);
}

// ---------------------------------------------------------- address spaces

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Prot {
    ReadOnly,
    ReadWrite,
    ReadExec,
}

pub struct AddressSpace {
    pml4: u64,
    /// Next free address for `mmap`-style allocations.
    mmap_next: u64,
    /// Bytes of private memory mapped (for accounting).
    pub resident: u64,
}

impl AddressSpace {
    pub fn new() -> Option<AddressSpace> {
        let pml4 = zeroed_frame()?;
        let k = table(kernel_pml4());
        let t = table(pml4);
        t[256..].copy_from_slice(&k[256..]);
        Some(AddressSpace { pml4, mmap_next: layout::MMAP_BASE, resident: 0 })
    }

    pub fn cr3(&self) -> u64 {
        self.pml4
    }

    fn flags(prot: Prot) -> u64 {
        PRESENT
            | USER
            | match prot {
                Prot::ReadOnly => NO_EXECUTE,
                Prot::ReadWrite => WRITABLE | NO_EXECUTE,
                Prot::ReadExec => 0,
            }
    }

    fn invalidate(&self, va: u64) {
        if Cr3::read().0.start_address().as_u64() == self.pml4 {
            x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
        }
    }

    /// Maps fresh zeroed pages over `[va, va + pages*4K)`. Already-mapped pages
    /// are kept (their permissions are widened), which lets ELF segments share
    /// a page.
    pub fn map_anon(&mut self, va: u64, pages: u64, prot: Prot) -> Result<(), isize> {
        use aurora_abi::err::*;
        if va % PAGE_SIZE != 0 || va.checked_add(pages * PAGE_SIZE).is_none_or(|e| e > layout::USER_END) {
            return Err(EINVAL);
        }
        for i in 0..pages {
            let page = va + i * PAGE_SIZE;
            let e = walk(self.pml4, page, true, true).ok_or(ENOMEM)?;
            if *e & PRESENT != 0 {
                let mut f = *e | Self::flags(prot) & (WRITABLE | USER);
                if prot == Prot::ReadExec {
                    f &= !NO_EXECUTE;
                }
                *e = f;
            } else {
                *e = zeroed_frame().ok_or(ENOMEM)? | Self::flags(prot);
                self.resident += PAGE_SIZE;
            }
            self.invalidate(page);
        }
        Ok(())
    }

    /// Maps externally owned frames (shared memory) at `va`.
    pub fn map_shared(&mut self, va: u64, frames: &[u64], prot: Prot) -> Result<(), isize> {
        for (i, &f) in frames.iter().enumerate() {
            let page = va + i as u64 * PAGE_SIZE;
            let e = walk(self.pml4, page, true, true).ok_or(aurora_abi::err::ENOMEM)?;
            *e = f | Self::flags(prot) | SHARED;
            self.invalidate(page);
        }
        Ok(())
    }

    /// Unmaps pages, freeing private frames.
    pub fn unmap(&mut self, va: u64, pages: u64) {
        let mut freed = Vec::new();
        for i in 0..pages {
            let page = va + i * PAGE_SIZE;
            if let Some(e) = walk(self.pml4, page, false, true) {
                if *e & PRESENT != 0 {
                    if *e & SHARED == 0 {
                        freed.push(*e & ADDR);
                        self.resident = self.resident.saturating_sub(PAGE_SIZE);
                    }
                    *e = 0;
                    self.invalidate(page);
                }
            }
        }
        // Other threads of this process may run on other CPUs: they must
        // forget the pages before the frames can be reused.
        shootdown(va, pages, Some(self.pml4));
        for f in freed {
            frame::free(f);
        }
    }

    /// Reserves `pages` of address space in the mmap region (with a guard page after).
    pub fn reserve(&mut self, pages: u64) -> Option<u64> {
        let va = self.mmap_next;
        let end = va.checked_add((pages + 1) * PAGE_SIZE)?;
        if end > layout::MMAP_END {
            return None;
        }
        self.mmap_next = end;
        Some(va)
    }

    /// Physical address and writability of the page containing `va`, if it is user-accessible.
    fn user_page(&self, va: u64) -> Option<(u64, bool)> {
        if va >= layout::USER_END {
            return None;
        }
        let e = walk(self.pml4, va, false, false)?;
        if *e & (PRESENT | USER) != (PRESENT | USER) {
            return None;
        }
        Some((*e & ADDR, *e & WRITABLE != 0))
    }

    /// True if `[addr, addr+len)` is entirely mapped for user access (and writable if `write`).
    pub fn check(&self, addr: u64, len: u64, write: bool) -> bool {
        if len == 0 {
            return addr <= layout::USER_END;
        }
        let Some(end) = addr.checked_add(len) else { return false };
        if end > layout::USER_END {
            return false;
        }
        let mut page = addr & !(PAGE_SIZE - 1);
        while page < end {
            match self.user_page(page) {
                Some((_, w)) if w || !write => {}
                _ => return false,
            }
            page += PAGE_SIZE;
        }
        true
    }

    /// Copies `data` into user memory at `va` through the physical window (works
    /// even when this address space is not the active one).
    pub fn write_bytes(&self, va: u64, data: &[u8]) -> Result<(), isize> {
        let mut done = 0usize;
        while done < data.len() {
            let cur = va + done as u64;
            let (phys, _) = self.user_page(cur).ok_or(aurora_abi::err::EFAULT)?;
            let off = cur % PAGE_SIZE;
            let n = ((PAGE_SIZE - off) as usize).min(data.len() - done);
            unsafe {
                core::ptr::copy_nonoverlapping(data[done..].as_ptr(), (phys_to_virt(phys) + off) as *mut u8, n);
            }
            done += n;
        }
        Ok(())
    }

    fn free_level(t: u64, level: u32, user_half: bool) {
        let entries = table(t);
        let limit = if level == 3 && user_half { 256 } else { 512 };
        for e in entries.iter().take(limit) {
            if *e & PRESENT == 0 {
                continue;
            }
            if level == 0 {
                if *e & SHARED == 0 {
                    frame::free(*e & ADDR);
                }
            } else {
                Self::free_level(*e & ADDR, level - 1, false);
                frame::free(*e & ADDR);
            }
        }
    }
}

impl Drop for AddressSpace {
    fn drop(&mut self) {
        // Never free the address space we're running on.
        if Cr3::read().0.start_address().as_u64() == self.pml4 {
            unsafe { load(kernel_pml4()) };
        }
        Self::free_level(self.pml4, 3, true);
        frame::free(self.pml4);
    }
}

/// Switches CR3.
///
/// # Safety
/// `pml4` must be a valid top-level table sharing the kernel half.
pub unsafe fn load(pml4: u64) {
    use x86_64::structures::paging::PhysFrame;
    let (_, flags) = Cr3::read();
    Cr3::write(PhysFrame::containing_address(x86_64::PhysAddr::new(pml4)), flags);
}
