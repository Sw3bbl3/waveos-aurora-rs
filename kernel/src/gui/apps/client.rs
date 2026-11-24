//! A window owned by a user process.

use super::{App, AppKind, Env};
use crate::drivers::input::KeyEvent;
use crate::gui::canvas::{Canvas, Pixels};
use crate::gui::geom::Rect;
use crate::gui::server;
use crate::gui::theme::{self, WINDOW_RADIUS};
use alloc::string::String;
use aurora_abi::{event, Event};

pub struct ClientApp {
    pub id: u32,
    kind: AppKind,
    resizable: bool,
    initial: (i32, i32),
    /// Content size (points) and scale last reported to the client (to
    /// detect resizes).
    reported: (i32, i32, i32),
}

impl ClientApp {
    pub fn new(id: u32, kind: AppKind) -> Option<ClientApp> {
        let (_, _, resizable, initial) = server::info(id)?;
        Some(ClientApp { id, kind, resizable, initial, reported: (initial.0, initial.1, crate::gui::scale()) })
    }

    fn pointer(&self, kind: u32, x: i32, y: i32, area: Rect, clicks: u32) {
        self.pointer_button(kind, x, y, area, clicks, 1);
    }

    fn pointer_button(&self, kind: u32, x: i32, y: i32, area: Rect, clicks: u32, button: u32) {
        let d = crate::drivers::input::modifiers().bits();
        let ev = Event { kind, x: x - area.x, y: y - area.y, a: button, b: clicks, d, ..Default::default() };
        server::push_event(self.id, ev);
    }
}

impl App for ClientApp {
    fn materials(&self) -> alloc::vec::Vec<Rect> {
        server::material_regions(self.id)
    }
    fn covers_window(&self) -> bool {
        true
    }
    fn kind(&self) -> AppKind {
        self.kind
    }
    fn title(&self) -> String {
        server::title(self.id)
    }
    fn size(&self) -> (i32, i32) {
        self.initial
    }
    fn resizable(&self) -> bool {
        self.resizable
    }
    fn min_size(&self) -> (i32, i32) {
        server::minimum(self.id)
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        // A new size, or a new scale (the window went to a display with
        // another one): the client draws its surface again.
        if (area.w, area.h, env.scale) != self.reported {
            self.reported = (area.w, area.h, env.scale);
            server::set_window_scale(self.id, env.scale);
            server::push_event(
                self.id,
                Event { kind: event::RESIZE, x: area.w, y: area.h, a: env.scale as u32, ..Default::default() },
            );
        }
        let bg = theme::current().window_bg;
        // Glass regions (content points) are the only see-through parts of
        // the surface: blend there, copy everywhere else.
        let glass: alloc::vec::Vec<Rect> =
            self.materials().into_iter().map(|r| r.offset(area.x, area.y).intersect(&area)).collect();
        // Full screen: square corners (the display's own).
        let corner = if server::fullscreen(self.id) { 0 } else { WINDOW_RADIUS };
        let drawn = server::with_content(self.id, |content| {
            let (px, w, h, k) = (content.px, content.width, content.height, content.scale.max(1) as i32);
            // A buffer of the program's (a Linux program's dma-buf or shared
            // memory through `wl-bridge`): read where it is, no copy before this one.
            // Several of the program's surfaces (a browser's page and its
            // video, pop-up menus): each drawn where it is, the whole window
            // white underneath as the bridge composed it.
            if !content.layers.is_empty() {
                let cs = cv.scale;
                let shape = cv.to_px(area);
                let px_of = |r: Rect| {
                    Rect::new(shape.x + r.x * cs / k, shape.y + r.y * cs / k, r.w * cs / k, r.h * cs / k)
                };
                // What lies under an opaque layer covering the whole window
                // is never seen: drawing starts there (a browser's page over
                // its main surface).
                let covers = |l: &server::Layer| l.buffer.opaque() && px_of(l.dst).intersect(&shape) == shape;
                let from = content.layers.iter().rposition(covers);
                if from.is_none() {
                    cv.with_clip(area, |cv| cv.fill_round_rect(area, corner, 0xFFFF_FFFF));
                }
                let radius = corner * cs;
                for l in &content.layers[from.unwrap_or(0)..] {
                    let b = &l.buffer;
                    let pixels = if b.opaque() { Pixels::Opaque } else { Pixels::Blend };
                    let dst = px_of(l.dst);
                    let stride = b.stride / 4;
                    // Square at the top (the compositor strip), round at the bottom.
                    let top = Rect::new(area.x, area.y, area.w, corner);
                    let lower = Rect::new(area.x, area.y + corner, area.w, (area.h - corner).max(0));
                    let size = (b.width, b.height);
                    cv.with_clip(top, |cv| cv.blit_layer_px(b.pixels(), stride, size, l.src, dst, shape, 0, pixels));
                    cv.with_clip(lower, |cv| {
                        cv.blit_layer_px(b.pixels(), stride, size, l.src, dst, shape, radius, pixels)
                    });
                }
                return (area.w, area.h);
            }
            if let Some(opaque) = content.buffer {
                let pixels = if opaque { Pixels::Opaque } else { Pixels::Over(0xFFFF_FFFF) };
                let dst = Rect::new(area.x, area.y, w as i32 / k, h as i32 / k);
                // Square at the top (the compositor strip); the bottom
                // corners are rounded when the buffer reaches them.
                let round = if dst.w >= area.w && dst.h >= area.h { corner } else { 0 };
                let top = Rect::new(area.x, area.y, area.w, corner);
                let lower = Rect::new(area.x, area.y + corner, area.w, (area.h - corner).max(0));
                cv.with_clip(top, |cv| cv.blit_buffer(px, content.stride, w, h, dst, 0, pixels));
                cv.with_clip(lower, |cv| cv.blit_buffer(px, content.stride, w, h, dst, round, pixels));
                return (dst.w, dst.h);
            }
            let k = k.max(1);
            // Drawn for another scale (the window is on its way to a display
            // with this one, or straddles two): scaled until it catches up.
            if k != cv.scale {
                let dst = Rect::new(area.x, area.y, w as i32 / k, h as i32 / k);
                cv.with_clip(area, |cv| cv.draw_image(px, w, h, dst, 255));
                return (w as i32 / k, h as i32 / k);
            }
            // The top of the content joins the compositor strip squarely;
            // only the window's bottom corners belong to this surface.
            for (clip, radius) in [
                (Rect::new(area.x, area.y, area.w, corner), 0),
                (Rect::new(area.x, area.y + corner, area.w, (area.h - corner).max(0)), corner),
            ] {
                let clip = clip.intersect(&cv.clip);
                if clip.is_empty() {
                    continue;
                }
                // The opaque parts: the clip minus every glass region.
                let mut opaque = alloc::vec![clip];
                for g in &glass {
                    opaque = opaque.iter().flat_map(|p| p.subtract(g)).filter(|p| !p.is_empty()).collect();
                }
                for part in opaque {
                    cv.with_clip(part, |cv| cv.blit(px, w, h, area, radius));
                }
                // Each glass pixel once, even where regions overlap.
                for (i, g) in glass.iter().enumerate() {
                    let mut parts = alloc::vec![clip.intersect(g)];
                    for earlier in &glass[..i] {
                        parts = parts.iter().flat_map(|p| p.subtract(earlier)).filter(|p| !p.is_empty()).collect();
                    }
                    for part in parts {
                        cv.with_clip(part, |cv| cv.blit_alpha(px, w, h, area, radius));
                    }
                }
            }
            // The part covered, in points.
            (w as i32 / cv.scale, h as i32 / cv.scale)
        });
        // Until the client catches up with a resize, fill the uncovered part.
        let (w, h) = drawn.unwrap_or((0, 0));
        if w < area.w {
            let r = Rect::new(area.x + w, area.y, area.w - w, area.h);
            cv.with_clip(r, |cv| cv.fill_round_rect(area, corner, bg));
        }
        if h < area.h {
            let r = Rect::new(area.x, area.y + h, w.min(area.w), area.h - h);
            cv.with_clip(r, |cv| cv.fill_round_rect(area, corner, bg));
        }
    }
    fn key(&mut self, ev: &KeyEvent, _env: &mut Env) -> bool {
        server::push_event(self.id, ev.to_event(self.id));
        false
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        self.pointer(event::POINTER_DOWN, x, y, area, 1);
        false
    }
    fn double_click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        self.pointer(event::POINTER_DOWN, x, y, area, 2);
        false
    }
    fn right_click(&mut self, x: i32, y: i32, area: Rect, _env: &mut Env) -> bool {
        self.pointer_button(event::POINTER_DOWN, x, y, area, 1, 2);
        false
    }
    fn release(&mut self, x: i32, y: i32, area: Rect) {
        self.pointer(event::POINTER_UP, x, y, area, 0);
    }
    fn drag(&mut self, x: i32, y: i32, area: Rect) -> bool {
        self.pointer_button(event::POINTER_MOVE, x, y, area, 0, 1);
        false
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        if x >= 0 {
            self.pointer_button(event::POINTER_MOVE, x, y, area, 0, 0);
        } else {
            // Pointer left the window.
            server::push_event(self.id, Event { kind: event::POINTER_MOVE, x: -1, y: -1, ..Default::default() });
        }
        false
    }
    fn scroll(&mut self, notches: (i32, i32), precise: (i32, i32), _area: Rect) -> bool {
        server::push_event(
            self.id,
            Event {
                kind: event::SCROLL,
                a: notches.1 as u32,
                b: notches.0 as u32,
                x: precise.0,
                y: precise.1,
                ..Default::default()
            },
        );
        false
    }
    fn focus(&mut self, focused: bool) {
        // (The desktop says "lost" before "gained".)
        server::set_frontmost(focused.then_some(self.id));
        server::note_focus(self.id, focused);
        server::push_event(self.id, Event { kind: event::FOCUS, a: focused as u32, ..Default::default() });
    }
    fn request_close(&mut self) -> bool {
        server::push_event(self.id, Event { kind: event::CLOSE_REQUESTED, ..Default::default() });
        false
    }
    fn request_close_tab(&mut self) -> bool {
        server::push_event(self.id, Event { kind: event::CLOSE_REQUESTED, a: 1, ..Default::default() });
        false
    }
    fn client_id(&self) -> Option<u32> {
        Some(self.id)
    }
}
