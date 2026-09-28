use super::{App, AppKind, Env};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::fs;
use crate::gui::canvas::Canvas;
use crate::gui::font::Font;
use crate::gui::geom::Rect;
use crate::gui::theme::{self, ACCENT};
use crate::gui::widgets::{button, wrap, ButtonStyle};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const TOOLBAR_H: i32 = 44;
const PAD: i32 = 18;
const LINE_H: i32 = 21;

pub struct Notes {
    text: String,
    caret: usize,
    path: Option<String>,
    dirty: bool,
    scroll: i32,
    caret_on: bool,
    last_blink: u64,
    status: Option<(String, u64)>,
    save_hover: bool,
    /// Keep the caret in view on the next layout (off while scrolling with the wheel).
    follow: bool,
    /// Visual lines from the last layout (byte ranges), used for hit-testing.
    layout: Vec<(usize, usize)>,
}

impl Notes {
    pub fn new(path: Option<String>) -> Self {
        let text =
            path.as_deref().and_then(fs::read).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        Self {
            caret: text.len(),
            text,
            path,
            dirty: false,
            scroll: 0,
            caret_on: true,
            last_blink: 0,
            status: None,
            save_hover: false,
            follow: true,
            layout: Vec::new(),
        }
    }

    fn font() -> Font {
        theme::ui(14)
    }

    fn text_area(area: Rect) -> Rect {
        Rect::new(area.x + PAD, area.y + TOOLBAR_H + 10, area.w - 2 * PAD, area.h - TOOLBAR_H - 20)
    }

    fn save_button(area: Rect) -> Rect {
        Rect::new(area.right() - 92, area.y + 6, 76, 30)
    }

    fn name(&self) -> String {
        match &self.path {
            Some(p) => String::from(p.rsplit('/').next().unwrap_or(p)),
            None => String::from("Untitled"),
        }
    }

    fn save(&mut self, now: u64) {
        let path = self.path.clone().unwrap_or_else(|| fs::unique_name("/Documents", "Untitled", ".txt"));
        if fs::write(&path, self.text.as_bytes()) {
            self.status = Some((format!("Saved to {path}"), now));
            self.path = Some(path);
            self.dirty = false;
        } else {
            self.status = Some((String::from("Could not save"), now));
        }
    }

    fn prev_char(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map(|(p, _)| p).unwrap_or(0)
    }

    fn next_char(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }

    fn line_of(&self, pos: usize) -> usize {
        self.layout.iter().rposition(|&(s, _)| s <= pos).unwrap_or(0)
    }

    /// Byte offset in visual line `line` closest to pixel column `px`.
    fn offset_at(&self, line: usize, px: i32) -> usize {
        let Some(&(s, e)) = self.layout.get(line) else { return self.text.len() };
        let f = Self::font();
        let mut x = 0;
        for (i, ch) in self.text[s..e].char_indices() {
            let a = f.advance(ch);
            if px < x + a / 2 {
                return s + i;
            }
            x += a;
        }
        e
    }

    fn caret_x(&self) -> i32 {
        let line = self.line_of(self.caret);
        let (s, _) = self.layout.get(line).copied().unwrap_or((0, 0));
        Self::font().width(&self.text[s..self.caret.max(s)])
    }
}

impl App for Notes {
    fn kind(&self) -> AppKind {
        AppKind::Notes
    }
    fn title(&self) -> String {
        format!("{}{} — Notes", self.name(), if self.dirty { " •" } else { "" })
    }
    fn size(&self) -> (i32, i32) {
        (560, 440)
    }
    fn single_instance(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let f = Self::font();
        // Toolbar.
        cv.fill_rect(Rect::new(area.x, area.y + TOOLBAR_H - 1, area.w, 1), t.separator);
        let label = match &self.status {
            Some((s, _)) => s.clone(),
            None => self.path.clone().unwrap_or_else(|| String::from("Not saved yet — Ctrl+S to save")),
        };
        cv.text_clipped(area.x + PAD, area.y + 27, &label, theme::ui(12), t.text_secondary, area.w - 140);
        button(cv, Self::save_button(area), "Save", ButtonStyle::Primary, self.save_hover);

        let ta = Self::text_area(area);
        self.layout = wrap(&self.text, f, ta.w);
        let visible = (ta.h / LINE_H).max(1);
        let caret_line = self.line_of(self.caret) as i32;
        if !self.follow {
        } else if caret_line < self.scroll {
            self.scroll = caret_line;
        } else if caret_line >= self.scroll + visible {
            self.scroll = caret_line - visible + 1;
        }

        cv.with_clip(ta, |cv| {
            if self.text.is_empty() {
                cv.text(ta.x, ta.y + 15, "Start typing…", f, t.text_secondary);
            }
            for (i, &(s, e)) in self.layout.iter().enumerate().skip(self.scroll as usize).take(visible as usize + 1) {
                let y = ta.y + (i as i32 - self.scroll) * LINE_H;
                cv.text(ta.x, y + 15, self.text[s..e].trim_end_matches('\n'), f, t.text);
            }
            if env.focused && self.caret_on {
                let y = ta.y + (caret_line - self.scroll) * LINE_H;
                cv.fill_rect(Rect::new(ta.x + self.caret_x(), y + 1, 2, 18), ACCENT);
            }
        });
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        self.caret_on = true;
        self.follow = true;
        self.last_blink = env.now_ms;
        if ev.mods.ctrl {
            if ev.ch == Some('s') || ev.ch == Some('S') {
                self.save(env.now_ms);
                return true;
            }
            return false;
        }
        match ev.code {
            KeyCode::Backspace if self.caret > 0 => {
                let p = self.prev_char(self.caret);
                self.text.replace_range(p..self.caret, "");
                self.caret = p;
            }
            KeyCode::Delete if self.caret < self.text.len() => {
                let n = self.next_char(self.caret);
                self.text.replace_range(self.caret..n, "");
            }
            KeyCode::Left => self.caret = self.prev_char(self.caret),
            KeyCode::Right => self.caret = self.next_char(self.caret),
            KeyCode::Up | KeyCode::Down => {
                let line = self.line_of(self.caret);
                let x = self.caret_x();
                let target = if ev.code == KeyCode::Up { line.checked_sub(1) } else { Some(line + 1) };
                match target {
                    Some(l) if l < self.layout.len() => self.caret = self.offset_at(l, x),
                    Some(_) => self.caret = self.text.len(),
                    None => self.caret = 0,
                }
                return true;
            }
            KeyCode::Home => self.caret = self.layout.get(self.line_of(self.caret)).map(|l| l.0).unwrap_or(0),
            KeyCode::End => {
                self.caret = self.layout.get(self.line_of(self.caret)).map(|l| l.1).unwrap_or(self.text.len())
            }
            KeyCode::Tab => {
                self.text.insert_str(self.caret, "    ");
                self.caret += 4;
                self.dirty = true;
            }
            _ => match ev.ch {
                Some(c) if c == '\n' || !c.is_control() => {
                    self.text.insert(self.caret, c);
                    self.caret += c.len_utf8();
                    self.dirty = true;
                }
                _ => return false,
            },
        }
        if matches!(ev.code, KeyCode::Backspace | KeyCode::Delete) {
            self.dirty = true;
        }
        self.status = None;
        true
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if Self::save_button(area).contains(x, y) {
            self.save(env.now_ms);
            return true;
        }
        let ta = Self::text_area(area);
        if y >= ta.y {
            let line = ((y - ta.y) / LINE_H + self.scroll).max(0) as usize;
            self.caret = if line >= self.layout.len() { self.text.len() } else { self.offset_at(line, x - ta.x) };
            self.caret_on = true;
            self.follow = true;
            self.last_blink = env.now_ms;
            return true;
        }
        false
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = Self::save_button(area).contains(x, y);
        core::mem::replace(&mut self.save_hover, h) != h
    }

    fn scroll(&mut self, delta: i32, _area: Rect) -> bool {
        let max = self.layout.len() as i32 - 1;
        self.follow = false;
        let s = (self.scroll + delta * 3).clamp(0, max.max(0));
        core::mem::replace(&mut self.scroll, s) != s
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let mut changed = false;
        if let Some((_, at)) = self.status {
            if env.now_ms - at > 4000 {
                self.status = None;
                changed = true;
            }
        }
        if env.focused && env.now_ms - self.last_blink >= 530 {
            self.caret_on = !self.caret_on;
            self.last_blink = env.now_ms;
            changed = true;
        }
        changed
    }
}
