//! Host-only costs and allocation bounds; not a physical GPU/display measurement.
use lumen::{canvas::Canvas, water::Water};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
struct Count;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Count = Count;
fn main() {
    for (w, h, scale) in [(1280, 800, 1), (1920, 1080, 1), (2560, 1600, 2)] {
        let bg: Vec<u32> = (0..w * h).map(|i| 0xFF304060 + (i % 128) as u32).collect();
        let mut out = bg.clone();
        let mut water = Water::new();
        let mut samples = Vec::with_capacity(150);
        let mut simulation = 0u128;
        let before = ALLOCS.load(Ordering::Relaxed);
        water.impulse(w / 2 / scale, h / 2 / scale, w / scale, h / scale, 0);
        water.impulse(w / 2 / scale + 70, h / 2 / scale, w / scale, h / scale, 0);
        for t in (16..2400).step_by(16) {
            let start = Instant::now();
            water.advance(t, w / scale, h / scale);
            simulation += start.elapsed().as_nanos();
            let mut cv = Canvas::new(&mut out, w, h).with_scale(scale);
            let start = Instant::now();
            water.paint(&mut cv, &bg, false);
            samples.push(start.elapsed().as_nanos());
        }
        let allocs = ALLOCS.load(Ordering::Relaxed) - before;
        assert_eq!(allocs, 0);
        assert!(!water.active());
        samples.sort();
        println!("{w}x{h} scale {scale}: render median {:.3}ms p95 {:.3}ms; simulation mean {:.3}ms; click/frame allocations {allocs}; idle {}",samples[samples.len()/2] as f64/1e6,samples[samples.len()*95/100] as f64/1e6,simulation as f64/samples.len() as f64/1e6,!water.advance(2600,w/scale,h/scale));
    }
}
