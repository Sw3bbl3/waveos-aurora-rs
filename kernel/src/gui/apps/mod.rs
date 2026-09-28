//! Built-in applications.
//!
//! In Milestone 1 apps run inside the compositor task and talk to it through
//! the [`App`] trait. In Milestone 2 the same trait moves behind IPC so apps
//! become separate user-space processes.

mod about;
mod calculator;
mod files;
mod notes;
mod power;
mod settings;
mod terminal;
mod welcome;

use super::canvas::Canvas;
use super::geom::Rect;
use super::icons::Icon;
use crate::drivers::input::KeyEvent;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AppKind {
    Welcome,
    About,
    Files,
    Terminal,
    Notes,
    Calculator,
    Settings,
    Power,
}

pub struct AppInfo {
    pub kind: AppKind,
    pub name: &'static str,
    pub icon: Icon,
    /// Always shown in the dock.
    pub pinned: bool,
    /// Listed in the launcher grid.
    pub listed: bool,
}

pub static CATALOG: &[AppInfo] = &[
    AppInfo { kind: AppKind::Files, name: "Files", icon: Icon::Files, pinned: true, listed: true },
    AppInfo { kind: AppKind::Terminal, name: "Terminal", icon: Icon::Terminal, pinned: true, listed: true },
    AppInfo { kind: AppKind::Notes, name: "Notes", icon: Icon::Notes, pinned: true, listed: true },
    AppInfo { kind: AppKind::Calculator, name: "Calculator", icon: Icon::Calculator, pinned: true, listed: true },
    AppInfo { kind: AppKind::Settings, name: "Settings", icon: Icon::Settings, pinned: true, listed: true },
    AppInfo { kind: AppKind::Welcome, name: "Welcome", icon: Icon::Welcome, pinned: false, listed: true },
    AppInfo { kind: AppKind::About, name: "About Aurora", icon: Icon::Aurora, pinned: false, listed: true },
    AppInfo { kind: AppKind::Power, name: "Power", icon: Icon::Aurora, pinned: false, listed: false },
];

pub fn info(kind: AppKind) -> &'static AppInfo {
    CATALOG.iter().find(|a| a.kind == kind).unwrap()
}

/// Things an app can ask the desktop to do.
pub enum Request {
    Open(AppKind),
    OpenFile(String),
    Close,
    Shutdown,
    Reboot,
    SetDark(bool),
    SetWallpaper(u8),
}

pub struct Env {
    pub now_ms: u64,
    pub focused: bool,
    pub screen: (i32, i32),
    pub requests: Vec<Request>,
}

pub trait App {
    fn kind(&self) -> AppKind;
    fn title(&self) -> String {
        String::from(info(self.kind()).name)
    }
    /// Initial content size (excluding the title bar).
    fn size(&self) -> (i32, i32);
    fn min_size(&self) -> (i32, i32) {
        (320, 200)
    }
    fn resizable(&self) -> bool {
        true
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env);
    fn key(&mut self, _ev: &KeyEvent, _env: &mut Env) -> bool {
        false
    }
    fn click(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    fn double_click(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    fn hover(&mut self, _x: i32, _y: i32, _area: Rect) -> bool {
        false
    }
    fn scroll(&mut self, _delta: i32, _area: Rect) -> bool {
        false
    }
    /// Called periodically (caret blink, clocks). Return true to repaint.
    fn tick(&mut self, _env: &mut Env) -> bool {
        false
    }
    /// Only one window of this app may exist.
    fn single_instance(&self) -> bool {
        true
    }
}

pub fn create(kind: AppKind) -> Box<dyn App> {
    match kind {
        AppKind::Welcome => Box::new(welcome::Welcome::new()),
        AppKind::About => Box::new(about::About::new()),
        AppKind::Files => Box::new(files::Files::new()),
        AppKind::Terminal => Box::new(terminal::Terminal::new()),
        AppKind::Notes => Box::new(notes::Notes::new(None)),
        AppKind::Calculator => Box::new(calculator::Calculator::new()),
        AppKind::Settings => Box::new(settings::Settings::new()),
        AppKind::Power => Box::new(power::PowerDialog::new()),
    }
}

pub fn open_file(path: &str) -> Box<dyn App> {
    Box::new(notes::Notes::new(Some(String::from(path))))
}

/// Hit-test helper: which of `rects` contains the point.
pub fn hit(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects.iter().position(|r| r.contains(x, y))
}
