//! AuroraKit — the WaveOS Aurora UI toolkit for user-space apps.
//!
//! An app implements [`App`] and calls [`run`]. AuroraKit creates the window,
//! draws the app into its shared surface with the Aurora drawing library
//! (`lumen`, the same code the window server uses), presents it, and
//! turns window-server events into `key`/`click`/`hover`/… calls.

#![no_std]

extern crate alloc;

pub mod text;
pub use lumen::ui;

fn refresh_effects() {
    use corekit::{abi::pref, prefs};
    let flags = [pref::REDUCE_TRANSPARENCY, pref::REDUCE_MOTION, pref::HIGH_CONTRAST, pref::DOCK_AUTOHIDE]
        .iter()
        .enumerate()
        .fold(0, |flags, (i, key)| flags | ((prefs::get_bool(key) as u8) << i));
    theme::set_effects(prefs::get_int(pref::GLASS, 70) as u8, flags);
    theme::set_dock_size(prefs::get_int(pref::DOCK_SIZE, 48) as u8);
}

pub use corekit::abi::input::{KeyCode, KeyEvent, Modifiers};
pub use lumen::{canvas, font, geom, icons, math, theme, wallpaper, widgets};

use alloc::string::String;
use alloc::vec::Vec;
use canvas::Canvas;
use corekit::abi::{event, nr, win, Event, SurfaceInfo};
use corekit::sys::{call, str_args};
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
    /// Opt-in transparent regions, in window-local coordinates. Opaque is the default.
    fn materials(&self, _area: Rect) -> Vec<corekit::abi::MaterialRegion> {
        Vec::new()
    }
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
    /// Scrolling with its precise amounts: `notches` whole wheel notches
    /// down, and `dx`, `dy` in 1/120 notch (touchpads move by fractions).
    /// By default it calls [`App::scroll`] with the notches.
    fn wheel(&mut self, notches: i32, _dx: i32, _dy: i32, area: Rect) -> bool {
        notches != 0 && self.scroll(notches, area)
    }
    /// Something is dragged over the window (`x < 0`: it left). Return true to redraw.
    fn drag_over(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        false
    }
    /// Something was dropped at (x, y); read it with `corekit::dnd::dropped(kind)`.
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
    /// Ctrl+W. By default it closes the window; tabbed apps can close a tab.
    fn close_tab_requested(&mut self, env: &mut Env) -> bool {
        self.close_requested(env)
    }
    /// Called about ten times a second.
    fn tick(&mut self, _env: &mut Env) -> bool {
        false
    }
    /// Lets apps do background work (e.g. reading a pipe) between events.
    /// Return the longest time (ms) AuroraKit may sleep before calling `tick` again.
    fn tick_interval(&self) -> u64 {
        100
    }
    /// Absolute uptime deadline. None retains the legacy periodic tick.
    /// Called after each tick and input event; implementations must not
    /// postpone an existing deadline just because the pointer moved.
    fn next_wakeup(&self, _env: &Env) -> Option<u64> {
        None
    }
}

struct Window {
    id: u64,
    surface: SurfaceInfo,
    /// What the surface shows (the compositor may read it at any time).
    previous: Vec<u32>,
    previous_size: (u32, u32),
    /// Where frames are drawn; changes reach the surface only in `present`,
    /// so a window is never shown half-drawn.
    back: Vec<u32>,
}

impl Window {
    fn surface(id: u64) -> SurfaceInfo {
        let mut s = SurfaceInfo::default();
        let _ = call(nr::WIN_SURFACE, &[id, &mut s as *mut SurfaceInfo as u64]);
        s.scale = s.scale.max(1);
        s
    }

    /// The window's content size in points.
    fn points(&self) -> (i32, i32) {
        ((self.surface.width / self.surface.scale) as i32, (self.surface.height / self.surface.scale) as i32)
    }

    fn pixels(&mut self) -> &mut [u32] {
        let n = (self.surface.stride * self.surface.height) as usize;
        if self.back.len() != n {
            self.back.clear();
            self.back.resize(n, 0);
        }
        &mut self.back
    }

    fn present(&mut self) {
        let s = self.surface;
        let n = (s.stride * s.height) as usize;
        if self.back.len() != n {
            return;
        }
        let resized = self.previous_size != (s.width, s.height) || self.previous.len() != n;
        let rect = lumen::canvas::changed_bounds(
            &self.previous,
            &self.back,
            s.width as i32,
            s.height as i32,
            s.stride as i32,
            resized,
        );
        if resized {
            self.previous.clear();
            self.previous.resize(n, 0);
        }
        self.previous_size = (s.width, s.height);
        let Some(r) = rect else { return };
        let surface = unsafe { core::slice::from_raw_parts_mut(s.addr as *mut u32, n) };
        for y in r.y..r.bottom() {
            let at = (y as u32 * s.stride + r.x as u32) as usize;
            let row = at..at + r.w as usize;
            surface[row.clone()].copy_from_slice(&self.back[row.clone()]);
            self.previous[row.clone()].copy_from_slice(&self.back[row]);
        }
        let _ = call(nr::WIN_PRESENT, &[self.id, r.x as u64, r.y as u64, r.w as u64, r.h as u64]);
    }

    fn set_title(&self, title: &str) {
        let [p, l] = str_args(title);
        let _ = call(nr::WIN_SET_TITLE, &[self.id, p, l]);
    }
}

fn apply(requests: Vec<Request>) {
    use corekit::process::desktop;
    for r in requests {
        match r {
            Request::OpenApp(name) => {
                let _ = desktop::open_app(&name);
            }
            Request::OpenFile(path) => {
                let _ = desktop::open_file(&path);
            }
            Request::Close => corekit::process::exit(0),
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
    for face in [Face::Regular, Face::SemiBold, Face::Bold, Face::Mono] {
        if !font::installed(face) {
            if let Ok(data) = corekit::fs::read(face.file()) {
                font::install(face, alloc::boxed::Box::leak(data.into_boxed_slice()));
            }
        }
    }
}

/// Decodes an app icon picture for [`lumen::icons`].
fn load_icon(path: &str) -> Option<(alloc::vec::Vec<u32>, u32, u32)> {
    let data = corekit::fs::read(path).ok()?;
    let img = aurora_image::decode(&data).ok()?;
    Some((img.pixels, img.width, img.height))
}

/// Runs `app` until its window is closed. Returns the process exit code.
pub fn run<A: App>(mut app: A) -> i32 {
    refresh_effects();
    load_fonts();
    lumen::icons::set_loader(load_icon);
    let (dark, wallpaper, accent) = corekit::process::desktop::theme();
    theme::set_dark(dark);
    theme::set_wallpaper(wallpaper);
    theme::set_accent(accent);
    let custom = corekit::process::desktop::custom_accent();
    if custom != 0 {
        theme::set_custom_tones(theme::tones_from_fill(custom));
    }
    let info = corekit::process::sys_info();
    let screen = (info.screen_w as i32, info.screen_h as i32);

    let (w, h) = app.size();
    let mut title = app.title();
    let [tp, tl] = str_args(&title);
    let flags = if app.resizable() { win::RESIZABLE } else { 0 };
    let id = match call(nr::WIN_CREATE, &[w as u64, h as u64, tp, tl, flags as u64]) {
        Ok(id) => id,
        Err(e) => {
            corekit::eprintln!("could not create window: {}", e);
            return 1;
        }
    };
    let mut win =
        Window { id, surface: Window::surface(id), previous: Vec::new(), previous_size: (0, 0), back: Vec::new() };
    let mut env = Env {
        now_ms: corekit::time::uptime_ms(),
        focused: true,
        screen,
        requests: Vec::new(),
        mods: Modifiers::default(),
    };
    let mut dirty = true;
    let mut next_tick = 0;
    // Opt-in validation counters; ordinary sessions do not collect these.
    let metrics = corekit::fs::exists("/Settings/runtime-metrics");
    let mut sample_at = env.now_ms;
    let mut ticks = 0u64;
    let mut events = 0u64;

    loop {
        env.now_ms = corekit::time::uptime_ms();
        if env.now_ms >= next_tick {
            ticks += 1;
            dirty |= app.tick(&mut env);
            next_tick = app.next_wakeup(&env).unwrap_or(env.now_ms + app.tick_interval());
        }
        if metrics && env.now_ms.saturating_sub(sample_at) >= 10_000 {
            let stats = corekit::process::sys_stats();
            let cpu: u64 = stats.cpu.iter().map(|c| c.busy_ms).sum();
            if let Some(p) =
                corekit::process::list().into_iter().find(|p| p.user == 1 && p.pid == corekit::process::pid())
            {
                corekit::eprintln!(
                    "runtime-process: {}, cpu_ms={}, memory_kib={}, threads={}",
                    title,
                    p.cpu_ms,
                    p.mem_kib,
                    p.threads
                );
            }
            corekit::eprintln!(
                "runtime: {}, elapsed={}, ticks={}, events={}, cpu_ms={}, memory={}, tasks={}, frames={}",
                title,
                env.now_ms - sample_at,
                ticks,
                events,
                cpu,
                stats.mem_used,
                stats.tasks,
                stats.frames
            );
            sample_at = env.now_ms;
            ticks = 0;
            events = 0;
        }
        if dirty {
            // The app draws in points; the surface is in device pixels.
            let (pw, ph) = win.points();
            let (sh, stride, scale) = (win.surface.height as i32, win.surface.stride as i32, win.surface.scale as i32);
            let area = Rect::new(0, 0, pw, ph);
            let materials = app.materials(area);
            let _ = call(nr::WIN_MATERIALS, &[id, materials.as_ptr() as u64, materials.len() as u64]);
            let mut cv = Canvas::new(win.pixels(), stride, sh).with_scale(scale);
            cv.fill_rect(area, theme::current().window_bg);
            for region in &materials {
                let r = cv.to_px(Rect::new(region.x, region.y, region.width, region.height).intersect(&area));
                for y in r.y..r.bottom() {
                    let start = (y * stride + r.x) as usize;
                    cv.buf[start..start + r.w as usize].fill(0);
                }
            }
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

        // Layout can reveal an offscreen focus target and request another
        // frame; honor that deadline before sleeping as well as after input.
        if let Some(deadline) = app.next_wakeup(&env) {
            next_tick = deadline;
        }
        let timeout = next_tick.saturating_sub(corekit::time::uptime_ms()).max(1);
        let mut ev = Event::default();
        if call(nr::NEXT_EVENT, &[&mut ev as *mut Event as u64, timeout]).unwrap_or(0) == 0 {
            continue;
        }
        env.now_ms = corekit::time::uptime_ms();
        events += 1;
        if matches!(
            ev.kind,
            event::KEY | event::POINTER_DOWN | event::POINTER_UP | event::POINTER_MOVE | event::DRAG_OVER | event::DROP
        ) {
            env.mods = Modifiers::from_bits(ev.d);
        }
        let (pw, ph) = win.points();
        let area = Rect::new(0, 0, pw, ph);
        dirty |= match ev.kind {
            event::KEY => app.key(&KeyEvent::from_event(&ev), &mut env),
            event::POINTER_DOWN if ev.a == 2 => app.right_click(ev.x, ev.y, area, &mut env),
            event::POINTER_DOWN if ev.b >= 2 => app.double_click(ev.x, ev.y, area, &mut env),
            event::POINTER_DOWN => app.click(ev.x, ev.y, area, &mut env),
            event::POINTER_UP => app.release(ev.x, ev.y, area, &mut env),
            event::POINTER_MOVE if ev.a != 0 => app.drag(ev.x, ev.y, area, &mut env),
            event::POINTER_MOVE => app.hover(ev.x, ev.y, area),
            event::SCROLL => app.wheel(ev.a as i32, ev.x, ev.y, area),
            event::DRAG_OVER => app.drag_over(ev.x, ev.y, area, &mut env),
            event::DRAG_LEAVE => app.drag_over(-1, -1, area, &mut env),
            event::DROP => app.drop(ev.x, ev.y, ev.a, area, &mut env),
            event::DRAG_END => app.drag_end(ev.a != 0, &mut env),
            // A new size, or a new scale (`a`): a new surface.
            event::RESIZE => {
                if (ev.x, ev.y) != win.points() || (ev.a != 0 && ev.a != win.surface.scale) {
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
                refresh_effects();
                theme::set_dark(ev.a != 0);
                theme::set_wallpaper(ev.b as u8);
                theme::set_accent(ev.c as u8);
                if ev.d != 0 {
                    theme::set_custom_tones(theme::tones_from_fill(ev.d));
                }
                true
            }
            event::SCREEN => {
                env.screen = (ev.x, ev.y);
                true
            }
            event::CLOSE_REQUESTED => {
                if if ev.a == 1 { app.close_tab_requested(&mut env) } else { app.close_requested(&mut env) } {
                    let _ = call(nr::WIN_CLOSE, &[id]);
                    return 0;
                }
                true
            }
            _ => false,
        };
        apply(core::mem::take(&mut env.requests));
        if let Some(deadline) = app.next_wakeup(&env) {
            next_tick = deadline;
        }
    }
}

/// Hit-test helper: which of `rects` contains the point.
pub fn hit(rects: &[Rect], x: i32, y: i32) -> Option<usize> {
    rects.iter().position(|r| r.contains(x, y))
}
