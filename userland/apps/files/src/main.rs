//! Files — browse and manage the filesystem.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora::fs::{self, Entry};
use ripple::canvas::{with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::icons::{self, Icon};
use ripple::theme::{self, ACCENT};
use ripple::{App, Env, Request};
use ripple::{KeyCode, KeyEvent};

const SIDEBAR_W: i32 = 170;
const TOOLBAR_H: i32 = 46;
const TILE_W: i32 = 104;
const TILE_H: i32 = 100;
const PLACES: [&str; 5] = ["/Desktop", "/Documents", "/Downloads", "/Pictures", "/System"];

pub struct Files {
    cwd: String,
    entries: Vec<Entry>,
    selected: Option<usize>,
    back: Vec<String>,
    hovered_place: Option<usize>,
}

impl Files {
    pub fn new() -> Self {
        let mut f =
            Self { cwd: String::new(), entries: Vec::new(), selected: None, back: Vec::new(), hovered_place: None };
        f.navigate("/Documents", false);
        f
    }

    fn navigate(&mut self, dir: &str, push: bool) {
        if push && self.cwd != dir {
            self.back.push(self.cwd.clone());
        }
        self.cwd = String::from(dir);
        self.refresh();
        self.selected = None;
    }

    fn refresh(&mut self) {
        self.entries = fs::read_dir(&self.cwd).unwrap_or_default();
    }

    fn main_area(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR_W, area.y + TOOLBAR_H, area.w - SIDEBAR_W, area.h - TOOLBAR_H - 28)
    }

    fn place_rect(area: Rect, i: usize) -> Rect {
        Rect::new(area.x + 10, area.y + 40 + i as i32 * 32, SIDEBAR_W - 20, 28)
    }

    fn back_button(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR_W + 12, area.y + 9, 30, 28)
    }

    fn tile(main: Rect, i: usize) -> Rect {
        let cols = ((main.w - 24) / TILE_W).max(1);
        let (c, r) = (i as i32 % cols, i as i32 / cols);
        Rect::new(main.x + 16 + c * TILE_W, main.y + 12 + r * TILE_H, TILE_W - 8, TILE_H - 8)
    }

    fn entry_at(&self, area: Rect, x: i32, y: i32) -> Option<usize> {
        let main = Self::main_area(area);
        (0..self.entries.len()).find(|&i| Self::tile(main, i).contains(x, y))
    }

    fn open(&mut self, i: usize, env: &mut Env) {
        let e = self.entries[i].clone();
        if e.is_dir {
            self.navigate(&fs::join(&self.cwd, &e.name), true);
        } else {
            env.requests.push(Request::OpenFile(fs::join(&self.cwd, &e.name)));
        }
    }
}

impl App for Files {
    fn title(&self) -> String {
        let name = self.cwd.rsplit('/').next().filter(|s| !s.is_empty()).unwrap_or("RAM Disk");
        format!("{name} — Files")
    }
    fn size(&self) -> (i32, i32) {
        (760, 470)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        self.refresh();
        // Sidebar.
        let side = Rect::new(area.x, area.y, SIDEBAR_W, area.h);
        cv.with_clip(side, |cv| {
            cv.fill_rect_round_bottom(
                Rect::new(area.x, area.y, SIDEBAR_W + 20, area.h),
                theme::WINDOW_RADIUS,
                t.window_bg_alt,
            )
        });
        cv.fill_rect(Rect::new(area.x + SIDEBAR_W - 1, area.y, 1, area.h), t.separator);
        cv.text(area.x + 18, area.y + 26, "Places", theme::ui_bold(12), t.text_secondary);
        for (i, p) in PLACES.iter().enumerate() {
            let r = Self::place_rect(area, i);
            let active = self.cwd == *p || self.cwd.starts_with(&format!("{p}/"));
            if active {
                cv.fill_round_rect(r, 7, with_alpha(ACCENT, 0x30));
            } else if self.hovered_place == Some(i) {
                cv.fill_round_rect(r, 7, t.hover);
            }
            icons::draw(cv, Icon::Folder, Rect::new(r.x + 8, r.y + 5, 18, 18));
            cv.text(r.x + 34, r.y + 19, &p[1..], theme::ui(13), t.text);
        }

        // Toolbar.
        let bb = Self::back_button(area);
        let can_back = !self.back.is_empty();
        cv.fill_round_rect(bb, 7, if can_back { t.hover } else { 0 });
        cv.text_centered(bb, "←", theme::ui_bold(16), if can_back { t.text } else { t.text_secondary });
        cv.text(bb.right() + 12, area.y + 29, &self.cwd, theme::ui_bold(14), t.text);
        cv.fill_rect(Rect::new(area.x + SIDEBAR_W, area.y + TOOLBAR_H - 1, area.w - SIDEBAR_W, 1), t.separator);

        // Items.
        let main = Self::main_area(area);
        cv.with_clip(main, |cv| {
            if self.entries.is_empty() {
                let (cx, cy) = main.center();
                let msg = "This folder is empty";
                let f = theme::ui(14);
                cv.text(cx - f.width(msg) / 2, cy, msg, f, t.text_secondary);
            }
            for (i, e) in self.entries.iter().enumerate() {
                let r = Self::tile(main, i);
                if self.selected == Some(i) {
                    cv.fill_round_rect(r, 10, with_alpha(ACCENT, 0x2C));
                }
                let icon = if e.is_dir { Icon::Folder } else { Icon::Document };
                icons::draw(cv, icon, Rect::new(r.x + (r.w - 52) / 2, r.y + 6, 52, 52));
                let f = theme::ui(12);
                let w = f.width(&e.name).min(r.w - 8);
                cv.text_clipped(r.x + (r.w - w) / 2, r.y + 76, &e.name, f, t.text, r.w - 8);
            }
        });

        // Status bar.
        let status = format!(
            "{} item{} · RAM disk (not saved across reboots)",
            self.entries.len(),
            if self.entries.len() == 1 { "" } else { "s" }
        );
        cv.fill_rect(Rect::new(area.x + SIDEBAR_W, area.bottom() - 28, area.w - SIDEBAR_W, 1), t.separator);
        cv.text(area.x + SIDEBAR_W + 14, area.bottom() - 10, &status, theme::ui(11), t.text_secondary);
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        if let Some(i) = (0..PLACES.len()).find(|&i| Self::place_rect(area, i).contains(x, y)) {
            self.navigate(PLACES[i], true);
            return true;
        }
        if Self::back_button(area).contains(x, y) {
            if let Some(prev) = self.back.pop() {
                self.navigate(&prev, false);
            }
            return true;
        }
        let sel = self.entry_at(area, x, y);
        core::mem::replace(&mut self.selected, sel) != sel
    }

    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(i) = self.entry_at(area, x, y) {
            self.open(i, env);
            return true;
        }
        false
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = (0..PLACES.len()).find(|&i| Self::place_rect(area, i).contains(x, y));
        core::mem::replace(&mut self.hovered_place, h) != h
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed || self.entries.is_empty() {
            return false;
        }
        let n = self.entries.len();
        match ev.code {
            KeyCode::Right | KeyCode::Down => self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(n - 1))),
            KeyCode::Left | KeyCode::Up => self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1))),
            KeyCode::Enter => {
                if let Some(i) = self.selected {
                    self.open(i, env);
                }
            }
            KeyCode::Backspace => {
                if let Some(prev) = self.back.pop() {
                    self.navigate(&prev, false);
                }
            }
            _ => return false,
        }
        true
    }
}

aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let mut app = Files::new();
    if let Some(dir) = args.get(1) {
        app.navigate(dir, false);
    }
    ripple::run(app)
}
