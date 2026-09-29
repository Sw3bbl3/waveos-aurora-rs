//! Reusable controls drawn in the Aurora style.

use crate::canvas::{with_alpha, Canvas};
use crate::font::Font;
use crate::geom::Rect;
use crate::theme::{self, ACCENT};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Secondary,
    Danger,
}

pub fn button(cv: &mut Canvas, r: Rect, label: &str, style: ButtonStyle, hovered: bool) {
    let t = theme::current();
    let font = theme::ui_bold(13);
    match style {
        ButtonStyle::Primary | ButtonStyle::Danger => {
            let base = if style == ButtonStyle::Primary { ACCENT } else { theme::CLOSE };
            let top = if hovered {
                crate::canvas::mix(base, 0xFFFF_FFFF, 40)
            } else {
                crate::canvas::mix(base, 0xFFFF_FFFF, 20)
            };
            cv.fill_round_rect_vgradient(r, 8, top, base);
            cv.text_centered(r, label, font, 0xFFFF_FFFF);
        }
        ButtonStyle::Secondary => {
            cv.fill_round_rect(r, 8, t.control_bg);
            if hovered {
                cv.fill_round_rect(r, 8, t.hover);
            }
            cv.stroke_round_rect(r, 8, t.control_border);
            cv.text_centered(r, label, font, t.text);
        }
    }
}

pub fn text_field(cv: &mut Canvas, r: Rect, text: &str, placeholder: &str, focused: bool, caret: bool) {
    let t = theme::current();
    let font = theme::ui(14);
    cv.fill_round_rect(r, 8, t.control_bg);
    cv.stroke_round_rect(r, 8, if focused { with_alpha(ACCENT, 0xC0) } else { t.control_border });
    let base = r.y + (r.h + font.ascent as i32 + font.descent as i32) / 2;
    let x = r.x + 12;
    let w = if text.is_empty() {
        cv.text(x, base, placeholder, font, t.text_secondary);
        0
    } else {
        cv.text_clipped(x, base, text, font, t.text, r.w - 24)
    };
    if focused && caret {
        cv.fill_rect(Rect::new(x + w + 1, r.y + 8, 2, r.h - 16), ACCENT);
    }
}

/// An on/off switch (about 42×22).
pub fn toggle(cv: &mut Canvas, r: Rect, on: bool) {
    let t = theme::current();
    let track = if on {
        ACCENT
    } else if t.dark {
        0xFF4A_4A55
    } else {
        0xFFD6_D6DD
    };
    cv.fill_round_rect(r, r.h / 2, track);
    let d = r.h - 4;
    let x = if on { r.right() - 2 - d } else { r.x + 2 };
    cv.fill_circle(x + d / 2, r.y + r.h / 2 + 1, d / 2, 0x30000000);
    cv.fill_circle(x + d / 2, r.y + r.h / 2, d / 2, 0xFFFF_FFFF);
}

/// A horizontal slider; `value` in 0..=1000. Returns the knob rectangle.
pub fn slider(cv: &mut Canvas, r: Rect, value: i32, hovered: bool) -> Rect {
    let t = theme::current();
    let v = value.clamp(0, 1000);
    let cy = r.y + r.h / 2;
    let track = Rect::new(r.x, cy - 2, r.w, 4);
    cv.fill_round_rect(track, 2, if t.dark { 0xFF4A_4A55 } else { 0xFFD6_D6DD });
    let filled = Rect::new(r.x, cy - 2, r.w * v / 1000, 4);
    cv.fill_round_rect(filled, 2, ACCENT);
    let kx = r.x + r.w * v / 1000;
    let rad = if hovered { 10 } else { 9 };
    cv.fill_circle(kx, cy + 1, rad, 0x28000000);
    cv.fill_circle(kx, cy, rad, 0xFFFF_FFFF);
    cv.stroke_round_rect(Rect::new(kx - rad, cy - rad, 2 * rad, 2 * rad), rad, t.control_border);
    Rect::new(kx - rad, cy - rad, 2 * rad, 2 * rad)
}

/// Slider value (0..=1000) for pointer x over the slider rectangle `r`.
pub fn slider_value(r: Rect, x: i32) -> i32 {
    ((x - r.x) * 1000 / r.w.max(1)).clamp(0, 1000)
}

/// Segment rectangles of a segmented control.
pub fn segments(r: Rect, n: usize) -> Vec<Rect> {
    let n = n.max(1) as i32;
    let w = (r.w - 4) / n;
    (0..n).map(|i| Rect::new(r.x + 2 + i * w, r.y + 2, w, r.h - 4)).collect()
}

/// A segmented control (like a row of radio buttons).
pub fn segmented(cv: &mut Canvas, r: Rect, labels: &[&str], selected: usize, hovered: Option<usize>) {
    let t = theme::current();
    cv.fill_round_rect(r, 8, if t.dark { 0xFF2E_2E38 } else { 0xFFE6_E6EC });
    let f = theme::ui(12);
    for (i, s) in segments(r, labels.len()).into_iter().enumerate() {
        if i == selected {
            cv.fill_round_rect(s, 6, if t.dark { 0xFF5A_5A68 } else { 0xFFFF_FFFF });
        } else if hovered == Some(i) {
            cv.fill_round_rect(s, 6, t.hover);
        }
        cv.text_centered(s, labels[i], f, t.text);
    }
}

/// A thin progress/usage bar; `value` in 0..=1000.
pub fn progress(cv: &mut Canvas, r: Rect, value: i32, color: u32) {
    let t = theme::current();
    cv.fill_round_rect(r, r.h / 2, if t.dark { 0xFF3A_3A44 } else { 0xFFE3_E3EA });
    let w = (r.w * value.clamp(0, 1000) / 1000).max(if value > 0 { r.h } else { 0 });
    cv.fill_round_rect(Rect::new(r.x, r.y, w, r.h), r.h / 2, color);
}

/// Splits `text` into visual lines no wider than `max_w`, returning byte ranges.
/// Hard newlines always break; long lines wrap at spaces when possible.
pub fn wrap(text: &str, font: Font, max_w: i32) -> Vec<(usize, usize)> {
    let mut lines = Vec::new();
    for (i, line) in text.split('\n').scan(0usize, |pos, l| {
        let s = *pos;
        *pos += l.len() + 1;
        Some((s, l))
    }) {
        let mut ls = i;
        let mut w = 0;
        let mut last_space: Option<usize> = None;
        let mut idx = i;
        for ch in line.chars() {
            let a = font.advance(ch);
            if w + a > max_w && idx > ls {
                let brk = match last_space {
                    Some(sp) if sp > ls => sp + 1,
                    _ => idx,
                };
                lines.push((ls, brk));
                w = font.width(&text[brk..idx]);
                ls = brk;
                last_space = None;
            }
            if ch == ' ' {
                last_space = Some(idx);
            }
            w += a;
            idx += ch.len_utf8();
        }
        lines.push((ls, i + line.len()));
    }
    lines
}
