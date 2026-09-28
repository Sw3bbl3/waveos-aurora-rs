use super::{App, AppKind, Env};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::canvas::{mix, rgb, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use alloc::format;
use alloc::string::{String, ToString};

const KEYS: [[&str; 4]; 5] =
    [["C", "±", "%", "÷"], ["7", "8", "9", "×"], ["4", "5", "6", "−"], ["1", "2", "3", "+"], ["0", "", ".", "="]];

pub struct Calculator {
    display: String,
    acc: Option<f64>,
    op: Option<char>,
    fresh: bool,
    hovered: Option<(usize, usize)>,
    pressed: Option<(usize, usize)>,
}

impl Calculator {
    pub fn new() -> Self {
        Self { display: "0".into(), acc: None, op: None, fresh: true, hovered: None, pressed: None }
    }

    fn value(&self) -> f64 {
        self.display.parse().unwrap_or(0.0)
    }

    fn show(&mut self, v: f64) {
        self.display = if !v.is_finite() {
            "Error".into()
        } else if v == (v as i64) as f64 && v.abs() < 1e15 {
            format!("{}", v as i64)
        } else {
            let s = format!("{:.10}", v);
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        };
    }

    fn apply(&mut self) {
        if let (Some(a), Some(op)) = (self.acc, self.op) {
            let b = self.value();
            let r = match op {
                '+' => a + b,
                '-' => a - b,
                '*' => a * b,
                '/' => {
                    if b == 0.0 {
                        f64::NAN
                    } else {
                        a / b
                    }
                }
                _ => b,
            };
            self.show(r);
        }
        self.acc = None;
        self.op = None;
    }

    fn press(&mut self, key: &str) {
        match key {
            "C" => *self = Self { hovered: self.hovered, ..Self::new() },
            "±" => {
                let v = -self.value();
                self.show(v);
            }
            "%" => {
                let v = self.value() / 100.0;
                self.show(v);
            }
            "." => {
                if self.fresh {
                    self.display = "0".into();
                    self.fresh = false;
                }
                if !self.display.contains('.') {
                    self.display.push('.');
                }
            }
            "=" => {
                self.apply();
                self.fresh = true;
            }
            "+" | "−" | "×" | "÷" => {
                if self.op.is_some() && !self.fresh {
                    self.apply();
                }
                self.acc = Some(self.value());
                self.op = Some(match key {
                    "+" => '+',
                    "−" => '-',
                    "×" => '*',
                    _ => '/',
                });
                self.fresh = true;
            }
            d => {
                if self.fresh || self.display == "0" || self.display == "Error" {
                    self.display.clear();
                    self.fresh = false;
                }
                if self.display.len() < 16 {
                    self.display.push_str(d);
                }
            }
        }
    }

    fn cell(area: Rect, row: usize, col: usize) -> Rect {
        let pad = 14;
        let gap = 10;
        let top = area.y + 110;
        let cw = (area.w - 2 * pad - 3 * gap) / 4;
        let ch = (area.bottom() - pad - top - 4 * gap) / 5;
        let mut r = Rect::new(area.x + pad + col as i32 * (cw + gap), top + row as i32 * (ch + gap), cw, ch);
        if row == 4 && col == 0 {
            r.w = cw * 2 + gap;
        }
        r
    }

    fn key_at(area: Rect, x: i32, y: i32) -> Option<(usize, usize)> {
        for (r, row) in KEYS.iter().enumerate() {
            for (c, k) in row.iter().enumerate() {
                if !k.is_empty() && Self::cell(area, r, c).contains(x, y) {
                    return Some((r, c));
                }
            }
        }
        None
    }
}

impl App for Calculator {
    fn kind(&self) -> AppKind {
        AppKind::Calculator
    }
    fn size(&self) -> (i32, i32) {
        (300, 440)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        let bg = if t.dark { rgb(0x1C, 0x1C, 0x22) } else { rgb(0xF2, 0xF2, 0xF6) };
        cv.fill_rect_round_bottom(area, theme::WINDOW_RADIUS, bg);
        // Pending operation hint.
        if let (Some(a), Some(op)) = (self.acc, self.op) {
            let mut tmp = Calculator::new();
            tmp.show(a);
            let sym = match op {
                '+' => "+",
                '-' => "−",
                '*' => "×",
                _ => "÷",
            };
            let hint = format!("{} {}", tmp.display, sym);
            let f = theme::ui(14);
            cv.text(area.right() - 20 - f.width(&hint), area.y + 36, &hint, f, t.text_secondary);
        }
        let f = theme::ui(if self.display.len() > 11 { 28 } else { 40 });
        let w = f.width(&self.display);
        cv.text(area.right() - 20 - w, area.y + 88, &self.display, f, t.text);

        for (r, row) in KEYS.iter().enumerate() {
            for (c, k) in row.iter().enumerate() {
                if k.is_empty() {
                    continue;
                }
                let cell = Self::cell(area, r, c);
                let (base, fg) = if c == 3 {
                    (rgb(0xFF, 0x9F, 0x0A), 0xFFFF_FFFF)
                } else if r == 0 {
                    (if t.dark { rgb(0x5A, 0x5A, 0x66) } else { rgb(0xD4, 0xD4, 0xDC) }, t.text)
                } else {
                    (if t.dark { rgb(0x36, 0x36, 0x40) } else { 0xFFFF_FFFF }, t.text)
                };
                let mut color = base;
                if self.pressed == Some((r, c)) {
                    color = mix(base, 0xFF00_0000, 40);
                } else if self.hovered == Some((r, c)) {
                    color = mix(base, 0xFFFF_FFFF, 36);
                }
                cv.fill_round_rect(cell, cell.h.min(cell.w) / 2, color);
                if c != 3 && r != 0 && !t.dark {
                    cv.stroke_round_rect(cell, cell.h.min(cell.w) / 2, 0x14000000);
                }
                cv.text_centered(cell, k, theme::ui(20), fg);
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        if let Some((r, c)) = Self::key_at(area, x, y) {
            self.press(KEYS[r][c]);
            self.pressed = Some((r, c));
            return true;
        }
        false
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = Self::key_at(area, x, y);
        let changed = self.hovered != h || self.pressed.is_some();
        self.hovered = h;
        self.pressed = None;
        changed
    }

    fn key(&mut self, ev: &KeyEvent, _env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        let key = match (ev.code, ev.ch) {
            (KeyCode::Enter, _) => "=",
            (KeyCode::Escape, _) => "C",
            (KeyCode::Backspace, _) => {
                if !self.fresh && self.display.len() > 1 {
                    self.display.pop();
                } else {
                    self.display = "0".into();
                    self.fresh = true;
                }
                return true;
            }
            (_, Some(c)) => match c {
                '0'..='9' => {
                    let s = c.to_string();
                    self.press(&s);
                    return true;
                }
                '.' | ',' => ".",
                '+' => "+",
                '-' => "−",
                '*' | 'x' => "×",
                '/' => "÷",
                '=' => "=",
                '%' => "%",
                'c' | 'C' => "C",
                _ => return false,
            },
            _ => return false,
        };
        self.press(key);
        true
    }
}
