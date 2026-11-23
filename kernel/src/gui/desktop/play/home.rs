//! Wave Play's home: rows of large tiles (Continue, Games, Apps, Settings)
//! over a backdrop lit by the focused tile's artwork.
//!
//! Everything that moves is a spring: the focused tile's growth, the row
//! scrolling under it, the rows sliding up and down, the parallax of the
//! artwork inside the tile. Focus is remembered per row. Holding a
//! direction at the end of a row bumps against it; a fresh press wraps.

use super::glyphs;
use super::library::{self, Art, Entry, Kind, Picture};
use super::pad::Button;
use crate::gui::canvas::{mix, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use lumen::layer;
use lumen::spring::{self, Spring};

/// Proportions, in points, for a `w`×`h` screen.
#[derive(Clone, Copy)]
pub struct Geo {
    pub w: i32,
    pub h: i32,
    pub tile: i32,
    pub gap: i32,
    pub margin: i32,
    /// Top of the focused row's tiles.
    pub anchor: i32,
    /// From one row's tiles to the next.
    pub pitch: i32,
    pub radius: i32,
}

pub fn geo(w: i32, h: i32) -> Geo {
    let tile = (h * 190 / 1000).clamp(120, 260);
    Geo {
        w,
        h,
        tile,
        gap: tile * 14 / 100,
        margin: (w * 64 / 1000).max(40),
        anchor: h * 47 / 100,
        pitch: tile + tile * 48 / 100,
        radius: (tile * 9 / 100).max(10),
    }
}

/// How much the focused tile grows (thousandths).
const GROW: i32 = 120;

pub struct Row {
    pub title: &'static str,
    pub items: Vec<usize>,
}

/// Searching with a controller: a strip of letters to pick from, the
/// matches in one row below it.
pub struct Search {
    pub query: String,
    /// The letter picked in the strip.
    pub key: usize,
    /// In the strip (or down in the results).
    pub in_keys: bool,
    /// What was focused before, to come back to.
    before: Option<String>,
}

/// The strip: letters, digits, a space and delete.
const KEYS: &[&str] = &[
    "A", "B", "C", "D", "E", "F", "G", "H", "I", "J", "K", "L", "M", "N", "O", "P", "Q", "R", "S", "T", "U", "V", "W",
    "X", "Y", "Z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "Space", "Delete",
];

/// What a press in search did.
pub enum Searched {
    Nothing,
    Moved,
    Typed,
    Bump,
    Closed,
    /// Accept on a result: open it.
    Open,
}

/// The lit backdrop, kept small (a sixth of the screen each way) and
/// filtered up when drawn: it is all soft light, so nothing is lost.
pub struct Backdrop {
    pub sw: i32,
    pub sh: i32,
    cur: Vec<u32>,
    prev: Vec<u32>,
    shown: Vec<u32>,
    fade: Spring,
    /// The entry it was made for, and the one it should change to (when).
    pub entry: Option<usize>,
    pub due: Option<(u64, usize)>,
    /// Where it drifted to, in whole device pixels, and its source window.
    drift: (i32, i32),
    part: (i32, i32, i32, i32),
}

const BACKDROP_DIV: i32 = 6;
/// How far the backdrop wanders (device pixels each way) and how long a lap takes.
const DRIFT: i32 = 18;
const DRIFT_PERIOD: u64 = 60_000;

impl Backdrop {
    fn new() -> Backdrop {
        Backdrop {
            sw: 0,
            sh: 0,
            cur: Vec::new(),
            prev: Vec::new(),
            shown: Vec::new(),
            fade: Spring::new(1000, spring::SMOOTH),
            entry: None,
            due: None,
            drift: (0, 0),
            part: (0, 0, 256, 256),
        }
    }

    /// Light from `pic` (its colours, and the picture itself, blurred).
    fn make(pic: Option<&Picture>, sw: i32, sh: i32) -> Vec<u32> {
        let base = 0xFF0A_0D12u32;
        let tint = pic.map_or(0xFF3A_4A62, |p| p.tint);
        let mut buf = alloc::vec![mix(base, tint, 34); (sw * sh) as usize];
        {
            let mut cv = Canvas::new(&mut buf, sw, sh);
            if let Some(p) = pic {
                // A broad wash of the artwork's own colours, upper right …
                layer::draw_scaled(
                    &mut cv,
                    &p.field,
                    2,
                    2,
                    Rect::new(sw * 34 / 100, -sh / 5, sw * 9 / 10, sh * 12 / 10),
                    130,
                    0,
                );
                // … and the artwork itself, huge and soft, behind it.
                let s = sh * 13 / 10;
                cv.draw_image(
                    &p.pixels,
                    p.w,
                    p.h,
                    Rect::new(sw - s * 3 / 4, -s / 5, s, s * p.h as i32 / p.w as i32),
                    70,
                );
            }
        }
        let mut buf = lumen::wallpaper::blur(&buf, sw, sh, (sw / 16).max(2));
        // Calm the left and bottom, where the text and tiles sit.
        for y in 0..sh {
            for x in 0..sw {
                let left = (sw - x) * 150 / sw;
                let bottom = (y * 2 - sh).max(0) * 120 / sh;
                let a = (left + bottom).min(200) as u32;
                let i = (y * sw + x) as usize;
                buf[i] = lumen::canvas::blend(buf[i], base, a);
            }
        }
        buf
    }

    fn ensure(&mut self, w: i32, h: i32) {
        let k = crate::gui::scale();
        let (sw, sh) = ((w * k / BACKDROP_DIV).max(8), (h * k / BACKDROP_DIV).max(8));
        if (sw, sh) != (self.sw, self.sh) {
            self.sw = sw;
            self.sh = sh;
            self.cur = Self::make(None, sw, sh);
            self.shown = self.cur.clone();
            self.prev.clear();
            self.entry = None;
        }
    }

    /// Changes to `pic` (fading over from what shows now).
    fn show(&mut self, entry: usize, pic: Option<&Picture>, now: u64, instant: bool) {
        self.prev = core::mem::take(&mut self.shown);
        self.cur = Self::make(pic, self.sw, self.sh);
        self.entry = Some(entry);
        if instant || self.prev.len() != self.cur.len() {
            self.shown = self.cur.clone();
            self.fade.snap(1000, now);
        } else {
            self.shown = self.prev.clone();
            self.fade.snap(0, now);
            self.fade.set_target(1000, now);
        }
    }

    /// Advances the fade and the drift; true when the screen needs drawing again.
    fn step(&mut self, now: u64, w_px: i32, h_px: i32, still: bool) -> bool {
        let mut changed = false;
        if !self.fade.settled() {
            self.fade.advance(now);
            let t = self.fade.value().clamp(0, 1000) * 256 / 1000;
            if self.prev.len() == self.cur.len() {
                for ((o, &a), &b) in self.shown.iter_mut().zip(&self.prev).zip(&self.cur) {
                    *o = mix(a, b, t);
                }
            } else {
                self.shown.clone_from(&self.cur);
            }
            if self.fade.settled() {
                self.shown.clone_from(&self.cur);
                self.prev = Vec::new();
            }
            changed = true;
        }
        // The drift, in whole device pixels so each repaint moves one.
        let phase = (now % DRIFT_PERIOD * 1024 / DRIFT_PERIOD) as i32;
        // Still: it stays where it drifted to (no jump back).
        let d = if still {
            self.drift
        } else {
            (DRIFT * lumen::math::sin(phase) / 16384, DRIFT * lumen::math::cos(phase * 2) / 16384 / 2)
        };
        let (mx, my) = (self.sw * 256 * DRIFT / w_px.max(1), self.sh * 256 * DRIFT / h_px.max(1));
        self.part = (
            mx + d.0 * self.sw * 256 / w_px.max(1),
            my + d.1 * self.sh * 256 / h_px.max(1),
            self.sw * 256 - 2 * mx,
            self.sh * 256 - 2 * my,
        );
        if d != self.drift {
            self.drift = d;
            changed = true;
        }
        changed
    }

    pub fn moving(&self) -> bool {
        !self.fade.settled()
    }

    pub fn paint(&self, cv: &mut Canvas, screen: Rect, opacity: u32) {
        if self.shown.len() == (self.sw * self.sh) as usize && self.sw > 0 {
            layer::draw_part(cv, &self.shown, self.sw as u32, self.sh as u32, self.part, screen, opacity, 0);
        }
    }
}

pub struct Home {
    pub entries: Vec<Entry>,
    pub pics: Vec<Option<Arc<Picture>>>,
    tried: Vec<bool>,
    pub rows: Vec<Row>,
    pub row: usize,
    /// The column last focused in each row.
    cols: Vec<usize>,
    scroll_y: Spring,
    scroll_x: Vec<Spring>,
    /// The focused tile's growth (0..1000), and the one it left, shrinking.
    pub focus: Spring,
    previous: Option<(usize, usize, Spring)>,
    /// The artwork lagging behind a move inside the focused tile (points).
    parallax: Spring,
    pub backdrop: Backdrop,
    /// Entries running in windows Wave Play shows (entry, window).
    pub running: Vec<(usize, u32)>,
    pub search: Option<Search>,
    /// The proportions and time of the last step (rebuilding re-aims the scroll).
    last: Option<(Geo, u64)>,
}

impl Home {
    pub fn new() -> Home {
        Home {
            entries: Vec::new(),
            pics: Vec::new(),
            tried: Vec::new(),
            rows: Vec::new(),
            row: 0,
            cols: Vec::new(),
            scroll_y: Spring::new(0, spring::SNAPPY),
            scroll_x: Vec::new(),
            focus: Spring::new(1000, spring::SNAPPY),
            previous: None,
            parallax: Spring::new(0, spring::SMOOTH),
            backdrop: Backdrop::new(),
            running: Vec::new(),
            search: None,
            last: None,
        }
    }

    /// Reads the library again, keeping the focus on the same entry.
    pub fn refresh(&mut self, cache: &[(Art, Arc<Picture>)]) {
        let focused = self.focused().map(|i| self.entries[i].id.clone());
        let entries = library::gather();
        self.pics = entries.iter().map(|e| cache.iter().find(|(a, _)| *a == e.art).map(|(_, p)| p.clone())).collect();
        self.tried = self.pics.iter().map(|p| p.is_some()).collect();
        self.entries = entries;
        self.rebuild(focused.as_deref());
    }

    /// Lays out the rows (Continue changes as things start and stop).
    pub fn rebuild(&mut self, focus_id: Option<&str>) {
        let focus_id = focus_id.map(String::from).or_else(|| self.focused().map(|i| self.entries[i].id.clone()));
        let mut rows = Vec::new();
        let mut cont: Vec<usize> = Vec::new();
        for &(e, _) in &self.running {
            if !cont.contains(&e) {
                cont.push(e);
            }
        }
        for id in library::recent() {
            if let Some(i) = self.entries.iter().position(|e| e.id == id) {
                if !cont.contains(&i) && cont.len() < 8 {
                    cont.push(i);
                }
            }
        }
        if let Some(q) = self.search.as_ref().map(|s| s.query.to_lowercase()) {
            // Searching: one row of what matches.
            let items: Vec<usize> = (0..self.entries.len())
                .filter(|&i| {
                    let e = &self.entries[i];
                    !q.trim().is_empty()
                        && (e.title.to_lowercase().contains(q.trim()) || e.detail.to_lowercase().contains(q.trim()))
                })
                .collect();
            rows.push(Row { title: "Results", items });
        } else {
            if !cont.is_empty() {
                rows.push(Row { title: "Continue", items: cont });
            }
            for (kind, title) in [(Kind::Game, "Games"), (Kind::App, "Apps"), (Kind::Setting, "Settings")] {
                let items: Vec<usize> = (0..self.entries.len()).filter(|&i| self.entries[i].kind == kind).collect();
                if !items.is_empty() {
                    rows.push(Row { title, items });
                }
            }
        }
        // Start on the games (or what to continue) the first time.
        let old_rows = core::mem::replace(&mut self.rows, rows);
        let first_time = old_rows.is_empty();
        self.cols = self.rows.iter().map(|_| 0).collect();
        self.scroll_x = self.rows.iter().map(|_| Spring::new(0, spring::SNAPPY)).collect();
        self.previous = None;
        let target = focus_id.and_then(|id| {
            self.rows
                .iter()
                .enumerate()
                .find_map(|(r, row)| row.items.iter().position(|&i| self.entries[i].id == id).map(|c| (r, c)))
        });
        let (r, c) = match target {
            Some(rc) if !first_time => rc,
            _ => (0, 0),
        };
        self.row = r.min(self.rows.len().saturating_sub(1));
        if let Some(col) = self.cols.get_mut(self.row) {
            *col = c;
        }
        if let Some((g, now)) = self.last {
            self.follow(&g, now);
        }
    }

    // ------------------------------------------------------------ search

    pub fn open_search(&mut self, now: u64) {
        if self.search.is_some() {
            return;
        }
        let before = self.focused().map(|i| self.entries[i].id.clone());
        self.search = Some(Search { query: String::new(), key: 0, in_keys: true, before });
        self.rebuild(None);
        self.scroll_y.snap(0, now);
        self.focus.snap(1000, now);
    }

    pub fn close_search(&mut self, g: &Geo, now: u64) {
        if let Some(s) = self.search.take() {
            self.rebuild(s.before.as_deref());
            self.follow(g, now);
            let t = self.scroll_y.target();
            self.scroll_y.snap(t, now);
            if let Some(e) = self.focused() {
                self.backdrop.due = Some((now, e));
            }
        }
    }

    /// Typing on a keyboard while searching (letters, digits, space, delete).
    pub fn search_type(&mut self, ch: Option<char>, delete: bool, now: u64) -> bool {
        let Some(s) = self.search.as_mut() else { return false };
        match ch {
            _ if delete => {
                s.query.pop();
            }
            Some(c) if (c.is_ascii_alphanumeric() || c == ' ') && s.query.len() < 24 => {
                s.query.push(c.to_ascii_lowercase())
            }
            _ => return false,
        }
        s.in_keys = true;
        self.rebuild(None);
        if let Some(sx) = self.scroll_x.get_mut(0) {
            sx.snap(0, now);
        }
        true
    }

    /// A press while searching.
    pub fn search_button(&mut self, g: &Geo, b: Button, repeat: bool, now: u64) -> Searched {
        let Some(s) = self.search.as_mut() else { return Searched::Nothing };
        if !s.in_keys {
            return match b {
                Button::Up => {
                    s.in_keys = true;
                    Searched::Moved
                }
                Button::Back => {
                    s.in_keys = true;
                    Searched::Moved
                }
                Button::Accept => Searched::Open,
                Button::Left | Button::Right => match self.go(g, b, repeat, now) {
                    Some(true) => Searched::Moved,
                    Some(false) => Searched::Bump,
                    None => Searched::Nothing,
                },
                _ => Searched::Nothing,
            };
        }
        let n = KEYS.len();
        match b {
            Button::Left | Button::Right => {
                let right = b == Button::Right;
                if (right && s.key + 1 >= n || !right && s.key == 0) && repeat {
                    return Searched::Bump;
                }
                s.key = if right { (s.key + 1) % n } else { (s.key + n - 1) % n };
                Searched::Moved
            }
            Button::Lb | Button::Rb => {
                s.key = if b == Button::Rb { (s.key + 7).min(n - 1) } else { s.key.saturating_sub(7) };
                Searched::Moved
            }
            Button::Accept | Button::X => {
                let key = if b == Button::X { "Delete" } else { KEYS[s.key] };
                match key {
                    "Delete" => {
                        s.query.pop();
                    }
                    "Space" if !s.query.is_empty() && s.query.len() < 24 => s.query.push(' '),
                    "Space" => {}
                    k if s.query.len() < 24 => s.query.push_str(&k.to_lowercase()),
                    _ => {}
                }
                self.rebuild(None);
                if let Some(c) = self.cols.get_mut(0) {
                    *c = 0;
                }
                if let Some(sx) = self.scroll_x.get_mut(0) {
                    sx.snap(0, now);
                }
                if let Some(e) = self.focused() {
                    self.backdrop.due = Some((now + 250, e));
                }
                Searched::Typed
            }
            Button::Down => {
                if self.rows.first().is_some_and(|r| !r.items.is_empty()) {
                    s.in_keys = false;
                    self.focus.snap(0, now);
                    self.focus.set_target(1000, now);
                    Searched::Moved
                } else {
                    Searched::Bump
                }
            }
            Button::Back | Button::Y => {
                self.close_search(g, now);
                Searched::Closed
            }
            _ => Searched::Nothing,
        }
    }

    /// The search field and the letter strip, where the title usually is.
    fn paint_search(&self, cv: &mut Canvas, g: &Geo, family: u32) {
        let Some(s) = &self.search else { return };
        let field_h = (g.h * 56 / 1000).clamp(40, 64);
        let field =
            Rect::new(g.margin, g.anchor - g.tile * 9 / 10 - field_h - 30, (g.w - 2 * g.margin).min(760), field_h);
        cv.fill_round_rect(field, field_h / 2, 0x2EFF_FFFF);
        cv.stroke_round_rect(field, field_h / 2, 0x33FF_FFFF);
        let f = theme::ui_bold((field_h * 40 / 100) as u16);
        let base = field.y + field.h / 2 + f.ascent as i32 / 2 - 2;
        // A magnifier.
        let (mx, my) = (field.x + field_h / 2 + 4, field.y + field.h / 2 - 2);
        cv.stroke_arc(mx * 16, my * 16, field_h * 16 / 6, 40, 0, 1024, 0xCCFF_FFFF);
        cv.line(mx + field_h / 8, my + field_h / 8, mx + field_h / 4, my + field_h / 4, 2, 0xCCFF_FFFF);
        let tx = field.x + field_h + 6;
        if s.query.is_empty() {
            cv.text(tx, base, "Search games and apps", f, 0x8CFF_FFFF);
        } else {
            let w = cv.text(tx, base, &s.query, f, 0xFFFF_FFFF);
            if s.in_keys {
                cv.fill_rect(Rect::new(tx + w + 2, field.y + field.h / 4, 2, field.h / 2), theme::accent());
            }
        }
        // The strip of letters.
        let key = (field_h * 78 / 100).max(30);
        let gap = key / 6;
        let y = field.bottom() + 14;
        let kf = theme::ui_bold((key * 42 / 100) as u16);
        let widths: Vec<i32> = KEYS.iter().map(|k| if k.len() > 1 { kf.width(k) + key * 2 / 3 } else { key }).collect();
        let total: i32 = widths.iter().sum::<i32>() + gap * (KEYS.len() as i32 - 1);
        let avail = g.w - 2 * g.margin;
        // Keep the picked letter in view.
        let before: i32 = widths[..s.key].iter().sum::<i32>() + gap * s.key as i32;
        let offset = if total <= avail { 0 } else { (before - avail / 2).clamp(0, total - avail) };
        let mut x = g.margin - offset;
        for (i, (k, w)) in KEYS.iter().zip(&widths).enumerate() {
            let r = Rect::new(x, y, *w, key);
            x += w + gap;
            if r.right() < g.margin - key || r.x > g.w - g.margin + key {
                continue;
            }
            let picked = i == s.key;
            let (bg, fg) = match (picked, s.in_keys) {
                (true, true) => (0xFFF2_F4F8, 0xFF10_141A),
                (true, false) => (0x40FF_FFFF, 0xFFFF_FFFF),
                _ => (0x1AFF_FFFF, 0xD9FF_FFFF),
            };
            cv.fill_round_rect(r, key / 4, bg);
            cv.text_centered(r, k, kf, fg);
        }
        let hints: &[(Button, &str)] = if s.in_keys {
            &[(Button::Accept, "Type"), (Button::X, "Delete"), (Button::Back, "Done")]
        } else {
            &[(Button::Accept, "Open"), (Button::Back, "Keys")]
        };
        paint_hint_row(cv, g.w - g.margin, g.h - (g.h * 50 / 1000).max(30), family, hints, 255);
        if self.rows.first().is_none_or(|r| r.items.is_empty()) && !s.query.trim().is_empty() {
            let f = theme::ui((g.tile / 10).clamp(14, 22) as u16);
            cv.text(g.margin, g.anchor + g.tile / 2, "Nothing matches. Try fewer letters.", f, 0xA6FF_FFFF);
        }
    }

    pub fn focused(&self) -> Option<usize> {
        let row = self.rows.get(self.row)?;
        row.items.get(*self.cols.get(self.row)?).copied()
    }

    pub fn col(&self) -> usize {
        self.cols.get(self.row).copied().unwrap_or(0)
    }

    pub fn is_running(&self, entry: usize) -> Option<u32> {
        self.running.iter().find(|r| r.0 == entry).map(|r| r.1)
    }

    /// Decodes artwork for what is (about to be) on screen, a few per call.
    pub fn load_some(&mut self, cache: &mut Vec<(Art, Arc<Picture>)>, budget: usize) -> bool {
        let mut order: Vec<usize> = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            if r + 1 >= self.row && r <= self.row + 1 {
                order.extend(row.items.iter().copied());
            }
        }
        order.extend(0..self.entries.len());
        let mut loaded = 0;
        for i in order {
            if loaded >= budget {
                break;
            }
            if self.tried.get(i).copied().unwrap_or(true) {
                continue;
            }
            self.tried[i] = true;
            let art = self.entries[i].art.clone();
            let pic = cache.iter().find(|(a, _)| *a == art).map(|(_, p)| p.clone()).or_else(|| {
                let p = library::load(&art)?;
                cache.push((art.clone(), p.clone()));
                Some(p)
            });
            for (k, e) in self.entries.iter().enumerate() {
                if e.art == art {
                    self.pics[k] = pic.clone();
                    self.tried[k] = true;
                }
            }
            loaded += 1;
        }
        loaded > 0
    }

    // ------------------------------------------------------------ moving

    fn row_x(&self, g: &Geo, c: usize) -> i32 {
        g.margin + c as i32 * (g.tile + g.gap)
    }

    /// Keeps column `c` of the focused row on screen (with room to grow).
    fn follow(&mut self, g: &Geo, now: u64) {
        let c = self.col();
        let Some(s) = self.scroll_x.get_mut(self.row) else { return };
        let x = g.margin + c as i32 * (g.tile + g.gap);
        let room = g.tile * GROW / 2000 + 8;
        let mut target = s.target();
        if x - target < g.margin + room {
            target = x - g.margin - room;
        }
        if x + g.tile - target > g.w - g.margin - room {
            target = x + g.tile - (g.w - g.margin - room);
        }
        s.set_target(target.max(0), now);
        self.scroll_y.set_target(self.row as i32 * g.pitch, now);
    }

    fn moved_focus(&mut self, from: (usize, usize), dx: i32, now: u64) {
        let mut old = self.focus;
        old.set_target(0, now);
        self.previous = Some((from.0, from.1, old));
        self.focus.snap(0, now);
        self.focus.set_target(1000, now);
        if dx != 0 {
            self.parallax.snap(dx * 14, now);
            self.parallax.set_target(0, now);
        }
        if let Some(e) = self.focused() {
            self.backdrop.due = Some((now + 170, e));
        }
    }

    /// Moves the focus; returns whether it moved (false: it bumped an end).
    pub fn go(&mut self, g: &Geo, b: Button, repeat: bool, now: u64) -> Option<bool> {
        if self.rows.is_empty() {
            return None;
        }
        let from = (self.row, self.col());
        let len = self.rows[self.row].items.len();
        match b {
            Button::Left | Button::Right => {
                let right = b == Button::Right;
                let c = self.col();
                let next = if right { c + 1 } else { c.wrapping_sub(1) };
                if next < len {
                    self.cols[self.row] = next;
                } else if !repeat && len > 1 {
                    // A deliberate press past the end goes round.
                    self.cols[self.row] = if right { 0 } else { len - 1 };
                } else {
                    self.scroll_x[self.row].kick(if right { 900 } else { -900 }, now);
                    return Some(false);
                }
                self.follow(g, now);
                self.moved_focus(from, if right { 1 } else { -1 }, now);
            }
            Button::Up | Button::Down | Button::Lb | Button::Rb => {
                let down = matches!(b, Button::Down | Button::Rb);
                let next = if down { self.row + 1 } else { self.row.wrapping_sub(1) };
                if next >= self.rows.len() {
                    self.scroll_y.kick(if down { 700 } else { -700 }, now);
                    return Some(false);
                }
                self.row = next;
                self.cols[next] = self.cols[next].min(self.rows[next].items.len() - 1);
                self.follow(g, now);
                self.moved_focus(from, 0, now);
            }
            _ => return None,
        }
        Some(true)
    }

    /// Advances every spring; returns (something is animating, the whole
    /// screen needs drawing). The drift alone only asks for a repaint.
    pub fn step(&mut self, g: &Geo, now: u64, reduced: bool, still: bool) -> (bool, bool) {
        self.last = Some((*g, now));
        if reduced {
            for s in self.scroll_x.iter_mut().chain([&mut self.scroll_y, &mut self.focus, &mut self.parallax]) {
                let t = s.target();
                s.snap(t, now);
            }
            self.previous = None;
        }
        let mut moving = false;
        let mut full = false;
        if self.scroll_y.advance(now) {
            full = true;
            moving = true;
        }
        for s in &mut self.scroll_x {
            moving |= s.advance(now);
        }
        moving |= self.focus.advance(now);
        moving |= self.parallax.advance(now);
        if let Some((_, _, s)) = &mut self.previous {
            if !s.advance(now) {
                self.previous = None;
            }
            moving = true;
        }
        if let Some((at, e)) = self.backdrop.due {
            if now >= at {
                self.backdrop.due = None;
                if self.backdrop.entry != Some(e) {
                    let pic = self.pics.get(e).cloned().flatten();
                    self.backdrop.show(e, pic.as_deref(), now, reduced);
                }
            }
        }
        let k = crate::gui::scale();
        self.backdrop.ensure(g.w, g.h);
        full |= self.backdrop.step(now, g.w * k, g.h * k, still || reduced);
        (moving || self.backdrop.moving() || self.backdrop.due.is_some(), full)
    }

    /// Shows the focused entry's light at once (entering).
    pub fn settle_backdrop(&mut self, g: &Geo, now: u64) {
        self.backdrop.ensure(g.w, g.h);
        if let Some(e) = self.focused() {
            let pic = self.pics.get(e).cloned().flatten();
            self.backdrop.show(e, pic.as_deref(), now, true);
        }
        self.follow(g, now);
        let t = self.scroll_y.target();
        self.scroll_y.snap(t, now);
    }

    /// The part of the screen the focused row (and the hero above it) covers.
    pub fn row_band(&self, g: &Geo) -> Rect {
        let top = g.anchor - g.tile * 9 / 10 - 40;
        Rect::new(0, top, g.w, g.h - top)
    }

    /// Where entry `e`'s tile is now (for zooming out of and back into it).
    pub fn tile_of(&self, g: &Geo, e: usize) -> Option<Rect> {
        let sy = self.scroll_y.value();
        for (r, row) in self.rows.iter().enumerate() {
            if let Some(c) = row.items.iter().position(|&i| i == e) {
                let x = self.row_x(g, c) - self.scroll_x[r].value();
                let y = g.anchor + r as i32 * g.pitch - sy;
                return Some(Rect::new(x, y, g.tile, g.tile));
            }
        }
        None
    }

    /// The tile under (x, y), as (row, column).
    pub fn hit(&self, g: &Geo, x: i32, y: i32) -> Option<(usize, usize)> {
        let sy = self.scroll_y.value();
        for (r, row) in self.rows.iter().enumerate() {
            let top = g.anchor + r as i32 * g.pitch - sy;
            if y < top || y >= top + g.tile || top < g.anchor - g.pitch / 2 {
                continue;
            }
            let sx = self.scroll_x[r].value();
            return (0..row.items.len())
                .find(|&c| {
                    let left = self.row_x(g, c) - sx;
                    x >= left && x < left + g.tile
                })
                .map(|c| (r, c));
        }
        None
    }

    /// Focuses a tile picked with the pointer.
    pub fn focus_at(&mut self, g: &Geo, r: usize, c: usize, now: u64) {
        let from = (self.row, self.col());
        let dx = (c as i32 - from.1 as i32).signum();
        self.row = r;
        self.cols[r] = c;
        self.follow(g, now);
        self.moved_focus(from, if r == from.0 { dx } else { 0 }, now);
    }

    pub fn focused_tile(&self, g: &Geo) -> Rect {
        let sy = self.scroll_y.value();
        let x = self.row_x(g, self.col()) - self.scroll_x.get(self.row).map_or(0, |s| s.value());
        let r = Rect::new(x, g.anchor + self.row as i32 * g.pitch - sy, g.tile, g.tile);
        grown(r, GROW)
    }

    // ---------------------------------------------------------- painting

    /// Paints the home. `intro` (0..1000, past 1000 when overshooting) stages
    /// it in while the desktop morphs away; `None` is fully there.
    pub fn paint(&self, cv: &mut Canvas, g: &Geo, family: u32, intro: Option<i32>, show_backdrop: bool) {
        let screen = Rect::new(0, 0, g.w, g.h);
        if show_backdrop {
            self.backdrop.paint(cv, screen, 255);
        }
        let p = intro.unwrap_or(1000);
        let fade = |a: i32, b: i32| spring::stage(p, a, b).clamp(0, 1000);
        // Legibility: the status strip and the bottom hints.
        let chrome = fade(560, 1000);
        layer::scrim(cv, Rect::new(0, 0, g.w, g.h / 7), 0, (90 * chrome / 1000) as u32, 0);
        layer::scrim(cv, Rect::new(0, g.h * 3 / 4, g.w, g.h / 4), 0, 0, (150 * chrome / 1000) as u32);

        let sy = self.scroll_y.value();
        let mut order: Vec<usize> = (0..self.rows.len()).filter(|&r| r != self.row).collect();
        order.push(self.row);
        for r in order {
            let row = &self.rows[r];
            let y = g.anchor + r as i32 * g.pitch - sy;
            if y > g.h + 20 || y + g.tile < -40 {
                continue;
            }
            // Rows above the focused one make way for the title.
            let dist = y - g.anchor;
            let row_alpha = if dist < 0 {
                (1000 + dist * 2200 / g.pitch).clamp(0, 1000)
            } else if r == self.row {
                1000
            } else {
                // Below: dimmer, fading out towards the hints.
                (560 - (y + g.tile - g.h * 86 / 100).max(0) * 560 / g.tile.max(1)).clamp(0, 560)
            };
            if row_alpha == 0 {
                continue;
            }
            let sx = self.scroll_x[r].value();
            // Label.
            let label_t = fade(520, 940) * row_alpha / 1000;
            if label_t > 0 {
                let f = theme::ui_bold((g.tile / 13).clamp(12, 18) as u16);
                let a = (label_t * 235 / 1000) as u32;
                cv.text(g.margin, y - g.tile * GROW / 2000 - g.tile / 14 - 6, row.title, f, a << 24 | 0xFFFFFF);
            }
            let focused_col = (r == self.row && self.col() < row.items.len()).then(|| self.col());
            // The focused tile last, over its neighbours.
            let mut cols: Vec<usize> = (0..row.items.len()).filter(|&c| Some(c) != focused_col).collect();
            cols.extend(focused_col);
            for c in cols {
                let x = self.row_x(g, c) - sx;
                if x > g.w + g.tile || x + g.tile < -g.tile {
                    continue;
                }
                let e = row.items[c];
                let mut grow = 0;
                if Some(c) == focused_col && !self.search.as_ref().is_some_and(|s| s.in_keys) {
                    grow = self.focus.value();
                } else if let Some((pr, pc, s)) = &self.previous {
                    if (*pr, *pc) == (r, c) {
                        grow = s.value().max(0);
                    }
                }
                // Tiles rise into place one after another.
                let rank = if r == self.row { c as i32 - self.col() as i32 + 2 } else { 6 + c as i32 };
                let t = if intro.is_some() {
                    spring::stage(p, 420 + 42 * rank.clamp(0, 9), 860 + 42 * rank.clamp(0, 9))
                } else {
                    1000
                };
                if t <= 0 {
                    continue;
                }
                let rise = (1000 - t) * g.tile / 3 / 1000;
                let alpha = row_alpha * t.clamp(0, 1000) / 1000;
                let tile = Rect::new(x, y + rise, g.tile, g.tile);
                let par = if Some(c) == focused_col { self.parallax.value() } else { 0 };
                self.paint_tile(cv, g, tile, e, grow, alpha, par);
            }
        }
        if self.search.is_some() {
            self.paint_search(cv, g, family);
            self.paint_strip(cv, g, chrome);
            return;
        }
        self.paint_title(cv, g, fade(500, 960));
        self.paint_strip(cv, g, chrome);
        self.paint_hints(cv, g, family, chrome);
    }

    #[allow(clippy::too_many_arguments)] // a tile: where, which, and three amounts of motion
    fn paint_tile(&self, cv: &mut Canvas, g: &Geo, tile: Rect, e: usize, grow: i32, alpha: i32, parallax: i32) {
        let grow = grow.clamp(-100, 1150);
        let lift = g.tile * 3 / 100 * grow.max(0) / 1000;
        let r = grown(tile, GROW * grow / 1000).offset(0, -lift);
        let radius = g.radius * r.w / g.tile.max(1);
        let a = (alpha.clamp(0, 1000) * 255 / 1000) as u32;
        if a == 0 {
            return;
        }
        let gf = grow.clamp(0, 1000) as u32;
        // Depth: the focused tile floats higher, its shadow softer and deeper.
        cv.shadow(r, radius, 18 + 14 * gf as i32 / 1000, 8 + 10 * gf as i32 / 1000, (70 + 110 * gf / 1000) * a / 255);
        let pic = self.pics.get(e).cloned().flatten();
        match (&self.entries[e].art, &pic) {
            (Art::Picture(_), Some(p)) => {
                // Cover art: fill the tile, cropped to its shape.
                let (sw, sh) = (p.w as i64 * 256, p.h as i64 * 256);
                let (cw, ch) = if sw * r.h as i64 > sh * r.w as i64 {
                    (sh * r.w as i64 / r.h.max(1) as i64, sh)
                } else {
                    (sw, sw * r.h as i64 / r.w.max(1) as i64)
                };
                let part = (((sw - cw) / 2) as i32, ((sh - ch) / 2) as i32, cw as i32, ch as i32);
                cv.fill_round_rect(r, radius, (a << 24) | 0x101318);
                lumen::layer::draw_part(cv, &p.pixels, p.w, p.h, part, r, a, radius);
            }
            (_, Some(p)) => {
                // The icon's own colours as a soft field, the icon floating on it.
                layer::draw_scaled(cv, &p.field, 2, 2, r, a, radius);
                cv.with_clip(r, |cv| layer::scrim(cv, r, 0, 0, 70 * a / 255));
                let s = r.w * 54 / 100;
                let icon = Rect::new(r.x + (r.w - s) / 2 + parallax, r.y + (r.h - s) / 2 - r.h / 30, s, s);
                cv.shadow(icon.inset(s / 10), s / 5, s / 8, s / 16, 60 * a / 255);
                cv.draw_image(&p.pixels, p.w, p.h, icon, a);
            }
            (art, None) => {
                cv.fill_round_rect(r, radius, (a << 24) | 0x2A3342);
                let s = r.w * 46 / 100;
                if let Art::Icon(i) = art {
                    crate::gui::icons::draw(cv, *i, Rect::new(r.x + (r.w - s) / 2, r.y + (r.h - s) / 2, s, s));
                }
            }
        }
        // A fine edge, and around the focused tile a crisp ring.
        cv.stroke_round_rect(r, radius, (((26 + 40 * gf / 1000) * a / 255) << 24) | 0xFFFFFF);
        if gf > 0 {
            let ring = ((235 * gf / 1000 * a / 255) << 24) | 0xFFFFFF;
            cv.stroke_round_rect(r.inset(-5), radius + 5, ring);
            cv.stroke_round_rect(r.inset(-4), radius + 4, ring);
        }
        if self.is_running(e).is_some() {
            let f = theme::ui_bold((g.tile / 17).clamp(10, 14) as u16);
            let label = "Running";
            let pill =
                Rect::new(r.x + 10, r.bottom() - 10 - f.ascent as i32 - 12, f.width(label) + 26, f.ascent as i32 + 12);
            cv.fill_round_rect(pill, pill.h / 2, (a * 3 / 4) << 24 | 0x0A0D12);
            cv.fill_circle(pill.x + 10, pill.y + pill.h / 2, 3, (a << 24) | 0x4CD964);
            cv.text(pill.x + 18, pill.y + pill.h / 2 + f.ascent as i32 / 2 - 1, label, f, (a << 24) | 0xFFFFFF);
        }
    }

    fn paint_title(&self, cv: &mut Canvas, g: &Geo, t: i32) {
        if t <= 0 {
            return;
        }
        let Some(e) = self.focused() else { return };
        let entry = &self.entries[e];
        let ft = self.focus.value().clamp(0, 1000);
        // The new title settles in as the tile grows.
        let a = (t * (400 + 600 * ft / 1000) / 1000 * 255 / 1000) as u32;
        let rise = (1000 - ft) * 10 / 1000;
        let size = (g.h * 46 / 1000).clamp(26, 60) as u16;
        let title_f = theme::display(size);
        let detail_f = theme::ui((size as i32 * 40 / 100).clamp(13, 22) as u16);
        let base = g.anchor - g.tile * 34 / 100 - 4 + rise;
        let mut detail = entry.detail.clone();
        if self.is_running(e).is_some() {
            detail = String::from("Running · press A to return");
        }
        cv.text(g.margin, base - detail_f.ascent as i32 - 16, &entry.title, title_f, (a << 24) | 0xFFFFFF);
        cv.text(g.margin, base, &detail, detail_f, ((a * 7 / 10) << 24) | 0xFFFFFF);
    }

    fn paint_strip(&self, cv: &mut Canvas, g: &Geo, t: i32) {
        if t <= 0 {
            return;
        }
        let a = (t * 255 / 1000) as u32;
        let fg = (a << 24) | 0xFFFFFF;
        let dim = ((a * 7 / 10) << 24) | 0xFFFFFF;
        let y = (g.h * 55 / 1000).max(34);
        let f = theme::ui_bold(15);
        // Left: where you are.
        cv.text(g.margin, y + 5, "Wave Play", f, dim);
        // Right, inwards: clock, battery, network, controllers.
        let clock = super::super::bar::time_text();
        let cf = theme::ui_bold(17);
        let mut x = g.w - g.margin - cf.width(&clock);
        cv.text(x, y + 6, &clock, cf, fg);
        if let Some(b) = super::super::bar::battery() {
            let charging = b.flags & aurora_abi::power::CHARGING != 0;
            let label = alloc::format!("{}%", b.percent);
            x -= f.width(&label) + 18;
            cv.text(x, y + 5, &label, f, dim);
            x -= 30;
            crate::gui::icons::battery(cv, Rect::new(x, y - 5, 24, 12), b.percent, charging, fg, fg);
        }
        x -= 34;
        glyphs::network(cv, x, y - 2, network_up(), fg);
        for _ in crate::drivers::gamepad::list() {
            x -= 36;
            glyphs::controller(cv, x + 12, y, 24, fg);
        }
    }

    fn paint_hints(&self, cv: &mut Canvas, g: &Geo, family: u32, t: i32) {
        if t <= 0 {
            return;
        }
        let a = (t * 255 / 1000) as u32;
        let running = self.focused().is_some_and(|e| self.is_running(e).is_some());
        let open = match self.focused().map(|e| &self.entries[e].launch) {
            Some(library::Launch::Desktop) => "Desktop",
            _ if running => "Resume",
            _ => "Open",
        };
        let hints = [(Button::Accept, open), (Button::Y, "Search"), (Button::Menu, "Menu"), (Button::View, "Desktop")];
        paint_hint_row(cv, g.w - g.margin, g.h - (g.h * 50 / 1000).max(30), family, &hints, a);
    }
}

/// Glyph-and-label hints, right-aligned so they end at `right`.
pub fn paint_hint_row(cv: &mut Canvas, right: i32, cy: i32, family: u32, hints: &[(Button, &str)], a: u32) {
    let f = theme::ui_bold(14);
    let size = 26;
    let widths: Vec<i32> = hints.iter().map(|(_, l)| size + 9 + f.width(l)).collect();
    let mut x = right - widths.iter().sum::<i32>() - 26 * (hints.len() as i32 - 1);
    for ((b, label), w) in hints.iter().zip(widths) {
        let gw = glyphs::button(
            cv,
            family,
            *b,
            x + size / 2,
            cy,
            size,
            (a << 24) | 0x0E1116,
            ((a * 92 / 100) << 24) | 0xFFFFFF,
        );
        cv.text(x + gw + 9, cy + f.ascent as i32 / 2 - 1, label, f, (a << 24) | 0xFFFFFF);
        x += w.max(gw + 9 + f.width(label)) + 26;
    }
}

/// `r` grown by `permille` thousandths about its centre.
pub fn grown(r: Rect, permille: i32) -> Rect {
    let (w, h) = (r.w + r.w * permille / 1000, r.h + r.h * permille / 1000);
    Rect::new(r.x - (w - r.w) / 2, r.y - (h - r.h) / 2, w, h)
}

fn network_up() -> bool {
    use aurora_abi::net::IF_LOOPBACK;
    crate::net::interfaces().iter().any(|i| i.flags & IF_LOOPBACK == 0 && i.ip != [0; 4])
}
