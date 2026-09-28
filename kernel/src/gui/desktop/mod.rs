//! The Aurora desktop: window management, input routing and scene painting.
//!
//! Rendering is damage-driven: every state change records the screen
//! rectangles it affects, and `render` repaints only those, back to front:
//! wallpaper → windows → dock → menu bar → launcher → menus → cursor.

mod shell;

use super::apps::{self, App, AppKind, Env, Request};
use super::canvas::Canvas;
use super::cursor;
use super::geom::Rect;
use super::theme::{self, MENUBAR_H, TITLEBAR_H, WINDOW_RADIUS};
use super::wallpaper;
use crate::drivers::input::{InputEvent, KeyCode, KeyEvent, BUTTON_LEFT};
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

pub use shell::{Launcher, Menu};

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
        };
        d.open(AppKind::Welcome);
        d.damage_all();
        d
    }

    fn make_wallpaper(w: i32, h: i32) -> (Vec<u32>, Vec<u32>) {
        let wp = wallpaper::generate(theme::wallpaper(), w, h);
        let blurred = wallpaper::blur(&wp, w, h, 18);
        (wp, blurred)
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
        let app = apps::create(kind);
        if app.single_instance() {
            if let Some(id) = self.windows.iter().find(|w| w.app.kind() == kind).map(|w| w.id) {
                self.raise(id);
                return;
            }
        }
        self.add_window(app);
    }

    fn add_window(&mut self, app: Box<dyn App>) {
        let (cw, ch) = app.size();
        let wa = self.work_area();
        let w = cw.min(wa.w);
        let h = (ch + TITLEBAR_H).min(wa.h);
        let cascade = (self.windows.len() as i32 % 6) * 28;
        let x = wa.x + (wa.w - w) / 2 + cascade - 56;
        let y = wa.y + (wa.h - h) / 3 + cascade - 28;
        let rect = Rect::new(x.max(wa.x), y.max(wa.y), w, h);
        let id = self.next_id;
        self.next_id += 1;
        let prev = self.focused_id();
        self.windows.push(Window { id, app, rect, minimized: false, saved: None });
        if let Some(p) = prev {
            self.damage_window(p);
        }
        self.damage_window(id);
        self.damage_shell();
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
        w.minimized = false;
        self.windows.push(w);
        if let Some(p) = prev {
            self.damage_window(p);
        }
        self.damage_window(id);
        self.damage_shell();
    }

    pub fn close(&mut self, id: u32) {
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
            match w.saved.take() {
                Some(r) => w.rect = r,
                None => {
                    w.saved = Some(w.rect);
                    w.rect = wa;
                }
            }
            self.damage_window(id);
        }
    }

    // ------------------------------------------------------------- requests

    fn apply(&mut self, source: Option<u32>, env: Env) {
        for req in env.requests {
            match req {
                Request::Open(kind) => self.open(kind),
                Request::OpenFile(path) => self.add_window(apps::open_file(&path)),
                Request::Close => {
                    if let Some(id) = source {
                        self.close(id);
                    }
                }
                Request::Shutdown => self.power = Some(PowerAction::Shutdown),
                Request::Reboot => self.power = Some(PowerAction::Reboot),
                Request::SetDark(d) => {
                    theme::set_dark(d);
                    self.damage_all();
                }
                Request::SetWallpaper(i) => {
                    theme::set_wallpaper(i);
                    let (wp, bl) = Self::make_wallpaper(self.w, self.h);
                    self.wallpaper = wp;
                    self.blurred = bl;
                    self.damage_all();
                }
            }
        }
        if self.power.is_some() {
            self.damage_all();
        }
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
        if released & BUTTON_LEFT != 0 {
            self.drag = None;
        }
        if wheel != 0 {
            if let Some(w) = self.window_at(x, y) {
                let id = self.windows[w].id;
                self.with_app(id, |app, area, _| app.scroll(wheel as i32, area));
            }
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<usize> {
        self.windows.iter().rposition(|w| !w.minimized && w.rect.contains(x, y))
    }

    fn pointer_moved(&mut self, x: i32, y: i32) {
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
                Some(0) => self.close(id),
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
            self.with_app(id, |app, area, env| app.double_click(x, y, area, env));
        } else {
            self.with_app(id, |app, area, env| app.click(x, y, area, env));
        }
    }

    fn key(&mut self, k: KeyEvent) {
        if k.pressed {
            // Global shortcuts.
            if k.code == KeyCode::Super || (k.mods.ctrl && k.code == KeyCode::Escape) {
                self.toggle_launcher();
                return;
            }
            if k.mods.alt && k.code == KeyCode::Tab {
                self.cycle_windows();
                return;
            }
            if (k.mods.alt && k.code == KeyCode::F(4)) || (k.mods.ctrl && matches!(k.ch, Some('w') | Some('W'))) {
                if let Some(id) = self.focused_id() {
                    self.close(id);
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
        100
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
            if self.windows[i].minimized || !self.windows[i].bounds().intersects(&clip) {
                continue;
            }
            let is_focused = Some(self.windows[i].id) == focused;
            let env = self.env(is_focused);
            let hover = self.traffic_hover == Some(self.windows[i].id);
            paint_window(cv, &mut self.windows[i], is_focused, hover, &env);
        }
        self.paint_shell(cv);
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

fn paint_window(cv: &mut Canvas, w: &mut Window, focused: bool, hover: bool, env: &Env) {
    let t = theme::current();
    let r = w.rect;
    let strength = if focused { t.shadow } else { t.shadow * 6 / 10 };
    cv.shadow(r, WINDOW_RADIUS, SHADOW_BLUR, if focused { SHADOW_OFFSET } else { 6 }, strength);
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
