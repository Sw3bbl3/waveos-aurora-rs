//! Notifications and Drift.
//!
//! A new notification rises out of Crest for a few seconds (see `bar`);
//! clicking it brings its app forward. Clicking Crest opens Drift: today's
//! date, a month calendar, Calm (do not disturb) and the history.

use super::Desktop;
use crate::gui::apps;
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::notify::{self, Notification};
use crate::gui::settings;
use crate::gui::theme::{self, MENUBAR_H};
use crate::gui::widgets;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const SLIDE_MS: u64 = 240;
const CENTER_W: i32 = 700;
const CENTER_H: i32 = 420;
const LEFT_W: i32 = 300;
const HISTORY: usize = 50;
const CARD_H: i32 = 70;

pub struct Center {
    opened_at: u64,
    closing_at: Option<u64>,
    /// Month shown relative to the current one.
    month_offset: i32,
    hover: Option<CenterHit>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CenterHit {
    PrevMonth,
    NextMonth,
    Dnd,
    Clear,
    Card(usize),
}

/// Ease-out cubic on 0..=1000.
fn ease_out(t: i64) -> i64 {
    let t = t.clamp(0, 1000);
    let inv = 1000 - t;
    1000 - inv * inv / 1000 * inv / 1000
}

pub(super) fn icon_for(path: &str) -> Icon {
    apps::by_path(path).map(|a| a.icon).unwrap_or(Icon::Aurora)
}

fn app_name(path: &str) -> String {
    match apps::by_path(path) {
        Some(a) => String::from(a.name),
        None if path.is_empty() => String::from("WaveOS Aurora"),
        None => String::from(path.rsplit('/').next().unwrap_or(path).trim_end_matches(".elf")),
    }
}

/// Day of week (0 = Sunday) — Sakamoto's method.
fn weekday(y: i32, m: u32, d: u32) -> u32 {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let y = if m < 3 { y - 1 } else { y };
    ((y + y / 4 - y / 100 + y / 400 + T[(m - 1) as usize] + d as i32).rem_euclid(7)) as u32
}

fn days_in(y: i32, m: u32) -> u32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

impl Desktop {
    /// Takes new notifications from apps and the system: each rises out of
    /// Crest (unless Calm is on or Drift is open) and joins the history.
    pub(super) fn take_notifications(&mut self) {
        let dnd = settings::get_bool("dnd", false);
        let mut chime = false;
        while let Some(n) = notify::take() {
            // Apps' notifications chime; the system's own (screenshots, …) have their own sounds.
            chime |= !dnd && n.pid != 0;
            if !dnd && self.center.is_none() {
                self.notice(n.clone());
            }
            self.history.insert(0, n);
            self.history.truncate(HISTORY);
            if self.center.is_some() {
                let r = self.center_rect();
                self.damage(r);
            }
        }
        if chime {
            crate::drivers::audio::play_sound("notify");
        }
    }

    /// True while Drift slides.
    pub(super) fn notifications_moving(&self) -> bool {
        self.center.as_ref().is_some_and(|c| self.now_ms < c.opened_at + SLIDE_MS || c.closing_at.is_some())
    }

    pub(super) fn notification_deadline(&self) -> u64 {
        u64::MAX
    }

    pub(super) fn step_notifications(&mut self) -> bool {
        let now = self.now_ms;
        let mut moving = false;
        if let Some(c) = &self.center {
            let done_closing = c.closing_at.is_some_and(|t| now >= t + SLIDE_MS);
            let animating = now < c.opened_at + SLIDE_MS || c.closing_at.is_some();
            if animating {
                let r = self.center_area();
                self.damage(r);
                moving = true;
            }
            if done_closing {
                self.center = None;
            }
        }
        moving
    }

    /// Focuses the app that posted `n` (or opens it).
    pub(super) fn activate(&mut self, n: &Notification) {
        if n.pid != 0 {
            let win = self
                .windows
                .iter()
                .rev()
                .find(|w| w.app.client_id().and_then(crate::gui::server::pid_of) == Some(n.pid));
            if let Some(id) = win.map(|w| w.id) {
                self.raise(id);
                return;
            }
        }
        if let Some(a) = apps::by_path(&n.app_path) {
            self.open(a.kind);
        }
    }

    /// Icon, app name, time, title and body of one notification.
    fn paint_card(&self, cv: &mut Canvas, r: Rect, n: &Notification) {
        let t = theme::current();
        icons::draw(cv, icon_for(&n.app_path), Rect::new(r.x + 14, r.y + (r.h - 36) / 2, 36, 36));
        let x = r.x + 62;
        let w = r.w - 62 - 14;
        let small = theme::ui(11);
        let app = app_name(&n.app_path).to_uppercase();
        cv.text_clipped(x, r.y + 20, &app, small, t.text_secondary, w - 70);
        let tw = small.width(&n.time);
        cv.text(r.right() - 14 - tw, r.y + 20, &n.time, small, t.text_secondary);
        cv.text_clipped(x, r.y + 38, &n.title, theme::ui_bold(13), t.text, w);
        cv.text_clipped(x, r.y + 56, &n.body, theme::ui(12), t.text, w);
    }

    // ----------------------------------------------- notification center

    /// Drift drops from Crest: two columns, the calendar and Calm on the
    /// left, the notifications on the right.
    fn center_rect(&self) -> Rect {
        let now = self.now_ms;
        let (open, close) = match &self.center {
            Some(c) => (
                ease_out(((now.saturating_sub(c.opened_at)) * 1000 / SLIDE_MS) as i64),
                c.closing_at.map(|t| ease_out(((now.saturating_sub(t)) * 1000 / SLIDE_MS) as i64)).unwrap_or(0),
            ),
            None => (0, 0),
        };
        // Grows out of Crest: narrower and higher while it opens.
        let p = (open - close).clamp(0, 1000) as i32;
        let w = CENTER_W - (1000 - p) * 120 / 1000;
        let h = CENTER_H - (1000 - p) * 60 / 1000;
        let y = MENUBAR_H + 4 - (1000 - p) * 14 / 1000;
        Rect::new((self.w - w) / 2, y, w, h)
    }

    fn center_area(&self) -> Rect {
        Rect::new((self.w - CENTER_W) / 2 - 50, MENUBAR_H - 30, CENTER_W + 100, CENTER_H + 90)
    }

    pub(super) fn toggle_center(&mut self) {
        let now = self.now_ms;
        match &mut self.center {
            Some(c) if c.closing_at.is_none() => c.closing_at = Some(now),
            Some(_) => {}
            None => {
                self.center = Some(Center { opened_at: now, closing_at: None, month_offset: 0, hover: None });
            }
        }
        let a = self.center_area();
        self.damage(a);
        self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
    }

    pub(super) fn center_open(&self) -> bool {
        self.center.as_ref().is_some_and(|c| c.closing_at.is_none())
    }

    fn center_columns(&self) -> (Rect, Rect) {
        let p = self.center_rect();
        let left = Rect::new(p.x + 18, p.y + 18, LEFT_W, p.h - 36);
        let right = Rect::new(left.right() + 36, p.y + 18, p.right() - 18 - (left.right() + 36), p.h - 36);
        (left, right)
    }

    /// Layout of the panel's interactive parts.
    fn center_hits(&self) -> Vec<(CenterHit, Rect)> {
        let (left, right) = self.center_columns();
        let cal = Rect::new(left.x, left.y + 66, left.w, 252);
        let mut v = alloc::vec![
            (CenterHit::PrevMonth, Rect::new(cal.right() - 64, cal.y + 12, 26, 26)),
            (CenterHit::NextMonth, Rect::new(cal.right() - 34, cal.y + 12, 26, 26)),
            (CenterHit::Dnd, Rect::new(left.x, cal.bottom() + 12, left.w, 52)),
            (CenterHit::Clear, Rect::new(right.right() - 60, right.y + 4, 60, 26)),
        ];
        for (i, _) in self.history.iter().enumerate() {
            let r = Rect::new(right.x, right.y + 44 + i as i32 * (CARD_H + 8), right.w, CARD_H);
            if r.bottom() > right.bottom() {
                break;
            }
            v.push((CenterHit::Card(i), r));
        }
        v
    }

    /// Returns true if the panel consumed the click.
    pub(super) fn center_press(&mut self, x: i32, y: i32) -> bool {
        if !self.center_open() {
            return false;
        }
        let p = self.center_rect();
        if !p.contains(x, y) {
            self.toggle_center();
            // Let a click on the clock only close (not reopen) the panel.
            return y < MENUBAR_H;
        }
        let hit = self.center_hits().into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h);
        match hit {
            Some(CenterHit::PrevMonth) => self.center.as_mut().unwrap().month_offset -= 1,
            Some(CenterHit::NextMonth) => self.center.as_mut().unwrap().month_offset += 1,
            Some(CenterHit::Dnd) => settings::set_bool("dnd", !settings::get_bool("dnd", false)),
            Some(CenterHit::Clear) => self.history.clear(),
            Some(CenterHit::Card(i)) => {
                if let Some(n) = self.history.get(i).cloned() {
                    self.toggle_center();
                    self.activate(&n);
                }
            }
            None => {}
        }
        let a = self.center_area();
        self.damage(a);
        true
    }

    pub(super) fn update_center_hover(&mut self, x: i32, y: i32) {
        if !self.center_open() {
            return;
        }
        let h = self.center_hits().into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h);
        if let Some(c) = &mut self.center {
            if c.hover != h {
                c.hover = h;
                let a = self.center_area();
                self.damage(a);
            }
        }
    }

    pub(super) fn paint_center(&self, cv: &mut Canvas) {
        let Some(c) = &self.center else { return };
        if !self.center_area().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        let p = self.center_rect();
        cv.shadow(p, 24, 34, 10, t.shadow);
        cv.frosted(p, 24, t.panel_tint);
        cv.stroke_round_rect(p, 24, if t.dark { 0x24FF_FFFF } else { 0x80FF_FFFF });
        if p.w < CENTER_W - 40 {
            // Still growing out of Crest: just the shell.
            return;
        }
        let (left, right) = self.center_columns();
        let d = crate::drivers::rtc::now();
        let wd = weekday(d.year as i32, d.month as u32, d.day as u32) as usize;
        cv.text(left.x + 4, left.y + 26, DAYS[wd], theme::display(24), t.text);
        let date = format!("{} {}, {}", MONTHS[(d.month.clamp(1, 12) - 1) as usize], d.day, d.year);
        cv.text(left.x + 4, left.y + 50, &date, theme::ui(14), t.text_secondary);
        cv.fill_rect(Rect::new(left.right() + 18, p.y + 22, 1, p.h - 44), t.separator);

        // Calendar.
        let cal = Rect::new(left.x, left.y + 66, left.w, 252);
        cv.fill_round_rect(cal, 18, with_alpha(t.text, 0x0A));
        let months = d.year as i32 * 12 + d.month as i32 - 1 + c.month_offset;
        let (y, m) = (months.div_euclid(12), (months.rem_euclid(12) + 1) as u32);
        let title = format!("{} {}", MONTHS[(m - 1) as usize], y);
        cv.text(cal.x + 16, cal.y + 30, &title, theme::ui_bold(15), t.text);
        for (hit, r) in self.center_hits() {
            let hovered = c.hover == Some(hit);
            match hit {
                CenterHit::PrevMonth | CenterHit::NextMonth => {
                    if hovered {
                        cv.fill_round_rect(r, 13, t.hover);
                    }
                    let (cx, cy) = r.center();
                    let dd = if hit == CenterHit::PrevMonth { 1 } else { -1 };
                    cv.line(cx + 3 * dd, cy - 5, cx - 2 * dd, cy, 2, t.text);
                    cv.line(cx - 2 * dd, cy, cx + 3 * dd, cy + 5, 2, t.text);
                }
                CenterHit::Dnd => {
                    let on = settings::get_bool("dnd", false);
                    cv.fill_round_rect(r, 18, if hovered { t.hover } else { with_alpha(t.text, 0x0A) });
                    let dot = Rect::new(r.x + 10, r.y + 10, 32, 32);
                    cv.fill_round_rect(dot, 16, if on { theme::accent() } else { with_alpha(t.text, 0x18) });
                    // A crescent: Calm.
                    let (cx, cy) = dot.center();
                    let ink = if on { 0xFFFF_FFFF } else { t.text };
                    let bg = cv.buf[(cy * cv.width + cx) as usize];
                    cv.fill_circle(cx, cy, 8, ink);
                    cv.fill_circle(cx + 4, cy - 3, 7, bg);
                    cv.text(r.x + 52, r.y + 22, "Calm", theme::ui_bold(13), t.text);
                    cv.text(
                        r.x + 52,
                        r.y + 40,
                        if on { "Notifications wait" } else { "Off" },
                        theme::ui(12),
                        t.text_secondary,
                    );
                    widgets::toggle(cv, Rect::new(r.right() - 54, r.y + 15, 42, 22), on);
                }
                CenterHit::Clear if !self.history.is_empty() => {
                    if hovered {
                        cv.fill_round_rect(r, 13, t.hover);
                    }
                    cv.text_centered(r, "Clear", theme::ui_bold(12), theme::accent());
                }
                _ => {}
            }
        }
        let f = theme::ui(11);
        let cell_w = (cal.w - 24) / 7;
        for (k, name) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
            let r = Rect::new(cal.x + 12 + k as i32 * cell_w, cal.y + 44, cell_w, 20);
            cv.text_centered(r, name, f, t.text_secondary);
        }
        let first = weekday(y, m, 1) as i32;
        let today = (c.month_offset == 0).then_some(d.day as u32);
        let body = theme::ui(13);
        for day in 1..=days_in(y, m) {
            let slot = first + day as i32 - 1;
            let r = Rect::new(cal.x + 12 + (slot % 7) * cell_w, cal.y + 68 + (slot / 7) * 29, cell_w, 28);
            let label = format!("{day}");
            if today == Some(day) {
                let (cx, cy) = r.center();
                cv.fill_circle(cx, cy, 13, theme::accent());
                cv.text_centered(r, &label, theme::ui_bold(13), 0xFFFF_FFFF);
            } else {
                let weekend = slot % 7 == 0 || slot % 7 == 6;
                cv.text_centered(r, &label, body, if weekend { t.text_secondary } else { t.text });
            }
        }

        // Notifications.
        cv.text(right.x + 4, right.y + 22, "Notifications", theme::ui_bold(15), t.text);
        if self.history.is_empty() {
            let r = Rect::new(right.x, right.y + 40, right.w, right.h - 40);
            let (cx, cy) = r.center();
            // Still water: three calm lines.
            for (k, w) in [44, 30, 18].iter().enumerate() {
                let yy = cy - 34 + k as i32 * 9;
                cv.fill_round_rect(
                    Rect::new(cx - w / 2, yy, *w, 3),
                    2,
                    with_alpha(theme::waterline(), 0x90 - k as u8 * 0x28),
                );
            }
            cv.text_centered(Rect::new(r.x, cy - 6, r.w, 20), "All quiet", theme::ui_bold(14), t.text);
            cv.text_centered(
                Rect::new(r.x, cy + 14, r.w, 20),
                "New notifications will drift in here.",
                theme::ui(12),
                t.text_secondary,
            );
        }
        for (hit, r) in self.center_hits() {
            if let CenterHit::Card(i) = hit {
                let bg = if c.hover == Some(hit) { with_alpha(t.text, 0x16) } else { with_alpha(t.text, 0x0A) };
                cv.fill_round_rect(r, 16, bg);
                self.paint_card(cv, r, &self.history[i]);
            }
        }
    }
}
