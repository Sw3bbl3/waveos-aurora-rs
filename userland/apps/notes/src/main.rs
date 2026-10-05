//! Notes — a text editor.
//!
//! Selection with mouse and keyboard, the clipboard, undo/redo, find with
//! highlighted matches, adjustable font and size, word count, Save As, and a
//! reminder about unsaved changes when closing.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurorakit::canvas::{with_alpha, Canvas};
use aurorakit::font::Font;
use aurorakit::geom::Rect;
use aurorakit::text::{TextField, TextView};
use aurorakit::theme;
use aurorakit::widgets::{button, ButtonStyle};
use aurorakit::{App, Env, KeyCode, KeyEvent, Request};
use corekit::fs;

const TOOLBAR_H: i32 = 46;
const FIND_H: i32 = 44;
const STATUS_H: i32 = 26;
const PAD: i32 = 20;
const SIZES: [u16; 7] = [12, 13, 14, 15, 17, 20, 24];
const FOLDERS: [&str; 4] = ["/Documents", "/Desktop", "/Downloads", "/Pictures"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Save,
    Mono,
    Smaller,
    Larger,
    Find,
    FindPrev,
    FindNext,
    FindDone,
    SheetCancel,
    SheetConfirm,
    SheetDiscard,
    Folder(usize),
}

enum Sheet {
    SaveAs { name: TextField, folder: usize, then_close: bool },
    ConfirmClose,
}

struct Find {
    field: TextField,
    /// Typing goes to the find field (otherwise to the text, matches stay lit).
    focused: bool,
    matches: Vec<(usize, usize)>,
    current: usize,
    query_rev: u64,
    text_rev: u64,
}

pub struct Notes {
    view: TextView,
    path: Option<String>,
    saved_rev: u64,
    mono: bool,
    size: usize,
    find: Option<Find>,
    sheet: Option<Sheet>,
    hover: Option<Hit>,
    status: Option<(String, u64)>,
    area: Rect,
}

fn font_for(mono: bool, size: usize) -> Font {
    let px = SIZES[size.min(SIZES.len() - 1)];
    if mono {
        theme::mono(px)
    } else {
        theme::ui(px)
    }
}

impl Notes {
    pub fn new(path: Option<String>) -> Self {
        let text = path
            .as_deref()
            .and_then(|p| fs::read(p).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let size = 3;
        let mut view = TextView::new(&text, font_for(false, size));
        view.placeholder = String::from("Start typing…");
        let saved_rev = view.edit.revision();
        Self {
            view,
            path,
            saved_rev,
            mono: false,
            size,
            find: None,
            sheet: None,
            hover: None,
            status: None,
            area: Rect::new(0, 0, 600, 460),
        }
    }

    fn dirty(&self) -> bool {
        self.view.edit.revision() != self.saved_rev
    }

    fn name(&self) -> String {
        match &self.path {
            Some(p) => String::from(fs::file_name(p)),
            None => String::from("Untitled"),
        }
    }

    fn set_font(&mut self) {
        self.view.set_font(font_for(self.mono, self.size));
    }

    // ------------------------------------------------------------ layout

    fn text_rect(&self, area: Rect) -> Rect {
        let top = area.y + TOOLBAR_H + if self.find.is_some() { FIND_H } else { 0 } + 12;
        Rect::new(area.x + PAD, top, area.w - 2 * PAD - 8, area.bottom() - STATUS_H - 8 - top)
    }

    fn toolbar_hits(area: Rect) -> Vec<(Hit, Rect)> {
        let y = area.y + 8;
        let r = area.right();
        alloc::vec![
            (Hit::Save, Rect::new(r - 84, y, 72, 30)),
            (Hit::Find, Rect::new(r - 124, y, 32, 30)),
            (Hit::Larger, Rect::new(r - 164, y, 32, 30)),
            (Hit::Smaller, Rect::new(r - 198, y, 32, 30)),
            (Hit::Mono, Rect::new(r - 256, y, 50, 30)),
        ]
    }

    fn find_hits(area: Rect) -> Vec<(Hit, Rect)> {
        let y = area.y + TOOLBAR_H + 7;
        let r = area.right();
        alloc::vec![
            (Hit::FindDone, Rect::new(r - 76, y, 64, 30)),
            (Hit::FindNext, Rect::new(r - 116, y, 32, 30)),
            (Hit::FindPrev, Rect::new(r - 150, y, 32, 30)),
        ]
    }

    fn find_field_rect(area: Rect) -> Rect {
        Rect::new(area.x + PAD, area.y + TOOLBAR_H + 7, (area.w - 2 * PAD - 250).max(120), 30)
    }

    fn sheet_rect(&self, area: Rect) -> Rect {
        let h = match self.sheet {
            Some(Sheet::SaveAs { .. }) => 214,
            _ => 150,
        };
        let w = 400.min(area.w - 40);
        Rect::new(area.x + (area.w - w) / 2, area.y + TOOLBAR_H + 10, w, h)
    }

    fn sheet_hits(&self, area: Rect) -> Vec<(Hit, Rect)> {
        let s = self.sheet_rect(area);
        let y = s.bottom() - 46;
        let mut v = Vec::new();
        match &self.sheet {
            Some(Sheet::SaveAs { .. }) => {
                v.push((Hit::SheetConfirm, Rect::new(s.right() - 108, y, 90, 30)));
                v.push((Hit::SheetCancel, Rect::new(s.right() - 208, y, 90, 30)));
                let fw = (s.w - 36) / FOLDERS.len() as i32;
                for i in 0..FOLDERS.len() {
                    v.push((Hit::Folder(i), Rect::new(s.x + 18 + i as i32 * fw, s.y + 104, fw - 6, 28)));
                }
            }
            Some(Sheet::ConfirmClose) => {
                v.push((Hit::SheetConfirm, Rect::new(s.right() - 98, y, 80, 30)));
                v.push((Hit::SheetCancel, Rect::new(s.right() - 188, y, 80, 30)));
                v.push((Hit::SheetDiscard, Rect::new(s.x + 18, y, 110, 30)));
            }
            None => {}
        }
        v
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        let area = self.area;
        if self.sheet.is_some() {
            return self.sheet_hits(area).into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h);
        }
        let mut all = Self::toolbar_hits(area);
        if self.find.is_some() {
            all.extend(Self::find_hits(area));
        }
        all.into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h)
    }

    // ------------------------------------------------------------ actions

    fn write_to(&mut self, path: String, now: u64) -> bool {
        match fs::write(&path, self.view.edit.text().as_bytes()) {
            Ok(()) => {
                self.status = Some((format!("Saved to {path}"), now));
                self.path = Some(path);
                self.saved_rev = self.view.edit.revision();
                true
            }
            Err(e) => {
                self.status = Some((format!("Couldn't save: {e}"), now));
                false
            }
        }
    }

    fn save(&mut self, now: u64) {
        match self.path.clone() {
            Some(p) => {
                self.write_to(p, now);
            }
            None => self.open_save_as(false),
        }
    }

    fn open_save_as(&mut self, then_close: bool) {
        let base = self.path.as_deref().map(fs::file_name).unwrap_or("Untitled.txt");
        let mut name = TextField::new(base, "Name");
        // Select the name without its extension, like other systems do.
        let stem = base.rfind('.').filter(|&i| i > 0).unwrap_or(base.len());
        name.edit.select(0, stem);
        let folder = self.path.as_deref().and_then(|p| FOLDERS.iter().position(|f| fs::parent(p) == *f)).unwrap_or(0);
        self.sheet = Some(Sheet::SaveAs { name, folder, then_close });
    }

    fn confirm_sheet(&mut self, env: &mut Env) {
        match self.sheet.take() {
            Some(Sheet::SaveAs { name, folder, then_close }) => {
                let mut n = String::from(name.text().trim());
                if n.is_empty() {
                    n = String::from("Untitled.txt");
                }
                if !n.contains('.') {
                    n.push_str(".txt");
                }
                let path = fs::join(FOLDERS[folder], &n);
                if self.write_to(path, env.now_ms) && then_close {
                    env.requests.push(Request::Close);
                }
            }
            Some(Sheet::ConfirmClose) => {
                if self.path.is_some() {
                    self.save(env.now_ms);
                    if !self.dirty() {
                        env.requests.push(Request::Close);
                    }
                } else {
                    self.open_save_as(true);
                }
            }
            None => {}
        }
    }

    fn open_find(&mut self) {
        let selected = String::from(self.view.edit.selected_text());
        let seed = if !selected.is_empty() && !selected.contains('\n') { selected } else { String::new() };
        if let Some(f) = &mut self.find {
            if !seed.is_empty() {
                f.field.edit.set_text(&seed);
            }
            f.field.edit.select_all();
            f.focused = true;
            self.refresh_find();
            return;
        }
        let mut field = TextField::new(&seed, "Find");
        field.edit.select_all();
        self.find = Some(Find {
            field,
            focused: true,
            matches: Vec::new(),
            current: 0,
            query_rev: u64::MAX,
            text_rev: u64::MAX,
        });
        self.refresh_find();
    }

    /// Recomputes matches (case-insensitive) when the query or text changed.
    fn refresh_find(&mut self) {
        let Some(f) = &mut self.find else {
            self.view.highlights.clear();
            self.view.current_highlight = None;
            return;
        };
        let (qr, tr) = (f.field.edit.revision(), self.view.edit.revision());
        if (qr, tr) != (f.query_rev, f.text_rev) {
            f.query_rev = qr;
            f.text_rev = tr;
            f.matches.clear();
            let q = f.field.text().to_lowercase();
            if !q.is_empty() {
                let hay = self.view.edit.text().to_lowercase();
                // Lower-casing can change byte lengths; only trust it when it didn't.
                if hay.len() == self.view.edit.text().len() {
                    let mut from = 0;
                    while let Some(i) = hay[from..].find(&q) {
                        let a = from + i;
                        f.matches.push((a, a + q.len()));
                        from = a + q.len().max(1);
                        if f.matches.len() >= 5000 {
                            break;
                        }
                    }
                }
            }
            let caret = self.view.edit.caret();
            f.current = f.matches.iter().position(|&(a, _)| a >= caret).unwrap_or(0);
        }
        self.view.highlights = f.matches.clone();
        self.view.current_highlight = (!f.matches.is_empty()).then_some(f.current);
    }

    fn find_step(&mut self, delta: i32) {
        let Some(f) = &mut self.find else { return };
        if f.matches.is_empty() {
            return;
        }
        let n = f.matches.len() as i32;
        f.current = (f.current as i32 + delta).rem_euclid(n) as usize;
        let (a, b) = f.matches[f.current];
        self.view.edit.select(a, b);
        self.view.reveal(a);
        self.view.current_highlight = Some(f.current);
    }

    fn close_find(&mut self) {
        self.find = None;
        self.refresh_find();
    }

    fn run(&mut self, h: Hit, env: &mut Env) {
        match h {
            Hit::Save => self.save(env.now_ms),
            Hit::Mono => {
                self.mono = !self.mono;
                self.set_font();
            }
            Hit::Smaller => {
                self.size = self.size.saturating_sub(1);
                self.set_font();
            }
            Hit::Larger => {
                self.size = (self.size + 1).min(SIZES.len() - 1);
                self.set_font();
            }
            Hit::Find => {
                if self.find.is_some() {
                    self.close_find();
                } else {
                    self.open_find();
                }
            }
            Hit::FindPrev => self.find_step(-1),
            Hit::FindNext => self.find_step(1),
            Hit::FindDone => self.close_find(),
            Hit::SheetCancel => self.sheet = None,
            Hit::SheetConfirm => self.confirm_sheet(env),
            Hit::SheetDiscard => {
                self.sheet = None;
                env.requests.push(Request::Close);
            }
            Hit::Folder(i) => {
                if let Some(Sheet::SaveAs { folder, .. }) = &mut self.sheet {
                    *folder = i;
                }
            }
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw_tool(&self, cv: &mut Canvas, h: Hit, r: Rect) {
        let t = theme::current();
        let hovered = self.hover == Some(h);
        match h {
            Hit::Save => button(cv, r, "Save", ButtonStyle::Primary, hovered),
            Hit::SheetConfirm => {
                let label = match self.sheet {
                    Some(Sheet::SaveAs { .. }) | Some(Sheet::ConfirmClose) => "Save",
                    None => "OK",
                };
                button(cv, r, label, ButtonStyle::Primary, hovered)
            }
            Hit::SheetCancel => button(cv, r, "Cancel", ButtonStyle::Secondary, hovered),
            Hit::SheetDiscard => button(cv, r, "Don't Save", ButtonStyle::Secondary, hovered),
            Hit::FindDone => button(cv, r, "Done", ButtonStyle::Secondary, hovered),
            Hit::Folder(i) => {
                let selected = matches!(&self.sheet, Some(Sheet::SaveAs { folder, .. }) if *folder == i);
                if selected {
                    cv.fill_round_rect(r, 7, theme::accent());
                } else {
                    cv.fill_round_rect(r, 7, if hovered { t.hover } else { t.window_bg_alt });
                }
                let label = FOLDERS[i].trim_start_matches('/');
                cv.text_centered(r, label, theme::ui(12), if selected { 0xFFFF_FFFF } else { t.text });
            }
            _ => {
                if hovered {
                    cv.fill_round_rect(r, 7, t.hover);
                }
                let (cx, cy) = r.center();
                let c = t.text;
                match h {
                    Hit::Mono => {
                        if self.mono {
                            cv.fill_round_rect(r, 7, with_alpha(theme::accent(), 0x30));
                        }
                        let label = if self.mono { "Mono" } else { "Sans" };
                        cv.text_centered(r, label, if self.mono { theme::mono(13) } else { theme::ui_bold(13) }, c);
                    }
                    Hit::Smaller => cv.text_centered(r, "A", theme::ui_bold(11), c),
                    Hit::Larger => cv.text_centered(r, "A", theme::ui_bold(17), c),
                    Hit::Find => {
                        cv.fill_circle(cx - 2, cy - 2, 6, c);
                        cv.fill_circle(cx - 2, cy - 2, 4, if hovered { t.titlebar } else { t.titlebar });
                        cv.line(cx + 2, cy + 2, cx + 7, cy + 7, 2, c);
                    }
                    Hit::FindPrev | Hit::FindNext => {
                        let d = if h == Hit::FindPrev { 1 } else { -1 };
                        cv.line(cx - 5, cy + 3 * d, cx, cy - 2 * d, 2, c);
                        cv.line(cx, cy - 2 * d, cx + 5, cy + 3 * d, 2, c);
                    }
                    _ => {}
                }
            }
        }
    }

    fn draw_sheet(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        cv.fill_rect(area, if t.dark { 0x6000_0000 } else { 0x3000_0000 });
        let s = self.sheet_rect(area);
        cv.shadow(s, 12, 24, 8, t.shadow);
        cv.fill_round_rect(s, 12, t.window_bg);
        cv.stroke_round_rect(s, 12, t.window_border);
        let title_f = theme::ui_bold(15);
        let body_f = theme::ui(13);
        let name = self.name();
        match &mut self.sheet {
            Some(Sheet::SaveAs { name: field, .. }) => {
                cv.text(s.x + 18, s.y + 32, "Save As", title_f, t.text);
                field.draw(cv, Rect::new(s.x + 18, s.y + 48, s.w - 36, 34), env.focused);
                cv.text(s.x + 18, s.y + 98, "Where:", theme::ui(12), t.text_secondary);
            }
            Some(Sheet::ConfirmClose) => {
                let msg = format!("Save changes to “{name}”?");
                cv.text_clipped(s.x + 18, s.y + 34, &msg, title_f, t.text, s.w - 36);
                cv.text(
                    s.x + 18,
                    s.y + 60,
                    "Your changes will be lost if you don't save them.",
                    body_f,
                    t.text_secondary,
                );
            }
            None => {}
        }
        for (h, r) in self.sheet_hits(area) {
            self.draw_tool(cv, h, r);
        }
    }
}

impl App for Notes {
    fn title(&self) -> String {
        format!("{}{} — Notes", self.name(), if self.dirty() { " •" } else { "" })
    }

    fn size(&self) -> (i32, i32) {
        (640, 480)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        self.refresh_find();
        let t = theme::current();

        // Toolbar.
        let bar = Rect::new(area.x, area.y, area.w, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(area.x, bar.bottom() - 1, area.w, 1), t.separator);
        let label = match &self.status {
            Some((s, _)) => s.clone(),
            None => self.path.clone().unwrap_or_else(|| String::from("Not saved yet")),
        };
        cv.text_clipped(area.x + PAD, area.y + 28, &label, theme::ui(12), t.text_secondary, area.w - 300);
        for (h, r) in Self::toolbar_hits(area) {
            self.draw_tool(cv, h, r);
        }

        // Find bar.
        let editing_text = self.sheet.is_none() && !self.find.as_ref().is_some_and(|f| f.focused);
        if let Some(f) = &mut self.find {
            let fb = Rect::new(area.x, area.y + TOOLBAR_H, area.w, FIND_H);
            cv.fill_rect(fb, t.window_bg_alt);
            cv.fill_rect(Rect::new(area.x, fb.bottom() - 1, area.w, 1), t.separator);
            let fr = Self::find_field_rect(area);
            f.field.draw(cv, fr, env.focused && self.sheet.is_none() && f.focused);
            let count = if f.field.text().is_empty() {
                String::new()
            } else if f.matches.is_empty() {
                String::from("Not found")
            } else {
                format!("{} of {}", f.current + 1, f.matches.len())
            };
            cv.text(fr.right() + 12, fr.y + 20, &count, theme::ui(12), t.text_secondary);
            for (h, r) in Self::find_hits(area) {
                self.draw_tool(cv, h, r);
            }
        }

        // Text.
        let tr = self.text_rect(area);
        self.view.draw(cv, tr, env.focused && editing_text);

        // Status bar.
        let sb = Rect::new(area.x, area.bottom() - STATUS_H, area.w, STATUS_H);
        cv.fill_rect(Rect::new(area.x, sb.y, area.w, 1), t.separator);
        let text = self.view.edit.text();
        let words = text.split_whitespace().count();
        let chars = text.chars().count();
        let (line, col) = self.view.caret_line_col();
        let info = format!(
            "Line {line}, Column {col}   ·   {words} word{}   ·   {chars} character{}",
            if words == 1 { "" } else { "s" },
            if chars == 1 { "" } else { "s" }
        );
        cv.text(sb.x + PAD, sb.y + 18, &info, theme::ui(12), t.text_secondary);
        let state = if self.dirty() { "Edited" } else { "Saved" };
        let f = theme::ui(12);
        cv.text(sb.right() - PAD - f.width(state), sb.y + 18, state, f, t.text_secondary);

        if self.sheet.is_some() {
            self.draw_sheet(cv, area, env);
        }
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        let lower = ev.ch.map(|c| c.to_ascii_lowercase());

        // Sheets take the keyboard.
        if let Some(sheet) = &mut self.sheet {
            match (ev.code, sheet) {
                (KeyCode::Escape, _) => self.sheet = None,
                (KeyCode::Enter, _) => self.confirm_sheet(env),
                (_, Sheet::SaveAs { name, .. }) => {
                    name.key(ev, env.now_ms);
                }
                _ => return false,
            }
            return true;
        }

        if ctrl {
            match lower {
                Some('s') if ev.mods.shift => {
                    self.open_save_as(false);
                    return true;
                }
                Some('s') => {
                    self.save(env.now_ms);
                    return true;
                }
                Some('f') => {
                    self.open_find();
                    return true;
                }
                Some('n') => {
                    env.requests.push(Request::OpenApp(String::from("Notes")));
                    return false;
                }
                Some('=') | Some('+') => {
                    self.run(Hit::Larger, env);
                    return true;
                }
                Some('-') => {
                    self.run(Hit::Smaller, env);
                    return true;
                }
                Some('g') => {
                    self.find_step(if ev.mods.shift { -1 } else { 1 });
                    return true;
                }
                _ => {}
            }
        }

        if ev.code == KeyCode::Escape && self.find.is_some() {
            self.close_find();
            return true;
        }
        if let Some(f) = self.find.as_mut().filter(|f| f.focused) {
            match ev.code {
                KeyCode::Enter => self.find_step(if ev.mods.shift { -1 } else { 1 }),
                KeyCode::F3 => self.find_step(if ev.mods.shift { -1 } else { 1 }),
                _ => {
                    f.field.key(ev, env.now_ms);
                    self.refresh_find();
                    // Jump to the first match at or after the caret as you type.
                    if let Some(f) = &self.find {
                        if let Some(&(a, b)) = f.matches.get(f.current) {
                            self.view.edit.select(a, b);
                            self.view.reveal(a);
                        }
                    }
                }
            }
            return true;
        }

        let r = self.view.key(ev, env.now_ms);
        if r.edited {
            self.status = None;
        }
        r.handled
    }

    fn click(&mut self, x: i32, y: i32, _area: Rect, env: &mut Env) -> bool {
        if let Some(h) = self.hit_at(x, y) {
            self.run(h, env);
            return true;
        }
        if let Some(Sheet::SaveAs { name, .. }) = &mut self.sheet {
            return name.click(x, y, env.mods.shift, env.now_ms);
        }
        if self.sheet.is_some() {
            return false;
        }
        if let Some(f) = &mut self.find {
            if f.field.click(x, y, env.mods.shift, env.now_ms) {
                f.focused = true;
                return true;
            }
            if self.view.click(x, y, env.mods.shift, env.now_ms) {
                // Typing goes to the text now; matches stay highlighted.
                f.focused = false;
                return true;
            }
            return false;
        }
        self.view.click(x, y, env.mods.shift, env.now_ms)
    }

    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if self.sheet.is_some() || self.hit_at(x, y).is_some() {
            return self.click(x, y, area, env);
        }
        if let Some(f) = &mut self.find {
            if f.field.double_click(x, y, env.now_ms) {
                return true;
            }
        }
        self.view.double_click(x, y, env.now_ms)
    }

    fn drag(&mut self, x: i32, y: i32, _area: Rect, _env: &mut Env) -> bool {
        if let Some(Sheet::SaveAs { name, .. }) = &mut self.sheet {
            return name.drag(x);
        }
        if let Some(f) = &mut self.find {
            if f.field.drag(x) {
                return true;
            }
        }
        self.view.drag(x, y)
    }

    fn release(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        self.view.release();
        if let Some(f) = &mut self.find {
            f.field.release();
        }
        if let Some(Sheet::SaveAs { name, .. }) = &mut self.sheet {
            name.release();
        }
        false
    }

    fn hover(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        let h = if x < 0 { None } else { self.hit_at(x, y) };
        core::mem::replace(&mut self.hover, h) != h
    }

    fn scroll(&mut self, delta: i32, _area: Rect) -> bool {
        self.sheet.is_none() && self.view.scroll_by(delta)
    }

    fn close_requested(&mut self, _env: &mut Env) -> bool {
        if !self.dirty() {
            return true;
        }
        self.sheet = Some(Sheet::ConfirmClose);
        false
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let mut changed = false;
        if let Some((_, at)) = self.status {
            if env.now_ms - at > 4000 {
                self.status = None;
                changed = true;
            }
        }
        let field_focused = self.find.as_ref().is_some_and(|f| f.focused) || self.sheet.is_some();
        changed |= self.view.tick(env.now_ms, env.focused && !field_focused);
        if let Some(f) = &mut self.find {
            changed |= f.field.tick(env.now_ms, env.focused && self.sheet.is_none() && f.focused);
        }
        if let Some(Sheet::SaveAs { name, .. }) = &mut self.sheet {
            changed |= name.tick(env.now_ms, env.focused);
        }
        changed
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    aurorakit::run(Notes::new(path))
}
