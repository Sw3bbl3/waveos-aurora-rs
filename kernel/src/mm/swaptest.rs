//! Self-tests for swapping and the out-of-memory killer (`cargo xtask
//! test`; `AURORA_TEST_FILTER=swap` runs only these).
//!
//! - [`semantics`]: address spaces made here, their pages swapped out on
//!   demand (`pageout`) and checked byte for byte when they come back:
//!   private pages, pages shared since `fork` (written by one side after),
//!   shared memory mapped twice (a write on one side seen on the other
//!   after both swapped it out), locked pages (never swapped), protection
//!   changes and moves of swapped pages, GPU buffers never swapped, and
//!   every slot given back.
//! - [`overcommit`]: a program (`fuzz swap`) fills several times the memory
//!   it may have with a pattern of its own per page, from two threads, and
//!   reads every page back twice in other orders, while `kswapd` swaps.
//! - [`file_full`]: with the compressed store shut, pages go to the swap
//!   file; once that is full too, the program asking for more fails (`mmap`)
//!   or is the one ended, and the kernel is unharmed.
//! - [`oom`]: the out-of-memory killer picks the right program, and when
//!   memory runs out under a program that only reads its own memory, ends
//!   that one — not the program in front, not a protected one.
//!
//! To keep them quick, programs get only part of memory (the rest is held
//! here, as kernel memory that grows would be): `swap_test_ram` MiB (boot
//! option, `AURORA_SWAP_TEST_RAM`; default 48, `all` for everything), and
//! fill `swap_factor` times that (`AURORA_SWAP_FACTOR`, default 3).

use super::vmm::{self, AddressSpace, Prot};
use super::{frame, kswapd, swap, swapfile, PAGE_SIZE};
use crate::{sched, time};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::Ordering;

fn opt(key: &str) -> Option<String> {
    crate::gui::prefs::boot_option(key)
}

/// A page's contents: a 64-byte block of its own repeated (compresses
/// well, and is never one repeated word), or for `random`, noise.
fn pattern(seed: u64, page: u64, random: bool) -> [u8; PAGE_SIZE as usize] {
    let mut out = [0u8; PAGE_SIZE as usize];
    let mut x = seed ^ page.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0x1234_5678;
    let mut block = [0u64; 8];
    for (i, w) in out.as_chunks_mut::<8>().0.iter_mut().enumerate() {
        if random || i < 8 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            block[i % 8] = x;
        }
        *w = if random { x } else { block[i % 8] }.to_le_bytes();
    }
    out
}

fn check_page(a: &mut AddressSpace, va: u64, want: &[u8], what: &str) {
    let mut got = [0u8; PAGE_SIZE as usize];
    a.read_bytes(va, &mut got).unwrap_or_else(|e| panic!("{what}: page {va:#x} unreadable ({e})"));
    if let Some(i) = got.iter().zip(want).position(|(g, w)| g != w) {
        panic!("{what}: page {va:#x} differs at byte {i}: {:#x}, not {:#x}", got[i], want[i]);
    }
}

/// Slots in use: back to `before` once nothing holds swapped pages.
fn slots_settle(before: u64, what: &str) {
    let deadline = time::uptime_ms() + 15_000;
    while swap::stats().slots > before && time::uptime_ms() < deadline {
        swap::writeback(4096);
        sched::sleep_ms(20);
    }
    let s = swap::stats();
    assert!(s.slots <= before, "{what}: {} swap slots still in use (were {before})", s.slots);
}

pub fn semantics() {
    const N: u64 = 48;
    let (base, moved) = (0x40_0000u64, 0x80_0000u64);
    let slots_before = swap::stats().slots;
    let (_, frames_before) = super::leak_check_counts();
    let seed = 7;
    {
        // Private pages: out, and back on touch.
        let mut a = AddressSpace::new().unwrap();
        a.map_anon(base, N, Prot::ReadWrite).unwrap();
        for p in 0..N {
            a.write_bytes(base + p * PAGE_SIZE, &pattern(seed, p, false)).unwrap();
        }
        let resident = a.resident;
        assert_eq!(a.pageout(base, N), N, "every private page goes");
        assert!((0..N).all(|p| a.is_swapped(base + p * PAGE_SIZE)) && a.swapped == N);
        assert_eq!(a.resident, resident - N * PAGE_SIZE, "resident memory follows");
        for p in 0..N {
            check_page(&mut a, base + p * PAGE_SIZE, &pattern(seed, p, false), "private");
        }
        assert_eq!((a.swapped, a.resident), (0, resident));

        // Shared since `fork`: both see the parent's pages until one writes.
        assert_eq!(a.pageout(base, N), N);
        let mut b = a.fork_copy().unwrap();
        assert_eq!(b.swapped, N, "the child's entries refer to the same slots");
        b.write_bytes(base, b"child wrote this").unwrap();
        check_page(&mut a, base + PAGE_SIZE, &pattern(seed, 1, false), "parent after fork");
        let mut first = pattern(seed, 0, false);
        check_page(&mut a, base, &first, "parent's page the child wrote");
        first[..16].copy_from_slice(b"child wrote this");
        check_page(&mut b, base, &first, "child's own copy");
        // Both swap out what they share (cached until neither maps it), then write back.
        a.pageout(base, N);
        b.pageout(base, N);
        swap::writeback(4096);
        a.write_bytes(base + 2 * PAGE_SIZE, b"parent wrote").unwrap();
        for p in 1..N {
            let mut want = pattern(seed, p, false);
            check_page(&mut b, base + p * PAGE_SIZE, &want, "child after both swapped");
            if p == 2 {
                want[..12].copy_from_slice(b"parent wrote");
            }
            check_page(&mut a, base + p * PAGE_SIZE, &want, "parent after both swapped");
        }
        drop(b);

        // Locked pages never leave; unlocked, they may.
        a.mlock(base, 8, true).unwrap();
        a.pageout(base, N);
        for p in 0..N {
            let va = base + p * PAGE_SIZE;
            assert_eq!(a.is_swapped(va), p >= 8, "page {p}: locked pages stay");
            assert_eq!(a.is_locked(va), p < 8);
        }
        assert_eq!(a.page_counts().locked, 8);
        a.mlock(base, 8, false).unwrap();
        assert_eq!(a.pageout(base, 8), 8, "unlocked pages go");

        // Protection changes and moves keep swapped pages.
        a.protect(base, N, Some(Prot::ReadOnly)).unwrap();
        let regions = a.regions();
        assert!(regions.iter().any(|r| r.0 <= base && r.1 >= base + N * PAGE_SIZE && !r.2), "{regions:x?}");
        a.protect(base, N, Some(Prot::ReadWrite)).unwrap();
        a.move_pages(base, moved, N).unwrap();
        assert!(a.is_swapped(moved + 9 * PAGE_SIZE) && !a.is_swapped(base + 9 * PAGE_SIZE));
        for p in 3..N {
            check_page(&mut a, moved + p * PAGE_SIZE, &pattern(seed, p, false), "moved");
        }
        // Unmapped while out: the slots go.
        a.pageout(moved, N);
        a.unmap(moved, N / 2);
        assert_eq!(a.swapped, N / 2);
    }

    {
        // Shared memory (a shared anonymous mapping, its object gone) in two
        // address spaces: swapped out by both, written back, then a write
        // through one is read through the other.
        let frames: Vec<u64> = (0..N).map(|_| vmm::shm_alloc().unwrap()).collect();
        let mut a = AddressSpace::new().unwrap();
        let mut b = AddressSpace::new().unwrap();
        a.map_shared(base, &frames, Prot::ReadWrite).unwrap();
        b.map_shared(moved, &frames, Prot::ReadWrite).unwrap();
        for &f in &frames {
            vmm::shm_release(f);
        }
        for p in 0..N {
            a.write_bytes(base + p * PAGE_SIZE, &pattern(seed + 1, p, p % 4 == 3)).unwrap();
        }
        assert_eq!(a.pageout(base, N), N);
        let cached = swap::stats().cached_pages;
        assert!(cached >= N, "a's entries are out, b still maps the frames: cached");
        b.pageout(moved, N);
        let written = swap::writeback(4096);
        assert!(written > 0, "frames nobody maps are written out");
        b.write_bytes(moved + 5 * PAGE_SIZE, b"written by b").unwrap();
        let mut want = pattern(seed + 1, 5, false);
        want[..12].copy_from_slice(b"written by b");
        check_page(&mut a, base + 5 * PAGE_SIZE, &want, "shared page written by the other side");
        // One side out, the other writes: the cached frame is what both see.
        a.pageout(base, N);
        b.write_bytes(moved + 6 * PAGE_SIZE, b"after a let go").unwrap();
        let mut want = pattern(seed + 1, 6, false);
        want[..14].copy_from_slice(b"after a let go");
        check_page(&mut a, base + 6 * PAGE_SIZE, &want, "cached shared page");
        for p in (0..N).filter(|&p| p != 5 && p != 6) {
            check_page(&mut a, base + p * PAGE_SIZE, &pattern(seed + 1, p, p % 4 == 3), "shared");
            check_page(&mut b, moved + p * PAGE_SIZE, &pattern(seed + 1, p, p % 4 == 3), "shared, other side");
        }
    }
    {
        // Device memory (a GPU buffer a program maps: `BufferObject::user_frames`)
        // is never swapped out, while the buffer lives (it may be on the
        // screen) or after (another device may still use it); the last
        // mapping frees it.
        let frames: Vec<u64> = (0..8).map(|_| frame::alloc().unwrap()).collect();
        vmm::shm_count(&frames);
        let mut a = AddressSpace::new().unwrap();
        a.map_shared(base, &frames, Prot::ReadWrite).unwrap();
        assert_eq!(a.pageout(base, 8), 0, "a GPU buffer's pages were swapped out");
        for &f in &frames {
            vmm::shm_release(f);
        }
        assert_eq!(a.pageout(base, 8), 0, "a GPU buffer's pages were swapped out once the buffer was gone");
        assert_eq!(a.reclaim(64, 64).evicted, 0);
        assert!((0..8).all(|p| !a.is_swapped(base + p * PAGE_SIZE)));
    }
    let map_us = {
        // What kmscube does every frame: map a 4 MiB buffer, unmap it.
        // Holds and releases go under one lock per call, and the buffer
        // outlives every mapping.
        let frames: Vec<u64> = (0..1024).map(|_| frame::alloc().unwrap()).collect();
        vmm::shm_count(&frames);
        let mut a = AddressSpace::new().unwrap();
        let rounds = 32;
        let t0 = time::now_ns();
        for _ in 0..rounds {
            a.map_shared(base, &frames, Prot::ReadWrite).unwrap();
            a.unmap(base, 1024);
        }
        let us = (time::now_ns() - t0) / 1000 / rounds;
        a.map_shared(base, &frames, Prot::ReadWrite).unwrap();
        assert_eq!(a.pageout(base, 1024), 0, "a GPU buffer's pages were swapped out");
        drop(a);
        for &f in &frames {
            vmm::shm_release(f);
        }
        assert!(us < 50_000, "mapping and unmapping a 4 MiB GPU buffer took {us} us");
        us
    };
    slots_settle(slots_before, "after the address spaces went");
    let (_, frames_after) = super::leak_check_counts();
    assert!(frames_after + 8 >= frames_before, "swapping leaked {} frames", frames_before - frames_after);
    let s = swap::stats();
    crate::kprint!("[{} out, {} in, {} written back; 4 MiB GPU buffer mapped and unmapped in {map_us} us] ", s.swapouts, s.swapins, s.writebacks);
}

// ------------------------------------------------------- with programs

/// Memory held here so programs get only `swap_test_ram` MiB.
pub struct Balloon(Vec<u64>);

impl Balloon {
    /// Takes frames until about `leave_mib` MiB are left to programs.
    pub fn leaving(leave_mib: Option<u64>) -> Balloon {
        let mut held = Vec::new();
        if let Some(mib) = leave_mib {
            let keep = kswapd::high_watermark() + mib * 256;
            let free = frame::counts().1;
            held.reserve(free.saturating_sub(keep) as usize);
            while frame::counts().1 > keep {
                match frame::alloc() {
                    Some(f) => held.push(f),
                    None => break,
                }
            }
        }
        Balloon(held)
    }

    /// Takes frames until more of the kernel's reserve is gone than
    /// programs bringing pages back from swap may dip into (kernel memory
    /// growing under the programs): theirs must shrink.
    fn squeeze(&mut self) {
        let target = frame::kernel_reserve() * 7 / 8 - 256;
        while frame::counts().1 > target {
            match frame::alloc() {
                Some(f) => self.0.push(f),
                None => break,
            }
        }
    }
}

impl Drop for Balloon {
    fn drop(&mut self) {
        for f in self.0.drain(..) {
            frame::free(f);
        }
    }
}

fn ram_mib() -> Option<u64> {
    match opt("swap_test_ram").as_deref() {
        Some("all") => None,
        Some(v) => v.parse().ok().or(Some(48)),
        None => Some(48),
    }
}

/// A program the tests run, with its output.
struct Prog {
    pid: u32,
    proc: Arc<crate::proc::Process>,
    pipe: Arc<crate::proc::pipe::Pipe>,
    _read: Arc<crate::proc::Handle>,
    out: String,
}

impl Prog {
    fn start(argv: &[&str]) -> Prog {
        let (r, w) = crate::proc::new_pipe(true);
        let pid = crate::proc::spawn("/System/Bin/fuzz", argv, [None, Some(w.clone()), Some(w)], 0)
            .unwrap_or_else(|e| panic!("spawn {argv:?}: {e}"));
        let pipe = match &*r {
            crate::proc::Handle::PipeRead { pipe, .. } => pipe.clone(),
            _ => unreachable!(),
        };
        Prog { pid, proc: crate::proc::get(pid).unwrap(), pipe, _read: r, out: String::new() }
    }

    fn poll(&mut self) {
        let mut buf = [0u8; 256];
        while let Ok(n @ 1..) = self.pipe.read(&mut buf, true) {
            self.out.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
    }

    /// Waits for `text` in its output (or its end); true if it came.
    fn wait_for(&mut self, text: &str, ms: u64) -> bool {
        let deadline = time::uptime_ms() + ms;
        while time::uptime_ms() < deadline {
            self.poll();
            if self.out.contains(text) {
                return true;
            }
            if self.proc.exit_code().is_some() {
                self.poll();
                return self.out.contains(text);
            }
            sched::sleep_ms(20);
        }
        false
    }

    fn exit(&mut self, ms: u64) -> Option<i64> {
        let deadline = time::uptime_ms() + ms;
        while self.proc.exit_code().is_none() && time::uptime_ms() < deadline {
            self.poll();
            sched::sleep_ms(20);
        }
        self.poll();
        self.proc.exit_code()
    }

    fn kill(&mut self) {
        let _ = crate::proc::kill(self.pid);
        self.exit(10_000);
    }
}

/// What swap did meanwhile, watched while programs run.
#[derive(Default)]
struct Peaks {
    zram_bytes: u64,
    zram_pages: u64,
    disk_pages: u64,
    level: u32,
}

impl Peaks {
    fn see(&mut self) {
        let s = swap::stats();
        self.zram_bytes = self.zram_bytes.max(s.zram_bytes);
        self.zram_pages = self.zram_pages.max(s.zram_pages);
        self.disk_pages = self.disk_pages.max(s.disk_pages);
        self.level = self.level.max(kswapd::level());
    }
}

fn wait_gone(pids: &[u32]) {
    let deadline = time::uptime_ms() + 15_000;
    while pids.iter().any(|&p| crate::proc::get(p).is_some_and(|p| !p.is_reaped())) && time::uptime_ms() < deadline {
        sched::sleep_ms(20);
    }
}

pub fn overcommit() {
    let factor: u64 = opt("swap_factor").and_then(|v| v.parse().ok()).unwrap_or(3);
    let ram = ram_mib();
    let slots_before = swap::stats().slots;
    let before = swap::stats();
    let limits = swap::limits();
    let _ = swapfile::activate();
    let balloon = Balloon::leaving(ram);
    let (total, free) = frame::counts();
    let avail = free.saturating_sub(frame::kernel_reserve());
    let mib = (avail * factor / 256).max(16);
    // Room for what doesn't fit in memory, in the compressed store.
    swap::set_limits((avail * factor).max(total), (avail * factor * PAGE_SIZE / 4).max(limits.1));
    let t0 = time::uptime_ms();
    let (mib_s, seed) = (alloc::format!("{mib}"), alloc::format!("{}", 0x5EED_u64 + mib));
    let mut prog = Prog::start(&["fuzz", "swap", &mib_s, &seed, "2", "2"]);
    let mut peaks = Peaks::default();
    let deadline = time::uptime_ms() + 30 * 60_000;
    while prog.proc.exit_code().is_none() && time::uptime_ms() < deadline {
        prog.poll();
        peaks.see();
        sched::sleep_ms(50);
    }
    let code = prog.exit(1000);
    for line in prog.out.lines() {
        crate::kprintln!("    {line}");
    }
    assert_eq!(code, Some(0), "the pattern did not survive swapping (see above)");
    assert!(prog.out.contains("swap: ok"));
    let ms = time::uptime_ms() - t0;
    wait_gone(&[prog.pid]);
    drop(prog);
    drop(balloon);
    swap::set_limits(limits.0, limits.1);
    let s = swap::stats();
    let avg = |(n, ns): (u64, u64), (n0, ns0): (u64, u64)| (ns - ns0).checked_div((n - n0) * 1000).unwrap_or(0);
    crate::kprint!(
        "[{} MiB through {} MiB in {} s: {} pages out, {} in; up to {} MiB compressed into {} MiB, {} pages on disk; \
         swap-in {} us compressed, {} us from disk; pressure up to {}] ",
        mib,
        avail / 256,
        ms / 1000,
        s.swapouts - before.swapouts,
        s.swapins - before.swapins,
        peaks.zram_pages / 256,
        peaks.zram_bytes >> 20,
        peaks.disk_pages,
        avg(s.in_zram, before.in_zram),
        avg(s.in_disk, before.in_disk),
        peaks.level
    );
    assert!(s.swapouts > before.swapouts && s.swapins > before.swapins, "nothing was swapped");
    slots_settle(slots_before, "after the program ended");
}

pub fn file_full() {
    let pages = match swapfile::activate() {
        Ok(p) => p,
        Err(e) => panic!("no swap file on the test volume: {}", aurora_abi::err::name(e)),
    };
    assert!(pages >= 1024, "a swap file of {pages} pages");
    let slots_before = swap::stats().slots;
    let before = swap::stats();
    let limits = swap::limits();
    let kills = kswapd::oom_kills();
    let failures = frame::stats().failures;
    let balloon = Balloon::leaving(Some(ram_mib().unwrap_or(48).min(32)));
    let avail = frame::counts().1.saturating_sub(frame::kernel_reserve());
    // The compressed store takes nearly nothing: pages go to the file.
    swap::set_limits(64, 64 * 1024);
    let fits = ((avail + pages * 3 / 4) / 256).max(8);
    let mib = alloc::format!("{fits}");
    let mut prog = Prog::start(&["fuzz", "swap", &mib, "77", "2", "1"]);
    let mut peaks = Peaks::default();
    while prog.proc.exit_code().is_none() {
        prog.poll();
        peaks.see();
        sched::sleep_ms(50);
    }
    let code = prog.exit(1000);
    for line in prog.out.lines() {
        crate::kprintln!("    {line}");
    }
    assert_eq!(code, Some(0), "pages through the swap file came back wrong");
    assert!(peaks.disk_pages > pages / 2, "the swap file was hardly used ({} pages)", peaks.disk_pages);
    wait_gone(&[prog.pid]);

    // Twice what memory and the file hold together: the pages it maps are
    // zero (swapped out as nothing) until it writes them, then the file
    // fills; it gets `ENOMEM` from `mmap`, or is ended as out of memory
    // once it writes — the only program to end. The kernel is unharmed.
    let over = alloc::format!("{}", (avail + pages) * 2 / 256 + 8);
    let mut prog2 = Prog::start(&["fuzz", "swap", &over, "78", "1", "1"]);
    let code = prog2.exit(5 * 60_000);
    let refused = code == Some(1) && prog2.out.contains("mmap of");
    assert!(refused || code == Some(-9), "a program asking for more than memory and swap: {code:?} {}", prog2.out);
    wait_gone(&[prog2.pid]);
    assert!(kswapd::oom_kills() <= kills + 1, "only the program itself was ended");
    assert_eq!(frame::stats().failures, failures, "kernel allocations failed meanwhile");
    assert_eq!(swapfile::free_pages(), pages, "the swap file's pages all came back");
    drop(balloon);
    swap::set_limits(limits.0, limits.1);
    let s = swap::stats();
    crate::kprint!(
        "[{} MiB through a {} MiB swap file: {} reads, {} writes, swap-in {} us] ",
        fits,
        pages / 256,
        swapfile::io_counts().0,
        swapfile::io_counts().1,
        (s.in_disk.1 - before.in_disk.1).checked_div((s.in_disk.0 - before.in_disk.0) * 1000).unwrap_or(0)
    );
    slots_settle(slots_before, "after the swap file filled");
}

pub fn oom() {
    let slots_before = swap::stats().slots;
    let limits = swap::limits();
    let kills = kswapd::oom_kills();
    while crate::gui::notify::take().is_some() {}
    // 1. The choice: a protected service (biggest), the program in front
    //    (big), two in the background: the bigger background one goes.
    let mut progs: Vec<Prog> = [("32", "1"), ("24", "2"), ("12", "3"), ("4", "4")]
        .iter()
        .map(|&(mib, seed)| Prog::start(&["fuzz", "swap", mib, seed, "1", "0"]))
        .collect();
    for p in progs.iter_mut() {
        assert!(p.wait_for("swap: wrote", 120_000), "holder: {}", p.out);
    }
    progs[0].proc.oom_protected.store(true, Ordering::Relaxed);
    crate::gui::server::set_foreground(progs[1].pid);
    let (victim, _) = kswapd::choose_victim().expect("a victim");
    assert_eq!(victim.pid, progs[2].pid, "chose {} '{}'", victim.pid, victim.name());
    drop(victim);
    for p in progs.iter_mut() {
        p.kill();
    }
    wait_gone(&progs.iter().map(|p| p.pid).collect::<Vec<_>>());
    drop(progs);

    // 2. For real: a program writes its memory and pauses, and is swapped
    //    out meanwhile; a background program and a protected one fill
    //    memory; swap is full, and kernel memory grows a little. Then the
    //    first becomes the program in front and reads its memory again,
    //    which needs memory back: the background one must go, not the one
    //    in front or the protected one.
    let mut balloon = Balloon::leaving(Some(48));
    let mut front = Prog::start(&["fuzz", "swap", "32", "11", "1", "100000", "6000"]);
    assert!(front.wait_for("swap: wrote", 120_000), "front: {}", front.out);
    let out = front.proc.aspace.lock().as_mut().map_or(0, |a| a.pageout(0, aurora_abi::layout::USER_END / PAGE_SIZE));
    assert!(out >= 8192, "the program in front was swapped out ({out} pages)");
    let mut back = Prog::start(&["fuzz", "swap", "16", "12", "1", "0"]);
    assert!(back.wait_for("swap: wrote", 120_000), "background: {}", back.out);
    let mut service = Prog::start(&["fuzz", "swap", "8", "13", "1", "0"]);
    service.proc.oom_protected.store(true, Ordering::Relaxed);
    assert!(service.wait_for("swap: wrote", 120_000), "service: {}", service.out);
    // Swap holds no more than it does now.
    swap::set_capacity_cap(Some(swap::stats().slots));
    balloon.squeeze();
    crate::gui::server::set_foreground(front.pid);
    let killed = back.exit(120_000);
    front.poll();
    assert_eq!(killed, Some(-9), "the background program was not ended; front: {}", front.out);
    assert!(front.proc.exit_code().is_none(), "the program in front ended: {}", front.out);
    assert!(service.proc.exit_code().is_none(), "the protected program ended");
    assert!(kswapd::oom_kills() > kills);
    let note = core::iter::from_fn(crate::gui::notify::take).find(|n| n.title == "Out of memory");
    assert!(note.is_some_and(|n| n.body.contains("fuzz")), "no notification");
    // The one in front kept every page right.
    drop(balloon);
    front.poll();
    assert!(!front.out.contains("MISMATCH"), "{}", front.out);
    for p in [&mut front, &mut service] {
        p.kill();
    }
    wait_gone(&[front.pid, back.pid, service.pid]);
    crate::gui::server::set_foreground(0);
    swap::set_capacity_cap(None);
    swap::set_limits(limits.0, limits.1);
    slots_settle(slots_before, "after the out-of-memory test");
    crate::kprint!("[{} killed] ", kswapd::oom_kills() - kills);
}
