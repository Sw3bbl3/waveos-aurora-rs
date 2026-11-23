//! The displays Lumen Server shows the desktop on, and its frame loop.
//!
//! [`Compositor`] opens every connected display of every device
//! ([`Output::open_all`]) and lays them out in one space of points
//! (`lumen::displays`): the main display at (0, 0) with the bar and Harbor,
//! the others beside it as Settings arranged them (each display's place,
//! resolution, scale and wallpaper are preferences named after its EDID
//! identity, so they come back with the monitor). Each display has its own
//! mode, scale, back buffer and damage: a frame paints, for every display,
//! the part of the desktop's damage that falls on it, with a canvas whose
//! origin is the display's corner.
//!
//! With one display, presenting happens right after painting, as it always
//! did. With several, each display presents from a task of its own
//! ([`Presenter`]): a display that is slow (or stops answering) skips
//! frames — its damage waits until it has taken the last picture — and the
//! others carry on. Mirroring shows the main display's picture on every
//! display (copied, not painted again).
//!
//! Hotplug (`gpu::hotplug`, a host window resized) and preference changes
//! run [`Compositor::reconfigure`]: displays that went away are switched
//! off and their windows move to the main display (`Desktop::set_displays`,
//! which brings them back when the display returns).

use super::desktop::{Desktop, DisplayArea, PowerAction};
use super::{boot, cursor, notify, prefs, server, settings, stages, PointerOn};
use crate::drivers::gpu::{self, output::Output, ConnectorInfo, Damage, Framebuffer, KmsDevice, Status};
use crate::drivers::input;
use crate::sync::IrqMutex;
use crate::{sched, time};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::{display_info, DisplayInfo, DisplayMode};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use lumen::canvas::Canvas;
use lumen::displays::{self, Record};
use lumen::geom::Rect;

// ------------------------------------------------------------ presenting

/// Presents one display's damage from a task of its own, so a slow display
/// holds up nobody but itself.
pub struct Presenter {
    dev: Arc<dyn KmsDevice>,
    queue: IrqMutex<Queue>,
    /// The task presenting (set by the task itself).
    task: AtomicU64,
    /// Presents done since the start.
    pub total: AtomicU64,
    /// Presents done, their total and longest time, and pixels (since the
    /// last statistics line).
    pub presents: AtomicU64,
    pub ns: AtomicU64,
    pub max_ns: AtomicU64,
    pub pixels: AtomicU64,
    /// Self-tests: every present takes this much longer (a slow display).
    pub delay_ms: AtomicU64,
}

#[derive(Default)]
struct Queue {
    fb: Option<Arc<Framebuffer>>,
    rects: Vec<Damage>,
    busy: bool,
    quit: bool,
}

/// Presenters waiting for their task to pick them up.
static STARTING: IrqMutex<Vec<Arc<Presenter>>> = IrqMutex::new(Vec::new());

impl Presenter {
    fn start(dev: Arc<dyn KmsDevice>) -> Arc<Presenter> {
        let p = Arc::new(Presenter {
            dev,
            queue: IrqMutex::new(Queue::default()),
            task: AtomicU64::new(u64::MAX),
            total: AtomicU64::new(0),
            presents: AtomicU64::new(0),
            ns: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
            pixels: AtomicU64::new(0),
            delay_ms: AtomicU64::new(0),
        });
        STARTING.lock().push(p.clone());
        sched::spawn("present", present_task);
        p
    }

    fn wake(&self) {
        let t = self.task.load(Ordering::Relaxed);
        if t != u64::MAX {
            sched::wake(t);
        }
    }

    /// Still showing the last picture, or with one waiting.
    pub fn busy(&self) -> bool {
        let q = self.queue.lock();
        q.busy || !q.rects.is_empty()
    }

    fn submit(&self, fb: Arc<Framebuffer>, rects: &[Damage]) {
        {
            let mut q = self.queue.lock();
            q.fb = Some(fb);
            q.rects.extend_from_slice(rects);
            if q.rects.len() > 16 {
                let all = q.rects.iter().fold(None::<Damage>, |acc, r| {
                    Some(match acc {
                        None => *r,
                        Some(a) => {
                            let (x, y) = (a.x.min(r.x), a.y.min(r.y));
                            let (rx, by) = ((a.x + a.w).max(r.x + r.w), (a.y + a.h).max(r.y + r.h));
                            Damage { x, y, w: rx - x, h: by - y }
                        }
                    })
                });
                q.rects = all.into_iter().collect();
            }
        }
        self.wake();
    }

    /// Waits up to `ms` for the display to take what it was given.
    fn wait_idle(&self, ms: u64) -> bool {
        let end = time::uptime_ms() + ms;
        while self.busy() {
            if time::uptime_ms() >= end {
                return false;
            }
            sched::sleep_ms(1);
        }
        true
    }

    fn stop(&self) {
        self.queue.lock().quit = true;
        self.wake();
    }
}

fn present_task() {
    // Whichever presenter is waiting: from now on this task is its.
    let Some(p) = STARTING.lock().pop() else { return };
    p.task.store(sched::current_id(), Ordering::Relaxed);
    loop {
        sched::wait_until(1000, || {
            let q = p.queue.lock();
            q.quit || !q.rects.is_empty()
        });
        let (fb, rects) = {
            let mut q = p.queue.lock();
            if q.quit {
                return;
            }
            let Some(fb) = q.fb.clone() else {
                q.rects.clear();
                continue;
            };
            if q.rects.is_empty() {
                continue;
            }
            q.busy = true;
            (fb, core::mem::take(&mut q.rects))
        };
        let started = time::now_ns();
        let delay = p.delay_ms.load(Ordering::Relaxed);
        if delay > 0 {
            sched::sleep_ms(delay);
        }
        if let Err(e) = p.dev.dirty(&fb, &rects) {
            log!("gui", "present failed on {}: {:?}", p.dev.name(), e);
        }
        let elapsed = time::now_ns() - started;
        p.total.fetch_add(1, Ordering::Relaxed);
        p.presents.fetch_add(1, Ordering::Relaxed);
        p.ns.fetch_add(elapsed, Ordering::Relaxed);
        p.max_ns.fetch_max(elapsed, Ordering::Relaxed);
        p.pixels.fetch_add(rects.iter().map(|r| r.w as u64 * r.h as u64).sum(), Ordering::Relaxed);
        p.queue.lock().busy = false;
        // Damage the compositor held back for this display can go now.
        server::poke();
    }
}

// ------------------------------------------------------------- displays

/// One display the desktop uses.
pub struct Screen {
    pub out: Output,
    /// Its id for apps (the connector's object id: unique, never 0).
    pub id: u32,
    /// Identity hash (EDID and connector), what its preferences are named after.
    pub key: u32,
    pub name: String,
    pub connector: String,
    /// Where it is, in points (mirrors: the main display's place).
    pub rect: Rect,
    pub scale: i32,
    pub record: Record,
    /// Shows the main display's picture.
    pub mirror: bool,
    /// Its own resolution while it mirrors at the main display's.
    unmirrored: Option<(u32, u32)>,
    pub mm: (u32, u32),
    materials: lumen::material::MaterialCache,
    cursor_shown: Option<(u32, u64, i32)>,
    /// The pointer is on this display's cursor plane.
    pointer_here: bool,
    presenter: Option<Arc<Presenter>>,
    /// Damage (points) held back while the display was busy.
    deferred: Vec<Rect>,
    /// Frames painted, the time they took, frames skipped while busy, and
    /// presents done inline (one display): count, total and longest, pixels
    /// (since the last statistics line), and all of them.
    pub painted: u64,
    pub paint_ns: u64,
    pub skipped: u64,
    pub presents: (u64, u64, u64, u64),
    inline_total: u64,
}

/// A connector's identity for preferences, and a name for people.
pub fn identity(conn: &ConnectorInfo) -> (u32, String) {
    let edid = conn.edid.as_ref().and_then(|e| aurora_edid::parse(e).ok());
    let id = match &edid {
        Some(e) => alloc::format!(
            "{}{:04x}{:08x}{}{}",
            e.manufacturer_str(),
            e.product,
            e.serial,
            e.name.as_deref().unwrap_or(""),
            e.serial_text.as_deref().unwrap_or("")
        ),
        None => String::new(),
    };
    let key = displays::identity_hash(&alloc::format!("{id}@{}", conn.name()));
    let name = match edid.as_ref().and_then(|e| e.name.clone()) {
        // Several identical monitors: tell them apart by connector.
        Some(n) if !n.is_empty() => alloc::format!("{n} ({})", conn.name()),
        _ => conn.name(),
    };
    (key, name)
}

/// The saved preferences of the display with identity `key`.
pub fn record(key: u32) -> Option<Record> {
    settings::get(&displays::pref_key(key)).and_then(|v| Record::parse(&v))
}

/// "Keep this resolution": with one display, for the bootloader too
/// (`boot.conf`); with several, in the main display's own preferences, so
/// it follows that display (the bootloader sets up only the first).
pub fn keep_main_resolution(w: u32, h: u32) -> Result<(), isize> {
    let list = published();
    match list.first() {
        Some(main) if list.len() > 1 => {
            let r = Record { width: w, height: h, ..record(main.key).unwrap_or_default() };
            prefs::set(&displays::pref_key(main.key), &r.format())
        }
        _ => prefs::set_boot_resolution(w, h),
    }
}

/// Device pixels per point for a `w`×`h` display: its own choice (Settings,
/// per display), else the system's (`pref::DISPLAY_SCALE`), else 2 on
/// screens of 2560×1440 and up. 2× only when it leaves 1024×640 points and
/// the size divides evenly.
pub fn scale_for(w: u32, h: u32, own: u8) -> i32 {
    let fits = w / 2 >= 1024 && h / 2 >= 640 && w.is_multiple_of(2) && h.is_multiple_of(2);
    let choice = match own {
        1 => Some("1"),
        2 => Some("2"),
        _ => None,
    };
    let system = settings::get(aurora_abi::pref::DISPLAY_SCALE);
    match choice.or(system.as_deref()) {
        Some("1") => 1,
        Some("2") if fits => 2,
        _ if fits && w >= 2560 && h >= 1440 => 2,
        _ => 1,
    }
}

impl Screen {
    fn new(out: Output) -> Screen {
        let conn = out.connector_info();
        let (key, name) = conn.as_ref().map(identity).unwrap_or((0, String::from("Display")));
        let connector = conn.as_ref().map(|c| c.name()).unwrap_or_default();
        let mm = conn.as_ref().map_or((0, 0), |c| c.mm);
        let record = record(key).unwrap_or_default();
        Screen {
            id: out.connector,
            out,
            key,
            name,
            connector,
            rect: Rect::default(),
            scale: 1,
            record,
            mirror: false,
            unmirrored: None,
            mm,
            materials: Default::default(),
            cursor_shown: None,
            pointer_here: false,
            presenter: None,
            deferred: Vec::new(),
            painted: 0,
            paint_ns: 0,
            skipped: 0,
            presents: (0, 0, 0, 0),
            inline_total: 0,
        }
    }

    /// Busy presenting: its damage waits.
    pub fn busy(&self) -> bool {
        self.presenter.as_ref().is_some_and(|p| p.busy())
    }

    /// The presenter (several displays), for the self-tests.
    #[cfg(feature = "ktest")]
    pub fn presenter(&self) -> Option<&Arc<Presenter>> {
        self.presenter.as_ref()
    }

    /// Presents done on this display (inline or by its task).
    pub fn present_count(&self) -> u64 {
        self.inline_total + self.presenter.as_ref().map_or(0, |p| p.total.load(Ordering::Relaxed))
    }

    fn set_size(&mut self, w: u32, h: u32) -> gpu::Result<()> {
        if let Some(p) = &self.presenter {
            p.wait_idle(500);
        }
        self.cursor_shown = None;
        self.out.set_size(w, h)
    }

    fn present(&mut self, rects: &[Damage]) {
        match &self.presenter {
            Some(p) => p.submit(self.out.framebuffer(), rects),
            None => {
                let started = time::now_ns();
                if let Err(e) = self.out.present(rects) {
                    log!("gui", "present failed: {:?}", e);
                }
                let elapsed = time::now_ns() - started;
                // "flush" in the time by stage: getting the picture to the display.
                stages::add(stages::Stage::Flush, elapsed);
                self.presents.0 += 1;
                self.inline_total += 1;
                self.presents.1 += elapsed;
                self.presents.2 = self.presents.2.max(elapsed);
                self.presents.3 += rects.iter().map(|r| r.w as u64 * r.h as u64).sum::<u64>();
            }
        }
    }

    fn close(mut self) {
        if let Some(p) = self.presenter.take() {
            p.wait_idle(200);
            p.stop();
        }
        if let Err(e) = self.out.disable() {
            log!("gui", "could not switch {} off: {:?}", self.connector, e);
        }
        log!("gui", "display {} ({}) removed", self.connector, self.name);
    }
}

// ------------------------------------------------------- what apps see

static PUBLISHED: IrqMutex<Vec<DisplayInfo>> = IrqMutex::new(Vec::new());
/// Largest display, in points (windows may be that large).
static LARGEST: IrqMutex<Option<(i32, i32)>> = IrqMutex::new(None);

/// The displays as `desktop::DISPLAYS` reports them, the main one first.
pub fn published() -> Vec<DisplayInfo> {
    PUBLISHED.lock().clone()
}

pub fn largest() -> Option<(i32, i32)> {
    *LARGEST.lock()
}

fn fill(dst: &mut [u8], s: &str) {
    let n = s.len().min(dst.len() - 1);
    // Whole characters only.
    let n = (0..=n).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0);
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
}

/// A display's resolutions (`desktop::DISPLAY_MODES`), the current one flagged.
pub fn modes_of(id: u32) -> Option<Vec<DisplayMode>> {
    let info = PUBLISHED.lock().iter().find(|d| d.id == id).copied()?;
    let current = (info.pixel_width, info.pixel_height);
    for dev in gpu::devices() {
        let Some(conn) = dev.connectors().into_iter().find(|c| c.id == id) else { continue };
        let caps = dev.caps();
        let live = if caps.live_modeset { aurora_abi::display::LIVE } else { 0 };
        let follows = if caps.follows_host { aurora_abi::display::FOLLOWS_WINDOW } else { 0 };
        let mut out: Vec<DisplayMode> = Vec::new();
        for m in conn.modes.iter().filter(|m| m.hdisplay >= 800 && m.vdisplay >= 600) {
            let flags = live | follows | if m.is_preferred() { aurora_abi::display::PREFERRED } else { 0 };
            match out.iter_mut().find(|o| (o.width, o.height) == m.size()) {
                Some(o) => o.flags |= flags,
                None => out.push(DisplayMode { width: m.hdisplay, height: m.vdisplay, flags, _pad: 0 }),
            }
        }
        if !out.iter().any(|m| (m.width, m.height) == current) {
            out.push(DisplayMode { width: current.0, height: current.1, flags: live, _pad: 0 });
        }
        for m in &mut out {
            if (m.width, m.height) == current {
                m.flags |= aurora_abi::display::CURRENT;
            }
        }
        out.sort_by_key(|m| (m.width, m.height));
        return Some(out);
    }
    None
}

// ------------------------------------------------------------ compositor

/// Lumen Server: the displays, the desktop, and one frame at a time.
pub struct Compositor {
    pub screens: Vec<Screen>,
    /// Index of the main display in `screens`.
    pub main: usize,
    pub mirror: bool,
    pub desktop: Desktop,
    /// For each of the desktop's display areas, its screen.
    areas: Vec<usize>,
    hw_cursor: bool,
    next_tick: u64,
    /// Full-screen frames (count, total ns, max ns, since) and partial updates (count, total ns).
    frame_stats: (u64, u64, u64, u64),
    partial_stats: (u64, u64),
    /// The last frame's painting and presenting time (ns), and frames with damage.
    pub last_frame_ns: u64,
    pub frames: u64,
    ready: bool,
}

/// Whether the compositor runs (the self-tests make their own).
pub static RUNNING: AtomicBool = AtomicBool::new(false);

impl Compositor {
    /// Opens the displays and composes the desktop on them.
    pub fn new() -> Compositor {
        // The first display (the primary device's) in the resolution chosen
        // in Settings when it can switch now (otherwise the bootloader
        // already applied it); every display in its own saved resolution.
        let boot = if crate::drivers::display::live() { prefs::boot_resolution() } else { None };
        let first = AtomicBool::new(true);
        let outs = Output::open_all(|conn| {
            let own = record(identity(conn).0).filter(|r| r.width > 0).map(|r| (r.width, r.height));
            let is_first = first.swap(false, Ordering::Relaxed);
            own.or(if is_first { boot } else { None })
        });
        let mut screens: Vec<Screen> = outs.into_iter().map(Screen::new).collect();
        if screens.is_empty() {
            screens.push(Screen::new(Output::open(boot).expect("no display")));
        }
        let (main, mirror) = choose_main(&screens);
        let areas = lay_out(&mut screens, main, mirror);
        let m = &screens[main];
        super::set_main(&m.out, m.scale);
        let t0 = time::uptime_ms();
        let desktop = Desktop::new(m.rect.w, m.rect.h);
        log!("gui", "desktop composed in {} ms ({}x{})", time::uptime_ms() - t0, m.rect.w, m.rect.h);
        let mut c = Compositor {
            screens,
            main,
            mirror,
            desktop,
            areas,
            hw_cursor: false,
            next_tick: 0,
            frame_stats: (0, 0, 0, time::uptime_ms()),
            partial_stats: (0, 0),
            last_frame_ns: 0,
            frames: 0,
            ready: false,
        };
        c.hw_cursor_setup();
        c.apply();
        c
    }

    fn choose_main(&mut self) {
        (self.main, self.mirror) = choose_main(&self.screens);
    }

    fn lay_out(&mut self) {
        self.areas = lay_out(&mut self.screens, self.main, self.mirror);
    }

    fn hw_cursor_setup(&mut self) {
        // The pointer goes on the displays' cursor planes when they all have
        // one (`cursor=software` in boot.conf draws it into the picture instead).
        self.hw_cursor =
            self.screens.iter().all(|s| s.out.has_cursor()) && prefs::boot_option("cursor").as_deref() != Some("software");
        cursor::set_hardware(self.hw_cursor);
        for s in &mut self.screens {
            s.cursor_shown = None;
            s.pointer_here = false;
        }
    }

    /// Hands the layout to the desktop and to apps; presenters for several displays.
    fn apply(&mut self) {
        let several = self.screens.len() > 1;
        for s in &mut self.screens {
            match (&s.presenter, several) {
                (None, true) => s.presenter = Some(Presenter::start(s.out.dev.clone())),
                (Some(_), false) => {
                    if let Some(p) = s.presenter.take() {
                        p.wait_idle(200);
                        p.stop();
                    }
                }
                _ => {}
            }
            s.deferred.clear();
            s.materials.clear();
        }
        let main_area = self.areas.iter().position(|&i| i == self.main).unwrap_or(0);
        let areas: Vec<DisplayArea> = self
            .areas
            .iter()
            .map(|&i| {
                let s = &self.screens[i];
                DisplayArea {
                    id: s.id,
                    key: s.key,
                    rect: s.rect,
                    scale: s.scale,
                    wallpaper: s.record.wallpaper,
                    name: s.name.clone(),
                }
            })
            .collect();
        self.desktop.set_displays(areas, main_area);
        self.publish();
        let list: Vec<String> = self
            .screens
            .iter()
            .map(|s| {
                let (w, h) = s.out.size();
                alloc::format!(
                    "{} {}x{}@{}x at ({}, {}){}",
                    s.connector,
                    w,
                    h,
                    s.scale,
                    s.rect.x,
                    s.rect.y,
                    if s.mirror { " mirroring" } else { "" }
                )
            })
            .collect();
        log!("gui", "displays: {}; main {}", list.join(", "), self.screens[self.main].connector);
    }

    fn publish(&self) {
        let mut list = Vec::new();
        let order = core::iter::once(self.main).chain((0..self.screens.len()).filter(|&i| i != self.main));
        for i in order {
            let s = &self.screens[i];
            let (pw, ph) = s.out.size();
            let caps = s.out.dev.caps();
            let mut d = DisplayInfo {
                id: s.id,
                flags: if i == self.main { display_info::PRIMARY } else { 0 }
                    | if s.mirror { display_info::MIRROR } else { 0 }
                    | if caps.live_modeset { display_info::LIVE } else { 0 }
                    | if caps.follows_host { display_info::FOLLOWS_WINDOW } else { 0 },
                x: s.rect.x,
                y: s.rect.y,
                width: s.rect.w,
                height: s.rect.h,
                pixel_width: pw,
                pixel_height: ph,
                scale: s.scale as u32,
                refresh_mhz: s.out.mode().vrefresh * 1000,
                mm_width: s.mm.0,
                mm_height: s.mm.1,
                key: s.key,
                wallpaper: s.record.wallpaper as i32,
                ..Default::default()
            };
            fill(&mut d.name, &s.name);
            fill(&mut d.connector, &s.connector);
            list.push(d);
        }
        *LARGEST.lock() = list.iter().map(|d| (d.width, d.height)).max_by_key(|&(w, h)| w as i64 * h as i64);
        *PUBLISHED.lock() = list;
    }

    /// Displays were plugged in or out, a host window resized, or display
    /// preferences changed: probe every device and set everything up again.
    pub fn reconfigure(&mut self) {
        let before: Vec<(u32, (u32, u32), i32, Rect)> =
            self.screens.iter().map(|s| (s.key, s.out.size(), s.scale, s.rect)).collect();
        let main_before = (self.screens[self.main].key, self.screens[self.main].out.size(), self.screens[self.main].scale);
        // What is connected now.
        let mut connected: Vec<(Arc<dyn KmsDevice>, ConnectorInfo)> = Vec::new();
        for dev in gpu::devices() {
            for conn in dev.probe().into_iter().filter(|c| c.status == Status::Connected) {
                connected.push((dev.clone(), conn));
            }
        }
        // Gone: switched off. (The last display stays, whatever it reports.)
        let mut i = 0;
        while i < self.screens.len() {
            let here = connected.iter().any(|(d, c)| c.id == self.screens[i].out.connector && Arc::ptr_eq(d, &self.screens[i].out.dev));
            if !here && self.screens.len() > 1 {
                let s = self.screens.remove(i);
                s.close();
            } else {
                i += 1;
            }
        }
        // New: opened in their saved (or preferred) resolution.
        for (dev, conn) in &connected {
            if self.screens.iter().any(|s| s.out.connector == conn.id && Arc::ptr_eq(&s.out.dev, dev)) {
                continue;
            }
            let taken: Vec<_> = self.screens.iter().filter(|s| Arc::ptr_eq(&s.out.dev, dev)).map(|s| s.out.crtc).collect();
            let want = record(identity(conn).0).filter(|r| r.width > 0).map(|r| (r.width, r.height));
            match Output::open_connector(dev.clone(), conn, want, &taken) {
                Some(out) => {
                    let s = Screen::new(out);
                    log!("gui", "display {} ({}) connected", s.connector, s.name);
                    self.screens.push(s);
                }
                None => log!("gui", "could not open {} (no free CRTC?)", conn.name()),
            }
        }
        // Each display's saved resolution, or its preferred one (a host window resized).
        for s in &mut self.screens {
            s.record = record(s.key).unwrap_or_default();
            let want = (s.record.width, s.record.height);
            if want != (0, 0) && want != s.out.size() {
                if let Err(e) = s.set_size(want.0, want.1) {
                    log!("gui", "{} can't show {}x{} ({:?})", s.connector, want.0, want.1, e);
                }
            } else if want == (0, 0) {
                if let Some(p) = &s.presenter {
                    p.wait_idle(500);
                }
                if s.out.follow_preferred() == Some(true) {
                    s.cursor_shown = None;
                }
            }
        }
        let main_key = self.screens.get(self.main).map(|s| s.key);
        self.choose_main();
        if settings::get(displays::PRIMARY_PREF).is_none() {
            // No choice saved: the main display stays the main one while connected.
            if let Some(i) = main_key.and_then(|k| self.screens.iter().position(|s| s.key == k)) {
                self.main = i;
            }
        }
        // Mirroring: the other displays switch to the main display's
        // resolution when they can, and back afterwards.
        let size = self.screens[self.main].out.size();
        for i in (0..self.screens.len()).filter(|&i| i != self.main) {
            let s = &mut self.screens[i];
            if self.mirror && s.out.size() != size {
                let own = s.out.size();
                if s.set_size(size.0, size.1).is_ok() {
                    s.unmirrored.get_or_insert(own);
                }
            } else if !self.mirror {
                if let Some((w, h)) = s.unmirrored.take() {
                    let _ = s.set_size(w, h);
                }
            }
        }
        self.lay_out();
        let main_after = (self.screens[self.main].key, self.screens[self.main].out.size(), self.screens[self.main].scale);
        let m = &self.screens[self.main];
        super::set_main(&m.out, m.scale);
        let after: Vec<(u32, (u32, u32), i32, Rect)> =
            self.screens.iter().map(|s| (s.key, s.out.size(), s.scale, s.rect)).collect();
        if after != before {
            self.hw_cursor_setup();
        }
        self.apply();
        if main_after != main_before {
            let m = &self.screens[self.main];
            let (w, h) = (m.rect.w, m.rect.h);
            log!("gui", "main display {}: drawing at {}x ({}x{} points)", m.connector, m.scale, w, h);
            server::broadcast(aurora_abi::Event { kind: aurora_abi::event::SCREEN, x: w, y: h, ..Default::default() });
        }
    }

    /// Settings' resolution for the main display (`set_display`).
    fn set_main_size(&mut self, w: u32, h: u32) {
        let main = self.main;
        match self.screens[main].set_size(w, h) {
            Ok(()) => {
                self.lay_out();
                let m = &self.screens[main];
                super::set_main(&m.out, m.scale);
                self.hw_cursor_setup();
                self.apply();
                let (w, h) = (self.screens[main].rect.w, self.screens[main].rect.h);
                server::broadcast(aurora_abi::Event { kind: aurora_abi::event::SCREEN, x: w, y: h, ..Default::default() });
            }
            Err(e) => log!("gui", "the display can't show {}x{} ({:?})", w, h, e),
        }
    }

    /// The display the pointer is on (an index into `screens`).
    pub fn pointer_screen(&self) -> usize {
        let (x, y) = self.desktop.pointer_at();
        self.areas
            .iter()
            .copied()
            .find(|&i| self.screens[i].rect.contains(x, y))
            .unwrap_or(self.main)
    }

    /// One frame: input, commands, damage painted on each display and
    /// presented, the pointer. Returns how long to wait for the next (ms).
    pub fn frame(&mut self) -> u64 {
        let now = time::uptime_ms();
        let d = &mut self.desktop;
        d.set_now(now);
        while let Some(ev) = input::pop() {
            d.handle(ev, now);
        }
        d.process_commands();
        // A new scale, displays plugged in or out, display preferences.
        let rescale = core::mem::take(&mut d.rescale_request);
        if core::mem::take(&mut d.displays_request) || rescale {
            self.reconfigure();
        }
        if let Some((w, h)) = self.desktop.resolution_request.take() {
            self.set_main_size(w, h);
        }
        let d = &mut self.desktop;
        // Input can start an animation, change focus, or unhover an expired
        // banner. Recalculate before waiting so new work isn't delayed.
        self.next_tick = self.next_tick.min(d.next_wakeup());
        if now >= self.next_tick {
            self.next_tick = now + d.tick(now);
        }
        let scene_changed = d.scene_changed();
        let damage = d.take_damage();
        if !damage.is_empty() {
            crate::telemetry::FRAMES.fetch_add(1, Ordering::Relaxed);
        }
        let frame_start = time::now_ns();
        let batch_pixels: i64 = damage.iter().map(|r| r.area()).sum();
        let painted = self.paint(&damage, scene_changed);
        self.update_pointer(now);
        self.desktop.finish_frame();
        for id in self.desktop.take_presented() {
            server::frame_done(id);
        }
        if painted {
            self.frames += 1;
            self.last_frame_ns = time::now_ns() - frame_start;
        }
        if batch_pixels > 0 {
            let full = damage.iter().any(|r| *r == self.desktop.bounds());
            log!(
                "frame",
                "{}, {}, {}, {} display(s)",
                if full {
                    "full"
                } else if scene_changed {
                    "partial"
                } else {
                    "cursor"
                },
                batch_pixels,
                self.last_frame_ns / 1_000_000,
                self.screens.len()
            );
        }
        self.log_stats();
        if core::mem::take(&mut self.desktop.screenshot) {
            let m = &self.screens[self.main];
            let (w, h) = m.out.size();
            super::take_screenshot(unsafe { m.out.back_buffer() }, w, h);
        }
        self.next_tick.saturating_sub(time::uptime_ms()).max(1)
    }

    /// Paints `damage` (desktop points) on every display it touches, plus
    /// what each display held back while it was busy. Returns whether
    /// anything was painted.
    fn paint(&mut self, damage: &[Rect], scene_changed: bool) -> bool {
        let mut any = false;
        let mut rects: Vec<Damage> = Vec::new();
        let mut mirrored: Vec<Damage> = Vec::new();
        let main = self.main;
        for (area, &si) in self.areas.iter().enumerate() {
            let screen = &mut self.screens[si];
            let mut todo: Vec<Rect> =
                damage.iter().map(|r| r.intersect(&screen.rect)).filter(|r| !r.is_empty()).collect();
            if screen.busy() {
                // Still taking the last picture: this damage waits.
                if !todo.is_empty() {
                    screen.skipped += 1;
                    screen.deferred.extend(todo);
                    merge(&mut screen.deferred);
                }
                continue;
            }
            let force = !screen.deferred.is_empty();
            todo.append(&mut screen.deferred);
            if force {
                merge(&mut todo);
            }
            if todo.is_empty() {
                continue;
            }
            any = true;
            let started = time::now_ns();
            let (pw, ph) = screen.out.size();
            let back = unsafe { screen.out.back_buffer() };
            let full = screen.rect;
            rects.clear();
            for r in todo {
                let r_started = time::now_ns();
                crate::telemetry::PIXELS.fetch_add(r.area() as u64, Ordering::Relaxed);
                let mut cv =
                    Canvas::new(back, pw as i32, ph as i32).with_scale(screen.scale).with_origin(full.x, full.y);
                screen.materials.begin_frame();
                cv.materials = Some(&mut screen.materials);
                cv.clip = r;
                self.desktop.paint_display(area, &mut cv, force);
                let px = cv.to_px(r).intersect(&Rect::new(0, 0, pw as i32, ph as i32));
                if px.w > 0 && px.h > 0 {
                    rects.push(Damage { x: px.x as u32, y: px.y as u32, w: px.w as u32, h: px.h as u32 });
                }
                let elapsed = time::now_ns() - r_started;
                if r == full {
                    self.frame_stats.0 += 1;
                    self.frame_stats.1 += elapsed;
                    self.frame_stats.2 = self.frame_stats.2.max(elapsed);
                } else if scene_changed {
                    self.partial_stats.0 += 1;
                    self.partial_stats.1 += elapsed;
                }
            }
            screen.painted += 1;
            screen.paint_ns += time::now_ns() - started;
            // Everything painted on this display this frame goes to it at once.
            if !rects.is_empty() {
                screen.present(&rects);
            }
            if si == main && self.mirror {
                mirrored.extend_from_slice(&rects);
            }
        }
        if !mirrored.is_empty() {
            self.mirror_main(&mirrored);
        }
        any
    }

    /// Mirroring: the main display's new pixels on every other display.
    fn mirror_main(&mut self, rects: &[Damage]) {
        let main = self.main;
        let (mw, mh) = self.screens[main].out.size();
        let src = unsafe { self.screens[main].out.back_buffer() };
        for i in (0..self.screens.len()).filter(|&i| i != main) {
            let s = &mut self.screens[i];
            if s.busy() {
                s.skipped += 1;
                continue;
            }
            let (w, h) = s.out.size();
            let dst = unsafe { s.out.back_buffer() };
            let mut done = Vec::new();
            for r in rects {
                let r = r.clamp(w.min(mw), h.min(mh));
                if r.is_empty() {
                    continue;
                }
                for y in r.y..r.y + r.h {
                    let (a, b) = ((y * mw + r.x) as usize, (y * w + r.x) as usize);
                    dst[b..b + r.w as usize].copy_from_slice(&src[a..a + r.w as usize]);
                }
                done.push(r);
            }
            if !done.is_empty() {
                s.present(&done);
            }
        }
    }

    /// The pointer on the cursor plane of the display it is on (hidden on the others).
    fn update_pointer(&mut self, now: u64) {
        if !self.hw_cursor {
            return;
        }
        let (pos, shape) = self.desktop.cursor_state();
        let on = self.pointer_screen();
        let mut failed = false;
        let mut result = PointerOn::Plane;
        for (i, s) in self.screens.iter_mut().enumerate() {
            let here = i == on || (self.mirror && on == self.main);
            if !here {
                if s.pointer_here {
                    s.pointer_here = false;
                    s.cursor_shown = None;
                    let _ = s.out.hide_cursor();
                }
                continue;
            }
            s.pointer_here = true;
            let local = (pos.0 - s.rect.x, pos.1 - s.rect.y);
            let r = super::update_hw_cursor(&mut s.out, &mut s.cursor_shown, local, shape, now, s.scale);
            if r == PointerOn::Failed {
                failed = true;
            }
            if i == on {
                result = r;
            }
        }
        if failed {
            log!("gui", "a display's cursor plane failed; drawing the pointer instead");
            self.hw_cursor = false;
            for s in &mut self.screens {
                let _ = s.out.hide_cursor();
            }
            result = PointerOn::Failed;
        }
        // Moving between the plane and the picture: repaint (rare: a
        // shape too large for the plane at 2x).
        if cursor::hardware() != (result == PointerOn::Plane) {
            cursor::set_hardware(result == PointerOn::Plane);
            self.desktop.damage_all();
            self.next_tick = time::uptime_ms();
        }
    }

    /// Every 10 seconds: frames, time by stage, and presents per display.
    fn log_stats(&mut self) {
        let presented = self.screens.iter().any(|s| s.present_count() > 0);
        if !(self.frame_stats.0 > 0 || self.partial_stats.0 > 0 || presented)
            || time::uptime_ms() - self.frame_stats.3 < 10_000
        {
            return;
        }
        let (w, h) = (self.desktop.w, self.desktop.h);
        log!(
            "lumen",
            "{} scene frames, mean {} us, max {} us; {} partial updates, mean {} us; at {}x{}",
            self.frame_stats.0,
            self.frame_stats.1 / self.frame_stats.0.max(1) / 1000,
            self.frame_stats.2 / 1000,
            self.partial_stats.0,
            self.partial_stats.1 / self.partial_stats.0.max(1) / 1000,
            w,
            h
        );
        let mut spent = String::new();
        stages::take(&mut spent);
        if !spent.is_empty() {
            log!("lumen", "time by stage: {}", spent);
        }
        for s in &mut self.screens {
            let (n, ns, max, px) = match &s.presenter {
                Some(p) => (
                    p.presents.swap(0, Ordering::Relaxed),
                    p.ns.swap(0, Ordering::Relaxed),
                    p.max_ns.swap(0, Ordering::Relaxed),
                    p.pixels.swap(0, Ordering::Relaxed),
                ),
                None => core::mem::take(&mut s.presents),
            };
            log!(
                "lumen",
                "present on {} {}: {} frames, mean {} us, max {} us, {} kpx per frame{}",
                s.out.dev.name(),
                s.connector,
                n,
                ns / n.max(1) / 1000,
                max / 1000,
                px / n.max(1) / 1000,
                if s.skipped > 0 { alloc::format!("; {} frames held back while busy", s.skipped) } else { String::new() }
            );
            s.skipped = 0;
        }
        self.frame_stats = (0, 0, 0, time::uptime_ms());
        self.partial_stats = (0, 0);
    }

    /// After the first frame: the boot screen goes, the startup sound plays.
    fn first_frame_done(&mut self) {
        if self.ready {
            return;
        }
        self.ready = true;
        boot::finish();
        log!("boot", "Aurora desktop ready");
        crate::drivers::audio::play_sound("startup");
        // Written a moment later, off the compositor, so it includes late devices.
        sched::spawn("bootlog", || {
            sched::sleep_ms(3000);
            crate::report::save_boot_log();
        });
        crate::telemetry::stage("desktop", "Desktop ready");
    }

    /// The compositor task's loop.
    pub fn run(mut self) -> ! {
        RUNNING.store(true, Ordering::Relaxed);
        loop {
            let wait = self.frame();
            self.first_frame_done();
            if let Some(action) = self.desktop.power {
                crate::fs::sync_all();
                sched::sleep_ms(800);
                match action {
                    PowerAction::Shutdown => crate::power::shutdown(),
                    PowerAction::Reboot => crate::power::reboot(),
                }
            }
            sched::wait_until(wait, || {
                input::pending() || server::pending() || notify::pending() || crate::drivers::gamepad::pending()
            });
        }
    }
}

#[cfg(feature = "ktest")]
impl Screen {
    /// The scanout (CRTC index) the display is on: QEMU's `head`.
    pub fn head(&self) -> u32 {
        self.out.dev.crtcs().iter().find(|c| c.id == self.out.crtc).map_or(0, |c| c.index)
    }

    /// The pixel of the back buffer at (`x`, `y`) device pixels.
    pub fn pixel(&self, x: u32, y: u32) -> u32 {
        let (w, _) = self.out.size();
        let px = unsafe { self.out.back_buffer() };
        px[(y * w + x) as usize]
    }

    /// Whether the display's cursor plane shows the pointer.
    pub fn cursor_shown(&self) -> bool {
        let dev = &self.out.dev;
        let Some(index) = dev.crtcs().iter().find(|c| c.id == self.out.crtc).map(|c| c.index) else { return false };
        let planes = dev.planes();
        let cursor = planes.iter().find(|p| p.kind == gpu::PlaneKind::Cursor && p.possible_crtcs & (1 << index) != 0);
        let (_, state) = dev.state();
        cursor.is_some_and(|c| state.iter().any(|p| p.plane == c.id && p.fb.is_some()))
    }
}

#[cfg(feature = "ktest")]
impl Compositor {
    /// Waits until every display has taken its last picture.
    pub fn wait_presented(&self, ms: u64) -> bool {
        self.screens.iter().all(|s| s.presenter.as_ref().is_none_or(|p| p.wait_idle(ms)))
    }

    /// Runs frames (about 60 a second) until nothing changes any more, or `ms` pass.
    pub fn settle(&mut self, ms: u64) {
        let end = time::uptime_ms() + ms;
        let mut quiet = 0;
        while time::uptime_ms() < end && quiet < 3 {
            let before = self.frames;
            self.frame();
            self.wait_presented(1000);
            let busy = self.screens.iter().any(|s| !s.deferred.is_empty());
            quiet = if self.frames == before && !busy { quiet + 1 } else { 0 };
            sched::sleep_ms(16);
        }
    }

    /// Frames until `done`, or `ms` pass.
    pub fn until(&mut self, ms: u64, done: impl Fn(&Compositor) -> bool) -> bool {
        let end = time::uptime_ms() + ms;
        while time::uptime_ms() < end {
            if done(self) {
                return true;
            }
            self.frame();
            sched::sleep_ms(20);
        }
        done(self)
    }

    /// How many displays are areas of the desktop (one while mirroring).
    pub fn areas(&self) -> usize {
        self.areas.len()
    }

    /// Ends the self-test's compositor: the presenters stop.
    pub fn close(mut self) {
        for s in &mut self.screens {
            if let Some(p) = s.presenter.take() {
                p.wait_idle(500);
                p.stop();
            }
        }
    }
}

/// The main display (the one chosen in Settings when connected, else the
/// first) and whether the others mirror it.
fn choose_main(screens: &[Screen]) -> (usize, bool) {
    let want = settings::get(displays::PRIMARY_PREF).and_then(|v| u32::from_str_radix(&v, 16).ok());
    let main = want.and_then(|k| screens.iter().position(|s| s.key == k)).unwrap_or(0);
    let mirror = screens.len() > 1 && settings::get(displays::MIRROR_PREF).as_deref() == Some("1");
    (main, mirror)
}

/// Scales and places: the main display at (0, 0), the others where they
/// were saved, touching (or, mirroring, all on the main one). Returns the
/// screens that are areas of the desktop.
fn lay_out(screens: &mut [Screen], main: usize, mirror: bool) -> Vec<usize> {
    for s in screens.iter_mut() {
        let (w, h) = s.out.size();
        s.scale = scale_for(w, h, s.record.scale);
        s.mirror = false;
    }
    if mirror {
        let (w, h) = screens[main].out.size();
        let k = screens[main].scale;
        for (i, s) in screens.iter_mut().enumerate() {
            s.rect = Rect::new(0, 0, w as i32 / k, h as i32 / k);
            s.mirror = i != main;
        }
        return alloc::vec![main];
    }
    let sizes: Vec<(i32, i32)> =
        screens.iter().map(|s| (s.out.size().0 as i32 / s.scale, s.out.size().1 as i32 / s.scale)).collect();
    // Saved places are relative to the main display as it was then: if
    // that was another one, this one's saved place is the new origin.
    let base = record(screens[main].key).map_or((0, 0), |r| (r.x, r.y));
    let saved: Vec<Option<(i32, i32)>> = screens
        .iter()
        .enumerate()
        .map(|(i, s)| (i != main && record(s.key).is_some()).then_some((s.record.x - base.0, s.record.y - base.1)))
        .collect();
    let rects = displays::arrange(&sizes, &saved, main);
    for (s, r) in screens.iter_mut().zip(rects) {
        s.rect = r;
    }
    (0..screens.len()).collect()
}

/// Overlapping rectangles become their union.
fn merge(rects: &mut Vec<Rect>) {
    let mut merged = true;
    while merged {
        merged = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if rects[i].intersects(&rects[j]) {
                    rects[i] = rects[i].union(&rects[j]);
                    rects.swap_remove(j);
                    merged = true;
                    break 'outer;
                }
            }
        }
    }
    if rects.len() > 12 {
        let all = rects.iter().fold(Rect::default(), |acc, r| acc.union(r));
        *rects = alloc::vec![all];
    }
}
