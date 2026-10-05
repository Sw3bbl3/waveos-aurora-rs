//! A window owned by a user process.

use super::{App, AppKind, Env};
use crate::drivers::input::KeyEvent;
use crate::gui::canvas::Canvas;
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
    /// Content size last reported to the client (to detect resizes).
    reported: (i32, i32),
}

impl ClientApp {
    pub fn new(id: u32, kind: AppKind) -> Option<ClientApp> {
        let (_, _, resizable, initial) = server::info(id)?;
        Some(ClientApp { id, kind, resizable, initial, reported: initial })
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
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        if (area.w, area.h) != self.reported {
            self.reported = (area.w, area.h);
            server::push_event(self.id, Event { kind: event::RESIZE, x: area.w, y: area.h, ..Default::default() });
        }
        let bg = theme::current().window_bg;
        let glass = !self.materials().is_empty();
        let drawn = server::with_surface(self.id, |px, w, h| {
            if glass {
                cv.blit_alpha(px, w, h, area, WINDOW_RADIUS);
            } else {
                cv.blit(px, w, h, area, WINDOW_RADIUS);
            }
            (w as i32, h as i32)
        });
        // Until the client catches up with a resize, fill the uncovered part.
        let (w, h) = drawn.unwrap_or((0, 0));
        if w < area.w {
            let r = Rect::new(area.x + w, area.y, area.w - w, area.h);
            cv.with_clip(r, |cv| cv.fill_rect_round_bottom(area, WINDOW_RADIUS, bg));
        }
        if h < area.h {
            let r = Rect::new(area.x, area.y + h, w.min(area.w), area.h - h);
            cv.with_clip(r, |cv| cv.fill_rect_round_bottom(area, WINDOW_RADIUS, bg));
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
    fn scroll(&mut self, delta: i32, _area: Rect) -> bool {
        server::push_event(self.id, Event { kind: event::SCROLL, a: delta as u32, ..Default::default() });
        false
    }
    fn focus(&mut self, focused: bool) {
        server::push_event(self.id, Event { kind: event::FOCUS, a: focused as u32, ..Default::default() });
    }
    fn request_close(&mut self) -> bool {
        server::push_event(self.id, Event { kind: event::CLOSE_REQUESTED, ..Default::default() });
        false
    }
    fn client_id(&self) -> Option<u32> {
        Some(self.id)
    }
}
