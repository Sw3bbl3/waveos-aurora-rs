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
