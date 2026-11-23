//! Controller button glyphs in each family's own symbols (Xbox letters,
//! PlayStation shapes, Nintendo letters in their positions), and the small
//! status symbols of Wave Play's strip.

use super::pad::Button;
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::theme;
use aurora_abi::gamepad as abi;

/// What `b` is called on a `family` pad, when it is a letter.
fn letter(family: u32, b: Button) -> Option<&'static str> {
    Some(match (family, b) {
        (abi::NINTENDO, Button::Accept) => "A",
        (abi::NINTENDO, Button::Back) => "B",
        (abi::NINTENDO, Button::X) => "Y",
        (abi::NINTENDO, Button::Y) => "X",
        (abi::PLAYSTATION, Button::Accept | Button::Back | Button::X | Button::Y) => return None,
        (_, Button::Accept) => "A",
        (_, Button::Back) => "B",
        (_, Button::X) => "X",
        (_, Button::Y) => "Y",
        (abi::PLAYSTATION, Button::Lb) => "L1",
        (abi::PLAYSTATION, Button::Rb) => "R1",
        (abi::NINTENDO, Button::Lb) => "L",
        (abi::NINTENDO, Button::Rb) => "R",
        (_, Button::Lb) => "LB",
        (_, Button::Rb) => "RB",
        (abi::PLAYSTATION, Button::Lt) => "L2",
        (abi::PLAYSTATION, Button::Rt) => "R2",
        (_, Button::Lt) => "LT",
        (_, Button::Rt) => "RT",
        _ => return None,
    })
}

/// Draws the glyph for `b` centred at (cx, cy), `size` points across, in
/// `fg` on a disc of `bg`. Returns its width.
#[allow(clippy::too_many_arguments)] // a glyph: which, where, how big, its two colours
pub fn button(cv: &mut Canvas, family: u32, b: Button, cx: i32, cy: i32, size: i32, fg: u32, bg: u32) -> i32 {
    let r = size / 2;
    match b {
        Button::Lb | Button::Rb | Button::Lt | Button::Rt => {
            let label = letter(family, b).unwrap_or("");
            let f = theme::ui_bold((size * 9 / 20).max(9) as u16);
            let w = (f.width(label) + size * 2 / 3).max(size);
            let pill = Rect::new(cx - w / 2, cy - r, w, size);
            cv.fill_round_rect(pill, size / 3, bg);
            cv.text_centered(pill, label, f, fg);
            return w;
        }
        Button::Menu | Button::View | Button::Guide => {
            cv.fill_circle(cx, cy, r, bg);
            let u = (size / 9).max(1);
            match b {
                // Three lines.
                Button::Menu => {
                    for k in -1..=1 {
                        cv.fill_round_rect(Rect::new(cx - 3 * u, cy + k * 2 * u - u / 2, 6 * u, u.max(2)), 1, fg);
                    }
                }
                // Two overlapping windows.
                Button::View => {
                    cv.stroke_round_rect(Rect::new(cx - 3 * u, cy - 3 * u, 4 * u, 4 * u), 1, fg);
                    cv.fill_round_rect(Rect::new(cx - u, cy - u, 4 * u, 4 * u), 1, fg);
                }
                // A house.
                _ => {
                    let pts = [(cx - 3 * u, cy), (cx, cy - 3 * u), (cx + 3 * u, cy)];
                    cv.polyline(&pts, u.max(2), fg);
                    cv.fill_rect(Rect::new(cx - 2 * u, cy, 4 * u, 3 * u), fg);
                }
            }
            return size;
        }
        _ => {}
    }
    cv.fill_circle(cx, cy, r, bg);
    if let Some(l) = letter(family, b) {
        let f = theme::ui_bold((size * 11 / 20).max(9) as u16);
        cv.text_centered(Rect::new(cx - r, cy - r, size, size), l, f, fg);
        return size;
    }
    // PlayStation shapes.
    let s = size * 2 / 9;
    let w = (size / 10).max(2);
    match b {
        Button::Accept => {
            cv.line(cx - s, cy - s, cx + s, cy + s, w, fg);
            cv.line(cx - s, cy + s, cx + s, cy - s, w, fg);
        }
        Button::Back => cv.stroke_arc(cx * 16, cy * 16, s * 16 + 8, w * 16, 0, 1024, fg),
        Button::X => cv.stroke_round_rect(Rect::new(cx - s, cy - s, 2 * s, 2 * s), 1, fg),
        _ => {
            let pts = [(cx, cy - s), (cx + s, cy + s * 3 / 4), (cx - s, cy + s * 3 / 4), (cx, cy - s)];
            cv.polyline(&pts, w, fg);
        }
    }
    size
}

/// A small controller silhouette (the strip shows one per pad).
pub fn controller(cv: &mut Canvas, cx: i32, cy: i32, w: i32, c: u32) {
    let h = w * 3 / 5;
    let body = Rect::new(cx - w / 2, cy - h / 2, w, h * 3 / 4);
    cv.fill_round_rect(body, h / 3, c);
    // The grips.
    cv.fill_circle(cx - w / 2 + h / 4, cy + h / 6, h / 3, c);
    cv.fill_circle(cx + w / 2 - h / 4, cy + h / 6, h / 3, c);
    // D-pad and buttons cut out of it.
    let hole = 0xC000_0000;
    let u = (w / 12).max(1);
    cv.fill_rect(Rect::new(cx - w / 4 - u, cy - h / 6 - u / 2, 3 * u, u), hole);
    cv.fill_rect(Rect::new(cx - w / 4, cy - h / 6 - 3 * u / 2, u, 3 * u), hole);
    cv.fill_circle(cx + w / 4, cy - h / 6, u, hole);
}

/// Network bars: `bars` of 3 lit.
pub fn network(cv: &mut Canvas, x: i32, cy: i32, connected: bool, c: u32) {
    for k in 0..3 {
        let h = 5 + k * 4;
        let lit = connected || k == 0;
        cv.fill_round_rect(
            Rect::new(x + k * 6, cy + 7 - h, 4, h),
            1,
            if lit { c } else { c & 0x00FF_FFFF | 0x4000_0000 },
        );
    }
}
