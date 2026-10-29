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
/// In a 4 KiB page entry the same bit selects the PAT slot (see `arch::pat`).
const PAT_4K: u64 = 1 << 7;
/// Software bit: the frame is owned elsewhere (e.g. a shared surface); don't free it.
const SHARED: u64 = 1 << 9;
/// Software bit, with `SHARED`: a page of a cached file, shared read-only
/// by every mapping of it (see `fs::mapcache`). Making it
/// writable gives the address space a copy of its own.
const COPY_ON_WRITE: u64 = 1 << 10;
/// Software bit, with `SHARED`: the entry holds a count on the frame (see
/// `SHM_REFS`), which it gives back when it goes. Shared frames without it
/// (window surfaces) are their owners' to free: a stale entry for one must
/// never touch the count of whatever the frame has become since.
const COUNTED: u64 = 1 << 11;
/// Software bit, with `COPY_ON_WRITE`: the page is writable, and was made
/// read-only only to be shared after `fork`. A write fault gives the
/// writer the page to itself (see [`AddressSpace::resolve_write`]).
const FORK_WRITE: u64 = 1 << 52;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

/// Kernel virtual area for window surfaces and other dynamic kernel mappings.
pub const KVA_BASE: u64 = 0xFFFF_C000_0000_0000;
pub const KVA_SIZE: u64 = 64 << 30;

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

// ------------------------------------------------------- shared memory

/// Frames of shared memory objects (Linux `memfd`, `/dev/shm`, shared
/// anonymous mappings), with how many holders each has: the object, and
/// every page table entry mapping it. The last one to let go frees it.
/// (Other shared frames, such as window surfaces, aren't counted here:
/// their owners free them.)
static SHM_REFS: IrqMutex<alloc::collections::BTreeMap<u64, u32>> = IrqMutex::new(alloc::collections::BTreeMap::new());

/// A zeroed frame for a shared memory object, held once (by the object).
pub fn shm_alloc() -> Option<u64> {
    let f = zeroed_frame()?;
    SHM_REFS.lock().insert(f, 1);
    Some(f)
}

/// Another holder for a counted frame (no effect on others).
fn shm_retain(frame: u64) -> bool {
    match SHM_REFS.lock().get_mut(&frame) {
        Some(n) => {
            *n += 1;
            true
        }
        None => false,
    }
}

/// A counted frame holding `data` (a page at most; the rest zeroed), held
/// once (by its caller).
pub fn shm_alloc_with(data: &[u8]) -> Option<u64> {
    let f = frame::alloc()?;
    let page = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(f) as *mut u8, PAGE_SIZE as usize) };
    page[..data.len()].copy_from_slice(data);
    page[data.len()..].fill(0);
    SHM_REFS.lock().insert(f, 1);
    Some(f)
}

/// Another holder for a counted frame (`shm_alloc`).
pub fn shm_hold(frame: u64) {
    shm_retain(frame);
}

/// Counts private frames as shared by `holders` (a `fork`: parent and child).
fn shm_adopt(frames: &[u64], holders: u32) {
    let mut refs = SHM_REFS.lock();
    for &f in frames {
        *refs.entry(f).or_insert(0) += holders;
    }
}

/// How many hold a counted frame (0: it isn't one).
fn shm_holders(frame: u64) -> u32 {
    SHM_REFS.lock().get(&frame).copied().unwrap_or(0)
}

/// A holder lets go of a counted frame; the last one frees it.
pub fn shm_release(frame: u64) {
    let last = {
        let mut refs = SHM_REFS.lock();
        match refs.get_mut(&frame) {
            Some(n) if *n > 1 => {
                *n -= 1;
                false
            }
            Some(_) => {
                refs.remove(&frame);
                true
            }
            None => false,
        }
    };
    if last {
        frame::free(frame);
    }
}

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

/// The kernel virtual area's free space. Its lock is also held while the
/// area's page table entries change: two CPUs mapping neighbouring ranges
/// at once would otherwise each add a page table for the same 2 MiB, and
/// one of the mappings would be lost.
static KVA: IrqMutex<Kva> = IrqMutex::new(Kva { next: KVA_BASE, free: Vec::new() });

/// Maps `frames` contiguously into the kernel virtual area; returns the address.
pub fn kmap(frames: &[u64]) -> Option<u64> {
    let pages = frames.len() as u64;
    let mut k = KVA.lock();
    let base = if let Some(i) = k.free.iter().position(|&(_, p)| p == pages) {
        k.free.swap_remove(i).0
    } else {
        let b = k.next;
        // One unmapped guard page between allocations.
        k.next += (pages + 1) * PAGE_SIZE;
        if k.next > KVA_BASE + KVA_SIZE {
            return None;
        }
        b
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

/// Maps `size` bytes of device memory at `phys` (a framebuffer) into the
/// kernel virtual area, write-combining where the CPU supports it.
pub fn kmap_wc(phys: u64, size: u64) -> Option<u64> {
    if !crate::arch::pat::write_combining() {
        return None;
    }
    let start = phys & !(PAGE_SIZE - 1);
    let pages = (phys + size - start).div_ceil(PAGE_SIZE);
    let mut k = KVA.lock();
    let base = k.next;
    k.next += (pages + 1) * PAGE_SIZE;
    if k.next > KVA_BASE + KVA_SIZE {
        return None;
    }
    let pml4 = kernel_pml4();
    for i in 0..pages {
        let va = base + i * PAGE_SIZE;
        let e = walk(pml4, va, true, false)?;
        // Bit 7 of a 4 KiB entry selects PAT slot 4: write-combining.
        *e = (start + i * PAGE_SIZE) | PRESENT | WRITABLE | NO_EXECUTE | PAT_4K;
        x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
    }
    Some(base + (phys - start))
}

/// True if `[addr, addr+len)` is entirely mapped for user access (and
/// writable if `write`) in the address space the CPU runs on.
pub fn check_current(addr: u64, len: u64, write: bool) -> bool {
    check_range(Cr3::read().0.start_address().as_u64(), addr, len, write)
}

/// [`AddressSpace::check`] for the tables at `pml4`.
fn check_range(pml4: u64, addr: u64, len: u64, write: bool) -> bool {
    if len == 0 {
        return addr <= layout::USER_END;
    }
    let Some(end) = addr.checked_add(len) else { return false };
    if end > layout::USER_END {
        return false;
    }
    let mut page = addr & !(PAGE_SIZE - 1);
    while page < end {
        match walk(pml4, page, false, false) {
            // (A page shared since `fork` counts as writable: writing to it
            // faults, and the fault gives the writer a page of its own.)
            Some(e) if *e & (PRESENT | USER) == PRESENT | USER && (!write || *e & (WRITABLE | FORK_WRITE) != 0) => {}
            _ => return false,
        }
        page += PAGE_SIZE;
    }
    true
}

/// The page-table entries on the way to `va` in the current address space,
/// top level first, as far as they are present (0 after; for fault
/// reports, so nothing is allocated).
pub fn entries_for(va: u64) -> [u64; 4] {
    let mut out = [0; 4];
    let mut t = Cr3::read().0.start_address().as_u64();
    for (i, level) in (0..=3u32).rev().enumerate() {
        let e = table(t)[idx(va, level)];
        out[i] = e;
        if e & PRESENT == 0 || (e & HUGE != 0 && level > 0) {
            break;
        }
        t = e & ADDR;
    }
    out
}

/// The physical address behind kernel address `va`, if it is mapped
/// (following 2 MiB and 1 GiB pages too).
pub fn translate(va: u64) -> Option<u64> {
    let mut t = kernel_pml4();
    for level in (1..=3).rev() {
        let e = table(t)[idx(va, level)];
        if e & PRESENT == 0 {
            return None;
        }
        if e & HUGE != 0 && level < 3 {
            let span = 1u64 << (12 + 9 * level);
            return Some((e & ADDR & !(span - 1)) | (va & (span - 1)));
        }
        t = e & ADDR;
    }
    let e = table(t)[idx(va, 0)];
    (e & PRESENT != 0).then_some((e & ADDR) | (va & (PAGE_SIZE - 1)))
}

/// The page-table entry mapping kernel address `va` (tests).
#[cfg(feature = "ktest")]
pub fn kernel_entry(va: u64) -> Option<u64> {
    walk(kernel_pml4(), va, false, false).map(|e| *e)
}

/// Unmaps a `kmap` range on every CPU. Must not be called while holding an
/// `IrqMutex` (other CPUs have to take the shootdown IPI).
pub fn kunmap(base: u64, pages: u64) {
    let pml4 = kernel_pml4();
    {
        let _tables = KVA.lock();
        for i in 0..pages {
            let va = base + i * PAGE_SIZE;
            if let Some(e) = walk(pml4, va, false, false) {
                *e = 0;
                x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
            }
        }
    }
    // Outside the lock: the other CPUs must be free to take the IPI.
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
#[allow(clippy::enum_variant_names)] // the usual names for page protections
pub enum Prot {
    ReadOnly,
    ReadWrite,
    ReadExec,
    /// Writable code (just-in-time compilers in Linux programs).
    ReadWriteExec,
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
                Prot::ReadWriteExec => WRITABLE,
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
        if !va.is_multiple_of(PAGE_SIZE) || va.checked_add(pages * PAGE_SIZE).is_none_or(|e| e > layout::USER_END) {
            return Err(EINVAL);
        }
        for i in 0..pages {
            let page = va + i * PAGE_SIZE;
            let e = walk(self.pml4, page, true, true).ok_or(ENOMEM)?;
            if *e & COPY_ON_WRITE != 0 && Self::flags(prot) & WRITABLE != 0 {
                // A cached file's page: writable means a copy of its own.
                self.protect(page, 1, Some(prot))?;
                continue;
            }
            if *e & PRESENT != 0 {
                let mut f = *e | Self::flags(prot) & (WRITABLE | USER);
                if matches!(prot, Prot::ReadExec | Prot::ReadWriteExec) {
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

    /// Gives `[va, va + pages*4K)` the protection `prot` (Linux `mprotect`):
    /// present pages change their permissions, missing ones are mapped
    /// zeroed, and `None` (no access) unmaps the range.
    pub fn protect(&mut self, va: u64, pages: u64, prot: Option<Prot>) -> Result<(), isize> {
        use aurora_abi::err::*;
        if !va.is_multiple_of(PAGE_SIZE) || va.checked_add(pages * PAGE_SIZE).is_none_or(|e| e > layout::USER_END) {
            return Err(EINVAL);
        }
        let Some(prot) = prot else {
            self.unmap(va, pages);
            return Ok(());
        };
        let writable = Self::flags(prot) & WRITABLE != 0;
        // Cached file pages this replaces with copies, let go of once no CPU can see them.
        let mut copied = Vec::new();
        for i in 0..pages {
            let page = va + i * PAGE_SIZE;
            let e = walk(self.pml4, page, true, true).ok_or(ENOMEM)?;
            if *e & (PRESENT | COPY_ON_WRITE) == PRESENT | COPY_ON_WRITE && writable {
                let f = frame::alloc().ok_or(ENOMEM)?;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        phys_to_virt(*e & ADDR) as *const u8,
                        phys_to_virt(f) as *mut u8,
                        PAGE_SIZE as usize,
                    );
                }
                copied.push(*e);
                *e = f | Self::flags(prot);
                self.resident += PAGE_SIZE;
            } else if *e & PRESENT != 0 {
                *e = (*e & (ADDR | SHARED | COUNTED | COPY_ON_WRITE)) | Self::flags(prot);
            } else {
                *e = zeroed_frame().ok_or(ENOMEM)? | Self::flags(prot);
                self.resident += PAGE_SIZE;
            }
            self.invalidate(page);
        }
        // Permissions shrank too: every CPU running this process must see it.
        shootdown(va, pages, Some(self.pml4));
        for e in copied {
            Self::release_shared(e);
        }
        Ok(())
    }

    /// True if no page of `[va, va + pages*4K)` is mapped.
    pub fn is_free(&self, va: u64, pages: u64) -> bool {
        (0..pages).all(|i| self.user_page(va + i * PAGE_SIZE).is_none())
    }

    /// Maps externally owned frames (shared memory) at `va`.
    pub fn map_shared(&mut self, va: u64, frames: &[u64], prot: Prot) -> Result<(), isize> {
        self.map_counted(va, frames, Self::flags(prot) | SHARED)
    }

    /// Maps pages of a cached file (counted frames) at `va`, read-only for
    /// everyone: a protection that allows writing gives this address space
    /// its own copies.
    pub fn map_cached(&mut self, va: u64, frames: &[u64], prot: Prot) -> Result<(), isize> {
        self.map_counted(va, frames, Self::flags(prot) & !WRITABLE | SHARED | COPY_ON_WRITE)?;
        if Self::flags(prot) & WRITABLE != 0 {
            self.protect(va, frames.len() as u64, Some(prot))?;
        }
        Ok(())
    }

    fn map_counted(&mut self, va: u64, frames: &[u64], flags: u64) -> Result<(), isize> {
        for (i, &f) in frames.iter().enumerate() {
            let page = va + i as u64 * PAGE_SIZE;
            let e = walk(self.pml4, page, true, true).ok_or(aurora_abi::err::ENOMEM)?;
            if *e & PRESENT != 0 {
                Self::drop_entry(*e, &mut self.resident);
            }
            let counted = if shm_retain(f) { COUNTED } else { 0 };
            *e = f | flags | counted;
            self.invalidate(page);
        }
        Ok(())
    }

    /// Lets go of what a present entry maps (a private frame, or a hold on a shared one).
    fn drop_entry(e: u64, resident: &mut u64) {
        if e & SHARED == 0 {
            frame::free(e & ADDR);
            *resident = resident.saturating_sub(PAGE_SIZE);
        } else {
            Self::release_shared(e);
        }
    }

    /// Gives back a shared entry's count on its frame, if it holds one.
    fn release_shared(e: u64) {
        if e & COUNTED != 0 {
            shm_release(e & ADDR);
        }
    }

    /// Moves `pages` mapped at `from` to `to` (Linux `mremap`), without
    /// copying: the page table entries move. `to` must be free.
    pub fn move_pages(&mut self, from: u64, to: u64, pages: u64) -> Result<(), isize> {
        for i in 0..pages {
            let (src, dst) = (from + i * PAGE_SIZE, to + i * PAGE_SIZE);
            let entry = match walk(self.pml4, src, false, true) {
                Some(e) if *e & PRESENT != 0 => core::mem::replace(e, 0),
                _ => continue,
            };
            let d = walk(self.pml4, dst, true, true).ok_or(aurora_abi::err::ENOMEM)?;
            *d = entry;
            self.invalidate(src);
            self.invalidate(dst);
        }
        shootdown(from, pages, Some(self.pml4));
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
                        self.resident = self.resident.saturating_sub(PAGE_SIZE);
                    }
                    freed.push(*e);
                    *e = 0;
                    self.invalidate(page);
                }
            }
        }
        // Other threads of this process may run on other CPUs: they must
        // forget the pages before the frames can be reused.
        shootdown(va, pages, Some(self.pml4));
        for e in freed {
            if e & SHARED == 0 {
                frame::free(e & ADDR);
            } else {
                Self::release_shared(e);
            }
        }
    }

    /// A copy of this address space (Linux `fork`), made without copying
    /// memory: private pages become shared by both, read-only and
    /// copy-on-write, and whoever writes to one first gets a copy (or, if
    /// the other let go of it, the page itself). Shared memory stays
    /// shared; other shared frames (window surfaces) are left out.
    ///
    /// (Copying it all took Firefox a second per process it started, for a
    /// child that replaces it all at once with `exec`.)
    pub fn fork_copy(&mut self) -> Option<AddressSpace> {
        let mut child = AddressSpace::new()?;
        child.mmap_next = self.mmap_next;
        let mut shared = 0;
        let ok = Self::copy_level(self.pml4, child.pml4, 3, &mut shared);
        self.resident = self.resident.saturating_sub(shared);
        // The parent's pages are read-only now: no CPU may keep writing
        // through what it remembers of them.
        if Cr3::read().0.start_address().as_u64() == self.pml4 {
            x86_64::instructions::tlb::flush_all();
        }
        shootdown(0, u64::MAX, Some(self.pml4));
        // On failure, dropping the partial copy lets go of what it holds.
        ok.then_some(child)
    }

    fn copy_level(src: u64, dst: u64, level: u32, shared: &mut u64) -> bool {
        let limit = if level == 3 { 256 } else { 512 };
        if level == 0 {
            let mut adopted = Vec::new();
            for i in 0..limit {
                let e = table(src)[i];
                if e & PRESENT == 0 {
                    continue;
                }
                if e & SHARED != 0 {
                    // Shared memory stays shared with the child; other
                    // shared frames (window surfaces) stay with the parent.
                    if e & COUNTED != 0 {
                        shm_retain(e & ADDR);
                        table(dst)[i] = e;
                    }
                    continue;
                }
                let writable = if e & WRITABLE != 0 { FORK_WRITE } else { 0 };
                let both = e & !WRITABLE | SHARED | COUNTED | COPY_ON_WRITE | writable;
                table(src)[i] = both;
                table(dst)[i] = both;
                adopted.push(e & ADDR);
                *shared += PAGE_SIZE;
            }
            shm_adopt(&adopted, 2);
            return true;
        }
        for i in 0..limit {
            let e = table(src)[i];
            if e & PRESENT == 0 {
                continue;
            }
            let Some(t) = zeroed_frame() else { return false };
            table(dst)[i] = t | (e & !ADDR);
            if !Self::copy_level(e & ADDR, t, level - 1, shared) {
                return false;
            }
        }
        true
    }

    /// A write to `va` faulted: if the page is one shared since `fork`, it
    /// becomes this address space's own (the frame itself when nobody else
    /// holds it any more, a copy otherwise) and the write can go again.
    /// Also true when another thread already did that.
    pub fn resolve_write(&mut self, va: u64) -> bool {
        let page = va & !(PAGE_SIZE - 1);
        let Some(e) = walk(self.pml4, page, false, false) else { return false };
        let old = *e;
        if old & (PRESENT | USER) != PRESENT | USER {
            return false;
        }
        if old & WRITABLE != 0 {
            return true;
        }
        if old & (COPY_ON_WRITE | FORK_WRITE) != COPY_ON_WRITE | FORK_WRITE {
            return false;
        }
        let flags = old & !(ADDR | SHARED | COUNTED | COPY_ON_WRITE | FORK_WRITE) | WRITABLE;
        let frame = old & ADDR;
        if shm_holders(frame) == 1 {
            // The last holder: no copy needed.
            SHM_REFS.lock().remove(&frame);
            *e = frame | flags;
            self.resident += PAGE_SIZE;
            self.invalidate(page);
            return true;
        }
        let Some(copy) = frame::alloc() else { return false };
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(frame) as *const u8,
                phys_to_virt(copy) as *mut u8,
                PAGE_SIZE as usize,
            );
        }
        *e = copy | flags;
        self.resident += PAGE_SIZE;
        self.invalidate(page);
        // Other threads must stop reading the shared page before this one writes its own.
        shootdown(page, 1, Some(self.pml4));
        shm_release(frame);
        true
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

    /// The user mappings, lowest first, as (start, end, writable, executable),
    /// neighbouring pages with the same permissions merged.
    pub fn regions(&self) -> Vec<(u64, u64, bool, bool)> {
        let mut out: Vec<(u64, u64, bool, bool)> = Vec::new();
        let next = |e: u64| (e & PRESENT != 0 && e & HUGE == 0).then(|| table(e & ADDR));
        for (i4, &e4) in table(self.pml4)[..256].iter().enumerate() {
            let Some(l3) = next(e4) else { continue };
            for (i3, &e3) in l3.iter().enumerate() {
                let Some(l2) = next(e3) else { continue };
                for (i2, &e2) in l2.iter().enumerate() {
                    let Some(l1) = next(e2) else { continue };
                    for (i1, &e) in l1.iter().enumerate() {
                        if e & (PRESENT | USER) != (PRESENT | USER) {
                            continue;
                        }
                        let va = (i4 as u64) << 39 | (i3 as u64) << 30 | (i2 as u64) << 21 | (i1 as u64) << 12;
                        let (w, x) = (e & (WRITABLE | FORK_WRITE) != 0, e & NO_EXECUTE == 0);
                        match out.last_mut() {
                            Some(last) if last.1 == va && (last.2, last.3) == (w, x) => last.1 += PAGE_SIZE,
                            _ => out.push((va, va + PAGE_SIZE, w, x)),
                        }
                    }
                }
            }
        }
        out
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

    /// True if `[addr, addr+len)` is entirely mapped for user access (and
    /// writable if `write`). (System calls check the running process with
    /// [`check_current`]; this is for the self-tests.)
    #[cfg(feature = "ktest")]
    pub fn check(&self, addr: u64, len: u64, write: bool) -> bool {
        check_range(self.pml4, addr, len, write)
    }

    /// Copies `data` into user memory at `va` through the physical window (works
    /// even when this address space is not the active one). Pages shared
    /// since `fork` become this address space's own first; pages of cached
    /// files are refused: everyone mapping the file shares them.
    pub fn write_bytes(&mut self, va: u64, data: &[u8]) -> Result<(), isize> {
        let mut done = 0usize;
        while done < data.len() {
            let cur = va + done as u64;
            if walk(self.pml4, cur, false, false).is_some_and(|e| *e & COPY_ON_WRITE != 0) && !self.resolve_write(cur) {
                return Err(aurora_abi::err::EFAULT);
            }
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
                } else {
                    Self::release_shared(*e);
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
