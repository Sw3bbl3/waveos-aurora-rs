//! Spotlight (Super+Space): one search box for apps, files, settings,
//! quick calculations and system commands.

use super::{Desktop, PowerAction};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::apps::{self, AppKind, CATALOG};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{self, ACCENT, MENUBAR_H};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const WIDTH: i32 = 660;
const FIELD_H: i32 = 56;
const ROW_H: i32 = 46;
const MAX_ROWS: usize = 9;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    DarkMode,
    LightMode,
    Restart,
    ShutDown,
    EmptyTrash,
    Screenshot,
}

const COMMANDS: [(Command, &str, &str); 6] = [
    (Command::DarkMode, "Dark Mode", "Switch the appearance to dark"),
    (Command::LightMode, "Light Mode", "Switch the appearance to light"),
    (Command::Screenshot, "Take Screenshot", "Save the screen to Pictures"),
    (Command::EmptyTrash, "Empty Trash", "Permanently delete the items in the Trash"),
    (Command::Restart, "Restart", "Restart WaveOS Aurora"),
    (Command::ShutDown, "Shut Down", "Turn off the computer"),
];

/// Settings panes Spotlight can open (the argument Settings understands).
const PANES: [(&str, &str); 8] = [
    ("Appearance", "appearance"),
    ("Wallpaper", "appearance"),
    ("Display", "display"),
    ("Keyboard", "keyboard"),
    ("Date & Time", "datetime"),
    ("Sound", "sound"),
    ("Notifications", "notifications"),
    ("About This Computer", "about"),
];

#[derive(Clone, PartialEq, Debug)]
enum Result {
    App(AppKind),
    File { path: String, dir: bool },
    Pane(&'static str, &'static str),
    Calc(String),
    Cmd(Command),
}

pub struct Spotlight {
    query: String,
    results: Vec<Result>,
    selected: usize,
    hover: Option<usize>,
}

// ------------------------------------------------------------ calculator

// `f64::abs`/`fract` live in std; the kernel only has core.
fn abs(v: f64) -> f64 {
    if v < 0.0 {
        -v
    } else {
        v
    }
}

fn frac(v: f64) -> f64 {
    if abs(v) >= 9.0e15 {
        0.0
    } else {
        v - (v as i64) as f64
    }
}

/// Evaluates + − × ÷ % ^ and parentheses on decimal numbers.
fn calc(input: &str) -> Option<f64> {
    struct P<'a> {
        s: &'a [u8],
        i: usize,
    }
    impl P<'_> {
        fn ws(&mut self) {
            while self.i < self.s.len() && self.s[self.i] == b' ' {
                self.i += 1;
            }
        }
        fn peek(&mut self) -> Option<u8> {
            self.ws();
            self.s.get(self.i).copied()
        }
        fn expr(&mut self) -> Option<f64> {
            let mut v = self.term()?;
            while let Some(c) = self.peek() {
                match c {
                    b'+' => {
                        self.i += 1;
                        v += self.term()?
                    }
                    b'-' => {
                        self.i += 1;
                        v -= self.term()?
                    }
                    _ => break,
                }
            }
            Some(v)
        }
        fn term(&mut self) -> Option<f64> {
            let mut v = self.power()?;
            while let Some(c) = self.peek() {
                match c {
                    b'*' | b'x' => {
                        self.i += 1;
                        v *= self.power()?
                    }
                    b'/' => {
                        self.i += 1;
                        let d = self.power()?;
                        if d == 0.0 {
                            return None;
                        }
                        v /= d
                    }
                    b'%' => {
                        self.i += 1;
                        let d = self.power()?;
                        if d == 0.0 {
                            return None;
                        }
                        v %= d
                    }
                    _ => break,
                }
            }
            Some(v)
        }
        fn power(&mut self) -> Option<f64> {
            let base = self.unary()?;
            if self.peek() == Some(b'^') {
                self.i += 1;
                let e = self.power()?;
                if frac(e) != 0.0 || abs(e) > 64.0 {
                    return None;
                }
                let mut r = 1.0;
                for _ in 0..abs(e) as u32 {
                    r *= base;
                }
                return Some(if e < 0.0 { 1.0 / r } else { r });
            }
            Some(base)
        }
        fn unary(&mut self) -> Option<f64> {
            match self.peek()? {
                b'-' => {
                    self.i += 1;
                    Some(-self.unary()?)
                }
                b'(' => {
                    self.i += 1;
                    let v = self.expr()?;
                    (self.peek() == Some(b')')).then(|| self.i += 1)?;
                    Some(v)
                }
                _ => self.number(),
            }
        }
        fn number(&mut self) -> Option<f64> {
            self.ws();
            let start = self.i;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'.') {
                self.i += 1;
            }
            core::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
        }
    }
    let text = input.replace('×', "*").replace('÷', "/").replace('−', "-");
    if !text.bytes().any(|b| b"+-*/%^x".contains(&b)) {
        return None; // a bare number isn't a calculation
    }
    let mut p = P { s: text.as_bytes(), i: 0 };
    let v = p.expr()?;
    p.ws();
    (p.i == p.s.len() && v.is_finite()).then_some(v)
}

fn format_number(v: f64) -> String {
    if frac(v) == 0.0 && abs(v) < 1e15 {
        return format!("{}", v as i64);
    }
    let s = format!("{:.10}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    String::from(s)
}

impl Spotlight {
    fn search(&mut self) {
        let q = self.query.trim().to_lowercase();
        let mut out = Vec::new();
        if q.is_empty() {
            self.results = out;
            return;
        }
        if let Some(v) = calc(&q) {
            out.push(Result::Calc(format_number(v)));
        }
        let mut apps: Vec<(usize, AppKind)> = CATALOG
            .iter()
            .filter(|a| a.listed)
            .filter_map(|a| a.name.to_lowercase().find(&q).map(|p| (p, a.kind)))
            .collect();
        apps.sort_by_key(|(p, _)| *p);
        out.extend(apps.into_iter().map(|(_, k)| Result::App(k)));
        for (label, arg) in PANES {
            if label.to_lowercase().contains(&q) {
                out.push(Result::Pane(label, arg));
            }
        }
        for (cmd, label, _) in COMMANDS {
            if label.to_lowercase().contains(&q) {
                out.push(Result::Cmd(cmd));
            }
        }
        for (path, dir) in crate::fs::index::search(&q, 12) {
            out.push(Result::File { path, dir });
        }
        out.truncate(MAX_ROWS);
        self.results = out;
        self.selected = 0;
    }
}

impl Desktop {
    fn spotlight_rect(&self) -> Rect {
        let rows = self.spotlight.as_ref().map(|s| s.results.len()).unwrap_or(0) as i32;
        let h = FIELD_H + if rows > 0 { rows * ROW_H + 16 } else { 0 };
        Rect::new((self.w - WIDTH) / 2, MENUBAR_H + self.h / 6, WIDTH, h)
    }

    fn spotlight_area(&self) -> Rect {
        let r = self.spotlight_rect();
        Rect::new(r.x - 50, r.y - 50, r.w + 100, (FIELD_H + MAX_ROWS as i32 * ROW_H + 16) + 110)
    }

    fn spotlight_rows(&self) -> Vec<Rect> {
        let r = self.spotlight_rect();
        let n = self.spotlight.as_ref().map(|s| s.results.len()).unwrap_or(0);
        (0..n as i32).map(|i| Rect::new(r.x + 8, r.y + FIELD_H + 8 + i * ROW_H, r.w - 16, ROW_H)).collect()
    }

    pub(super) fn toggle_spotlight(&mut self) {
        let a = self.spotlight_area();
        self.damage(a);
        if self.spotlight.take().is_none() {
            if self.launcher.is_some() {
                self.toggle_launcher();
            }
            self.close_menu();
            self.spotlight = Some(Spotlight { query: String::new(), results: Vec::new(), selected: 0, hover: None });
        }
    }

    pub(super) fn spotlight_open(&self) -> bool {
        self.spotlight.is_some()
    }

    fn run_result(&mut self, r: Result, reveal: bool) {
        self.toggle_spotlight();
        match r {
            Result::App(k) => self.open(k),
            Result::File { path, dir } => {
                let files = apps::info(AppKind::Files).path;
                if dir {
                    self.spawn_program(files, &[path.as_str()]);
                } else if reveal {
                    let parent = crate::fs::parent_path(&path);
                    self.spawn_program(files, &[parent.as_str()]);
                } else {
                    self.open_file(&path);
                }
            }
            Result::Pane(_, arg) => {
                let settings = apps::info(AppKind::Settings).path;
                self.spawn_program(settings, &[arg]);
            }
            Result::Calc(v) => {
                let _ = crate::gui::clipboard::set(aurora_abi::clip::TEXT, v.as_bytes());
                crate::gui::notify::system("Copied to the clipboard", &v);
            }
            Result::Cmd(c) => match c {
                Command::DarkMode => self.set_dark(true),
                Command::LightMode => self.set_dark(false),
                Command::Restart => self.power = Some(PowerAction::Reboot),
                Command::ShutDown => self.power = Some(PowerAction::Shutdown),
                Command::EmptyTrash => {
                    let _ = crate::fs::trash::empty();
                    self.refresh_trash();
                }
                Command::Screenshot => self.screenshot = true,
            },
        }
    }

    pub(super) fn spotlight_key(&mut self, k: &KeyEvent) {
        if !k.pressed {
            return;
        }
        let Some(s) = self.spotlight.as_mut() else { return };
        match k.code {
            KeyCode::Escape => {
                self.toggle_spotlight();
                return;
            }
            KeyCode::Enter => {
                if let Some(r) = s.results.get(s.selected).cloned() {
                    self.run_result(r, k.mods.ctrl);
                }
                return;
            }
            KeyCode::Down if !s.results.is_empty() => s.selected = (s.selected + 1) % s.results.len(),
            KeyCode::Up if !s.results.is_empty() => s.selected = (s.selected + s.results.len() - 1) % s.results.len(),
            KeyCode::Backspace => {
                if k.mods.ctrl {
                    s.query.clear();
                } else {
                    s.query.pop();
                }
                s.search();
            }
            _ => match k.ch {
                Some(c) if !c.is_control() && !k.mods.ctrl && s.query.len() < 64 => {
                    s.query.push(c);
                    s.search();
                }
                _ => return,
            },
        }
        let a = self.spotlight_area();
        self.damage(a);
    }

    /// Returns true if Spotlight consumed the click.
    pub(super) fn spotlight_press(&mut self, x: i32, y: i32) -> bool {
        if self.spotlight.is_none() {
            return false;
        }
        if let Some(i) = self.spotlight_rows().iter().position(|r| r.contains(x, y)) {
            let r = self.spotlight.as_ref().unwrap().results[i].clone();
            self.run_result(r, false);
            return true;
        }
        if !self.spotlight_rect().contains(x, y) {
            self.toggle_spotlight();
        }
        true
    }

    pub(super) fn spotlight_hover(&mut self, x: i32, y: i32) {
        let rows = self.spotlight_rows();
        if let Some(s) = self.spotlight.as_mut() {
            let h = rows.iter().position(|r| r.contains(x, y));
            if h != s.hover {
                s.hover = h;
                if let Some(i) = h {
                    s.selected = i;
                }
                let a = self.spotlight_area();
                self.damage(a);
            }
        }
    }

    pub(super) fn paint_spotlight(&self, cv: &mut Canvas) {
        let Some(s) = &self.spotlight else { return };
        if !self.spotlight_area().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        let r = self.spotlight_rect();
        cv.shadow(r, 18, 40, 14, t.shadow);
        cv.glass(&self.blurred, r, 18, t.panel_tint);
        cv.stroke_round_rect(r, 18, if t.dark { 0x30FF_FFFF } else { 0x80FF_FFFF });
        // Magnifying glass.
        let (mx, my) = (r.x + 30, r.y + FIELD_H / 2 - 2);
        cv.fill_circle(mx, my, 9, t.text_secondary);
        cv.fill_circle(mx, my, 6, with_alpha(t.window_bg, 0xFF));
        cv.line(mx + 6, my + 6, mx + 12, my + 12, 3, t.text_secondary);
        let f = theme::ui(22);
        let base = r.y + (FIELD_H + f.ascent as i32 + f.descent as i32) / 2;
        if s.query.is_empty() {
            cv.text(r.x + 56, base, "Spotlight Search", f, t.text_secondary);
        } else {
            let w = cv.text(r.x + 56, base, &s.query, f, t.text);
            cv.fill_rect(Rect::new(r.x + 58 + w, r.y + 14, 2, FIELD_H - 28), ACCENT);
        }
        if s.results.is_empty() {
            return;
        }
        cv.fill_rect(Rect::new(r.x + 1, r.y + FIELD_H, r.w - 2, 1), t.separator);
        let small = theme::ui(12);
        for (i, (res, row)) in s.results.iter().zip(self.spotlight_rows()).enumerate() {
            let sel = i == s.selected;
            if sel {
                cv.fill_round_rect(row, 10, ACCENT);
            }
            let fg = if sel { 0xFFFF_FFFF } else { t.text };
            let fg2 = if sel { 0xDDFF_FFFF } else { t.text_secondary };
            let ir = Rect::new(row.x + 10, row.y + 7, 32, 32);
            let (title, subtitle, kind): (String, String, &str) = match res {
                Result::App(k) => {
                    let a = apps::info(*k);
                    icons::draw(cv, a.icon, ir);
                    (String::from(a.name), String::new(), "Application")
                }
                Result::File { path, dir } => {
                    let icon = if *dir {
                        Icon::Folder
                    } else if aurora_image::is_image_name(path) {
                        Icon::Picture
                    } else {
                        Icon::Document
                    };
                    icons::draw(cv, icon, ir);
                    let name = String::from(path.rsplit('/').next().unwrap_or(path));
                    (
                        name,
                        crate::fs::parent_path(path),
                        if *dir {
                            "Folder"
                        } else if aurora_image::is_image_name(path) {
                            "Picture"
                        } else {
                            "Document"
                        },
                    )
                }
                Result::Pane(label, _) => {
                    icons::draw(cv, Icon::Settings, ir);
                    (String::from(*label), String::from("Settings"), "Settings")
                }
                Result::Calc(v) => {
                    icons::draw(cv, Icon::Calculator, ir);
                    (format!("= {v}"), String::from("Press Enter to copy the result"), "Calculator")
                }
                Result::Cmd(c) => {
                    icons::draw(cv, Icon::Aurora, ir);
                    let (_, label, desc) = COMMANDS.iter().find(|(k, _, _)| k == c).unwrap();
                    (String::from(*label), String::from(*desc), "Command")
                }
            };
            let kw = small.width(kind);
            cv.text(row.right() - 14 - kw, row.y + 28, kind, small, fg2);
            if subtitle.is_empty() {
                cv.text_clipped(row.x + 54, row.y + 29, &title, theme::ui_bold(14), fg, row.w - 70 - kw);
            } else {
                cv.text_clipped(row.x + 54, row.y + 21, &title, theme::ui_bold(14), fg, row.w - 70 - kw);
                cv.text_clipped(row.x + 54, row.y + 38, &subtitle, small, fg2, row.w - 70 - kw);
            }
        }
    }
}

#[cfg(feature = "ktest")]
pub fn calc_for_test(s: &str) -> Option<f64> {
    calc(s)
}
