//! Times Wave Play's frames at 1920×1080 on the host: the drawing the
//! compositor does for each stage of its transitions (the same lumen calls,
//! with stand-in pictures). `cargo run --release -p lumen --example play_bench`
//! prints the median of many runs; 16.7 ms is one frame at 60 Hz.

use lumen::canvas::Canvas;
use lumen::font::{self, Face};
use lumen::geom::Rect;
use lumen::layer;
use lumen::theme;
use std::time::Instant;

const W: i32 = 1920;
const H: i32 = 1080;

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[v.len() / 2]
}

fn time(runs: usize, mut f: impl FnMut()) -> f64 {
    f();
    median(
        (0..runs)
            .map(|_| {
                let t = Instant::now();
                f();
                t.elapsed().as_secs_f64() * 1000.0
            })
            .collect(),
    )
}

/// A smooth stand-in picture (wallpaper, window, icon).
fn picture(w: i32, h: i32, seed: u32) -> Vec<u32> {
    (0..w * h)
        .map(|i| {
            let (x, y) = ((i % w) as u32, (i / w) as u32);
            let r = ((x * 255 / w as u32) ^ seed) & 0xFF;
            let g = (y * 255 / h as u32) & 0xFF;
            0xFF00_0000 | (r << 16) | (g << 8) | (seed & 0xFF)
        })
        .collect()
}

fn main() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/");
    for (face, file) in [
        (Face::Regular, "Manrope-Medium.ttf"),
        (Face::SemiBold, "Manrope-Bold.ttf"),
        (Face::Bold, "Manrope-ExtraBold.ttf"),
    ] {
        let data = std::fs::read(format!("{root}{file}")).expect("font");
        font::install(face, Box::leak(data.into_boxed_slice()));
    }
    theme::set_dark(true);
    let screen = Rect::new(0, 0, W, H);
    let base = picture(W, H, 0x40);
    let blurred = lumen::wallpaper::blur_coarse(&base, W, H, 18);
    let backdrop = lumen::wallpaper::blur(&picture(W / 6, H / 6, 0x80), W / 6, H / 6, 12);
    let windows = [
        (Rect::new(140, 120, 1180, 760), picture(1180, 760, 7)),
        (Rect::new(700, 260, 1000, 700), picture(1000, 700, 9)),
    ];
    let icon = picture(256, 256, 0x33);
    let field: Vec<u32> = (0..16).map(|i| 0xFF20_3040 + i * 0x0A0503).collect();
    let mut buf = vec![0u32; (W * H) as usize];
    let runs = 40;
    let tile = 221;

    let tiles = |cv: &mut Canvas, rise: i32, alpha: u32| {
        for c in 0..7 {
            let r = Rect::new(124 + c * (tile + 28), 560 + rise, tile, tile);
            cv.shadow(r, 18, 22, 10, 90 * alpha / 255);
            layer::draw_scaled(cv, &field, 4, 4, r, alpha, 18);
            let s = tile * 54 / 100;
            cv.draw_image(&icon, 256, 256, Rect::new(r.x + (tile - s) / 2, r.y + (tile - s) / 2, s, s), alpha);
            cv.stroke_round_rect(r, 18, 0x30FF_FFFF);
        }
        cv.text(124, 470, "Doom", theme::display(50), 0xFFFF_FFFF);
        cv.text(124, 510, "Freedoom or your own WAD files", theme::ui(20), 0xB3FF_FFFF);
    };

    let mut results = Vec::new();
    {
        let mut cv = Canvas::new(&mut buf, W, H);
        results.push((
            "backdrop, filtered up (1/6 size)",
            time(runs, || {
                layer::draw_part(
                    &mut cv,
                    &backdrop,
                    (W / 6) as u32,
                    (H / 6) as u32,
                    (256, 256, (W / 6 - 2) * 256, (H / 6 - 2) * 256),
                    screen,
                    255,
                    0,
                )
            }),
        ));
        results
            .push(("wallpaper → blurred cross-fade", time(runs, || layer::crossfade(&mut cv, &base, &blurred, 128))));
        results.push((
            "two windows receding (scaled, faded, shadows)",
            time(runs, || {
                for (r, px) in &windows {
                    let s = Rect::new(r.x + r.w / 20, r.y + r.h / 20, r.w * 9 / 10, r.h * 9 / 10);
                    cv.shadow(s, 16, 30, 10, 80);
                    layer::draw_scaled(&mut cv, px, r.w as u32, r.h as u32, s, 160, 16);
                }
            }),
        ));
        results.push(("seven tiles rising, title", time(runs, || tiles(&mut cv, 40, 180))));
    }
    // Whole frames.
    let mut frame = |name: &'static str, results: &mut Vec<(&str, f64)>, f: &mut dyn FnMut(&mut Canvas)| {
        let mut cv = Canvas::new(&mut buf, W, H);
        results.push((name, time(runs, || f(&mut cv))));
    };
    frame("FRAME morph, early (desktop receding)", &mut results, &mut |cv| {
        layer::crossfade(cv, &base, &blurred, 160);
        layer::draw_part(
            cv,
            &backdrop,
            (W / 6) as u32,
            (H / 6) as u32,
            (256, 256, (W / 6 - 2) * 256, (H / 6 - 2) * 256),
            screen,
            60,
            0,
        );
        for (r, px) in &windows {
            let s = Rect::new(r.x + r.w / 40, r.y + r.h / 40, r.w * 19 / 20, r.h * 19 / 20);
            cv.shadow(s, 16, 30, 10, 100);
            layer::draw_scaled(cv, px, r.w as u32, r.h as u32, s, 200, 16);
        }
    });
    frame("FRAME morph, middle (blur to backdrop, tiles rising)", &mut results, &mut |cv| {
        cv.blit_same(&blurred, screen);
        layer::draw_part(
            cv,
            &backdrop,
            (W / 6) as u32,
            (H / 6) as u32,
            (256, 256, (W / 6 - 2) * 256, (H / 6 - 2) * 256),
            screen,
            160,
            0,
        );
        let (r, px) = &windows[0];
        layer::draw_scaled(cv, px, r.w as u32, r.h as u32, Rect::new(r.x + 50, r.y + 40, r.w - 100, r.h - 80), 60, 16);
        tiles(cv, 60, 120);
    });
    frame("FRAME home, whole screen", &mut results, &mut |cv| {
        layer::draw_part(
            cv,
            &backdrop,
            (W / 6) as u32,
            (H / 6) as u32,
            (256, 256, (W / 6 - 2) * 256, (H / 6 - 2) * 256),
            screen,
            255,
            0,
        );
        layer::scrim(cv, Rect::new(0, H * 3 / 4, W, H / 4), 0, 0, 150);
        tiles(cv, 0, 255);
    });
    frame("FRAME focus move (the row band only)", &mut results, &mut |cv| {
        cv.clip = Rect::new(0, 380, W, 700);
        layer::draw_part(
            cv,
            &backdrop,
            (W / 6) as u32,
            (H / 6) as u32,
            (256, 256, (W / 6 - 2) * 256, (H / 6 - 2) * 256),
            screen,
            255,
            0,
        );
        tiles(cv, 0, 255);
        cv.clip = screen;
    });
    let sharp = base.clone();
    let small = layer::shrink(&sharp, W as u32, H as u32, (W / 4) as u32, (H / 4) as u32);
    frame("FRAME quick overlay opening", &mut results, &mut |cv| {
        cv.blit_same(&sharp, screen);
        layer::draw_scaled(cv, &small, (W / 4) as u32, (H / 4) as u32, screen, 140, 0);
        cv.fill_rect(screen, 0x4000_0000);
        let p = Rect::new(40, 32, 440, H - 64);
        cv.shadow(p, 26, 40, 14, 128);
        cv.fill_round_rect(p, 26, 0xF214_181F);
    });
    frame("FRAME launch zoom (card at half size over the home)", &mut results, &mut |cv| {
        layer::draw_part(
            cv,
            &backdrop,
            (W / 6) as u32,
            (H / 6) as u32,
            (256, 256, (W / 6 - 2) * 256, (H / 6 - 2) * 256),
            screen,
            255,
            0,
        );
        tiles(cv, 0, 255);
        let r = Rect::new(400, 250, 1000, 560);
        cv.shadow(r, 9, 30, 12, 70);
        layer::draw_scaled(cv, &field, 4, 4, r, 255, 9);
        cv.draw_image(&icon, 256, 256, Rect::new(r.x + 380, r.y + 160, 240, 240), 255);
    });
    for (name, ms) in &results {
        println!("{:>8.2} ms  {}", ms, name);
    }
}
