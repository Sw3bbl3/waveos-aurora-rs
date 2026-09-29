//! Applications as seen by the window server.
//!
//! Real apps are user-space programs in `/System/Apps`; the window server
//! shows their windows through [`ClientApp`], which composites the process's
//! shared surface and forwards input as events. A few system dialogs (power,
//! crash reports) are built into the server and implement [`App`] directly.

mod client;
mod crash;
mod display;
mod power;

pub use client::ClientApp;
pub use crash::CrashDialog;
pub use display::DisplayConfirm;

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
    Preview,
    Paint,
    Clock,
    Activity,
    Power,
    Crash,
    DisplayConfirm,
    /// A program not in the catalog.
    Other,
}

pub struct AppInfo {
    pub kind: AppKind,
    pub name: &'static str,
    pub icon: Icon,
    /// Always shown in the dock.
    pub pinned: bool,
    /// Listed in the launcher grid.
    pub listed: bool,
    /// Program to run; empty for dialogs built into the window server.
    pub path: &'static str,
    /// Only one window of this app may exist (opening it again focuses it).
    pub single: bool,
}

pub static CATALOG: &[AppInfo] = &[
    AppInfo {
        kind: AppKind::Files,
        name: "Files",
        icon: Icon::Files,
        pinned: true,
        listed: true,
        path: "/System/Apps/Files.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::Terminal,
        name: "Terminal",
        icon: Icon::Terminal,
        pinned: true,
        listed: true,
        path: "/System/Apps/Terminal.elf",
        single: false,
    },
    AppInfo {
        kind: AppKind::Notes,
        name: "Notes",
        icon: Icon::Notes,
        pinned: true,
        listed: true,
        path: "/System/Apps/Notes.elf",
        single: false,
    },
    AppInfo {
        kind: AppKind::Calculator,
        name: "Calculator",
        icon: Icon::Calculator,
        pinned: true,
        listed: true,
        path: "/System/Apps/Calculator.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::Settings,
        name: "Settings",
        icon: Icon::Settings,
        pinned: true,
        listed: true,
        path: "/System/Apps/Settings.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::Preview,
        name: "Preview",
        icon: Icon::Preview,
        pinned: false,
        listed: true,
        path: "/System/Apps/Preview.elf",
        single: false,
    },
    AppInfo {
        kind: AppKind::Paint,
        name: "Paint",
        icon: Icon::Paint,
        pinned: false,
        listed: true,
        path: "/System/Apps/Paint.elf",
        single: false,
    },
    AppInfo {
        kind: AppKind::Clock,
        name: "Clock",
        icon: Icon::Clock,
        pinned: false,
        listed: true,
        path: "/System/Apps/Clock.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::Activity,
        name: "Activity Monitor",
        icon: Icon::Activity,
        pinned: false,
        listed: true,
        path: "/System/Apps/Activity.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::Welcome,
        name: "Welcome",
        icon: Icon::Welcome,
        pinned: false,
        listed: true,
        path: "/System/Apps/Welcome.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::About,
        name: "About Aurora",
        icon: Icon::Aurora,
        pinned: false,
        listed: true,
        path: "/System/Apps/About.elf",
        single: true,
    },
    AppInfo {
        kind: AppKind::Power,
        name: "Power",
        icon: Icon::Aurora,
        pinned: false,
        listed: false,
        path: "",
        single: true,
    },
    AppInfo {
        kind: AppKind::Crash,
        name: "Problem Report",
        icon: Icon::Aurora,
        pinned: false,
        listed: false,
        path: "",
        single: false,
    },
    AppInfo {
        kind: AppKind::DisplayConfirm,
        name: "Display",
        icon: Icon::Settings,
        pinned: false,
        listed: false,
        path: "",
        single: true,
    },
    AppInfo {
        kind: AppKind::Other,
        name: "App",
        icon: Icon::Aurora,
        pinned: false,
        listed: false,
        path: "",
        single: false,
    },
];

pub fn info(kind: AppKind) -> &'static AppInfo {
    CATALOG.iter().find(|a| a.kind == kind).unwrap()
}

pub fn by_path(path: &str) -> Option<&'static AppInfo> {
    CATALOG.iter().find(|a| !a.path.is_empty() && a.path == path)
}

/// Things an app can ask the desktop to do.
pub enum Request {
    Open(AppKind),
    Close,
    Shutdown,
    Reboot,
    /// Keep the display mode just chosen.
    KeepDisplay,
    /// Go back to the previous display mode.
    RevertDisplay,
}

/// Context passed to built-in dialogs.
#[allow(dead_code)] // not every dialog uses every field
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
        (240, 160)
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
    fn right_click(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    fn release(&mut self, _x: i32, _y: i32, _area: Rect) {}
    fn hover(&mut self, _x: i32, _y: i32, _area: Rect) -> bool {
        false
    }
    /// Pointer moved with the button held after a press in this window (may be outside it).
    fn drag(&mut self, x: i32, y: i32, area: Rect) -> bool {
        self.hover(x, y, area)
    }
    fn scroll(&mut self, _delta: i32, _area: Rect) -> bool {
        false
    }
    fn focus(&mut self, _focused: bool) {}
    /// Periodic work. Return true to repaint.
    fn tick(&mut self, _env: &mut Env) -> bool {
        false
    }
    /// The close button was pressed. Return true to close the window now;
    /// client apps return false and close themselves.
    fn request_close(&mut self) -> bool {
        true
    }
    /// The window-server id of a client window.
    fn client_id(&self) -> Option<u32> {
        None
    }
}

/// Creates a dialog built into the window server.
pub fn create_builtin(kind: AppKind) -> Option<Box<dyn App>> {
    match kind {
        AppKind::Power => Some(Box::new(power::PowerDialog::new())),
        _ => None,
    }
}

/// Hit-test helper: which of `rects` contains the point.
pub fn hit(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects.iter().position(|r| r.contains(x, y))
}
