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
    Preview,
    Paint,
    Clock,
    Activity,
    /// The web browser: a globe riding a wave.
    Surf,
    /// A picture file (PNG/BMP) in Files.
    Picture,
    Trash,
    TrashFull,
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
        Icon::Preview
        | Icon::Paint
        | Icon::Clock
        | Icon::Activity
        | Icon::Surf
        | Icon::Picture
        | Icon::Trash
        | Icon::TrashFull => draw_more(cv, icon, r),
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

/// A landscape: sky, sun and two hills, clipped to `r` with corner radius `rad`.
fn landscape(cv: &mut Canvas, r: Rect, rad: i32, u: &dyn Fn(i32) -> i32) {
    cv.fill_round_rect_vgradient(r, rad, rgb(0x6C, 0xC4, 0xFF), rgb(0xC8, 0xEC, 0xFF));
    cv.with_clip(r, |cv| {
        cv.fill_circle(r.x + r.w * 7 / 10, r.y + r.h * 3 / 10, u(5).max(2), rgb(0xFF, 0xD3, 0x4D));
        let hill = |cx: i32, cy: i32, rr: i32, c: u32, cv: &mut Canvas| cv.fill_circle(cx, cy, rr, c);
        hill(r.x + r.w / 4, r.bottom() + r.h / 5, r.h * 3 / 5, rgb(0x34, 0xC7, 0x59), cv);
        hill(r.x + r.w * 3 / 4, r.bottom() + r.h / 3, r.h * 3 / 5, rgb(0x24, 0xA1, 0x48), cv);
    });
}

/// Draws the icons added with the M4 apps.
fn draw_more(cv: &mut Canvas, icon: Icon, r: Rect) {
    let s = r.w;
    let rad = radius(s);
    let u = |v: i32| v * s / 48;
    match icon {
        Icon::Preview => {
            cv.fill_round_rect_vgradient(r, rad, rgb(0xF4, 0xF6, 0xFA), rgb(0xD9, 0xDE, 0xE8));
            let photo = Rect::new(r.x + u(8), r.y + u(10), s - u(16), s - u(20));
            cv.fill_round_rect(photo.inset(-u(2)), u(4), 0xFFFF_FFFF);
            landscape(cv, photo, u(3), &u);
        }
        Icon::Picture => {
            let page = Rect::new(r.x + u(5), r.y + u(9), s - u(10), s - u(18));
            cv.fill_round_rect(page.inset(-u(2)), u(4), 0xFFFF_FFFF);
            cv.stroke_round_rect(page.inset(-u(2)), u(4), 0x30000000);
            landscape(cv, page, u(2), &u);
        }
        Icon::Paint => {
            cv.fill_round_rect_dgradient(r, rad, rgb(0xFF, 0xF3, 0xE0), rgb(0xFF, 0xD6, 0xA5));
            let (cx, cy) = r.center();
            // Palette: a disc with paint dots.
            cv.fill_circle(cx - u(2), cy + u(1), u(15), 0xFFFF_FFFF);
            cv.fill_circle(cx + u(6), cy + u(8), u(4), rgb(0xFF, 0xE8, 0xC8));
            let dots =
                [(-9, -3, 0xFF3B30u32), (-3, -10, 0xFFCC00), (6, -8, 0x34C759), (9, 0, 0x007AFF), (-8, 7, 0xAF52DE)];
            for (dx, dy, c) in dots {
                cv.fill_circle(cx - u(2) + u(dx), cy + u(1) + u(dy), u(3).max(2), 0xFF00_0000 | c);
            }
            // Brush.
            cv.line(cx + u(4), cy + u(2), cx + u(17), cy - u(14), u(3).max(2), rgb(0x8B, 0x5A, 0x2B));
            cv.fill_circle(cx + u(4), cy + u(2), u(3).max(2), rgb(0x1D, 0x1D, 0x24));
        }
        Icon::Clock => {
            cv.fill_round_rect_vgradient(r, rad, rgb(0x2C, 0x2C, 0x34), rgb(0x10, 0x10, 0x16));
            let (cx, cy) = r.center();
            let rr = u(17);
            cv.fill_circle(cx, cy, rr, 0xFFFF_FFFF);
            for k in 0..12 {
                let a = k * 1024 / 12;
                let (x0, y0) = (cx + (rr - u(2)) * sin(a + 256) / 16384, cy + (rr - u(2)) * sin(a) / 16384);
                let (x1, y1) = (cx + (rr - u(4)) * sin(a + 256) / 16384, cy + (rr - u(4)) * sin(a) / 16384);
                cv.line(x0, y0, x1, y1, if k % 3 == 0 { 2 } else { 1 }, 0xFF1D_1D24);
            }
            // 10:10, the watchmaker's smile.
            let hand = |a: i32, len: i32, w: i32, c: u32, cv: &mut Canvas| {
                cv.line(cx, cy, cx + len * sin(a + 256) / 16384, cy + len * sin(a) / 16384, w, c)
            };
            hand(-256 + 1024 * 10 / 12 + 1024 * 10 / 60 / 12, u(9), u(3).max(2), 0xFF1D_1D24, cv);
            hand(-256 + 1024 * 10 / 60, u(13), u(2).max(1), 0xFF1D_1D24, cv);
            hand(-256 + 1024 * 40 / 60, u(14), 1, rgb(0xFF, 0x95, 0x00), cv);
            cv.fill_circle(cx, cy, u(2).max(2), rgb(0xFF, 0x95, 0x00));
        }
        Icon::Activity => {
            cv.fill_round_rect_vgradient(r, rad, rgb(0x1F, 0x24, 0x2E), rgb(0x0B, 0x0E, 0x14));
            let g = Rect::new(r.x + u(7), r.y + u(12), s - u(14), s - u(24));
            for i in 1..4 {
                cv.fill_rect(Rect::new(g.x, g.y + g.h * i / 4, g.w, 1), 0x30FF_FFFF);
            }
            let ys = [70, 55, 62, 30, 42, 18, 36, 25, 50, 40];
            let pts: Vec<(i32, i32)> =
                ys.iter().enumerate().map(|(i, &v)| (g.x + g.w * i as i32 / 9, g.y + g.h * v / 80)).collect();
            cv.polyline(&pts, u(3).max(2), rgb(0x30, 0xD1, 0x58));
        }
        Icon::Surf => {
            cv.fill_round_rect_dgradient(r, rad, rgb(0x0A, 0x6C, 0xFF), rgb(0x2D, 0xD4, 0xBF));
            let (cx, cy) = r.center();
            let rr = u(15);
            cv.fill_circle(cx, cy, rr, 0xFFFF_FFFF);
            let blue = rgb(0x0A, 0x6C, 0xFF);
            // Meridian and equator.
            cv.stroke_round_rect(
                Rect::new(cx - u(6), cy - rr + 1, u(12), rr * 2 - 2),
                u(6),
                blue & 0x00FF_FFFF | 0x9000_0000,
            );
            cv.fill_rect(
                Rect::new(cx - rr + u(2), cy - u(5), rr * 2 - u(4), u(1).max(1)),
                blue & 0x00FF_FFFF | 0x9000_0000,
            );
            // The wave across its lower half.
            wave(cv, Rect::new(cx - rr + u(3), cy + u(2), rr * 2 - u(6), u(8)), 1, u(3).max(2), blue);
        }
        Icon::Trash | Icon::TrashFull => {
            let body = Rect::new(r.x + u(11), r.y + u(12), s - u(22), s - u(16));
            cv.fill_round_rect_vgradient(body, u(4), 0xE8F4_F6FA, 0xD0C8_CCD6);
            cv.stroke_round_rect(body, u(4), 0x50000000);
            for i in 1..4 {
                let x = body.x + body.w * i / 4;
                cv.fill_rect(Rect::new(x, body.y + u(5), 1, body.h - u(10)), 0x40000000);
            }
            if icon == Icon::TrashFull {
                let paper = Rect::new(body.x + u(3), r.y + u(6), body.w - u(6), u(9));
                cv.fill_round_rect(paper, u(2), 0xFFFF_FFFF);
                cv.stroke_round_rect(paper, u(2), 0x40000000);
            }
            cv.fill_round_rect(Rect::new(r.x + u(9), r.y + u(9), s - u(18), u(4)), u(2), 0xF0B8_BEC8);
        }
        _ => {}
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

/// Speaker glyph fitted to `r`: 0–3 sound waves for `level` (0–100), or a
/// cross when `level` is `None` (muted).
pub fn speaker(cv: &mut Canvas, r: Rect, level: Option<u32>, c: u32) {
    let h = r.h;
    let cy = r.y + h / 2;
    // Body, then a cone widening to the right.
    let body_w = (h * 2 / 9).max(2);
    let body_h = (h * 2 / 5).max(3);
    let x0 = r.x + h / 12;
    cv.fill_rect(Rect::new(x0, cy - body_h / 2, body_w, body_h), c);
    let cone_w = (h * 3 / 10).max(3);
    for i in 0..cone_w {
        let half = body_h / 2 + (h * 2 / 5 - body_h / 2) * (i + 1) / cone_w;
        cv.fill_rect(Rect::new(x0 + body_w + i, cy - half, 1, half * 2), c);
    }
    let cx = x0 + body_w + cone_w;
    let stroke = (h / 11).max(1);
    match level {
        None => {
            let s = h / 5;
            let mx = cx + h / 4 + s;
            cv.line(mx - s, cy - s, mx + s, cy + s, stroke, c);
            cv.line(mx - s, cy + s, mx + s, cy - s, stroke, c);
        }
        Some(level) => {
            let waves = match level {
                0 => 0,
                1..=33 => 1,
                34..=66 => 2,
                _ => 3,
            };
            for k in 1..=waves {
                let radius = h * (1 + 2 * k) / 10;
                // An arc from −50° to +50° (1024 phase units per turn).
                let pts: Vec<(i32, i32)> = (-142..=142)
                    .step_by(20)
                    .map(|p| (cx + radius * sin(p + 256) / 16384, cy + radius * sin(p) / 16384))
                    .collect();
                cv.polyline(&pts, stroke, c);
            }
        }
    }
}

/// Battery glyph in `r` (about 2:1): an outline with its terminal, filled to
/// `percent`; `fill` colours the charge (e.g. red when low). `charging` adds a bolt.
pub fn battery(cv: &mut Canvas, r: Rect, percent: u32, charging: bool, c: u32, fill: u32) {
    let tip = (r.w / 12).max(2);
    let body = Rect::new(r.x, r.y, r.w - tip - 1, r.h);
    let radius = (r.h / 4).max(2);
    cv.stroke_round_rect(body, radius, c);
    cv.fill_round_rect(Rect::new(body.right() + 1, r.y + r.h / 3, tip, r.h - 2 * (r.h / 3)), 1, c);
    let inner = body.inset(2);
    let w = inner.w * percent.min(100) as i32 / 100;
    if w > 0 {
        cv.fill_round_rect(Rect::new(inner.x, inner.y, w.max(2), inner.h), (radius - 1).max(1), fill);
    }
    if charging {
        // A lightning bolt across the middle.
        let (cx, cy, s) = (body.x + body.w / 2, body.y + body.h / 2, body.h * 2 / 5);
        let bolt = [(cx + s / 3, cy - s), (cx - s / 2, cy + s / 6), (cx + s / 2, cy - s / 6), (cx - s / 3, cy + s)];
        cv.polyline(&bolt, (r.h / 8).max(2), 0xFF1C_1C1E);
        cv.polyline(&bolt, (r.h / 14).max(1), 0xFFFF_FFFF);
    }
}
