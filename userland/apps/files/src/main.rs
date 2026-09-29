//! Files — browse and manage the filesystem.
//!
//! Sidebar with favourites and volumes, a toolbar (back, New Folder, Rename,
//! Delete), a grid of items, right-click context menus, inline rename,
//! confirmation before deleting, and copy / cut / paste (also Ctrl+C/X/V).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use aurora::fs::{self, Entry};
use ripple::canvas::{with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::icons::{self, Icon};
use ripple::theme::{self, ACCENT};
use ripple::widgets::{button, text_field, ButtonStyle};
use ripple::{App, Env, KeyCode, KeyEvent, Request};

const SIDEBAR_W: i32 = 184;
const TOOLBAR_H: i32 = 48;
const STATUS_H: i32 = 28;
const TILE_W: i32 = 108;
const TILE_H: i32 = 104;
const MENU_W: i32 = 190;
const MENU_ITEM_H: i32 = 28;

struct Place {
    label: &'static str,
    path: &'static str,
    icon: Icon,
}

const FAVORITES: [Place; 4] = [
    Place { label: "Desktop", path: "/Desktop", icon: Icon::Folder },
    Place { label: "Documents", path: "/Documents", icon: Icon::Folder },
    Place { label: "Downloads", path: "/Downloads", icon: Icon::Folder },
    Place { label: "Pictures", path: "/Pictures", icon: Icon::Folder },
];

const LOCATIONS: [Place; 3] = [
    Place { label: "Aurora HD", path: "/", icon: Icon::Files },
    Place { label: "System", path: "/System", icon: Icon::Aurora },
    Place { label: "EFI Boot", path: "/Boot", icon: Icon::Settings },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Open,
    Rename,
    Copy,
    Cut,
    Paste,
    Delete,
    NewFolder,
    NewFile,
}

struct Menu {
    x: i32,
    y: i32,
    items: Vec<(Action, &'static str)>,
    hover: Option<usize>,
}

impl Menu {
    fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, MENU_W, self.items.len() as i32 * MENU_ITEM_H + 10)
    }
    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        let r = self.rect();
        if !r.contains(x, y) {
            return None;
        }
        let i = (y - r.y - 5) / MENU_ITEM_H;
        (i >= 0 && (i as usize) < self.items.len()).then_some(i as usize)
    }
}

struct Clipboard {
    path: String,
    cut: bool,
}

pub struct Files {
    cwd: String,
    entries: Vec<Entry>,
    selected: Option<usize>,
    back: Vec<String>,
    hover_place: Option<(usize, usize)>,
    hover_button: Option<usize>,
    menu: Option<Menu>,
    /// (entry, edited name, whole name selected — the next keystroke replaces it)
    renaming: Option<(usize, String, bool)>,
    confirm_delete: Option<usize>,
    clipboard: Option<Clipboard>,
    status: Option<(String, u64)>,
    scroll: i32,
    caret_on: bool,
}

fn read_only(path: &str) -> bool {
    fs::stat(path).is_ok_and(|s| s.read_only != 0)
}

impl Files {
    pub fn new() -> Self {
        let mut f = Self {
            cwd: String::new(),
            entries: Vec::new(),
            selected: None,
            back: Vec::new(),
            hover_place: None,
            hover_button: None,
            menu: None,
            renaming: None,
            confirm_delete: None,
            clipboard: None,
            status: None,
            scroll: 0,
            caret_on: true,
        };
        f.navigate("/Documents", false);
        f
    }

    fn navigate(&mut self, dir: &str, push: bool) {
        if !fs::is_dir(dir) {
            self.flash(format!("Can't open {dir}"));
            return;
        }
        if push && self.cwd != dir {
            self.back.push(self.cwd.clone());
        }
        self.cwd = String::from(dir);
        self.selected = None;
        self.renaming = None;
        self.scroll = 0;
        self.refresh();
    }

    fn refresh(&mut self) {
        self.entries = fs::read_dir(&self.cwd).unwrap_or_default();
        if let Some(s) = self.selected {
            if s >= self.entries.len() {
                self.selected = None;
            }
        }
    }

    fn flash(&mut self, msg: String) {
        self.status = Some((msg, aurora::time::uptime_ms()));
    }

    fn path_of(&self, i: usize) -> String {
        fs::join(&self.cwd, &self.entries[i].name)
    }

    fn writable_here(&self) -> bool {
        !read_only(&self.cwd)
    }

    // ------------------------------------------------------------ layout

    fn place_rects(area: Rect) -> (Vec<Rect>, Vec<Rect>) {
        let mut y = area.y + 36;
        let fav = (0..FAVORITES.len())
            .map(|_| {
                let r = Rect::new(area.x + 10, y, SIDEBAR_W - 20, 28);
                y += 30;
                r
            })
            .collect();
        y += 34;
        let loc = (0..LOCATIONS.len())
            .map(|_| {
                let r = Rect::new(area.x + 10, y, SIDEBAR_W - 20, 28);
                y += 30;
                r
            })
            .collect();
        (fav, loc)
    }

    fn place_at(area: Rect, x: i32, y: i32) -> Option<(usize, usize)> {
        let (fav, loc) = Self::place_rects(area);
        if let Some(i) = fav.iter().position(|r| r.contains(x, y)) {
            return Some((0, i));
        }
        loc.iter().position(|r| r.contains(x, y)).map(|i| (1, i))
    }

    fn place(section: usize, i: usize) -> &'static Place {
        if section == 0 {
            &FAVORITES[i]
        } else {
            &LOCATIONS[i]
        }
    }

    /// Back, New Folder, Rename, Delete — packed against the right edge;
    /// Rename/Delete only take space when something is selected.
    fn toolbar_buttons(&self, area: Rect) -> [Rect; 4] {
        let y = area.y + 9;
        let right = area.right() - 12;
        let back = Rect::new(area.x + SIDEBAR_W + 12, y, 32, 30);
        if self.selected.is_some() {
            [
                back,
                Rect::new(right - 82 - 8 - 82 - 8 - 102, y, 102, 30),
                Rect::new(right - 82 - 8 - 82, y, 82, 30),
                Rect::new(right - 82, y, 82, 30),
            ]
        } else {
            [back, Rect::new(right - 102, y, 102, 30), Rect::new(0, 0, 0, 0), Rect::new(0, 0, 0, 0)]
        }
    }

    fn main_area(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR_W, area.y + TOOLBAR_H, area.w - SIDEBAR_W, area.h - TOOLBAR_H - STATUS_H)
    }

    fn cols(main: Rect) -> i32 {
        ((main.w - 24) / TILE_W).max(1)
    }

    fn tile(&self, main: Rect, i: usize) -> Rect {
        let cols = Self::cols(main);
        let (c, r) = (i as i32 % cols, i as i32 / cols);
        Rect::new(main.x + 16 + c * TILE_W, main.y + 12 + r * TILE_H - self.scroll, TILE_W - 8, TILE_H - 8)
    }

    fn entry_at(&self, area: Rect, x: i32, y: i32) -> Option<usize> {
        let main = Self::main_area(area);
        if !main.contains(x, y) {
            return None;
        }
        (0..self.entries.len()).find(|&i| self.tile(main, i).contains(x, y))
    }

    fn confirm_rects(area: Rect) -> (Rect, Rect, Rect) {
        let main = Self::main_area(area);
        let d = Rect::new(main.x + (main.w - 380) / 2, main.y + 60, 380, 150);
        (d, Rect::new(d.right() - 112, d.bottom() - 50, 96, 34), Rect::new(d.right() - 220, d.bottom() - 50, 96, 34))
    }

    // ----------------------------------------------------------- actions

    fn run(&mut self, action: Action, env: &mut Env) {
        let sel = self.selected;
        match action {
            Action::Open => {
                if let Some(i) = sel {
                    self.open(i, env);
                }
            }
            Action::Rename => {
                if let Some(i) = sel {
                    if self.writable_here() {
                        self.renaming = Some((i, self.entries[i].name.clone(), true));
                    } else {
                        self.flash(String::from("This location is read-only"));
                    }
                }
            }
            Action::Copy | Action::Cut => {
                if let Some(i) = sel {
                    self.clipboard = Some(Clipboard { path: self.path_of(i), cut: action == Action::Cut });
                    let verb = if action == Action::Cut { "Cut" } else { "Copied" };
                    self.flash(format!("{verb} “{}”", self.entries[i].name));
                }
            }
            Action::Paste => self.paste(),
            Action::Delete => {
                if let Some(i) = sel {
                    if self.writable_here() {
                        self.confirm_delete = Some(i);
                    } else {
                        self.flash(String::from("This location is read-only"));
                    }
                }
            }
            Action::NewFolder => self.create(true),
            Action::NewFile => self.create(false),
        }
    }

    fn open(&mut self, i: usize, env: &mut Env) {
        let e = self.entries[i].clone();
        let path = self.path_of(i);
        if e.is_dir {
            self.navigate(&path, true);
        } else if path.ends_with(".elf") || path.starts_with("/System/Bin/") {
            env.requests.push(Request::OpenApp(path));
        } else {
            env.requests.push(Request::OpenFile(path));
        }
    }

    fn create(&mut self, dir: bool) {
        if !self.writable_here() {
            self.flash(String::from("This location is read-only"));
            return;
        }
        let path = if dir {
            fs::unique_name(&self.cwd, "New Folder", "")
        } else {
            fs::unique_name(&self.cwd, "Untitled", ".txt")
        };
        let r = if dir { fs::mkdir(&path) } else { fs::write(&path, b"") };
        match r {
            Ok(()) => {
                self.refresh();
                let name = fs::file_name(&path).to_string();
                if let Some(i) = self.entries.iter().position(|e| e.name == name) {
                    self.selected = Some(i);
                    self.renaming = Some((i, name, true));
                }
            }
            Err(e) => self.flash(format!("Couldn't create it: {e}")),
        }
    }

    fn paste(&mut self) {
        let Some(clip) = self.clipboard.take() else { return };
        if !self.writable_here() {
            self.flash(String::from("This location is read-only"));
            self.clipboard = Some(clip);
            return;
        }
        let name = fs::file_name(&clip.path).to_string();
        let (base, ext) = match name.rfind('.') {
            Some(i) if i > 0 && !fs::is_dir(&clip.path) => (&name[..i], &name[i..]),
            _ => (name.as_str(), ""),
        };
        let mut target = fs::join(&self.cwd, &name);
        if fs::exists(&target) {
            target = fs::unique_name(&self.cwd, &format!("{base} copy"), ext);
        }
        if (target.clone() + "/").starts_with(&(clip.path.clone() + "/")) {
            self.flash(String::from("Can't paste a folder into itself"));
            return;
        }
        let r = if clip.cut { fs::move_path(&clip.path, &target) } else { fs::copy(&clip.path, &target) };
        match r {
            Ok(()) => {
                self.flash(format!("{} “{}”", if clip.cut { "Moved" } else { "Pasted" }, fs::file_name(&target)));
                if !clip.cut {
                    self.clipboard = Some(clip);
                }
            }
            Err(e) => {
                self.flash(format!("Couldn't paste: {e}"));
                self.clipboard = Some(clip);
            }
        }
        self.refresh();
    }

    fn finish_rename(&mut self, commit: bool) {
        let Some((i, name, _)) = self.renaming.take() else { return };
        let name = String::from(name.trim());
        if !commit || i >= self.entries.len() || name.is_empty() || name == self.entries[i].name {
            return;
        }
        if name.contains('/') {
            self.flash(String::from("Names can't contain “/”"));
            return;
        }
        let from = self.path_of(i);
        let to = fs::join(&self.cwd, &name);
        match fs::rename(&from, &to) {
            Ok(()) => {
                self.refresh();
                self.selected = self.entries.iter().position(|e| e.name == name);
            }
            Err(e) => self.flash(format!("Couldn't rename: {e}")),
        }
    }

    fn delete_confirmed(&mut self) {
        let Some(i) = self.confirm_delete.take() else { return };
        if i >= self.entries.len() {
            return;
        }
        let name = self.entries[i].name.clone();
        match fs::remove_all(&self.path_of(i)) {
            Ok(()) => self.flash(format!("Deleted “{name}”")),
            Err(e) => self.flash(format!("Couldn't delete “{name}”: {e}")),
        }
        self.selected = None;
        self.refresh();
    }

    fn open_menu(&mut self, x: i32, y: i32, area: Rect, on_item: bool) {
        let mut items = Vec::new();
        let writable = self.writable_here();
        if on_item {
            items.push((Action::Open, "Open"));
            if writable {
                items.push((Action::Rename, "Rename"));
            }
            items.push((Action::Copy, "Copy"));
            if writable {
                items.push((Action::Cut, "Cut"));
                items.push((Action::Delete, "Delete…"));
            }
        } else if writable {
            items.push((Action::NewFolder, "New Folder"));
            items.push((Action::NewFile, "New Text File"));
            if self.clipboard.is_some() {
                items.push((Action::Paste, "Paste"));
            }
        }
        if items.is_empty() {
            return;
        }
        let h = items.len() as i32 * MENU_ITEM_H + 10;
        let mx = x.min(area.right() - MENU_W - 4);
        let my = y.min(area.bottom() - h - 4);
        self.menu = Some(Menu { x: mx, y: my, items, hover: None });
    }
}

impl App for Files {
    fn title(&self) -> String {
        let name = match self.cwd.as_str() {
            "/" => "Aurora HD",
            "/Boot" => "EFI Boot",
            p => fs::file_name(p),
        };
        format!("{name} — Files")
    }
    fn size(&self) -> (i32, i32) {
        (800, 480)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        self.refresh();

        // Sidebar.
        let side = Rect::new(area.x, area.y, SIDEBAR_W, area.h);
        cv.fill_rect(side, t.window_bg_alt);
        cv.fill_rect(Rect::new(area.x + SIDEBAR_W - 1, area.y, 1, area.h), t.separator);
        let (fav, loc) = Self::place_rects(area);
        cv.text(area.x + 18, area.y + 26, "Favorites", theme::ui_bold(12), t.text_secondary);
        cv.text(area.x + 18, loc[0].y - 10, "Locations", theme::ui_bold(12), t.text_secondary);
        for (section, rects) in [(0usize, &fav), (1, &loc)] {
            for (i, r) in rects.iter().enumerate() {
                let p = Self::place(section, i);
                let active = self.cwd == p.path
                    || (p.path != "/" && self.cwd.starts_with(&format!("{}/", p.path)))
                    || (section == 1
                        && p.path == "/"
                        && !self.cwd.starts_with("/System")
                        && !self.cwd.starts_with("/Boot")
                        && !FAVORITES
                            .iter()
                            .any(|f| self.cwd == f.path || self.cwd.starts_with(&format!("{}/", f.path))));
                if active {
                    cv.fill_round_rect(*r, 7, with_alpha(ACCENT, 0x30));
                } else if self.hover_place == Some((section, i)) {
                    cv.fill_round_rect(*r, 7, t.hover);
                }
                icons::draw(cv, p.icon, Rect::new(r.x + 8, r.y + 5, 18, 18));
                cv.text(r.x + 34, r.y + 19, p.label, theme::ui(13), t.text);
            }
        }

        // Toolbar.
        let [back, new_folder, rename, delete] = self.toolbar_buttons(area);
        let can_back = !self.back.is_empty();
        if can_back && self.hover_button == Some(0) {
            cv.fill_round_rect(back, 7, t.hover);
        }
        cv.text_centered(back, "←", theme::ui_bold(16), if can_back { t.text } else { t.text_secondary });
        cv.text_clipped(
            back.right() + 10,
            area.y + 30,
            &self.cwd,
            theme::ui_bold(14),
            t.text,
            new_folder.x - back.right() - 20,
        );
        let writable = self.writable_here();
        if writable {
            button(cv, new_folder, "New Folder", ButtonStyle::Secondary, self.hover_button == Some(1));
            if self.selected.is_some() {
                button(cv, rename, "Rename", ButtonStyle::Secondary, self.hover_button == Some(2));
                button(cv, delete, "Delete", ButtonStyle::Secondary, self.hover_button == Some(3));
            }
        } else {
            let msg = "Read-only";
            let f = theme::ui(12);
            cv.text(area.right() - 16 - f.width(msg), area.y + 29, msg, f, t.text_secondary);
        }
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
                let r = self.tile(main, i);
                if r.bottom() < main.y || r.y > main.bottom() {
                    continue;
                }
                if self.selected == Some(i) {
                    cv.fill_round_rect(r, 10, with_alpha(ACCENT, 0x2C));
                }
                let icon = if e.is_dir {
                    Icon::Folder
                } else if e.name.ends_with(".elf") {
                    Icon::Aurora
                } else {
                    Icon::Document
                };
                icons::draw(cv, icon, Rect::new(r.x + (r.w - 52) / 2, r.y + 6, 52, 52));
                match &self.renaming {
                    Some((ri, text, all)) if *ri == i => {
                        let field = Rect::new(r.x - 6, r.y + 62, r.w + 12, 26);
                        if *all {
                            cv.fill_round_rect(field, 8, t.control_bg);
                            cv.stroke_round_rect(field, 8, with_alpha(ACCENT, 0xC0));
                            let f = theme::ui(14);
                            let w = f.width(text).min(field.w - 16);
                            cv.fill_round_rect(
                                Rect::new(field.x + 10, field.y + 4, w + 4, field.h - 8),
                                4,
                                with_alpha(ACCENT, 0x55),
                            );
                            cv.text_clipped(field.x + 12, field.y + 18, text, f, t.text, field.w - 20);
                        } else {
                            // Show the end of long names while typing.
                            let f = theme::ui(14);
                            let mut shown: &str = text;
                            while f.width(shown) > field.w - 28 && !shown.is_empty() {
                                let next = shown.char_indices().nth(1).map(|(i, _)| i).unwrap_or(shown.len());
                                shown = &shown[next..];
                            }
                            text_field(cv, field, shown, "", true, self.caret_on && env.focused);
                        }
                    }
                    _ => {
                        let f = theme::ui(12);
                        let w = f.width(&e.name).min(r.w - 8);
                        cv.text_clipped(r.x + (r.w - w) / 2, r.y + 78, &e.name, f, t.text, r.w - 8);
                    }
                }
            }
        });

        // Status bar.
        let bar_y = area.bottom() - STATUS_H;
        cv.fill_rect(Rect::new(area.x + SIDEBAR_W, bar_y, area.w - SIDEBAR_W, 1), t.separator);
        let status = match &self.status {
            Some((msg, _)) => msg.clone(),
            None => {
                let n = self.entries.len();
                let items = format!("{n} item{}", if n == 1 { "" } else { "s" });
                if self.cwd.starts_with("/System") {
                    format!("{items} · System image (read-only)")
                } else if self.cwd.starts_with("/Boot") {
                    format!("{items} · EFI System Partition")
                } else {
                    let i = aurora::process::sys_info();
                    if i.disk_total > 0 {
                        format!("{items} · {} MB free of {} MB", i.disk_free >> 20, i.disk_total >> 20)
                    } else {
                        format!("{items} · in memory (not saved)")
                    }
                }
            }
        };
        cv.text_clipped(
            area.x + SIDEBAR_W + 14,
            area.bottom() - 10,
            &status,
            theme::ui(11),
            t.text_secondary,
            area.w - SIDEBAR_W - 28,
        );

        // Delete confirmation.
        if let Some(i) = self.confirm_delete {
            if let Some(e) = self.entries.get(i) {
                cv.fill_rect(main, with_alpha(0x000000, 0x30));
                let (d, del, cancel) = Self::confirm_rects(area);
                cv.shadow(d, 12, 20, 6, t.shadow);
                cv.fill_round_rect(d, 12, t.window_bg);
                cv.stroke_round_rect(d, 12, t.window_border);
                let title = format!("Delete “{}”?", e.name);
                cv.text_clipped(d.x + 20, d.y + 34, &title, theme::ui_bold(15), t.text, d.w - 40);
                let what =
                    if e.is_dir { "The folder and everything in it will be deleted." } else { "This can't be undone." };
                cv.text(d.x + 20, d.y + 58, what, theme::ui(13), t.text_secondary);
                button(cv, del, "Delete", ButtonStyle::Danger, false);
                button(cv, cancel, "Cancel", ButtonStyle::Secondary, false);
            }
        }

        // Context menu.
        if let Some(m) = &self.menu {
            let r = m.rect();
            cv.shadow(r, 10, 18, 6, t.shadow * 2 / 3);
            cv.fill_round_rect(r, 10, t.window_bg);
            cv.stroke_round_rect(r, 10, t.window_border);
            for (i, (_, label)) in m.items.iter().enumerate() {
                let ir = Rect::new(r.x + 5, r.y + 5 + i as i32 * MENU_ITEM_H, r.w - 10, MENU_ITEM_H);
                let hovered = m.hover == Some(i);
                if hovered {
                    cv.fill_round_rect(ir, 6, ACCENT);
                }
                cv.text(ir.x + 12, ir.y + 19, label, theme::ui(13), if hovered { 0xFFFF_FFFF } else { t.text });
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(m) = self.menu.take() {
            if let Some(i) = m.item_at(x, y) {
                let action = m.items[i].0;
                self.run(action, env);
            }
            return true;
        }
        if self.confirm_delete.is_some() {
            let (_, del, cancel) = Self::confirm_rects(area);
            if del.contains(x, y) {
                self.delete_confirmed();
            } else if cancel.contains(x, y) {
                self.confirm_delete = None;
            }
            return true;
        }
        if self.renaming.is_some() {
            self.finish_rename(true);
        }
        if let Some((s, i)) = Self::place_at(area, x, y) {
            let path = Self::place(s, i).path;
            self.navigate(path, true);
            return true;
        }
        let [back, new_folder, rename, delete] = self.toolbar_buttons(area);
        if back.contains(x, y) {
            if let Some(prev) = self.back.pop() {
                self.navigate(&prev, false);
            }
            return true;
        }
        if self.writable_here() {
            if new_folder.contains(x, y) {
                self.run(Action::NewFolder, env);
                return true;
            }
            if self.selected.is_some() && rename.contains(x, y) {
                self.run(Action::Rename, env);
                return true;
            }
            if self.selected.is_some() && delete.contains(x, y) {
                self.run(Action::Delete, env);
                return true;
            }
        }
        let sel = self.entry_at(area, x, y);
        if Self::main_area(area).contains(x, y) {
            self.selected = sel;
        }
        true
    }

    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if self.menu.is_some() || self.confirm_delete.is_some() {
            return self.click(x, y, area, env);
        }
        if let Some(i) = self.entry_at(area, x, y) {
            self.selected = Some(i);
            self.open(i, env);
            return true;
        }
        false
    }

    fn right_click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        if self.confirm_delete.is_some() {
            return false;
        }
        self.finish_rename(true);
        if !Self::main_area(area).contains(x, y) {
            return false;
        }
        let hit = self.entry_at(area, x, y);
        self.selected = hit;
        self.open_menu(x, y, area, hit.is_some());
        true
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        if let Some(m) = &mut self.menu {
            let h = m.item_at(x, y);
            return core::mem::replace(&mut m.hover, h) != h;
        }
        let place = Self::place_at(area, x, y);
        let buttons = self.toolbar_buttons(area);
        let button = buttons.iter().position(|r| r.contains(x, y));
        let changed = place != self.hover_place || button != self.hover_button;
        self.hover_place = place;
        self.hover_button = button;
        changed
    }

    fn scroll(&mut self, delta: i32, area: Rect) -> bool {
        let main = Self::main_area(area);
        let rows = (self.entries.len() as i32 + Self::cols(main) - 1) / Self::cols(main);
        let max = (rows * TILE_H + 24 - main.h).max(0);
        let s = (self.scroll + delta * 40).clamp(0, max);
        core::mem::replace(&mut self.scroll, s) != s
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        if let Some((_, text, all)) = &mut self.renaming {
            match ev.code {
                KeyCode::Enter => self.finish_rename(true),
                KeyCode::Escape => self.finish_rename(false),
                _ if ev.mods.ctrl && matches!(ev.ch, Some('a') | Some('A')) => *all = true,
                KeyCode::Backspace => {
                    if core::mem::replace(all, false) {
                        text.clear();
                    } else {
                        text.pop();
                    }
                }
                KeyCode::Left | KeyCode::Right | KeyCode::End | KeyCode::Home => *all = false,
                _ => match ev.ch {
                    Some(c) if !c.is_control() && !ev.mods.ctrl && text.len() < 200 => {
                        if core::mem::replace(all, false) {
                            text.clear();
                        }
                        text.push(c);
                    }
                    _ => return false,
                },
            }
            return true;
        }
        if self.confirm_delete.is_some() {
            match ev.code {
                KeyCode::Enter => self.delete_confirmed(),
                KeyCode::Escape => self.confirm_delete = None,
                _ => return false,
            }
            return true;
        }
        if self.menu.take().is_some() && ev.code == KeyCode::Escape {
            return true;
        }
        if ev.mods.ctrl {
            let action = match ev.ch.map(|c| c.to_ascii_lowercase()) {
                Some('c') => Action::Copy,
                Some('x') => Action::Cut,
                Some('v') => Action::Paste,
                Some('n') => Action::NewFolder,
                _ => return false,
            };
            self.run(action, env);
            return true;
        }
        let n = self.entries.len();
        let cols = 6usize;
        match ev.code {
            KeyCode::Right if n > 0 => self.selected = Some(self.selected.map_or(0, |s| (s + 1).min(n - 1))),
            KeyCode::Left if n > 0 => self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(1))),
            KeyCode::Down if n > 0 => self.selected = Some(self.selected.map_or(0, |s| (s + cols).min(n - 1))),
            KeyCode::Up if n > 0 => self.selected = Some(self.selected.map_or(0, |s| s.saturating_sub(cols))),
            KeyCode::Enter => self.run(Action::Open, env),
            KeyCode::F2 => self.run(Action::Rename, env),
            KeyCode::Delete => self.run(Action::Delete, env),
            KeyCode::Backspace => {
                if let Some(prev) = self.back.pop() {
                    self.navigate(&prev, false);
                }
            }
            _ => return false,
        }
        true
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let mut changed = false;
        if let Some((_, at)) = self.status {
            if env.now_ms.saturating_sub(at) > 4000 {
                self.status = None;
                changed = true;
            }
        }
        if self.renaming.is_some() {
            self.caret_on = (env.now_ms / 530) % 2 == 0;
            changed = true;
        }
        changed
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
