//! Full-screen windows, and what the compositor asks before it scans a
//! program's buffer out directly instead of compositing.
//!
//! A client window asks with `win_set_fullscreen` (`wl-bridge` does for
//! `xdg_toplevel.set_fullscreen`): its content then covers the display it
//! is on — no frame, shadow or rounded corners, above the bar and Harbor —
//! until it asks again or closes. Shoal, menus, Drift, Spotlight, the
//! switcher and Crest's notices still show over it.
//!
//! [`Desktop::scanout_window`] tells the compositor when one window's
//! content is all a display shows (a full-screen window, or the program in
//! front in Wave Play) with nothing of the desktop over it and the pointer
//! on the cursor plane: the display can then show the program's own buffer
//! (`gpu::owner`'s desktop commits), and goes back to compositing as soon
//! as anything else appears.

use super::{cursor, server, Desktop, Rect};
use lumen::displays;
use crate::gui::canvas::Canvas;
use lumen::windowing::CHROME_H;

impl Desktop {
    /// A client window asked to be full screen, or not (`Command::Fullscreen`).
    pub(super) fn fullscreen_command(&mut self, client: u32) {
        let Some(id) = self.window_of_client(client) else { return };
        let Some(i) = self.index_of(id) else { return };
        let want = server::fullscreen(client);
        if want == self.windows[i].fullscreen.is_some() {
            return;
        }
        if want {
            let r = self.windows[i].rect;
            self.windows[i].fullscreen = Some(r);
            self.windows[i].rect = self.fullscreen_rect(r);
            self.raise(id);
        } else if let Some(r) = self.windows[i].fullscreen.take() {
            self.windows[i].rect = r;
        }
        server::push_event(
            client,
            aurora_abi::Event { kind: aurora_abi::event::FULLSCREEN, a: want as u32, ..Default::default() },
        );
        self.damage_all();
    }

    /// Where a full-screen window goes from `r`: its content over the
    /// whole display it is on (its handle strip just above).
    fn fullscreen_rect(&self, r: Rect) -> Rect {
        let rects = self.display_rects();
        let d = rects[displays::home(&rects, r)];
        Rect::new(d.x, d.y - CHROME_H, d.w, d.h + CHROME_H)
    }

    /// After the displays changed: full-screen windows cover their display again.
    pub(super) fn refit_fullscreen(&mut self) {
        for i in 0..self.windows.len() {
            if self.windows[i].fullscreen.is_some() {
                let c = self.windows[i].content();
                self.windows[i].rect = self.fullscreen_rect(c);
            }
        }
    }

    /// The full-screen window whose content holds (`x`, `y`), topmost first.
    pub(super) fn fullscreen_at(&self, x: i32, y: i32) -> Option<usize> {
        self.windows.iter().rposition(|w| w.fullscreen.is_some() && !w.minimized && w.content().contains(x, y))
    }

    /// Full-screen windows, over the bar and Harbor: their content alone.
    pub(super) fn paint_fullscreen(&mut self, cv: &mut Canvas) {
        let focused = self.focused_id();
        for i in 0..self.windows.len() {
            let w = &self.windows[i];
            if w.fullscreen.is_none() || w.minimized || w.animating || !w.content().intersects(&cv.clip) {
                continue;
            }
            let env = self.env_at(Some(w.id) == focused, w.rect);
            let w = &mut self.windows[i];
            let content = w.content();
            cv.with_clip(content, |cv| {
                cv.fill_rect(content, 0xFF00_0000);
                w.app.draw(cv, content, &env)
            });
        }
    }

    /// Something of the desktop's own shows over windows (or soon will).
    fn overlays_up(&self) -> bool {
        self.launcher.is_some()
            || self.menu.is_some()
            || self.center.is_some()
            || self.spotlight.is_some()
            || self.switcher.is_some()
            || self.current.is_some()
            || self.drag.is_some()
            || self.dnd.is_some()
            || self.tour.is_some()
            || self.reveal.is_some()
            || !self.ghosts.is_empty()
            || self.moorings_open
            || self.items.menu.is_some()
            || self.items.rename.is_some()
            || self.crest_busy()
            || self.power.is_some()
    }

    /// The client window (its server id) whose content alone is what
    /// display area `area` shows — a full-screen window, or the program in
    /// front in Wave Play — with nothing over it, the pointer on the
    /// cursor plane or not drawn there (hidden, or on another display):
    /// the display may scan the program's buffer out. `None` whenever
    /// anything else is to be seen there.
    pub fn scanout_window(&self, area: usize) -> Option<u32> {
        if self.shore.is_some() || self.screenshot {
            return None;
        }
        let rects = self.display_rects();
        let rect = *rects.get(area)?;
        let (x, y) = self.cursor;
        let drawn = !cursor::hardware() && rect.contains(x, y) && self.cursor_shape() != aurora_abi::cursor::HIDDEN;
        if drawn {
            return None;
        }
        if self.play_covers() {
            let main = self.displays.is_empty() || area == self.main_index();
            return if main { self.play_scanout() } else { None };
        }
        if self.overlays_up() {
            return None;
        }
        let i = self.windows.iter().rposition(|w| w.fullscreen.is_some() && !w.minimized && w.rect.intersects(&rect))?;
        let w = &self.windows[i];
        if w.animating || w.awaiting_frame.is_some() || w.content() != rect {
            return None;
        }
        w.app.client_id()
    }

    /// The content rectangle of client window `client` (self-tests).
    #[cfg(feature = "ktest")]
    pub fn client_content(&self, client: u32) -> Option<Rect> {
        self.windows.iter().find(|w| w.app.client_id() == Some(client)).map(|w| w.content())
    }
}
