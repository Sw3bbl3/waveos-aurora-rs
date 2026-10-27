//! Times the drawing primitives behind one desktop frame at 1280×800.
//!
//! `cargo run --release -p lumen --example frame_bench` prints the median
//! of many runs for each primitive and for the whole frame. The frame is a
//! stand-in for the real desktop: wallpaper, three windows with shadows and
//! rounded corners, the glass bar and Harbor, text and icons.

use lumen::canvas::Canvas;
use lumen::font::{self, Face};
use lumen::geom::Rect;
use lumen::icons::{self, Icon};
use lumen::material::MaterialCache;
use lumen::theme;
use std::time::Instant;

const W: i32 = 1280;
const H: i32 = 800;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn time(runs: usize, mut f: impl FnMut()) -> f64 {
    f();
    let samples = (0..runs)
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    median(samples)
}

fn main() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/");
    for (face, file) in [
        (Face::Regular, "Manrope-Medium.ttf"),
        (Face::SemiBold, "Manrope-Bold.ttf"),
        (Face::Bold, "Manrope-ExtraBold.ttf"),
        (Face::Mono, "JetBrainsMono-Regular.ttf"),
    ] {
        let data = std::fs::read(format!("{root}{file}")).expect("font");
        font::install(face, Box::leak(data.into_boxed_slice()));
    }
    theme::set_dark(true);
    let wallpaper = lumen::wallpaper::generate(0, W, H);
    let window: Vec<u32> = (0..(720 * 480)).map(|i| 0xFF11_1527 + (i % 7) as u32).collect();
    let mut buf = vec![0u32; (W * H) as usize];
    let mut materials = MaterialCache::default();
    let screen = Rect::new(0, 0, W, H);
    let windows = [Rect::new(80, 90, 720, 480), Rect::new(420, 180, 720, 480), Rect::new(260, 260, 720, 480)];
    let glass = [
        Rect::new(8, 8, 120, 38),
        Rect::new(588, 8, 104, 38),
        Rect::new(1180, 8, 92, 38),
        Rect::new(334, 718, 348, 72),
        Rect::new(690, 718, 68, 72),
        Rect::new(766, 718, 180, 72),
    ];
    let runs = 60;

    let mut results: Vec<(&str, f64)> = Vec::new();
    {
        let mut cv = Canvas::new(&mut buf, W, H);
        results.push(("wallpaper blit", time(runs, || cv.blit_same(&wallpaper, screen))));
        results.push((
            "3 window shadows",
            time(runs, || {
                for r in windows {
                    cv.shadow(r, 18, 34, 12, 150);
                }
            }),
        ));
        results.push((
            "3 window blits (rounded)",
            time(runs, || {
                for r in windows {
                    cv.blit(&window, 720, 480, r, 18);
                }
            }),
        ));
        results.push((
            "3 window borders",
            time(runs, || {
                for r in windows {
                    cv.stroke_round_rect(r, 18, 0x1CFF_FFFF);
                }
            }),
        ));
        cv.materials = Some(&mut materials);
        results.push((
            "6 glass panels (cached)",
            time(runs, || {
                cv.materials.as_deref_mut().unwrap().begin_frame();
                for r in glass {
                    cv.frosted(r, 19, 0x7510_162E);
                }
            }),
        ));
        cv.materials = None;
        let f = theme::ui(13);
        results.push((
            "40 lines of text",
            time(runs, || {
                for i in 0..40 {
                    cv.text(
                        100,
                        120 + i * 14,
                        "The quick brown fox jumps over the lazy dog 0123456789",
                        f,
                        0xFFF2_F4FA,
                    );
                }
            }),
        ));
        results.push((
            "10 icons (vector)",
            time(runs, || {
                for i in 0..10 {
                    icons::draw(&mut cv, Icon::Folder, Rect::new(340 + i * 56, 730, 48, 48));
                }
            }),
        ));
        results.push(("full-screen round rect fill", time(runs, || cv.fill_round_rect(screen, 24, 0x80FF_FFFF))));
    }
    let total: f64 = results.iter().map(|(_, t)| t).sum();
    let full = time(10, || drop(lumen::wallpaper::blur(&wallpaper, W, H, 18)));
    let coarse = time(10, || drop(lumen::wallpaper::blur_coarse(&wallpaper, W, H, 18)));
    println!("login blur: full {full:.2} ms, coarse {coarse:.2} ms (not part of a frame)");
    for (name, t) in &results {
        println!("{name:<30} {t:>8.3} ms");
    }
    println!("{:<30} {total:>8.3} ms", "frame (sum)");
}
