//! Swap: where pages of programs go when memory runs short, and the
//! bookkeeping of frames more than one holder shares.
//!
//! **Slots.** A page taken out of memory gets a *slot*: a number its page
//! table entries hold instead of a frame (see `vmm`, `SWAPPED`), and a
//! count of those entries. A slot's page is in one of these states:
//! - *cached*: still in a frame (shared pages, see below), which the slot
//!   holds;
//! - *zero* or *filled* with one repeated 64-bit word (no storage at all);
//! - *compressed* (LZ4, `aurora_lz4`) in the compressed pool, up to a
//!   limit ([`set_limits`]; a quarter of memory by default) — the zram of
//!   WaveOS;
//! - on *disk*, in the swap file (`mm::swapfile`), encrypted;
//! - *busy*: moving between those (whoever wants it waits).
//!
//! **Private pages** (a program's own, held by one page table entry) are
//! stored at once when they are taken out. **Shared pages** — counted
//! frames: shared memory, pages shared since `fork`, mapped files — have
//! several entries mapping them, in several address spaces. Each entry is
//! swapped out on its own as the scan reaches it: it takes the frame's
//! slot (made the first time, *caching* the frame) and lets go of the
//! frame. A fault on such an entry maps the cached frame again, so every
//! process keeps seeing the same page. Only once the slot is the frame's
//! last holder is the frame written out and freed ([`writeback`]). This
//! is the "swap cache" of other systems; it finds every address space
//! mapping a page without a reverse map, because each one lets go of the
//! page itself.
//!
//! Frames held by an object (a `memfd` or `/dev/shm` file that is still
//! open, the mapped-file cache, the vDSO) stay in memory: their entries
//! may still be swapped out, but the frame stays until the object lets go.
//! Frames with a futex waiter keyed by their address are *pinned* so the
//! key stays valid.
//!
//! **Memory of its own.** Swap works when memory is short and broken up,
//! so it never asks the heap for more than small objects: the slot table
//! and the compressed pool are made of single frames (taken from the
//! kernel's reserve, a frame per 100-odd slots, or per few compressed
//! pages, each of which frees a frame), linked through the physical
//! window; free slots are a list through the table itself.
//!
//! Everything here is under one lock ([`PAGES`]), taken with interrupts
//! off and never held while compressing or doing I/O: a slot is marked
//! busy instead.

use super::{frame, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use alloc::collections::{BTreeMap, VecDeque};
use core::sync::atomic::{AtomicU64, Ordering};

/// No slot.
pub const NONE: u32 = u32::MAX;
/// A page compressed to more than this goes to disk (or stays).
const MAX_COMPRESSED: usize = 3 * PAGE_SIZE as usize / 4;

/// A frame with more than one possible holder.
pub struct Counted {
    /// Holders: page table entries mapping it, objects, and the slot caching it.
    pub holders: u32,
    /// Of those, objects (a shared memory file, the mapped-file cache, the
    /// vDSO): while any holds it, the frame stays in memory.
    pub objects: u32,
    /// Futex waiters keyed by its physical address: it stays where it is.
    pub pins: u32,
    /// The slot caching it, or [`NONE`].
    pub slot: u32,
}

/// Where a page is kept while it is out of memory.
pub enum Stored {
    Zero,
    Fill(u64),
    /// An object of the compressed pool, and its length.
    Zram { obj: u64, len: u16 },
    Disk { at: u32, nonce: u64 },
}

enum State {
    /// On the free list, before slot `next` ([`NONE`]: the end).
    Free(u32),
    Busy,
    Cached(u64),
    Out(Stored),
}

struct Slot {
    refs: u32,
    /// Shared memory (mapped writable by all), not copy-on-write.
    shared: bool,
    /// On the write-back queue.
    queued: bool,
    state: State,
}

/// Slots per frame of the slot table.
const PER_FRAME: usize = PAGE_SIZE as usize / core::mem::size_of::<Slot>();
/// Frames of table each directory frame lists, and directory frames: room
/// for 512 × 512 × `PER_FRAME` slots (over 100 GiB of swap).
const PER_DIR: usize = 512;
const DIRS: usize = 512;

// ------------------------------------------------------------ compressed pool

/// Pool frames start with this header; objects of one size class follow.
#[repr(C)]
struct PoolHead {
    /// The class's frames with room, doubly linked (physical addresses; 0 ends).
    next: u64,
    prev: u64,
    class: u32,
    used: u32,
    /// Objects in use.
    map: u64,
}
const POOL_HEAD: usize = 32;
/// Size classes: multiples of 64 bytes up to [`MAX_COMPRESSED`].
const POOL_STEP: usize = 64;
const POOL_CLASSES: usize = MAX_COMPRESSED / POOL_STEP;

fn pool_size(class: usize) -> usize {
    (class + 1) * POOL_STEP
}

fn pool_objects(class: usize) -> u32 {
    ((PAGE_SIZE as usize - POOL_HEAD) / pool_size(class)).min(64) as u32
}

fn pool_head(f: u64) -> &'static mut PoolHead {
    unsafe { &mut *(phys_to_virt(f) as *mut PoolHead) }
}

/// Where object `obj` (a frame and an index) lies.
fn pool_ptr(obj: u64) -> *mut u8 {
    let f = obj & !(PAGE_SIZE - 1);
    let class = pool_head(f).class as usize;
    (phys_to_virt(f) as usize + POOL_HEAD + (obj & (PAGE_SIZE - 1)) as usize * pool_size(class)) as *mut u8
}

pub struct Pages {
    pub counted: BTreeMap<u64, Counted>,
    /// Directory frames (0: not made yet).
    dir: [u64; DIRS],
    /// Frames of slot table made.
    tables: u32,
    free_head: u32,
    /// Slots that cache a frame and may be written back.
    queue: VecDeque<u32>,
    /// Slots in use (any state but free).
    used: u64,
    /// Per size class, the first pool frame with room (0: none).
    pool: [u64; POOL_CLASSES],
    pool_frames: u64,
    zram_bytes: u64,
    zram_pages: u64,
    same_pages: u64,
    disk_pages: u64,
    cached_pages: u64,
}

/// The counted frames and the slots (see the module comment).
pub static PAGES: IrqMutex<Pages> = IrqMutex::new(Pages {
    counted: BTreeMap::new(),
    dir: [0; DIRS],
    tables: 0,
    free_head: NONE,
    queue: VecDeque::new(),
    used: 0,
    pool: [0; POOL_CLASSES],
    pool_frames: 0,
    zram_bytes: 0,
    zram_pages: 0,
    same_pages: 0,
    disk_pages: 0,
    cached_pages: 0,
});

/// Pages the compressed store may hold (two per frame of memory by
/// default) and the bytes of memory it may take (a quarter of memory).
static ZRAM_SLOTS: AtomicU64 = AtomicU64::new(0);
static ZRAM_LIMIT: AtomicU64 = AtomicU64::new(0);

static SWAPOUTS: AtomicU64 = AtomicU64::new(0);
static SWAPINS: AtomicU64 = AtomicU64::new(0);
static WRITEBACKS: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
/// Swap-in time by source: (count, total ns) for compressed and disk pages.
static IN_ZRAM: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
static IN_DISK: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

/// Sets the default limits from the size of memory (at boot).
pub fn init() {
    let total = frame::counts().0;
    ZRAM_SLOTS.store(2 * total, Ordering::Relaxed);
    ZRAM_LIMIT.store(total * PAGE_SIZE / 4, Ordering::Relaxed);
}

/// Changes how many pages and bytes the compressed store may hold (tests).
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn set_limits(pages: u64, bytes: u64) {
    ZRAM_SLOTS.store(pages, Ordering::Relaxed);
    ZRAM_LIMIT.store(bytes, Ordering::Relaxed);
}

/// At most this many slots, whatever the stores could hold (tests: swap
/// that fills quickly).
static CAPACITY_CAP: AtomicU64 = AtomicU64::new(u64::MAX);

#[cfg(feature = "ktest")]
pub fn set_capacity_cap(cap: Option<u64>) {
    CAPACITY_CAP.store(cap.unwrap_or(u64::MAX), Ordering::Relaxed);
}

/// (pages, bytes) the compressed store may hold.
pub fn limits() -> (u64, u64) {
    (ZRAM_SLOTS.load(Ordering::Relaxed), ZRAM_LIMIT.load(Ordering::Relaxed))
}

impl Pages {
    fn valid(&self, s: u32) -> bool {
        (s as usize) < self.tables as usize * PER_FRAME
    }

    fn slot(&mut self, s: u32) -> &mut Slot {
        let (t, i) = (s as usize / PER_FRAME, s as usize % PER_FRAME);
        let dir = self.dir[t / PER_DIR];
        // SAFETY: table frames hold initialized slots and live as long as
        // the kernel; `s` is below the slots made (callers check `valid`
        // or got `s` from `alloc`).
        unsafe {
            let table = *(phys_to_virt(dir) as *const u64).add(t % PER_DIR);
            &mut *(phys_to_virt(table) as *mut Slot).add(i)
        }
    }

    /// Slots that may be in use: as many as a quarter of the kernel's
    /// reserve holds in table frames (a 40-byte slot per page swapped out,
    /// zero pages included, which need no other room). The stores limit
    /// what they hold themselves.
    fn capacity(&self) -> u64 {
        (frame::kernel_reserve() / 4 * PER_FRAME as u64).min(CAPACITY_CAP.load(Ordering::Relaxed))
    }

    /// Whether the compressed store could take another page.
    fn zram_room(&self) -> bool {
        self.zram_pages < ZRAM_SLOTS.load(Ordering::Relaxed)
            && (self.pool_frames + 1) * PAGE_SIZE <= ZRAM_LIMIT.load(Ordering::Relaxed)
    }

    /// A frame more of slot table, its slots on the free list.
    fn grow(&mut self) -> Option<()> {
        let t = self.tables as usize;
        if t >= DIRS * PER_DIR {
            return None;
        }
        if self.dir[t / PER_DIR] == 0 {
            let d = frame::alloc()?;
            unsafe { core::ptr::write_bytes(phys_to_virt(d) as *mut u8, 0, PAGE_SIZE as usize) };
            self.dir[t / PER_DIR] = d;
        }
        let f = frame::alloc()?;
        let base = (t * PER_FRAME) as u32;
        let slots = phys_to_virt(f) as *mut Slot;
        for i in 0..PER_FRAME {
            let next = if i + 1 < PER_FRAME { base + i as u32 + 1 } else { self.free_head };
            let slot = Slot { refs: 0, shared: false, queued: false, state: State::Free(next) };
            unsafe { slots.add(i).write(slot) };
        }
        unsafe { *(phys_to_virt(self.dir[t / PER_DIR]) as *mut u64).add(t % PER_DIR) = f };
        self.tables += 1;
        self.free_head = base;
        Some(())
    }

    fn alloc(&mut self, shared: bool, refs: u32, state: State) -> Option<u32> {
        if self.used >= self.capacity() {
            return None;
        }
        if self.free_head == NONE {
            self.grow()?;
        }
        let s = self.free_head;
        let State::Free(next) = self.slot(s).state else { unreachable!("swap: slot {s} on the free list in use") };
        self.free_head = next;
        self.used += 1;
        *self.slot(s) = Slot { refs, shared, queued: false, state };
        Some(s)
    }

    /// Frees slot `s` (no references left, not busy). Returns the frame it
    /// cached, whose hold the caller must give back ([`Pages::drop_hold`]).
    fn release_slot(&mut self, s: u32) -> Option<u64> {
        let head = self.free_head;
        let slot = self.slot(s);
        let state = core::mem::replace(&mut slot.state, State::Free(head));
        slot.refs = 0;
        self.free_head = s;
        self.used -= 1;
        match state {
            State::Cached(f) => {
                self.cached_pages -= 1;
                if let Some(c) = self.counted.get_mut(&f) {
                    c.slot = NONE;
                }
                Some(f)
            }
            State::Out(stored) => {
                self.forget(stored);
                None
            }
            State::Free(_) | State::Busy => None,
        }
    }

    /// Lets go of stored contents.
    fn forget(&mut self, stored: Stored) {
        self.forget_counts(&stored);
        self.discard(stored);
    }

    /// Frees stored contents, wiped: the pool's memory is the kernel's
    /// again, and the disk's is encrypted with a key that dies with the boot.
    fn discard(&mut self, stored: Stored) {
        match stored {
            Stored::Zero | Stored::Fill(_) => {}
            Stored::Zram { obj, .. } => self.pool_free(obj),
            Stored::Disk { at, .. } => super::swapfile::free(at),
        }
    }

    fn note_stored(&mut self, stored: &Stored) {
        match stored {
            Stored::Zero | Stored::Fill(_) => self.same_pages += 1,
            Stored::Zram { len, .. } => {
                self.zram_pages += 1;
                self.zram_bytes += *len as u64;
            }
            Stored::Disk { .. } => self.disk_pages += 1,
        }
    }

    /// The counts of what is stored, before it is taken out of a slot.
    fn forget_counts(&mut self, stored: &Stored) {
        match stored {
            Stored::Zero | Stored::Fill(_) => self.same_pages -= 1,
            Stored::Zram { len, .. } => {
                self.zram_pages -= 1;
                self.zram_bytes -= *len as u64;
            }
            Stored::Disk { .. } => self.disk_pages -= 1,
        }
    }

    /// One holder lets go of counted frame `f`: true if that was the last
    /// (the caller frees the frame). A cached frame left with only its
    /// slot holding it goes on the write-back queue.
    fn drop_hold(&mut self, f: u64) -> bool {
        let Some(c) = self.counted.get_mut(&f) else { return false };
        c.holders = c.holders.saturating_sub(1);
        if c.holders == 0 {
            self.counted.remove(&f);
            return true;
        }
        let (holders, pins, slot) = (c.holders, c.pins, c.slot);
        if holders == 1 && pins == 0 && slot != NONE {
            self.enqueue(slot);
        }
        false
    }

    /// Puts slot `s` on the write-back queue (if there is room for it).
    fn enqueue(&mut self, s: u32) {
        if !self.slot(s).queued && self.queue.try_reserve(1).is_ok() {
            self.slot(s).queued = true;
            self.queue.push_back(s);
        }
    }

    fn set_cached(&mut self, s: u32, f: u64) {
        self.slot(s).state = State::Cached(f);
        self.cached_pages += 1;
    }

    /// Room for `len` compressed bytes in the pool, if the limit allows.
    fn pool_alloc(&mut self, len: usize) -> Option<u64> {
        let class = len.max(1).div_ceil(POOL_STEP) - 1;
        if self.pool[class] == 0 {
            if (self.pool_frames + 1) * PAGE_SIZE > ZRAM_LIMIT.load(Ordering::Relaxed) {
                return None;
            }
            let f = frame::alloc()?;
            *pool_head(f) = PoolHead { next: 0, prev: 0, class: class as u32, used: 0, map: 0 };
            self.pool[class] = f;
            self.pool_frames += 1;
        }
        let f = self.pool[class];
        let h = pool_head(f);
        let i = (!h.map).trailing_zeros() as u64;
        h.map |= 1 << i;
        h.used += 1;
        if h.used == pool_objects(class) {
            // Full: off the list of frames with room.
            self.pool[class] = h.next;
            if h.next != 0 {
                pool_head(h.next).prev = 0;
            }
            h.next = 0;
        }
        Some(f | i)
    }

    /// Gives back pool object `obj`, wiped; its frame goes once empty.
    fn pool_free(&mut self, obj: u64) {
        let f = obj & !(PAGE_SIZE - 1);
        let i = obj & (PAGE_SIZE - 1);
        let h = pool_head(f);
        let class = h.class as usize;
        unsafe { core::ptr::write_bytes(pool_ptr(obj), 0, pool_size(class)) };
        let was_full = h.used == pool_objects(class);
        h.map &= !(1 << i);
        h.used -= 1;
        let unlink = |pool: &mut [u64; POOL_CLASSES], h: &mut PoolHead| {
            if h.prev != 0 {
                pool_head(h.prev).next = h.next;
            } else {
                pool[class] = h.next;
            }
            if h.next != 0 {
                pool_head(h.next).prev = h.prev;
            }
        };
        if h.used == 0 {
            if !was_full {
                unlink(&mut self.pool, h);
            }
            self.pool_frames -= 1;
            frame::free(f);
        } else if was_full {
            h.prev = 0;
            h.next = self.pool[class];
            if h.next != 0 {
                pool_head(h.next).prev = f;
            }
            self.pool[class] = f;
        }
    }
}

// ------------------------------------------------------------ counted frames

/// Starts counting frame `f` with `holders` holders, `objects` of them objects.
pub fn count(f: u64, holders: u32, objects: u32) {
    PAGES.lock().counted.insert(f, Counted { holders, objects, pins: 0, slot: NONE });
}

/// Another holder for counted frame `f` (an object if `object`). False if
/// it isn't counted.
pub fn hold(f: u64, object: bool) -> bool {
    match PAGES.lock().counted.get_mut(&f) {
        Some(c) => {
            c.holders += 1;
            c.objects += object as u32;
            true
        }
        None => false,
    }
}

/// Counts private frames as held by `holders` page table entries each (`fork`).
pub fn adopt(frames: &[u64], holders: u32) {
    let mut p = PAGES.lock();
    for &f in frames {
        p.counted.entry(f).or_insert(Counted { holders: 0, objects: 0, pins: 0, slot: NONE }).holders += holders;
    }
}


/// A holder (an object if `object`) lets go of `f`; the last one frees it.
pub fn release(f: u64, object: bool) {
    let last = {
        let mut p = PAGES.lock();
        if object {
            if let Some(c) = p.counted.get_mut(&f) {
                c.objects = c.objects.saturating_sub(1);
            }
        }
        p.drop_hold(f)
    };
    if last {
        frame::free(f);
    }
}

/// A page table entry claims counted frame `f` as its own, if it is the
/// only holder (a write to a page shared since `fork`, once the others let
/// go). True if it did: `f` is a private frame now.
pub fn claim(f: u64) -> bool {
    let mut p = PAGES.lock();
    if p.counted.get(&f).is_some_and(|c| c.holders == 1 && c.slot == NONE) {
        p.counted.remove(&f);
        return true;
    }
    false
}

/// Keeps counted frame `f` where it is (a futex keyed by its address waits on it).
pub fn pin(f: u64) -> bool {
    match PAGES.lock().counted.get_mut(&f) {
        Some(c) => {
            c.pins += 1;
            true
        }
        None => false,
    }
}

pub fn unpin(f: u64) {
    let mut p = PAGES.lock();
    let Some(c) = p.counted.get_mut(&f) else { return };
    c.pins = c.pins.saturating_sub(1);
    // Left only in its slot's hands meanwhile: it may be written back now.
    let (pins, holders, slot) = (c.pins, c.holders, c.slot);
    if pins == 0 && holders == 1 && slot != NONE {
        p.enqueue(slot);
    }
}

// ------------------------------------------------------------- swapping out

/// A private page is being taken out: a busy slot holding its one
/// reference, or None if swap is full.
pub fn begin_private() -> Option<u32> {
    let s = PAGES.lock().alloc(false, 1, State::Busy);
    if s.is_none() {
        REFUSED.fetch_add(1, Ordering::Relaxed);
    }
    s
}

/// Gives up a slot from [`begin_private`] (the page stays).
pub fn abandon(s: u32) {
    PAGES.lock().release_slot(s);
}

/// Busy slot `s` (from [`begin_private`]) holds its page in `stored` now.
pub fn set_out(s: u32, stored: Stored) {
    let mut p = PAGES.lock();
    p.note_stored(&stored);
    p.slot(s).state = State::Out(stored);
    SWAPOUTS.fetch_add(1, Ordering::Relaxed);
}

/// A page table entry mapping counted frame `f` is being taken out: the
/// frame's slot (made now, caching the frame, if it has none) with one
/// more reference, for the entry to hold instead. None if the frame must
/// stay in memory (an object holds it, a futex is keyed by it) or swap is
/// full. `shared`: shared memory rather than copy-on-write.
pub fn begin_counted(f: u64, shared: bool) -> Option<u32> {
    let mut p = PAGES.lock();
    let c = p.counted.get(&f)?;
    if c.objects > 0 || c.pins > 0 {
        return None;
    }
    let mut s = c.slot;
    if s == NONE {
        s = p.alloc(shared, 0, State::Busy)?;
        p.set_cached(s, f);
        let c = p.counted.get_mut(&f).unwrap();
        c.slot = s;
        c.holders += 1;
    }
    p.slot(s).refs += 1;
    Some(s)
}

/// The entry kept its frame after all ([`begin_counted`] undone).
pub fn undo_counted(s: u32) {
    drop_ref(s);
}

/// The entry that took slot `s` instead of counted frame `f` lets go of
/// the frame (once no CPU can reach it through the entry any more). The
/// slot still holds it, so it is never freed here.
pub fn finish_counted(f: u64) {
    let last = PAGES.lock().drop_hold(f);
    debug_assert!(!last, "a swapped-out shared frame lost its cache");
    if last {
        frame::free(f);
    }
    SWAPOUTS.fetch_add(1, Ordering::Relaxed);
}

/// Another page table entry refers to slot `s` (`fork`).
pub fn retain(s: u32) {
    let mut p = PAGES.lock();
    if p.valid(s) {
        p.slot(s).refs += 1;
    }
}

/// A page table entry referring to slot `s` goes (unmapped, or its address
/// space ended).
pub fn drop_ref(s: u32) {
    let free = {
        let mut p = PAGES.lock();
        if !p.valid(s) {
            return;
        }
        let slot = p.slot(s);
        slot.refs = slot.refs.saturating_sub(1);
        // A busy slot is freed by whoever made it busy, when done.
        if slot.refs > 0 || matches!(slot.state, State::Busy | State::Free(_)) {
            return;
        }
        match p.release_slot(s) {
            Some(f) => p.drop_hold(f).then_some(f),
            None => None,
        }
    };
    if let Some(f) = free {
        frame::free(f);
    }
}

/// Whether slot `s` caches its page in a frame (bringing it back takes none).
pub fn is_cached(s: u32) -> bool {
    let mut p = PAGES.lock();
    p.valid(s) && matches!(p.slot(s).state, State::Cached(_))
}

// --------------------------------------------------------------- swapping in

/// What a page table entry that referred to a slot maps now.
pub enum Map {
    /// The address space's own frame.
    Private(u64),
    /// A counted frame (held for the entry): shared memory if `shared`,
    /// otherwise copy-on-write.
    Counted { frame: u64, shared: bool },
}

pub enum In {
    /// Moving: try again shortly.
    Busy,
    Mapped(Map),
    /// The contents, taken out of the store (the slot is busy until
    /// [`finish_in`] or [`abort_in`]).
    Load(Stored),
}

/// A fault on an entry referring to slot `s`. The entry's reference is
/// given up once this returns [`In::Mapped`] (or with [`finish_in`]).
pub fn begin_in(s: u32) -> In {
    let mut p = PAGES.lock();
    if !p.valid(s) {
        return In::Busy;
    }
    let shared = p.slot(s).shared;
    match &p.slot(s).state {
        State::Busy | State::Free(_) => In::Busy,
        &State::Cached(f) => {
            let c = p.counted.get_mut(&f).unwrap();
            c.holders += 1;
            let slot = p.slot(s);
            slot.refs -= 1;
            if slot.refs == 0 {
                p.release_slot(s);
                p.drop_hold(f);
            }
            SWAPINS.fetch_add(1, Ordering::Relaxed);
            In::Mapped(own_if_alone(&mut p, f, shared))
        }
        State::Out(_) => {
            let State::Out(stored) = core::mem::replace(&mut p.slot(s).state, State::Busy) else { unreachable!() };
            p.forget_counts(&stored);
            In::Load(stored)
        }
    }
}

/// A copy-on-write frame nobody else holds becomes private.
fn own_if_alone(p: &mut Pages, f: u64, shared: bool) -> Map {
    if !shared && p.counted.get(&f).is_some_and(|c| c.holders == 1 && c.slot == NONE && c.objects == 0) {
        p.counted.remove(&f);
        return Map::Private(f);
    }
    Map::Counted { frame: f, shared }
}

/// For read-ahead around a fault: if slot `s` is a private page stored on
/// disk at page `at` of the swap file, it becomes busy, to be read along
/// (then [`finish_in`], or [`abort_in`]); returns its nonce.
pub fn begin_readahead(s: u32, at: u32) -> Option<u64> {
    let mut p = PAGES.lock();
    if !p.valid(s) {
        return None;
    }
    let slot = p.slot(s);
    let nonce = match slot.state {
        State::Out(Stored::Disk { at: there, nonce }) if there == at && slot.refs == 1 && !slot.shared => nonce,
        _ => return None,
    };
    slot.state = State::Busy;
    p.disk_pages -= 1;
    Some(nonce)
}

/// Puts back what [`begin_in`] took (loading it failed).
pub fn abort_in(s: u32, stored: Stored) {
    let mut p = PAGES.lock();
    p.note_stored(&stored);
    p.slot(s).state = State::Out(stored);
}

/// Fills frame `f` with what [`begin_in`] took out; false if it could not
/// be read back (a disk error, or a compressed page that doesn't decode).
/// The storage is not given back here.
pub fn load(stored: &Stored, f: u64) -> bool {
    let page = unsafe { core::slice::from_raw_parts_mut(phys_to_virt(f) as *mut u8, PAGE_SIZE as usize) };
    let start = crate::time::now_ns();
    let ok = match stored {
        Stored::Zero => {
            page.fill(0);
            true
        }
        Stored::Fill(v) => {
            for w in page.as_chunks_mut::<8>().0 {
                *w = v.to_le_bytes();
            }
            true
        }
        // (The object is the busy slot's: nobody frees it meanwhile.)
        &Stored::Zram { obj, len } => {
            let data = unsafe { core::slice::from_raw_parts(pool_ptr(obj), len as usize) };
            aurora_lz4::decompress(data, page) == Ok(PAGE_SIZE as usize)
        }
        Stored::Disk { at, nonce } => super::swapfile::read(*at, *nonce, page),
    };
    let took = crate::time::now_ns().saturating_sub(start);
    let counter = match stored {
        Stored::Zram { .. } => Some(&IN_ZRAM),
        Stored::Disk { .. } => Some(&IN_DISK),
        _ => None,
    };
    if let Some(c) = counter {
        c[0].fetch_add(1, Ordering::Relaxed);
        c[1].fetch_add(took, Ordering::Relaxed);
    }
    ok
}

/// Notes `pages` read from the swap file in one request taking `ns`.
pub fn note_disk_in(pages: u64, ns: u64) {
    IN_DISK[0].fetch_add(pages, Ordering::Relaxed);
    IN_DISK[1].fetch_add(ns, Ordering::Relaxed);
}

/// The page of busy slot `s` is in frame `f` now (the stored copy is let
/// go). The entry's reference goes; if others remain, the slot caches the
/// frame for them.
pub fn finish_in(s: u32, f: u64, stored: Stored) -> Map {
    let mut p = PAGES.lock();
    p.discard(stored);
    let shared = p.slot(s).shared;
    p.slot(s).refs -= 1;
    SWAPINS.fetch_add(1, Ordering::Relaxed);
    if p.slot(s).refs == 0 {
        p.release_slot(s);
        if shared {
            p.counted.insert(f, Counted { holders: 1, objects: 0, pins: 0, slot: NONE });
            Map::Counted { frame: f, shared }
        } else {
            Map::Private(f)
        }
    } else {
        // Held by the slot and by this entry.
        p.counted.insert(f, Counted { holders: 2, objects: 0, pins: 0, slot: s });
        p.set_cached(s, f);
        Map::Counted { frame: f, shared }
    }
}

// --------------------------------------------------------------- write-back

/// Writes out up to `max` cached frames that only their slot still holds,
/// freeing them; returns how many. Run by `kswapd`, holding no other lock.
pub fn writeback(max: usize) -> usize {
    let mut done = 0;
    let mut looked = 0;
    while done < max && looked < max * 4 {
        looked += 1;
        // A cached frame nobody but its slot holds becomes busy.
        let (s, f) = {
            let mut p = PAGES.lock();
            let Some(s) = p.queue.pop_front() else { break };
            p.slot(s).queued = false;
            let State::Cached(f) = p.slot(s).state else { continue };
            let Some(c) = p.counted.get(&f) else { continue };
            if c.holders != 1 || c.pins > 0 || c.objects > 0 {
                continue;
            }
            p.counted.remove(&f);
            p.slot(s).state = State::Busy;
            p.cached_pages -= 1;
            (s, f)
        };
        let stored = store(f);
        let mut p = PAGES.lock();
        match stored {
            Some(stored) => {
                p.note_stored(&stored);
                p.slot(s).state = State::Out(stored);
                if p.slot(s).refs == 0 {
                    p.release_slot(s);
                }
                drop(p);
                frame::free(f);
                WRITEBACKS.fetch_add(1, Ordering::Relaxed);
                done += 1;
            }
            None => {
                // No room: it stays cached (tried again when it is queued again).
                p.counted.insert(f, Counted { holders: 1, objects: 0, pins: 0, slot: s });
                p.set_cached(s, f);
                if p.slot(s).refs == 0 {
                    if let Some(f) = p.release_slot(s) {
                        if p.drop_hold(f) {
                            drop(p);
                            frame::free(f);
                        }
                    }
                }
                break;
            }
        }
    }
    done
}

/// Whether cached frames are waiting to be written back.
pub fn writeback_pending() -> bool {
    !PAGES.lock().queue.is_empty()
}

// ----------------------------------------------------------------- storing

/// Scratch for the compressor, shared by everyone storing pages.
struct Scratch {
    table: [u16; aurora_lz4::TABLE],
    out: [u8; MAX_COMPRESSED],
}

static SCRATCH: crate::sync::Mutex<Scratch> =
    crate::sync::Mutex::new(Scratch { table: [0; aurora_lz4::TABLE], out: [0; MAX_COMPRESSED] });

/// Whether a page could be stored now: a slot for it, and room in the
/// compressed store or the swap file.
pub fn has_room() -> bool {
    let p = PAGES.lock();
    p.used < p.capacity() && (p.zram_room() || super::swapfile::free_pages() > 0)
}

/// Whether the swap file has room for pages that don't compress.
pub fn disk_has_room() -> bool {
    super::swapfile::free_pages() > 0
}

/// Stores the contents of frame `f` (which nobody can change meanwhile):
/// in memory if it can ([`store_in_memory`]), otherwise in the swap file.
/// None if nothing had room.
fn store(f: u64) -> Option<Stored> {
    if let Some(stored) = store_in_memory(f) {
        return Some(stored);
    }
    let page = unsafe { core::slice::from_raw_parts(phys_to_virt(f) as *const u8, PAGE_SIZE as usize) };
    let (at, nonce) = super::swapfile::write(page)?;
    Some(Stored::Disk { at, nonce })
}

/// Stores the contents of frame `f` without the swap file: a page of one
/// repeated word as just that word, others compressed if they shrink to
/// 3/4 and the pool has room. None: it must go to disk (or stay).
pub fn store_in_memory(f: u64) -> Option<Stored> {
    let page = unsafe { core::slice::from_raw_parts(phys_to_virt(f) as *const u8, PAGE_SIZE as usize) };
    let first = u64::from_le_bytes(page[..8].try_into().unwrap());
    if page.as_chunks::<8>().0.iter().all(|w| u64::from_le_bytes(*w) == first) {
        return Some(if first == 0 { Stored::Zero } else { Stored::Fill(first) });
    }
    if PAGES.lock().zram_room() {
        let mut s = SCRATCH.lock();
        let Scratch { table, out } = &mut *s;
        if let Some(n) = aurora_lz4::compress(page, out, table) {
            let mut p = PAGES.lock();
            if let Some(obj) = p.pool_alloc(n) {
                unsafe { core::ptr::copy_nonoverlapping(out.as_ptr(), pool_ptr(obj), n) };
                return Some(Stored::Zram { obj, len: n as u16 });
            }
        }
    }
    None
}

// ------------------------------------------------------------------ figures

pub struct Stats {
    /// Slots in use, and how many could be.
    pub slots: u64,
    pub capacity: u64,
    /// Pages the stores could hold: the compressed store's limit in pages,
    /// and the swap file's.
    pub store_pages: u64,
    /// Pages compressed in memory, their compressed bytes, and the frames
    /// the pool takes; pages of one repeated word; pages in the swap file;
    /// shared pages still cached.
    pub zram_pages: u64,
    pub zram_bytes: u64,
    pub pool_frames: u64,
    pub same_pages: u64,
    pub disk_pages: u64,
    pub cached_pages: u64,
    /// Frames of slot table.
    pub table_frames: u64,
    /// Counted frames.
    pub counted: u64,
    pub swapouts: u64,
    pub swapins: u64,
    pub writebacks: u64,
    /// Pages that could not be stored (swap full).
    pub refused: u64,
    /// Swap-ins from the compressed store and from disk: (count, total ns).
    pub in_zram: (u64, u64),
    pub in_disk: (u64, u64),
}

pub fn stats() -> Stats {
    stats_with(Some(&PAGES.lock()))
}

/// [`stats`] without waiting (zeros if the lock is busy).
pub fn stats_nowait() -> Stats {
    match PAGES.try_lock() {
        Some(p) => stats_with(Some(&p)),
        None => stats_with(None),
    }
}

fn stats_with(p: Option<&Pages>) -> Stats {
    let pair = |a: &[AtomicU64; 2]| (a[0].load(Ordering::Relaxed), a[1].load(Ordering::Relaxed));
    let get = |f: fn(&Pages) -> u64| p.map_or(0, f);
    Stats {
        slots: get(|p| p.used),
        capacity: get(|p| p.capacity()),
        store_pages: ZRAM_SLOTS.load(Ordering::Relaxed) + super::swapfile::pages(),
        zram_pages: get(|p| p.zram_pages),
        zram_bytes: get(|p| p.zram_bytes),
        pool_frames: get(|p| p.pool_frames),
        same_pages: get(|p| p.same_pages),
        disk_pages: get(|p| p.disk_pages),
        cached_pages: get(|p| p.cached_pages),
        table_frames: get(|p| p.tables as u64 + p.dir.iter().filter(|&&d| d != 0).count() as u64),
        counted: get(|p| p.counted.len() as u64),
        swapouts: SWAPOUTS.load(Ordering::Relaxed),
        swapins: SWAPINS.load(Ordering::Relaxed),
        writebacks: WRITEBACKS.load(Ordering::Relaxed),
        refused: REFUSED.load(Ordering::Relaxed),
        in_zram: pair(&IN_ZRAM),
        in_disk: pair(&IN_DISK),
    }
}

/// Counted frames, for memory reports.
pub fn counted_frames() -> u64 {
    PAGES.try_lock().map_or(0, |p| p.counted.len() as u64)
}
