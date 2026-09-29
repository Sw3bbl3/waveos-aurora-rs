//! Files — browse and manage the filesystem.
//!
//! Icon and list views (sortable columns), multiple selection (Ctrl/Shift
//! click, rubber band, keyboard), drag and drop (between windows, onto
//! folders and sidebar places, onto the dock), search as you type, Quick Look
//! (Space), picture thumbnails, inline rename, the system clipboard, and a
//! Trash with Put Back.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora::abi::drag;
use aurora::fs;
use aurora::process::desktop;
use aurora::sync::Mutex;
use aurora_image::Image;
use core::sync::atomic::{AtomicBool, Ordering};
use ripple::canvas::{with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::icons::{self, Icon};
use ripple::text::TextField;
use ripple::theme;
use ripple::widgets::{button, ButtonStyle};
use ripple::{App, Env, KeyCode, KeyEvent, Request};

const SIDEBAR_W: i32 = 190;
const TOOLBAR_H: i32 = 50;
const HEADER_H: i32 = 28;
const STATUS_H: i32 = 28;
const TILE_W: i32 = 108;
const TILE_H: i32 = 108;
const ROW_H: i32 = 26;
const MENU_W: i32 = 200;
const MENU_ITEM_H: i32 = 28;
const TRASH: &str = "/Trash";

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

const LOCATIONS: [Place; 4] = [
    Place { label: "Aurora HD", path: "/", icon: Icon::Files },
    Place { label: "System", path: "/System", icon: Icon::Aurora },
    Place { label: "EFI Boot", path: "/Boot", icon: Icon::Settings },
    Place { label: "Trash", path: TRASH, icon: Icon::Trash },
];

/// A sidebar entry: a fixed place or a mounted volume.
struct PlaceRef<'a> {
    label: &'a str,
    path: &'a str,
    icon: Icon,
    /// A removable volume (shows an eject button).
    volume: bool,
}

impl<'a> From<&'a Place> for PlaceRef<'a> {
    fn from(p: &'a Place) -> Self {
        PlaceRef { label: p.label, path: p.path, icon: p.icon, volume: false }
    }
}

const VOLUMES: &str = "/Volumes";

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Icons,
    List,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SortKey {
    Name,
    Date,
    Size,
    Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Open,
    OpenWith(&'static str),
    QuickLook,
    Rename,
    Copy,
    Cut,
    Paste,
    Duplicate,
    Trash,
    PutBack,
    DeleteForever,
    EmptyTrash,
    NewFolder,
    NewFile,
    ViewIcons,
    ViewList,
    SetWallpaper,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hover {
    Place(usize, usize),
    Back,
    Forward,
    ViewIcons,
    ViewList,
    NewFolder,
    EmptyTrash,
    Column(SortKey),
}

#[derive(Clone)]
struct Item {
    name: String,
    path: String,
    is_dir: bool,
    size: u64,
    mtime: u64,
}

struct Menu {
    x: i32,
    y: i32,
    items: Vec<Option<(Action, &'static str)>>,
    hover: Option<usize>,
}

impl Menu {
    fn rect(&self) -> Rect {
        let h: i32 = self.items.iter().map(|i| if i.is_some() { MENU_ITEM_H } else { 9 }).sum();
        Rect::new(self.x, self.y, MENU_W, h + 10)
    }
    fn rows(&self) -> Vec<Rect> {
        let r = self.rect();
        let mut y = r.y + 5;
        self.items
            .iter()
            .map(|i| {
                let h = if i.is_some() { MENU_ITEM_H } else { 9 };
                let row = Rect::new(r.x + 5, y, r.w - 10, h);
                y += h;
                row
            })
            .collect()
    }
    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows().iter().position(|r| r.contains(x, y)).filter(|&i| self.items[i].is_some())
    }
}

enum Confirm {
    DeleteForever(Vec<String>),
    EmptyTrash,
}

struct Press {
    x: i32,
    y: i32,
    item: Option<usize>,
    /// The item was already selected (so a plain release narrows the selection to it).
    was_selected: bool,
    dragging: bool,
}

/// Shared with the background search thread.
struct SearchState {
    results: Vec<Item>,
    done: bool,
    cancel: AtomicBool,
}

struct QuickLook {
    index: usize,
    image: Option<Image>,
    text: Option<String>,
}

pub struct Files {
    cwd: String,
    items: Vec<Item>,
    selection: BTreeSet<usize>,
    anchor: Option<usize>,
    cursor: Option<usize>,
    back: Vec<String>,
    forward: Vec<String>,
    view: View,
    sort: (SortKey, bool),
    search: TextField,
    search_focused: bool,
    search_state: Option<Arc<Mutex<SearchState>>>,
    search_query: String,
    hover: Option<Hover>,
    menu: Option<Menu>,
    rename: Option<(usize, TextField)>,
    confirm: Option<Confirm>,
    cut: Vec<String>,
    status: Option<(String, u64)>,
    scroll: i32,
    press: Option<Press>,
    band: Option<(i32, i32, i32, i32)>,
    band_base: BTreeSet<usize>,
    drop_target: Option<usize>,
    drop_place: Option<(usize, usize)>,
    drop_here: bool,
    quicklook: Option<QuickLook>,
    thumbs: BTreeMap<String, Option<Image>>,
    typed: (String, u64),
    last_refresh: u64,
    /// Mounted volumes under /Volumes: (name, path).
    volumes: Vec<(String, String)>,
    area: Rect,
}

fn read_only(path: &str) -> bool {
    fs::stat(path).is_ok_and(|s| s.read_only != 0)
}

fn is_image(name: &str) -> bool {
    aurora_image::is_image_name(name)
}

fn is_text(name: &str) -> bool {
    let lower = name.to_lowercase();
    [".txt", ".md", ".conf", ".rs", ".log", ".json", ".csv", ".sh", ".toml"].iter().any(|e| lower.ends_with(e))
        || !name.contains('.')
}

fn icon_of(item: &Item) -> Icon {
    if item.is_dir {
        if item.path == TRASH {
            Icon::Trash
        } else {
            Icon::Folder
        }
    } else if item.name.ends_with(".elf") {
        Icon::Aurora
    } else if is_image(&item.name) {
        Icon::Picture
    } else {
        Icon::Document
    }
}

fn kind_of(item: &Item) -> &'static str {
    if item.is_dir {
        return "Folder";
    }
    let lower = item.name.to_lowercase();
    if lower.ends_with(".png") {
        "PNG image"
    } else if lower.ends_with(".bmp") {
        "BMP image"
    } else if lower.ends_with(".elf") || item.path.starts_with("/System/Bin/") {
        "Application"
    } else if lower.ends_with(".txt") || lower.ends_with(".md") {
        "Text document"
    } else if lower.ends_with(".conf") {
        "Settings file"
    } else {
        "Document"
    }
}

fn human_size(n: u64) -> String {
    if n < 1000 {
        format!("{n} bytes")
    } else if n < 1_000_000 {
        format!("{:.1} KB", n as f64 / 1000.0)
    } else if n < 1_000_000_000 {
        format!("{:.1} MB", n as f64 / 1_000_000.0)
    } else {
        format!("{:.2} GB", n as f64 / 1_000_000_000.0)
    }
}

/// Seconds since 2000-01-01 (local time) → "Sep 29, 2026, 2:05 PM".
fn human_date(secs: u64) -> String {
    if secs == 0 {
        return String::from("—");
    }
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Civil date from days since 2000-03-01 (Howard Hinnant's algorithm, shifted).
    let z = days + 10957 + 719468; // days since 0000-03-01
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let (h, min) = (rem / 3600, rem / 60 % 60);
    let (h12, ampm) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    format!("{} {}, {}, {}:{:02} {}", aurora::time::MONTHS[(m - 1) as usize], d, y, h12, min, ampm)
}

impl Files {
    pub fn new(start: &str) -> Self {
        let mut f = Self {
            cwd: String::new(),
            items: Vec::new(),
            selection: BTreeSet::new(),
            anchor: None,
            cursor: None,
            back: Vec::new(),
            forward: Vec::new(),
            view: View::Icons,
            sort: (SortKey::Name, true),
            search: TextField::new("", "Search"),
            search_focused: false,
            search_state: None,
            search_query: String::new(),
            hover: None,
            menu: None,
            rename: None,
            confirm: None,
            cut: Vec::new(),
            status: None,
            scroll: 0,
            press: None,
            band: None,
            band_base: BTreeSet::new(),
            drop_target: None,
            drop_place: None,
            drop_here: false,
            quicklook: None,
            thumbs: BTreeMap::new(),
            typed: (String::new(), 0),
            last_refresh: 0,
            volumes: Vec::new(),
            area: Rect::new(0, 0, 860, 520),
        };
        f.refresh_volumes();
        f.navigate(start, false);
        f
    }

    // ---------------------------------------------------------- navigation

    fn searching(&self) -> bool {
        !self.search_query.is_empty()
    }

    fn navigate(&mut self, dir: &str, push: bool) {
        if dir == TRASH && !fs::is_dir(TRASH) {
            let _ = fs::mkdir(TRASH);
        }
        if !fs::is_dir(dir) {
            self.flash(format!("Can't open {dir}"));
            return;
        }
        if push && self.cwd != dir {
            self.back.push(self.cwd.clone());
            self.forward.clear();
        }
        self.cwd = String::from(dir);
        self.stop_search();
        self.search.edit.set_text("");
        self.search_query.clear();
        self.selection.clear();
        self.anchor = None;
        self.cursor = None;
        self.rename = None;
        self.quicklook = None;
        self.scroll = 0;
        self.refresh();
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            self.forward.push(self.cwd.clone());
            self.navigate(&prev, false);
        }
    }

    fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            self.back.push(self.cwd.clone());
            self.navigate(&next, false);
        }
    }

    fn go_up(&mut self) {
        if self.cwd != "/" {
            let parent = String::from(fs::parent(&self.cwd));
            self.navigate(&parent, true);
        }
    }

    /// Re-reads the folder, keeping the selection (by path).
    fn refresh(&mut self) {
        if self.searching() {
            return;
        }
        let selected: Vec<String> =
            self.selection.iter().filter_map(|&i| self.items.get(i)).map(|e| e.path.clone()).collect();
        let cwd = self.cwd.clone();
        self.items = fs::read_dir(&cwd)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.name.starts_with('.'))
            .map(|e| Item {
                path: fs::join(&cwd, &e.name),
                name: e.name,
                is_dir: e.is_dir,
                size: e.size,
                mtime: e.mtime,
            })
            .collect();
        self.sort_items();
        self.selection =
            self.items.iter().enumerate().filter(|(_, e)| selected.contains(&e.path)).map(|(i, _)| i).collect();
        if self.cursor.is_some_and(|c| c >= self.items.len()) {
            self.cursor = None;
        }
        self.last_refresh = aurora::time::uptime_ms();
    }

    fn sort_items(&mut self) {
        let (key, asc) = self.sort;
        self.items.sort_by(|a, b| {
            let dirs = b.is_dir.cmp(&a.is_dir);
            let ord = match key {
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Date => a.mtime.cmp(&b.mtime),
                SortKey::Size => a.size.cmp(&b.size),
                SortKey::Kind => kind_of(a).cmp(kind_of(b)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
            };
            dirs.then(if asc { ord } else { ord.reverse() })
        });
    }

    fn set_sort(&mut self, key: SortKey) {
        let selected: Vec<String> = self.selected_paths();
        self.sort = if self.sort.0 == key { (key, !self.sort.1) } else { (key, true) };
        self.sort_items();
        self.selection =
            self.items.iter().enumerate().filter(|(_, e)| selected.contains(&e.path)).map(|(i, _)| i).collect();
    }

    fn flash(&mut self, msg: String) {
        self.status = Some((msg, aurora::time::uptime_ms()));
    }

    fn selected_paths(&self) -> Vec<String> {
        self.selection.iter().filter_map(|&i| self.items.get(i)).map(|e| e.path.clone()).collect()
    }

    fn single(&self) -> Option<usize> {
        (self.selection.len() == 1).then(|| *self.selection.iter().next().unwrap())
    }

    fn in_trash(&self) -> bool {
        self.cwd == TRASH && !self.searching()
    }

    fn writable_here(&self) -> bool {
        !self.searching() && !read_only(&self.cwd)
    }

    fn select_only(&mut self, i: usize) {
        self.selection.clear();
        self.selection.insert(i);
        self.anchor = Some(i);
        self.cursor = Some(i);
    }

    // ------------------------------------------------------------ search

    fn stop_search(&mut self) {
        if let Some(s) = self.search_state.take() {
            s.lock().cancel.store(true, Ordering::Relaxed);
        }
    }

    fn start_search(&mut self) {
        self.stop_search();
        let q = String::from(self.search.text().trim());
        self.search_query = q.clone();
        self.selection.clear();
        self.cursor = None;
        self.scroll = 0;
        if q.is_empty() {
            self.refresh();
            return;
        }
        self.items.clear();
        let state =
            Arc::new(Mutex::new(SearchState { results: Vec::new(), done: false, cancel: AtomicBool::new(false) }));
        self.search_state = Some(state.clone());
        let root = self.cwd.clone();
        let needle = q.to_lowercase();
        let job = aurora::thread::spawn(move || {
            let mut stack = alloc::vec![root];
            let mut found = 0usize;
            while let Some(dir) = stack.pop() {
                if state.lock().cancel.load(Ordering::Relaxed) {
                    return;
                }
                let mut batch = Vec::new();
                for e in fs::read_dir(&dir).unwrap_or_default() {
                    if e.name.starts_with('.') {
                        continue;
                    }
                    let path = fs::join(&dir, &e.name);
                    if e.is_dir {
                        stack.push(path.clone());
                    }
                    if e.name.to_lowercase().contains(&needle) {
                        batch.push(Item { name: e.name, path, is_dir: e.is_dir, size: e.size, mtime: e.mtime });
                    }
                }
                found += batch.len();
                state.lock().results.extend(batch);
                if found > 2000 {
                    break;
                }
            }
            state.lock().done = true;
        });
        if job.is_err() {
            self.flash(String::from("Couldn't start the search"));
        }
    }

    /// Pulls new results from the search thread; true if anything changed.
    fn poll_search(&mut self) -> bool {
        let Some(state) = &self.search_state else { return false };
        let mut s = state.lock();
        if s.results.is_empty() {
            return false;
        }
        self.items.extend(s.results.drain(..));
        drop(s);
        self.sort_items();
        true
    }

    fn search_done(&self) -> bool {
        self.search_state.as_ref().is_none_or(|s| s.lock().done)
    }

    // ------------------------------------------------------------ layout

    fn place_rects(&self, area: Rect) -> (Vec<Rect>, Vec<Rect>) {
        let mut y = area.y + 38;
        let fav = (0..FAVORITES.len())
            .map(|_| {
                let r = Rect::new(area.x + 10, y, SIDEBAR_W - 20, 28);
                y += 30;
                r
            })
            .collect();
        y += 34;
        let loc = (0..LOCATIONS.len() + self.volumes.len())
            .map(|_| {
                let r = Rect::new(area.x + 10, y, SIDEBAR_W - 20, 28);
                y += 30;
                r
            })
            .collect();
        (fav, loc)
    }

    fn place_at(&self, area: Rect, x: i32, y: i32) -> Option<(usize, usize)> {
        let (fav, loc) = self.place_rects(area);
        if let Some(i) = fav.iter().position(|r| r.contains(x, y)) {
            return Some((0, i));
        }
        loc.iter().position(|r| r.contains(x, y)).map(|i| (1, i))
    }

    /// Locations: the fixed places, then volumes, then the Trash last.
    fn place(&self, section: usize, i: usize) -> PlaceRef<'_> {
        let fixed = LOCATIONS.len() - 1;
        match (section, i) {
            (0, i) => (&FAVORITES[i]).into(),
            (_, i) if i < fixed => (&LOCATIONS[i]).into(),
            (_, i) if i < fixed + self.volumes.len() => {
                let (name, path) = &self.volumes[i - fixed];
                PlaceRef { label: name, path, icon: Icon::Files, volume: true }
            }
            _ => (&LOCATIONS[fixed]).into(),
        }
    }

    /// The eject button on a volume's sidebar row.
    fn eject_rect(row: Rect) -> Rect {
        Rect::new(row.right() - 26, row.y + 4, 20, 20)
    }

    /// Re-reads /Volumes; leaves a volume that has gone away.
    fn refresh_volumes(&mut self) -> bool {
        let mut v: Vec<(String, String)> = fs::read_dir(VOLUMES)
            .map(|es| {
                es.into_iter().filter(|e| e.is_dir).map(|e| (e.name.clone(), format!("{VOLUMES}/{}", e.name))).collect()
            })
            .unwrap_or_default();
        v.sort();
        if v == self.volumes {
            return false;
        }
        self.volumes = v;
        let inside_gone = self.cwd.starts_with(VOLUMES)
            && !self.volumes.iter().any(|(_, p)| self.cwd == *p || self.cwd.starts_with(&format!("{p}/")));
        if inside_gone {
            self.navigate("/", true);
        }
        true
    }

    fn toolbar(&self, area: Rect) -> Vec<(Hover, Rect)> {
        let y = area.y + 10;
        let x0 = area.x + SIDEBAR_W + 10;
        let r = area.right() - 12;
        let mut v = alloc::vec![
            (Hover::Back, Rect::new(x0, y, 30, 30)),
            (Hover::Forward, Rect::new(x0 + 32, y, 30, 30)),
            (Hover::ViewList, Rect::new(r - 200 - 10 - 34, y, 34, 30)),
            (Hover::ViewIcons, Rect::new(r - 200 - 10 - 70, y, 34, 30)),
        ];
        if self.in_trash() {
            v.push((Hover::EmptyTrash, Rect::new(r - 200 - 10 - 70 - 118, y, 108, 30)));
        } else if self.writable_here() {
            v.push((Hover::NewFolder, Rect::new(r - 200 - 10 - 70 - 42, y, 34, 30)));
        }
        v
    }

    fn search_rect(area: Rect) -> Rect {
        Rect::new(area.right() - 212, area.y + 10, 200, 30)
    }

    fn main_area(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR_W, area.y + TOOLBAR_H, area.w - SIDEBAR_W, area.h - TOOLBAR_H - STATUS_H)
    }

    /// Where items are laid out (below the list header in list view).
    fn content(&self, area: Rect) -> Rect {
        let m = Self::main_area(area);
        match self.view {
            View::Icons => m,
            View::List => Rect::new(m.x, m.y + HEADER_H, m.w, m.h - HEADER_H),
        }
    }

    fn cols(&self, area: Rect) -> i32 {
        match self.view {
            View::Icons => ((self.content(area).w - 24) / TILE_W).max(1),
            View::List => 1,
        }
    }

    /// Screen rectangle of item `i`.
    fn item_rect(&self, area: Rect, i: usize) -> Rect {
        let c = self.content(area);
        match self.view {
            View::Icons => {
                let cols = self.cols(area);
                let (col, row) = (i as i32 % cols, i as i32 / cols);
                Rect::new(c.x + 16 + col * TILE_W, c.y + 12 + row * TILE_H - self.scroll, TILE_W - 8, TILE_H - 8)
            }
            View::List => Rect::new(c.x + 8, c.y + 4 + i as i32 * ROW_H - self.scroll, c.w - 16, ROW_H),
        }
    }

    fn item_at(&self, area: Rect, x: i32, y: i32) -> Option<usize> {
        let c = self.content(area);
        if !c.contains(x, y) {
            return None;
        }
        match self.view {
            View::Icons => {
                (0..self.items.len()).find(|&i| {
                    let r = self.item_rect(area, i);
                    // The icon and the name, not the whole tile.
                    Rect::new(r.x + 8, r.y, r.w - 16, r.h).contains(x, y)
                })
            }
            View::List => {
                let i = (y - c.y - 4 + self.scroll) / ROW_H;
                (i >= 0 && (i as usize) < self.items.len() && x < c.right() - 8).then_some(i as usize)
            }
        }
    }

    fn columns(&self, area: Rect) -> [(SortKey, &'static str, Rect); 4] {
        let m = Self::main_area(area);
        let w = m.w - 16;
        let name_w = w - 170 - 90 - 110;
        let x = m.x + 8;
        [
            (SortKey::Name, "Name", Rect::new(x, m.y, name_w, HEADER_H)),
            (SortKey::Date, "Date Modified", Rect::new(x + name_w, m.y, 170, HEADER_H)),
            (SortKey::Size, "Size", Rect::new(x + name_w + 170, m.y, 90, HEADER_H)),
            (SortKey::Kind, "Kind", Rect::new(x + name_w + 260, m.y, 110, HEADER_H)),
        ]
    }

    fn content_height(&self, area: Rect) -> i32 {
        match self.view {
            View::Icons => {
                let rows = (self.items.len() as i32 + self.cols(area) - 1) / self.cols(area);
                rows * TILE_H + 24
            }
            View::List => self.items.len() as i32 * ROW_H + 8,
        }
    }

    fn clamp_scroll(&mut self) {
        let area = self.area;
        let max = (self.content_height(area) - self.content(area).h).max(0);
        self.scroll = self.scroll.clamp(0, max);
    }

    fn reveal(&mut self, i: usize) {
        let area = self.area;
        let c = self.content(area);
        let r = self.item_rect(area, i);
        if r.y < c.y {
            self.scroll -= c.y - r.y + 8;
        } else if r.bottom() > c.bottom() {
            self.scroll += r.bottom() - c.bottom() + 8;
        }
        self.clamp_scroll();
    }

    fn confirm_rects(area: Rect) -> (Rect, Rect, Rect) {
        let main = Self::main_area(area);
        let d = Rect::new(main.x + (main.w - 400) / 2, main.y + 50, 400, 150);
        (d, Rect::new(d.right() - 124, d.bottom() - 50, 108, 34), Rect::new(d.right() - 232, d.bottom() - 50, 96, 34))
    }

    // ------------------------------------------------------------ actions

    fn run(&mut self, action: Action, env: &mut Env) {
        let paths = self.selected_paths();
        match action {
            Action::Open => {
                let sel: Vec<usize> = self.selection.iter().copied().collect();
                for i in sel.into_iter().take(8) {
                    self.open(i, env);
                }
            }
            Action::OpenWith(app) => {
                for p in paths.into_iter().take(8) {
                    let _ =
                        aurora::process::spawn(&format!("/System/Apps/{app}.elf"), &[p.as_str()], Default::default());
                }
            }
            Action::QuickLook => self.toggle_quicklook(),
            Action::Rename => {
                if let Some(i) = self.single() {
                    if self.writable_here() || self.searching() && !read_only(&self.items[i].path) {
                        let name = self.items[i].name.clone();
                        let mut field = TextField::new(&name, "Name");
                        let stem = if self.items[i].is_dir {
                            name.len()
                        } else {
                            name.rfind('.').filter(|&k| k > 0).unwrap_or(name.len())
                        };
                        field.edit.select(0, stem);
                        self.rename = Some((i, field));
                    } else {
                        self.flash(String::from("This location is read-only"));
                    }
                }
            }
            Action::Copy | Action::Cut => {
                if paths.is_empty() {
                    return;
                }
                aurora::clipboard::set_files(&paths);
                self.cut = if action == Action::Cut { paths.clone() } else { Vec::new() };
                let what = if paths.len() == 1 {
                    format!("“{}”", fs::file_name(&paths[0]))
                } else {
                    format!("{} items", paths.len())
                };
                self.flash(format!("{} {what}", if action == Action::Cut { "Cut" } else { "Copied" }));
            }
            Action::Paste => self.paste(),
            Action::Duplicate => {
                for p in paths {
                    let dir = String::from(fs::parent(&p));
                    let (stem, ext) = split_ext(fs::file_name(&p), fs::is_dir(&p));
                    let target = fs::unique_name(&dir, &format!("{stem} copy"), &ext);
                    if let Err(e) = fs::copy(&p, &target) {
                        self.flash(format!("Couldn't duplicate: {e}"));
                    }
                }
                self.refresh();
            }
            Action::Trash => self.trash(paths),
            Action::PutBack => {
                let n = paths.len();
                for p in &paths {
                    if let Err(e) = desktop::put_back(fs::file_name(p)) {
                        self.flash(format!("Couldn't put back “{}”: {e}", fs::file_name(p)));
                    }
                }
                self.flash(format!("Put back {n} item{}", if n == 1 { "" } else { "s" }));
                self.refresh();
            }
            Action::DeleteForever => {
                if !paths.is_empty() {
                    self.confirm = Some(Confirm::DeleteForever(paths));
                }
            }
            Action::EmptyTrash => self.confirm = Some(Confirm::EmptyTrash),
            Action::NewFolder => self.create(true),
            Action::NewFile => self.create(false),
            Action::ViewIcons => {
                self.view = View::Icons;
                self.scroll = 0;
            }
            Action::ViewList => {
                self.view = View::List;
                self.scroll = 0;
            }
            Action::SetWallpaper => {
                if let Some(p) = paths.first() {
                    env.requests.push(Request::SetWallpaperImage(p.clone()));
                    self.flash(String::from("Set as desktop wallpaper"));
                }
            }
        }
    }

    fn open(&mut self, i: usize, env: &mut Env) {
        let Some(e) = self.items.get(i).cloned() else { return };
        if e.is_dir {
            self.navigate(&e.path, true);
        } else if e.path.ends_with(".elf") || e.path.starts_with("/System/Bin/") {
            env.requests.push(Request::OpenApp(e.path));
        } else {
            env.requests.push(Request::OpenFile(e.path));
        }
    }

    fn trash(&mut self, paths: Vec<String>) {
        if paths.is_empty() {
            return;
        }
        if self.in_trash() {
            self.confirm = Some(Confirm::DeleteForever(paths));
            return;
        }
        let mut moved = 0;
        for p in &paths {
            match desktop::trash(p) {
                Ok(()) => moved += 1,
                Err(e) => self.flash(format!("Couldn't move “{}” to the Trash: {e}", fs::file_name(p))),
            }
        }
        if moved > 0 {
            self.flash(if moved == 1 {
                format!("Moved “{}” to the Trash", fs::file_name(&paths[0]))
            } else {
                format!("Moved {moved} items to the Trash")
            });
        }
        self.selection.clear();
        if self.searching() {
            self.start_search();
        } else {
            self.refresh();
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
                if let Some(i) = self.items.iter().position(|e| e.path == path) {
                    self.select_only(i);
                    self.reveal(i);
                    let mut env = Env {
                        now_ms: 0,
                        focused: true,
                        screen: (0, 0),
                        requests: Vec::new(),
                        mods: Default::default(),
                    };
                    self.run(Action::Rename, &mut env);
                }
            }
            Err(e) => self.flash(format!("Couldn't create it: {e}")),
        }
    }

    /// Copies (or moves) `sources` into `dir`. Returns how many succeeded.
    fn transfer(&mut self, sources: &[String], dir: &str, mv: bool) -> usize {
        let mut done = 0;
        for src in sources {
            if fs::parent(src) == dir && mv {
                continue; // already there
            }
            let into_itself = (String::from(dir) + "/").starts_with(&(src.clone() + "/"));
            if into_itself {
                self.flash(String::from("Can't put a folder inside itself"));
                continue;
            }
            let name = fs::file_name(src);
            let mut target = fs::join(dir, name);
            if fs::exists(&target) {
                let (stem, ext) = split_ext(name, fs::is_dir(src));
                target = fs::unique_name(dir, &format!("{stem} copy"), &ext);
            }
            let r = if mv { fs::move_path(src, &target) } else { fs::copy(src, &target) };
            match r {
                Ok(()) => done += 1,
                Err(e) => self.flash(format!("Couldn't {} “{name}”: {e}", if mv { "move" } else { "copy" })),
            }
        }
        done
    }

    fn paste(&mut self) {
        if !self.writable_here() {
            self.flash(String::from("This location is read-only"));
            return;
        }
        let Some(paths) = aurora::clipboard::files() else {
            self.flash(String::from("Nothing to paste"));
            return;
        };
        let mv = !self.cut.is_empty() && self.cut == paths;
        let cwd = self.cwd.clone();
        let n = self.transfer(&paths, &cwd, mv);
        if mv {
            self.cut.clear();
        }
        if n > 0 {
            self.flash(format!("{} {n} item{}", if mv { "Moved" } else { "Pasted" }, if n == 1 { "" } else { "s" }));
        }
        self.refresh();
    }

    fn finish_rename(&mut self, commit: bool) {
        let Some((i, field)) = self.rename.take() else { return };
        let name = String::from(field.text().trim());
        let Some(item) = self.items.get(i).cloned() else { return };
        if !commit || name.is_empty() || name == item.name {
            return;
        }
        if name.contains('/') {
            self.flash(String::from("Names can't contain “/”"));
            return;
        }
        let to = fs::join(fs::parent(&item.path), &name);
        match fs::rename(&item.path, &to) {
            Ok(()) => {
                self.refresh();
                if let Some(k) = self.items.iter().position(|e| e.path == to) {
                    self.select_only(k);
                }
            }
            Err(e) => self.flash(format!("Couldn't rename: {e}")),
        }
    }

    fn confirmed(&mut self) {
        match self.confirm.take() {
            Some(Confirm::DeleteForever(paths)) => {
                for p in &paths {
                    if let Err(e) = fs::remove_all(p) {
                        self.flash(format!("Couldn't delete “{}”: {e}", fs::file_name(p)));
                    }
                }
                self.selection.clear();
                self.refresh();
            }
            Some(Confirm::EmptyTrash) => {
                match desktop::empty_trash() {
                    Ok(()) => self.flash(String::from("The Trash is empty")),
                    Err(e) => self.flash(format!("Couldn't empty the Trash: {e}")),
                }
                self.selection.clear();
                self.refresh();
            }
            None => {}
        }
    }

    fn open_menu(&mut self, x: i32, y: i32, on_item: bool) {
        let mut items: Vec<Option<(Action, &'static str)>> = Vec::new();
        let writable = self.writable_here();
        let paths = self.selected_paths();
        let one = paths.len() == 1;
        if on_item && self.in_trash() {
            items.push(Some((Action::PutBack, "Put Back")));
            items.push(Some((Action::DeleteForever, "Delete Immediately…")));
        } else if on_item {
            items.push(Some((Action::Open, "Open")));
            let pictures = paths.iter().all(|p| is_image(p));
            if paths.iter().all(|p| !fs::is_dir(p)) {
                items.push(Some((
                    Action::OpenWith(if pictures { "Notes" } else { "Preview" }),
                    if pictures { "Open with Notes" } else { "Open with Preview" },
                )));
            }
            items.push(Some((Action::QuickLook, "Quick Look")));
            items.push(None);
            if one && !read_only(&paths[0]) {
                items.push(Some((Action::Rename, "Rename")));
            }
            items.push(Some((Action::Copy, "Copy")));
            if !paths.iter().any(|p| read_only(p)) {
                items.push(Some((Action::Cut, "Cut")));
                items.push(Some((Action::Duplicate, "Duplicate")));
            }
            if one && pictures {
                items.push(None);
                items.push(Some((Action::SetWallpaper, "Set as Wallpaper")));
            }
            if !paths.iter().any(|p| read_only(p) || p.starts_with("/System")) {
                items.push(None);
                items.push(Some((Action::Trash, "Move to Trash")));
            }
        } else {
            if self.in_trash() {
                items.push(Some((Action::EmptyTrash, "Empty Trash…")));
            } else if writable {
                items.push(Some((Action::NewFolder, "New Folder")));
                items.push(Some((Action::NewFile, "New Text File")));
                if aurora::clipboard::files().is_some() {
                    items.push(Some((Action::Paste, "Paste")));
                }
            }
            if !items.is_empty() {
                items.push(None);
            }
            items.push(Some((
                if self.view == View::Icons { Action::ViewList } else { Action::ViewIcons },
                if self.view == View::Icons { "View as List" } else { "View as Icons" },
            )));
        }
        let menu = Menu { x, y, items, hover: None };
        let r = menu.rect();
        let area = self.area;
        let (mx, my) = (x.min(area.right() - r.w - 4), y.min(area.bottom() - r.h - 4));
        self.menu = Some(Menu { x: mx, y: my, ..menu });
    }

    // --------------------------------------------------------- quick look

    fn toggle_quicklook(&mut self) {
        if self.quicklook.take().is_some() {
            return;
        }
        let i = self.cursor.or_else(|| self.selection.iter().next().copied());
        if let Some(i) = i {
            self.quicklook = Some(self.load_quicklook(i));
        }
    }

    fn load_quicklook(&self, i: usize) -> QuickLook {
        let item = &self.items[i];
        let mut q = QuickLook { index: i, image: None, text: None };
        if !item.is_dir && item.size < 16 << 20 {
            if is_image(&item.name) {
                q.image = fs::read(&item.path)
                    .ok()
                    .and_then(|d| aurora_image::decode(&d).ok())
                    .map(|img| img.thumbnail(900, 620));
            } else if is_text(&item.name) {
                q.text = fs::read(&item.path).ok().map(|d| {
                    let s = String::from_utf8_lossy(&d[..d.len().min(8192)]).into_owned();
                    s.lines().take(40).collect::<Vec<_>>().join("\n")
                });
            }
        }
        q
    }

    // ------------------------------------------------------------ drawing

    fn draw_sidebar(&self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let side = Rect::new(area.x, area.y, SIDEBAR_W, area.h);
        cv.fill_rect(side, t.window_bg_alt);
        cv.fill_rect(Rect::new(area.x + SIDEBAR_W - 1, area.y, 1, area.h), t.separator);
        let (fav, loc) = self.place_rects(area);
        cv.text(area.x + 18, area.y + 27, "Favorites", theme::ui_bold(12), t.text_secondary);
        cv.text(area.x + 18, loc[0].y - 10, "Locations", theme::ui_bold(12), t.text_secondary);
        for (section, rects) in [(0usize, &fav), (1, &loc)] {
            for (i, r) in rects.iter().enumerate() {
                let p = self.place(section, i);
                let inside =
                    |root: &str| self.cwd == root || (root != "/" && self.cwd.starts_with(&format!("{root}/")));
                let active = if p.path == "/" {
                    !self.cwd.starts_with("/System")
                        && !self.cwd.starts_with("/Boot")
                        && !inside(VOLUMES)
                        && !inside(TRASH)
                        && !FAVORITES.iter().any(|f| inside(f.path))
                } else {
                    inside(p.path)
                };
                if self.drop_place == Some((section, i)) {
                    cv.fill_round_rect(*r, 7, with_alpha(theme::accent(), 0x70));
                } else if active {
                    cv.fill_round_rect(*r, 7, with_alpha(theme::accent(), 0x30));
                } else if self.hover == Some(Hover::Place(section, i)) {
                    cv.fill_round_rect(*r, 7, t.hover);
                }
                icons::draw(cv, p.icon, Rect::new(r.x + 8, r.y + 5, 18, 18));
                let label_w = if p.volume { r.w - 66 } else { r.w - 40 };
                cv.text_clipped(r.x + 34, r.y + 19, p.label, theme::ui(13), t.text, label_w);
                if p.volume {
                    // ⏏: a triangle over a bar.
                    let e = Self::eject_rect(*r);
                    let c = if self.hover == Some(Hover::Place(section, i)) { t.text } else { t.text_secondary };
                    let (cx, top) = (e.x + e.w / 2, e.y + 5);
                    for row in 0..6 {
                        cv.fill_rect(Rect::new(cx - row, top + row, 2 * row + 1, 1), c);
                    }
                    cv.fill_rect(Rect::new(cx - 5, top + 8, 11, 2), c);
                }
            }
        }
    }

    fn draw_toolbar(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let bar = Rect::new(area.x + SIDEBAR_W, area.y, area.w - SIDEBAR_W, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(bar.x, bar.bottom() - 1, bar.w, 1), t.separator);
        let tools = self.toolbar(area);
        let mut title_right = Self::search_rect(area).x - 12;
        for (h, r) in &tools {
            let hovered = self.hover == Some(*h);
            let (cx, cy) = r.center();
            match h {
                Hover::Back | Hover::Forward => {
                    let enabled = if *h == Hover::Back { !self.back.is_empty() } else { !self.forward.is_empty() };
                    if hovered && enabled {
                        cv.fill_round_rect(*r, 7, t.hover);
                    }
                    let c = if enabled { t.text } else { with_alpha(t.text_secondary, 0x80) };
                    let d = if *h == Hover::Back { 1 } else { -1 };
                    cv.line(cx + 3 * d, cy - 6, cx - 3 * d, cy, 2, c);
                    cv.line(cx - 3 * d, cy, cx + 3 * d, cy + 6, 2, c);
                }
                Hover::ViewIcons | Hover::ViewList => {
                    let on = (*h == Hover::ViewIcons) == (self.view == View::Icons);
                    if on {
                        cv.fill_round_rect(*r, 7, with_alpha(theme::accent(), 0x30));
                    } else if hovered {
                        cv.fill_round_rect(*r, 7, t.hover);
                    }
                    let c = if on { theme::accent() } else { t.text };
                    if *h == Hover::ViewIcons {
                        for k in 0..4 {
                            cv.fill_round_rect(Rect::new(cx - 7 + (k % 2) * 8, cy - 7 + (k / 2) * 8, 6, 6), 2, c);
                        }
                    } else {
                        for k in 0..3 {
                            cv.fill_rect(Rect::new(cx - 8, cy - 6 + k * 5, 3, 2), c);
                            cv.fill_rect(Rect::new(cx - 3, cy - 6 + k * 5, 11, 2), c);
                        }
                    }
                    title_right = title_right.min(r.x - 8);
                }
                Hover::NewFolder => {
                    if hovered {
                        cv.fill_round_rect(*r, 7, t.hover);
                    }
                    icons::draw(cv, Icon::Folder, Rect::new(cx - 10, cy - 9, 20, 20));
                    cv.fill_circle(cx + 8, cy + 6, 6, theme::accent());
                    cv.fill_rect(Rect::new(cx + 5, cy + 5, 7, 2), 0xFFFF_FFFF);
                    cv.fill_rect(Rect::new(cx + 7, cy + 3, 2, 7), 0xFFFF_FFFF);
                    title_right = title_right.min(r.x - 8);
                }
                Hover::EmptyTrash => {
                    button(cv, *r, "Empty Trash", ButtonStyle::Secondary, hovered);
                    title_right = title_right.min(r.x - 8);
                }
                _ => {}
            }
        }
        let title = if self.searching() {
            format!("Searching “{}”", self.search_query)
        } else {
            String::from(match self.cwd.as_str() {
                "/" => "Aurora HD",
                "/Boot" => "EFI Boot",
                p => fs::file_name(p),
            })
        };
        let tx = area.x + SIDEBAR_W + 82;
        cv.text_clipped(tx, area.y + 31, &title, theme::ui_bold(15), t.text, title_right - tx);
        let sr = Self::search_rect(area);
        self.search.draw(cv, sr, env.focused && self.search_focused);
        // Magnifier inside the field when empty.
        if self.search.text().is_empty() && !self.search_focused {
            let (mx, my) = (sr.right() - 20, sr.y + 14);
            cv.fill_circle(mx, my, 5, t.text_secondary);
            cv.fill_circle(mx, my, 3, t.control_bg);
            cv.line(mx + 3, my + 3, mx + 7, my + 7, 2, t.text_secondary);
        }
    }

    fn draw_items(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let main = Self::main_area(area);
        let content = self.content(area);
        if self.drop_here {
            cv.stroke_round_rect(main.inset(3), 8, with_alpha(theme::accent(), 0xC0));
        }
        if self.view == View::List {
            let hdr = Rect::new(main.x, main.y, main.w, HEADER_H);
            cv.fill_rect(hdr, t.window_bg);
            cv.fill_rect(Rect::new(hdr.x, hdr.bottom() - 1, hdr.w, 1), t.separator);
            for (key, label, r) in self.columns(area) {
                if self.hover == Some(Hover::Column(key)) {
                    cv.fill_rect(r, t.hover);
                }
                let f = if self.sort.0 == key { theme::ui_bold(12) } else { theme::ui(12) };
                cv.text(r.x + 10, r.y + 18, label, f, t.text_secondary);
                if self.sort.0 == key {
                    let ax = r.x + 16 + f.width(label);
                    let d = if self.sort.1 { -1 } else { 1 };
                    cv.line(ax - 4, r.y + 14 - 2 * d, ax, r.y + 14 + 2 * d, 1, t.text_secondary);
                    cv.line(ax, r.y + 14 + 2 * d, ax + 4, r.y + 14 - 2 * d, 1, t.text_secondary);
                }
                if key != SortKey::Name {
                    cv.fill_rect(Rect::new(r.x, r.y + 6, 1, r.h - 12), t.separator);
                }
            }
        }
        let focused_view = env.focused && !self.search_focused && self.rename.is_none();
        let cols = self.columns(area);
        cv.with_clip(content, |cv| {
            if self.items.is_empty() {
                let (_, cy) = content.center();
                let msg = if self.searching() {
                    if self.search_done() {
                        "No results"
                    } else {
                        "Searching…"
                    }
                } else if self.in_trash() {
                    "The Trash is empty"
                } else {
                    "This folder is empty"
                };
                cv.text_centered(Rect::new(content.x, cy - 12, content.w, 24), msg, theme::ui(14), t.text_secondary);
            }
            let first = match self.view {
                View::Icons => ((self.scroll - 12) / TILE_H).max(0) as usize * self.cols(area) as usize,
                View::List => ((self.scroll - 4) / ROW_H).max(0) as usize,
            };
            for i in first..self.items.len() {
                let r = self.item_rect(area, i);
                if r.y > content.bottom() {
                    break;
                }
                let item = &self.items[i];
                let selected = self.selection.contains(&i);
                let drop = self.drop_target == Some(i);
                let sel_bg = if drop {
                    with_alpha(theme::accent(), 0x70)
                } else if focused_view {
                    with_alpha(theme::accent(), 0x34)
                } else {
                    with_alpha(t.text_secondary, 0x30)
                };
                let icon = icon_of(item);
                let cut = self.cut.contains(&item.path);
                match self.view {
                    View::Icons => {
                        if selected || drop {
                            cv.fill_round_rect(Rect::new(r.x + 18, r.y + 2, r.w - 36, 60), 10, sel_bg);
                        }
                        let ir = Rect::new(r.x + (r.w - 52) / 2, r.y + 6, 52, 52);
                        match self.thumbs.get(&item.path) {
                            Some(Some(img)) => {
                                let (w, h) = aurora_image::fit(img.width, img.height, 52, 52);
                                let tr = Rect::new(
                                    ir.x + (52 - w as i32) / 2,
                                    ir.y + (52 - h as i32) / 2,
                                    w as i32,
                                    h as i32,
                                );
                                cv.fill_rect(tr.inset(-2), 0xFFFF_FFFF);
                                cv.draw_image(&img.pixels, img.width, img.height, tr, 255);
                                cv.stroke_round_rect(tr.inset(-2), 1, 0x30000000);
                            }
                            _ => icons::draw(cv, icon, ir),
                        }
                        if cut {
                            cv.fill_rect(ir, with_alpha(t.window_bg, 0x90));
                        }
                        if !matches!(&self.rename, Some((ri, _)) if *ri == i) {
                            let f = theme::ui(12);
                            let w = f.width(&item.name).min(r.w - 4);
                            let lr = Rect::new(r.x + (r.w - w) / 2 - 6, r.y + 64, w + 12, 20);
                            if selected {
                                cv.fill_round_rect(
                                    lr,
                                    6,
                                    if focused_view { theme::accent() } else { with_alpha(t.text_secondary, 0x60) },
                                );
                            }
                            let fg = if selected && focused_view { 0xFFFF_FFFF } else { t.text };
                            cv.text_clipped(lr.x + 6, r.y + 78, &item.name, f, fg, r.w - 4);
                        }
                    }
                    View::List => {
                        if i % 2 == 1 && !selected {
                            cv.fill_rect(r, with_alpha(t.text_secondary, 0x0C));
                        }
                        if selected || drop {
                            cv.fill_round_rect(r, 6, if focused_view && !drop { theme::accent() } else { sel_bg });
                        }
                        let fg = if selected && focused_view && !drop { 0xFFFF_FFFF } else { t.text };
                        let fg2 = if selected && focused_view && !drop { 0xDDFF_FFFF } else { t.text_secondary };
                        icons::draw(cv, icon, Rect::new(r.x + 8, r.y + 4, 18, 18));
                        let f = theme::ui(13);
                        let name_col = cols[0].2;
                        if !matches!(&self.rename, Some((ri, _)) if *ri == i) {
                            let label = if self.searching() {
                                format!("{}  —  {}", item.name, fs::parent(&item.path))
                            } else {
                                item.name.clone()
                            };
                            cv.text_clipped(r.x + 34, r.y + 18, &label, f, fg, name_col.w - 44);
                        }
                        let small = theme::ui(12);
                        cv.text_clipped(
                            cols[1].2.x + 10,
                            r.y + 18,
                            &human_date(item.mtime),
                            small,
                            fg2,
                            cols[1].2.w - 14,
                        );
                        let size = if item.is_dir { String::from("—") } else { human_size(item.size) };
                        cv.text_clipped(cols[2].2.x + 10, r.y + 18, &size, small, fg2, cols[2].2.w - 14);
                        cv.text_clipped(cols[3].2.x + 10, r.y + 18, kind_of(item), small, fg2, cols[3].2.w - 14);
                    }
                }
            }
            if let Some((x0, y0, x1, y1)) = self.band {
                let r = Rect::new(x0.min(x1), y0.min(y1) - self.scroll, (x1 - x0).abs(), (y1 - y0).abs());
                cv.fill_rect(r, with_alpha(theme::accent(), 0x28));
                cv.stroke_round_rect(r, 1, with_alpha(theme::accent(), 0x90));
            }
        });
        // Inline rename field (drawn on top, may overflow the tile).
        if let Some(i) = self.rename.as_ref().map(|(i, _)| *i) {
            let r = self.item_rect(area, i);
            let fr = match self.view {
                View::Icons => Rect::new(r.x - 8, r.y + 62, r.w + 16, 28),
                View::List => Rect::new(r.x + 28, r.y - 1, cols[0].2.w - 32, ROW_H + 2),
            };
            if let Some((_, field)) = &mut self.rename {
                if content.contains(fr.x + 1, fr.y + 1) {
                    cv.with_clip(content, |cv| field.draw(cv, fr, env.focused));
                }
            }
        }
        // Scroll indicator.
        let total = self.content_height(area);
        if total > content.h {
            let h = (content.h * content.h / total).max(24);
            let y = content.y + (content.h - h) * self.scroll / (total - content.h).max(1);
            cv.fill_round_rect(Rect::new(content.right() - 7, y, 5, h), 3, with_alpha(t.text_secondary, 0x70));
        }
    }

    fn draw_status(&self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let bar = Rect::new(area.x + SIDEBAR_W, area.bottom() - STATUS_H, area.w - SIDEBAR_W, STATUS_H);
        cv.fill_rect(Rect::new(bar.x, bar.y, bar.w, 1), t.separator);
        let text = match &self.status {
            Some((msg, _)) => msg.clone(),
            None => {
                let n = self.items.len();
                let mut s = format!("{n} item{}", if n == 1 { "" } else { "s" });
                if !self.selection.is_empty() {
                    let bytes: u64 = self.selection.iter().filter_map(|&i| self.items.get(i)).map(|e| e.size).sum();
                    s = format!("{} of {s} selected", self.selection.len());
                    if bytes > 0 {
                        s.push_str(&format!(" ({})", human_size(bytes)));
                    }
                }
                if self.searching() {
                    s.push_str(if self.search_done() { " · search finished" } else { " · searching…" });
                } else if self.cwd.starts_with("/System") {
                    s.push_str(" · System image (read-only)");
                } else if self.cwd.starts_with("/Boot") {
                    s.push_str(" · EFI System Partition");
                } else {
                    let i = aurora::process::sys_info();
                    if i.disk_total > 0 {
                        s.push_str(&format!(" · {} MB available", i.disk_free >> 20));
                    }
                }
                s
            }
        };
        cv.text_clipped(bar.x + 14, bar.y + 18, &text, theme::ui(12), t.text_secondary, bar.w - 28);
    }

    fn draw_overlays(&mut self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let main = Self::main_area(area);
        if let Some(c) = &self.confirm {
            cv.fill_rect(area, with_alpha(0x000000, 0x30));
            let (d, ok, cancel) = Self::confirm_rects(area);
            cv.shadow(d, 12, 20, 6, t.shadow);
            cv.fill_round_rect(d, 12, t.window_bg);
            cv.stroke_round_rect(d, 12, t.window_border);
            let (title, what, verb) = match c {
                Confirm::DeleteForever(p) if p.len() == 1 => {
                    (format!("Delete “{}” immediately?", fs::file_name(&p[0])), "You can't undo this action.", "Delete")
                }
                Confirm::DeleteForever(p) => {
                    (format!("Delete {} items immediately?", p.len()), "You can't undo this action.", "Delete")
                }
                Confirm::EmptyTrash => (
                    String::from("Empty the Trash?"),
                    "Everything in the Trash will be deleted permanently.",
                    "Empty Trash",
                ),
            };
            cv.text_clipped(d.x + 20, d.y + 36, &title, theme::ui_bold(15), t.text, d.w - 40);
            cv.text(d.x + 20, d.y + 60, what, theme::ui(13), t.text_secondary);
            button(cv, ok, verb, ButtonStyle::Danger, false);
            button(cv, cancel, "Cancel", ButtonStyle::Secondary, false);
        }
        if let Some(q) = &self.quicklook {
            let Some(item) = self.items.get(q.index) else { return };
            cv.fill_rect(main, with_alpha(0x000000, 0x40));
            let (w, h) = match (&q.image, &q.text) {
                (Some(img), _) => {
                    let (w, h) = aurora_image::fit(img.width, img.height, (main.w - 80) as u32, (main.h - 110) as u32);
                    ((w as i32 + 32).max(300), h as i32 + 76)
                }
                (None, Some(_)) => ((main.w - 80).min(560), (main.h - 60).min(440)),
                _ => (320, 220),
            };
            let card = Rect::new(main.x + (main.w - w) / 2, main.y + (main.h - h) / 2, w, h);
            cv.shadow(card, 14, 30, 10, t.shadow);
            cv.fill_round_rect(card, 14, t.window_bg);
            cv.stroke_round_rect(card, 14, t.window_border);
            let head = format!(
                "{}  ·  {}",
                item.name,
                if item.is_dir { String::from("Folder") } else { human_size(item.size) }
            );
            cv.text_clipped(card.x + 16, card.y + 28, &head, theme::ui_bold(13), t.text, card.w - 32);
            let body = Rect::new(card.x + 16, card.y + 44, card.w - 32, card.h - 60);
            match (&q.image, &q.text) {
                (Some(img), _) => {
                    let (iw, ih) = aurora_image::fit(img.width, img.height, body.w as u32, body.h as u32);
                    let r = Rect::new(body.x + (body.w - iw as i32) / 2, body.y, iw as i32, ih as i32);
                    if !img.is_opaque() {
                        cv.checkerboard(r, 8, 0xFFFF_FFFF, 0xFFDD_DDE2);
                    }
                    cv.draw_image(&img.pixels, img.width, img.height, r, 255);
                }
                (None, Some(text)) => {
                    cv.fill_round_rect(body, 8, t.window_bg_alt);
                    let f = theme::mono(12);
                    cv.with_clip(body.inset(10), |cv| {
                        for (k, line) in text.lines().enumerate() {
                            cv.text(body.x + 12, body.y + 22 + k as i32 * 17, line, f, t.text);
                        }
                    });
                }
                _ => {
                    icons::draw(cv, icon_of(item), Rect::new(card.x + (card.w - 72) / 2, body.y + 20, 72, 72));
                    cv.text_centered(
                        Rect::new(card.x, body.y + 110, card.w, 20),
                        kind_of(item),
                        theme::ui(13),
                        t.text_secondary,
                    );
                }
            }
        }
        if let Some(m) = &self.menu {
            let r = m.rect();
            cv.shadow(r, 10, 18, 6, t.shadow * 2 / 3);
            cv.fill_round_rect(r, 10, t.window_bg);
            cv.stroke_round_rect(r, 10, t.window_border);
            for (i, (it, row)) in m.items.iter().zip(m.rows()).enumerate() {
                let Some((_, label)) = it else {
                    cv.fill_rect(Rect::new(row.x + 6, row.y + 4, row.w - 12, 1), t.separator);
                    continue;
                };
                let hovered = m.hover == Some(i);
                if hovered {
                    cv.fill_round_rect(row, 6, theme::accent());
                }
                cv.text(row.x + 12, row.y + 19, label, theme::ui(13), if hovered { 0xFFFF_FFFF } else { t.text });
            }
        }
    }

    // ---------------------------------------------------------- selection

    fn band_select(&mut self, area: Rect) {
        let Some((x0, y0, x1, y1)) = self.band else { return };
        let band = Rect::new(x0.min(x1), y0.min(y1) - self.scroll, (x1 - x0).abs().max(1), (y1 - y0).abs().max(1));
        let mut sel = self.band_base.clone();
        for i in 0..self.items.len() {
            let r = self.item_rect(area, i);
            let r = if self.view == View::Icons { Rect::new(r.x + 18, r.y + 2, r.w - 36, r.h - 4) } else { r };
            if r.intersects(&band) {
                sel.insert(i);
            }
        }
        self.selection = sel;
    }

    fn move_cursor(&mut self, delta: i32, extend: bool) {
        let n = self.items.len() as i32;
        if n == 0 {
            return;
        }
        let from = self.cursor.map(|c| c as i32).unwrap_or(if delta > 0 { -1 } else { n });
        let to = (from + delta).clamp(0, n - 1) as usize;
        if extend {
            let a = self.anchor.unwrap_or(to);
            self.selection = (a.min(to)..=a.max(to)).collect();
        } else {
            self.selection.clear();
            self.selection.insert(to);
            self.anchor = Some(to);
        }
        self.cursor = Some(to);
        self.reveal(to);
        if let Some(q) = &self.quicklook {
            if q.index != to {
                self.quicklook = Some(self.load_quicklook(to));
            }
        }
    }

    /// Makes the next thumbnail (one per tick keeps the window responsive).
    fn make_thumbnail(&mut self) -> bool {
        if self.view != View::Icons {
            return false;
        }
        let area = self.area;
        let c = self.content(area);
        let next = self.items.iter().enumerate().find(|(i, e)| {
            let r = self.item_rect(area, *i);
            !e.is_dir
                && is_image(&e.name)
                && e.size < 12 << 20
                && r.bottom() >= c.y
                && r.y <= c.bottom()
                && !self.thumbs.contains_key(&e.path)
        });
        let Some((_, e)) = next else { return false };
        let path = e.path.clone();
        let thumb = fs::read(&path).ok().and_then(|d| aurora_image::decode(&d).ok()).map(|img| img.thumbnail(104, 104));
        self.thumbs.insert(path, thumb);
        true
    }
}

fn split_ext(name: &str, is_dir: bool) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (String::from(&name[..i]), String::from(&name[i..])),
        _ => (String::from(name), String::new()),
    }
}

impl App for Files {
    fn title(&self) -> String {
        let name = if self.searching() {
            "Search"
        } else {
            match self.cwd.as_str() {
                "/" => "Aurora HD",
                "/Boot" => "EFI Boot",
                p => fs::file_name(p),
            }
        };
        format!("{name} — Files")
    }

    fn size(&self) -> (i32, i32) {
        (880, 540)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        self.clamp_scroll();
        self.draw_sidebar(cv, area);
        self.draw_items(cv, area, env);
        self.draw_toolbar(cv, area, env);
        self.draw_status(cv, area);
        self.draw_overlays(cv, area);
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(m) = self.menu.take() {
            if let Some(i) = m.item_at(x, y) {
                if let Some((action, _)) = m.items[i] {
                    self.run(action, env);
                }
            }
            return true;
        }
        if self.confirm.is_some() {
            let (_, ok, cancel) = Self::confirm_rects(area);
            if ok.contains(x, y) {
                self.confirmed();
            } else if cancel.contains(x, y) {
                self.confirm = None;
            }
            return true;
        }
        if self.quicklook.is_some() {
            self.quicklook = None;
            return true;
        }
        if let Some((_, field)) = &mut self.rename {
            if field.click(x, y, env.mods.shift, env.now_ms) {
                return true;
            }
            self.finish_rename(true);
        }
        // Search field.
        if self.search.click(x, y, env.mods.shift, env.now_ms) {
            self.search_focused = true;
            return true;
        }
        self.search_focused = false;
        if let Some((s, i)) = self.place_at(area, x, y) {
            let p = self.place(s, i);
            let (path, volume) = (String::from(p.path), p.volume);
            let row = self.place_rects(area).1[i];
            if volume && Self::eject_rect(row).contains(x, y) {
                let name = String::from(p.label);
                match aurora::process::desktop::eject(&path) {
                    Ok(()) => self.flash(format!("“{name}” can now be removed.")),
                    Err(e) => self.flash(format!("Couldn't eject “{name}”: {e}")),
                }
                self.refresh_volumes();
                return true;
            }
            self.navigate(&path, true);
            return true;
        }
        if let Some((h, _)) = self.toolbar(area).into_iter().find(|(_, r)| r.contains(x, y)) {
            match h {
                Hover::Back => self.go_back(),
                Hover::Forward => self.go_forward(),
                Hover::ViewIcons => self.run(Action::ViewIcons, env),
                Hover::ViewList => self.run(Action::ViewList, env),
                Hover::NewFolder => self.run(Action::NewFolder, env),
                Hover::EmptyTrash => self.run(Action::EmptyTrash, env),
                _ => {}
            }
            return true;
        }
        if self.view == View::List {
            if let Some((key, _, _)) = self.columns(area).into_iter().find(|(_, _, r)| r.contains(x, y)) {
                self.set_sort(key);
                return true;
            }
        }
        if !self.content(area).contains(x, y) {
            return true;
        }
        let hit = self.item_at(area, x, y);
        let (ctrl, shift) = (env.mods.ctrl || env.mods.super_key, env.mods.shift);
        match hit {
            Some(i) if shift => {
                let a = self.anchor.unwrap_or(i);
                self.selection = (a.min(i)..=a.max(i)).collect();
                self.cursor = Some(i);
            }
            Some(i) if ctrl => {
                if !self.selection.remove(&i) {
                    self.selection.insert(i);
                }
                self.anchor = Some(i);
                self.cursor = Some(i);
            }
            Some(i) => {
                let was = self.selection.contains(&i);
                if !was {
                    self.select_only(i);
                }
                self.cursor = Some(i);
                self.press = Some(Press { x, y, item: Some(i), was_selected: was, dragging: false });
            }
            None => {
                if !ctrl && !shift {
                    self.selection.clear();
                    self.cursor = None;
                }
                self.band_base = self.selection.clone();
                self.press = Some(Press { x, y, item: None, was_selected: false, dragging: false });
            }
        }
        true
    }

    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if self.menu.is_some() || self.confirm.is_some() || self.quicklook.is_some() {
            return self.click(x, y, area, env);
        }
        if let Some((_, field)) = &mut self.rename {
            if field.double_click(x, y, env.now_ms) {
                return true;
            }
        }
        if self.search.double_click(x, y, env.now_ms) {
            self.search_focused = true;
            return true;
        }
        if let Some(i) = self.item_at(area, x, y) {
            self.select_only(i);
            self.open(i, env);
            return true;
        }
        self.click(x, y, area, env)
    }

    fn drag(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        if let Some((_, field)) = &mut self.rename {
            if field.drag(x) {
                return true;
            }
        }
        if self.search_focused && self.search.drag(x) {
            return true;
        }
        let Some(p) = &self.press else { return false };
        if p.dragging {
            return false;
        }
        if (x - p.x).abs() < 5 && (y - p.y).abs() < 5 {
            return false;
        }
        let (px, py, item) = (p.x, p.y, p.item);
        match item {
            Some(_) => {
                // Hand the selected items to the window server as a drag.
                if let Some(p) = &mut self.press {
                    p.dragging = true;
                }
                let paths = self.selected_paths();
                let icon = if paths.len() == 1 && fs::is_dir(&paths[0]) {
                    drag::FOLDER
                } else if paths.iter().all(|p| is_image(p)) {
                    drag::PICTURE
                } else {
                    drag::DOCUMENT
                };
                aurora::dnd::start_files(&paths, icon);
                false
            }
            None => {
                let c = self.content(area);
                let (sx, sy) = (px - c.x, py + self.scroll - c.y);
                let (cx, cy) = (x.clamp(c.x, c.right()) - c.x, y.clamp(c.y - 40, c.bottom() + 40) + self.scroll - c.y);
                self.band = Some((sx + c.x, sy + c.y, cx + c.x, cy + c.y));
                // Auto-scroll at the edges.
                if y > c.bottom() {
                    self.scroll += 12;
                } else if y < c.y {
                    self.scroll -= 12;
                }
                self.clamp_scroll();
                self.band_select(area);
                true
            }
        }
    }

    fn release(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        self.search.release();
        if let Some((_, f)) = &mut self.rename {
            f.release();
        }
        let redraw = self.band.take().is_some();
        if let Some(p) = self.press.take() {
            // A plain click on an item of a multi-selection narrows it to that item.
            if let (Some(i), true, false) = (p.item, p.was_selected, p.dragging) {
                self.select_only(i);
                return true;
            }
        }
        redraw
    }

    fn right_click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        if self.confirm.is_some() || self.quicklook.is_some() {
            return false;
        }
        self.finish_rename(true);
        if !self.content(area).contains(x, y) {
            return false;
        }
        let hit = self.item_at(area, x, y);
        if let Some(i) = hit {
            if !self.selection.contains(&i) {
                self.select_only(i);
            }
        } else {
            self.selection.clear();
        }
        self.open_menu(x, y, hit.is_some());
        true
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        if let Some(m) = &mut self.menu {
            let h = m.item_at(x, y);
            return core::mem::replace(&mut m.hover, h) != h;
        }
        let h = if x < 0 {
            None
        } else if let Some((s, i)) = self.place_at(area, x, y) {
            Some(Hover::Place(s, i))
        } else if let Some((h, _)) = self.toolbar(area).into_iter().find(|(_, r)| r.contains(x, y)) {
            Some(h)
        } else if self.view == View::List {
            self.columns(area).into_iter().find(|(_, _, r)| r.contains(x, y)).map(|(k, _, _)| Hover::Column(k))
        } else {
            None
        };
        core::mem::replace(&mut self.hover, h) != h
    }

    fn scroll(&mut self, delta: i32, _area: Rect) -> bool {
        if self.quicklook.is_some() || self.confirm.is_some() {
            return false;
        }
        let before = self.scroll;
        self.scroll += delta * 40;
        self.clamp_scroll();
        self.scroll != before
    }

    fn drag_over(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        let before = (self.drop_target, self.drop_place, self.drop_here);
        self.drop_target = None;
        self.drop_place = None;
        self.drop_here = false;
        if x >= 0 {
            if let Some((s, i)) = self.place_at(area, x, y) {
                if !read_only(self.place(s, i).path) {
                    self.drop_place = Some((s, i));
                }
            } else if self.content(area).contains(x, y) {
                match self.item_at(area, x, y) {
                    Some(i)
                        if self.items[i].is_dir && !self.selection.contains(&i) && !read_only(&self.items[i].path) =>
                    {
                        self.drop_target = Some(i)
                    }
                    _ => self.drop_here = self.writable_here(),
                }
            }
        }
        before != (self.drop_target, self.drop_place, self.drop_here)
    }

    fn drop(&mut self, x: i32, y: i32, kind: u32, area: Rect, env: &mut Env) -> bool {
        let target = if let Some((s, i)) = self.drop_place {
            Some(String::from(self.place(s, i).path))
        } else if let Some(i) = self.drop_target {
            self.items.get(i).map(|e| e.path.clone())
        } else if self.drop_here {
            Some(self.cwd.clone())
        } else {
            None
        };
        self.drag_over(-1, -1, area, env);
        let _ = (x, y);
        let Some(dir) = target else { return true };
        match aurora::dnd::dropped(kind) {
            Some(aurora::dnd::Dropped::Files(paths)) => {
                if dir == TRASH {
                    self.trash(paths);
                    return true;
                }
                // Move within a volume; copy across volumes or with Ctrl held.
                let same_volume = |p: &str| {
                    let vol = |s: &str| {
                        if s.starts_with("/Boot") {
                            1
                        } else if s.starts_with("/System") {
                            2
                        } else {
                            0
                        }
                    };
                    vol(p) == vol(&dir)
                };
                let copy = env.mods.ctrl || paths.iter().any(|p| !same_volume(p));
                let n = self.transfer(&paths, &dir, !copy);
                if n > 0 {
                    let verb = if copy { "Copied" } else { "Moved" };
                    self.flash(format!("{verb} {n} item{} to {}", if n == 1 { "" } else { "s" }, fs::file_name(&dir)));
                }
                self.refresh();
            }
            Some(aurora::dnd::Dropped::Text(text)) => {
                // Dropped text becomes a new text file.
                let path = fs::unique_name(&dir, "Dropped Text", ".txt");
                if fs::write(&path, text.as_bytes()).is_ok() {
                    self.flash(format!("Saved “{}”", fs::file_name(&path)));
                }
                self.refresh();
            }
            None => {}
        }
        true
    }

    fn drag_end(&mut self, _dropped: bool, _env: &mut Env) -> bool {
        self.press = None;
        if self.searching() {
            self.start_search();
        } else {
            self.refresh();
        }
        true
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        let lower = ev.ch.map(|c| c.to_ascii_lowercase());

        if self.confirm.is_some() {
            match ev.code {
                KeyCode::Enter => self.confirmed(),
                KeyCode::Escape => self.confirm = None,
                _ => return false,
            }
            return true;
        }
        if let Some(m) = &self.menu {
            if ev.code == KeyCode::Escape {
                self.menu = None;
                return true;
            }
            let _ = m;
        }
        if let Some((_, field)) = &mut self.rename {
            match ev.code {
                KeyCode::Enter => self.finish_rename(true),
                KeyCode::Escape => self.finish_rename(false),
                _ => {
                    field.key(ev, env.now_ms);
                }
            }
            return true;
        }
        if self.search_focused {
            match ev.code {
                KeyCode::Escape => {
                    self.search.edit.set_text("");
                    self.search_focused = false;
                    self.start_search();
                }
                KeyCode::Enter | KeyCode::Down => {
                    self.search_focused = false;
                    if !self.items.is_empty() {
                        self.move_cursor(1, false);
                    }
                }
                _ => {
                    let r = self.search.key(ev, env.now_ms);
                    if r.edited {
                        self.start_search();
                    }
                }
            }
            return true;
        }
        if self.quicklook.is_some() && matches!(ev.code, KeyCode::Escape) {
            self.quicklook = None;
            return true;
        }

        if ctrl {
            let action = match (lower, ev.mods.shift) {
                (Some('a'), _) => {
                    self.selection = (0..self.items.len()).collect();
                    return true;
                }
                (Some('c'), _) => Action::Copy,
                (Some('x'), _) => Action::Cut,
                (Some('v'), _) => Action::Paste,
                (Some('d'), _) => Action::Duplicate,
                (Some('n'), true) => Action::NewFolder,
                (Some('o'), _) => Action::Open,
                (Some('f'), _) => {
                    self.search_focused = true;
                    self.search.edit.select_all();
                    return true;
                }
                (Some('1'), _) => Action::ViewIcons,
                (Some('2'), _) => Action::ViewList,
                _ if ev.code == KeyCode::Backspace => Action::Trash,
                _ => return false,
            };
            self.run(action, env);
            return true;
        }
        if ev.mods.alt {
            match ev.code {
                KeyCode::Left => self.go_back(),
                KeyCode::Right => self.go_forward(),
                KeyCode::Up => self.go_up(),
                _ => return false,
            }
            return true;
        }
        let cols = self.cols(self.area);
        let shift = ev.mods.shift;
        match ev.code {
            KeyCode::Right if self.view == View::Icons => self.move_cursor(1, shift),
            KeyCode::Left if self.view == View::Icons => self.move_cursor(-1, shift),
            KeyCode::Down => self.move_cursor(cols, shift),
            KeyCode::Up => self.move_cursor(-cols, shift),
            KeyCode::Home => self.move_cursor(-(self.items.len() as i32), shift),
            KeyCode::End => self.move_cursor(self.items.len() as i32, shift),
            KeyCode::Enter => self.run(Action::Open, env),
            KeyCode::F2 => self.run(Action::Rename, env),
            KeyCode::Delete => {
                let action = if self.in_trash() { Action::DeleteForever } else { Action::Trash };
                self.run(action, env)
            }
            KeyCode::Backspace => self.go_back(),
            KeyCode::Escape => {
                if self.searching() {
                    self.search.edit.set_text("");
                    self.start_search();
                } else {
                    self.selection.clear();
                }
            }
            _ => match ev.ch {
                Some(' ') => self.toggle_quicklook(),
                // Type to select: jump to the first name starting with what was typed.
                Some(c) if !c.is_control() => {
                    if env.now_ms - self.typed.1 > 900 {
                        self.typed.0.clear();
                    }
                    self.typed.0.extend(c.to_lowercase());
                    self.typed.1 = env.now_ms;
                    let prefix = self.typed.0.clone();
                    if let Some(i) = self.items.iter().position(|e| e.name.to_lowercase().starts_with(&prefix)) {
                        self.select_only(i);
                        self.reveal(i);
                    }
                }
                _ => return false,
            },
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
        if let Some((_, f)) = &mut self.rename {
            changed |= f.tick(env.now_ms, env.focused);
        }
        if self.search_focused {
            changed |= self.search.tick(env.now_ms, env.focused);
        }
        changed |= self.poll_search();
        changed |= self.make_thumbnail();
        // Pick up changes made elsewhere (other windows, Terminal).
        let busy = self.rename.is_some() || self.press.is_some() || self.menu.is_some();
        if !busy && env.now_ms - self.last_refresh > 2000 && !self.searching() {
            changed |= self.refresh_volumes();
            let before: Vec<(String, u64, u64)> =
                self.items.iter().map(|e| (e.name.clone(), e.size, e.mtime)).collect();
            self.refresh();
            let after: Vec<(String, u64, u64)> = self.items.iter().map(|e| (e.name.clone(), e.size, e.mtime)).collect();
            changed |= before != after;
        }
        changed
    }
}

aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let start = args.get(1).map(|p| fs::resolve(&fs::cwd(), p)).unwrap_or_else(|| String::from("/Documents"));
    ripple::run(Files::new(&start))
}
