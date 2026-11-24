//! Self-tests and micro-benchmarks for the frame allocator and the heap
//! (built with `--features ktest`; `AURORA_TEST_FILTER=mm: cargo xtask test`
//! runs only these).
//!
//! The benchmark's lines start with `mm-bench:` so runs can be compared by
//! grepping the serial log. Under QEMU TCG the absolute numbers swing a lot
//! between runs; compare builds by alternating them.

use super::{frame, heap, irq_off_max_ns, reset_irq_off_max};
use crate::arch::percpu;
use crate::sync::IrqMutex;
use crate::{sched, time};
use alloc::alloc::{alloc, dealloc, Layout};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// Live slots each worker keeps; a step frees a random occupied slot or
/// fills an empty one, so about half are live at a time.
const SLOTS: usize = 1024;
const HEAP_STEPS: u32 = 200_000;
const FRAME_ROUNDS: u32 = 200;
const FRAME_BATCH: usize = 256;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

/// A kernel-like mix: mostly small objects, some buffers, a few big ones.
fn size(r: &mut Rng) -> usize {
    let x = r.next();
    let pick = x % 100;
    let v = (x >> 8) as usize;
    match pick {
        0..70 => 16 + v % 240,
        70..90 => 256 + v % 1792,
        90..98 => 2048 + v % 6144,
        _ => 8192 + v % 57344,
    }
}

fn heap_work(seed: u64, steps: u32) {
    let mut rng = Rng(seed | 1);
    let mut slots: Vec<(usize, usize)> = alloc::vec![(0, 0); SLOTS];
    for _ in 0..steps {
        let i = (rng.next() % SLOTS as u64) as usize;
        let (p, n) = slots[i];
        if p != 0 {
            unsafe { dealloc(p as *mut u8, Layout::from_size_align_unchecked(n, 8)) };
            slots[i] = (0, 0);
        } else {
            let n = size(&mut rng);
            let p = unsafe { alloc(Layout::from_size_align_unchecked(n, 8)) };
            assert!(!p.is_null(), "benchmark allocation failed");
            unsafe {
                p.write(1);
                p.add(n - 1).write(2);
            }
            slots[i] = (p as usize, n);
        }
    }
    for (p, n) in slots {
        if p != 0 {
            unsafe { dealloc(p as *mut u8, Layout::from_size_align_unchecked(n, 8)) };
        }
    }
}

fn frame_work(rounds: u32) {
    let mut held = [0u64; FRAME_BATCH];
    for _ in 0..rounds {
        for h in held.iter_mut() {
            *h = frame::alloc().expect("benchmark frame");
        }
        for &h in held.iter() {
            frame::free(h);
        }
    }
}

static GO: AtomicU32 = AtomicU32::new(0);
static DONE: AtomicU32 = AtomicU32::new(0);
static CPUS_SEEN: AtomicU32 = AtomicU32::new(0);
static SEED: AtomicU64 = AtomicU64::new(1);
/// 0 = heap, 1 = frames
static KIND: AtomicU32 = AtomicU32::new(0);

fn worker() {
    let seed = SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    while GO.load(Ordering::Acquire) == 0 {
        core::hint::spin_loop();
    }
    CPUS_SEEN.fetch_or(1 << percpu::cpu_id(), Ordering::Relaxed);
    match KIND.load(Ordering::Relaxed) {
        0 => heap_work(seed, HEAP_STEPS),
        _ => frame_work(FRAME_ROUNDS),
    }
    DONE.fetch_add(1, Ordering::Release);
}

/// Runs `n` workers at once; returns the wall time in ns.
fn parallel(kind: u32, n: u32) -> u64 {
    GO.store(0, Ordering::SeqCst);
    DONE.store(0, Ordering::SeqCst);
    CPUS_SEEN.store(0, Ordering::SeqCst);
    KIND.store(kind, Ordering::SeqCst);
    for _ in 0..n {
        sched::spawn("mm-bench", worker);
    }
    sched::sleep_ms(20);
    let t0 = time::now_ns();
    GO.store(1, Ordering::Release);
    while DONE.load(Ordering::Acquire) < n {
        sched::sleep_ms(1);
    }
    time::now_ns() - t0
}

fn per_op(ns: u64, ops: u64) -> u64 {
    ns / ops.max(1)
}

/// Each phase runs this many times; the fastest and the median are
/// reported (host noise under TCG only ever adds time).
const REPS: usize = 5;

/// (fastest, median) of `REPS` runs of `f`, each returning ns per op.
fn measure(mut f: impl FnMut() -> u64) -> (u64, u64) {
    let mut v = [0u64; REPS];
    for x in v.iter_mut() {
        *x = f();
    }
    v.sort_unstable();
    (v[0], v[REPS / 2])
}

fn contiguous_runs() -> (u64, u64) {
    // Low memory fragmented: hold every other frame of 8192 handed out,
    // then take and give back runs.
    let mut held: Vec<u64> = Vec::with_capacity(8192);
    for _ in 0..8192 {
        held.push(frame::alloc().expect("frame"));
    }
    for (i, &f) in held.iter().enumerate() {
        if i % 2 == 0 {
            frame::free(f);
        }
    }
    const RUNS: u64 = 500;
    let t0 = time::now_ns();
    for _ in 0..RUNS {
        let run = frame::alloc_contiguous(16).expect("contiguous run");
        for i in 0..16 {
            frame::free(run + i * 4096);
        }
    }
    let cont = per_op(time::now_ns() - t0, RUNS);
    let t0 = time::now_ns();
    for _ in 0..RUNS {
        let run = frame::alloc_contiguous_below(4, 1 << 32).expect("run below 4 GiB");
        for i in 0..4 {
            frame::free(run + i * 4096);
        }
    }
    let cont32 = per_op(time::now_ns() - t0, RUNS);
    for (i, &f) in held.iter().enumerate() {
        if i % 2 == 1 {
            frame::free(f);
        }
    }
    (cont, cont32)
}

pub fn run() {
    let cpus = percpu::count() as u32;
    let boot_max = irq_off_max_ns();
    crate::kprintln!();
    crate::kprintln!("mm-bench: irq-off worst since boot {} ns", boot_max);
    reset_irq_off_max();
    let (h0, m0) = heap::cache_counts();
    let f0 = frame::stats();

    let heap_ops = HEAP_STEPS as u64;
    let (h1, h1m) = measure(|| {
        let t0 = time::now_ns();
        heap_work(0x1234_5678, HEAP_STEPS);
        per_op(time::now_ns() - t0, heap_ops)
    });
    crate::kprintln!("mm-bench: heap 1 task: {} ns/op (median {}; {} ops x{})", h1, h1m, heap_ops, REPS);
    let (hn, hnm) = measure(|| per_op(parallel(0, cpus), heap_ops * cpus as u64));
    crate::kprintln!(
        "mm-bench: heap {} tasks: {} ns/op aggregate (median {}), speedup x{}.{:02} (cpus used {})",
        cpus,
        hn,
        hnm,
        h1 / hn.max(1),
        h1 * 100 / hn.max(1) % 100,
        CPUS_SEEN.load(Ordering::Relaxed).count_ones()
    );

    let frame_ops = FRAME_ROUNDS as u64 * FRAME_BATCH as u64 * 2;
    let (f1, f1m) = measure(|| {
        let t0 = time::now_ns();
        frame_work(FRAME_ROUNDS);
        per_op(time::now_ns() - t0, frame_ops)
    });
    crate::kprintln!("mm-bench: frames 1 task: {} ns/op (median {})", f1, f1m);
    let (fnn, fnm) = measure(|| per_op(parallel(1, cpus), frame_ops * cpus as u64));
    crate::kprintln!(
        "mm-bench: frames {} tasks: {} ns/op aggregate (median {}), speedup x{}.{:02}",
        cpus,
        fnn,
        fnm,
        f1 / fnn.max(1),
        f1 * 100 / fnn.max(1) % 100
    );

    let mut conts = [(0u64, 0u64); REPS];
    for c in conts.iter_mut() {
        *c = contiguous_runs();
    }
    let (c16, c16m) = measure({
        let mut i = 0;
        move || {
            i += 1;
            conts[i - 1].0
        }
    });
    let (c4, c4m) = measure({
        let mut i = 0;
        move || {
            i += 1;
            conts[i - 1].1
        }
    });
    crate::kprintln!("mm-bench: contiguous 16-frame run (fragmented): {} ns/op (median {})", c16, c16m);
    crate::kprintln!("mm-bench: contiguous 4-frame run below 4 GiB: {} ns/op (median {})", c4, c4m);

    crate::kprintln!("mm-bench: irq-off worst during benchmark {} ns", irq_off_max_ns());
    let s = super::stats();
    crate::kprintln!(
        "mm-bench: heap {} KiB, {} KiB used; RAM {} MiB used of {} MiB",
        s.heap_size >> 10,
        s.heap_used >> 10,
        s.used_bytes >> 20,
        s.total_bytes >> 20
    );
    // Shared-lock trips: a heap operation touches its class lock only when
    // the CPU's stack runs dry or overflows; a frame, the buddy lock only
    // when the CPU's cache does.
    let (h, m) = heap::cache_counts();
    let f = frame::stats();
    let (dh, dm) = (h - h0, m - m0);
    let (fh, fm) = (f.cache_hits - f0.cache_hits, f.cache_misses - f0.cache_misses);
    crate::kprintln!(
        "mm-bench: CPU-cache hits during benchmark: heap {}/{} allocations ({}.{}%), frames {}/{} ({}.{}%)",
        dh,
        dh + dm,
        dh * 100 / (dh + dm).max(1),
        dh * 1000 / (dh + dm).max(1) % 10,
        fh,
        fh + fm,
        fh * 100 / (fh + fm).max(1),
        fh * 1000 / (fh + fm).max(1) % 10
    );
    crate::kprint!("mm-bench: done ");
}

// --- Tests ---------------------------------------------------------------

/// Size classes map every size to the smallest fitting class; objects of
/// every class come back aligned and usable.
pub fn classes() {
    heap::testing::check_classes();
    let mut held = Vec::new();
    for size in [1, 15, 16, 17, 100, 128, 129, 1000, 1024, 1025, 3000, 4096, 4097, 20000, 32768, 32769, 70000] {
        for align in [1, 8, 64, 4096, 16384] {
            let l = Layout::from_size_align(size, align).unwrap();
            let p = unsafe { alloc(l) };
            assert!(!p.is_null());
            assert_eq!(p as usize % align, 0, "alignment of {size}/{align}");
            unsafe { core::ptr::write_bytes(p, 0x5a, size) };
            held.push((p, l));
        }
    }
    for &(p, l) in &held {
        let s = unsafe { core::slice::from_raw_parts(p, l.size()) };
        assert!(s.iter().all(|&b| b == 0x5a), "objects overlap");
    }
    for (p, l) in held {
        unsafe { dealloc(p, l) };
    }
    // Growing and shrinking in place.
    let mut v: Vec<u8> = Vec::with_capacity(100);
    v.extend(core::iter::repeat_n(3, 100));
    v.reserve_exact(10_000);
    v.extend(core::iter::repeat_n(4, 100_000));
    v.truncate(5000);
    v.shrink_to_fit();
    assert!(v[..100].iter().all(|&b| b == 3) && v[100..].iter().all(|&b| b == 4));
    // The telemetry members are well-formed JSON once wrapped in an object.
    let mut json = alloc::string::String::from("{\"x\":0");
    super::write_telemetry(&mut json);
    json.push('}');
    assert!(json.contains(",\"framesFree\":") && json.contains("\"classes\":[["), "telemetry: {json}");
    let depth = json.chars().try_fold(0i32, |d, c| {
        let d = d + matches!(c, '{' | '[') as i32 - matches!(c, '}' | ']') as i32;
        (d >= 0).then_some(d)
    });
    assert_eq!(depth, Some(0), "telemetry brackets: {json}");
}

/// The buddy allocator: runs of every shape, alignment, the 4 GiB limit,
/// double frees, and the free count coming back.
pub fn frames() {
    // Tasks the previous tests started may still be exiting (their stacks
    // and memory coming back): wait until the free count holds still. On
    // a fast machine (hardware virtualization) they overlap this test.
    let (mut last, mut still) = (frame::counts().1, 0);
    for _ in 0..60 {
        crate::sched::sleep_ms(50);
        let now = frame::counts().1;
        still = if now == last { still + 1 } else { 0 };
        if still == 4 {
            break;
        }
        last = now;
    }
    // A run the lists can't give is retried after the heap gives back its
    // idle slabs: let it give them now, so they don't count as found
    // memory below (where RAM is more broken up by the firmware, as on
    // Arm's `virt`, the long runs take that path).
    super::heap::reclaim();
    frame::drain_caches();
    frame::check();
    let before = frame::counts().1;
    let doubles = frame::stats().double_frees;
    let mut runs = Vec::new();
    for count in [1u64, 2, 3, 5, 16, 17, 100, 512, 1024, 1500] {
        let p = frame::alloc_contiguous(count).expect("contiguous run");
        assert_eq!(p % 4096, 0);
        unsafe {
            (super::phys_to_virt(p) as *mut u64).write(count);
            (super::phys_to_virt(p + (count - 1) * 4096) as *mut u64).write(count);
        }
        runs.push((p, count));
    }
    for (i, &(p, n)) in runs.iter().enumerate() {
        for &(q, m) in &runs[i + 1..] {
            assert!(p + n * 4096 <= q || q + m * 4096 <= p, "runs overlap");
        }
    }
    let aligned = frame::alloc_run(16, 4, u64::MAX).unwrap();
    assert_eq!(aligned % (64 << 10), 0, "aligned run");
    let low = frame::alloc_contiguous_below(8, 1 << 32).unwrap();
    assert!(low + 8 * 4096 <= 1 << 32);
    if let Some(p) = frame::alloc_contiguous_below(4, 16 << 20) {
        assert!(p + 4 * 4096 <= 16 << 20, "run below 16 MiB");
        frame::free_run(p, 4);
    }
    frame::free_run(aligned, 16);
    frame::free_run(low, 8);
    for (p, n) in runs {
        frame::free_run(p, n);
    }
    let f = frame::alloc().unwrap();
    frame::free(f);
    frame::free(f);
    assert_eq!(frame::stats().double_frees, doubles + 1, "a double free is caught");
    frame::drain_caches();
    frame::check();
    assert_eq!(frame::counts().1, before, "frames leaked");
}

static STRESS_DONE: AtomicU32 = AtomicU32::new(0);
static STRESS_SEED: AtomicU64 = AtomicU64::new(7);
static STRESS_CPUS: AtomicU32 = AtomicU32::new(0);
/// Objects one worker allocated and another frees: (address, size, fill).
static HANDOFF: IrqMutex<Vec<(usize, usize, u8)>> = IrqMutex::new(Vec::new());

fn check_free((p, n, b): (usize, usize, u8)) {
    let s = unsafe { core::slice::from_raw_parts(p as *const u8, n) };
    assert!(s.iter().all(|&x| x == b), "heap object changed under its owner");
    unsafe { dealloc(p as *mut u8, Layout::from_size_align_unchecked(n, 16)) };
}

fn stress_worker() {
    let seed = STRESS_SEED.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed);
    let mut rng = Rng(seed | 1);
    let mut slots: Vec<(usize, usize, u8)> = alloc::vec![(0, 0, 0); 256];
    for step in 0..30_000u32 {
        STRESS_CPUS.fetch_or(1 << percpu::cpu_id(), Ordering::Relaxed);
        let i = (rng.next() % slots.len() as u64) as usize;
        if slots[i].0 != 0 {
            let obj = core::mem::take(&mut slots[i]);
            if step % 4 == 0 {
                HANDOFF.lock().push(obj);
            } else {
                check_free(obj);
            }
        } else {
            let n = size(&mut rng);
            let b = rng.next() as u8;
            let p = unsafe { alloc(Layout::from_size_align_unchecked(n, 16)) };
            assert!(!p.is_null(), "stress allocation failed");
            unsafe { core::ptr::write_bytes(p, b, n) };
            slots[i] = (p as usize, n, b);
        }
        if step % 8 == 0 {
            let other = HANDOFF.lock().pop();
            if let Some(obj) = other {
                check_free(obj);
            }
        }
    }
    for obj in slots {
        if obj.0 != 0 {
            check_free(obj);
        }
    }
    STRESS_DONE.fetch_add(1, Ordering::Release);
}

/// Every CPU allocates and frees at once, some objects freed on another
/// CPU than the one that allocated them; contents must survive, and the
/// heap must account for everything afterwards.
pub fn stress() {
    let cpus = percpu::count() as u32;
    let workers = cpus * 2;
    let (_, used_before) = heap::usage();
    STRESS_DONE.store(0, Ordering::SeqCst);
    STRESS_CPUS.store(0, Ordering::SeqCst);
    for _ in 0..workers {
        sched::spawn("mm-stress", stress_worker);
    }
    let deadline = time::uptime_ms() + 120_000;
    while STRESS_DONE.load(Ordering::Acquire) < workers {
        assert!(time::uptime_ms() < deadline, "stress workers did not finish");
        sched::sleep_ms(5);
    }
    let rest = core::mem::take(&mut *HANDOFF.lock());
    for obj in rest {
        check_free(obj);
    }
    crate::kprint!("[{} workers on {} CPUs] ", workers, STRESS_CPUS.load(Ordering::Relaxed).count_ones());
    let (size, used) = heap::usage();
    // Other kernel tasks allocate meanwhile; the workers' objects are gone.
    assert!(used < used_before + (1 << 20), "heap in use grew from {used_before} to {used}");
    // Under pressure the idle slabs go back.
    heap::reclaim();
    let (size_after, _) = heap::usage();
    assert!(size_after <= size, "reclaim grew the heap");
    frame::drain_caches();
    frame::check();
}

/// Running out of memory fails allocations cleanly, gives back the heap's
/// idle memory first, and recovers once frames are freed.
pub fn out_of_memory() {
    // (black_box: the compiler may otherwise assume an unused allocation succeeds.)
    let huge = core::hint::black_box(unsafe { alloc(Layout::from_size_align(1 << 40, 8).unwrap()) });
    assert!(huge.is_null(), "1 TiB allocation");
    assert!(frame::alloc_contiguous(1 << 30).is_none(), "4 TiB run");
    let failures = frame::stats().failures;
    // Leave some idle slabs and cached objects for the pressure to find.
    let junk: Vec<Vec<u8>> = (0..2000).map(|i| alloc::vec![i as u8; 300]).collect();
    drop(junk);
    let mut held: Vec<u64> = Vec::with_capacity(frame::counts().0 as usize);
    let (heap_before, _) = heap::usage();
    let reclaimed = heap::reclaimed_frames();
    let (_, free_before) = frame::counts();
    let t0 = time::now_ns();
    while let Some(f) = frame::alloc() {
        held.push(f);
    }
    // Everything is taken: runs, pages and new slabs all fail.
    let none_run = frame::alloc_contiguous(2).is_none();
    let big = core::hint::black_box(unsafe { alloc(Layout::from_size_align(256 << 10, 8).unwrap()) });
    let (heap_squeezed, _) = heap::usage();
    let got = held.len() as u64;
    for f in held.drain(..) {
        frame::free(f);
    }
    let ms = (time::now_ns() - t0) / 1_000_000;
    assert!(none_run, "a run was found with memory exhausted");
    assert!(big.is_null(), "a heap allocation succeeded with memory exhausted");
    assert!(frame::stats().failures > failures, "failures are counted");
    assert!(heap::reclaimed_frames() > reclaimed, "the heap gave back no idle memory");
    assert!(got + 1024 >= free_before, "only {got} of {free_before} free frames could be allocated");
    crate::kprint!("[{} frames taken in {} ms, heap {} -> {} KiB] ", got, ms, heap_before >> 10, heap_squeezed >> 10);
    frame::drain_caches();
    frame::check();
    assert!(frame::counts().1 + 64 >= free_before, "frames lost after running out");
}
