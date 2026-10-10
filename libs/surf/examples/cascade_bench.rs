//! Deterministic allocation and time comparison: cargo run --release -p nebula-engine --example cascade_bench
use nebula_engine::Page;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::time::Instant;

struct Count;
static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Count {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(l.size(), Relaxed);
        System.alloc(l)
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l)
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Relaxed);
        BYTES.fetch_add(n, Relaxed);
        System.realloc(p, l, n)
    }
}
#[global_allocator]
static ALLOCATOR: Count = Count;

fn main() {
    let mut html = String::from("<!doctype html><style>body{font-family:sans-serif;color:#123456} .card{padding:12px;margin:8px;border:1px solid #ccc;background:#fff;width:240px} .card p{font-size:14px;line-height:1.5;color:#456} .card b{font-weight:bold;color:#123}</style><body>");
    for i in 0..1000 {
        html.push_str(&format!("<div class=card><b>Card {i}</b><p>A small repeatable browser workload.</p></div>"));
    }
    let page = Page::parse(&html);
    drop(page.styles(1100, 700));
    ALLOCS.store(0, Relaxed);
    BYTES.store(0, Relaxed);
    let start = Instant::now();
    for _ in 0..20 {
        std::hint::black_box(page.styles(1100, 700));
    }
    let elapsed = start.elapsed();
    let (allocs, bytes) = (ALLOCS.load(Relaxed), BYTES.load(Relaxed));
    println!("20 cascades / 1000 cards: {elapsed:?}; {allocs} allocations; {bytes} cumulative allocated bytes (not resident memory)");
}
