use super::{hit, App, AppKind, Env, Request};
use crate::drivers::input::KeyCode;
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::icons;
use crate::gui::theme;
use crate::gui::widgets::{button, ButtonStyle};

pub struct PowerDialog {
    hovered: Option<usize>,
    /// The firmware supports sleep (S3).
    can_sleep: bool,
}

impl PowerDialog {
    pub fn new() -> Self {
        Self { hovered: None, can_sleep: crate::power::s3::available() }
    }
    /// Shut Down, Restart, Cancel, and Sleep (an empty rect when unavailable).
    fn buttons(&self, area: Rect) -> [Rect; 4] {
        let y = area.bottom() - 56;
        [
            Rect::new(area.right() - 132, y, 112, 34),
            Rect::new(area.right() - 256, y, 112, 34),
            Rect::new(area.x + 20, y, 96, 34),
            if self.can_sleep { Rect::new(area.right() - 380, y, 112, 34) } else { Rect::default() },
        ]
    }
}

impl App for PowerDialog {
    fn kind(&self) -> AppKind {
        AppKind::Power
    }
    fn title(&self) -> alloc::string::String {
        "Power".into()
    }
    fn size(&self) -> (i32, i32) {
        (if self.can_sleep { 520 } else { 400 }, 170)
    }
    fn resizable(&self) -> bool {
        false
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        let c = Rect::new(area.x + 24, area.y + 16, 44, 44);
        cv.fill_round_rect_dgradient(c, 22, theme::ACCENT, theme::ACCENT_2);
        icons::power(
            cv,
            c.x + 22,
            c.y + 23,
            20,
            0xFFFF_FFFF,
            crate::gui::canvas::mix(theme::ACCENT, theme::ACCENT_2, 128),
        );
        let question = if self.can_sleep { "Shut down, restart or sleep?" } else { "Shut down or restart?" };
        cv.text(area.x + 84, area.y + 34, question, theme::ui_bold(16), t.text);
        cv.text(area.x + 84, area.y + 56, "Your files are saved to disk first.", theme::ui(13), t.text_secondary);
        let [shut, restart, cancel, sleep] = self.buttons(area);
        button(cv, shut, "Shut Down", ButtonStyle::Danger, self.hovered == Some(0));
        button(cv, restart, "Restart", ButtonStyle::Secondary, self.hovered == Some(1));
        button(cv, cancel, "Cancel", ButtonStyle::Secondary, self.hovered == Some(2));
        if self.can_sleep {
            button(cv, sleep, "Sleep", ButtonStyle::Secondary, self.hovered == Some(3));
        }
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        match hit(&self.buttons(area), x, y) {
            Some(0) => env.requests.push(Request::Shutdown),
            Some(1) => env.requests.push(Request::Reboot),
            Some(2) => env.requests.push(Request::Close),
            Some(3) => env.requests.push(Request::Sleep),
            _ => {}
        }
        false
    }
    fn key(&mut self, ev: &crate::drivers::input::KeyEvent, env: &mut Env) -> bool {
        if ev.pressed && ev.code == KeyCode::Escape {
            env.requests.push(Request::Close);
        }
        false
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = hit(&self.buttons(area), x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
}
