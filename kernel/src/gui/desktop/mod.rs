//! The Aurora desktop: window management, input routing and scene painting.
//!
//! Rendering is damage-driven: every state change records the screen
//! rectangles it affects, and `render` repaints only those, back to front:
//! wallpaper → windows → dock → menu bar → launcher → menus → cursor.

mod anim;
mod dragdrop;
mod notifications;
mod shell;
mod spotlight;
mod volume;

#[cfg(feature = "ktest")]
pub use spotlight::calc_for_test;

use super::apps::{self, App, AppKind, ClientApp, CrashDialog, Env, Request};
use super::canvas::Canvas;
use super::cursor;
use super::geom::Rect;
use super::server::{self, Command};
use super::theme::{self, MENUBAR_H, TITLEBAR_H, WINDOW_RADIUS};
use super::wallpaper;
use crate::drivers::input::{InputEvent, KeyCode, KeyEvent, BUTTON_LEFT, BUTTON_RIGHT};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

pub use shell::{Launcher, Menu};

/// Decodes a picture and scales it to cover `w`×`h` (cropping the overflow).
fn picture_wallpaper(path: &str, w: i32, h: i32) -> Option<Vec<u32>> {
    let data = crate::fs::read_all(path).ok()?;
    let img = aurora_image::decode(&data).ok()?;
    let (iw, ih) = (img.width as i64, img.height as i64);
    // The largest centred crop with the screen's aspect ratio.
    let (cw, ch) =
        if iw * h as i64 > ih * w as i64 { (ih * w as i64 / h as i64, ih) } else { (iw, iw * h as i64 / w as i64) };
    let (cx, cy) = ((iw - cw) / 2, (ih - ch) / 2);
    let mut crop = aurora_image::Image::new(cw.max(1) as u32, ch.max(1) as u32, 0);
    for y in 0..crop.height as i64 {
        let src = ((cy + y) * iw + cx) as usize;
        let dst = (y * crop.width as i64) as usize;
        crop.pixels[dst..dst + crop.width as usize].copy_from_slice(&img.pixels[src..src + crop.width as usize]);
    }
    drop(img);
    let src = if crop.width as i32 > w * 2 { crop.thumbnail(w as u32 * 2, h as u32 * 2) } else { crop };
    let mut out = alloc::vec![0xFF00_0000u32; (w * h) as usize];
    let mut cv = Canvas::new(&mut out, w, h);
    cv.draw_image(&src.pixels, src.width, src.height, Rect::new(0, 0, w, h), 255);
    Some(out)
}

const SHADOW_BLUR: i32 = 30;
const SHADOW_OFFSET: i32 = 10;
const RESIZE_GRIP: i32 = 14;
const DOUBLE_CLICK_MS: u64 = 450;

pub struct Window {
    pub id: u32,
    pub app: Box<dyn App>,
    pub rect: Rect,
    pub minimized: bool,
    pub saved: Option<Rect>,
    /// Hidden while an animation stands in for it.
    pub animating: bool,
    /// A client window waits (up to a moment) for its first frame before it appears.
    pub awaiting_frame: Option<u64>,
}

impl Window {
    pub fn content(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y + TITLEBAR_H, self.rect.w, self.rect.h - TITLEBAR_H)
    }

    pub fn titlebar(&self) -> Rect {
        Rect::new(self.rect.x, self.rect.y, self.rect.w, TITLEBAR_H)
    }

    /// Everything this window paints, including its shadow.
    pub fn bounds(&self) -> Rect {
        let r = self.rect;
        Rect::new(r.x - SHADOW_BLUR, r.y - SHADOW_BLUR, r.w + 2 * SHADOW_BLUR, r.h + 2 * SHADOW_BLUR + SHADOW_OFFSET)
    }

    /// Close, minimize, zoom button centres.
    pub fn traffic(&self) -> [(i32, i32); 3] {
        let y = self.rect.y + TITLEBAR_H / 2;
        [(self.rect.x + 22, y), (self.rect.x + 42, y), (self.rect.x + 62, y)]
    }

    fn traffic_hit(&self, x: i32, y: i32) -> Option<usize> {
        self.traffic().iter().position(|&(cx, cy)| (x - cx).abs() <= 8 && (y - cy).abs() <= 8)
    }

    fn grip(&self) -> Rect {
        Rect::new(self.rect.right() - RESIZE_GRIP, self.rect.bottom() - RESIZE_GRIP, RESIZE_GRIP, RESIZE_GRIP)
    }
}

#[derive(Clone, Copy)]
enum DragKind {
    Move,
    Resize,
}

#[derive(Clone, Copy)]
struct Drag {
    window: u32,
    kind: DragKind,
    start_x: i32,
    start_y: i32,
    start_rect: Rect,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    Shutdown,
    Reboot,
}

pub struct Desktop {
    pub w: i32,
    pub h: i32,
    wallpaper: Vec<u32>,
    blurred: Vec<u32>,
    windows: Vec<Window>,
    next_id: u32,
    cursor: (i32, i32),
    buttons: u8,
    drag: Option<Drag>,
    damage: Vec<Rect>,
    launcher: Option<Launcher>,
    menu: Option<Menu>,
    dock_hover: Option<usize>,
    traffic_hover: Option<u32>,
    last_click: (u64, i32, i32),
    clock: String,
    now_ms: u64,
    pub power: Option<PowerAction>,
    /// Set when the user asks for a screenshot; the compositor takes it after the next frame.
    pub screenshot: bool,
    /// Apps started but whose window hasn't appeared yet (to ignore double launches).
    launching: Vec<(AppKind, u64)>,
    last_focus: Option<u32>,
    /// Window whose content was pressed: it receives the pointer until release.
    grab: Option<u32>,
    /// A drag-and-drop in progress.
    dnd: Option<dragdrop::Session>,
    trash_full: bool,
    ghosts: Vec<anim::Ghost>,
    banners: Vec<notifications::Banner>,
    banner_hover: Option<u32>,
    history: Vec<super::notify::Notification>,
    center: Option<notifications::Center>,
    spotlight: Option<spotlight::Spotlight>,
    /// Super is held and nothing else was pressed since.
    super_alone: bool,
    /// A resolution change for the compositor to carry out.
    pub resolution_request: Option<(u32, u32)>,
    /// The resolution to go back to if a new one isn't confirmed.
    previous_resolution: Option<(u32, u32)>,
    /// Ask "Keep this resolution?" after the next resize.
    confirm_resolution: bool,
    volume: volume::VolumeUi,
}

impl Desktop {
    pub fn new(w: i32, h: i32) -> Self {
        let (wallpaper, blurred) = Self::make_wallpaper(w, h);
        let mut d = Self {
            w,
            h,
            wallpaper,
            blurred,
            windows: Vec::new(),
            next_id: 1,
            cursor: (w / 2, h / 2),
            buttons: 0,
            drag: None,
            damage: Vec::new(),
            launcher: None,
            menu: None,
            dock_hover: None,
            traffic_hover: None,
            last_click: (0, 0, 0),
            clock: String::new(),
            now_ms: 0,
            power: None,
            screenshot: false,
            launching: Vec::new(),
            last_focus: None,
            grab: None,
            dnd: None,
            trash_full: crate::fs::trash::count() > 0,
            ghosts: Vec::new(),
            banners: Vec::new(),
            banner_hover: None,
            history: Vec::new(),
            center: None,
            spotlight: None,
            super_alone: false,
            resolution_request: None,
            previous_resolution: None,
            confirm_resolution: false,
            volume: volume::VolumeUi::default(),
        };
        d.open(AppKind::Welcome);
        d.damage_all();
        d
    }

    fn make_wallpaper(w: i32, h: i32) -> (Vec<u32>, Vec<u32>) {
        let custom = (theme::wallpaper() == wallpaper::CUSTOM)
            .then(|| super::settings::get("wallpaper_image"))
            .flatten()
            .and_then(|p| picture_wallpaper(&p, w, h));
        let wp = custom.unwrap_or_else(|| wallpaper::generate(theme::wallpaper(), w, h));
        let blurred = wallpaper::blur(&wp, w, h, 18);
        (wp, blurred)
    }

    pub fn set_now(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
    }

    pub fn screen(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h)
    }

    pub fn damage(&mut self, r: Rect) {
        let r = r.intersect(&self.screen());
        if !r.is_empty() {
            self.damage.push(r);
        }
    }

    pub fn damage_all(&mut self) {
        self.damage.clear();
        self.damage.push(self.screen());
    }

    fn env(&self, focused: bool) -> Env {
        Env { now_ms: self.now_ms, focused, screen: (self.w, self.h), requests: Vec::new() }
    }

    // ---------------------------------------------------------------- windows

    fn focused(&self) -> Option<&Window> {
        self.windows.iter().rev().find(|w| !w.minimized)
    }

    fn focused_id(&self) -> Option<u32> {
        self.focused().map(|w| w.id)
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// Area available to windows (between the menu bar and the dock).
    fn work_area(&self) -> Rect {
        let bottom = self.h - theme::DOCK_H - theme::DOCK_MARGIN - 8;
        Rect::new(8, MENUBAR_H + 8, self.w - 16, bottom - MENUBAR_H - 8)
    }

    pub fn open(&mut self, kind: AppKind) {
        let info = apps::info(kind);
        if info.single {
            if let Some(id) = self.windows.iter().rev().find(|w| w.app.kind() == kind).map(|w| w.id) {
                self.raise(id);
                return;
            }
        }
        if let Some(app) = apps::create_builtin(kind) {
            self.add_window(app);
            return;
        }
        if info.path.is_empty() {
            return;
        }
        let now = self.now_ms;
        if self.launching.iter().any(|&(k, t)| k == kind && now < t + 3000) {
            return;
        }
        self.launching.push((kind, now));
        self.spawn_program(info.path, &[]);
    }

    /// Starts a program (its window appears when it calls `win_create`).
    fn spawn_program(&mut self, path: &str, args: &[&str]) {
        let mut argv = alloc::vec![path];
        argv.extend_from_slice(args);
        if let Err(e) = crate::proc::spawn(path, &argv, [None, None, None], 0) {
            log!("gui", "could not start {}: {}", path, aurora_abi::err::name(e));
        }
    }

    /// Opens a document in the app for its type (pictures in Preview, the rest in Notes).
    pub fn open_file(&mut self, path: &str) {
        let kind = if aurora_image::is_image_name(path) { AppKind::Preview } else { AppKind::Notes };
        self.spawn_program(apps::info(kind).path, &[path]);
    }

    fn add_window(&mut self, app: Box<dyn App>) {
        let (cw, ch) = app.size();
        let wa = self.work_area();
        let w = cw.min(wa.w);
        let h = (ch + TITLEBAR_H).min(wa.h);
        let cascade = (self.windows.len() as i32 % 6) * 28;
        let x = wa.x + (wa.w - w) / 2 + cascade - 56;
        let y = wa.y + (wa.h - h) / 3 + cascade - 28;
        let x = x.min(wa.right() - w).max(wa.x);
        let y = y.min(wa.bottom() - h).max(wa.y);
        let rect = Rect::new(x, y, w, h);
        let id = self.next_id;
        self.next_id += 1;
        let prev = self.focused_id();
        // Client windows appear (animated) once they have drawn their first frame.
        let client = app.client_id().is_some();
        let awaiting_frame = client.then_some(self.now_ms);
        self.windows.push(Window { id, app, rect, minimized: false, saved: None, animating: client, awaiting_frame });
        if let Some(p) = prev {
            self.damage_window(p);
        }
        if !client {
            self.animate_open(id);
        }
        self.damage_window(id);
        self.damage_shell();
    }

    /// Shows a client window that has drawn its first frame (or took too long).
    fn reveal_window(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        if self.windows[i].awaiting_frame.take().is_some() {
            self.windows[i].animating = false;
            self.animate_open(id);
        }
    }

    /// The dock icon of the app owning window `id` (for minimize/restore).
    fn dock_icon_of(&self, id: u32) -> Rect {
        let kind = self.index_of(id).map(|i| self.windows[i].app.kind());
        let (dock, items) = self.dock_layout();
        items
            .into_iter()
            .find(|(it, _)| matches!(it, shell::DockItem::App(k) if Some(*k) == kind))
            .map(|(_, r)| r)
            .unwrap_or(Rect::new(dock.x + dock.w / 2 - 24, dock.y + 8, 48, 48))
    }

    fn damage_window(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            let b = self.windows[i].bounds();
            self.damage(b);
        }
    }

    /// Menu bar (app name) and dock (running indicators) depend on window state.
    fn damage_shell(&mut self) {
        self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
        let d = self.dock_rect();
        self.damage(d.inset(-40));
    }

    pub fn raise(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        let prev = self.focused_id();
        let mut w = self.windows.remove(i);
        let was_minimized = core::mem::replace(&mut w.minimized, false);
        self.windows.push(w);
        if was_minimized {
            let from = self.dock_icon_of(id);
            self.animate_restore(id, from);
        }
        if let Some(p) = prev {
            self.damage_window(p);
        }
        self.damage_window(id);
        self.damage_shell();
    }

    /// Asks the window's app to close (client apps decide for themselves).
    pub fn request_close(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            if self.windows[i].app.request_close() {
                self.close(id);
            }
        }
    }

    fn window_of_client(&self, client: u32) -> Option<u32> {
        self.windows.iter().find(|w| w.app.client_id() == Some(client)).map(|w| w.id)
    }

    pub fn close(&mut self, id: u32) {
        self.animate_close(id);
        if let Some(i) = self.index_of(id) {
            let b = self.windows[i].bounds();
            self.windows.remove(i);
            self.damage(b);
            if self.drag.is_some_and(|d| d.window == id) {
                self.drag = None;
            }
            if let Some(f) = self.focused_id() {
                self.damage_window(f);
            }
            self.damage_shell();
        }
    }

    pub fn minimize(&mut self, id: u32) {
        let target = self.dock_icon_of(id);
        self.animate_minimize(id, target);
        if let Some(i) = self.index_of(id) {
            self.windows[i].minimized = true;
            let b = self.windows[i].bounds();
            self.damage(b);
            // Keep minimized windows at the bottom of the stack.
            let w = self.windows.remove(i);
            self.windows.insert(0, w);
            if let Some(f) = self.focused_id() {
                self.damage_window(f);
            }
            self.damage_shell();
        }
    }

    pub fn toggle_zoom(&mut self, id: u32) {
        let wa = self.work_area();
        if let Some(i) = self.index_of(id) {
            if !self.windows[i].app.resizable() {
                return;
            }
            self.damage_window(id);
            let w = &mut self.windows[i];
            let old = w.rect;
            match w.saved.take() {
                Some(r) => w.rect = r,
                None => {
                    w.saved = Some(w.rect);
                    w.rect = wa;
                }
            }
            self.animate_zoom(id, old);
            self.damage_window(id);
        }
    }

    // ------------------------------------------------------------- requests

    fn apply(&mut self, source: Option<u32>, env: Env) {
        for req in env.requests {
            match req {
                Request::Open(kind) => self.open(kind),
                Request::Close => {
                    if let Some(id) = source {
                        self.close(id);
                    }
                }
                Request::Shutdown => self.power = Some(PowerAction::Shutdown),
                Request::Reboot => self.power = Some(PowerAction::Reboot),
                Request::KeepDisplay | Request::RevertDisplay => {
                    let revert = matches!(req, Request::RevertDisplay);
                    if let Some(prev) = self.previous_resolution.take() {
                        if revert {
                            self.resolution_request = Some(prev);
                        } else if let Err(e) = super::prefs::set_boot_resolution(self.w as u32, self.h as u32) {
                            // Kept for this session; the next start uses the firmware's choice.
                            log!("gui", "could not save the resolution: {}", aurora_abi::err::name(e));
                        }
                    }
                    if let Some(id) = source {
                        self.close(id);
                    }
                }
            }
        }
        if self.power.is_some() {
            self.damage_all();
        }
    }

    /// Applies a preference changed through `pref_set`.
    fn pref_changed(&mut self, key: &str) {
        use aurora_abi::pref;
        match key {
            pref::DARK => self.set_dark(super::prefs::get_bool(pref::DARK)),
            pref::ACCENT => {
                theme::set_accent(super::prefs::get(pref::ACCENT).and_then(|v| v.parse().ok()).unwrap_or(0));
                self.broadcast_theme();
                self.damage_all();
            }
            pref::CLOCK_24H | pref::CLOCK_SECONDS | pref::CLOCK_DATE => {
                self.clock = shell::clock_text();
                self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
            }
            other => super::prefs::apply_system(other),
        }
    }

    /// Adapts the desktop to a new screen size.
    pub fn resize(&mut self, w: i32, h: i32) {
        self.w = w;
        self.h = h;
        let (wp, bl) = Self::make_wallpaper(w, h);
        self.wallpaper = wp;
        self.blurred = bl;
        self.cursor = (self.cursor.0.min(w - 1), self.cursor.1.min(h - 1));
        let wa = self.work_area();
        for win in &mut self.windows {
            let r = &mut win.rect;
            r.w = r.w.min(wa.w);
            r.h = r.h.min(wa.h);
            r.x = r.x.clamp(wa.x, (wa.right() - r.w).max(wa.x));
            r.y = r.y.clamp(wa.y, (wa.bottom() - r.h).max(wa.y));
            if win.saved.is_some() {
                *r = wa; // zoomed windows stay zoomed
            }
        }
        self.ghosts.clear();
        for win in &mut self.windows {
            win.animating = win.awaiting_frame.is_some();
        }
        if core::mem::take(&mut self.confirm_resolution) {
            self.add_window(Box::new(apps::DisplayConfirm::new((w, h))));
        }
        self.damage_all();
    }

    fn broadcast_theme(&self) {
        server::broadcast(aurora_abi::Event {
            kind: aurora_abi::event::THEME,
            a: theme::current().dark as u32,
            b: theme::wallpaper() as u32,
            c: theme::accent_index() as u32,
            ..Default::default()
        });
    }

    pub fn set_dark(&mut self, dark: bool) {
        theme::set_dark(dark);
        super::save_settings();
        self.broadcast_theme();
        self.damage_all();
    }

    pub fn set_wallpaper(&mut self, index: u8) {
        theme::set_wallpaper(index);
        super::save_settings();
        let (wp, bl) = Self::make_wallpaper(self.w, self.h);
        self.wallpaper = wp;
        self.blurred = bl;
        self.broadcast_theme();
        self.damage_all();
    }

    /// Uses a picture file as the wallpaper.
    pub fn set_wallpaper_image(&mut self, path: &str) {
        super::settings::set("wallpaper_image", path);
        self.set_wallpaper(wallpaper::CUSTOM);
    }

    // ------------------------------------------------------ window server

    /// Applies requests from client processes and shows crash reports.
    pub fn process_commands(&mut self) {
        for cmd in server::take_commands() {
            match cmd {
                Command::Created(id) => {
                    let kind = server::info(id)
                        .and_then(|(pid, ..)| crate::proc::get(pid))
                        .and_then(|p| apps::by_path(&p.path))
                        .map(|a| a.kind)
                        .unwrap_or(AppKind::Other);
                    self.launching.retain(|(k, _)| *k != kind);
                    if let Some(app) = ClientApp::new(id, kind) {
                        self.add_window(Box::new(app));
                    }
                }
                Command::Closed(id) => {
                    if let Some(w) = self.window_of_client(id) {
                        self.close(w);
                    }
                }
                Command::Present(id, r) => {
                    if let Some(w) = self.window_of_client(id) {
                        self.reveal_window(w);
                    }
                    if let Some(i) = self.window_of_client(id).and_then(|w| self.index_of(w)) {
                        if !self.windows[i].minimized {
                            let c = self.windows[i].content();
                            self.damage(Rect::new(c.x + r.x, c.y + r.y, r.w, r.h).intersect(&c));
                        }
                    }
                }
                Command::Title(id) => {
                    if let Some(i) = self.window_of_client(id).and_then(|w| self.index_of(w)) {
                        let t = self.windows[i].titlebar();
                        self.damage(t);
                        self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
                    }
                }
                Command::OpenApp(path) => match apps::by_path(&path) {
                    Some(a) => self.open(a.kind),
                    None => self.spawn_program(&path, &[]),
                },
                Command::OpenFile(path) => self.open_file(&path),
                Command::SetDark(d) => self.set_dark(d),
                Command::SetWallpaper(i) => self.set_wallpaper(i),
                Command::SetWallpaperImage(p) => self.set_wallpaper_image(&p),
                Command::Shutdown => self.power = Some(PowerAction::Shutdown),
                Command::Reboot => self.power = Some(PowerAction::Reboot),
                Command::DragStart => self.begin_drag(),
                Command::TrashChanged => self.refresh_trash(),
                Command::Pref(key) => self.pref_changed(&key),
                Command::SetResolution(w, h) => {
                    if (w as i32, h as i32) != (self.w, self.h) {
                        self.previous_resolution.get_or_insert((self.w as u32, self.h as u32));
                        self.resolution_request = Some((w, h));
                        self.confirm_resolution = true;
                    }
                }
            }
        }
        while let Some(c) = crate::proc::take_crash() {
            // Programs started from a shell are reported by that shell instead.
            if c.parent != 0 && crate::proc::get(c.parent).is_some_and(|p| p.exit_code().is_none()) {
                continue;
            }
            let app = apps::by_path(&c.path);
            let name = app.map(|a| String::from(a.name)).unwrap_or(c.name);
            self.add_window(Box::new(CrashDialog::new(name, c.reason, app.map(|a| a.kind))));
        }
        self.take_notifications();
        if self.power.is_some() {
            self.damage_all();
        }
    }

    /// True when the compositor should draw at full frame rate.
    pub fn wants_frames(&self) -> bool {
        self.animating()
            || !self.banners.is_empty()
            || self.center.is_some()
            || self.launching.iter().any(|&(_, t)| self.now_ms < t + 3000)
    }

    /// Runs `f` against window `id`'s app and applies its requests.
    fn with_app(&mut self, id: u32, f: impl FnOnce(&mut dyn App, Rect, &mut Env) -> bool) {
        let Some(i) = self.index_of(id) else { return };
        let focused = self.focused_id() == Some(id);
        let mut env = self.env(focused);
        let content = self.windows[i].content();
        let before = self.windows[i].app.title();
        let redraw = f(self.windows[i].app.as_mut(), content, &mut env);
        if redraw {
            self.damage(content);
        }
        if self.windows[i].app.title() != before {
            let t = self.windows[i].titlebar();
            self.damage(t);
            self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
        }
        self.apply(Some(id), env);
    }

    // ---------------------------------------------------------------- input

    pub fn handle(&mut self, ev: InputEvent, now_ms: u64) {
        self.now_ms = now_ms;
        match ev {
            InputEvent::Pointer { x, y, buttons, wheel } => self.pointer(x, y, buttons, wheel),
            InputEvent::Key(k) => self.key(k),
        }
    }

    fn pointer(&mut self, x: i32, y: i32, buttons: u8, wheel: i8) {
        let x = x.clamp(0, self.w - 1);
        let y = y.clamp(0, self.h - 1);
        let moved = (x, y) != self.cursor;
        if moved {
            let (ox, oy) = self.cursor;
            self.damage(cursor::bounds(ox, oy));
            self.cursor = (x, y);
            self.damage(cursor::bounds(x, y));
        }
        let pressed = buttons & !self.buttons;
        let released = self.buttons & !buttons;
        self.buttons = buttons;

        if moved {
            self.pointer_moved(x, y);
        }
        if pressed & BUTTON_LEFT != 0 {
            self.press(x, y);
        }
        if pressed & BUTTON_RIGHT != 0 {
            self.right_press(x, y);
        }
        if released & BUTTON_LEFT != 0 && self.volume_release() {
            // The volume slider had the pointer.
        } else if released & BUTTON_LEFT != 0 && self.dnd.is_some() {
            self.grab = None;
            self.drop_drag(x, y);
        } else if released & BUTTON_LEFT != 0 {
            self.grab = None;
            if self.drag.take().is_none() {
                if let Some(f) = self.focused_id() {
                    self.with_app(f, |app, area, _| {
                        app.release(x, y, area);
                        false
                    });
                }
            }
        }
        if wheel != 0 {
            if let Some(w) = self.window_at(x, y) {
                let id = self.windows[w].id;
                self.with_app(id, |app, area, _| app.scroll(wheel as i32, area));
            }
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<usize> {
        self.windows.iter().rposition(|w| !w.minimized && w.awaiting_frame.is_none() && w.rect.contains(x, y))
    }

    fn pointer_moved(&mut self, x: i32, y: i32) {
        if self.buttons & BUTTON_LEFT != 0 && self.volume_drag(x) {
            return;
        }
        if self.dnd.is_some() {
            self.drag_moved(x, y);
            return;
        }
        if let Some(d) = self.drag {
            let Some(i) = self.index_of(d.window) else {
                self.drag = None;
                return;
            };
            let old = self.windows[i].bounds();
            let (dx, dy) = (x - d.start_x, y - d.start_y);
            let w = &mut self.windows[i];
            match d.kind {
                DragKind::Move => {
                    let ny = (d.start_rect.y + dy).clamp(MENUBAR_H, self.h - TITLEBAR_H);
                    w.rect = Rect::new(d.start_rect.x + dx, ny, d.start_rect.w, d.start_rect.h);
                }
                DragKind::Resize => {
                    let (mw, mh) = w.app.min_size();
                    w.rect.w = (d.start_rect.w + dx).max(mw);
                    w.rect.h = (d.start_rect.h + dy).max(mh + TITLEBAR_H);
                }
            }
            w.saved = None;
            let new = w.bounds();
            self.damage(old.union(&new));
            return;
        }

        // A pressed window content keeps the pointer (drawing, selecting, dragging).
        if let Some(g) = self.grab {
            if self.index_of(g).is_some() && self.buttons & BUTTON_LEFT != 0 {
                self.with_app(g, |app, area, _| app.drag(x, y, area));
                return;
            }
            self.grab = None;
        }

        self.update_banner_hover(x, y);
        self.update_center_hover(x, y);
        if self.spotlight_open() {
            self.spotlight_hover(x, y);
            return;
        }
        self.update_shell_hover(x, y);

        // Traffic-light hover glyphs.
        let th =
            self.window_at(x, y).filter(|&i| self.windows[i].traffic_hit(x, y).is_some()).map(|i| self.windows[i].id);
        if th != self.traffic_hover {
            for id in [self.traffic_hover, th].into_iter().flatten() {
                if let Some(i) = self.index_of(id) {
                    let t = self.windows[i].titlebar();
                    self.damage(t);
                }
            }
            self.traffic_hover = th;
        }

        // Hover inside the focused window's content.
        if let Some(f) = self.focused_id() {
            let over_top = self.window_at(x, y).map(|i| self.windows[i].id) == Some(f);
            let (hx, hy) = if over_top && self.launcher.is_none() && self.menu.is_none() { (x, y) } else { (-1, -1) };
            self.with_app(f, |app, area, _| app.hover(hx, hy, area));
        }
    }

    fn press(&mut self, x: i32, y: i32) {
        let double = self.now_ms - self.last_click.0 < DOUBLE_CLICK_MS
            && (x - self.last_click.1).abs() < 5
            && (y - self.last_click.2).abs() < 5;
        self.last_click = if double { (0, x, y) } else { (self.now_ms, x, y) };

        if self.volume_press(x, y) || self.spotlight_press(x, y) || self.center_press(x, y) {
            return;
        }
        if let Some(i) = self.banner_at(x, y) {
            self.banner_click(i);
            return;
        }
        // The clock opens the Notification Center.
        if y < MENUBAR_H && x > self.w - 250 && self.menu.is_none() && self.launcher.is_none() {
            self.toggle_center();
            return;
        }
        if self.shell_press(x, y) {
            return;
        }

        let Some(i) = self.window_at(x, y) else { return };
        let id = self.windows[i].id;
        if self.focused_id() != Some(id) {
            self.raise(id);
        }
        let i = self.index_of(id).unwrap();
        let w = &self.windows[i];
        if w.titlebar().contains(x, y) {
            match w.traffic_hit(x, y) {
                Some(0) => self.request_close(id),
                Some(1) => self.minimize(id),
                Some(2) => self.toggle_zoom(id),
                _ if double => self.toggle_zoom(id),
                _ => {
                    self.drag =
                        Some(Drag { window: id, kind: DragKind::Move, start_x: x, start_y: y, start_rect: w.rect })
                }
            }
        } else if w.app.resizable() && w.grip().contains(x, y) {
            self.drag = Some(Drag { window: id, kind: DragKind::Resize, start_x: x, start_y: y, start_rect: w.rect });
        } else if double {
            self.grab = Some(id);
            self.with_app(id, |app, area, env| app.double_click(x, y, area, env));
        } else {
            self.grab = Some(id);
            self.with_app(id, |app, area, env| app.click(x, y, area, env));
        }
    }

    /// Right-click: focuses the window under the pointer and forwards the click to its content.
    fn right_press(&mut self, x: i32, y: i32) {
        if self.menu.is_some() || self.launcher.is_some() || y < MENUBAR_H || self.dock_rect().contains(x, y) {
            return;
        }
        let Some(i) = self.window_at(x, y) else { return };
        let id = self.windows[i].id;
        if self.focused_id() != Some(id) {
            self.raise(id);
        }
        let Some(i) = self.index_of(id) else { return };
        if self.windows[i].content().contains(x, y) {
            self.with_app(id, |app, area, env| app.right_click(x, y, area, env));
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if self.dnd.is_some() {
            if k.pressed && k.code == KeyCode::Escape {
                self.cancel_drag();
            }
            return;
        }
        // Volume keys work everywhere.
        if k.pressed && self.volume_key(k.code) {
            return;
        }
        if k.pressed && k.code == KeyCode::Escape && self.volume_popover_open() {
            self.close_volume_popover();
            return;
        }
        // The Super key alone (pressed and released) opens the launcher; with
        // another key it is a modifier (Super+Space, Super+Shift+3, …).
        if k.code == KeyCode::Super {
            if k.pressed {
                self.super_alone = true;
            } else if core::mem::take(&mut self.super_alone) {
                if self.spotlight_open() {
                    self.toggle_spotlight();
                }
                self.toggle_launcher();
            }
            return;
        }
        if k.pressed {
            self.super_alone = false;
        }
        if k.pressed && (k.mods.super_key || k.mods.ctrl) && k.ch == Some(' ') {
            self.toggle_spotlight();
            return;
        }
        if self.spotlight_open() {
            self.spotlight_key(&k);
            return;
        }
        if k.pressed && k.code == KeyCode::Escape && self.center_open() {
            self.toggle_center();
            return;
        }
        if k.pressed {
            // Global shortcuts.
            if k.mods.ctrl && k.code == KeyCode::Escape {
                self.toggle_launcher();
                return;
            }
            let shot_combo = k.mods.super_key && k.mods.shift && matches!(k.ch, Some('3') | Some('#'));
            if k.code == KeyCode::PrintScreen || shot_combo {
                self.screenshot = true;
                return;
            }
            if k.mods.alt && k.code == KeyCode::Tab {
                self.cycle_windows();
                return;
            }
            if (k.mods.alt && k.code == KeyCode::F4) || (k.mods.ctrl && matches!(k.ch, Some('w') | Some('W'))) {
                if let Some(id) = self.focused_id() {
                    self.request_close(id);
                }
                return;
            }
            if k.code == KeyCode::Escape && self.menu.is_some() {
                self.close_menu();
                return;
            }
        }
        if self.launcher.is_some() {
            self.launcher_key(&k);
            return;
        }
        if let Some(id) = self.focused_id() {
            self.with_app(id, |app, _, env| app.key(&k, env));
        }
    }

    fn cycle_windows(&mut self) {
        let visible: Vec<u32> = self.windows.iter().filter(|w| !w.minimized).map(|w| w.id).collect();
        if visible.len() > 1 {
            self.raise(visible[0]);
        }
    }

    // ------------------------------------------------------------------ time

    /// Periodic work: clock, caret blink, live app data. Returns ms until the next tick.
    pub fn tick(&mut self, now_ms: u64) -> u64 {
        self.now_ms = now_ms;
        let clock = shell::clock_text();
        if clock != self.clock {
            self.clock = clock;
            self.damage(Rect::new(self.w - 260, 0, 260, MENUBAR_H));
        }
        let ids: Vec<u32> = self.windows.iter().filter(|w| !w.minimized).map(|w| w.id).collect();
        for id in ids {
            self.with_app(id, |app, _, env| app.tick(env));
        }
        // Tell apps when they gain or lose focus (caret blinking, etc.).
        let focus = self.focused_id();
        if focus != self.last_focus {
            for (id, on) in [(self.last_focus, false), (focus, true)] {
                if let Some(id) = id {
                    self.with_app(id, |app, _, _| {
                        app.focus(on);
                        false
                    });
                }
            }
            self.last_focus = focus;
        }
        self.launching.retain(|&(_, t)| now_ms < t + 5000);
        // Client windows that never drew still appear after a moment.
        let late: Vec<u32> =
            self.windows.iter().filter(|w| w.awaiting_frame.is_some_and(|t| now_ms > t + 600)).map(|w| w.id).collect();
        for id in late {
            self.reveal_window(id);
        }
        let mut busy = self.step_animations();
        busy |= self.step_notifications();
        self.step_volume();
        // Dock icons bounce while their app starts.
        if self.launching.iter().any(|&(_, t)| now_ms < t + 3000) {
            let d = self.dock_rect();
            self.damage(d.inset(-44));
            busy = true;
        }
        if busy {
            16
        } else {
            100
        }
    }

    // ------------------------------------------------------------- painting

    /// Merges pending damage into a small set of rectangles and clears it.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        let mut rects: Vec<Rect> = core::mem::take(&mut self.damage);
        // Merge overlapping or nearly-touching rectangles until stable.
        let mut merged = true;
        while merged {
            merged = false;
            'outer: for i in 0..rects.len() {
                for j in i + 1..rects.len() {
                    let a = rects[i];
                    let b = rects[j];
                    let u = a.union(&b);
                    if a.inset(-16).intersects(&b) || u.area() <= a.area() + b.area() + 4096 {
                        rects[i] = u;
                        rects.swap_remove(j);
                        merged = true;
                        break 'outer;
                    }
                }
            }
        }
        if rects.len() > 12 {
            let all = rects.iter().fold(Rect::default(), |acc, r| acc.union(r));
            return alloc::vec![all];
        }
        rects
    }

    pub fn paint(&mut self, cv: &mut Canvas) {
        cv.blit_same(&self.wallpaper, cv.clip);
        let focused = self.focused_id();
        let clip = cv.clip;
        for i in 0..self.windows.len() {
            let w = &self.windows[i];
            if w.minimized || w.animating || !w.bounds().intersects(&clip) {
                continue;
            }
            let is_focused = Some(self.windows[i].id) == focused;
            let env = self.env(is_focused);
            let hover = self.traffic_hover == Some(self.windows[i].id);
            paint_window(cv, &mut self.windows[i], is_focused, hover, &env, true);
        }
        self.paint_ghosts(cv);
        self.paint_shell(cv);
        self.paint_banners(cv);
        self.paint_center(cv);
        self.paint_volume(cv);
        self.paint_spotlight(cv);
        self.paint_drag(cv);
        if let Some(p) = self.power {
            self.paint_power_overlay(cv, p);
        }
        cursor::draw(cv, self.cursor.0, self.cursor.1);
    }

    fn paint_power_overlay(&self, cv: &mut Canvas, p: PowerAction) {
        cv.fill_rect(self.screen(), 0xC000_0000);
        let msg = if p == PowerAction::Shutdown { "Shutting down…" } else { "Restarting…" };
        let f = theme::ui_bold(20);
        let (cx, cy) = self.screen().center();
        super::icons::draw(cv, super::icons::Icon::Aurora, Rect::new(cx - 36, cy - 90, 72, 72));
        cv.text(cx - f.width(msg) / 2, cy + 20, msg, f, 0xFFFF_FFFF);
    }
}

fn paint_window(cv: &mut Canvas, w: &mut Window, focused: bool, hover: bool, env: &Env, shadow: bool) {
    let t = theme::current();
    let r = w.rect;
    if shadow {
        let strength = if focused { t.shadow } else { t.shadow * 6 / 10 };
        cv.shadow(r, WINDOW_RADIUS, SHADOW_BLUR, if focused { SHADOW_OFFSET } else { 6 }, strength);
    }
    cv.fill_round_rect(r, WINDOW_RADIUS, t.window_bg);

    // Title bar.
    let tb = w.titlebar();
    cv.with_clip(tb, |cv| cv.fill_round_rect(r, WINDOW_RADIUS, t.titlebar));
    cv.fill_rect(Rect::new(r.x, tb.bottom() - 1, r.w, 1), t.separator);
    let title = w.app.title();
    let f = theme::ui_bold(13);
    let tw = f.width(&title).min(r.w - 180);
    cv.text_clipped(
        r.x + (r.w - tw) / 2,
        tb.y + 25,
        &title,
        f,
        if focused { t.text } else { t.text_secondary },
        r.w - 180,
    );

    let colors = [theme::CLOSE, theme::MINIMIZE, theme::ZOOM];
    let can_zoom = w.app.resizable();
    for (i, &(cx, cy)) in w.traffic().iter().enumerate() {
        let active = (focused || hover) && (i != 2 || can_zoom);
        cv.fill_circle(cx, cy, 6, if active { colors[i] } else { t.traffic_inactive });
        if hover && active {
            let g = 0xA0_00_00_00 | 0x1A1A1A;
            match i {
                0 => {
                    cv.line(cx - 2, cy - 2, cx + 2, cy + 2, 1, g);
                    cv.line(cx + 2, cy - 2, cx - 2, cy + 2, 1, g);
                }
                1 => cv.fill_rect(Rect::new(cx - 3, cy, 7, 1), g),
                _ => {
                    cv.fill_rect(Rect::new(cx - 3, cy, 7, 1), g);
                    cv.fill_rect(Rect::new(cx, cy - 3, 1, 7), g);
                }
            }
        }
    }

    // App content.
    let content = w.content();
    cv.with_clip(content, |cv| w.app.draw(cv, content, env));
    cv.stroke_round_rect(r, WINDOW_RADIUS, t.window_border);
}
