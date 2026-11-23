//! The system open and save panel (see `sandbox::panel`): built into the
//! window server, so the app that asked sees only what the person picks.
//! It lists what the logged-in account may read; the places on the left
//! are the home folder's own folders and removable volumes.

use super::{hit, App, AppKind, Env, Request};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::fs::{self, perm};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use crate::gui::widgets::{button, ButtonStyle};
use crate::sandbox::panel;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::sandbox as cap;

const ROW: i32 = 26;
const SIDE: i32 = 150;
/// Names in the home folder that are the system's, not the person's.
const HIDDEN: [&str; 9] = ["Library", "Security", "Settings", "Linux", "Applications", "Users", "System", "Boot", "Trash"];

struct Item {
    name: String,
    dir: bool,
}

pub struct PanelDialog {
    req: panel::Request,
    home: String,
    folder: String,
    items: Vec<Item>,
    selected: Option<usize>,
    scroll: i32,
    /// Saving: the file name.
    name: String,
    answered: bool,
    hovered: Option<usize>,
    places: Vec<(String, String)>,
}

impl PanelDialog {
    pub fn new(req: panel::Request) -> Self {
        let home = req.start.clone();
        let mut places = alloc::vec![(String::from("Home"), home.clone())];
        for name in ["Desktop", "Documents", "Downloads", "Pictures", "Music", "Movies"] {
            let p = fs::join(&home, name);
            if fs::is_dir(&p) && perm::session_allows(&p, perm::R | perm::X) {
                places.push((String::from(name), p));
            }
        }
        for v in fs::readdir("/Volumes").unwrap_or_default() {
            places.push((v.name.clone(), fs::join("/Volumes", &v.name)));
        }
        let name = req.name.clone();
        let mut d = Self {
            req,
            folder: String::new(),
            home,
            items: Vec::new(),
            selected: None,
            scroll: 0,
            name,
            answered: false,
            hovered: None,
            places,
        };
        let start = d.home.clone();
        d.enter(&start);
        d
    }

    fn enter(&mut self, folder: &str) {
        if !perm::session_allows(folder, perm::R | perm::X) {
            return;
        }
        let mut items: Vec<Item> = fs::readdir(folder)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.name.starts_with('.'))
            .filter(|e| !(folder == self.home && HIDDEN.contains(&e.name.as_str())))
            .filter(|e| self.req.mode != cap::PANEL_FOLDER || e.kind == fs::Kind::Dir)
            .map(|e| Item { dir: e.kind == fs::Kind::Dir, name: e.name })
            .collect();
        items.sort_by_cached_key(|i| (!i.dir, i.name.to_lowercase()));
        self.items = items;
        self.folder = String::from(folder);
        self.selected = None;
        self.scroll = 0;
    }

    fn up(&mut self) {
        if self.folder != "/" {
            let parent = fs::parent_path(&self.folder);
            self.enter(&parent);
        }
    }

    fn list_area(area: Rect) -> Rect {
        Rect::new(area.x + SIDE + 12, area.y + 48, area.w - SIDE - 24, area.h - 48 - 70)
    }

    fn place_rects(&self, area: Rect) -> Vec<Rect> {
        (0..self.places.len()).map(|i| Rect::new(area.x + 10, area.y + 48 + i as i32 * 30, SIDE - 12, 28)).collect()
    }

    /// Cancel, the action, Up.
    fn buttons(&self, area: Rect) -> [Rect; 3] {
        let y = area.bottom() - 50;
        [
            Rect::new(area.right() - 214, y, 92, 34),
            Rect::new(area.right() - 112, y, 92, 34),
            Rect::new(area.x + SIDE + 12, area.y + 10, 34, 28),
        ]
    }

    fn name_field(area: Rect) -> Rect {
        Rect::new(area.x + 16, area.bottom() - 50, area.w - 260, 34)
    }

    fn action(&self) -> &'static str {
        match self.req.mode {
            cap::PANEL_SAVE => "Save",
            cap::PANEL_FOLDER => "Choose",
            _ => "Open",
        }
    }

    /// What the action would pick now.
    fn choice(&self) -> Option<String> {
        let sel = self.selected.and_then(|i| self.items.get(i));
        match self.req.mode {
            cap::PANEL_SAVE => {
                let n = self.name.trim();
                (!n.is_empty() && !n.contains('/') && n != "." && n != "..").then(|| fs::join(&self.folder, n))
            }
            cap::PANEL_FOLDER => Some(sel.map(|i| fs::join(&self.folder, &i.name)).unwrap_or_else(|| self.folder.clone())),
            _ => sel.filter(|i| !i.dir).map(|i| fs::join(&self.folder, &i.name)),
        }
    }

    fn finish(&mut self, path: Option<String>, env: &mut Env) {
        if !self.answered {
            self.answered = true;
            panel::answer(self.req.id, path);
        }
        env.requests.push(Request::Close);
    }

    fn confirm(&mut self, env: &mut Env) {
        // Opening a folder goes into it.
        if self.req.mode == cap::PANEL_OPEN {
            if let Some(i) = self.selected.and_then(|i| self.items.get(i)).filter(|i| i.dir) {
                let p = fs::join(&self.folder, &i.name);
                self.enter(&p);
                return;
            }
        }
        if let Some(p) = self.choice() {
            self.finish(Some(p), env);
        }
    }

    fn row_at(&self, area: Rect, x: i32, y: i32) -> Option<usize> {
        let l = Self::list_area(area);
        if !l.contains(x, y) {
            return None;
        }
        let i = ((y - l.y + self.scroll) / ROW) as usize;
        (i < self.items.len()).then_some(i)
    }
}

impl Drop for PanelDialog {
    fn drop(&mut self) {
        if !self.answered {
            panel::answer(self.req.id, None);
        }
    }
}

impl App for PanelDialog {
    fn kind(&self) -> AppKind {
        AppKind::Panel
    }
    fn title(&self) -> String {
        format!("{} — {}", self.action(), self.req.app)
    }
    fn size(&self) -> (i32, i32) {
        (640, 440)
    }
    fn resizable(&self) -> bool {
        false
    }
    fn panel(&self) -> Option<u64> {
        Some(self.req.id)
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        // The places.
        cv.fill_rect(Rect::new(area.x, area.y, SIDE, area.h), with_alpha(t.text, if t.dark { 0x0C } else { 0x07 }));
        cv.text(area.x + 16, area.y + 30, "Places", theme::ui_bold(12), t.text_secondary);
        let rects = self.place_rects(area);
        for (i, ((name, path), r)) in self.places.iter().zip(&rects).enumerate() {
            let here = *path == self.folder;
            if here || self.hovered == Some(10 + i) {
                cv.fill_round_rect(*r, 8, if here { with_alpha(theme::accent(), 0x40) } else { t.hover });
            }
            cv.text_clipped(r.x + 10, r.y + 19, name, theme::ui(13), t.text, r.w - 16);
        }
        // Where it is.
        let [cancel, act, up] = self.buttons(area);
        button(cv, up, "↑", ButtonStyle::Secondary, self.hovered == Some(2));
        cv.text_clipped(up.right() + 10, area.y + 30, &self.folder, theme::ui_bold(13), t.text, area.right() - up.right() - 24);
        // What is in it.
        let l = Self::list_area(area);
        cv.fill_round_rect(l, 10, with_alpha(t.text, if t.dark { 0x10 } else { 0x08 }));
        let saved = cv.clip;
        cv.clip = cv.clip.intersect(&l);
        for (i, item) in self.items.iter().enumerate() {
            let y = l.y + i as i32 * ROW - self.scroll;
            if y + ROW < l.y || y > l.bottom() {
                continue;
            }
            let r = Rect::new(l.x + 4, y + 2, l.w - 8, ROW - 2);
            if self.selected == Some(i) {
                cv.fill_round_rect(r, 6, with_alpha(theme::accent(), 0x55));
            }
            let mark = if item.dir { "▸" } else { "·" };
            cv.text(r.x + 8, y + 19, mark, theme::ui(13), t.text_secondary);
            let dim = self.req.mode == cap::PANEL_SAVE && !item.dir;
            cv.text_clipped(r.x + 26, y + 19, &item.name, theme::ui(13), if dim { t.text_secondary } else { t.text }, r.w - 34);
        }
        if self.items.is_empty() {
            cv.text(l.x + 16, l.y + 26, "Nothing here", theme::ui(13), t.text_secondary);
        }
        cv.clip = saved;
        // Saving: the name.
        if self.req.mode == cap::PANEL_SAVE {
            let f = Self::name_field(area);
            cv.fill_round_rect(f, 10, with_alpha(t.text, if t.dark { 0x12 } else { 0x0B }));
            cv.stroke_round_rect(f, 10, with_alpha(theme::accent(), 0xC0));
            let end = cv.text_clipped(f.x + 12, f.y + 23, &self.name, theme::ui(14), t.text, f.w - 24);
            cv.fill_rect(Rect::new(f.x + 13 + end, f.y + 8, 2, 18), theme::accent_glow());
        } else {
            let note = format!("“{}” may use only what you choose.", self.req.app);
            cv.text_clipped(area.x + 16, cancel.y + 22, &note, theme::ui(12), t.text_secondary, cancel.x - area.x - 28);
        }
        button(cv, cancel, "Cancel", ButtonStyle::Secondary, self.hovered == Some(0));
        let style = if self.choice().is_some() || self.selected.is_some() { ButtonStyle::Primary } else { ButtonStyle::Secondary };
        button(cv, act, self.action(), style, self.hovered == Some(1));
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        match hit(&self.buttons(area), x, y) {
            Some(0) => {
                self.finish(None, env);
                return true;
            }
            Some(1) => {
                self.confirm(env);
                return true;
            }
            Some(2) => {
                self.up();
                return true;
            }
            _ => {}
        }
        if let Some(i) = hit(&self.place_rects(area), x, y) {
            let p = self.places[i].1.clone();
            self.enter(&p);
            return true;
        }
        if let Some(i) = self.row_at(area, x, y) {
            self.selected = Some(i);
            if self.req.mode == cap::PANEL_SAVE && !self.items[i].dir {
                self.name = self.items[i].name.clone();
            }
        }
        true
    }
    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(i) = self.row_at(area, x, y) {
            self.selected = Some(i);
            if self.items[i].dir {
                let p = fs::join(&self.folder, &self.items[i].name);
                self.enter(&p);
            } else {
                self.confirm(env);
            }
        }
        true
    }
    fn scroll(&mut self, notches: (i32, i32), _precise: (i32, i32), area: Rect) -> bool {
        let l = Self::list_area(area);
        let max = (self.items.len() as i32 * ROW - l.h).max(0);
        let s = (self.scroll + notches.1 * ROW * 3).clamp(0, max);
        core::mem::replace(&mut self.scroll, s) != s
    }
    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        match ev.code {
            KeyCode::Escape => self.finish(None, env),
            KeyCode::Enter => self.confirm(env),
            KeyCode::Up => self.selected = Some(self.selected.map_or(0, |i| i.saturating_sub(1))),
            KeyCode::Down if !self.items.is_empty() => {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(self.items.len() - 1)))
            }
            KeyCode::Backspace if self.req.mode == cap::PANEL_SAVE => {
                self.name.pop();
            }
            KeyCode::Backspace => self.up(),
            _ => match ev.ch {
                Some(c) if self.req.mode == cap::PANEL_SAVE && !c.is_control() && c != '/' && !ev.mods.ctrl => {
                    if self.name.chars().count() < 200 {
                        self.name.push(c);
                    }
                }
                _ => return false,
            },
        }
        true
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = hit(&self.buttons(area), x, y).or_else(|| hit(&self.place_rects(area), x, y).map(|i| 10 + i));
        core::mem::replace(&mut self.hovered, h) != h
    }
    fn request_close(&mut self) -> bool {
        if !self.answered {
            self.answered = true;
            panel::answer(self.req.id, None);
        }
        true
    }
}
