//! Vector app icons drawn from canvas primitives (no image assets).

use crate::canvas::{rgb, Canvas};
use crate::geom::Rect;
use crate::math::sin;
use crate::theme;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Aurora,
    Launcher,
    Files,
    Terminal,
    Notes,
    Calculator,
    Settings,
    Welcome,
    Folder,
    Document,
}

fn radius(size: i32) -> i32 {
    size * 23 / 100
}

/// Draws a sine wave stroke across `r` (the Aurora mark).
pub fn wave(cv: &mut Canvas, r: Rect, periods: i32, thickness: i32, c: u32) {
    let steps = 12 * periods;
    let amp = r.h / 2 - thickness / 2;
    let mid = r.y + r.h / 2;
    let pts: Vec<(i32, i32)> =
        (0..=steps).map(|i| (r.x + r.w * i / steps, mid - amp * sin(i * 1024 * periods / steps) / 16384)).collect();
    cv.polyline(&pts, thickness, c);
}

pub fn draw(cv: &mut Canvas, icon: Icon, r: Rect) {
    let s = r.w;
    let rad = radius(s);
    let u = |v: i32| v * s / 48; // design grid is 48 px
    match icon {
        Icon::Aurora => {
            cv.fill_round_rect_dgradient(r, rad, rgb(0x7C, 0x5C, 0xFF), rgb(0x2D, 0xD4, 0xBF));
            let inner = Rect::new(r.x + u(9), r.y + u(14), s - u(18), s - u(28));
            wave(cv, inner, 2, u(4).max(2), 0xFFFF_FFFF);
            let lower = inner.offset(0, u(9));
            wave(cv, Rect::new(lower.x + u(6), lower.y, lower.w - u(12), lower.h), 1, u(3).max(1), 0xAAFF_FFFF);
        }
        Icon::Launcher => {
            cv.fill_round_rect_dgradient(r, rad, rgb(0x5B, 0x6C, 0xFF), rgb(0xA8, 0x55, 0xF7));
            let d = u(8);
            let gap = u(4);
            let total = 3 * d + 2 * gap;
            let ox = r.x + (s - total) / 2;
            let oy = r.y + (s - total) / 2;
            for i in 0..3 {
                for j in 0..3 {
                    cv.fill_round_rect(Rect::new(ox + i * (d + gap), oy + j * (d + gap), d, d), u(2), 0xF0FF_FFFF);
                }
            }
        }
        Icon::Files | Icon::Folder => {
            if icon == Icon::Files {
                cv.fill_round_rect_vgradient(r, rad, rgb(0xE8, 0xF1, 0xFF), rgb(0xC7, 0xDB, 0xFA));
            }
            let (x0, y0) = if icon == Icon::Files { (r.x + u(8), r.y + u(12)) } else { (r.x + u(3), r.y + u(7)) };
            let fw = if icon == Icon::Files { s - u(16) } else { s - u(6) };
            let fh = if icon == Icon::Files { s - u(22) } else { s - u(12) };
            cv.fill_round_rect(Rect::new(x0, y0, fw * 45 / 100, fh / 3), u(3), rgb(0x3B, 0x82, 0xF6));
            cv.fill_round_rect(Rect::new(x0, y0 + u(4), fw, fh - u(4)), u(4), rgb(0x3B, 0x82, 0xF6));
            cv.fill_round_rect_vgradient(
                Rect::new(x0, y0 + u(8), fw, fh - u(8)),
                u(4),
                rgb(0x6C, 0xAE, 0xFF),
                rgb(0x3E, 0x8B, 0xF8),
            );
        }
        Icon::Terminal => {
            cv.fill_round_rect_vgradient(r, rad, rgb(0x33, 0x38, 0x45), rgb(0x14, 0x17, 0x1F));
            let f = theme::mono(if s >= 40 { 14 } else { 13 });
            cv.text(r.x + u(9), r.y + u(28), ">_", f, rgb(0x7E, 0xE7, 0x87));
        }
        Icon::Notes => {
            cv.fill_round_rect(r, rad, rgb(0xFF, 0xFD, 0xF5));
            let band = Rect::new(r.x, r.y, s, u(14));
            cv.with_clip(band, |cv| cv.fill_round_rect_vgradient(r, rad, rgb(0xFD, 0xD8, 0x5C), rgb(0xF5, 0xA5, 0x24)));
            for i in 0..4 {
                let y = r.y + u(22) + i * u(6);
                cv.fill_rect(
                    Rect::new(r.x + u(9), y, s - u(18) - if i == 3 { u(12) } else { 0 }, u(2).max(1)),
                    0x40000000,
                );
            }
        }
        Icon::Calculator => {
            cv.fill_round_rect_vgradient(r, rad, rgb(0x48, 0x48, 0x54), rgb(0x24, 0x24, 0x2C));
            cv.fill_round_rect(Rect::new(r.x + u(8), r.y + u(7), s - u(16), u(10)), u(3), rgb(0xA7, 0xF3, 0xD0));
            let d = u(8);
            for row in 0..2 {
                for col in 0..3 {
                    let c = if col == 2 { rgb(0xFF, 0x9F, 0x0A) } else { rgb(0x8A, 0x8A, 0x96) };
                    cv.fill_round_rect(
                        Rect::new(r.x + u(8) + col * (d + u(4)), r.y + u(22) + row * (d + u(3)), d, d),
                        d / 2,
                        c,
                    );
                }
            }
        }
        Icon::Settings => {
            cv.fill_round_rect_vgradient(r, rad, rgb(0xB4, 0xBA, 0xC6), rgb(0x5B, 0x63, 0x73));
            let (cx, cy) = r.center();
            let outer = u(13);
            for k in 0..8 {
                let a = k * 128;
                let tx = cx + (outer + u(1)) * sin(a + 256) / 16384;
                let ty = cy + (outer + u(1)) * sin(a) / 16384;
                cv.fill_circle(tx, ty, u(4), 0xFFF4_F4F7);
            }
            cv.fill_circle(cx, cy, outer, 0xFFF4_F4F7);
            cv.fill_circle(cx, cy, u(6), rgb(0x7A, 0x82, 0x92));
        }
        Icon::Welcome => {
            cv.fill_round_rect_dgradient(r, rad, rgb(0xFF, 0x7A, 0x9A), rgb(0xFF, 0xB8, 0x4D));
            let f = theme::ui_bold(if s >= 40 { 20 } else { 14 });
            cv.text_centered(r, "Hi", f, 0xFFFF_FFFF);
        }
        Icon::Document => {
            let page = Rect::new(r.x + u(8), r.y + u(3), s - u(16), s - u(6));
            cv.fill_round_rect(page, u(3), 0xFFFF_FFFF);
            cv.stroke_round_rect(page, u(3), 0x30000000);
            for i in 0..5 {
                let y = page.y + u(10) + i * u(6);
                cv.fill_rect(
                    Rect::new(page.x + u(5), y, page.w - u(10) - if i == 4 { u(8) } else { 0 }, u(2).max(1)),
                    0x38000000,
                );
            }
        }
    }
}

/// Power symbol for buttons.
pub fn power(cv: &mut Canvas, cx: i32, cy: i32, size: i32, c: u32, bg: u32) {
    let r = size / 2;
    let t = (size / 8).max(2);
    cv.fill_circle(cx, cy, r, c);
    cv.fill_circle(cx, cy, r - t, bg);
    cv.fill_rect(Rect::new(cx - t * 2, cy - r - 1, t * 4, r), bg);
    cv.line(cx, cy - r + 1, cx, cy - 1, t, c);
}
