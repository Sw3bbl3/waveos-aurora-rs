//! Files — browse and manage the filesystem.
//!
//! Icon and list views (sortable columns), multiple selection (Ctrl/Shift
//! click, keyboard), drag and drop (between windows, onto folders and
//! sidebar places, onto Harbor), search as you type, Quick Look (Space),
//! picture thumbnails, inline rename, the system clipboard, tabs, and a
//! Trash with Put Back.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_image::Image as Picture;
use core::sync::atomic::{AtomicBool, Ordering};
use corekit::fs;
use corekit::process::desktop;
use corekit::sync::Mutex;
use waveui::lumen::icons::Icon;
use waveui::Request;

mod transfer;
mod window;
use window::Files;

type SearchWorker = Arc<Mutex<Option<corekit::thread::JoinHandle<()>>>>;

/// The narrowest a grid card gets, and every card's height (picture, name
/// and padding).
pub const TILE_W: i32 = 180;
pub const TILE_H: i32 = 148;
pub const ROW_H: i32 = 32;
const TRASH: &str = "/Trash";

pub struct Place {
    pub label: &'static str,
    pub path: &'static str,
    pub symbol: waveui::Symbol,
}

pub const FAVORITES: [Place; 4] = [
    Place { label: "Desktop", path: "/Desktop", symbol: waveui::Symbol::Display },
    Place { label: "Documents", path: "/Documents", symbol: waveui::Symbol::Document },
    Place { label: "Downloads", path: "/Downloads", symbol: waveui::Symbol::Download },
    Place { label: "Pictures", path: "/Pictures", symbol: waveui::Symbol::Photo },
];

pub const LOCATIONS: [Place; 4] = [
    Place { label: "Aurora HD", path: "/", symbol: waveui::Symbol::Drive },
    Place { label: "System", path: "/System", symbol: waveui::Symbol::Gear },
    Place { label: "EFI Boot", path: "/Boot", symbol: waveui::Symbol::Lock },
    Place { label: "Trash", path: TRASH, symbol: waveui::Symbol::Trash },
];

const VOLUMES: &str = "/Volumes";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Icons,
    List,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortKey {
    Name,
    Date,
    Size,
    Kind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
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

#[derive(Clone)]
pub struct Item {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
}

pub enum Confirm {
    DeleteForever(Vec<String>),
    EmptyTrash,
}

/// Shared with the background search thread.
struct SearchState {
    results: Vec<Item>,
    done: bool,
    cancel: AtomicBool,
}

pub struct QuickLook {
    pub index: usize,
    pub image: Option<Picture>,
    pub text: Option<String>,
}

/// One tab: a folder (or search results) and what is selected in it.
pub struct Folder {
    pub cwd: String,
    pub items: Vec<Item>,
    pub selection: BTreeSet<usize>,
    anchor: Option<usize>,
    pub cursor: Option<usize>,
    back: Vec<String>,
    forward: Vec<String>,
    pub view: ViewMode,
    pub sort: (SortKey, bool),
    /// What is typed in the search field.
    pub query: String,
    search_state: Option<Arc<Mutex<SearchState>>>,
    /// The query the results are for.
    search_query: String,
    /// The item being renamed and its new name.
    pub rename: Option<(usize, String)>,
    pub confirm: Option<Confirm>,
    cut: Arc<Mutex<Vec<String>>>,
    transfers: transfer::Shared,
    worker: SearchWorker,
    search_at: Option<u64>,
    resume_search: bool,
    pub status: Option<(String, u64)>,
    pub quicklook: Option<QuickLook>,
    pub thumbs: BTreeMap<String, Option<Picture>>,
    typed: (String, u64),
    last_refresh: u64,
    /// Mounted volumes under /Volumes: (name, path).
    pub volumes: Vec<(String, String)>,
    /// Icon-view columns at the last layout (keyboard moves by rows).
    pub cols: core::cell::Cell<i32>,
    /// Asks the view to scroll an item into sight.
    pub reveal: Option<usize>,
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
    format!("{} {}, {}, {}:{:02} {}", corekit::time::MONTHS[(m - 1) as usize], d, y, h12, min, ampm)
}

fn same_volume(a: &str, b: &str) -> bool {
    let vol = |s: &str| {
        if s.starts_with("/Boot") {
            1
        } else if s.starts_with("/System") {
            2
        } else {
            0
        }
    };
    vol(a) == vol(b)
}

impl Folder {
    pub fn new(start: &str, transfers: transfer::Shared, worker: SearchWorker, cut: Arc<Mutex<Vec<String>>>) -> Self {
        let mut f = Self {
            cwd: String::new(),
            items: Vec::new(),
            selection: BTreeSet::new(),
            anchor: None,
            cursor: None,
            back: Vec::new(),
            forward: Vec::new(),
            view: ViewMode::Icons,
            sort: (SortKey::Name, true),
            query: String::new(),
            search_state: None,
            search_query: String::new(),
            rename: None,
            confirm: None,
            cut,
            transfers,
            worker,
            search_at: None,
            resume_search: false,
            status: None,
            quicklook: None,
            thumbs: BTreeMap::new(),
            typed: (String::new(), 0),
            last_refresh: 0,
            volumes: Vec::new(),
            cols: core::cell::Cell::new(5),
            reveal: None,
        };
        f.refresh_volumes();
        f.navigate(start, false);
        f
    }

    pub fn title(&self) -> String {
        if self.searching() {
            format!("Searching “{}”", self.search_query)
        } else if self.cwd == "/" {
            String::from("Aurora HD")
        } else {
            String::from(fs::file_name(&self.cwd))
        }
    }

    // ---------------------------------------------------------- navigation

    pub fn searching(&self) -> bool {
        !self.search_query.is_empty()
    }

    pub fn can_go_back(&self) -> bool {
        !self.back.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.forward.is_empty()
    }

    pub fn navigate(&mut self, dir: &str, push: bool) {
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
        self.query.clear();
        self.search_query.clear();
        self.selection.clear();
        self.anchor = None;
        self.cursor = None;
        self.rename = None;
        self.quicklook = None;
        self.refresh();
    }

    pub fn go_back(&mut self) {
        if let Some(prev) = self.back.pop() {
            self.forward.push(self.cwd.clone());
            self.navigate(&prev, false);
        }
    }

    pub fn go_forward(&mut self) {
        if let Some(next) = self.forward.pop() {
            self.back.push(self.cwd.clone());
            self.navigate(&next, false);
        }
    }

    pub fn go_up(&mut self) {
        if self.cwd != "/" {
            let parent = String::from(fs::parent(&self.cwd));
            self.navigate(&parent, true);
        }
    }

    /// Re-reads the folder, keeping the selection (by path).
    pub fn refresh(&mut self) {
        if self.searching() {
            return;
        }
        let selected = self.selected_paths();
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
        self.last_refresh = corekit::time::uptime_ms();
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

    pub fn set_sort(&mut self, key: SortKey) {
        let selected: Vec<String> = self.selected_paths();
        self.sort = if self.sort.0 == key { (key, !self.sort.1) } else { (key, true) };
        self.sort_items();
        self.selection =
            self.items.iter().enumerate().filter(|(_, e)| selected.contains(&e.path)).map(|(i, _)| i).collect();
    }

    pub fn flash(&mut self, msg: String) {
        self.status = Some((msg, corekit::time::uptime_ms()));
    }

    pub fn selected_paths(&self) -> Vec<String> {
        self.selection.iter().filter_map(|&i| self.items.get(i)).map(|e| e.path.clone()).collect()
    }

    fn single(&self) -> Option<usize> {
        (self.selection.len() == 1).then(|| *self.selection.iter().next().unwrap())
    }

    pub fn in_trash(&self) -> bool {
        self.cwd == TRASH && !self.searching()
    }

    pub fn writable_here(&self) -> bool {
        !self.searching() && !read_only(&self.cwd)
    }

    pub fn select_only(&mut self, i: usize) {
        self.selection.clear();
        self.selection.insert(i);
        self.anchor = Some(i);
        self.cursor = Some(i);
    }

    /// A click on item `i`: Ctrl adds or removes it, Shift extends from the anchor.
    pub fn click(&mut self, i: usize, ctrl: bool, shift: bool) {
        if self.rename.as_ref().is_some_and(|(r, _)| *r != i) {
            self.finish_rename(true);
        }
        if ctrl {
            if !self.selection.remove(&i) {
                self.selection.insert(i);
            }
            self.anchor = Some(i);
            self.cursor = Some(i);
        } else if shift {
            let a = self.anchor.unwrap_or(i);
            self.selection = (a.min(i)..=a.max(i)).collect();
            self.cursor = Some(i);
        } else {
            self.select_only(i);
        }
        if let Some(q) = &self.quicklook {
            if q.index != i {
                self.quicklook = Some(self.load_quicklook(i));
            }
        }
    }

    /// A right click on item `i` selects it unless it is part of the selection.
    pub fn target(&mut self, i: usize) {
        if !self.selection.contains(&i) {
            self.select_only(i);
        }
    }

    // ------------------------------------------------------------ search

    pub fn search_at_pending(&self) -> bool {
        self.search_at.is_some()
    }

    pub fn set_resume(&mut self, resume: bool) {
        self.resume_search = resume;
    }

    pub fn take_resume(&mut self) -> bool {
        core::mem::take(&mut self.resume_search)
    }

    pub fn stop_search(&mut self) {
        self.search_at = None;
        if let Some(s) = self.search_state.take() {
            s.lock().cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn start_search(&mut self) {
        self.stop_search();
        self.search_at = Some(corekit::time::uptime_ms() + 200);
    }

    fn launch_search(&mut self) {
        self.search_at = None;
        let q = String::from(self.query.trim());
        self.search_query = q.clone();
        self.selection.clear();
        self.cursor = None;
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
        let job = corekit::thread::spawn(move || {
            let mut stack = alloc::vec![root];
            let mut found = 0usize;
            while let Some(dir) = stack.pop() {
                if state.lock().cancel.load(Ordering::Relaxed) {
                    state.lock().done = true;
                    return;
                }
                let mut batch = Vec::new();
                for e in fs::read_dir(&dir).unwrap_or_default() {
                    if found + batch.len() >= 2000 {
                        break;
                    }
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
                if found >= 2000 {
                    break;
                }
            }
            state.lock().done = true;
        });
        match job {
            Ok(h) => *self.worker.lock() = Some(h),
            Err(_) => {
                self.search_state = None;
                self.flash(String::from("Couldn't start the search"));
            }
        }
    }

    /// Pulls new results from the search thread; true if anything changed.
    fn poll_search(&mut self) -> bool {
        let Some(state) = &self.search_state else { return false };
        let mut s = state.lock();
        if s.results.is_empty() {
            return false;
        }
        self.items.append(&mut s.results);
        drop(s);
        self.sort_items();
        true
    }

    pub fn search_done(&self) -> bool {
        self.search_state.as_ref().is_none_or(|s| s.lock().done)
    }

    // ---------------------------------------------------------- volumes

    /// Re-reads /Volumes; leaves a volume that has gone away.
    pub fn refresh_volumes(&mut self) -> bool {
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

    // ------------------------------------------------------------ actions

    pub fn run(&mut self, action: Action, out: &mut Vec<Request>) {
        let paths = self.selected_paths();
        match action {
            Action::Open => {
                let sel: Vec<usize> = self.selection.iter().copied().collect();
                for i in sel.into_iter().take(8) {
                    self.open(i, out);
                }
            }
            Action::OpenWith(app) => {
                for p in paths.into_iter().take(8) {
                    let _ =
                        corekit::process::spawn(&format!("/System/Apps/{app}.elf"), &[p.as_str()], Default::default());
                }
            }
            Action::QuickLook => self.toggle_quicklook(),
            Action::Rename => {
                if let Some(i) = self.single() {
                    if self.writable_here() || self.searching() && !read_only(&self.items[i].path) {
                        self.rename = Some((i, self.items[i].name.clone()));
                    } else {
                        self.flash(String::from("This location is read-only"));
                    }
                }
            }
            Action::Copy | Action::Cut => {
                if paths.is_empty() {
                    return;
                }
                corekit::clipboard::set_files(&paths);
                *self.cut.lock() = if action == Action::Cut { paths.clone() } else { Vec::new() };
                let what = if paths.len() == 1 {
                    format!("“{}”", fs::file_name(&paths[0]))
                } else {
                    format!("{} items", paths.len())
                };
                self.flash(format!("{} {what}", if action == Action::Cut { "Cut" } else { "Copied" }));
            }
            Action::Paste => self.paste(),
            Action::Duplicate => {
                let sources = paths
                    .into_iter()
                    .map(|p| {
                        let dir = String::from(fs::parent(&p));
                        (p, dir)
                    })
                    .collect();
                if !self.transfers.lock().start(sources, false) {
                    self.flash(String::from("Finish or cancel the current transfer first"));
                }
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
            Action::NewFolder => self.create(true, out),
            Action::NewFile => self.create(false, out),
            Action::ViewIcons => self.view = ViewMode::Icons,
            Action::ViewList => self.view = ViewMode::List,
            Action::SetWallpaper => {
                if let Some(p) = paths.first() {
                    out.push(Request::SetWallpaperImage(p.clone()));
                    self.flash(String::from("Set as desktop wallpaper"));
                }
            }
        }
    }

    pub fn open(&mut self, i: usize, out: &mut Vec<Request>) {
        let Some(e) = self.items.get(i).cloned() else { return };
        if e.is_dir {
            self.navigate(&e.path, true);
        } else if e.path.ends_with(".elf") || e.path.starts_with("/System/Bin/") {
            out.push(Request::OpenApp(e.path));
        } else {
            out.push(Request::OpenFile(e.path));
        }
    }

    pub fn trash(&mut self, paths: Vec<String>) {
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

    fn create(&mut self, dir: bool, out: &mut Vec<Request>) {
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
                    self.reveal = Some(i);
                    self.run(Action::Rename, out);
                }
            }
            Err(e) => self.flash(format!("Couldn't create it: {e}")),
        }
    }

    pub fn transfer(&mut self, sources: &[String], dir: &str, moving: bool) {
        let jobs = sources.iter().map(|p| (p.clone(), String::from(dir))).collect();
        let started = self.transfers.lock().start(jobs, moving);
        if !started {
            self.flash(String::from("Finish or cancel the current transfer first"));
        }
    }

    /// Files dropped on `dir`: moved within a volume, copied across volumes
    /// or with Ctrl held; text becomes a new file.
    pub fn dropped(&mut self, dir: &str, items: corekit::dnd::Dropped, copy: bool) {
        match items {
            corekit::dnd::Dropped::Files(paths) => {
                if dir == TRASH {
                    self.trash(paths);
                    return;
                }
                if paths.iter().any(|p| p == dir || fs::parent(p) == dir) {
                    return;
                }
                let copy = copy || paths.iter().any(|p| !same_volume(p, dir));
                self.transfer(&paths, dir, !copy);
            }
            corekit::dnd::Dropped::Text(text) => {
                let path = fs::unique_name(dir, "Dropped Text", ".txt");
                if fs::write(&path, text.as_bytes()).is_ok() {
                    self.flash(format!("Saved “{}”", fs::file_name(&path)));
                }
                self.refresh();
            }
        }
    }

    fn paste(&mut self) {
        if !self.writable_here() {
            self.flash(String::from("This location is read-only"));
            return;
        }
        let Some(paths) = corekit::clipboard::files() else {
            self.flash(String::from("Nothing to paste"));
            return;
        };
        let moving = {
            let cut = self.cut.lock();
            !cut.is_empty() && *cut == paths
        };
        let cwd = self.cwd.clone();
        self.transfer(&paths, &cwd, moving);
    }

    pub fn finish_rename(&mut self, commit: bool) {
        let Some((i, name)) = self.rename.take() else { return };
        let name = String::from(name.trim());
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

    pub fn confirmed(&mut self) {
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

    /// What a right click offers: on the selection, or on the folder.
    pub fn menu(&self, on_item: bool) -> Vec<Option<(Action, &'static str)>> {
        self.menu_with(&self.selected_paths(), on_item)
    }

    /// The menu item `i` would get if it were the selection.
    pub fn menu_for(&self, i: usize) -> Vec<Option<(Action, &'static str)>> {
        self.menu_with(&[self.items[i].path.clone()], true)
    }

    fn menu_with(&self, paths: &[String], on_item: bool) -> Vec<Option<(Action, &'static str)>> {
        let mut items: Vec<Option<(Action, &'static str)>> = Vec::new();
        let writable = self.writable_here();
        let one = paths.len() == 1;
        if on_item && self.in_trash() {
            items.push(Some((Action::PutBack, "Put Back")));
            items.push(Some((Action::DeleteForever, "Delete Immediately…")));
        } else if on_item {
            items.push(Some((Action::Open, "Open")));
            let pictures = paths.iter().all(|p| is_image(p));
            if paths.iter().all(|p| !fs::is_dir(p)) {
                items.push(Some((
                    // Program names: Lens is Preview.elf.
                    Action::OpenWith(if pictures { "Notes" } else { "Preview" }),
                    if pictures { "Open with Notes" } else { "Open with Lens" },
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
                items.push(Some((Action::Paste, "Paste")));
            }
            if !items.is_empty() {
                items.push(None);
            }
            items.push(Some((
                if self.view == ViewMode::Icons { Action::ViewList } else { Action::ViewIcons },
                if self.view == ViewMode::Icons { "View as List" } else { "View as Icons" },
            )));
        }
        items
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

    // ------------------------------------------------------------ keyboard

    pub fn select_all(&mut self) {
        self.selection = (0..self.items.len()).collect();
    }

    pub fn move_cursor(&mut self, delta: i32, extend: bool) {
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
        self.reveal = Some(to);
        if let Some(q) = &self.quicklook {
            if q.index != to {
                self.quicklook = Some(self.load_quicklook(to));
            }
        }
    }

    /// Type to select: jump to the first name starting with what was typed.
    pub fn type_select(&mut self, c: char, now: u64) {
        if now - self.typed.1 > 900 {
            self.typed.0.clear();
        }
        self.typed.0.extend(c.to_lowercase());
        self.typed.1 = now;
        let prefix = self.typed.0.clone();
        if let Some(i) = self.items.iter().position(|e| e.name.to_lowercase().starts_with(&prefix)) {
            self.select_only(i);
            self.reveal = Some(i);
        }
    }

    /// Makes the next thumbnail (one per call keeps the window responsive).
    fn make_thumbnail(&mut self) -> bool {
        if self.view != ViewMode::Icons {
            return false;
        }
        let next = self
            .items
            .iter()
            .take(400)
            .find(|e| !e.is_dir && is_image(&e.name) && e.size < 12 << 20 && !self.thumbs.contains_key(&e.path));
        let Some(e) = next else { return false };
        let path = e.path.clone();
        let thumb = fs::read(&path).ok().and_then(|d| aurora_image::decode(&d).ok()).map(|img| img.thumbnail(420, 280));
        // Charge capacity, path storage and conservative map-node overhead,
        // including failed decodes. Negative cache entries must not grow
        // without bound while browsing many folders of unsupported images.
        let cost = |path: &String, image: &Option<Picture>| {
            path.capacity() + image.as_ref().map_or(0, |i| i.pixels.capacity() * 4) + 192
        };
        let used: usize = self.thumbs.iter().map(|(p, i)| cost(p, i)).sum();
        if used + cost(&path, &thumb) > 12 * 1024 * 1024 {
            self.thumbs.clear();
        }
        self.thumbs.insert(path, thumb);
        true
    }

    /// Background work: search, thumbnails, changes made elsewhere. True to redraw.
    pub fn background(&mut self, now: u64) -> bool {
        let mut changed = false;
        {
            let mut worker = self.worker.lock();
            if worker.as_ref().is_some_and(|h| h.is_finished()) {
                worker.take().unwrap().join();
            }
        }
        if self.search_at.is_some_and(|at| now >= at) && self.worker.lock().is_none() {
            self.launch_search();
            changed = true;
        }
        if let Some((_, at)) = self.status {
            if now.saturating_sub(at) > 4000 {
                self.status = None;
                changed = true;
            }
        }
        changed |= self.poll_search();
        changed |= self.make_thumbnail();
        // Pick up changes made elsewhere (other windows, Terminal).
        if self.rename.is_none() && now - self.last_refresh > 2000 && !self.searching() {
            changed |= self.refresh_volumes();
            let before: Vec<(String, u64, u64)> =
                self.items.iter().map(|e| (e.name.clone(), e.size, e.mtime)).collect();
            self.refresh();
            let after: Vec<(String, u64, u64)> = self.items.iter().map(|e| (e.name.clone(), e.size, e.mtime)).collect();
            changed |= before != after;
        }
        changed
    }

    /// Still something to do soon (a search, thumbnails to make).
    pub fn busy(&self) -> bool {
        self.search_at.is_some()
            || !self.search_done()
            || self.view == ViewMode::Icons
                && self
                    .items
                    .iter()
                    .take(400)
                    .any(|e| !e.is_dir && is_image(&e.name) && e.size < 12 << 20 && !self.thumbs.contains_key(&e.path))
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    if matches!(args.get(1).map(String::as_str), Some("--powercut-prepare" | "--powercut-verify")) {
        let phase = match args.get(2).map(String::as_str) {
            Some("0") => 0,
            Some("1") => 1,
            Some("2") => 2,
            _ => return 1,
        };
        return Files::powercut_check(phase, args.get(1).is_some_and(|s| s == "--powercut-prepare"));
    }
    if args.get(1).is_some_and(|s| s == "--self-check") {
        return Files::self_check();
    }
    let start = args.get(1).map(|p| fs::resolve(&fs::cwd(), p)).unwrap_or_else(|| String::from("/Documents"));
    waveui::run(Files::new(&start))
}

/// "photo.png" → ("photo", ".png"); folders keep their dots.
fn split_ext(name: &str, is_dir: bool) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 && !is_dir => (String::from(&name[..i]), String::from(&name[i..])),
        _ => (String::from(name), String::new()),
    }
}
