//! The administrator prompt: "<App> wants to make changes". Built into the
//! window server, so the app that asked never sees what is typed. It
//! checks an administrator's name and password with `users::db` (rate
//! limited) and answers `users::elevate`. Closing it, Cancel or Escape say
//! no; so does the window going away for any other reason.

use super::{hit, App, AppKind, Env, Request};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use crate::gui::widgets::{button, ButtonStyle};
use crate::users::{db, elevate};
use alloc::format;
use alloc::string::String;

pub struct ElevateDialog {
    req: elevate::Request,
    name: String,
    password: String,
    /// 0 = name, 1 = password.
    focus: usize,
    error: Option<String>,
    answered: bool,
    hovered: Option<usize>,
    /// The caret's blink phase last drawn.
    blink: bool,
}

impl ElevateDialog {
    pub fn new(req: elevate::Request) -> Self {
        // The logged-in account if it is an administrator, else the first one.
        let me = db::get(crate::users::session().uid).filter(|u| u.admin);
        let name = me.or_else(|| db::all().into_iter().find(|u| u.admin)).map(|u| u.name).unwrap_or_default();
        Self {
            req,
            focus: if name.is_empty() { 0 } else { 1 },
            name,
            password: String::new(),
            error: None,
            answered: false,
            hovered: None,
            blink: false,
        }
    }

    fn fields(&self, area: Rect) -> [Rect; 2] {
        let x = area.x + 84;
        let w = area.w - 104;
        [Rect::new(x, area.y + 116, w, 34), Rect::new(x, area.y + 172, w, 34)]
    }

    /// Allow, Cancel.
    fn buttons(&self, area: Rect) -> [Rect; 2] {
        let y = area.bottom() - 52;
        [Rect::new(area.right() - 112, y, 92, 34), Rect::new(area.right() - 214, y, 92, 34)]
    }

    fn say(&mut self, granted: bool, env: &mut Env) {
        if !self.answered {
            self.answered = true;
            elevate::answer(self.req.id, granted);
        }
        env.requests.push(Request::Close);
    }

    fn allow(&mut self, env: &mut Env) {
        let user = db::by_name(self.name.trim()).filter(|u| u.admin);
        let password = core::mem::take(&mut self.password);
        // Unknown names and non-administrators are checked against nobody
        // (the same work), so the answer doesn't tell them apart.
        let uid = user.as_ref().map_or(u32::MAX - 1, |u| u.uid);
        match db::verify(uid, &password) {
            Ok(()) if user.is_some() => {
                log!("users", "{} approved '{}' for pid {}", self.name.trim(), self.req.reason, self.req.pid);
                self.say(true, env);
            }
            Err(aurora_abi::err::EAGAIN) => {
                let s = db::wait_ms(uid).div_ceil(1000);
                self.error = Some(format!("Too many tries. Wait {s} s."));
            }
            _ => {
                self.error = Some(String::from("That isn't an administrator's name and password."));
                crate::drivers::audio::play_sound("error");
            }
        }
        self.focus = 1;
    }
}

impl Drop for ElevateDialog {
    fn drop(&mut self) {
        if !self.answered {
            elevate::answer(self.req.id, false);
        }
    }
}

impl App for ElevateDialog {
    fn kind(&self) -> AppKind {
        AppKind::Elevate
    }
    fn title(&self) -> String {
        "Administrator Approval".into()
    }
    fn size(&self) -> (i32, i32) {
        (460, 290)
    }
    fn resizable(&self) -> bool {
        false
    }
    fn elevation(&self) -> Option<u64> {
        Some(self.req.id)
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        // A padlock on the accent gradient.
        let icon = Rect::new(area.x + 24, area.y + 18, 44, 44);
        cv.fill_round_rect_dgradient(icon, 22, theme::ACCENT, theme::ACCENT_2);
        let (cx, cy) = icon.center();
        cv.stroke_arc(cx * 16, (cy - 3) * 16, 7 * 16, 30, 512, 1024, 0xFFFF_FFFF);
        cv.fill_round_rect(Rect::new(cx - 10, cy - 3, 20, 15), 4, 0xFFFF_FFFF);
        let title = format!("“{}” wants to make changes.", self.req.app);
        cv.text_clipped(area.x + 84, area.y + 36, &title, theme::ui_bold(16), t.text, area.w - 104);
        let what = if self.req.reason.is_empty() {
            String::from("A change for every account")
        } else {
            // "change the clock" → "Change the clock"
            let mut c = self.req.reason.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        };
        cv.text_clipped(area.x + 84, area.y + 58, &what, theme::ui(13), t.text_secondary, area.w - 104);
        cv.text(
            area.x + 84,
            area.y + 80,
            "Enter an administrator's name and password to allow this.",
            theme::ui(12),
            t.text_secondary,
        );
        let caret = env.focused && (env.now_ms / 530).is_multiple_of(2);
        let fields = self.fields(area);
        for (i, (r, label)) in fields.iter().zip(["Name", "Password"]).enumerate() {
            cv.text(r.x, r.y - 7, label, theme::ui_bold(12), t.text_secondary);
            cv.fill_round_rect(*r, 10, with_alpha(t.text, if t.dark { 0x12 } else { 0x0B }));
            let focused = self.focus == i;
            cv.stroke_round_rect(
                *r,
                10,
                if focused { with_alpha(theme::accent(), 0xC0) } else { with_alpha(t.text, 0x18) },
            );
            let shown: String = if i == 1 { self.password.chars().map(|_| '•').collect() } else { self.name.clone() };
            let end = cv.text_clipped(r.x + 12, r.y + 23, &shown, theme::ui(14), t.text, r.w - 24);
            if focused && caret {
                cv.fill_rect(Rect::new(r.x + 13 + end, r.y + 8, 2, 18), theme::accent_glow());
            }
        }
        let [allow, cancel] = self.buttons(area);
        if let Some(e) = &self.error {
            cv.text_clipped(area.x + 24, allow.y + 22, e, theme::ui_bold(12), 0xFFFF_6B6B, cancel.x - area.x - 36);
        }
        button(cv, allow, "Allow", ButtonStyle::Primary, self.hovered == Some(0));
        button(cv, cancel, "Cancel", ButtonStyle::Secondary, self.hovered == Some(1));
    }
    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(i) = hit(&self.fields(area), x, y) {
            self.focus = i;
            return true;
        }
        match hit(&self.buttons(area), x, y) {
            Some(0) => self.allow(env),
            Some(1) => self.say(false, env),
            _ => {}
        }
        true
    }
    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        match ev.code {
            KeyCode::Escape => self.say(false, env),
            KeyCode::Enter if self.focus == 0 => self.focus = 1,
            KeyCode::Enter => self.allow(env),
            KeyCode::Tab => self.focus = 1 - self.focus,
            KeyCode::Backspace => {
                if self.focus == 0 {
                    self.name.pop();
                } else {
                    self.password.pop();
                }
            }
            _ => match ev.ch {
                Some(c) if !c.is_control() && !ev.mods.ctrl && !ev.mods.super_key => {
                    let field = if self.focus == 0 { &mut self.name } else { &mut self.password };
                    if field.chars().count() < 64 {
                        field.push(c);
                    }
                    self.error = None;
                }
                _ => return false,
            },
        }
        true
    }
    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = hit(&self.buttons(area), x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
    fn tick(&mut self, env: &mut Env) -> bool {
        // The caret blinks.
        let phase = (env.now_ms / 530).is_multiple_of(2);
        core::mem::replace(&mut self.blink, phase) != phase
    }
    fn request_close(&mut self) -> bool {
        if !self.answered {
            self.answered = true;
            elevate::answer(self.req.id, false);
        }
        true
    }
}
