//! Virtual memory: per-process address spaces and kernel virtual allocations.
//!
//! Every address space shares the kernel half (PML4 entries 256..512). At boot
//! all 256 kernel PML4 slots are pre-populated with (possibly empty) PDPTs, so
//! any later kernel mapping — MMIO, window surfaces — lands in page-table pages
//! that every address space already references.
//!
//! A user page may also be *swapped out* (`mm::swap`): its entry is then not
//! present and holds a swap slot instead of a frame ([`SWAPPED`]), with the
//! page's protection, and a fault brings the page back.

use super::swap::{self, Map};
use super::{frame, phys_to_virt, swapfile, PAGE_SIZE};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use aurora_abi::layout;
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::registers::control::Cr3;
use x86_64::registers::model_specific::{Efer, EferFlags};

const PRESENT: u64 = 1 << 0;
const WRITABLE: u64 = 1 << 1;
const USER: u64 = 1 << 2;
/// Set by the CPU when the page is used, and when it is written.
const ACCESSED: u64 = 1 << 5;
const DIRTY: u64 = 1 << 6;
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
/// `swap::PAGES`), which it gives back when it goes. Shared frames without it
/// (window surfaces) are their owners' to free: a stale entry for one must
/// never touch the count of whatever the frame has become since.
const COUNTED: u64 = 1 << 11;
/// Software bit, with `COPY_ON_WRITE`: the page is writable, and was made
/// read-only only to be shared after `fork`. A write fault gives the
/// writer the page to itself (see [`AddressSpace::resolve_write`]).
const FORK_WRITE: u64 = 1 << 52;
/// Software bit: the page is locked in memory (Linux `mlock`): never swapped out.
const LOCKED: u64 = 1 << 53;
/// Software bit: swapping the page out failed for want of room (it didn't
/// compress, and the swap file was full or missing); not tried again until
/// it is written ([`DIRTY`] is cleared when this is set) or the swap file
/// has room.
const INCOMPRESSIBLE: u64 = 1 << 54;
/// In an entry that is not present: the page is swapped out, to the slot
/// in the address bits. `WRITABLE`, `USER` and `NO_EXECUTE` keep its
/// protection (writable, for a page shared copy-on-write, meaning it
/// becomes writable once it is the address space's own).
const SWAPPED: u64 = 1 << 62;
const NO_EXECUTE: u64 = 1 << 63;
const ADDR: u64 = 0x000F_FFFF_FFFF_F000;

fn is_swap(e: u64) -> bool {
    e & (PRESENT | SWAPPED) == SWAPPED
}

fn slot_of(e: u64) -> u32 {
    ((e & ADDR) >> 12) as u32
}

fn swap_entry(slot: u32, writable: bool, exec: bool) -> u64 {
    SWAPPED | USER | (slot as u64) << 12 | if writable { WRITABLE } else { 0 } | if exec { 0 } else { NO_EXECUTE }
}

/// The CPU sets accessed and dirty bits in entries while the kernel looks
/// at them: entries the reclaim scan changes are changed atomically.
fn atomic(e: &mut u64) -> &AtomicU64 {
    // SAFETY: an aligned u64 in a page table, which lives as long as the entry.
    unsafe { &*(e as *mut u64 as *const AtomicU64) }
}

/// The outcome of a page fault the kernel tried to resolve.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    /// The access can go again.
    Resolved,
    /// A real fault: nothing is (or may be) there.
    Invalid,
    /// The page is there to make, but memory is out for now.
    NoMemory,
}

/// Kernel virtual area for window surfaces and other dynamic kernel mappings.
pub const KVA_BASE: u64 = 0xFFFF_C000_0000_0000;
pub const KVA_SIZE: u64 = 64 << 30;

static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);

// ------------------------------------------------------- shared memory

// Frames of shared memory objects (Linux `memfd`, `/dev/shm`, shared
// anonymous mappings), mapped files and pages shared since `fork` are
// *counted* (`swap::PAGES`): how many hold each — objects, every page
// table entry mapping it, and the swap slot caching it. The last one to
// let go frees it. (Other shared frames, such as window surfaces, aren't
// counted: their owners free them.)

/// A zeroed frame for a shared memory object, held once (by the object).
/// (No address space is locked here: it may wait for memory.)
pub fn shm_alloc() -> Option<u64> {
    let f = frame::alloc_user()?;
    unsafe { core::ptr::write_bytes(phys_to_virt(f) as *mut u8, 0, PAGE_SIZE as usize) };
    swap::count(f, 1, 1);
    Some(f)
}

/// Another holder for a counted frame: a page table entry (no effect on others).
fn shm_retain(frame: u64) -> bool {
    swap::hold(frame, false)
}

/// A counted frame holding `data` (a page at most; the rest zeroed), held
/// once (by its caller, an object).
pub fn shm_alloc_with(data: &[u8]) -> Option<u64> {
    let f = frame::alloc_user()?;
    let page = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(f) as *mut u8, PAGE_SIZE as usize) };
    page[..data.len()].copy_from_slice(data);
    page[data.len()..].fill(0);
    swap::count(f, 1, 1);
    Some(f)
}

/// Another holder for a counted frame (`shm_alloc`): an object.
pub fn shm_hold(frame: u64) {
    swap::hold(frame, true);
}

/// Starts counting frames another owner allocated (a GPU buffer a program
/// maps), held once by that owner, which lets go with [`shm_release`].
/// They are device memory: never swapped out, by any mapping, ever (a
/// display may scan them out, another device may read them).
pub fn shm_count(frames: &[u64]) {
    swap::count_device(frames);
}

/// An object lets go of a counted frame; the last holder frees it.
pub fn shm_release(frame: u64) {
    swap::release(frame, true);
}

/// Lets a frame pinned by [`AddressSpace::shared_frame_pinned`] move again.
pub fn unpin_frame(phys: u64) {
    swap::unpin(phys & ADDR);
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

/// A frame for a program's own memory: refused once only the kernel's
/// reserve is left. Never waits for reclaim: an address space's lock is
/// held here, and `kswapd` may need it to make room. The rule: *nobody
/// waits for memory holding an address-space lock* — the fault and `mmap`
/// paths let go of it and wait outside (`proc::with_memory`).
fn user_frame() -> Option<u64> {
    frame::alloc_user_nowait()
}

/// [`user_frame`], zeroed.
fn zeroed_user_frame() -> Option<u64> {
    let f = user_frame()?;
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
/// Tables for user mappings (`user`) are the program's memory, refused once
/// only the kernel's reserve is left like its pages (a program mapping and
/// unmapping all over its address space used to fill memory with them).
fn walk(pml4: u64, va: u64, create: bool, user: bool) -> Option<&'static mut u64> {
    let mut t = pml4;
    for level in (1..=3).rev() {
        let e = &mut table(t)[idx(va, level)];
        if *e & PRESENT == 0 {
            if !create {
                return None;
            }
            let f = if user { zeroed_user_frame()? } else { zeroed_frame()? };
            *e = f | PRESENT | WRITABLE | if user { USER } else { 0 };
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
    // First fit among freed ranges, split (a free range of `p` pages is
    // followed by its guard page, as every allocation is). Reusing only
    // ranges of exactly the same size let the area — and its page tables —
    // grow without bound with surfaces of ever new sizes (found by `fuzz`).
    let base = if let Some(i) = k.free.iter().position(|&(_, p)| p >= pages) {
        let (b, p) = k.free[i];
        if p >= pages + 2 {
            k.free[i] = (b + (pages + 1) * PAGE_SIZE, p - pages - 1);
        } else {
            k.free.swap_remove(i);
        }
        b
    } else {
        let b = k.next;
        // One unmapped guard page between allocations.
        let next = b + (pages + 1) * PAGE_SIZE;
        if next > KVA_BASE + KVA_SIZE {
            return None;
        }
        k.next = next;
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

/// Maps frame `f` at kernel address `va` (writable, no-execute), outside
/// the kernel virtual area: for `mm::kstack`, which serializes its own
/// calls. False if a page table could not be allocated.
pub fn kmap_fixed(va: u64, f: u64) -> bool {
    match walk(kernel_pml4(), va, true, false) {
        Some(e) => {
            *e = f | PRESENT | WRITABLE | NO_EXECUTE;
            x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
            true
        }
        None => false,
    }
}

/// Unmaps the page at kernel address `va` (from [`kmap_fixed`]) on this
/// CPU only; returns its frame. The caller shoots down other CPUs' TLBs.
pub fn kunmap_fixed(va: u64) -> Option<u64> {
    let e = walk(kernel_pml4(), va, false, false)?;
    let old = core::mem::replace(e, 0);
    x86_64::instructions::tlb::flush(x86_64::VirtAddr::new(va));
    (old & PRESENT != 0).then_some(old & ADDR)
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

/// Pages mapped in the kernel virtual area, and pages of it ever used
/// (tests: where frames are held).
#[cfg(feature = "ktest")]
pub fn held_pages() -> (u64, u64) {
    let end = KVA.lock().next;
    let mut mapped = 0;
    let mut va = KVA_BASE;
    while va < end {
        if walk(kernel_pml4(), va, false, false).is_some_and(|e| *e & PRESENT != 0) {
            mapped += 1;
        }
        va += PAGE_SIZE;
    }
    (mapped, (end - KVA_BASE) / PAGE_SIZE)
}

/// (pages of the kernel virtual area in use, counted shared frames), for
/// memory reports; zeros where a lock is busy.
pub fn usage() -> (u64, u64) {
    let kva =
        KVA.try_lock().map_or(0, |k| (k.next - KVA_BASE) / PAGE_SIZE - k.free.iter().map(|&(_, p)| p + 1).sum::<u64>());
    (kva, swap::counted_frames())
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
    let mut k = KVA.lock();
    k.free.push((base, pages));
    // Merge neighbours (each range with the guard page after it), and give
    // a range at the end back to the never-used part.
    // (In place: the heap frees large blocks through here.)
    k.free.sort_unstable_by_key(|&(b, _)| b);
    let mut i = 0;
    while i + 1 < k.free.len() {
        let ((b, p), (nb, np)) = (k.free[i], k.free[i + 1]);
        if b + (p + 1) * PAGE_SIZE == nb {
            k.free[i].1 = p + 1 + np;
            k.free.remove(i + 1);
        } else {
            i += 1;
        }
    }
    if let Some(&(b, p)) = k.free.last() {
        if b + (p + 1) * PAGE_SIZE == k.next {
            k.next = b;
            k.free.pop();
        }
    }
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

/// Pages the reclaim scan swaps out at a time (one TLB shootdown each).
const BATCH: usize = 64;

/// What a reclaim scan of an address space did.
#[derive(Default, Clone, Copy, Debug)]
pub struct Reclaimed {
    /// Pages looked at.
    pub scanned: u64,
    /// Pages swapped out.
    pub evicted: u64,
    /// Pages that could not be stored (no room for them as they were).
    pub failed: u64,
    /// Swap is full: no point going on.
    pub full: bool,
}

/// An address space's pages (see [`AddressSpace::page_counts`]).
#[derive(Default, Clone, Copy, Debug)]
pub struct PageCounts {
    pub private: u64,
    pub shared: u64,
    pub swapped: u64,
    pub locked: u64,
}

pub struct AddressSpace {
    pml4: u64,
    /// Next free address for `mmap`-style allocations (randomized; `mm::aslr`).
    mmap_next: u64,
    /// The top of the main thread's stack (randomized by the program loaders).
    pub stack_top: u64,
    /// Bytes of private memory mapped (for accounting).
    pub resident: u64,
    /// Pages swapped out (entries holding a swap slot).
    pub swapped: u64,
    /// Where the next reclaim scan starts.
    cursor: u64,
    /// Memory mapped but not made yet: start -> (end, protection), not
    /// overlapping. A page in one gets a zeroed frame when first touched
    /// (see [`AddressSpace::resolve_fault`]), so reserving gigabytes costs
    /// nothing (WebKit's JavaScript engine asks for 8 GiB at a time).
    lazy: alloc::collections::BTreeMap<u64, (u64, Prot)>,
}

impl AddressSpace {
    pub fn new() -> Option<AddressSpace> {
        let pml4 = zeroed_user_frame()?;
        let k = table(kernel_pml4());
        let t = table(pml4);
        t[256..].copy_from_slice(&k[256..]);
        Some(AddressSpace {
            pml4,
            mmap_next: super::aslr::mmap_base(),
            stack_top: layout::STACK_TOP,
            resident: 0,
            swapped: 0,
            cursor: 0,
            lazy: Default::default(),
        })
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

    /// Calls `f` with the address and entry of each present page in
    /// `[start, end)`, skipping missing page tables whole.
    fn each_present(&self, start: u64, end: u64, f: &mut dyn FnMut(u64, &mut u64)) {
        self.each_entry(start, end, &mut |va, e| {
            if *e & PRESENT != 0 {
                f(va, e)
            }
        });
    }

    /// Calls `f` with the address and entry of each page in `[start, end)`
    /// that is present or swapped out.
    fn each_entry(&self, start: u64, end: u64, f: &mut dyn FnMut(u64, &mut u64)) {
        fn visit(t: u64, level: u32, base: u64, start: u64, end: u64, f: &mut dyn FnMut(u64, &mut u64)) {
            let span = 1u64 << (12 + 9 * level);
            let first = ((start.max(base) - base) / span) as usize;
            for i in first..512 {
                let lo = base + i as u64 * span;
                if lo >= end {
                    break;
                }
                let e = &mut table(t)[i];
                if level == 0 {
                    if *e & PRESENT != 0 || is_swap(*e) {
                        f(lo, e);
                    }
                } else if *e & PRESENT != 0 && *e & HUGE == 0 {
                    visit(*e & ADDR, level - 1, lo, start, end, f);
                }
            }
        }
        if start < end {
            visit(self.pml4, 3, 0, start, end.min(layout::USER_END), f);
        }
    }

    /// The protection of the lazy range holding `va`, if any.
    fn lazy_at(&self, va: u64) -> Option<Prot> {
        self.lazy.range(..=va).next_back().filter(|(_, &(end, _))| va < end).map(|(_, &(_, p))| p)
    }

    /// Takes `[start, end)` out of the lazy ranges (splitting them).
    fn lazy_remove(&mut self, start: u64, end: u64) {
        let hit: Vec<(u64, u64, Prot)> =
            self.lazy.range(..end).rev().take_while(|(_, &(e, _))| e > start).map(|(&s, &(e, p))| (s, e, p)).collect();
        for (s, e, p) in hit {
            self.lazy.remove(&s);
            if s < start {
                self.lazy.insert(s, (start, p));
            }
            if e > end {
                self.lazy.insert(end, (e, p));
            }
        }
    }

    /// Makes `[va, va + pages*4K)` memory that is zero until touched, with
    /// protection `prot` (pages there already are kept).
    pub fn map_lazy(&mut self, va: u64, pages: u64, prot: Prot) -> Result<(), isize> {
        let end = va.checked_add(pages * PAGE_SIZE).filter(|&e| e <= layout::USER_END);
        let Some(end) = end.filter(|_| va.is_multiple_of(PAGE_SIZE)) else { return Err(aurora_abi::err::EINVAL) };
        // Each range is kernel memory the program asks for (Linux allows
        // as many mappings, `vm.max_map_count`).
        if self.lazy.len() >= 65_530 {
            return Err(aurora_abi::err::ENOMEM);
        }
        self.lazy_remove(va, end);
        self.lazy.insert(va, (end, prot));
        Ok(())
    }

    /// Every page of `[va, va + pages*4K)` has been made (or swapped out
    /// since): the range needn't be remembered as lazy memory any more.
    pub fn made(&mut self, va: u64, pages: u64) {
        self.lazy_remove(va, va.saturating_add(pages.saturating_mul(PAGE_SIZE)));
    }

    /// True if `[va, va + pages*4K)` has no lazy memory.
    fn lazy_free(&self, va: u64, end: u64) -> bool {
        self.lazy.range(..end).next_back().is_none_or(|(_, &(e, _))| e <= va)
    }

    /// A fault at `va` (a write if `write`): a lazy page gets its zeroed
    /// frame, a swapped-out page comes back, a page shared since `fork`
    /// becomes this address space's own (see [`AddressSpace::resolve_write`]).
    pub fn fault(&mut self, va: u64, write: bool) -> Fault {
        let page = va & !(PAGE_SIZE - 1);
        if page >= layout::USER_END {
            return Fault::Invalid;
        }
        if let Some(e) = walk(self.pml4, page, false, false) {
            if is_swap(*e) {
                let r = self.swap_in(page, *e);
                if r != Fault::Resolved {
                    return r;
                }
            }
            if *e & PRESENT != 0 {
                return if !write || *e & WRITABLE != 0 { Fault::Resolved } else { self.write_fault(page) };
            }
        }
        let Some(prot) = self.lazy_at(page) else { return Fault::Invalid };
        if write && Self::flags(prot) & WRITABLE == 0 {
            return Fault::Invalid;
        }
        let Some(e) = walk(self.pml4, page, true, true) else { return Fault::NoMemory };
        let Some(f) = zeroed_user_frame() else { return Fault::NoMemory };
        *e = f | Self::flags(prot);
        self.resident += PAGE_SIZE;
        self.invalidate(page);
        Fault::Resolved
    }

    /// [`AddressSpace::fault`]: true if the access can go again.
    pub fn resolve_fault(&mut self, va: u64, write: bool) -> bool {
        self.fault(va, write) == Fault::Resolved
    }

    /// Brings the page at `page` back from swap slot `entry` holds.
    fn swap_in(&mut self, page: u64, entry: u64) -> Fault {
        let slot = slot_of(entry);
        // The frame comes first: waiting for memory must not keep the slot
        // busy. (Bringing a page back may dip into the reserve a little:
        // it is memory the program had, and its slot is freed for the next.)
        let mut spare = None;
        let mut waits = 0u32;
        let map = loop {
            if spare.is_none() && !swap::is_cached(slot) {
                match frame::alloc_user_refault() {
                    Some(f) => spare = Some(f),
                    None => return Fault::NoMemory,
                }
            }
            match swap::begin_in(slot) {
                // Being moved (written back, or brought in by another
                // process sharing it): it won't be long.
                swap::In::Busy => {
                    waits += 1;
                    if waits < 64 {
                        crate::sched::yield_now();
                    } else {
                        crate::sched::sleep_ms(1);
                    }
                }
                swap::In::Mapped(m) => break m,
                swap::In::Load(stored) => {
                    let Some(f) = spare.take() else {
                        swap::abort_in(slot, stored);
                        continue;
                    };
                    // From disk, the pages swapped out after it come along
                    // when they lie after it on the disk.
                    if let swap::Stored::Disk { at, nonce } = stored {
                        if let Some(map) = self.read_ahead(page, slot, f, at, nonce) {
                            break map;
                        }
                    }
                    if !swap::load(&stored, f) {
                        swap::abort_in(slot, stored);
                        frame::free(f);
                        return Fault::Invalid;
                    }
                    break swap::finish_in(slot, f, stored);
                }
            }
        };
        if let Some(f) = spare {
            frame::free(f);
        }
        let prot = entry & (USER | NO_EXECUTE);
        let writable = entry & WRITABLE != 0;
        let e = match map {
            Map::Private(f) => {
                self.resident += PAGE_SIZE;
                f | PRESENT | ACCESSED | prot | if writable { WRITABLE } else { 0 }
            }
            Map::Counted { frame, shared: true } => {
                frame | PRESENT | ACCESSED | SHARED | COUNTED | prot | if writable { WRITABLE } else { 0 }
            }
            Map::Counted { frame, shared: false } => {
                frame
                    | PRESENT
                    | ACCESSED
                    | SHARED
                    | COUNTED
                    | COPY_ON_WRITE
                    | prot
                    | if writable { FORK_WRITE } else { 0 }
            }
        };
        // (Still the swap entry: changing it takes this address space's lock.)
        *walk(self.pml4, page, false, false).unwrap() = e;
        self.swapped = self.swapped.saturating_sub(1);
        Fault::Resolved
    }

    /// Swap-in read-ahead: brings page `page` (slot `slot`, on disk at
    /// `at`, into frame `f`) back together with the private pages after it
    /// in the address space that lie after it in the swap file (up to 8 in
    /// all, in one request), when memory is not short. The others are
    /// mapped not yet accessed, so they go first if unused. None (nothing
    /// done) if there are none, or the read failed.
    fn read_ahead(&mut self, page: u64, slot: u32, f: u64, at: u32, nonce: u64) -> Option<Map> {
        const AHEAD: usize = 8;
        if frame::counts().1 < super::kswapd::low_watermark() {
            return None;
        }
        let mut more = [(0u64, 0u32, 0u64, 0u64); AHEAD - 1];
        let mut n = 0;
        while n < AHEAD - 1 {
            let va = page + (n as u64 + 1) * PAGE_SIZE;
            let Some(e) = walk(self.pml4, va, false, false) else { break };
            if !is_swap(*e) {
                break;
            }
            let s = slot_of(*e);
            let Some(nonce) = swap::begin_readahead(s, at + n as u32 + 1) else { break };
            let Some(frame) = user_frame() else {
                swap::abort_in(s, swap::Stored::Disk { at: at + n as u32 + 1, nonce });
                break;
            };
            more[n] = (va, s, nonce, frame);
            n += 1;
        }
        let page_of =
            |f: u64| unsafe { core::slice::from_raw_parts_mut(phys_to_virt(f) as *mut u8, PAGE_SIZE as usize) };
        let mut nonces = [nonce; AHEAD];
        let mut pages: [&mut [u8]; AHEAD] = core::array::from_fn(|_| &mut [][..]);
        pages[0] = page_of(f);
        for i in 0..n {
            nonces[i + 1] = more[i].2;
            pages[i + 1] = page_of(more[i].3);
        }
        let start = crate::time::now_ns();
        if n == 0 || !swapfile::read_run(at, &nonces[..n + 1], &mut pages[..n + 1]) {
            for (i, &(_, s, nonce, frame)) in more[..n].iter().enumerate() {
                swap::abort_in(s, swap::Stored::Disk { at: at + i as u32 + 1, nonce });
                frame::free(frame);
            }
            return None;
        }
        swap::note_disk_in(n as u64 + 1, crate::time::now_ns() - start);
        for (i, &(va, s, nonce, frame)) in more[..n].iter().enumerate() {
            let Map::Private(frame) = swap::finish_in(s, frame, swap::Stored::Disk { at: at + i as u32 + 1, nonce })
            else {
                unreachable!("read-ahead takes private pages only")
            };
            let e = walk(self.pml4, va, false, false).unwrap();
            let writable = if *e & WRITABLE != 0 { WRITABLE } else { 0 };
            *e = frame | PRESENT | (*e & (USER | NO_EXECUTE)) | writable;
            self.resident += PAGE_SIZE;
            self.swapped = self.swapped.saturating_sub(1);
        }
        Some(swap::finish_in(slot, f, swap::Stored::Disk { at, nonce }))
    }

    /// Makes sure `[va, va + len)` is there to read (or write) now, the way
    /// faults would: for system calls about to copy to or from it.
    pub fn prefault(&mut self, va: u64, len: u64, write: bool) -> Fault {
        let Some(end) = va.checked_add(len) else { return Fault::Invalid };
        let mut page = va & !(PAGE_SIZE - 1);
        while page < end {
            let r = match walk(self.pml4, page, false, false) {
                Some(e) if *e & (PRESENT | USER) == PRESENT | USER => {
                    if !write || *e & WRITABLE != 0 {
                        Fault::Resolved
                    } else {
                        self.write_fault(page)
                    }
                }
                _ => self.fault(page, write),
            };
            if r != Fault::Resolved {
                return r;
            }
            page += PAGE_SIZE;
        }
        Fault::Resolved
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
            if is_swap(*e) {
                match self.swap_in(page, *e) {
                    Fault::Resolved => {}
                    Fault::NoMemory => return Err(ENOMEM),
                    Fault::Invalid => return Err(EIO),
                }
            }
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
                *e = zeroed_user_frame().ok_or(ENOMEM)? | Self::flags(prot);
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
        let end = va + pages * PAGE_SIZE;
        // Cached file pages this replaces with copies, let go of once no CPU can see them.
        let mut copied = Vec::new();
        let mut failed = false;
        let mut resident = 0;
        let flags = Self::flags(prot);
        self.each_entry(va, end, &mut |_, e| {
            if is_swap(*e) {
                // Swapped out: it comes back with the new protection.
                *e = swap_entry(slot_of(*e), writable, flags & NO_EXECUTE == 0);
                return;
            }
            if *e & COPY_ON_WRITE != 0 && writable {
                let Some(f) = user_frame() else {
                    failed = true;
                    return;
                };
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        phys_to_virt(*e & ADDR) as *const u8,
                        phys_to_virt(f) as *mut u8,
                        PAGE_SIZE as usize,
                    );
                }
                copied.push(*e);
                *e = f | flags | *e & LOCKED;
                resident += PAGE_SIZE;
            } else {
                *e = (*e & (ADDR | SHARED | COUNTED | COPY_ON_WRITE | LOCKED)) | flags;
            }
        });
        self.resident += resident;
        // Pages not there yet come when touched, with this protection.
        self.map_lazy(va, pages, prot)?;
        // Permissions shrank too: every CPU running this process must see it.
        if Cr3::read().0.start_address().as_u64() == self.pml4 {
            x86_64::instructions::tlb::flush_all();
        }
        shootdown(va, pages, Some(self.pml4));
        for e in copied {
            Self::release_shared(e);
        }
        if failed {
            return Err(ENOMEM);
        }
        Ok(())
    }

    /// True if no page of `[va, va + pages*4K)` is mapped.
    pub fn is_free(&self, va: u64, pages: u64) -> bool {
        let end = va.saturating_add(pages * PAGE_SIZE);
        let mut used = false;
        self.each_entry(va, end, &mut |_, e| used |= *e & USER != 0);
        !used && self.lazy_free(va, end)
    }

    /// The physical address of user address `va` in shared memory (as
    /// [`AddressSpace::shared_frame`]), with its frame pinned there until
    /// [`unpin_frame`] if it is a counted one (true): a futex keyed by it
    /// must find it in the same place.
    pub fn shared_frame_pinned(&self, va: u64) -> Option<(u64, bool)> {
        let phys = self.shared_frame(va)?;
        Some((phys, swap::pin(phys & ADDR)))
    }

    /// The physical address of user address `va`, if it is in a page of
    /// shared memory (a memfd, `/dev/shm`, a shared anonymous mapping),
    /// where other processes may see it too.
    pub fn shared_frame(&self, va: u64) -> Option<u64> {
        let e = *walk(self.pml4, va & !(PAGE_SIZE - 1), false, false)?;
        (e & PRESENT != 0 && e & SHARED != 0 && e & COPY_ON_WRITE == 0).then_some((e & ADDR) | (va & (PAGE_SIZE - 1)))
    }

    /// Changes the aligned user word at `va` atomically with `f`; returns
    /// its old value. The page is made present and writable first.
    pub fn atomic_update_u32(&mut self, va: u64, f: impl Fn(u32) -> u32) -> Result<u32, isize> {
        if !va.is_multiple_of(4) || self.prefault(va, 4, true) != Fault::Resolved {
            return Err(aurora_abi::err::EFAULT);
        }
        let e = *walk(self.pml4, va & !(PAGE_SIZE - 1), false, false).ok_or(aurora_abi::err::EFAULT)?;
        if e & PRESENT == 0 || e & WRITABLE == 0 {
            return Err(aurora_abi::err::EFAULT);
        }
        let word = super::phys_to_virt((e & ADDR) | (va & (PAGE_SIZE - 1)));
        // SAFETY: the frame is mapped writable in this address space, whose
        // lock the caller holds, so it stays allocated meanwhile.
        let atomic = unsafe { &*(word as *const core::sync::atomic::AtomicU32) };
        let mut old = atomic.load(Ordering::Relaxed);
        loop {
            match atomic.compare_exchange_weak(old, f(old), Ordering::SeqCst, Ordering::Relaxed) {
                Ok(_) => return Ok(old),
                Err(now) => old = now,
            }
        }
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
        // The holds on the frames first, under one lock (`walk` may need
        // the frame allocator, which mustn't wait inside it).
        let counted = swap::hold_many(frames);
        let mut replaced = Vec::new();
        let mut slots = Vec::new();
        let mut result = Ok(());
        let mut mapped = 0;
        for (i, (&f, &c)) in frames.iter().zip(&counted).enumerate() {
            let page = va + i as u64 * PAGE_SIZE;
            let Some(e) = walk(self.pml4, page, true, true) else {
                result = Err(aurora_abi::err::ENOMEM);
                break;
            };
            let old = core::mem::replace(e, f | flags | if c { COUNTED } else { 0 });
            // A page that wasn't mapped can't be in any TLB.
            if old & PRESENT != 0 {
                replaced.push(old);
                self.invalidate(page);
            } else if is_swap(old) {
                slots.push(slot_of(old));
            }
            mapped = i + 1;
        }
        self.swapped = self.swapped.saturating_sub(slots.len() as u64);
        for s in slots {
            swap::drop_ref(s);
        }
        // What the new entries replaced, and the holds not used (out of memory).
        let mut release: Vec<u64> =
            frames[mapped..].iter().zip(&counted[mapped..]).filter(|p| *p.1).map(|p| *p.0).collect();
        for e in replaced {
            if e & SHARED == 0 {
                frame::free(e & ADDR);
                self.resident = self.resident.saturating_sub(PAGE_SIZE);
            } else if e & COUNTED != 0 {
                release.push(e & ADDR);
            }
        }
        swap::release_many(&release);
        result
    }

    /// Gives back a shared entry's count on its frame, if it holds one.
    fn release_shared(e: u64) {
        if e & COUNTED != 0 {
            swap::release(e & ADDR, false);
        }
    }

    /// Moves `pages` mapped at `from` to `to` (Linux `mremap`), without
    /// copying: the page table entries move. `to` must be free.
    pub fn move_pages(&mut self, from: u64, to: u64, pages: u64) -> Result<(), isize> {
        let mut result = Ok(());
        for i in 0..pages {
            let (src, dst) = (from + i * PAGE_SIZE, to + i * PAGE_SIZE);
            // (Swapped-out pages move too: their entries hold their slots.)
            if walk(self.pml4, src, false, true).is_none_or(|e| *e & PRESENT == 0 && !is_swap(*e)) {
                continue;
            }
            // The destination's tables first: a page is never left in neither place.
            let Some(d) = walk(self.pml4, dst, true, true) else {
                result = Err(aurora_abi::err::ENOMEM);
                break;
            };
            let Some(e) = walk(self.pml4, src, false, true) else { continue };
            *d = core::mem::replace(e, 0);
            self.invalidate(src);
            self.invalidate(dst);
        }
        let tables = self.prune(from, from + pages * PAGE_SIZE);
        shootdown(from, pages.max(1), Some(self.pml4));
        for t in tables {
            frame::free(t);
        }
        result?;
        // Lazy memory there moves too.
        let (end, shift) = (from + pages * PAGE_SIZE, to.wrapping_sub(from));
        let parts: Vec<(u64, u64, Prot)> = self
            .lazy
            .range(..end)
            .filter(|(_, &(e, _))| e > from)
            .map(|(&s, &(e, p))| (s.max(from), e.min(end), p))
            .collect();
        self.lazy_remove(from, end);
        for (s, e, p) in parts {
            self.lazy.insert(s.wrapping_add(shift), (e.wrapping_add(shift), p));
        }
        Ok(())
    }

    /// Unmaps pages, freeing private frames, and the page tables left empty.
    pub fn unmap(&mut self, va: u64, pages: u64) {
        let end = va.saturating_add(pages.saturating_mul(PAGE_SIZE));
        self.lazy_remove(va, end);
        let mut freed = Vec::new();
        let mut private = 0;
        let mut slots = Vec::new();
        self.each_entry(va, end, &mut |_, e| {
            if is_swap(*e) {
                slots.push(slot_of(*e));
            } else {
                if *e & SHARED == 0 {
                    private += PAGE_SIZE;
                }
                freed.push(*e);
            }
            *e = 0;
        });
        self.resident = self.resident.saturating_sub(private);
        self.swapped = self.swapped.saturating_sub(slots.len() as u64);
        for s in slots {
            swap::drop_ref(s);
        }
        let tables = self.prune(va, end);
        if (!freed.is_empty() || !tables.is_empty()) && Cr3::read().0.start_address().as_u64() == self.pml4 {
            x86_64::instructions::tlb::flush_all();
        }
        // Other threads of this process may run on other CPUs: they must
        // forget the pages (and tables) before the frames can be reused.
        // (One `invlpg` also drops every cached upper-level entry, so a
        // freed table is never walked again.)
        shootdown(va, pages.max(!tables.is_empty() as u64), Some(self.pml4));
        let mut shared = Vec::new();
        for e in freed {
            if e & SHARED == 0 {
                frame::free(e & ADDR);
            } else if e & COUNTED != 0 {
                shared.push(e & ADDR);
            }
        }
        swap::release_many(&shared);
        for t in tables {
            frame::free(t);
        }
    }

    /// Unlinks the page tables under `[start, end)` that map nothing any
    /// more (after an unmap) and returns their frames, to be freed once no
    /// CPU can walk them. Without this, a program that maps and unmaps at
    /// ever new addresses (every `mmap` gets fresh address space) kept a
    /// page table for every 2 MiB it ever used, until it exited.
    fn prune(&mut self, start: u64, end: u64) -> Vec<u64> {
        fn visit(t: u64, level: u32, base: u64, start: u64, end: u64, out: &mut Vec<u64>) {
            let span = 1u64 << (12 + 9 * level);
            let limit = if level == 3 { 256 } else { 512 };
            let first = ((start.max(base) - base) / span) as usize;
            for i in first..limit {
                let lo = base + i as u64 * span;
                if lo >= end {
                    break;
                }
                let e = table(t)[i];
                if e & PRESENT == 0 || e & HUGE != 0 {
                    continue;
                }
                let child = e & ADDR;
                if level > 1 {
                    visit(child, level - 1, lo, start, end, out);
                }
                if table(child).iter().all(|&x| x == 0) {
                    table(t)[i] = 0;
                    out.push(child);
                }
            }
        }
        let mut out = Vec::new();
        let end = end.min(layout::USER_END);
        if start < end {
            visit(self.pml4, 3, 0, start, end, &mut out);
        }
        out
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
        child.stack_top = self.stack_top;
        child.lazy = self.lazy.clone();
        let mut shared = 0;
        let ok = Self::copy_level(self.pml4, child.pml4, 3, &mut shared, &mut child.swapped);
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

    fn copy_level(src: u64, dst: u64, level: u32, shared: &mut u64, swapped: &mut u64) -> bool {
        let limit = if level == 3 { 256 } else { 512 };
        if level == 0 {
            let mut adopted = Vec::new();
            for i in 0..limit {
                let e = table(src)[i];
                if is_swap(e) {
                    // Swapped out: both refer to the slot (and share the
                    // page copy-on-write once it comes back).
                    swap::retain(slot_of(e));
                    table(dst)[i] = e;
                    *swapped += 1;
                    continue;
                }
                if e & PRESENT == 0 {
                    continue;
                }
                // (Memory locked by the parent isn't the child's.)
                if e & SHARED != 0 {
                    // Shared memory stays shared with the child; other
                    // shared frames (window surfaces) stay with the parent.
                    if e & COUNTED != 0 {
                        shm_retain(e & ADDR);
                        table(dst)[i] = e & !LOCKED;
                    }
                    continue;
                }
                let writable = if e & WRITABLE != 0 { FORK_WRITE } else { 0 };
                let both = e & !WRITABLE | SHARED | COUNTED | COPY_ON_WRITE | writable;
                table(src)[i] = both;
                table(dst)[i] = both & !LOCKED;
                adopted.push(e & ADDR);
                *shared += PAGE_SIZE;
            }
            swap::adopt(&adopted, 2);
            return true;
        }
        for i in 0..limit {
            let e = table(src)[i];
            if e & PRESENT == 0 {
                continue;
            }
            let Some(t) = zeroed_user_frame() else { return false };
            table(dst)[i] = t | (e & !ADDR);
            if !Self::copy_level(e & ADDR, t, level - 1, shared, swapped) {
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
        self.write_fault(va & !(PAGE_SIZE - 1)) == Fault::Resolved
    }

    fn write_fault(&mut self, page: u64) -> Fault {
        let Some(e) = walk(self.pml4, page, false, false) else { return Fault::Invalid };
        let old = *e;
        if old & (PRESENT | USER) != PRESENT | USER {
            return Fault::Invalid;
        }
        if old & WRITABLE != 0 {
            return Fault::Resolved;
        }
        if old & (COPY_ON_WRITE | FORK_WRITE) != COPY_ON_WRITE | FORK_WRITE {
            return Fault::Invalid;
        }
        let flags = old & !(ADDR | SHARED | COUNTED | COPY_ON_WRITE | FORK_WRITE) | WRITABLE;
        let frame = old & ADDR;
        if swap::claim(frame) {
            // The last holder: no copy needed.
            *e = frame | flags;
            self.resident += PAGE_SIZE;
            self.invalidate(page);
            return Fault::Resolved;
        }
        let Some(copy) = user_frame() else { return Fault::NoMemory };
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
        swap::release(frame, false);
        Fault::Resolved
    }

    /// Linux `MADV_DONTNEED`: lazy memory in `[va, va + pages*4K)` lets go
    /// of its pages (they read as zero again); other private pages there
    /// are zeroed. Locked memory can't be (`EINVAL`).
    pub fn discard(&mut self, va: u64, pages: u64) -> Result<(), isize> {
        let end = va + pages * PAGE_SIZE;
        let mut locked = false;
        self.each_present(va, end, &mut |_, e| locked |= *e & LOCKED != 0);
        if locked {
            return Err(aurora_abi::err::EINVAL);
        }
        let mut lazy_pages = Vec::new();
        let mut other = Vec::new();
        self.each_entry(va, end, &mut |page, e| {
            if is_swap(*e) {
                lazy_pages.push(page);
            } else if *e & COPY_ON_WRITE == 0 || *e & FORK_WRITE != 0 {
                (if *e & SHARED == 0 || *e & FORK_WRITE != 0 { &mut lazy_pages } else { &mut other }).push(page);
            }
        });
        let mut freed = Vec::new();
        for page in lazy_pages {
            if self.lazy_at(page).is_none() {
                other.push(page);
                continue;
            }
            if let Some(e) = walk(self.pml4, page, false, false) {
                if is_swap(*e) {
                    swap::drop_ref(slot_of(*e));
                    self.swapped = self.swapped.saturating_sub(1);
                    *e = 0;
                    continue;
                }
                freed.push(*e);
                if *e & SHARED == 0 {
                    self.resident = self.resident.saturating_sub(PAGE_SIZE);
                }
                *e = 0;
            }
        }
        if !freed.is_empty() {
            if Cr3::read().0.start_address().as_u64() == self.pml4 {
                x86_64::instructions::tlb::flush_all();
            }
            shootdown(va, pages, Some(self.pml4));
            for e in freed {
                if e & SHARED == 0 {
                    frame::free(e & ADDR);
                } else {
                    Self::release_shared(e);
                }
            }
        }
        let zero = [0u8; PAGE_SIZE as usize];
        for page in other {
            let _ = self.write_bytes(page, &zero);
        }
        Ok(())
    }

    // ------------------------------------------------------------- reclaim

    /// Calls `f` with each present page from `start` up, in order, until
    /// it returns false; returns the address after the last page it saw
    /// (`USER_END` if it saw them all).
    fn scan_present(&self, start: u64, f: &mut dyn FnMut(u64, &mut u64) -> bool) -> u64 {
        fn visit(t: u64, level: u32, base: u64, start: u64, f: &mut dyn FnMut(u64, &mut u64) -> bool) -> Option<u64> {
            let span = 1u64 << (12 + 9 * level);
            let limit = if level == 3 { 256 } else { 512 };
            let first = (start.saturating_sub(base) / span) as usize;
            for i in first..limit {
                let lo = base + i as u64 * span;
                let e = &mut table(t)[i];
                if *e & PRESENT == 0 || (level > 0 && *e & HUGE != 0) {
                    continue;
                }
                if level == 0 {
                    if !f(lo, e) {
                        return Some(lo + PAGE_SIZE);
                    }
                } else if let Some(stop) = visit(*e & ADDR, level - 1, lo, start, f) {
                    return Some(stop);
                }
            }
            None
        }
        visit(self.pml4, 3, 0, start, f).unwrap_or(layout::USER_END)
    }

    /// Whether the reclaim scan may take the page `e` maps out of memory
    /// at all: a user page, not locked, private or counted.
    fn swappable(e: u64) -> bool {
        e & (PRESENT | USER) == PRESENT | USER && e & LOCKED == 0 && (e & SHARED == 0 || e & COUNTED != 0)
    }

    /// The reclaim scan (`kswapd`, holding this address space's lock):
    /// looks at up to `budget` pages from where the last scan stopped
    /// (going round), and swaps out up to `want` of them. A page used since
    /// it was last looked at is spared this time (its accessed bit is
    /// cleared: a second chance); locked pages and frames an object or a
    /// futex keeps in memory always are.
    pub fn reclaim(&mut self, budget: u64, want: u64) -> Reclaimed {
        /// One batch of the scan (on the stack: nothing here allocates,
        /// since memory is short).
        struct Look {
            found: [u64; BATCH],
            n: usize,
            scanned: u64,
            budget: u64,
            want: usize,
            disk_room: bool,
        }
        impl Look {
            fn more(&self) -> bool {
                self.scanned < self.budget && self.n < self.want
            }
            fn see(&mut self, va: u64, e: &mut u64, end: u64) -> bool {
                if va >= end {
                    return false;
                }
                if AddressSpace::swappable(*e) {
                    self.scanned += 1;
                    let a = atomic(e);
                    let now = a.load(Ordering::Relaxed);
                    if now & ACCESSED != 0 {
                        a.fetch_and(!ACCESSED, Ordering::Relaxed);
                    } else if now & INCOMPRESSIBLE != 0 && now & DIRTY != 0 {
                        // Written since it last failed to compress: it may now.
                        a.fetch_and(!INCOMPRESSIBLE, Ordering::Relaxed);
                    } else if now & INCOMPRESSIBLE == 0 || self.disk_room {
                        self.found[self.n] = va;
                        self.n += 1;
                    }
                }
                self.more()
            }
        }
        let disk_room = swap::disk_has_room();
        let mut out = Reclaimed::default();
        while out.evicted < want && out.scanned < budget && !out.full {
            let mut l = Look {
                found: [0; BATCH],
                n: 0,
                scanned: 0,
                budget: budget - out.scanned,
                want: (want - out.evicted).min(BATCH as u64) as usize,
                disk_room,
            };
            let begin = self.cursor;
            let mut stop = self.scan_present(begin, &mut |va, e| l.see(va, e, layout::USER_END));
            if stop >= layout::USER_END && l.more() {
                // Round again from the start, as far as where this batch began.
                stop = self.scan_present(0, &mut |va, e| l.see(va, e, begin)).min(begin);
            }
            self.cursor = if stop >= layout::USER_END { 0 } else { stop };
            out.scanned += l.scanned;
            if l.n == 0 {
                break;
            }
            self.evict(&l.found[..l.n], &mut out);
        }
        out
    }

    /// Swaps out the pages at `vas` (present and swappable when chosen;
    /// skipped if used since).
    fn evict(&mut self, vas: &[u64], out: &mut Reclaimed) {
        // 1. Each entry takes a slot instead of its frame.
        let mut taken = [(0u64, 0u64, 0u32); BATCH];
        let mut n = 0;
        for &va in vas.iter().take(BATCH) {
            let Some(e) = walk(self.pml4, va, false, false) else { continue };
            let old = *e;
            if !Self::swappable(old) || old & ACCESSED != 0 {
                continue;
            }
            let exec = old & NO_EXECUTE == 0;
            let slot = if old & SHARED == 0 {
                let Some(s) = swap::begin_private() else {
                    out.full = true;
                    break;
                };
                s
            } else {
                match swap::begin_counted(old & ADDR, old & COPY_ON_WRITE == 0) {
                    Some(s) => s,
                    None => continue,
                }
            };
            let new = swap_entry(slot, old & (WRITABLE | FORK_WRITE) != 0, exec);
            // The CPU may have just marked it accessed (or written): then it stays.
            if atomic(e).compare_exchange(old, new, Ordering::AcqRel, Ordering::Relaxed).is_err() {
                if old & SHARED == 0 {
                    swap::abandon(slot);
                } else {
                    swap::undo_counted(slot);
                }
                continue;
            }
            taken[n] = (va, old, slot);
            n += 1;
        }
        let taken = &taken[..n];
        if taken.is_empty() {
            return;
        }
        // 2. No CPU may reach the frames through what it remembers of the
        // entries before their contents are read.
        if Cr3::read().0.start_address().as_u64() == self.pml4 {
            x86_64::instructions::tlb::flush_all();
        }
        let lo = taken.iter().map(|t| t.0).min().unwrap();
        let hi = taken.iter().map(|t| t.0).max().unwrap();
        shootdown(lo, (hi - lo) / PAGE_SIZE + 1, Some(self.pml4));
        // 3. Private pages are stored now, in memory where they compress
        // (or are one repeated word), the rest to the swap file together;
        // shared ones stay cached in their slot until nobody maps them
        // (`swap::writeback`).
        let mut disk = [0usize; BATCH];
        let mut nd = 0;
        for (i, &(_, old, slot)) in taken.iter().enumerate() {
            let f = old & ADDR;
            if old & SHARED != 0 {
                swap::finish_counted(f);
            } else if let Some(stored) = swap::store_in_memory(f) {
                swap::set_out(slot, stored);
                frame::free(f);
                self.resident = self.resident.saturating_sub(PAGE_SIZE);
            } else {
                disk[nd] = i;
                nd += 1;
                continue;
            }
            self.swapped += 1;
            out.evicted += 1;
        }
        for run in disk[..nd].chunks(swapfile::RUN) {
            let pages: [&[u8]; swapfile::RUN] = core::array::from_fn(|k| {
                let f = taken[run[k.min(run.len() - 1)]].1 & ADDR;
                unsafe { core::slice::from_raw_parts(phys_to_virt(f) as *const u8, PAGE_SIZE as usize) }
            });
            // Together if there's room in a row; otherwise one by one.
            let together = swapfile::write_run(&pages[..run.len()]);
            for (k, &i) in run.iter().enumerate() {
                let (va, old, slot) = taken[i];
                let written = match together {
                    Some((at, nonce)) => Some((at + k as u32, nonce + k as u64)),
                    None => swapfile::write(pages[k]),
                };
                if let Some((at, nonce)) = written {
                    swap::set_out(slot, swap::Stored::Disk { at, nonce });
                    frame::free(old & ADDR);
                    self.resident = self.resident.saturating_sub(PAGE_SIZE);
                    self.swapped += 1;
                    out.evicted += 1;
                } else {
                    // No room for it: it stays, not to be tried again until written.
                    swap::abandon(slot);
                    *walk(self.pml4, va, false, false).unwrap() = (old | ACCESSED | INCOMPRESSIBLE) & !DIRTY;
                    out.failed += 1;
                    if !swap::has_room() {
                        out.full = true;
                    }
                }
            }
        }
    }

    /// Linux `MADV_PAGEOUT`: swaps out the pages of `[va, va + pages*4K)`
    /// now, used lately or not; returns how many went.
    pub fn pageout(&mut self, va: u64, pages: u64) -> u64 {
        let end = va.saturating_add(pages.saturating_mul(PAGE_SIZE)).min(layout::USER_END);
        let mut out = Reclaimed::default();
        let mut at = va;
        while at < end && !out.full {
            let (mut found, mut n) = ([0u64; BATCH], 0);
            at = self.scan_present(at, &mut |page, e| {
                if page >= end {
                    return false;
                }
                if Self::swappable(*e) {
                    atomic(e).fetch_and(!ACCESSED, Ordering::Relaxed);
                    found[n] = page;
                    n += 1;
                }
                n < BATCH
            });
            if n == 0 {
                break;
            }
            self.evict(&found[..n], &mut out);
        }
        out.evicted
    }

    /// Linux `MADV_WILLNEED`: brings back the swapped-out pages of
    /// `[va, va + pages*4K)`, as far as memory allows.
    pub fn swap_in_range(&mut self, va: u64, pages: u64) {
        let end = va.saturating_add(pages.saturating_mul(PAGE_SIZE));
        let mut found = Vec::new();
        self.each_entry(va, end, &mut |page, e| {
            if is_swap(*e) {
                found.push(page);
            }
        });
        for page in found {
            if self.fault(page, false) != Fault::Resolved {
                break;
            }
        }
    }

    /// Linux `MADV_COLD`: the pages of the range count as unused, so they
    /// are the first to go when memory runs short.
    pub fn cold(&mut self, va: u64, pages: u64) {
        let end = va.saturating_add(pages.saturating_mul(PAGE_SIZE));
        self.each_present(va, end, &mut |_, e| {
            atomic(e).fetch_and(!ACCESSED, Ordering::Relaxed);
        });
    }

    /// Linux `mlock` (`lock`) and `munlock`: the pages of the range are
    /// brought in (made, or swapped in) and kept in memory, or no longer
    /// kept. `ENOMEM` if part of the range isn't mapped, or memory is out.
    pub fn mlock(&mut self, va: u64, pages: u64, lock: bool) -> Result<(), isize> {
        use aurora_abi::err::*;
        let end = va.checked_add(pages.checked_mul(PAGE_SIZE).ok_or(ENOMEM)?).ok_or(ENOMEM)?;
        if !va.is_multiple_of(PAGE_SIZE) || end > layout::USER_END {
            return Err(EINVAL);
        }
        if lock {
            let mut page = va;
            while page < end {
                match self.fault(page, false) {
                    Fault::Resolved => {}
                    Fault::NoMemory => return Err(EAGAIN),
                    Fault::Invalid => return Err(ENOMEM),
                }
                page += PAGE_SIZE;
            }
        }
        self.each_present(va, end, &mut |_, e| {
            if *e & USER != 0 {
                let a = atomic(e);
                if lock {
                    a.fetch_or(LOCKED, Ordering::Relaxed);
                } else {
                    a.fetch_and(!LOCKED, Ordering::Relaxed);
                }
            }
        });
        Ok(())
    }

    /// Unlocks every page (Linux `munlockall`), or locks every page there
    /// is now (`mlockall(MCL_CURRENT)`), bringing swapped-out ones back.
    pub fn mlock_all(&mut self, lock: bool) -> Result<(), isize> {
        if lock {
            let mut swapped = Vec::new();
            self.each_entry(0, layout::USER_END, &mut |va, e| {
                if is_swap(*e) {
                    swapped.push(va);
                }
            });
            for va in swapped {
                if self.fault(va, false) == Fault::NoMemory {
                    return Err(aurora_abi::err::EAGAIN);
                }
            }
        }
        // Only what is there now: lazy memory not touched yet stays lazy.
        self.each_present(0, layout::USER_END, &mut |_, e| {
            if *e & USER != 0 {
                let a = atomic(e);
                if lock {
                    a.fetch_or(LOCKED, Ordering::Relaxed);
                } else {
                    a.fetch_and(!LOCKED, Ordering::Relaxed);
                }
            }
        });
        Ok(())
    }

    /// The address space's memory, counted page by page: (private pages
    /// present, shared pages mapped, pages swapped out, pages locked).
    pub fn page_counts(&self) -> PageCounts {
        let mut c = PageCounts::default();
        self.each_entry(0, layout::USER_END, &mut |_, e| {
            if is_swap(*e) {
                c.swapped += 1;
            } else if *e & USER != 0 {
                if *e & SHARED == 0 {
                    c.private += 1;
                } else {
                    c.shared += 1;
                }
                if *e & LOCKED != 0 {
                    c.locked += 1;
                }
            }
        });
        c
    }

    /// Whether the page at `va` is locked in memory (tests).
    #[cfg(feature = "ktest")]
    pub fn is_locked(&self, va: u64) -> bool {
        walk(self.pml4, va & !(PAGE_SIZE - 1), false, false)
            .is_some_and(|e| *e & (PRESENT | LOCKED) == PRESENT | LOCKED)
    }

    /// Reads user memory at `va` into `out` through the physical window,
    /// as the program would (bringing pages in) (tests).
    #[cfg(feature = "ktest")]
    pub fn read_bytes(&mut self, va: u64, out: &mut [u8]) -> Result<(), isize> {
        let mut done = 0usize;
        while done < out.len() {
            let cur = va + done as u64;
            if walk(self.pml4, cur, false, false).is_none_or(|e| *e & PRESENT == 0) && !self.resolve_fault(cur, false) {
                return Err(aurora_abi::err::EFAULT);
            }
            let (phys, _) = self.user_page(cur).ok_or(aurora_abi::err::EFAULT)?;
            let off = cur % PAGE_SIZE;
            let n = ((PAGE_SIZE - off) as usize).min(out.len() - done);
            unsafe {
                core::ptr::copy_nonoverlapping((phys_to_virt(phys) + off) as *const u8, out[done..].as_mut_ptr(), n);
            }
            done += n;
        }
        Ok(())
    }

    /// Whether the page at `va` is swapped out (compressed or on disk).
    pub fn is_swapped(&self, va: u64) -> bool {
        walk(self.pml4, va & !(PAGE_SIZE - 1), false, false).is_some_and(|e| is_swap(*e))
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

    /// Frames of this address space's own page tables (the top level included).
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))] // memory reports in self-tests
    pub fn table_frames(&self) -> u64 {
        fn count(t: u64, level: u32) -> u64 {
            let limit = if level == 3 { 256 } else { 512 };
            let mut n = 1;
            if level > 0 {
                for &e in table(t).iter().take(limit) {
                    if e & PRESENT != 0 && e & HUGE == 0 {
                        n += count(e & ADDR, level - 1);
                    }
                }
            }
            n
        }
        // Level-0 tables count themselves; their entries are pages, not tables.
        count(self.pml4, 3)
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
                        if e & (PRESENT | USER) != (PRESENT | USER) && !is_swap(e) {
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
        // Lazy memory is mapped too, touched or not.
        for (&start, &(end, prot)) in &self.lazy {
            let f = Self::flags(prot);
            out.push((start, end, f & WRITABLE != 0, f & NO_EXECUTE == 0));
        }
        out.sort_unstable_by_key(|r| r.0);
        let mut merged: Vec<(u64, u64, bool, bool)> = Vec::new();
        for r in out {
            match merged.last_mut() {
                Some(last) if r.0 <= last.1 && (last.2, last.3) == (r.2, r.3) => last.1 = last.1.max(r.1),
                _ => merged.push(r),
            }
        }
        merged
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
            if walk(self.pml4, cur, false, false).is_some_and(|e| is_swap(*e))
                && self.swap_in(cur & !(PAGE_SIZE - 1), *walk(self.pml4, cur, false, false).unwrap()) != Fault::Resolved
            {
                return Err(aurora_abi::err::EFAULT);
            }
            if walk(self.pml4, cur, false, false).is_some_and(|e| *e & COPY_ON_WRITE != 0) && !self.resolve_write(cur) {
                return Err(aurora_abi::err::EFAULT);
            }
            if walk(self.pml4, cur, false, false).is_none_or(|e| *e & PRESENT == 0) {
                self.resolve_fault(cur, true);
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
            if level == 0 && is_swap(*e) {
                swap::drop_ref(slot_of(*e));
                continue;
            }
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
