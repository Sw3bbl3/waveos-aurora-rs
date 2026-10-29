//! Real desktop items. Filesystem snapshots and mutations run on a worker.
use super::*;
use crate::gui::{
    clipboard, dnd,
    icons::{self, Icon},
    settings,
};
use crate::{fs, sync::Mutex};
use alloc::{collections::BTreeSet, format, string::ToString};
use aurora_abi::clip;
use aurora_desktop::shortcut::{Shortcut, Target};

use aurora_desktop::layout::{Grid, TILE_H, TILE_W};
#[derive(Clone, PartialEq, Eq)]
pub struct Item {
    path: String,
    name: String,
    dir: bool,
    mtime: u64,
    icon: Icon,
}
enum Job {
    Folder,
    Rename(String, String),
    Link(String, Target),
    Sort(String),
    Resolve(String),
}
enum Resolved {
    Builtin(AppKind),
    Program(String),
    Document(String, String),
    Missing,
}
static RESOLVED: Mutex<Vec<Resolved>> = Mutex::new(Vec::new());
fn read_link(path: &str) -> Option<Shortcut> {
    let (meta, _) = fs::stat(path).ok()?;
    if meta.kind == fs::Kind::Dir || meta.size > 4096 {
        return None;
    }
    Shortcut::parse(&fs::read_all(path).ok()?)
}
fn resolve(path: &str) -> Resolved {
    let mut next = path.to_string();
    let mut seen = BTreeSet::new();
    for _ in 0..16 {
        if !next.ends_with(".wave-link") {
            return classify_document(next);
        }
        if !seen.insert(next.clone()) {
            break;
        }
        match read_link(&next).map(|s| s.target) {
            Some(Target::Builtin(id)) => {
                return apps::CATALOG
                    .iter()
                    .find(|a| builtin_id(a) == id)
                    .map(|a| Resolved::Builtin(a.kind))
                    .unwrap_or(Resolved::Missing)
            }
            Some(Target::Package(id)) => {
                return super::super::installed::list()
                    .into_iter()
                    .find(|a| a.manifest.id == id)
                    .map(|a| Resolved::Program(a.path))
                    .unwrap_or(Resolved::Missing)
            }
            Some(Target::Path(p)) => {
                if p.ends_with(".wave-link") {
                    next = p;
                    continue;
                }
                return if fs::stat(&p).is_ok() { classify_document(p) } else { Resolved::Missing };
            }
            None => break,
        }
    }
    Resolved::Missing
}
static JOBS: Mutex<Vec<Job>> = Mutex::new(Vec::new());
type DesktopSnapshot = (Vec<Item>, Option<String>, bool);
static SNAPSHOT: Mutex<Option<DesktopSnapshot>> = Mutex::new(None);
fn classify_document(path: String) -> Resolved {
    if fs::is_dir(&path) {
        return Resolved::Document(apps::info(AppKind::Files).path.into(), path);
    }
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".elf") || path.starts_with("/System/Bin/") {
        return Resolved::Program(path);
    }
    if lower.ends_with(".gina") {
        return Resolved::Document("/System/Apps/Installer.elf".into(), path);
    }
    if let Some((_, extension)) = lower.rsplit_once('.') {
        if let Some(a) =
            super::super::installed::list().into_iter().find(|a| a.manifest.extensions.iter().any(|e| e == extension))
        {
            return Resolved::Document(a.path, path);
        }
    }
    let kind = if aurora_image::is_image_name(&path) {
        AppKind::Preview
    } else if lower.ends_with(".html") || lower.ends_with(".htm") {
        AppKind::Voyage
    } else {
        AppKind::Notes
    };
    Resolved::Document(apps::info(kind).path.into(), path)
}

fn worker() {
    if !fs::is_dir("/Desktop") {
        let _ = fs::mkdir("/Desktop");
    }
    let mut previous = Vec::new();
    let mut previous_trash = false;
    loop {
        let jobs = core::mem::take(&mut *JOBS.lock());
        let mut error = None;
        for job in jobs {
            let result = (|| -> Result<(), isize> {
                match job {
                    Job::Folder => {
                        let n = fs::trash::unique_in("/Desktop", "New Folder");
                        if n.is_empty() {
                            return Err(fs::EEXIST);
                        }
                        fs::mkdir(&fs::join("/Desktop", &n))?;
                    }
                    Job::Rename(from, name) => {
                        if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\n', '\r', '\0']) {
                            return Err(fs::EINVAL);
                        }
                        let to = fs::join("/Desktop", &name);
                        if to != from {
                            if fs::stat(&to).is_ok() {
                                return Err(fs::EEXIST);
                            }
                            fs::rename(&from, &to)?;
                        }
                    }
                    Job::Link(name, target) => {
                        let text = Shortcut { target }.encode().ok_or(fs::EINVAL)?;
                        let name = fs::trash::unique_in("/Desktop", &format!("{name}.wave-link"));
                        if name.is_empty() {
                            return Err(fs::EEXIST);
                        }
                        let p = fs::write_atomic(&fs::join("/Desktop", &name), text.as_bytes())?;
                        if let Some(e) = p.sync_error {
                            return Err(e);
                        }
                    }
                    Job::Sort(s) => settings::try_set("desktop_sort", &s)?,
                    Job::Resolve(path) => {
                        RESOLVED.lock().push(resolve(&path));
                        return Ok(());
                    }
                }
                fs::lookup("/Desktop")?.0.sync()
            })();
            if let Err(e) = result {
                error = Some(format!("Desktop: {}", aurora_abi::err::name(e)));
            }
        }
        match fs::readdir("/Desktop") {
            Ok(entries) => {
                let mut items = Vec::new();
                for e in entries {
                    if e.name.starts_with(".wave-save-") {
                        continue;
                    }
                    let path = fs::join("/Desktop", &e.name);
                    let mtime = fs::stat(&path).map(|(s, _)| s.mtime).unwrap_or(0);
                    let mut icon = if e.kind == fs::Kind::Dir {
                        Icon::Folder
                    } else if aurora_image::is_image_name(&path) {
                        Icon::Picture
                    } else {
                        Icon::Document
                    };
                    if path.ends_with(".wave-link") {
                        if let Some(link) = read_link(&path) {
                            icon = match link.target {
                                Target::Builtin(id) => apps::CATALOG
                                    .iter()
                                    .find(|a| builtin_id(a) == id)
                                    .map(|a| a.icon)
                                    .unwrap_or(Icon::Document),
                                Target::Package(_) => Icon::Aurora,
                                Target::Path(p) if fs::is_dir(&p) => Icon::Folder,
                                Target::Path(p) if aurora_image::is_image_name(&p) => Icon::Picture,
                                _ => Icon::Document,
                            };
                        }
                    }
                    items.push(Item { icon, path, name: e.name, dir: e.kind == fs::Kind::Dir, mtime });
                }
                items.sort_by(|a, b| a.path.cmp(&b.path));
                let trash_full = fs::trash::count() > 0;
                if items != previous || error.is_some() || trash_full != previous_trash {
                    previous = items.clone();
                    previous_trash = trash_full;
                    *SNAPSHOT.lock() = Some((items, error, trash_full));
                }
            }
            Err(e) => {
                *SNAPSHOT.lock() = Some((
                    previous.clone(),
                    Some(format!("Can't read Desktop: {}", aurora_abi::err::name(e))),
                    previous_trash,
                ))
            }
        }
        crate::sched::sleep_ms(750);
    }
}
#[derive(Default)]
pub struct Items {
    pub focused: bool,
    list: Vec<Item>,
    selected: BTreeSet<String>,
    anchor: Option<usize>,
    offset: usize,
    press: Option<(i32, i32, bool, bool)>, // origin, empty, crossed drag threshold
    marquee: Option<Rect>,
    base: BTreeSet<String>,
    pub(super) menu: Option<(Rect, Vec<(&'static str, u8)>)>,
    pub(super) rename: Option<(String, String)>,
    rename_selected: bool,
    menu_hover: Option<usize>,
    notice: Option<String>,
    sort: u8,
    link: Option<(String, Target)>,
}
impl Items {
    pub fn new() -> Self {
        crate::sched::spawn("desktop-files", worker);
        Self { sort: settings::get_int("desktop_sort", 0) as u8, ..Self::default() }
    }
    fn arrange(&mut self) {
        let sort = self.sort;
        self.list.sort_by(|a, b| {
            aurora_desktop::layout::compare(sort, (&a.name, a.dir, a.mtime), (&b.name, b.dir, b.mtime))
        });
    }
}
impl Desktop {
    fn item_layout(&self) -> Vec<(String, Rect)> {
        let grid = Grid::new(self.w, self.h, MENUBAR_H);
        let mut out: Vec<_> = self
            .items
            .list
            .iter()
            .skip(self.items.offset)
            .take(grid.capacity())
            .enumerate()
            .map(|(i, e)| {
                let (x, y) = grid.position(i);
                (e.path.clone(), Rect::new(x, y, TILE_W, TILE_H))
            })
            .collect();
        out.push((String::from("/Trash"), Rect::new(self.w - TILE_W - 16, self.h - 140, TILE_W, TILE_H)));
        out
    }
    pub(super) fn desktop_target(&self, x: i32, y: i32) -> Option<String> {
        if self.window_at(x, y).is_some() {
            return None;
        }
        if let Some((p, _)) = self.item_layout().into_iter().find(|(_, r)| r.contains(x, y)) {
            if p == "/Trash" || self.items.list.iter().any(|e| e.path == p && e.dir) {
                return Some(p);
            }
        }
        Some(String::from("/Desktop"))
    }
    fn item_at(&self, x: i32, y: i32) -> Option<String> {
        self.item_layout().into_iter().find(|(_, r)| r.contains(x, y)).map(|(p, _)| p)
    }
    pub(super) fn step_items(&mut self) {
        for result in core::mem::take(&mut *RESOLVED.lock()) {
            match result {
                Resolved::Builtin(kind) => self.open(kind),
                Resolved::Program(path) => self.spawn_program(&path, &[]),
                Resolved::Document(program, path) => self.spawn_program(&program, &[&path]),
                Resolved::Missing => {
                    self.items.notice = Some("This shortcut's target is missing or its format is invalid.".into());
                    self.damage_all();
                }
            }
        }
        let result = SNAPSHOT.lock().take();
        if let Some((items, error, trash_full)) = result {
            self.trash_full = trash_full;
            self.items.list = items;
            self.items.arrange();
            self.items.notice = error;
            self.items.selected.retain(|p| p == "/Trash" || self.items.list.iter().any(|e| &e.path == p));
            self.items.offset = self.items.offset.min(self.items.list.len().saturating_sub(1));
            self.damage_all();
        }
    }
    pub(super) fn paint_items(&self, cv: &mut Canvas) {
        for (path, r) in self.item_layout() {
            if !r.intersects(&cv.clip) {
                continue;
            }
            let selected = self.items.selected.contains(&path);
            if selected {
                cv.fill_round_rect(r.inset(3), 10, if self.items.focused { theme::accent() } else { 0x88585868 });
            }
            let entry = self.items.list.iter().find(|e| e.path == path);
            let icon = if path == "/Trash" {
                if self.trash_full {
                    Icon::TrashFull
                } else {
                    Icon::Trash
                }
            } else {
                entry.map(|e| e.icon).unwrap_or(Icon::Document)
            };
            icons::draw(cv, icon, Rect::new(r.x + 32, r.y + 6, 48, 48));
            if path.ends_with(".wave-link") {
                let (x, y) = (r.x + 29, r.y + 38);
                cv.fill_round_rect(Rect::new(x, y, 16, 16), 5, 0xFFF0F0F4);
                cv.line(x + 4, y + 12, x + 12, y + 4, 1, 0xFF202030);
                cv.line(x + 6, y + 4, x + 12, y + 4, 1, 0xFF202030);
                cv.line(x + 12, y + 4, x + 12, y + 10, 1, 0xFF202030);
            }
            let name = entry.map(|e| Shortcut::label(&e.name)).unwrap_or_else(|| "Trash".into());
            let f = theme::ui(12);
            let width = f.width(&name).min(r.w - 10);
            let x = r.x + (r.w - width) / 2;
            cv.text_clipped(
                x + 1,
                r.y + 64,
                &name,
                f,
                if theme::current().dark || selected { 0xDD000000 } else { 0xCCFFFFFF },
                r.w - 10,
            );
            cv.text_clipped(
                x,
                r.y + 63,
                &name,
                f,
                if theme::current().dark || selected { 0xFFFFFFFF } else { 0xFF17223B },
                r.w - 10,
            );
        }
        let capacity = Grid::new(self.w, self.h, MENUBAR_H).capacity();
        if self.items.list.len() > capacity {
            let text = format!(
                "{}–{} of {} · Scroll for more",
                self.items.offset + 1,
                (self.items.offset + capacity).min(self.items.list.len()),
                self.items.list.len()
            );
            let r = Rect::new(16, self.h - 140, theme::ui(12).width(&text) + 24, 32);
            cv.fill_round_rect(r, 8, 0xDD202432);
            cv.text(r.x + 12, r.y + 10, &text, theme::ui(12), 0xFFFFFFFF);
        }
        if let Some(r) = self.items.marquee {
            cv.fill_rect(r, 0x304F8FFF);
            cv.stroke_round_rect(r, 2, 0xBB92B8FF);
        }
    }
    pub(super) fn paint_item_overlay(&self, cv: &mut Canvas) {
        if let Some((r, rows)) = &self.items.menu {
            cv.fill_round_rect(*r, 12, if theme::current().dark { 0xFF292A32 } else { 0xFFF6F6F9 });
            for (i, (label, _)) in rows.iter().enumerate() {
                if self.items.menu_hover == Some(i) {
                    cv.fill_round_rect(Rect::new(r.x + 5, r.y + 6 + i as i32 * 29, r.w - 10, 29), 6, theme::accent());
                }
                cv.text(r.x + 14, r.y + 12 + i as i32 * 29, label, theme::ui(13), theme::current().text);
            }
        }
        if let Some((_, name)) = &self.items.rename {
            let r = Rect::new((self.w - 420) / 2, (self.h - 126) / 2, 420, 126);
            cv.fill_round_rect(r, 16, theme::current().window_bg);
            cv.text(r.x + 20, r.y + 18, "Rename · Enter to save · Esc to cancel", theme::ui(13), theme::current().text);
            cv.fill_round_rect(Rect::new(r.x + 18, r.y + 52, 384, 42), 8, theme::accent());
            cv.text_clipped(r.x + 30, r.y + 64, name, theme::ui(15), 0xFFFFFFFF, 356);
        }
        if let Some(s) = &self.items.notice {
            let r = Rect::new(20, self.h - 140, (theme::ui(13).width(s) + 32).min(self.w - 40), 42);
            cv.fill_round_rect(r, 10, 0xEE262630);
            cv.text_clipped(r.x + 16, r.y + 13, s, theme::ui(13), 0xFFFFFFFF, r.w - 32);
        }
    }
    pub(super) fn items_overlay_press(&mut self, x: i32, y: i32) -> bool {
        if self.items.rename.is_some() {
            return true;
        }
        if let Some((r, rows)) = self.items.menu.take() {
            if r.contains(x, y) {
                let i = ((y - r.y - 6) / 29).max(0) as usize;
                if let Some((_, action)) = rows.get(i) {
                    self.item_action(*action);
                }
            }
            self.damage_all();
            return true;
        }
        false
    }
    pub(super) fn items_press(&mut self, x: i32, y: i32, double: bool) {
        self.items.focused = true;
        self.items.notice = None;
        let hit = self.item_at(x, y);
        let mods = crate::drivers::input::modifiers();
        self.items.base = self.items.selected.clone();
        self.items.press = Some((x, y, hit.is_none(), false));
        if let Some(path) = hit {
            if double {
                self.items.press = None;
                self.open_file(&path);
            } else if mods.ctrl {
                if !self.items.selected.remove(&path) {
                    self.items.selected.insert(path.clone());
                }
            } else if mods.shift {
                if let (Some(a), Some(b)) = (
                    self.items.anchor.filter(|&a| a < self.items.list.len()),
                    self.items.list.iter().position(|e| e.path == path),
                ) {
                    for e in &self.items.list[a.min(b)..=a.max(b)] {
                        self.items.selected.insert(e.path.clone());
                    }
                }
            } else if !self.items.selected.contains(&path) {
                self.items.selected.clear();
                self.items.selected.insert(path.clone());
            }
            self.items.anchor = if path == "/Trash" {
                Some(self.items.list.len())
            } else {
                self.items.list.iter().position(|e| e.path == path)
            };
        } else if !mods.ctrl && !mods.shift {
            self.items.selected.clear();
            self.items.base.clear();
        }
        self.damage_all();
    }
    pub(super) fn items_motion(&mut self, x: i32, y: i32) -> bool {
        if let Some((r, rows)) = &self.items.menu {
            let hover = if r.contains(x, y) && y >= r.y + 6 {
                Some(((y - r.y - 6) / 29) as usize).filter(|i| *i < rows.len())
            } else {
                None
            };
            if hover != self.items.menu_hover {
                self.items.menu_hover = hover;
                self.damage_all();
            }
            return true;
        }
        let Some((sx, sy, empty, dragged)) = self.items.press else { return false };
        if !dragged && (x - sx).abs() + (y - sy).abs() < 6 {
            return true;
        }
        self.items.press = Some((sx, sy, empty, true));
        if empty {
            let r = Rect::new(x.min(sx), y.min(sy), (x - sx).abs(), (y - sy).abs());
            self.items.marquee = Some(r);
            self.items.selected = self.items.base.clone();
            for (p, tile) in self.item_layout() {
                if tile.intersects(&r) {
                    self.items.selected.insert(p);
                }
            }
        } else {
            let paths: Vec<_> = self.items.selected.iter().filter(|p| p.as_str() != "/Trash").cloned().collect();
            if !paths.is_empty() {
                let data = paths.join("\n");
                let _ = dnd::start(0, 0, clip::FILES, data.as_bytes(), paths.len() as u32, 0);
                self.items.press = None;
                self.begin_drag();
            }
        }
        self.damage_all();
        true
    }
    pub(super) fn items_release(&mut self, x: i32, y: i32) -> bool {
        let Some((sx, sy, empty, dragged)) = self.items.press.take() else { return false };
        self.items.marquee = None;
        let mods = crate::drivers::input::modifiers();
        if empty
            && !dragged
            && (x - sx).abs() + (y - sy).abs() < 6
            && !mods.ctrl
            && !mods.shift
            && !mods.alt
            && !mods.super_key
        {
            self.water_click(x, y);
        }
        self.damage_all();
        true
    }
    pub(super) fn items_context(&mut self, x: i32, y: i32) {
        self.items.focused = true;
        self.items.press = None;
        if let Some(p) = self.item_at(x, y) {
            if !self.items.selected.contains(&p) {
                self.items.selected.clear();
                self.items.selected.insert(p);
            }
        } else {
            self.items.selected.clear();
        }
        let mut rows = alloc::vec![
            ("New Folder", 1),
            ("Paste", 5),
            ("Sort by Name", 10),
            ("Sort by Kind", 11),
            ("Sort by Date Modified", 12)
        ];
        if !self.items.selected.is_empty() {
            rows.splice(0..0, [("Open", 0), ("Rename", 2), ("Copy", 3), ("Cut", 4), ("Move to Trash", 6)]);
        }
        self.items.menu_hover = None;
        self.items.menu = Some((
            Rect::new(
                x.min(self.w - 246),
                y.min(self.h - rows.len() as i32 * 29 - 16),
                236,
                rows.len() as i32 * 29 + 16,
            ),
            rows,
        ));
        self.damage_all();
    }
    pub(super) fn file_operation(&mut self, dir: &str, mode: &str, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let mut args = alloc::vec!["--desktop-operation", dir, mode];
        args.extend(paths.iter().map(String::as_str));
        self.spawn_program(apps::info(AppKind::Files).path, &args);
    }
    fn item_action(&mut self, a: u8) {
        let paths: Vec<_> = self.items.selected.iter().filter(|p| p.as_str() != "/Trash").cloned().collect();
        match a {
            0 => {
                let all: Vec<_> = self.items.selected.iter().cloned().collect();
                for p in all.iter().take(8) {
                    self.open_file(p);
                }
            }
            1 => JOBS.lock().push(Job::Folder),
            2 => {
                if paths.len() == 1 {
                    self.items.rename_selected = true;
                    self.items.rename = Some((paths[0].clone(), paths[0].rsplit('/').next().unwrap_or("").into()));
                }
            }
            3 | 4 => {
                let _ = clipboard::set(if a == 4 { clip::CUT_FILES } else { clip::FILES }, paths.join("\n").as_bytes());
            }
            5 => {
                if let Ok(n) = clipboard::get(clip::FILES, &mut []) {
                    let mut data = alloc::vec![0;n];
                    let Ok(actual) = clipboard::get(clip::FILES, &mut data) else { return };
                    if actual > data.len() {
                        self.items.notice = Some("Clipboard changed; please paste again.".into());
                        return;
                    }
                    data.truncate(actual);
                    let paths = String::from_utf8_lossy(&data).lines().map(String::from).collect::<Vec<_>>();
                    let moving = clipboard::get(clip::CUT_FILES, &mut []).is_ok();
                    self.file_operation("/Desktop", if moving { "move" } else { "copy" }, &paths);
                }
            }
            6 => self.file_operation("/Desktop", "trash", &paths),
            20 => {
                if let Some((name, target)) = self.items.link.take() {
                    self.add_desktop_link(&name, target);
                }
            }
            10..=12 => {
                self.items.sort = a - 10;
                self.items.offset = 0;
                self.items.arrange();
                JOBS.lock().push(Job::Sort(self.items.sort.to_string()));
            }
            _ => {}
        }
        self.damage_all();
    }
    pub(super) fn items_key(&mut self, k: &KeyEvent) {
        if !k.pressed {
            return;
        }
        if let Some((path, name)) = &mut self.items.rename {
            if k.mods.ctrl && k.ch.is_some_and(|c| c.eq_ignore_ascii_case(&'a')) {
                self.items.rename_selected = true;
                self.damage_all();
                return;
            }
            match k.code {
                KeyCode::Escape => self.items.rename = None,
                KeyCode::Enter => {
                    JOBS.lock().push(Job::Rename(path.clone(), name.clone()));
                    self.items.rename = None;
                }
                KeyCode::Backspace => {
                    if self.items.rename_selected {
                        name.clear();
                        self.items.rename_selected = false;
                    } else {
                        name.pop();
                    }
                }
                _ => {
                    if let Some(c) = k.ch.filter(|c| !c.is_control()) {
                        if self.items.rename_selected {
                            name.clear();
                            self.items.rename_selected = false;
                        }
                        if name.len() < 240 {
                            name.push(c);
                        }
                    }
                }
            }
            self.damage_all();
            return;
        }
        if let Some((_, rows)) = &self.items.menu {
            let len = rows.len();
            let current = self.items.menu_hover.unwrap_or(0);
            match k.code {
                KeyCode::Escape => self.items.menu = None,
                KeyCode::Down => self.items.menu_hover = Some((current + 1) % len),
                KeyCode::Up => self.items.menu_hover = Some((current + len - 1) % len),
                KeyCode::Enter => {
                    let action = rows[current].1;
                    self.items.menu = None;
                    self.item_action(action);
                }
                _ => {}
            }
            self.damage_all();
            return;
        }
        if k.code == KeyCode::Escape {
            self.items.menu = None;
            self.items.selected.clear();
            self.items.notice = None;
        } else if k.mods.ctrl {
            match k.ch.map(|c| c.to_ascii_lowercase()) {
                Some('a') => {
                    self.items.selected = self.items.list.iter().map(|e| e.path.clone()).collect();
                }
                Some('c') => self.item_action(3),
                Some('x') => self.item_action(4),
                Some('v') => self.item_action(5),
                Some('n') => self.item_action(1),
                _ => {}
            }
        } else {
            match k.code {
                KeyCode::Enter => self.item_action(0),
                KeyCode::F2 => self.item_action(2),
                KeyCode::Delete => self.item_action(6),
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                    let rows = ((self.h - MENUBAR_H - 150) / TILE_H).max(1);
                    let len = self.items.list.len() + 1;
                    if len > 0 {
                        let old = self.items.anchor.unwrap_or(0).min(len - 1);
                        let delta = match k.code {
                            KeyCode::Up => -1,
                            KeyCode::Down => 1,
                            KeyCode::Left => rows,
                            _ => -rows,
                        };
                        let next = (old as i32 + delta).clamp(0, len as i32 - 1) as usize;
                        if !k.mods.shift {
                            self.items.selected.clear();
                        }
                        self.items.selected.insert(if next == len - 1 {
                            String::from("/Trash")
                        } else {
                            self.items.list[next].path.clone()
                        });
                        self.items.anchor = Some(next);
                        let capacity = rows as usize * ((self.w - 24) / TILE_W).max(1) as usize;
                        if next == len - 1 {
                        } else if next < self.items.offset {
                            self.items.offset = next / rows as usize * rows as usize;
                        } else if next >= self.items.offset + capacity {
                            self.items.offset = (next / rows as usize + 1) * rows as usize - capacity;
                        }
                    }
                }
                _ => {}
            }
        }
        self.damage_all();
    }
    pub(super) fn items_scroll(&mut self, dy: i32) {
        let grid = Grid::new(self.w, self.h, MENUBAR_H);
        let max = grid.max_offset(self.items.list.len());
        self.items.offset = (self.items.offset as i32 + if dy < 0 { grid.rows as i32 } else { -(grid.rows as i32) })
            .clamp(0, max as i32) as usize;
        self.damage_all();
    }
    pub(super) fn shortcut_menu(&mut self, x: i32, y: i32, name: String, target: Target) {
        self.items.link = Some((name, target));
        self.items.menu =
            Some((Rect::new(x.min(self.w - 236), y.min(self.h - 50), 236, 45), alloc::vec![("Add to Desktop", 20)]));
        self.damage_all();
    }
    pub(super) fn add_desktop_link(&mut self, name: &str, target: Target) {
        JOBS.lock().push(Job::Link(name.into(), target));
    }
    pub(super) fn open_file_background(&mut self, path: &str) {
        JOBS.lock().push(Job::Resolve(path.into()));
    }
}
/// Persistent catalog IDs are independent of executable paths and display names.
pub(super) fn builtin_id(a: &apps::AppInfo) -> String {
    use AppKind::*;
    let id = match a.kind {
        Welcome => "welcome",
        Learn => "learn",
        About => "about",
        Files => "files",
        Terminal => "terminal",
        Notes => "notes",
        Calculator => "calculator",
        Settings => "settings",
        Preview => "preview",
        Paint => "paint",
        Clock => "clock",
        Activity => "activity",
        NetSurf => "netsurf",
        Power => "power",
        Crash => "crash",
        DisplayConfirm => "display-confirm",
        Other => "other",
        Installer => "installer",
        Studio => "studio",
        Doom => "doom",
        Firefox => "firefox",
        Voyage => "voyage",
    };
    id.into()
}
