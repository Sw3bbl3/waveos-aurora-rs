//! The privacy prompt: "“App” would like to use the microphone." Built
//! into the window server like the administrator prompt; it answers
//! `sandbox::consent`, which remembers the answer for the app and account.
//! Closing it or Escape say no without remembering it.

use super::{hit, App, AppKind, Env, Request};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::theme;
use crate::gui::widgets::{button, ButtonStyle};
use crate::sandbox::consent;
use alloc::format;
use alloc::string::String;
use aurora_abi::sandbox as cap;

pub struct ConsentDialog {
    req: consent::Request,
    answered: bool,
    hovered: Option<usize>,
}

impl ConsentDialog {
    pub fn new(req: consent::Request) -> Self {
        Self { req, answered: false, hovered: None }
    }

    /// Allow, Don't Allow.
    fn buttons(&self, area: Rect) -> [Rect; 2] {
        let y = area.bottom() - 52;
        [Rect::new(area.right() - 112, y, 92, 34), Rect::new(area.right() - 248, y, 126, 34)]
    }

    /// None: dismissed (the app is refused this time and asks again).
    fn say(&mut self, allowed: Option<bool>, env: &mut Env) {
        if !self.answered {
            self.answered = true;
            consent::answer(self.req.id, allowed == Some(true), allowed.is_some());
        }
        env.requests.push(Request::Close);
    }
}

impl Drop for ConsentDialog {
    fn drop(&mut self) {
        if !self.answered {
            consent::answer(self.req.id, false, false);
        }
    }
}

/// A small picture of what is asked for, white on the accent.
fn glyph(cv: &mut Canvas, icon: Rect, bit: u64) {
    let (cx, cy) = icon.center();
    let white = 0xFFFF_FFFF;
    match bit {
        cap::MICROPHONE => {
            cv.fill_round_rect(Rect::new(cx - 6, cy - 14, 12, 20), 6, white);
            cv.stroke_arc(cx * 16, (cy - 2) * 16, 10 * 16, 30, 0, 512, white);
            cv.fill_rect(Rect::new(cx - 1, cy + 8, 2, 6), white);
            cv.fill_rect(Rect::new(cx - 6, cy + 13, 12, 2), white);
        }
        cap::CAMERA => {
            cv.fill_round_rect(Rect::new(cx - 14, cy - 8, 20, 16), 4, white);
            cv.fill_rect(Rect::new(cx + 7, cy - 4, 7, 8), white);
        }
        cap::SCREEN_CAPTURE => {
            cv.stroke_round_rect(Rect::new(cx - 14, cy - 11, 28, 19), 3, white);
            cv.fill_rect(Rect::new(cx - 6, cy + 11, 12, 2), white);
        }
        _ => {
            // A folder.
            cv.fill_round_rect(Rect::new(cx - 14, cy - 10, 12, 6), 2, white);
            cv.fill_round_rect(Rect::new(cx - 14, cy - 7, 28, 18), 3, white);
        }
    }
}

impl App for ConsentDialog {
    fn kind(&self) -> AppKind {
        AppKind::Consent
    }
    fn title(&self) -> String {
        "Privacy".into()
    }
    fn size(&self) -> (i32, i32) {
        (480, 214)
    }
    fn resizable(&self) -> bool {
        false
    }
    fn consent(&self) -> Option<u64> {
        Some(self.req.id)
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        let icon = Rect::new(area.x + 24, area.y + 20, 44, 44);
        cv.fill_round_rect_dgradient(icon, 22, theme::ACCENT, theme::ACCENT_2);
        glyph(cv, icon, self.req.bit);
        let x = area.x + 84;
        let w = area.w - 104;
        let title = format!("“{}” would like to", self.req.name);
        cv.text_clipped(x, area.y + 36, &title, theme::ui_bold(16), t.text, w);
        cv.text_clipped(x, area.y + 58, &format!("{}.", consent::question(self.req.bit)), theme::ui_bold(16), t.text, w);
        let what = cap::name(self.req.bit).map(gina::describe_permission).unwrap_or("");
        cv.text_clipped(x, area.y + 84, what, theme::ui(13), t.text_secondary, w);
        cv.text_clipped(
            x,
            area.y + 106,
            "You can change this in Settings, Privacy & Security.",
            theme::ui(12),
            t.text_secondary,
            w,
        );
        let [allow, deny] = self.buttons(area);
        button(cv, allow, "Allow", ButtonStyle::Primary, self.hovered == Some(0));
        button(cv, deny, "Don't Allow", ButtonStyle::Secondary, self.hovered == Some(1));
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        match hit(&self.buttons(area), x, y) {
            Some(0) => self.say(Some(true), env),
            Some(1) => self.say(Some(false), env),
            _ => {}
        }
        true
    }
    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        match ev.code {
            KeyCode::Escape => self.say(None, env),
            _ => return false,
        }
        true
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = hit(&self.buttons(area), x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
    fn request_close(&mut self) -> bool {
        if !self.answered {
            self.answered = true;
            consent::answer(self.req.id, false, false);
        }
        true
    }
}
