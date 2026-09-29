//! Ripple — the WaveOS Aurora UI toolkit for user-space apps.
//!
//! An app implements [`App`] and calls [`run`]. Ripple creates the window,
//! draws the app into its shared surface with the Aurora drawing library
//! (`aurora-gfx`, the same code the window server uses), presents it, and
//! turns window-server events into `key`/`click`/`hover`/… calls.

#![no_std]

extern crate alloc;

pub mod text;

pub use aurora::abi::input::{KeyCode, KeyEvent, Modifiers};
pub use aurora_gfx::{canvas, font, geom, icons, math, theme, wallpaper, widgets};

use alloc::string::String;
use alloc::vec::Vec;
use aurora::abi::{event, nr, win, Event, SurfaceInfo};
use aurora::sys::{call, str_args};
use canvas::Canvas;
use geom::Rect;

/// Things an app can ask the system to do.
pub enum Request {
    /// Open an app by name ("About") or program path.
    OpenApp(String),
    /// Open a document with its default app.
    OpenFile(String),
    /// Close this window (and exit the app).
    Close,
    Shutdown,
    Reboot,
    SetDark(bool),
    SetWallpaper(u8),
    /// Use a picture file as the wallpaper.
    SetWallpaperImage(String),
}

pub struct Env {
    pub now_ms: u64,
    pub focused: bool,
    pub screen: (i32, i32),
    pub requests: Vec<Request>,
    /// Modifier keys currently held (as last reported by the keyboard).
    pub mods: Modifiers,
}

pub trait App {
    fn title(&self) -> String;
    /// Initial content size.
    fn size(&self) -> (i32, i32);
    fn resizable(&self) -> bool {
        true
    }
    /// Draws the whole content area.
    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env);
    /// Handlers return true when the app needs to be redrawn.
    fn key(&mut self, _ev: &KeyEvent, _env: &mut Env) -> bool {
        false
    }
    fn click(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    fn double_click(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    /// Secondary (right) click — e.g. to show a context menu.
    fn right_click(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    fn release(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    /// Pointer moved inside the window (`x < 0` when it left).
    fn hover(&mut self, _x: i32, _y: i32, _area: Rect) -> bool {
        false
    }
    /// Pointer moved with the left button held since a press in this window.
    /// Coordinates may lie outside the window.
    fn drag(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        self.hover(x, y, area)
    }
    fn scroll(&mut self, _delta: i32, _area: Rect) -> bool {
        false
    }
    /// Something is dragged over the window (`x < 0`: it left). Return true to redraw.
    fn drag_over(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    /// Something was dropped at (x, y); read it with `aurora::dnd::dropped(kind)`.
    fn drop(&mut self, _x: i32, _y: i32, _kind: u32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    /// A drag that started in this window ended (`dropped` = it landed somewhere).
    fn drag_end(&mut self, _dropped: bool, _env: &mut Env) -> bool {
        false
    }
    /// The close button (or Ctrl+W) was used. Return false to keep the window
    /// open, e.g. to ask about unsaved changes (then push `Request::Close`).
    fn close_requested(&mut self, _env: &mut Env) -> bool {
        true
    }
    /// Called about ten times a second.
    fn tick(&mut self, _env: &mut Env) -> bool {
        false
    }
    /// Lets apps do background work (e.g. reading a pipe) between events.
    /// Return the longest time (ms) Ripple may sleep before calling `tick` again.
    fn tick_interval(&self) -> u64 {
        100
    }
}

struct Window {
    id: u64,
    surface: SurfaceInfo,
}

impl Window {
    fn surface(id: u64) -> SurfaceInfo {
        let mut s = SurfaceInfo::default();
        let _ = call(nr::WIN_SURFACE, &[id, &mut s as *mut SurfaceInfo as u64]);
        s
    }

    fn pixels(&mut self) -> &mut [u32] {
        let s = &self.surface;
        unsafe { core::slice::from_raw_parts_mut(s.addr as *mut u32, (s.stride * s.height) as usize) }
    }

    fn present(&self) {
        let s = &self.surface;
        let _ = call(nr::WIN_PRESENT, &[self.id, 0, 0, s.width as u64, s.height as u64]);
    }

    fn set_title(&self, title: &str) {
        let [p, l] = str_args(title);
        let _ = call(nr::WIN_SET_TITLE, &[self.id, p, l]);
    }
}

fn apply(requests: Vec<Request>) {
    use aurora::process::desktop;
    for r in requests {
        match r {
            Request::OpenApp(name) => {
                let _ = desktop::open_app(&name);
            }
            Request::OpenFile(path) => {
                let _ = desktop::open_file(&path);
            }
            Request::Close => aurora::process::exit(0),
            Request::Shutdown => desktop::shutdown(),
            Request::Reboot => desktop::reboot(),
            Request::SetDark(d) => desktop::set_dark(d),
            Request::SetWallpaper(i) => desktop::set_wallpaper(i),
            Request::SetWallpaperImage(p) => {
                let _ = desktop::set_wallpaper_image(&p);
            }
        }
    }
}

/// Installs the system's TrueType faces (once per process).
pub fn load_fonts() {
    use font::Face;
    for face in [Face::Regular, Face::SemiBold, Face::Mono] {
        if !font::installed(face) {
            if let Ok(data) = aurora::fs::read(face.file()) {
                font::install(face, alloc::boxed::Box::leak(data.into_boxed_slice()));
            }
        }
    }
}

/// Runs `app` until its window is closed. Returns the process exit code.
pub fn run<A: App>(mut app: A) -> i32 {
    load_fonts();
    let (dark, wallpaper, accent) = aurora::process::desktop::theme();
    theme::set_dark(dark);
    theme::set_wallpaper(wallpaper);
    theme::set_accent(accent);
    let info = aurora::process::sys_info();
    let screen = (info.screen_w as i32, info.screen_h as i32);

    let (w, h) = app.size();
    let mut title = app.title();
    let [tp, tl] = str_args(&title);
    let flags = if app.resizable() { win::RESIZABLE } else { 0 };
    let id = match call(nr::WIN_CREATE, &[w as u64, h as u64, tp, tl, flags as u64]) {
        Ok(id) => id,
        Err(e) => {
            aurora::eprintln!("could not create window: {}", e);
            return 1;
        }
    };
    let mut win = Window { id, surface: Window::surface(id) };
    let mut env = Env {
        now_ms: aurora::time::uptime_ms(),
        focused: true,
        screen,
        requests: Vec::new(),
        mods: Modifiers::default(),
    };
    let mut dirty = true;
    let mut next_tick = 0;

    loop {
        env.now_ms = aurora::time::uptime_ms();
        if env.now_ms >= next_tick {
            dirty |= app.tick(&mut env);
            next_tick = env.now_ms + app.tick_interval();
        }
        if dirty {
            let (sw, sh) = (win.surface.width as i32, win.surface.height as i32);
            let stride = win.surface.stride as i32;
            let area = Rect::new(0, 0, sw, sh);
            let mut cv = Canvas::new(win.pixels(), stride, sh);
            cv.fill_rect(area, theme::current().window_bg);
            app.draw(&mut cv, area, &env);
            win.present();
            dirty = false;
        }
        let new_title = app.title();
        if new_title != title {
            win.set_title(&new_title);
            title = new_title;
        }
        apply(core::mem::take(&mut env.requests));

        let timeout = next_tick.saturating_sub(aurora::time::uptime_ms()).max(1);
        let mut ev = Event::default();
        if call(nr::NEXT_EVENT, &[&mut ev as *mut Event as u64, timeout]).unwrap_or(0) == 0 {
            continue;
        }
        env.now_ms = aurora::time::uptime_ms();
        if matches!(
            ev.kind,
            event::KEY | event::POINTER_DOWN | event::POINTER_UP | event::POINTER_MOVE | event::DRAG_OVER | event::DROP
        ) {
            env.mods = Modifiers::from_bits(ev.d);
        }
        let area = Rect::new(0, 0, win.surface.width as i32, win.surface.height as i32);
        dirty |= match ev.kind {
            event::KEY => app.key(&KeyEvent::from_event(&ev), &mut env),
            event::POINTER_DOWN if ev.a == 2 => app.right_click(ev.x, ev.y, area, &mut env),
            event::POINTER_DOWN if ev.b >= 2 => app.double_click(ev.x, ev.y, area, &mut env),
            event::POINTER_DOWN => app.click(ev.x, ev.y, area, &mut env),
            event::POINTER_UP => app.release(ev.x, ev.y, area, &mut env),
            event::POINTER_MOVE if ev.a != 0 => app.drag(ev.x, ev.y, area, &mut env),
            event::POINTER_MOVE => app.hover(ev.x, ev.y, area),
            event::SCROLL => app.scroll(ev.a as i32, area),
            event::DRAG_OVER => app.drag_over(ev.x, ev.y, area, &mut env),
            event::DRAG_LEAVE => app.drag_over(-1, -1, area, &mut env),
            event::DROP => app.drop(ev.x, ev.y, ev.a, area, &mut env),
            event::DRAG_END => app.drag_end(ev.a != 0, &mut env),
            event::RESIZE => {
                if (ev.x as u32, ev.y as u32) != (win.surface.width, win.surface.height) {
                    let _ = call(nr::WIN_RESIZE, &[id, ev.x as u64, ev.y as u64]);
                    win.surface = Window::surface(id);
                }
                true
            }
            event::FOCUS => {
                env.focused = ev.a != 0;
                true
            }
            event::THEME => {
                theme::set_dark(ev.a != 0);
                theme::set_wallpaper(ev.b as u8);
                theme::set_accent(ev.c as u8);
                true
            }
            event::SCREEN => {
                env.screen = (ev.x, ev.y);
                true
            }
            event::CLOSE_REQUESTED => {
                if app.close_requested(&mut env) {
                    let _ = call(nr::WIN_CLOSE, &[id]);
                    return 0;
                }
                true
            }
            _ => false,
        };
        apply(core::mem::take(&mut env.requests));
    }
}

/// Hit-test helper: which of `rects` contains the point.
pub fn hit(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects.iter().position(|r| r.contains(x, y))
}
