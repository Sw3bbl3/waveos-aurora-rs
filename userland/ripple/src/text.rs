//! Text editing: a buffer with caret, selection, undo and the clipboard
//! ([`TextEdit`]), a scrolling multi-line editor view ([`TextView`]) and a
//! single-line field ([`TextField`]).
//!
//! Keyboard conventions follow what people expect from macOS and Windows:
//! arrows (with Ctrl for words, Shift to select), Home/End, Ctrl+Home/End,
//! Ctrl+A/C/X/V, Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y, Ctrl+Backspace. Mouse:
//! click, Shift+click, drag to select, double-click a word, triple-click a
//! line.

use crate::canvas::{with_alpha, Canvas};
use crate::font::Font;
use crate::geom::Rect;
use crate::theme::{self, ACCENT};
use crate::{KeyCode, KeyEvent};
use alloc::string::String;
use alloc::vec::Vec;

const UNDO_LIMIT: usize = 200;
/// Keystrokes closer together than this are undone together.
const GROUP_MS: u64 = 1200;
const MULTI_CLICK_MS: u64 = 500;
const BLINK_MS: u64 = 530;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    Typing,
    Deleting,
    Other,
}

struct Snapshot {
    text: String,
    caret: usize,
    anchor: usize,
}

/// What a key did.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyResult {
    /// The key was used (redraw).
    pub handled: bool,
    /// The text changed.
    pub edited: bool,
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// A text buffer with a caret, a selection and undo history.
pub struct TextEdit {
    text: String,
    caret: usize,
    /// The other end of the selection (equal to `caret` when nothing is selected).
    anchor: usize,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    group: Option<(Group, u64)>,
    pub multiline: bool,
    /// Maximum length in bytes.
    pub max_len: usize,
    revision: u64,
}

impl TextEdit {
    pub fn new(text: &str, multiline: bool) -> Self {
        Self {
            text: String::from(text),
            caret: text.len(),
            anchor: text.len(),
            undo: Vec::new(),
            redo: Vec::new(),
            group: None,
            multiline,
            max_len: 16 << 20,
            revision: 0,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Replaces everything and forgets the undo history.
    pub fn set_text(&mut self, s: &str) {
        self.text = String::from(s);
        self.caret = s.len();
        self.anchor = s.len();
        self.undo.clear();
        self.redo.clear();
        self.group = None;
        self.revision += 1;
    }

    /// Incremented on every change of the text (for layout caches).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    pub fn anchor(&self) -> usize {
        self.anchor
    }

    /// The selected byte range, if any.
    pub fn selection(&self) -> Option<(usize, usize)> {
        (self.caret != self.anchor).then(|| (self.caret.min(self.anchor), self.caret.max(self.anchor)))
    }

    pub fn selected_text(&self) -> &str {
        self.selection().map(|(a, b)| &self.text[a..b]).unwrap_or("")
    }

    fn clamp(&self, pos: usize) -> usize {
        let mut p = pos.min(self.text.len());
        while !self.text.is_char_boundary(p) {
            p -= 1;
        }
        p
    }

    /// Moves the caret; with `extend` the selection grows from its anchor.
    pub fn set_caret(&mut self, pos: usize, extend: bool) {
        self.caret = self.clamp(pos);
        if !extend {
            self.anchor = self.caret;
        }
        self.group = None;
    }

    pub fn select(&mut self, from: usize, to: usize) {
        self.anchor = self.clamp(from);
        self.caret = self.clamp(to);
        self.group = None;
    }

    pub fn select_all(&mut self) {
        self.select(0, self.text.len());
    }

    pub fn prev_char(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map(|(p, _)| p).unwrap_or(0)
    }

    pub fn next_char(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }

    /// Start of the word before `i` (skipping whitespace and punctuation first).
    pub fn prev_word(&self, i: usize) -> usize {
        let mut p = i;
        let t = &self.text;
        while p > 0 && !is_word(t[..p].chars().next_back().unwrap()) {
            p = self.prev_char(p);
        }
        while p > 0 && is_word(t[..p].chars().next_back().unwrap()) {
            p = self.prev_char(p);
        }
        p
    }

    /// End of the word after `i`.
    pub fn next_word(&self, i: usize) -> usize {
        let mut p = i;
        let t = &self.text;
        while p < t.len() && !is_word(t[p..].chars().next().unwrap()) {
            p = self.next_char(p);
        }
        while p < t.len() && is_word(t[p..].chars().next().unwrap()) {
            p = self.next_char(p);
        }
        p
    }

    /// The word (or run of other characters) around `pos`.
    pub fn word_at(&self, pos: usize) -> (usize, usize) {
        let pos = self.clamp(pos);
        let t = &self.text;
        let kind = t[pos..].chars().next().or_else(|| t[..pos].chars().next_back()).map(is_word).unwrap_or(false);
        let mut s = pos;
        while s > 0 {
            let c = t[..s].chars().next_back().unwrap();
            if is_word(c) != kind || c == '\n' {
                break;
            }
            s = self.prev_char(s);
        }
        let mut e = pos;
        while e < t.len() {
            let c = t[e..].chars().next().unwrap();
            if is_word(c) != kind || c == '\n' {
                break;
            }
            e = self.next_char(e);
        }
        (s, e)
    }

    /// The logical line (between newlines) around `pos`, including its newline.
    pub fn line_at(&self, pos: usize) -> (usize, usize) {
        let pos = self.clamp(pos);
        let s = self.text[..pos].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let e = self.text[pos..].find('\n').map(|i| pos + i + 1).unwrap_or(self.text.len());
        (s, e)
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot { text: self.text.clone(), caret: self.caret, anchor: self.anchor }
    }

    /// Records an undo step unless this edit continues the current burst.
    fn checkpoint(&mut self, g: Group, now: u64) {
        let continuing = matches!(self.group, Some((last, t)) if last == g && g != Group::Other && now - t < GROUP_MS);
        if !continuing {
            self.undo.push(self.snapshot());
            if self.undo.len() > UNDO_LIMIT {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.group = Some((g, now));
    }

    fn replace(&mut self, a: usize, b: usize, s: &str) {
        self.text.replace_range(a..b, s);
        self.caret = a + s.len();
        self.anchor = self.caret;
        self.revision += 1;
    }

    fn clean<'a>(&self, s: &'a str) -> alloc::borrow::Cow<'a, str> {
        if self.multiline {
            alloc::borrow::Cow::Borrowed(s)
        } else {
            alloc::borrow::Cow::Owned(s.replace(['\n', '\r', '\t'], " "))
        }
    }

    /// Types `s` over the selection.
    pub fn insert(&mut self, s: &str, now: u64) -> bool {
        let s = self.clean(s);
        let (a, b) = self.selection().unwrap_or((self.caret, self.caret));
        if s.is_empty() && a == b {
            return false;
        }
        if self.text.len() - (b - a) + s.len() > self.max_len {
            return false;
        }
        let g = if s.len() <= 4 && a == b && !s.contains('\n') { Group::Typing } else { Group::Other };
        self.checkpoint(g, now);
        self.replace(a, b, &s);
        true
    }

    /// Deletes the selection, or the character (or word) before the caret.
    pub fn backspace(&mut self, word: bool, now: u64) -> bool {
        let (a, b) = match self.selection() {
            Some(r) => r,
            None if self.caret == 0 => return false,
            None if word => (self.prev_word(self.caret), self.caret),
            None => (self.prev_char(self.caret), self.caret),
        };
        self.checkpoint(Group::Deleting, now);
        self.replace(a, b, "");
        true
    }

    /// Deletes the selection, or the character (or word) after the caret.
    pub fn delete_forward(&mut self, word: bool, now: u64) -> bool {
        let (a, b) = match self.selection() {
            Some(r) => r,
            None if self.caret >= self.text.len() => return false,
            None if word => (self.caret, self.next_word(self.caret)),
            None => (self.caret, self.next_char(self.caret)),
        };
        self.checkpoint(Group::Deleting, now);
        self.replace(a, b, "");
        true
    }

    pub fn undo(&mut self) -> bool {
        let Some(s) = self.undo.pop() else { return false };
        self.redo.push(self.snapshot());
        self.restore(s);
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some(s) = self.redo.pop() else { return false };
        self.undo.push(self.snapshot());
        self.restore(s);
        true
    }

    fn restore(&mut self, s: Snapshot) {
        self.text = s.text;
        self.caret = s.caret;
        self.anchor = s.anchor;
        self.group = None;
        self.revision += 1;
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn copy(&self) -> bool {
        match self.selection() {
            Some((a, b)) => {
                aurora::clipboard::set_text(&self.text[a..b]);
                true
            }
            None => false,
        }
    }

    pub fn cut(&mut self, now: u64) -> bool {
        self.copy() && self.backspace(false, now)
    }

    pub fn paste(&mut self, now: u64) -> bool {
        match aurora::clipboard::text() {
            Some(t) if !t.is_empty() => {
                self.checkpoint(Group::Other, now);
                let s = self.clean(&t).into_owned();
                let (a, b) = self.selection().unwrap_or((self.caret, self.caret));
                self.replace(a, b, &s);
                true
            }
            _ => false,
        }
    }

    /// Handles editing and horizontal movement keys. Vertical movement depends
    /// on layout and is left to the view.
    pub fn handle_key(&mut self, ev: &KeyEvent, now: u64) -> KeyResult {
        if !ev.pressed {
            return KeyResult::default();
        }
        let (ctrl, shift) = (ev.mods.ctrl || ev.mods.super_key, ev.mods.shift);
        let moved = |this: &mut Self, pos: usize| {
            this.set_caret(pos, shift);
            KeyResult { handled: true, edited: false }
        };
        let edited = |ok: bool| KeyResult { handled: true, edited: ok };
        if ctrl {
            match ev.ch.map(|c| c.to_ascii_lowercase()) {
                Some('a') => {
                    self.select_all();
                    return KeyResult { handled: true, edited: false };
                }
                Some('c') => return KeyResult { handled: self.copy(), edited: false },
                Some('x') => return edited(self.cut(now)),
                Some('v') => return edited(self.paste(now)),
                Some('z') if shift => return edited(self.redo()),
                Some('z') => return edited(self.undo()),
                Some('y') => return edited(self.redo()),
                _ => {}
            }
        }
        match ev.code {
            KeyCode::Left => {
                let to = match (self.selection(), shift, ctrl) {
                    (Some((a, _)), false, false) => a,
                    (_, _, true) => self.prev_word(self.caret),
                    _ => self.prev_char(self.caret),
                };
                moved(self, to)
            }
            KeyCode::Right => {
                let to = match (self.selection(), shift, ctrl) {
                    (Some((_, b)), false, false) => b,
                    (_, _, true) => self.next_word(self.caret),
                    _ => self.next_char(self.caret),
                };
                moved(self, to)
            }
            KeyCode::Home if ctrl || !self.multiline => moved(self, 0),
            KeyCode::End if ctrl || !self.multiline => moved(self, self.text.len()),
            KeyCode::Backspace => edited(self.backspace(ctrl, now)),
            KeyCode::Delete => edited(self.delete_forward(ctrl, now)),
            KeyCode::Enter if self.multiline => edited(self.insert("\n", now)),
            KeyCode::Tab if self.multiline && !ctrl => edited(self.insert("    ", now)),
            _ => match ev.ch {
                Some(c) if !ctrl && !c.is_control() => {
                    let mut buf = [0u8; 4];
                    edited(self.insert(c.encode_utf8(&mut buf), now))
                }
                _ => KeyResult::default(),
            },
        }
    }
}

// ------------------------------------------------------------ text view

/// Counts clicks at the same spot: 1, 2 (word), 3 (line).
#[derive(Default)]
struct Clicks {
    at: u64,
    count: u32,
    x: i32,
    y: i32,
}

impl Clicks {
    fn register(&mut self, x: i32, y: i32, now: u64, double: bool) -> u32 {
        let near = (x - self.x).abs() < 6 && (y - self.y).abs() < 6 && now - self.at < MULTI_CLICK_MS;
        self.count = if double {
            2.max(if near { self.count + 1 } else { 2 })
        } else if near {
            self.count + 1
        } else {
            1
        };
        if self.count > 3 {
            self.count = 1;
        }
        (self.at, self.x, self.y) = (now, x, y);
        self.count
    }
}

/// A scrolling, word-wrapping multi-line editor.
pub struct TextView {
    pub edit: TextEdit,
    pub font: Font,
    pub line_h: i32,
    /// Scroll offset in pixels.
    pub scroll: i32,
    /// Byte ranges to highlight (e.g. find matches); `current` is drawn stronger.
    pub highlights: Vec<(usize, usize)>,
    pub current_highlight: Option<usize>,
    pub placeholder: String,
    layout: Vec<(usize, usize)>,
    layout_key: (u64, i32, Font),
    rect: Rect,
    caret_on: bool,
    last_blink: u64,
    goal_x: Option<i32>,
    dragging: bool,
    /// While dragging after a double/triple click: extend by words or lines.
    drag_unit: u32,
    drag_origin: (usize, usize),
    clicks: Clicks,
    follow: bool,
}

impl TextView {
    pub fn new(text: &str, font: Font) -> Self {
        let mut edit = TextEdit::new(text, true);
        edit.set_caret(0, false);
        Self {
            edit,
            font,
            line_h: font.line_height as i32 + 3,
            scroll: 0,
            highlights: Vec::new(),
            current_highlight: None,
            placeholder: String::new(),
            layout: Vec::new(),
            layout_key: (u64::MAX, 0, font),
            rect: Rect::new(0, 0, 0, 0),
            caret_on: true,
            last_blink: 0,
            goal_x: None,
            dragging: false,
            drag_unit: 1,
            drag_origin: (0, 0),
            clicks: Clicks::default(),
            follow: true,
        }
    }

    pub fn set_font(&mut self, font: Font) {
        self.font = font;
        self.line_h = font.line_height as i32 + 3;
        self.follow = true;
    }

    fn relayout(&mut self, width: i32) {
        let key = (self.edit.revision(), width, self.font);
        if key != self.layout_key {
            self.layout = crate::widgets::wrap(self.edit.text(), self.font, width.max(10));
            self.layout_key = key;
        }
    }

    fn line_of(&self, pos: usize) -> usize {
        self.layout.iter().rposition(|&(s, _)| s <= pos).unwrap_or(0)
    }

    /// Caret x of `pos` within its visual line.
    fn x_of(&self, pos: usize) -> i32 {
        let (s, e) = self.layout.get(self.line_of(pos)).copied().unwrap_or((0, 0));
        let line = &self.edit.text()[s..e];
        let stops = self.font.caret_stops(line);
        let idx = line.char_indices().position(|(i, _)| s + i >= pos).unwrap_or(stops.len() - 1);
        stops[idx]
    }

    /// Byte offset in visual line `line` nearest to pixel column `x`.
    fn offset_in_line(&self, line: usize, x: i32) -> usize {
        let Some(&(s, e)) = self.layout.get(line) else { return self.edit.text().len() };
        let text = &self.edit.text()[s..e];
        let stops = self.font.caret_stops(text);
        let mut best = e;
        for (k, (i, _)) in text.char_indices().enumerate() {
            if x < (stops[k] + stops[k + 1]) / 2 {
                best = s + i;
                break;
            }
        }
        // A soft-wrapped line ends before its first character on the next line.
        if best == e && line + 1 < self.layout.len() && self.layout[line + 1].0 == e && e > s {
            best = self.edit.prev_char(e);
        }
        best
    }

    /// Byte offset nearest to a point in view coordinates.
    pub fn pos_at(&self, x: i32, y: i32) -> usize {
        let r = self.rect;
        if y < r.y && self.scroll == 0 {
            return 0;
        }
        let line = (y - r.y + self.scroll).div_euclid(self.line_h);
        if line < 0 {
            return 0;
        }
        if line as usize >= self.layout.len() {
            return self.edit.text().len();
        }
        self.offset_in_line(line as usize, x - r.x)
    }

    fn content_height(&self) -> i32 {
        self.layout.len().max(1) as i32 * self.line_h
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_height() - self.rect.h + self.line_h / 2).max(0);
        self.scroll = self.scroll.clamp(0, max);
    }

    fn keep_caret_visible(&mut self) {
        let line = self.line_of(self.edit.caret()) as i32;
        let top = line * self.line_h;
        if top < self.scroll {
            self.scroll = top;
        } else if top + self.line_h > self.scroll + self.rect.h {
            self.scroll = top + self.line_h - self.rect.h;
        }
        self.clamp_scroll();
    }

    /// Scrolls so that byte offset `pos` is visible (e.g. a find match).
    pub fn reveal(&mut self, pos: usize) {
        let line = self.line_of(pos) as i32;
        let top = line * self.line_h;
        if top < self.scroll || top + self.line_h > self.scroll + self.rect.h {
            self.scroll = top - self.rect.h / 3;
            self.clamp_scroll();
        }
    }

    fn wake_caret(&mut self, now: u64) {
        self.caret_on = true;
        self.last_blink = now;
    }

    pub fn draw(&mut self, cv: &mut Canvas, r: Rect, focused: bool) {
        self.rect = r;
        self.relayout(r.w);
        if self.follow {
            self.keep_caret_visible();
            self.follow = false;
        } else {
            self.clamp_scroll();
        }
        let t = theme::current();
        let f = self.font;
        let baseline = (self.line_h + f.ascent as i32 + f.descent as i32) / 2;
        let sel = self.edit.selection();
        let first = (self.scroll / self.line_h).max(0) as usize;
        let count = (r.h / self.line_h + 2) as usize;
        cv.with_clip(r, |cv| {
            if self.edit.text().is_empty() && !self.placeholder.is_empty() {
                cv.text(r.x, r.y + baseline, &self.placeholder, f, t.text_secondary);
            }
            let sel_color = if focused { with_alpha(ACCENT, 0x55) } else { with_alpha(t.text_secondary, 0x40) };
            for (i, &(s, e)) in self.layout.iter().enumerate().skip(first).take(count) {
                let y = r.y + i as i32 * self.line_h - self.scroll;
                let line = &self.edit.text()[s..e];
                let mut stops: Option<Vec<i32>> = None;
                let text = self.edit.text();
                let line_h = self.line_h;
                let span = |cv: &mut Canvas, a0: usize, b0: usize, c: u32, stops: &mut Option<Vec<i32>>| {
                    let (a, b) = (a0.max(s), b0.min(e));
                    // A range running past the end of the line also covers its newline.
                    let spans_newline = b0 > e && text[e..].starts_with('\n');
                    if a > b || (a == b && !spans_newline) {
                        return;
                    }
                    let st = stops.get_or_insert_with(|| f.caret_stops(line));
                    let idx = |p: usize| line.char_indices().position(|(k, _)| s + k >= p).unwrap_or(st.len() - 1);
                    let (x0, mut x1) = (st[idx(a)], st[idx(b)]);
                    if spans_newline {
                        x1 += f.advance(' ');
                    }
                    cv.fill_rect(Rect::new(r.x + x0, y, (x1 - x0).max(0), line_h), c);
                };
                for (k, &(a, b)) in self.highlights.iter().enumerate() {
                    if b > s && a < e.max(s + 1) {
                        let c = if self.current_highlight == Some(k) { 0xC0FF_D60A } else { 0x60FF_D60A };
                        span(cv, a, b, c, &mut stops);
                    }
                }
                if let Some((a, b)) = sel {
                    if b >= s && a <= e {
                        span(cv, a, b, sel_color, &mut stops);
                    }
                }
                cv.text(r.x, y + baseline, line, f, t.text);
            }
            if focused && self.caret_on && sel.is_none() {
                let line = self.line_of(self.edit.caret()) as i32;
                let y = r.y + line * self.line_h - self.scroll;
                cv.fill_rect(Rect::new(r.x + self.x_of(self.edit.caret()), y + 1, 2, self.line_h - 2), ACCENT);
            }
        });
        // Scroll indicator.
        let total = self.content_height();
        if total > r.h {
            let h = (r.h * r.h / total).max(24);
            let y = r.y + (r.h - h) * self.scroll / (total - r.h).max(1);
            cv.fill_round_rect(Rect::new(r.right() + 4, y, 5, h), 3, with_alpha(t.text_secondary, 0x70));
        }
    }

    /// A press at (x, y); `count` 2 selects a word, 3 a line.
    fn press(&mut self, x: i32, y: i32, count: u32, shift: bool, now: u64) {
        let pos = self.pos_at(x, y);
        self.goal_x = None;
        self.dragging = true;
        self.drag_unit = count;
        match count {
            2 => {
                let (a, b) = self.edit.word_at(pos);
                self.edit.select(a, b);
                self.drag_origin = (a, b);
            }
            3 => {
                let (a, b) = self.edit.line_at(pos);
                self.edit.select(a, b);
                self.drag_origin = (a, b);
            }
            _ => {
                self.edit.set_caret(pos, shift);
                self.drag_origin = (pos, pos);
            }
        }
        self.wake_caret(now);
    }

    /// A left click in the view. Returns true if it was inside.
    pub fn click(&mut self, x: i32, y: i32, shift: bool, now: u64) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        let n = self.clicks.register(x, y, now, false);
        self.press(x, y, n, shift && n == 1, now);
        true
    }

    pub fn double_click(&mut self, x: i32, y: i32, now: u64) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        let n = self.clicks.register(x, y, now, true);
        self.press(x, y, n, false, now);
        true
    }

    pub fn drag(&mut self, x: i32, y: i32) -> bool {
        if !self.dragging {
            return false;
        }
        // Auto-scroll when dragging past the edges.
        if y < self.rect.y {
            self.scroll -= self.line_h;
        } else if y > self.rect.bottom() {
            self.scroll += self.line_h;
        }
        self.clamp_scroll();
        let pos = self.pos_at(x, y.clamp(self.rect.y, self.rect.bottom() - 1));
        let (oa, ob) = self.drag_origin;
        let (a, b) = match self.drag_unit {
            2 => self.edit.word_at(pos),
            3 => self.edit.line_at(pos),
            _ => (pos, pos),
        };
        if pos < oa {
            self.edit.select(ob, a);
        } else {
            self.edit.select(oa, b.max(ob));
        }
        true
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    /// Wheel scrolling (lines).
    pub fn scroll_by(&mut self, lines: i32) -> bool {
        let before = self.scroll;
        self.scroll += lines * self.line_h * 3;
        self.clamp_scroll();
        self.scroll != before
    }

    pub fn key(&mut self, ev: &KeyEvent, now: u64) -> KeyResult {
        if !ev.pressed {
            return KeyResult::default();
        }
        self.wake_caret(now);
        let shift = ev.mods.shift;
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        let vertical = |this: &mut Self, lines: i32| {
            let caret = this.edit.caret();
            let x = match this.goal_x {
                Some(x) => x,
                None => this.x_of(caret),
            };
            this.goal_x = Some(x);
            let line = this.line_of(caret) as i32 + lines;
            let pos = if line < 0 {
                0
            } else if line as usize >= this.layout.len() {
                this.edit.text().len()
            } else {
                this.offset_in_line(line as usize, x)
            };
            let goal = this.goal_x;
            this.edit.set_caret(pos, shift);
            this.goal_x = goal;
            this.follow = true;
            KeyResult { handled: true, edited: false }
        };
        let page = (self.rect.h / self.line_h - 1).max(1);
        match ev.code {
            KeyCode::Up => return vertical(self, -1),
            KeyCode::Down => return vertical(self, 1),
            KeyCode::PageUp => return vertical(self, -page),
            KeyCode::PageDown => return vertical(self, page),
            KeyCode::Home | KeyCode::End if !ctrl => {
                let (s, e) = self.layout.get(self.line_of(self.edit.caret())).copied().unwrap_or((0, 0));
                let to = if ev.code == KeyCode::Home { s } else { e };
                self.edit.set_caret(to, shift);
                self.goal_x = None;
                self.follow = true;
                return KeyResult { handled: true, edited: false };
            }
            _ => {}
        }
        let r = self.edit.handle_key(ev, now);
        if r.handled {
            self.goal_x = None;
            self.follow = true;
        }
        r
    }

    /// Caret blinking; returns true when a redraw is needed.
    pub fn tick(&mut self, now: u64, focused: bool) -> bool {
        if focused && now - self.last_blink >= BLINK_MS {
            self.caret_on = !self.caret_on;
            self.last_blink = now;
            return self.edit.selection().is_none();
        }
        false
    }

    /// (line, column) of the caret, 1-based, counting logical lines.
    pub fn caret_line_col(&self) -> (usize, usize) {
        let t = &self.edit.text()[..self.edit.caret()];
        let line = t.matches('\n').count() + 1;
        let col = t.rsplit('\n').next().map(|l| l.chars().count()).unwrap_or(0) + 1;
        (line, col)
    }
}

// ----------------------------------------------------------- text field

/// A single-line text field with selection and the clipboard.
pub struct TextField {
    pub edit: TextEdit,
    pub placeholder: String,
    scroll_x: i32,
    rect: Rect,
    caret_on: bool,
    last_blink: u64,
    dragging: bool,
    clicks: Clicks,
}

impl TextField {
    pub fn new(text: &str, placeholder: &str) -> Self {
        Self {
            edit: TextEdit::new(text, false),
            placeholder: String::from(placeholder),
            scroll_x: 0,
            rect: Rect::new(0, 0, 0, 0),
            caret_on: true,
            last_blink: 0,
            dragging: false,
            clicks: Clicks::default(),
        }
    }

    pub fn text(&self) -> &str {
        self.edit.text()
    }

    fn font() -> Font {
        theme::ui(14)
    }

    fn pos_at(&self, x: i32) -> usize {
        let text = self.edit.text();
        let stops = Self::font().caret_stops(text);
        let rel = x - (self.rect.x + 12) + self.scroll_x;
        for (k, (i, _)) in text.char_indices().enumerate() {
            if rel < (stops[k] + stops[k + 1]) / 2 {
                return i;
            }
        }
        text.len()
    }

    pub fn draw(&mut self, cv: &mut Canvas, r: Rect, focused: bool) {
        self.rect = r;
        let t = theme::current();
        let f = Self::font();
        cv.fill_round_rect(r, 8, t.control_bg);
        cv.stroke_round_rect(r, 8, if focused { with_alpha(ACCENT, 0xC0) } else { t.control_border });
        let inner = Rect::new(r.x + 12, r.y, r.w - 24, r.h);
        let base = r.y + (r.h + f.ascent as i32 + f.descent as i32) / 2;
        let text = self.edit.text();
        let stops = f.caret_stops(text);
        let at = |p: usize| stops[text.char_indices().position(|(k, _)| k >= p).unwrap_or(stops.len() - 1)];
        // Keep the caret in view.
        let cx = at(self.edit.caret());
        if cx - self.scroll_x > inner.w - 2 {
            self.scroll_x = cx - inner.w + 2;
        } else if cx < self.scroll_x {
            self.scroll_x = cx;
        }
        cv.with_clip(inner, |cv| {
            if text.is_empty() {
                cv.text(inner.x, base, &self.placeholder, f, t.text_secondary);
            }
            if let Some((a, b)) = self.edit.selection() {
                let c = if focused { with_alpha(ACCENT, 0x55) } else { with_alpha(t.text_secondary, 0x40) };
                let (x0, x1) = (at(a), at(b));
                cv.fill_rect(Rect::new(inner.x + x0 - self.scroll_x, r.y + 6, x1 - x0, r.h - 12), c);
            }
            cv.text(inner.x - self.scroll_x, base, text, f, t.text);
            if focused && self.caret_on && self.edit.selection().is_none() {
                cv.fill_rect(Rect::new(inner.x + cx - self.scroll_x, r.y + 8, 2, r.h - 16), ACCENT);
            }
        });
    }

    pub fn click(&mut self, x: i32, y: i32, shift: bool, now: u64) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        let n = self.clicks.register(x, y, now, false);
        let pos = self.pos_at(x);
        if n >= 2 {
            self.edit.select_all();
        } else {
            self.edit.set_caret(pos, shift);
        }
        self.dragging = true;
        self.caret_on = true;
        self.last_blink = now;
        true
    }

    pub fn double_click(&mut self, x: i32, y: i32, now: u64) -> bool {
        if !self.rect.contains(x, y) {
            return false;
        }
        self.clicks.register(x, y, now, true);
        let (a, b) = self.edit.word_at(self.pos_at(x));
        self.edit.select(a, b);
        true
    }

    pub fn drag(&mut self, x: i32) -> bool {
        if !self.dragging {
            return false;
        }
        let pos = self.pos_at(x);
        let anchor = self.edit.anchor();
        self.edit.select(anchor, pos);
        true
    }

    pub fn release(&mut self) {
        self.dragging = false;
    }

    pub fn key(&mut self, ev: &KeyEvent, now: u64) -> KeyResult {
        self.caret_on = true;
        self.last_blink = now;
        self.edit.handle_key(ev, now)
    }

    pub fn tick(&mut self, now: u64, focused: bool) -> bool {
        if focused && now - self.last_blink >= BLINK_MS {
            self.caret_on = !self.caret_on;
            self.last_blink = now;
            return true;
        }
        false
    }
}
