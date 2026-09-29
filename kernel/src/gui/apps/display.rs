//! "Keep this resolution?" — shown after a display mode change, reverting on
//! its own if the user can't see or answer it.

use super::{hit, App, AppKind, Env, Request};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::theme;
use crate::gui::widgets::{button, ButtonStyle};
use alloc::format;
use alloc::string::String;

const SECONDS: u64 = 15;

pub struct DisplayConfirm {
    started: Option<u64>,
    left: u64,
    hovered: Option<usize>,
    mode: (i32, i32),
}

impl DisplayConfirm {
    pub fn new(mode: (i32, i32)) -> Self {
        Self { started: None, left: SECONDS, hovered: None, mode }
    }

    fn buttons(area: Rect) -> [Rect; 2] {
        let y = area.bottom() - 54;
        [Rect::new(area.right() - 120, y, 100, 34), Rect::new(area.right() - 232, y, 100, 34)]
    }
}

impl App for DisplayConfirm {
    fn kind(&self) -> AppKind {
        AppKind::DisplayConfirm
    }
    fn title(&self) -> String {
        String::from("Display")
    }
    fn size(&self) -> (i32, i32) {
        (420, 160)
    }
    fn resizable(&self) -> bool {
        false
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        let title = format!("Keep {} × {}?", self.mode.0, self.mode.1);
        cv.text(area.x + 22, area.y + 36, &title, theme::ui_bold(16), t.text);
        let msg = format!("The previous resolution comes back in {} seconds.", self.left);
        cv.text(area.x + 22, area.y + 60, &msg, theme::ui(13), t.text_secondary);
        let [keep, revert] = Self::buttons(area);
        button(cv, keep, "Keep", ButtonStyle::Primary, self.hovered == Some(0));
        button(cv, revert, "Revert", ButtonStyle::Secondary, self.hovered == Some(1));
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        match hit(&Self::buttons(area), x, y) {
            Some(0) => env.requests.push(Request::KeepDisplay),
            Some(1) => env.requests.push(Request::RevertDisplay),
            _ => {}
        }
        false
    }
    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if ev.pressed {
            match ev.code {
                KeyCode::Enter => env.requests.push(Request::KeepDisplay),
                KeyCode::Escape => env.requests.push(Request::RevertDisplay),
                _ => {}
            }
        }
        false
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = hit(&Self::buttons(area), x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
    fn tick(&mut self, env: &mut Env) -> bool {
        let start = *self.started.get_or_insert(env.now_ms);
        let left = SECONDS.saturating_sub((env.now_ms - start) / 1000);
        if left == 0 {
            env.requests.push(Request::RevertDisplay);
        }
        core::mem::replace(&mut self.left, left) != left
    }
    fn request_close(&mut self) -> bool {
        false // closing means "revert"; handled by the Revert request
    }
}
