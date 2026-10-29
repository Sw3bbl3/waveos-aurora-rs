//! The Aurora desktop: window management, input routing and scene painting.
//!
//! Rendering is damage-driven: every state change records the screen
//! rectangles it affects, and `render` repaints only those, back to front:
//! wallpaper → windows → Harbor → bar → Shoal → menus → cursor.

mod anim;
mod bar;
mod items;
mod switcher;
use lumen::windowing::{self, Placement, Switcher};
mod dragdrop;
mod handle;
mod harbor;
mod notifications;
mod shell;
mod shoal;
mod shore;
mod spotlight;

#[cfg(feature = "ktest")]
pub use spotlight::calc_for_test;

use super::account::Account;
use super::apps::{self, App, AppKind, ClientApp, CrashDialog, Env, Request};
use super::canvas::Canvas;
use super::cursor;
use super::geom::Rect;
use super::server::{self, Command};
use super::theme::{self, MENUBAR_H, WINDOW_RADIUS};
use super::wallpaper;
use crate::drivers::input::{InputEvent, KeyCode, KeyEvent, BUTTON_LEFT, BUTTON_RIGHT};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

pub use shell::Menu;
pub use shoal::Launcher;

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
    pub placement: Placement,
    /// Hidden while an animation stands in for it.
    pub animating: bool,
    /// A client window waits (up to a moment) for its first frame before it appears.
    pub awaiting_frame: Option<u64>,
}

impl Window {
    /// Apps get the whole window: there is no title bar (see `handle`).
    pub fn content(&self) -> Rect {
        self.rect
    }

    /// Everything this window paints, including its shadow.
    pub fn bounds(&self) -> Rect {
        let r = self.rect;
        Rect::new(r.x - SHADOW_BLUR, r.y - SHADOW_BLUR, r.w + 2 * SHADOW_BLUR, r.h + 2 * SHADOW_BLUR + SHADOW_OFFSET)
    }

    fn grip(&self) -> Rect {
        Rect::new(self.rect.right() - RESIZE_GRIP, self.rect.bottom() - RESIZE_GRIP, RESIZE_GRIP, RESIZE_GRIP)
    }
}

#[derive(Clone, Copy)]
enum DragKind {
    Move,
    Resize(u8),
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
    water: lumen::water::Water,
    water_coarse: bool,
    items: items::Items,
    wallpaper: Vec<u32>,
    blurred: Vec<u32>,
    scene: Vec<u32>,
    scene_dirty: bool,
    windows: Vec<Window>,
    mru: Vec<u32>,
    switcher: Option<Switcher>,
    /// Scrolling not yet a whole notch: the window it is for, and the amounts.
    scroll: (Option<u32>, i32, i32),
    next_id: u32,
    cursor: (i32, i32),
    buttons: u8,
    drag: Option<Drag>,
    damage: Vec<Rect>,
    launcher: Option<Launcher>,
    /// Shoal's frosted backdrop, frozen on the frame it opened (so pointing
    /// at an app repaints a tile, not the whole screen).
    shoal_backdrop: Option<Vec<u32>>,
    menu: Option<Menu>,
    harbor: harbor::HarborState,
    /// Where a dragged window would moor, and as what.
    snap_preview: Option<(Rect, Placement)>,
    /// The handle of the window the pointer is near, and the pill button under it.
    handle: Option<handle::Handle>,
    handle_button: Option<usize>,
    /// Moorings' layouts row is showing (a window is dragged near the top).
    moorings_open: bool,
    mooring_zone: Option<(u8, u8, u8, u8)>,
    /// Super is held (Super + drag moves a window from anywhere).
    super_down: bool,
    last_click: (u64, i32, i32),
    clock: String,
    now_ms: u64,
    last_tick: u64,
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
    /// Glide's window pictures, taken when it opens.
    glide: Vec<(u32, aurora_image::Image)>,
    history: Vec<super::notify::Notification>,
    center: Option<notifications::Center>,
    spotlight: Option<spotlight::Spotlight>,
    /// Super is held and nothing else was pressed since.
    super_alone: bool,
    /// A resolution change for the compositor to carry out.
    pub resolution_request: Option<(u32, u32)>,
    /// The display scale preference changed.
    pub rescale_request: bool,
    /// The resolution to go back to if a new one isn't confirmed.
    previous_resolution: Option<(u32, u32)>,
    /// Ask "Keep this resolution?" after the next resize.
    confirm_resolution: bool,
    /// Crest, the island in the middle of the top bar.
    crest: bar::Crest,
    /// Current, the quick settings panel under the status ring.
    current: Option<bar::Current>,
    bar_hover: Option<bar::BarHit>,
    /// The WayV mark for the left bubble (decoded on first use).
    /// The WayV mark for the bar, and the scale it was made for.
    mark: Option<(i32, Option<aurora_image::Image>)>,
    /// Whether the power adapter was connected at the last check.
    last_ac: Option<bool>,
    /// The computer's account (None until first-boot setup).
    account: Option<Account>,
    /// Shore: setup, login or lock (nothing else gets input while it's up).
    shore: Option<shore::Shore>,
}

impl Desktop {
    pub fn new(w: i32, h: i32) -> Self {
        let (wallpaper, blurred) = Self::make_wallpaper(w, h);
        let mut d = Self {
            w,
            h,
            water: lumen::water::Water::new(),
            water_coarse: false,
            items: items::Items::new(),
            wallpaper,
            blurred,
            scene: alloc::vec![0; (w*h) as usize],
            scene_dirty: true,
            windows: Vec::new(),
            mru: Vec::new(),
            switcher: None,
            scroll: (None, 0, 0),
            next_id: 1,
            cursor: (w / 2, h / 2),
            buttons: 0,
            drag: None,
            damage: Vec::new(),
            launcher: None,
            shoal_backdrop: None,
            menu: None,
            harbor: harbor::HarborState::default(),
            snap_preview: None,
            handle: None,
            handle_button: None,
            moorings_open: false,
            mooring_zone: None,
            super_down: false,
            last_click: (0, 0, 0),
            clock: String::new(),
            now_ms: 0,
            last_tick: 0,
            power: None,
            screenshot: false,
            launching: Vec::new(),
            last_focus: None,
            grab: None,
            dnd: None,
            trash_full: false,
            ghosts: Vec::new(),
            glide: Vec::new(),
            history: Vec::new(),
            center: None,
            spotlight: None,
            super_alone: false,
            resolution_request: None,
            rescale_request: false,
            previous_resolution: None,
            confirm_resolution: false,
            crest: bar::Crest::default(),
            current: None,
            bar_hover: None,
            mark: None,
            last_ac: None,
            account: None,
            shore: None,
        };
        d.account = Account::load();
        d.shore = Self::startup_shore(d.account.as_ref());
        d.last_ac = Some(crate::acpi::power_info().flags & aurora_abi::power::AC_ONLINE != 0);
        d.clock = bar::time_text();
        d.crest_retarget();
        d.damage_all();
        d
    }

    /// The wallpaper for the current appearance: the user's picture (whose
    /// colours become the accent), the built-in day or night picture, or —
    /// when no picture can be read — the procedural rendering.
    /// Both are in device pixels, for a `w`×`h`-point screen.
    fn make_wallpaper(w: i32, h: i32) -> (Vec<u32>, Vec<u32>) {
        let k = super::scale();
        let (w, h) = (w * k, h * k);
        let index = theme::wallpaper();
        let custom = (index == wallpaper::CUSTOM)
            .then(|| super::settings::get("wallpaper_image"))
            .flatten()
            .and_then(|p| picture_wallpaper(&p, w, h));
        if let Some(px) = &custom {
            theme::set_custom_tones(theme::tones_from_picture(px, w, h));
        }
        let wp = custom
            .or_else(|| wallpaper::file(index, theme::current().dark).and_then(|f| picture_wallpaper(f, w, h)))
            .unwrap_or_else(|| wallpaper::generate(index, w, h));
        let blurred = wallpaper::blur_coarse(&wp, w, h, 18 * k);
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
            self.scene_dirty = true;
            self.damage.push(r);
        }
    }

    pub fn damage_all(&mut self) {
        self.scene_dirty = true;
        self.damage.clear();
        self.damage.push(self.screen());
    }

    fn env(&self, focused: bool) -> Env {
        Env { now_ms: self.now_ms, focused, screen: (self.w, self.h), requests: Vec::new() }
    }

    // ---------------------------------------------------------------- windows

    fn focused(&self) -> Option<&Window> {
        if self.items.focused {
            return None;
        }
        self.windows.iter().rev().find(|w| !w.minimized)
    }

    fn focused_id(&self) -> Option<u32> {
        self.focused().map(|w| w.id)
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    /// Area available to windows: below the bubbles, down to the bottom
    /// edge (Harbor sinks away from a window that reaches it).
    fn work_area(&self) -> Rect {
        Rect::new(8, MENUBAR_H + 8, self.w - 16, self.h - 8 - MENUBAR_H - 8)
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

    /// Opens a document in the app for its type (pictures in Preview, web pages in NetSurf, the rest in Notes).
    pub fn open_file(&mut self, path: &str) {
        self.open_file_background(path);
    }

    fn add_window(&mut self, app: Box<dyn App>) {
        self.items.focused = false;
        let (cw, ch) = app.size();
        let wa = self.work_area();
        let w = cw.min(wa.w);
        let h = ch.min(wa.h);
        let cascade = (self.windows.len() as i32 % 6) * 28;
        let x = wa.x + (wa.w - w) / 2 + cascade - 56;
        let y = wa.y + (wa.h - h) / 3 + cascade - 28;
        let x = x.min(wa.right() - w).max(wa.x);
        let y = y.min(wa.bottom() - h).max(wa.y);
        let rect = Rect::new(x, y, w, h);
        let id = self.next_id;
        self.next_id += 1;
        self.touch_mru(id);
        let prev = self.focused_id();
        // Client windows appear (animated) once they have drawn their first frame.
        let client = app.client_id().is_some();
        let awaiting_frame = client.then_some(self.now_ms);
        self.windows.push(Window {
            id,
            app,
            rect,
            minimized: false,
            saved: None,
            placement: Placement::Floating,
            animating: client,
            awaiting_frame,
        });
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
        self.update_harbor();
    }

    pub fn raise(&mut self, id: u32) {
        self.items.focused = false;
        let Some(i) = self.index_of(id) else { return };
        self.touch_mru(id);
        self.damage_all();
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
        self.mru.retain(|v| *v != id);
        if let Some(s) = &mut self.switcher {
            s.remove(id);
        }
        self.damage_all();
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
        let Some(i) = self.index_of(id) else { return };
        let placement =
            if self.windows[i].placement == Placement::Maximized { Placement::Floating } else { Placement::Maximized };
        self.place_window(id, placement);
    }

    fn place_window(&mut self, id: u32, placement: Placement) {
        let area = self.work_area();
        let Some(i) = self.index_of(id) else { return };
        if !self.windows[i].app.resizable() {
            return;
        }
        let w = &mut self.windows[i];
        let old = w.rect;
        let (mw, mh) = w.app.min_size();
        let next = if placement == Placement::Floating {
            windowing::clamp(w.saved.unwrap_or(old), area)
        } else {
            let Some(r) = windowing::target(placement, area, (mw, mh)) else { return };
            if w.placement == Placement::Floating {
                w.saved = Some(old);
            }
            r
        };
        w.rect = next;
        w.placement = placement;
        if placement == Placement::Floating {
            w.saved = None;
        }
        self.animate_zoom(id, old);
        self.damage_all();
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
                Request::Sleep => {
                    if let Some(id) = source {
                        self.close(id);
                    }
                    crate::power::sleep();
                }
                Request::KeepDisplay | Request::RevertDisplay => {
                    let revert = matches!(req, Request::RevertDisplay);
                    if let Some(prev) = self.previous_resolution.take() {
                        if revert {
                            self.resolution_request = Some(prev);
                        } else if let Err(e) = {
                            let (pw, ph) = super::screen_pixels();
                            super::prefs::set_boot_resolution(pw as u32, ph as u32)
                        } {
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
            // Draw again at the new scale (same resolution).
            pref::DISPLAY_SCALE => self.rescale_request = true,
            pref::GLASS
            | pref::REDUCE_TRANSPARENCY
            | pref::REDUCE_MOTION
            | pref::HIGH_CONTRAST
            | pref::DOCK_SIZE
            | pref::DOCK_AUTOHIDE => {
                super::prefs::apply_appearance();
                if matches!(key, pref::DOCK_SIZE | pref::DOCK_AUTOHIDE) {
                    let wa = self.work_area();
                    for win in &mut self.windows {
                        let (mw, mh) = win.app.min_size();
                        if let Some(r) = windowing::target(win.placement, wa, (mw, mh)) {
                            win.rect = r;
                        }
                    }
                }
                self.broadcast_theme();
                self.damage_all();
            }
            pref::DARK => self.set_dark(super::prefs::get_bool(pref::DARK)),
            pref::ACCENT => {
                self.shoal_backdrop = None;
                theme::set_accent(super::prefs::get(pref::ACCENT).and_then(|v| v.parse().ok()).unwrap_or(0));
                self.broadcast_theme();
                self.damage_all();
            }
            pref::CLOCK_24H | pref::CLOCK_SECONDS | pref::CLOCK_DATE | pref::CREST | pref::BATTERY_LEVEL => {
                self.clock = bar::time_text();
                self.crest_retarget();
                self.damage(Rect::new(0, 0, self.w, MENUBAR_H + 40));
                if self.current_open() {
                    let r = self.current_rect();
                    self.damage(r);
                }
            }
            pref::DND => {
                if self.current_open() {
                    let r = self.current_rect();
                    self.damage(r);
                }
            }
            other => super::prefs::apply_system(other),
        }
    }

    /// Adapts the desktop to a new screen size.
    pub fn resize(&mut self, w: i32, h: i32) {
        self.water.clear();
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
            if let Some(saved) = win.saved.as_mut() {
                *saved = windowing::clamp(*saved, wa);
            }
            if win.placement != Placement::Floating {
                let (mw, mh) = win.app.min_size();
                if let Some(target) = windowing::target(win.placement, wa, (mw, mh)) {
                    *r = target;
                } else {
                    *r = windowing::clamp(win.saved.take().unwrap_or(*r), wa);
                    win.placement = Placement::Floating;
                }
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
            d: theme::custom_fill(),
            ..Default::default()
        });
    }

    pub fn set_dark(&mut self, dark: bool) {
        self.water.clear();
        self.shoal_backdrop = None;
        theme::set_dark(dark);
        super::save_settings();
        // Each built-in wallpaper has a day and a night picture.
        if theme::wallpaper() != wallpaper::CUSTOM {
            let (wp, bl) = Self::make_wallpaper(self.w, self.h);
            self.wallpaper = wp;
            self.blurred = bl;
        }
        self.broadcast_theme();
        self.damage_all();
    }

    pub fn set_wallpaper(&mut self, index: u8) {
        self.water.clear();
        self.shoal_backdrop = None;
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
                Command::AppsChanged => {
                    if self.launcher.is_some() {
                        self.toggle_launcher();
                        self.toggle_launcher();
                    }
                    self.damage_all();
                }
                Command::Created(id) => {
                    let kind = server::info(id)
                        .and_then(|(pid, ..)| crate::proc::get(pid))
                        .and_then(|p| apps::by_path(&p.path()))
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
                    if let Some(w) = self.window_of_client(id) {
                        self.damage_handle(w);
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
                Command::PowerButton => self.open(AppKind::Power),
                Command::PowerChanged => self.power_changed(),
                // The screen lost its contents while asleep; waking asks for the password.
                Command::Resumed => {
                    self.lock();
                    self.damage_all();
                }
                Command::NetworkChanged => self.damage(Rect::new(0, 0, self.w, MENUBAR_H)),
                // Follow the screen's new preferred size, without asking.
                Command::DisplayChanged => self.resolution_request = Some((0, 0)),
                Command::SetResolution(w, h) => {
                    let (pw, ph) = super::screen_pixels();
                    if (w as i32, h as i32) != (pw, ph) {
                        self.previous_resolution.get_or_insert((pw as u32, ph as u32));
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
            || self.water.active()
            || self.notifications_moving()
            || self.crest_moving()
            || self.shore_moving()
            || self.harbor_moving()
    }

    /// Absolute deadline, recalculated after input and client commands. Client
    /// apps own their timers; only built-in dialogs need the legacy 100 ms tick.
    pub fn next_wakeup(&self) -> u64 {
        if self.focused_id() != self.last_focus {
            return self.now_ms;
        }
        // Sample the wall clock at most once a second (also catches time edits).
        let mut next = self.last_tick + 1000;
        if self.wants_frames() {
            next = next.min(self.last_tick + 16);
        }
        if self.windows.iter().any(|w| !w.minimized && w.app.client_id().is_none()) {
            next = next.min(self.last_tick + 100);
        }
        for window in &self.windows {
            if let Some(at) = window.awaiting_frame {
                next = next.min(at + 600);
            }
        }
        for &(_, at) in &self.launching {
            next = next.min(at + 5000);
        }
        // The caret on Shore blinks.
        if self.shore.is_some() {
            next = next.min(self.last_tick + 530);
        }
        next.min(self.notification_deadline()).min(self.bar_deadline()).min(self.harbor_deadline())
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
            self.damage_handle(id);
            self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
        }
        self.apply(Some(id), env);
    }

    // ---------------------------------------------------------------- input

    pub fn handle(&mut self, ev: InputEvent, now_ms: u64) {
        self.now_ms = now_ms;
        match ev {
            InputEvent::Pointer { x, y, buttons } => self.pointer(x, y, buttons),
            InputEvent::Scroll { dx, dy } => self.scroll(dx, dy),
            InputEvent::Key(k) => self.key(k),
        }
        if self.shore.is_none() {
            self.update_harbor();
        }
    }

    fn pointer(&mut self, x: i32, y: i32, buttons: u8) {
        let x = x.clamp(0, self.w - 1);
        let y = y.clamp(0, self.h - 1);
        let moved = (x, y) != self.cursor;
        if moved {
            let (ox, oy) = self.cursor;
            self.damage.push(cursor::bounds(ox, oy).intersect(&self.screen()));
            self.cursor = (x, y);
            self.damage.push(cursor::bounds(x, y).intersect(&self.screen()));
        }
        let pressed = buttons & !self.buttons;
        let released = self.buttons & !buttons;
        self.buttons = buttons;
        // Shore takes every click and key while it is up.
        if self.shore.is_some() {
            self.shore_pointer(x, y, pressed & BUTTON_LEFT != 0);
            return;
        }

        if moved {
            self.pointer_moved(x, y);
        }
        if pressed & BUTTON_LEFT != 0 {
            self.press(x, y);
        }
        if pressed & BUTTON_RIGHT != 0 {
            self.right_press(x, y);
        }
        if released & BUTTON_LEFT != 0 && self.current_release() {
            // Current's volume bar had the pointer.
        } else if released & BUTTON_LEFT != 0 && self.dnd.is_some() {
            self.grab = None;
            self.drop_drag(x, y);
        } else if released & BUTTON_LEFT != 0 && self.items_release(x, y) {
        } else if released & BUTTON_LEFT != 0 {
            self.grab = None;
            let drag = self.drag.take();
            if let Some(d) = drag {
                self.damage_all(); // Restore stationary material sampling.
                self.moorings_open = false;
                self.mooring_zone = None;
                if let Some((target, placement)) = self.snap_preview.take() {
                    if let Some(i) = self.index_of(d.window) {
                        self.windows[i].saved = Some(d.start_rect);
                        self.windows[i].rect = target;
                        self.windows[i].placement = placement;
                    }
                    self.damage_all();
                }
            } else {
                if let Some(f) = self.focused_id() {
                    self.with_app(f, |app, area, _| {
                        app.release(x, y, area);
                        false
                    });
                }
            }
        }
    }

    /// Scrolling (1/120 notch units) for the window under the pointer: whole
    /// notches for apps that step, and the exact amount for smooth ones.
    fn scroll(&mut self, dx: i32, dy: i32) {
        use crate::drivers::input::NOTCH;
        if self.shore.is_some() || self.switcher.is_some() {
            return;
        }
        let (x, y) = self.cursor;
        let Some(w) = self.window_at(x, y) else {
            self.items_scroll(dy);
            self.scroll = (None, 0, 0);
            return;
        };
        let id = self.windows[w].id;
        if self.scroll.0 != Some(id) {
            self.scroll = (Some(id), 0, 0);
        }
        self.scroll.1 += dx;
        self.scroll.2 += dy;
        let notches = (self.scroll.1 / NOTCH, self.scroll.2 / NOTCH);
        self.scroll.1 %= NOTCH;
        self.scroll.2 %= NOTCH;
        self.with_app(id, |app, area, _| app.scroll(notches, (dx, dy), area));
    }

    fn window_at(&self, x: i32, y: i32) -> Option<usize> {
        self.windows.iter().rposition(|w| !w.minimized && w.awaiting_frame.is_none() && w.rect.contains(x, y))
    }

    fn pointer_moved(&mut self, x: i32, y: i32) {
        if self.switcher.is_some() {
            return;
        }
        if self.buttons & BUTTON_LEFT != 0 && self.current_drag(x) {
            return;
        }
        if self.dnd.is_some() {
            self.drag_moved(x, y);
            return;
        }
        if self.items_motion(x, y) {
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
                    let ny = (d.start_rect.y + dy).clamp(MENUBAR_H, self.h - 40);
                    w.rect = Rect::new(d.start_rect.x + dx, ny, d.start_rect.w, d.start_rect.h);
                }
                DragKind::Resize(edges) => {
                    let (mw, mh) = w.app.min_size();
                    let min_h = mh;
                    if edges & 1 != 0 {
                        w.rect.x = (d.start_rect.x + dx).min(d.start_rect.right() - mw);
                        w.rect.w = d.start_rect.right() - w.rect.x;
                    }
                    if edges & 2 != 0 {
                        w.rect.w = (d.start_rect.w + dx).max(mw);
                    }
                    if edges & 4 != 0 {
                        w.rect.y = (d.start_rect.y + dy).clamp(MENUBAR_H, d.start_rect.bottom() - min_h);
                        w.rect.h = d.start_rect.bottom() - w.rect.y;
                    }
                    if edges & 8 != 0 {
                        w.rect.h = (d.start_rect.h + dy).max(min_h);
                    }
                }
            }
            w.saved = None;
            w.placement = Placement::Floating;
            let new = w.bounds();
            if matches!(d.kind, DragKind::Move) {
                self.moorings_drag(i, x, y);
            }
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

        self.bar_hover(x, y);
        self.update_center_hover(x, y);
        if self.spotlight_open() {
            self.spotlight_hover(x, y);
            return;
        }
        self.update_shell_hover(x, y);

        // The handle on a window's top edge.
        self.handle_hover(x, y);

        // Hover inside the focused window's content.
        if let Some(f) = self.focused_id() {
            let over_top = self.window_at(x, y).map(|i| self.windows[i].id) == Some(f);
            let (hx, hy) = if over_top && self.launcher.is_none() && self.menu.is_none() { (x, y) } else { (-1, -1) };
            self.with_app(f, |app, area, _| app.hover(hx, hy, area));
        }
    }

    fn press(&mut self, x: i32, y: i32) {
        if self.items_overlay_press(x, y) {
            return;
        }
        if self.switcher_press(x, y) {
            return;
        }
        let double = self.now_ms - self.last_click.0 < DOUBLE_CLICK_MS
            && (x - self.last_click.1).abs() < 5
            && (y - self.last_click.2).abs() < 5;
        self.last_click = if double { (0, x, y) } else { (self.now_ms, x, y) };

        if self.spotlight_press(x, y) || self.center_press(x, y) {
            return;
        }
        if self.menu.is_none() && self.bar_press(x, y) {
            return;
        }
        if self.shell_press(x, y) {
            return;
        }
        if self.handle_press(x, y, double) {
            return;
        }

        let Some(i) = self.window_at(x, y) else {
            self.items_press(x, y, double);
            return;
        };
        // Super + drag moves a window from anywhere in it.
        if self.super_down {
            let id = self.windows[i].id;
            self.super_alone = false;
            self.raise(id);
            self.begin_move(id, x, y);
            return;
        }
        let id = self.windows[i].id;
        if self.focused_id() != Some(id) {
            self.raise(id);
        }
        let i = self.index_of(id).unwrap();
        let w = &self.windows[i];
        let mut edges = 0u8;
        if w.app.resizable() {
            if x < w.rect.x + 5 {
                edges |= 1;
            }
            if x >= w.rect.right() - 5 {
                edges |= 2;
            }
            if y < w.rect.y + 5 {
                edges |= 4;
            }
            if y >= w.rect.bottom() - 5 {
                edges |= 8;
            }
            if w.grip().contains(x, y) {
                edges |= 10;
            }
        }
        if edges != 0 {
            self.drag =
                Some(Drag { window: id, kind: DragKind::Resize(edges), start_x: x, start_y: y, start_rect: w.rect });
            return;
        }
        if w.app.resizable() && w.grip().contains(x, y) {
            self.drag =
                Some(Drag { window: id, kind: DragKind::Resize(10), start_x: x, start_y: y, start_rect: w.rect });
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
        if self.shoal_context(x, y) {
            return;
        }
        if self.switcher.is_some() {
            return;
        }
        if self.menu.is_some() || self.launcher.is_some() || y < MENUBAR_H || self.dock_rect().contains(x, y) {
            return;
        }
        let Some(i) = self.window_at(x, y) else {
            self.items_context(x, y);
            return;
        };
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
        if self.shore.is_some() {
            self.shore_key(&k);
            return;
        }
        if self.items.menu.is_some() || self.items.rename.is_some() {
            self.items_key(&k);
            return;
        }
        if self.switcher_key(&k) {
            return;
        }
        if k.pressed && k.code == KeyCode::Escape && self.current_open() {
            self.close_current();
            return;
        }
        // Super L locks the screen.
        if k.pressed && k.mods.super_key && matches!(k.ch, Some('l') | Some('L')) {
            self.super_alone = false;
            self.lock();
            return;
        }
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
        // The Super key alone (pressed and released) opens the launcher; with
        // another key it is a modifier (Super+Space, Super+Shift+3, …).
        if k.code == KeyCode::Super {
            self.super_down = k.pressed;
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
            if k.mods.super_key {
                if let Some(id) = self.focused_id() {
                    let placement = match k.code {
                        KeyCode::Left => Some(Placement::Left),
                        KeyCode::Right => Some(Placement::Right),
                        KeyCode::Up => Some(Placement::Maximized),
                        KeyCode::Down => Some(Placement::Floating),
                        _ => None,
                    };
                    if let Some(p) = placement {
                        if p == Placement::Floating
                            && self.windows[self.index_of(id).unwrap()].placement == Placement::Floating
                        {
                            self.minimize(id);
                        } else {
                            self.place_window(id, p);
                        }
                        return;
                    }
                }
            }
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
                self.begin_switcher(k.mods.shift);
                return;
            }
            if (k.mods.alt && k.code == KeyCode::F4) || (k.mods.ctrl && matches!(k.ch, Some('w') | Some('W'))) {
                if let Some(id) = self.focused_id() {
                    if k.mods.ctrl && matches!(k.ch, Some('w') | Some('W')) {
                        if let Some(i) = self.index_of(id) {
                            if self.windows[i].app.request_close_tab() {
                                self.close(id);
                            }
                        }
                    } else {
                        self.request_close(id);
                    }
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
        if self.items.focused {
            self.items_key(&k);
            return;
        }
        if let Some(id) = self.focused_id() {
            self.with_app(id, |app, _, env| app.key(&k, env));
        }
    }

    fn water_click(&mut self, x: i32, y: i32) {
        if !theme::reduce_motion() && super::prefs::get_bool(aurora_abi::pref::DESKTOP_WATER) {
            self.water.impulse(x, y, self.w, self.h, self.now_ms);
        }
    }
    fn step_water(&mut self) -> bool {
        if self.shore.is_some() || theme::reduce_motion() || !super::prefs::get_bool(aurora_abi::pref::DESKTOP_WATER) {
            if self.water.active() {
                self.water.clear();
                self.damage_all();
            }
            return false;
        }
        let old = self.water.bounds();
        if self.water.advance(self.now_ms, self.w, self.h) {
            self.damage(old);
            self.damage(self.water.bounds());
        }
        self.water.active()
    }

    // ------------------------------------------------------------------ time

    /// Periodic work: clock, caret blink, live app data. Returns ms until the next tick.
    pub fn tick(&mut self, now_ms: u64) -> u64 {
        self.now_ms = now_ms;
        self.step_items();
        self.last_tick = now_ms;
        let clock = bar::time_text();
        if clock != self.clock {
            self.clock = clock;
            self.crest_retarget();
            self.damage(Rect::new(self.w / 2, 0, self.w / 2, MENUBAR_H));
            if self.shore.is_some() {
                self.damage_all();
            }
        }
        // The caret on Shore blinks: repaint just its field.
        if self.shore.is_some() {
            match self.caret_area() {
                Some(r) => self.damage(r),
                None => self.damage_all(),
            }
        }
        let ids: Vec<u32> = self.windows.iter().filter(|w| !w.minimized).map(|w| w.id).collect();
        for id in ids {
            self.with_app(id, |app, _, env| app.tick(env));
        }
        // Tell apps when they gain or lose focus (caret blinking, etc.).
        let focus = self.focused_id();
        if focus != self.last_focus {
            if let Some(id) = focus {
                self.touch_mru(id);
            }
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
            self.windows.iter().filter(|w| w.awaiting_frame.is_some_and(|t| now_ms >= t + 600)).map(|w| w.id).collect();
        for id in late {
            self.reveal_window(id);
        }
        let mut busy = self.step_animations();
        busy |= self.step_water();
        busy |= self.step_notifications();
        busy |= self.step_bar();
        busy |= self.shore_moving();
        if self.shore.is_none() {
            self.update_harbor();
        }
        busy |= self.step_harbor();
        if busy {
            16
        } else {
            self.next_wakeup().saturating_sub(now_ms).max(1)
        }
    }

    // ------------------------------------------------------------- painting

    /// Merges pending damage into a small set of rectangles and clears it.
    pub fn take_damage(&mut self) -> Vec<Rect> {
        // Reconstruct the scene bottom-up before sampling any backdrop. Cursor-only
        // updates reuse the composited scene and never re-blur it.
        if self.scene_dirty && !self.partial_safe() {
            self.damage.clear();
            return alloc::vec![self.screen()];
        }
        let mut rects: Vec<Rect> = core::mem::take(&mut self.damage);
        if self.scene_dirty {
            // Damage touching glass grows to take in the whole (padded) glass.
            windowing::grow_damage(&mut rects, &self.material_rects(), 40);
            let screen = self.screen();
            for r in rects.iter_mut() {
                *r = r.intersect(&screen);
            }
            let area: i64 = rects.iter().map(|r| r.area()).sum();
            if rects.len() > 12 || area * 10 > screen.area() * 7 {
                return alloc::vec![screen];
            }
            return rects;
        }
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

    pub fn scene_changed(&self) -> bool {
        self.scene_dirty
    }
    pub fn finish_frame(&mut self) {
        self.scene_dirty = false;
    }
    /// Whether damage may be repainted piecemeal (`take_damage` grows it
    /// around glass). A dragged window damages exactly its old and new
    /// bounds, so drags qualify; overlays and animations repaint everything.
    fn partial_safe(&self) -> bool {
        let k = super::scale();
        !(self.scene.len() != (self.w * k * self.h * k) as usize
            || self.animating()
            || self.launcher.is_some() && self.shoal_backdrop.is_none()
            || self.menu.is_some()
            || self.items.menu.is_some()
            || self.items.rename.is_some()
            || self.center.is_some()
            || self.spotlight.is_some()
            || self.current.is_some()
            || self.shore_moving()
            || self.crest_moving()
            || self.harbor_moving()
            || self.harbor.peek.is_some()
            || self.dnd.is_some()
            || self.power.is_some()
            || self.switcher.is_some())
    }

    /// Every glass surface on screen (they sample what lies beneath them).
    fn material_rects(&self) -> Vec<Rect> {
        // The bar's glass is its bubbles, not the whole strip.
        let mut materials =
            alloc::vec![self.left_bubble().0, self.right_bubble().0, self.crest_area(), self.dock_rect()];
        materials.extend(self.shore_materials());
        for w in &self.windows {
            if w.minimized {
                continue;
            }
            materials.extend(w.app.materials().into_iter().map(|r| r.offset(w.content().x, w.content().y)));
        }
        materials
    }

    pub fn paint(&mut self, cv: &mut Canvas) {
        if let Some(cache) = cv.materials.as_deref_mut() {
            cache.set_moving(self.drag.is_some() || self.animating());
        }
        if !self.scene_dirty && self.scene.len() == cv.buf.len() {
            cv.blit_same(&self.scene, cv.clip);
            cursor::draw(cv, self.cursor.0, self.cursor.1);
            return;
        }
        cv.blit_same(&self.wallpaper, cv.clip);
        if self.shore.is_some() {
            self.paint_shore(cv);
            if self.shore.as_ref().is_some_and(|s| s.mode != shore::Mode::Setup) {
                self.paint_bar(cv);
            }
            if let Some(p) = self.power {
                self.paint_power_overlay(cv, p);
            }
            self.save_scene(cv);
            cursor::draw(cv, self.cursor.0, self.cursor.1);
            return;
        }
        let water_start = crate::time::now_ns();
        self.water.paint(cv, &self.wallpaper, self.water_coarse);
        if self.water.active() {
            let elapsed = crate::time::now_ns().saturating_sub(water_start);
            // Hysteresis avoids alternating between expensive and cheap frames.
            if elapsed > 8_000_000 {
                self.water_coarse = true;
            } else if elapsed < 4_000_000 {
                self.water_coarse = false;
            }
        }
        self.paint_items(cv);
        let focused = self.focused_id();
        let clip = cv.clip;
        for i in 0..self.windows.len() {
            let w = &self.windows[i];
            if w.minimized || w.animating || !w.bounds().intersects(&clip) {
                continue;
            }
            let is_focused = Some(self.windows[i].id) == focused;
            let env = self.env(is_focused);
            paint_window(cv, &mut self.windows[i], is_focused, &env, true);
        }
        self.paint_ghosts(cv);
        self.paint_handle(cv);
        self.paint_moorings(cv);
        self.paint_shoal(cv);
        self.paint_harbor(cv);
        self.paint_bar(cv);
        self.paint_menu(cv);
        self.paint_center(cv);
        self.paint_spotlight(cv);
        self.paint_drag(cv);
        self.paint_switcher(cv);
        self.paint_item_overlay(cv);
        if let Some(p) = self.power {
            self.paint_power_overlay(cv, p);
        }
        self.save_scene(cv);
        cursor::draw(cv, self.cursor.0, self.cursor.1);
    }

    /// Keeps the composed scene (without the pointer) under the clip, to
    /// redraw the pointer alone over it.
    fn save_scene(&mut self, cv: &Canvas) {
        self.scene.resize(cv.buf.len(), 0);
        let clip = cv.pclip();
        for y in clip.y..clip.bottom() {
            let start = (y * cv.width + clip.x) as usize;
            let end = start + clip.w as usize;
            self.scene[start..end].copy_from_slice(&cv.buf[start..end]);
        }
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

fn paint_window(cv: &mut Canvas, w: &mut Window, focused: bool, env: &Env, shadow: bool) {
    let t = theme::current();
    let r = w.rect;
    if shadow {
        let strength = if focused { t.shadow } else { t.shadow * 6 / 10 };
        cv.shadow(r, WINDOW_RADIUS, SHADOW_BLUR, if focused { SHADOW_OFFSET } else { 6 }, strength);
    }
    let regions = w.app.materials();
    if regions.is_empty() && w.app.covers_window() {
        // The app paints the inside; only the anti-aliased corners blend
        // over the window colour.
        for band in
            [Rect::new(r.x, r.y, r.w, WINDOW_RADIUS), Rect::new(r.x, r.bottom() - WINDOW_RADIUS, r.w, WINDOW_RADIUS)]
        {
            cv.with_clip(band, |cv| cv.fill_round_rect(r, WINDOW_RADIUS, t.window_bg));
        }
    } else if regions.is_empty() || !shadow {
        cv.fill_round_rect(r, WINDOW_RADIUS, t.window_bg);
    } else {
        for region in regions {
            let region = region.offset(r.x, r.y).intersect(&r);
            cv.frosted(region, 12, t.glass_tint);
        }
    }
    // App content: the whole window, its corners rounded.
    cv.with_clip(r, |cv| w.app.draw(cv, r, env));
    if focused && shadow {
        cv.stroke_round_rect(r, WINDOW_RADIUS, if t.dark { 0x28FF_FFFF } else { 0x200E_1530 });
        handle::paint_waterline(cv, r, WINDOW_RADIUS);
    } else {
        cv.stroke_round_rect(r, WINDOW_RADIUS, t.window_border);
    }
}
