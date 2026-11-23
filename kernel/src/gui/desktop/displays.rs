//! The desktop across several displays.
//!
//! The compositor (`gui::screens`) hands the desktop one [`DisplayArea`]
//! per display: a rectangle in one global space of points, with the main
//! display at (0, 0). The desktop's own `w`×`h` is the main display, so the
//! bar, Harbor, Shoal, Spotlight, notifications, Shore and Wave Play stay
//! there; windows go anywhere. Each display has its own wallpaper and its
//! own copies of the pictures the painting reuses (scene, drag backdrop),
//! swapped in while that display is painted. `lumen::displays` has the
//! arrangement math (pointer crossing, which display a window is on,
//! carrying windows between displays).

use super::Desktop;
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::{cursor, server, theme, wallpaper};
use alloc::string::String;
use alloc::vec::Vec;
use lumen::displays;
use lumen::windowing::{self, Placement};

/// A display as the desktop sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct DisplayArea {
    /// Stays the same while the display is connected (apps see it).
    pub id: u32,
    /// The display's identity hash: the same whenever it is plugged in.
    pub key: u32,
    /// Where it is, in points.
    pub rect: Rect,
    /// Device pixels per point.
    pub scale: i32,
    /// Its wallpaper, or -1 for the main display's.
    pub wallpaper: i16,
    pub name: String,
}

/// The pictures a display other than the main one needs (the main
/// display's live in `Desktop` itself), swapped in while it is painted.
pub(super) struct Caches {
    key: u32,
    wallpaper: Vec<u32>,
    blurred: Vec<u32>,
    scene: Vec<u32>,
    backdrop: Vec<u32>,
    backdrop_ok: bool,
    /// The wallpaper's size (points), scale and picture.
    made: (i32, i32, i32, u8),
}

impl DisplayArea {
    /// The wallpaper picture this display shows.
    fn picture(&self) -> u8 {
        match self.wallpaper {
            w if w < 0 => theme::wallpaper(),
            w if w as u8 == wallpaper::CUSTOM || (w as usize) < wallpaper::NAMES.len() => w as u8,
            _ => theme::wallpaper(),
        }
    }
}

impl Caches {
    fn new(a: &DisplayArea) -> Caches {
        let (w, h, k, picture) = (a.rect.w, a.rect.h, a.scale, a.picture());
        let (wallpaper, blurred) = Desktop::make_wallpaper_at(w, h, k, picture, false);
        Caches {
            key: a.key,
            wallpaper,
            blurred,
            scene: alloc::vec![0; (w * k * h * k) as usize],
            backdrop: Vec::new(),
            backdrop_ok: false,
            made: (w, h, k, picture),
        }
    }
}

impl Desktop {
    /// Every display's rectangle (the main display alone until the compositor says more).
    pub(super) fn display_rects(&self) -> Vec<Rect> {
        if self.displays.is_empty() {
            alloc::vec![self.screen()]
        } else {
            self.rects.clone()
        }
    }

    pub(super) fn main_index(&self) -> usize {
        self.main.min(self.displays.len().saturating_sub(1))
    }

    /// The rectangle around every display: what damage and the pointer may cover.
    pub fn bounds(&self) -> Rect {
        if self.displays.is_empty() {
            self.screen()
        } else {
            self.all_displays
        }
    }

    /// The displays, as the compositor last set them.
    #[cfg(feature = "ktest")]
    pub fn displays(&self) -> &[DisplayArea] {
        &self.displays
    }

    /// (`x`, `y`) is on the main display (where the bar and Harbor are).
    pub(super) fn on_main(&self, x: i32, y: i32) -> bool {
        self.screen().contains(x, y)
    }

    /// Index of the display showing (`x`, `y`), or the nearest one.
    pub(super) fn display_at(&self, x: i32, y: i32) -> usize {
        displays::at(&self.rects, x, y).unwrap_or_else(|| displays::nearest(&self.rects, x, y))
    }

    /// Index of the display a window with this rectangle is on.
    pub(super) fn display_of_rect(&self, r: Rect) -> usize {
        if self.rects.len() < 2 {
            return 0;
        }
        displays::home(&self.rects, r)
    }

    /// The area windows use on display `i`: below the bar on the main
    /// display, the whole display (less a margin) elsewhere.
    pub(super) fn work_area_on(&self, i: usize) -> Rect {
        if self.displays.is_empty() || i == self.main_index() {
            return self.work_area();
        }
        let d = self.displays[i.min(self.displays.len() - 1)].rect;
        Rect::new(d.x + 8, d.y + 8, d.w - 16, d.h - 16)
    }

    /// The work area of the display a window with this rectangle is on.
    pub(super) fn work_area_of(&self, r: Rect) -> Rect {
        self.work_area_on(self.display_of_rect(r))
    }

    /// The scale of the display a window with this rectangle is on.
    pub(super) fn scale_of_rect(&self, r: Rect) -> i32 {
        if self.displays.is_empty() {
            return crate::gui::scale();
        }
        self.displays[self.display_of_rect(r)].scale
    }

    /// A pointer position from the input devices (which count from 0, 0 at
    /// the top-left of [`Desktop::bounds`]) in desktop points, kept on a
    /// display. Moves the devices' idea of the pointer along when it was
    /// stopped at an edge.
    pub(super) fn input_to_desktop(&self, x: i32, y: i32) -> (i32, i32) {
        if self.rects.is_empty() {
            return (x.clamp(0, self.w - 1), y.clamp(0, self.h - 1));
        }
        let (ox, oy) = self.input_origin;
        let (gx, gy) = (x + ox, y + oy);
        let (cx, cy) = displays::constrain(&self.rects, gx, gy);
        if (cx, cy) != (gx, gy) {
            crate::drivers::input::warp(cx - ox, cy - oy);
        }
        (cx, cy)
    }

    /// The compositor's displays changed (connected, removed, moved,
    /// resized, rescaled, or another chosen as the main one). `areas[main]`
    /// is at (0, 0) and its scale is `gui::scale()`. Windows move with
    /// their display; a removed display's windows go to the main one and
    /// come back when it does.
    pub fn set_displays(&mut self, areas: Vec<DisplayArea>, main: usize) {
        if areas.is_empty() {
            return;
        }
        let main = main.min(areas.len() - 1);
        let old: Vec<(u32, Rect)> = self.displays.iter().map(|d| (d.key, d.rect)).collect();
        let new: Vec<(u32, Rect)> = areas.iter().map(|d| (d.key, d.rect)).collect();
        for win in &mut self.windows {
            let (rect, away) = displays::migrate(&old, &new, main, win.rect, win.away);
            if let Some(saved) = win.saved {
                win.saved = Some(displays::migrate(&old, &new, main, saved, win.away).0);
            }
            win.rect = rect;
            win.away = away;
        }
        let (w, h) = (areas[main].rect.w, areas[main].rect.h);
        let main_key = areas[main].key;
        let main_changed = (w, h) != (self.w, self.h)
            || self.displays.get(self.main_index()).is_none_or(|d| d.key != main_key || d.scale != areas[main].scale);
        // Other displays keep their pictures when nothing they depend on changed.
        let mut kept: Vec<Caches> = self.others.drain(..).flatten().collect();
        self.others = areas
            .iter()
            .enumerate()
            .map(|(i, a)| {
                if i == main {
                    return None;
                }
                let made = (a.rect.w, a.rect.h, a.scale, a.picture());
                match kept.iter().position(|c| c.key == a.key && c.made == made) {
                    Some(at) => Some(kept.swap_remove(at)),
                    None => Some(Caches::new(a)),
                }
            })
            .collect();
        self.rects = areas.iter().map(|d| d.rect).collect();
        self.all_displays = displays::bounds(&self.rects);
        self.displays = areas;
        self.main = main;
        if main_changed {
            self.main_resized(w, h);
        }
        self.fit_windows();
        let rects = self.display_rects();
        self.cursor = displays::constrain(&rects, self.cursor.0, self.cursor.1);
        let b = self.bounds();
        self.input_origin = (b.x, b.y);
        crate::drivers::input::set_screen_size(b.w, b.h);
        crate::drivers::input::warp(self.cursor.0 - b.x, self.cursor.1 - b.y);
        self.invalidate_backdrops();
        self.displays_changed = true;
        self.damage_all();
    }

    /// Keeps every window inside the work area of its display (and fitted
    /// to it when moored or maximized), as after any change of size.
    pub(super) fn fit_windows(&mut self) {
        let rects = self.display_rects();
        let areas: Vec<Rect> = (0..rects.len()).map(|i| self.work_area_on(i)).collect();
        for win in &mut self.windows {
            let wa = areas[displays::home(&rects, win.rect)];
            let minimum = win.minimum();
            let r = &mut win.rect;
            r.w = r.w.min(wa.w);
            r.h = r.h.min(wa.h);
            r.x = r.x.clamp(wa.x, (wa.right() - r.w).max(wa.x));
            r.y = r.y.clamp(wa.y, (wa.bottom() - r.h).max(wa.y));
            if let Some(saved) = win.saved.as_mut() {
                *saved = windowing::clamp(*saved, wa);
            }
            if win.placement != Placement::Floating {
                match windowing::target(win.placement, wa, minimum) {
                    Some(target) => *r = target,
                    None => {
                        *r = windowing::clamp(win.saved.take().unwrap_or(*r), wa);
                        win.placement = Placement::Floating;
                    }
                }
            }
        }
    }

    /// The wallpaper changed (picture or appearance): every other display's too.
    pub(super) fn rebuild_wallpapers(&mut self) {
        for (i, a) in self.displays.iter().enumerate() {
            if let Some(Some(c)) = self.others.get_mut(i) {
                *c = Caches::new(a);
            }
        }
    }

    /// Something under a moved window changed: no display's backdrop is valid.
    pub(super) fn invalidate_backdrops(&mut self) {
        for c in self.others.iter_mut().flatten() {
            c.backdrop_ok = false;
        }
    }

    fn swap_caches(&mut self, i: usize) -> bool {
        let Some(Some(c)) = self.others.get_mut(i) else { return false };
        core::mem::swap(&mut self.wallpaper, &mut c.wallpaper);
        core::mem::swap(&mut self.blurred, &mut c.blurred);
        core::mem::swap(&mut self.scene, &mut c.scene);
        core::mem::swap(&mut self.backdrop, &mut c.backdrop);
        core::mem::swap(&mut self.backdrop_ok, &mut c.backdrop_ok);
        true
    }

    /// Paints display `i` (the canvas covers its back buffer, at its scale,
    /// with its corner as origin; the clip is the damage). `force` repaints
    /// the scene even when nothing changed since the last frame: the
    /// display skipped frames while it was busy.
    pub fn paint_display(&mut self, i: usize, cv: &mut Canvas, force: bool) {
        let main = self.displays.is_empty() || i == self.main_index();
        let swapped = !main && self.swap_caches(i);
        if !main && !swapped {
            cv.fill_rect(cv.clip, 0xFF00_0000);
            return;
        }
        self.force_scene = force;
        if !main && (self.shore.is_some() || self.play_covers() || self.power.is_some()) {
            self.paint_aside(cv);
        } else {
            self.paint(cv);
        }
        self.force_scene = false;
        if swapped {
            self.swap_caches(i);
        }
    }

    /// Another display while Shore (login, lock), Wave Play or the power
    /// overlay holds the main one: its wallpaper, dimmed, and no windows.
    fn paint_aside(&mut self, cv: &mut Canvas) {
        if self.wallpaper.len() == cv.buf.len() {
            cv.blit_same(&self.wallpaper, cv.clip);
        }
        cv.fill_rect(cv.clip, if self.play_covers() { 0xE000_0000 } else { 0x9000_0000 });
        self.save_scene(cv);
        let shape = self.cursor_shape();
        cursor::draw(cv, self.cursor.0, self.cursor.1, shape, self.now_ms);
    }

    /// Tells client windows which display they are on (when it changed, or
    /// for every window after the displays changed), and publishes it for
    /// `desktop::WINDOW_DISPLAY`.
    pub(super) fn report_displays(&mut self) {
        let all = core::mem::take(&mut self.displays_changed);
        if self.displays.is_empty() {
            return;
        }
        let rects = self.display_rects();
        let count = self.displays.len() as u32;
        for win in &mut self.windows {
            let Some(client) = win.app.client_id() else { continue };
            let d = &self.displays[displays::home(&rects, win.rect)];
            if !all && win.on_display == d.id {
                continue;
            }
            win.on_display = d.id;
            server::set_window_display(client, d.id);
            server::push_event(
                client,
                aurora_abi::Event {
                    kind: aurora_abi::event::DISPLAY,
                    x: d.rect.w,
                    y: d.rect.h,
                    a: d.id,
                    b: d.scale as u32,
                    c: count,
                    ..Default::default()
                },
            );
        }
        // New windows open where the pointer is, at that display's scale.
        let here = self.display_at(self.cursor.0, self.cursor.1);
        server::set_new_window_scale(self.displays[here].scale);
    }

    /// Window ids and rectangles, and whether they were taken off a display (self-tests).
    #[cfg(feature = "ktest")]
    pub fn window_rects(&self) -> Vec<(u32, Rect, bool)> {
        self.windows.iter().map(|w| (w.id, w.rect, w.away.is_some())).collect()
    }

    /// Where the pointer is, in desktop points.
    pub fn pointer_at(&self) -> (i32, i32) {
        self.cursor
    }

    /// Adds a built-in window (the self-tests' coloured cards).
    #[cfg(feature = "ktest")]
    pub fn add_test_window(&mut self, app: alloc::boxed::Box<dyn super::App>) -> u32 {
        self.add_window(app);
        self.windows.last().map_or(0, |w| w.id)
    }

    /// Moves a window (self-tests).
    #[cfg(feature = "ktest")]
    pub fn move_window(&mut self, id: u32, r: Rect) {
        if let Some(i) = self.index_of(id) {
            self.windows[i].rect = r;
            self.windows[i].away = None;
            self.windows[i].animating = false;
            self.damage_all();
        }
    }

    /// Ends Shore (setup, login) for the self-tests, which drive the desktop directly.
    #[cfg(feature = "ktest")]
    pub fn dismiss_shore(&mut self) {
        self.shore = None;
        self.damage_all();
    }
}
