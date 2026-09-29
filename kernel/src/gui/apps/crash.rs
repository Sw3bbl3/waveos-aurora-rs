//! "App quit unexpectedly" — shown when a user process is killed by a CPU fault.

use super::{hit, App, AppKind, Env, Request};
use crate::drivers::input::KeyCode;
use crate::gui::canvas::{rgb, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use crate::gui::widgets::{button, ButtonStyle};
use alloc::format;
use alloc::string::String;

pub struct CrashDialog {
    name: String,
    reason: String,
    reopen: Option<AppKind>,
    hovered: Option<usize>,
}

impl CrashDialog {
    pub fn new(name: String, reason: String, reopen: Option<AppKind>) -> Self {
        Self { name, reason, reopen, hovered: None }
    }

    fn buttons(&self, area: Rect) -> ([Rect; 2], usize) {
        let y = area.bottom() - 54;
        let ok = Rect::new(area.right() - 112, y, 92, 34);
        let reopen = Rect::new(area.right() - 220, y, 96, 34);
        ([ok, reopen], if self.reopen.is_some() { 2 } else { 1 })
    }
}

impl App for CrashDialog {
    fn kind(&self) -> AppKind {
        AppKind::Crash
    }
    fn title(&self) -> String {
        "Problem Report".into()
    }
    fn size(&self) -> (i32, i32) {
        (440, 190)
    }
    fn resizable(&self) -> bool {
        false
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        let icon = Rect::new(area.x + 24, area.y + 18, 44, 44);
        cv.fill_round_rect_dgradient(icon, 22, rgb(0xFF, 0x8A, 0x65), theme::CLOSE);
        cv.text_centered(icon, "!", theme::ui_bold(28), 0xFFFF_FFFF);
        let title = format!("“{}” quit unexpectedly.", self.name);
        cv.text_clipped(area.x + 84, area.y + 36, &title, theme::ui_bold(16), t.text, area.w - 104);
        cv.text(
            area.x + 84,
            area.y + 58,
            "Other apps and your files were not affected.",
            theme::ui(13),
            t.text_secondary,
        );
        cv.text_clipped(area.x + 84, area.y + 86, &self.reason, theme::mono(13), t.text_secondary, area.w - 104);
        let ([ok, reopen], n) = self.buttons(area);
        button(cv, ok, "OK", ButtonStyle::Primary, self.hovered == Some(0));
        if n == 2 {
            button(cv, reopen, "Reopen", ButtonStyle::Secondary, self.hovered == Some(1));
        }
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        let (rects, n) = self.buttons(area);
        match hit(&rects[..n], x, y) {
            Some(0) => env.requests.push(Request::Close),
            Some(1) => {
                env.requests.push(Request::Close);
                if let Some(k) = self.reopen {
                    env.requests.push(Request::Open(k));
                }
            }
            _ => {}
        }
        false
    }
    fn key(&mut self, ev: &crate::drivers::input::KeyEvent, env: &mut Env) -> bool {
        if ev.pressed && matches!(ev.code, KeyCode::Escape | KeyCode::Enter) {
            env.requests.push(Request::Close);
        }
        false
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let (rects, n) = self.buttons(area);
        let h = hit(&rects[..n], x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
}
