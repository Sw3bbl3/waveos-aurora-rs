//! Notification banners and the Notification Center (with the calendar).
//!
//! Banners slide in at the top right and leave after a few seconds; clicking
//! one brings its app forward. Clicking the clock in the menu bar opens the
//! Notification Center: today's date, a month calendar, Do Not Disturb, and
//! the notification history.

use super::Desktop;
use crate::gui::apps;
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::notify::{self, Notification};
use crate::gui::settings;
use crate::gui::theme::{self, DOCK_H, DOCK_MARGIN, MENUBAR_H};
use crate::gui::widgets;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const BANNER_W: i32 = 344;
const BANNER_H: i32 = 72;
const BANNER_MS: u64 = 5500;
const SLIDE_MS: u64 = 240;
const CENTER_W: i32 = 364;
const HISTORY: usize = 50;
const CARD_H: i32 = 70;

pub struct Banner {
    n: Notification,
    shown_at: u64,
    leaving_at: Option<u64>,
}

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

fn icon_for(path: &str) -> Icon {
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
    // ------------------------------------------------------------ banners

    fn banner_rect(&self, index: usize, b: &Banner) -> Rect {
        let now = self.now_ms;
        let enter = ease_out(((now.saturating_sub(b.shown_at)) * 1000 / SLIDE_MS) as i64);
        let leave = b.leaving_at.map(|t| ease_out(((now.saturating_sub(t)) * 1000 / SLIDE_MS) as i64)).unwrap_or(0);
        let slide = ((1000 - enter) + leave) * (BANNER_W as i64 + 24) / 1000;
        let x = self.w - BANNER_W - 12 + slide as i32;
        Rect::new(x, MENUBAR_H + 10 + index as i32 * (BANNER_H + 10), BANNER_W, BANNER_H)
    }

    fn banners_area(&self) -> Rect {
        let n = self.banners.len().max(1) as i32;
        Rect::new(self.w - BANNER_W - 60, MENUBAR_H, BANNER_W + 60, n * (BANNER_H + 10) + 40)
    }

    /// Takes new notifications from apps and the system.
    pub(super) fn take_notifications(&mut self) {
        let dnd = settings::get_bool("dnd", false);
        while let Some(n) = notify::take() {
            if !dnd && self.center.is_none() {
                // At most three at once: the oldest leaves.
                if self.banners.iter().filter(|b| b.leaving_at.is_none()).count() >= 3 {
                    if let Some(b) = self.banners.iter_mut().find(|b| b.leaving_at.is_none()) {
                        b.leaving_at = Some(self.now_ms);
                    }
                }
                self.banners.push(Banner { n: n.clone(), shown_at: self.now_ms, leaving_at: None });
            }
            self.history.insert(0, n);
            self.history.truncate(HISTORY);
            let a = self.banners_area();
            self.damage(a);
            if self.center.is_some() {
                let r = self.center_rect();
                self.damage(r);
            }
        }
    }

    /// Advances banner and panel animations; true while something moves.
    pub(super) fn step_notifications(&mut self) -> bool {
        let now = self.now_ms;
        let mut moving = false;
        for b in &mut self.banners {
            if b.leaving_at.is_none() && now >= b.shown_at + BANNER_MS && !(self.banner_hover == Some(b.n.id)) {
                b.leaving_at = Some(now);
            }
        }
        let before = self.banners.len();
        self.banners.retain(|b| b.leaving_at.is_none_or(|t| now < t + SLIDE_MS));
        if before != self.banners.len() {
            moving = true;
        }
        for b in &self.banners {
            if now < b.shown_at + SLIDE_MS || b.leaving_at.is_some() {
                moving = true;
            }
        }
        if moving || !self.banners.is_empty() && before != self.banners.len() {
            let a = self.banners_area();
            self.damage(a);
        }
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

    pub(super) fn banner_at(&self, x: i32, y: i32) -> Option<usize> {
        self.banners
            .iter()
            .enumerate()
            .position(|(i, b)| b.leaving_at.is_none() && self.banner_rect(i, b).contains(x, y))
    }

    /// Focuses the app that posted `n` (or opens it).
    fn activate(&mut self, n: &Notification) {
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

    pub(super) fn banner_click(&mut self, i: usize) {
        let now = self.now_ms;
        let n = self.banners[i].n.clone();
        self.banners[i].leaving_at = Some(now);
        let a = self.banners_area();
        self.damage(a);
        // The close button dismisses without opening.
        let r = self.banner_rect(i, &self.banners[i]);
        let (cx, cy) = self.cursor;
        if (cx - r.x).abs() < 14 && (cy - r.y).abs() < 14 {
            return;
        }
        self.activate(&n);
    }

    pub(super) fn update_banner_hover(&mut self, x: i32, y: i32) {
        let h = self.banner_at(x, y).map(|i| self.banners[i].n.id);
        if h != self.banner_hover {
            self.banner_hover = h;
            let a = self.banners_area();
            self.damage(a);
        }
    }

    pub(super) fn paint_banners(&self, cv: &mut Canvas) {
        if self.banners.is_empty() || !self.banners_area().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        for (i, b) in self.banners.iter().enumerate() {
            let r = self.banner_rect(i, b);
            if !r.inset(-30).intersects(&cv.clip) {
                continue;
            }
            cv.shadow(r, 16, 22, 6, t.shadow * 2 / 3);
            cv.glass(&self.blurred, r, 16, t.panel_tint);
            cv.stroke_round_rect(r, 16, if t.dark { 0x30FF_FFFF } else { 0x70FF_FFFF });
            self.paint_card(cv, r, &b.n);
            if self.banner_hover == Some(b.n.id) {
                cv.fill_circle(r.x + 2, r.y + 2, 10, if t.dark { 0xFF3A_3A46 } else { 0xFFF2_F2F6 });
                cv.stroke_round_rect(Rect::new(r.x - 8, r.y - 8, 20, 20), 10, t.window_border);
                cv.line(r.x - 1, r.y - 1, r.x + 5, r.y + 5, 1, t.text);
                cv.line(r.x + 5, r.y - 1, r.x - 1, r.y + 5, 1, t.text);
            }
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

    fn center_rect(&self) -> Rect {
        let top = MENUBAR_H + 8;
        let bottom = self.h - DOCK_H - DOCK_MARGIN - 12;
        let now = self.now_ms;
        let (open, close) = match &self.center {
            Some(c) => (
                ease_out(((now.saturating_sub(c.opened_at)) * 1000 / SLIDE_MS) as i64),
                c.closing_at.map(|t| ease_out(((now.saturating_sub(t)) * 1000 / SLIDE_MS) as i64)).unwrap_or(0),
            ),
            None => (0, 0),
        };
        let slide = ((1000 - open) + close) * (CENTER_W as i64 + 20) / 1000;
        Rect::new(self.w - CENTER_W - 10 + slide as i32, top, CENTER_W, bottom - top)
    }

    fn center_area(&self) -> Rect {
        let r = self.center_rect();
        Rect::new(self.w - CENTER_W - 60, r.y - 20, CENTER_W + 60, r.h + 60)
    }

    pub(super) fn toggle_center(&mut self) {
        let now = self.now_ms;
        match &mut self.center {
            Some(c) if c.closing_at.is_none() => c.closing_at = Some(now),
            Some(_) => {}
            None => {
                self.center = Some(Center { opened_at: now, closing_at: None, month_offset: 0, hover: None });
                // Banners give way to the panel.
                for b in &mut self.banners {
                    b.leaving_at.get_or_insert(now);
                }
            }
        }
        let a = self.center_area();
        self.damage(a);
        let b = self.banners_area();
        self.damage(b);
        self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
    }

    pub(super) fn center_open(&self) -> bool {
        self.center.as_ref().is_some_and(|c| c.closing_at.is_none())
    }

    /// Layout of the panel's interactive parts.
    fn center_hits(&self) -> Vec<(CenterHit, Rect)> {
        let p = self.center_rect();
        let cal = Rect::new(p.x + 14, p.y + 80, p.w - 28, 250);
        let mut v = alloc::vec![
            (CenterHit::PrevMonth, Rect::new(cal.right() - 64, cal.y + 12, 26, 26)),
            (CenterHit::NextMonth, Rect::new(cal.right() - 34, cal.y + 12, 26, 26)),
            (CenterHit::Dnd, Rect::new(p.x + 14, cal.bottom() + 12, p.w - 28, 44)),
            (CenterHit::Clear, Rect::new(p.right() - 70, cal.bottom() + 72, 56, 24)),
        ];
        let list_y = cal.bottom() + 104;
        for (i, _) in self.history.iter().enumerate() {
            let r = Rect::new(p.x + 14, list_y + i as i32 * (CARD_H + 8), p.w - 28, CARD_H);
            if r.bottom() > p.bottom() - 10 {
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
        cv.shadow(p, 18, 34, 10, t.shadow);
        cv.glass(&self.blurred, p, 18, t.panel_tint);
        cv.stroke_round_rect(p, 18, if t.dark { 0x30FF_FFFF } else { 0x80FF_FFFF });
        let d = crate::drivers::rtc::now();
        let wd = weekday(d.year as i32, d.month as u32, d.day as u32) as usize;
        cv.text(p.x + 20, p.y + 36, DAYS[wd], theme::ui_bold(22), t.text);
        let date = format!("{} {}, {}", MONTHS[(d.month.clamp(1, 12) - 1) as usize], d.day, d.year);
        cv.text(p.x + 20, p.y + 60, &date, theme::ui(15), t.text_secondary);

        // Calendar.
        let cal = Rect::new(p.x + 14, p.y + 80, p.w - 28, 250);
        cv.fill_round_rect(cal, 14, with_alpha(t.window_bg, 0xB0));
        let months = d.year as i32 * 12 + d.month as i32 - 1 + c.month_offset;
        let (y, m) = (months.div_euclid(12), (months.rem_euclid(12) + 1) as u32);
        let title = format!("{} {}", MONTHS[(m - 1) as usize], y);
        cv.text(cal.x + 16, cal.y + 30, &title, theme::ui_bold(15), t.text);
        for (hit, r) in self.center_hits() {
            let hovered = c.hover == Some(hit);
            match hit {
                CenterHit::PrevMonth | CenterHit::NextMonth => {
                    if hovered {
                        cv.fill_round_rect(r, 7, t.hover);
                    }
                    let (cx, cy) = r.center();
                    let dd = if hit == CenterHit::PrevMonth { 1 } else { -1 };
                    cv.line(cx + 3 * dd, cy - 5, cx - 2 * dd, cy, 2, t.text);
                    cv.line(cx - 2 * dd, cy, cx + 3 * dd, cy + 5, 2, t.text);
                }
                CenterHit::Dnd => {
                    cv.fill_round_rect(r, 12, with_alpha(t.window_bg, 0xB0));
                    icons::draw(cv, Icon::Clock, Rect::new(r.x + 10, r.y + 8, 28, 28));
                    cv.text(r.x + 48, r.y + 27, "Do Not Disturb", theme::ui_bold(13), t.text);
                    let on = settings::get_bool("dnd", false);
                    widgets::toggle(cv, Rect::new(r.right() - 54, r.y + 11, 42, 22), on);
                }
                CenterHit::Clear if !self.history.is_empty() => {
                    if hovered {
                        cv.fill_round_rect(r, 7, t.hover);
                    }
                    cv.text_centered(r, "Clear", theme::ui(12), theme::accent());
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

        // History.
        let hy = cal.bottom() + 72;
        cv.text(p.x + 20, hy + 17, "Notifications", theme::ui_bold(14), t.text);
        if self.history.is_empty() {
            let r = Rect::new(p.x, hy + 40, p.w, 40);
            cv.text_centered(r, "No Notifications", theme::ui(13), t.text_secondary);
        }
        for (hit, r) in self.center_hits() {
            if let CenterHit::Card(i) = hit {
                let bg =
                    if c.hover == Some(hit) { with_alpha(t.window_bg, 0xE0) } else { with_alpha(t.window_bg, 0xB0) };
                cv.fill_round_rect(r, 14, bg);
                self.paint_card(cv, r, &self.history[i]);
            }
        }
    }
}
