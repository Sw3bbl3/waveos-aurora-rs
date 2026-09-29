//! Settings — appearance, display, keyboard, date and time, sound,
//! notifications, and information about this computer.
//!
//! Start with a pane name (`Settings.elf keyboard`) to open that pane, as
//! Spotlight does.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora::abi::{display, pref, DateTime, DisplayMode};
use aurora::prefs;
use aurora_image::Image;
use ripple::canvas::{with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::icons::{self, Icon};
use ripple::text::TextField;
use ripple::theme;
use ripple::wallpaper;
use ripple::widgets::{self, button, ButtonStyle};
use ripple::{App, Env, KeyEvent, Request};

const SIDEBAR_W: i32 = 214;
const ROW_H: i32 = 44;
const MODE_H: i32 = 36;
/// Widths of the year, month, day, hour and minute steppers.
const FIELD_W: [i32; 5] = [100, 84, 84, 84, 84];
const THUMB_W: i32 = 132;
const THUMB_H: i32 = 84;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Pane {
    Appearance,
    Display,
    Keyboard,
    DateTime,
    Sound,
    Notifications,
    About,
}

const PANES: [(Pane, &str, &str, u32); 7] = [
    (Pane::Appearance, "Appearance", "appearance", 0xFF5E5CE6),
    (Pane::Display, "Display", "display", 0xFF0A84FF),
    (Pane::Keyboard, "Keyboard", "keyboard", 0xFF8E8E93),
    (Pane::DateTime, "Date & Time", "datetime", 0xFFFF9F0A),
    (Pane::Sound, "Sound", "sound", 0xFFFF375F),
    (Pane::Notifications, "Notifications", "notifications", 0xFFFF453A),
    (Pane::About, "About", "about", 0xFF30B0C7),
];

const LAYOUTS: [(&str, &str); 6] =
    [("us", "U.S."), ("uk", "British"), ("de", "German"), ("fr", "French"), ("es", "Spanish"), ("se", "Swedish")];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Pane(usize),
    Light,
    Dark,
    Accent(usize),
    Wallpaper(u8),
    CustomWallpaper,
    Mode(usize),
    Layout(usize),
    RepeatDelay,
    RepeatRate,
    TryField,
    Toggle(&'static str),
    Field(usize, i32),
    SetClock,
    Volume,
    AboutMore,
}

pub struct Settings {
    pane: Pane,
    hover: Option<Hit>,
    thumbs: Vec<Vec<u32>>,
    custom_thumb: Option<Image>,
    modes: Vec<DisplayMode>,
    note: Option<(String, u64)>,
    try_field: TextField,
    try_focused: bool,
    /// Date and time being edited: year, month, day, hour, minute.
    clock: [i32; 5],
    clock_edited: bool,
    dragging: Option<Hit>,
    area: Rect,
    info: aurora::abi::SysInfo,
    screen: (i32, i32),
}

fn days_in(y: i32, m: i32) -> i32 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Settings {
    pub fn new(start: Option<&str>) -> Self {
        let thumbs = (0..wallpaper::NAMES.len() as u8).map(|i| wallpaper::generate(i, THUMB_W, THUMB_H)).collect();
        let pane = start
            .and_then(|s| PANES.iter().find(|(_, _, id, _)| *id == s))
            .map(|(p, _, _, _)| *p)
            .unwrap_or(Pane::Appearance);
        let mut s = Self {
            pane,
            hover: None,
            thumbs,
            custom_thumb: None,
            modes: prefs::display_modes(),
            note: None,
            try_field: TextField::new("", "Type here to try your layout"),
            try_focused: false,
            clock: [0; 5],
            clock_edited: false,
            dragging: None,
            area: Rect::new(0, 0, 820, 560),
            info: aurora::process::sys_info(),
            screen: (0, 0),
        };
        s.load_custom_thumb();
        s.sync_clock();
        s
    }

    fn load_custom_thumb(&mut self) {
        let path = prefs::get("wallpaper_image");
        self.custom_thumb = path
            .and_then(|p| aurora::fs::read(&p).ok())
            .and_then(|d| aurora_image::decode(&d).ok())
            .map(|img| img.thumbnail(THUMB_W as u32 * 2, THUMB_H as u32 * 2));
    }

    fn sync_clock(&mut self) {
        let d = aurora::time::now();
        self.clock = [d.year as i32, d.month as i32, d.day as i32, d.hour as i32, d.minute as i32];
    }

    fn flash(&mut self, msg: String) {
        self.note = Some((msg, aurora::time::uptime_ms()));
    }

    fn content(area: Rect) -> Rect {
        Rect::new(area.x + SIDEBAR_W + 28, area.y + 24, area.w - SIDEBAR_W - 56, area.h - 48)
    }

    /// A grouped card of `n` rows starting at `y`.
    fn rows(c: Rect, y: i32, n: usize) -> Vec<Rect> {
        (0..n as i32).map(|i| Rect::new(c.x, y + i * ROW_H, c.w, ROW_H)).collect()
    }

    fn toggle_rect(row: Rect) -> Rect {
        Rect::new(row.right() - 58, row.y + (ROW_H - 24) / 2, 44, 24)
    }

    /// Every clickable thing on screen, for drawing and hit-testing alike.
    fn hits(&self, area: Rect) -> Vec<(Hit, Rect)> {
        let mut v = Vec::new();
        for (i, _) in PANES.iter().enumerate() {
            v.push((Hit::Pane(i), Rect::new(area.x + 10, area.y + 52 + i as i32 * 34, SIDEBAR_W - 20, 30)));
        }
        let c = Self::content(area);
        match self.pane {
            Pane::Appearance => {
                v.push((Hit::Light, Rect::new(c.x, c.y + 70, THUMB_W, THUMB_H)));
                v.push((Hit::Dark, Rect::new(c.x + THUMB_W + 20, c.y + 70, THUMB_W, THUMB_H)));
                for i in 0..theme::ACCENTS.len() {
                    v.push((Hit::Accent(i), Rect::new(c.x + i as i32 * 38, c.y + 222, 28, 28)));
                }
                let n = wallpaper::NAMES.len() as i32;
                for i in 0..n {
                    v.push((Hit::Wallpaper(i as u8), Rect::new(c.x + i * (THUMB_W + 16), c.y + 318, THUMB_W, THUMB_H)));
                }
                if self.custom_thumb.is_some() {
                    v.push((Hit::CustomWallpaper, Rect::new(c.x + n * (THUMB_W + 16), c.y + 318, THUMB_W, THUMB_H)));
                }
            }
            Pane::Display => {
                for i in 0..self.modes.len().min(12) {
                    v.push((Hit::Mode(i), Rect::new(c.x, c.y + 70 + i as i32 * MODE_H, c.w, MODE_H)));
                }
            }
            Pane::Keyboard => {
                for (i, r) in Self::rows(c, c.y + 64, LAYOUTS.len()).into_iter().enumerate() {
                    v.push((Hit::Layout(i), r));
                }
                let y = c.y + 64 + LAYOUTS.len() as i32 * ROW_H + 56;
                v.push((Hit::RepeatDelay, Rect::new(c.right() - 240, y + 12, 220, 20)));
                v.push((Hit::RepeatRate, Rect::new(c.right() - 240, y + ROW_H + 12, 220, 20)));
                v.push((Hit::TryField, Rect::new(c.x, y + 2 * ROW_H + 18, c.w, 34)));
            }
            Pane::DateTime => {
                let rows = Self::rows(c, c.y + 132, 3);
                for (r, key) in rows.iter().zip([pref::CLOCK_24H, pref::CLOCK_SECONDS, pref::CLOCK_DATE]) {
                    v.push((Hit::Toggle(key), Self::toggle_rect(*r)));
                }
                let y = c.y + 132 + 3 * ROW_H + 60;
                let mut x = c.x;
                for (i, w) in FIELD_W.iter().enumerate() {
                    v.push((Hit::Field(i, -1), Rect::new(x, y + 36, 26, 26)));
                    v.push((Hit::Field(i, 1), Rect::new(x + w - 26, y + 36, 26, 26)));
                    x += w + 10 + if i == 2 { 24 } else { 0 };
                }
                v.push((Hit::SetClock, Rect::new(c.right() - 120, y + 34, 110, 30)));
            }
            Pane::Sound => {
                v.push((Hit::Volume, Rect::new(c.x + 110, c.y + 70 + 12, c.w - 140, 20)));
                v.push((Hit::Toggle(pref::MUTED), Self::toggle_rect(Rect::new(c.x, c.y + 70 + ROW_H, c.w, ROW_H))));
            }
            Pane::Notifications => {
                v.push((Hit::Toggle(pref::DND), Self::toggle_rect(Rect::new(c.x, c.y + 70, c.w, ROW_H))));
            }
            Pane::About => {
                v.push((Hit::AboutMore, Rect::new(c.x + (c.w - 160) / 2, c.y + 420, 160, 32)));
            }
        }
        v
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits(self.area).into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h)
    }

    fn run(&mut self, h: Hit, x: i32, env: &mut Env) {
        match h {
            Hit::Pane(i) => {
                self.pane = PANES[i].0;
                self.try_focused = false;
                if self.pane == Pane::Display {
                    self.modes = prefs::display_modes();
                }
                if self.pane == Pane::DateTime {
                    self.sync_clock();
                    self.clock_edited = false;
                }
            }
            Hit::Light => env.requests.push(Request::SetDark(false)),
            Hit::Dark => env.requests.push(Request::SetDark(true)),
            Hit::Accent(i) => {
                let _ = prefs::set(pref::ACCENT, &format!("{i}"));
            }
            Hit::Wallpaper(i) => env.requests.push(Request::SetWallpaper(i)),
            Hit::CustomWallpaper => {
                if let Some(p) = prefs::get("wallpaper_image") {
                    env.requests.push(Request::SetWallpaperImage(p));
                }
            }
            Hit::Mode(i) => {
                if let Some(m) = self.modes.get(i).copied() {
                    match prefs::set_display(m.width, m.height) {
                        Ok(true) => {}
                        Ok(false) => self.flash(format!("{} × {} will be used when you restart", m.width, m.height)),
                        Err(e) => self.flash(format!("Couldn't change the resolution: {e}")),
                    }
                    self.modes = prefs::display_modes();
                }
            }
            Hit::Layout(i) => {
                let _ = prefs::set(pref::KEYBOARD, LAYOUTS[i].0);
            }
            Hit::RepeatDelay | Hit::RepeatRate => self.slide(h, x),
            Hit::TryField => self.try_focused = true,
            Hit::Toggle(key) => prefs::set_bool(key, !prefs::get_bool(key)),
            Hit::Field(i, d) => {
                let c = &mut self.clock;
                c[i] += d;
                let limits = [(2000, 2099), (1, 12), (1, days_in(c[0], c[1])), (0, 23), (0, 59)];
                let (lo, hi) = limits[i];
                if c[i] < lo {
                    c[i] = hi;
                } else if c[i] > hi {
                    c[i] = lo;
                }
                c[2] = c[2].min(days_in(c[0], c[1]));
                self.clock_edited = true;
            }
            Hit::SetClock => {
                let c = self.clock;
                let d = DateTime {
                    year: c[0] as u16,
                    month: c[1] as u8,
                    day: c[2] as u8,
                    hour: c[3] as u8,
                    minute: c[4] as u8,
                    second: 0,
                    weekday: 0,
                };
                match prefs::set_datetime(&d) {
                    Ok(()) => {
                        self.clock_edited = false;
                        self.flash(String::from("The date and time were set"));
                    }
                    Err(e) => self.flash(format!("Couldn't set the clock: {e}")),
                }
            }
            Hit::Volume => self.slide(h, x),
            Hit::AboutMore => env.requests.push(Request::OpenApp(String::from("About"))),
        }
    }

    /// Sets a slider preference from the pointer position.
    fn slide(&mut self, h: Hit, x: i32) {
        let Some(r) = self.hits(self.area).into_iter().find(|(k, _)| *k == h).map(|(_, r)| r) else { return };
        let v = widgets::slider_value(r, x);
        match h {
            // Delay: 4 steps (long → short, shown left to right as the value grows shorter).
            Hit::RepeatDelay => {
                let _ = prefs::set(pref::REPEAT_DELAY, &format!("{}", 3 - (v * 3 + 500) / 1000));
            }
            // Rate: 32 steps, faster to the right.
            Hit::RepeatRate => {
                let _ = prefs::set(pref::REPEAT_RATE, &format!("{}", 31 - (v * 31 + 500) / 1000));
            }
            Hit::Volume => {
                let _ = prefs::set(pref::VOLUME, &format!("{}", (v + 5) / 10));
            }
            _ => {}
        }
        self.dragging = Some(h);
    }

    // ------------------------------------------------------------ drawing

    fn heading(cv: &mut Canvas, c: Rect, y: i32, text: &str) {
        cv.text(c.x, y, text, theme::ui_bold(13), theme::current().text_secondary);
    }

    fn card(cv: &mut Canvas, r: Rect, rows: usize) {
        let t = theme::current();
        cv.fill_round_rect(r, 12, t.window_bg_alt);
        cv.stroke_round_rect(r, 12, t.separator);
        for i in 1..rows as i32 {
            cv.fill_rect(Rect::new(r.x + 16, r.y + i * ROW_H, r.w - 32, 1), t.separator);
        }
    }

    fn draw_sidebar(&self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let side = Rect::new(area.x, area.y, SIDEBAR_W, area.h);
        cv.fill_rect(side, t.window_bg_alt);
        cv.fill_rect(Rect::new(side.right() - 1, side.y, 1, side.h), t.separator);
        cv.text(area.x + 20, area.y + 34, "Settings", theme::ui_bold(18), t.text);
        for (i, (pane, label, _, color)) in PANES.iter().enumerate() {
            let r = Rect::new(area.x + 10, area.y + 52 + i as i32 * 34, SIDEBAR_W - 20, 30);
            if *pane == self.pane {
                cv.fill_round_rect(r, 8, theme::accent());
            } else if self.hover == Some(Hit::Pane(i)) {
                cv.fill_round_rect(r, 8, t.hover);
            }
            let ic = Rect::new(r.x + 8, r.y + 5, 20, 20);
            cv.fill_round_rect(ic, 6, *color);
            pane_glyph(cv, *pane, ic);
            let fg = if *pane == self.pane { 0xFFFF_FFFF } else { t.text };
            cv.text(r.x + 38, r.y + 20, label, theme::ui(13), fg);
        }
    }

    fn draw_pane(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let c = Self::content(area);
        let title = PANES.iter().find(|p| p.0 == self.pane).map(|p| p.1).unwrap_or("");
        cv.text(c.x, c.y + 24, title, theme::ui_bold(22), t.text);
        let hits = self.hits(area);
        let rect_of = |h: Hit| hits.iter().find(|(k, _)| *k == h).map(|(_, r)| *r);
        match self.pane {
            Pane::Appearance => {
                Self::heading(cv, c, c.y + 58, "Appearance");
                for (h, dark) in [(Hit::Light, false), (Hit::Dark, true)] {
                    let r = rect_of(h).unwrap();
                    mode_preview(cv, r, dark);
                    let selected = t.dark == dark;
                    self.ring(cv, r, selected, h);
                    let label = if dark { "Dark" } else { "Light" };
                    cv.text_centered(Rect::new(r.x, r.bottom() + 6, r.w, 20), label, theme::ui(13), t.text);
                }
                Self::heading(cv, c, c.y + 210, "Accent colour");
                for (i, (name, color)) in theme::ACCENTS.iter().enumerate() {
                    let r = rect_of(Hit::Accent(i)).unwrap();
                    let (cx, cy) = r.center();
                    let selected = theme::accent_index() as usize == i;
                    if selected {
                        cv.fill_circle(cx, cy, 14, with_alpha(*color, 0x50));
                    }
                    cv.fill_circle(cx, cy, 11, *color);
                    if selected {
                        cv.fill_circle(cx, cy, 4, 0xFFFF_FFFF);
                    }
                    if selected || self.hover == Some(Hit::Accent(i)) {
                        let label_x = c.x + theme::ACCENTS.len() as i32 * 38 + 8;
                        cv.text(label_x, cy + 5, name, theme::ui(13), t.text_secondary);
                    }
                }
                Self::heading(cv, c, c.y + 306, "Wallpaper");
                for i in 0..wallpaper::NAMES.len() as u8 {
                    let r = rect_of(Hit::Wallpaper(i)).unwrap();
                    let img = &self.thumbs[i as usize];
                    cv.fill_round_rect_with(r, 10, |px, py| img[((py - r.y) * THUMB_W + (px - r.x)) as usize]);
                    self.ring(cv, r, theme::wallpaper() == i, Hit::Wallpaper(i));
                    let label = wallpaper::NAMES[i as usize];
                    cv.text_centered(Rect::new(r.x, r.bottom() + 6, r.w, 20), label, theme::ui(13), t.text);
                }
                if let (Some(r), Some(img)) = (rect_of(Hit::CustomWallpaper), &self.custom_thumb) {
                    cv.with_clip(r, |cv| {
                        let (w, h) = aurora_image::fit(img.width, img.height, 10_000, r.h as u32);
                        let fill = if (w as i32) < r.w {
                            let s = r.w as u32 * img.height / img.width.max(1);
                            Rect::new(r.x, r.y + (r.h - s as i32) / 2, r.w, s as i32)
                        } else {
                            Rect::new(r.x + (r.w - w as i32) / 2, r.y, w as i32, h as i32)
                        };
                        cv.draw_image(&img.pixels, img.width, img.height, fill, 255);
                    });
                    self.ring(cv, r, theme::wallpaper() == wallpaper::CUSTOM, Hit::CustomWallpaper);
                    cv.text_centered(Rect::new(r.x, r.bottom() + 6, r.w, 20), "Your Picture", theme::ui(13), t.text);
                }
                let tip = "Tip: choose Set as Wallpaper in Preview or Files to use any picture.";
                cv.text_clipped(c.x, c.y + 450, tip, theme::ui(12), t.text_secondary, c.w);
            }
            Pane::Display => {
                let s = self.info;
                let live = self.modes.iter().any(|m| m.flags & display::LIVE != 0);
                let sub = if live {
                    "Changes apply right away; you'll be asked to keep them."
                } else {
                    "This display is set up by the firmware: changes apply when you restart."
                };
                cv.text(c.x, c.y + 52, sub, theme::ui(13), t.text_secondary);
                let n = self.modes.len().min(12);
                let card = Rect::new(c.x, c.y + 70, c.w, n as i32 * MODE_H);
                cv.fill_round_rect(card, 12, t.window_bg_alt);
                cv.stroke_round_rect(card, 12, t.separator);
                for i in 1..n as i32 {
                    cv.fill_rect(Rect::new(card.x + 16, card.y + i * MODE_H, card.w - 32, 1), t.separator);
                }
                for (i, m) in self.modes.iter().take(n).enumerate() {
                    let r = rect_of(Hit::Mode(i)).unwrap();
                    if self.hover == Some(Hit::Mode(i)) {
                        cv.with_clip(card, |cv| cv.fill_rect(r.inset(1), t.hover));
                    }
                    let label = format!("{} × {}", m.width, m.height);
                    cv.text(r.x + 16, r.y + 24, &label, theme::ui(14), t.text);
                    let mut tags = Vec::new();
                    if m.flags & display::CURRENT != 0 {
                        tags.push("In use");
                    }
                    if m.flags & display::AT_BOOT != 0 && m.flags & display::CURRENT == 0 {
                        tags.push("After restart");
                    }
                    if (m.width, m.height) == (1280, 800) {
                        tags.push("Designed for");
                    }
                    let tag = tags.join(" · ");
                    let f = theme::ui(12);
                    cv.text(r.right() - 50 - f.width(&tag), r.y + 24, &tag, f, t.text_secondary);
                    let (cx, cy) = (r.right() - 26, r.y + MODE_H / 2);
                    if m.flags & display::CURRENT != 0 {
                        cv.fill_circle(cx, cy, 8, theme::accent());
                        cv.fill_circle(cx, cy, 3, 0xFFFF_FFFF);
                    } else {
                        cv.fill_circle(cx, cy, 8, t.control_border);
                        cv.fill_circle(cx, cy, 7, t.control_bg);
                    }
                }
                let _ = s;
            }
            Pane::Keyboard => {
                Self::heading(cv, c, c.y + 52, "Layout");
                let card = Rect::new(c.x, c.y + 64, c.w, LAYOUTS.len() as i32 * ROW_H);
                Self::card(cv, card, LAYOUTS.len());
                let current = prefs::get(pref::KEYBOARD).unwrap_or_default();
                for (i, (id, name)) in LAYOUTS.iter().enumerate() {
                    let r = rect_of(Hit::Layout(i)).unwrap();
                    if self.hover == Some(Hit::Layout(i)) {
                        cv.with_clip(card, |cv| cv.fill_rect(r.inset(1), t.hover));
                    }
                    cv.text(r.x + 16, r.y + 27, name, theme::ui(14), t.text);
                    if current == *id {
                        let (cx, cy) = (r.right() - 26, r.y + ROW_H / 2);
                        cv.line(cx - 6, cy, cx - 2, cy + 5, 2, theme::accent());
                        cv.line(cx - 2, cy + 5, cx + 6, cy - 5, 2, theme::accent());
                    }
                }
                let y = c.y + 64 + LAYOUTS.len() as i32 * ROW_H + 44;
                Self::heading(cv, c, y - 2, "Key repeat");
                let card2 = Rect::new(c.x, y + 8, c.w, 2 * ROW_H);
                Self::card(cv, Rect::new(card2.x, card2.y - 4, card2.w, card2.h), 2);
                cv.text(c.x + 16, y + 32, "Delay until repeat", theme::ui(14), t.text);
                cv.text(c.x + 16, y + 32 + ROW_H, "Repeat speed", theme::ui(14), t.text);
                let delay = prefs::get_int(pref::REPEAT_DELAY, 1).clamp(0, 3) as i32;
                let rate = prefs::get_int(pref::REPEAT_RATE, 8).clamp(0, 31) as i32;
                let dr = rect_of(Hit::RepeatDelay).unwrap();
                widgets::slider(cv, dr, (3 - delay) * 1000 / 3, self.hover == Some(Hit::RepeatDelay));
                let rr = rect_of(Hit::RepeatRate).unwrap();
                widgets::slider(cv, rr, (31 - rate) * 1000 / 31, self.hover == Some(Hit::RepeatRate));
                let fr = rect_of(Hit::TryField).unwrap();
                self.try_field.draw(cv, fr, env.focused && self.try_focused);
            }
            Pane::DateTime => {
                let d = aurora::time::now();
                let h24 = prefs::get_bool(pref::CLOCK_24H);
                let time = if h24 {
                    format!("{:02}:{:02}:{:02}", d.hour, d.minute, d.second)
                } else {
                    let h = match d.hour % 12 {
                        0 => 12,
                        h => h,
                    };
                    format!("{}:{:02}:{:02} {}", h, d.minute, d.second, if d.hour < 12 { "AM" } else { "PM" })
                };
                cv.text(c.x, c.y + 80, &time, theme::ui_bold(36), t.text);
                cv.text(c.x, c.y + 108, &aurora::time::format_date(&d), theme::ui(14), t.text_secondary);
                let rows = Self::rows(c, c.y + 132, 3);
                Self::card(cv, Rect::new(c.x, rows[0].y, c.w, 3 * ROW_H), 3);
                for (r, (key, label)) in rows.iter().zip([
                    (pref::CLOCK_24H, "24-hour time"),
                    (pref::CLOCK_SECONDS, "Show seconds in the menu bar"),
                    (pref::CLOCK_DATE, "Show the date in the menu bar"),
                ]) {
                    cv.text(r.x + 16, r.y + 27, label, theme::ui(14), t.text);
                    widgets::toggle(cv, Self::toggle_rect(*r), prefs::get_bool(key));
                }
                let y = c.y + 132 + 3 * ROW_H + 60;
                Self::heading(cv, c, y - 12, "Set date and time");
                Self::card(cv, Rect::new(c.x - 10, y + 22, c.w + 20, 56), 1);
                let labels = ["Year", "Month", "Day", "Hour", "Minute"];
                let mut x = c.x;
                for i in 0..5 {
                    let w = FIELD_W[i];
                    cv.text_centered(Rect::new(x, y + 2, w, 16), labels[i], theme::ui(11), t.text_secondary);
                    let value = if i >= 3 { format!("{:02}", self.clock[i]) } else { format!("{}", self.clock[i]) };
                    cv.text_centered(Rect::new(x, y + 36, w, 26), &value, theme::ui_bold(14), t.text);
                    for (d, glyph) in [(-1, "−"), (1, "+")] {
                        let r = rect_of(Hit::Field(i, d)).unwrap();
                        if self.hover == Some(Hit::Field(i, d)) {
                            cv.fill_round_rect(r, 6, t.hover);
                        }
                        cv.text_centered(r, glyph, theme::ui(14), t.text_secondary);
                    }
                    x += w + 10 + if i == 2 { 24 } else { 0 };
                }
                let sr = rect_of(Hit::SetClock).unwrap();
                let style = if self.clock_edited { ButtonStyle::Primary } else { ButtonStyle::Secondary };
                button(cv, sr, "Set", style, self.hover == Some(Hit::SetClock));
            }
            Pane::Sound => {
                let card = Rect::new(c.x, c.y + 70, c.w, 3 * ROW_H);
                Self::card(cv, card, 3);
                cv.text(c.x + 16, c.y + 70 + 27, "Volume", theme::ui(14), t.text);
                let vol = prefs::get_int(pref::VOLUME, 70).clamp(0, 100) as i32;
                let vr = rect_of(Hit::Volume).unwrap();
                widgets::slider(cv, vr, vol * 10, self.hover == Some(Hit::Volume));
                cv.text(c.x + 16, c.y + 70 + ROW_H + 27, "Mute", theme::ui(14), t.text);
                widgets::toggle(cv, rect_of(Hit::Toggle(pref::MUTED)).unwrap(), prefs::get_bool(pref::MUTED));
                cv.text(c.x + 16, c.y + 70 + 2 * ROW_H + 27, "Output", theme::ui(14), t.text);
                let out = prefs::get("audio_device").unwrap_or_else(|| String::from("No output device found"));
                let f = theme::ui(13);
                cv.text(card.right() - 16 - f.width(&out), c.y + 70 + 2 * ROW_H + 27, &out, f, t.text_secondary);
            }
            Pane::Notifications => {
                let r = Rect::new(c.x, c.y + 70, c.w, ROW_H);
                Self::card(cv, r, 1);
                cv.text(r.x + 16, r.y + 27, "Do Not Disturb", theme::ui(14), t.text);
                widgets::toggle(cv, rect_of(Hit::Toggle(pref::DND)).unwrap(), prefs::get_bool(pref::DND));
                let text = "While Do Not Disturb is on, notifications don't show banners. They still \
                            collect in the Notification Center: click the clock in the menu bar.";
                let f = theme::ui(13);
                for (k, (a, b)) in widgets::wrap(text, f, c.w).into_iter().enumerate() {
                    cv.text(c.x, c.y + 142 + k as i32 * 20, &text[a..b], f, t.text_secondary);
                }
            }
            Pane::About => {
                let (cx, _) = c.center();
                icons::draw(cv, Icon::Aurora, Rect::new(cx - 44, c.y + 50, 88, 88));
                cv.text_centered(Rect::new(c.x, c.y + 150, c.w, 34), "WaveOS Aurora", theme::ui_bold(26), t.text);
                let i = &self.info;
                let version = format!("Version {}", aurora::process::fixed_str(&i.version, i.version_len));
                cv.text_centered(Rect::new(c.x, c.y + 184, c.w, 20), &version, theme::ui(14), t.text_secondary);
                let rows = [
                    ("Processor", aurora::process::fixed_str(&i.cpu, i.cpu_len)),
                    ("Memory", format!("{} MiB", i.mem_total >> 20)),
                    ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
                    ("Startup disk", aurora::process::fixed_str(&i.root, i.root_len)),
                    ("Available", format!("{} MB of {} MB", i.disk_free >> 20, i.disk_total >> 20)),
                ];
                let card = Rect::new(c.x, c.y + 216, c.w, rows.len() as i32 * 36);
                cv.fill_round_rect(card, 12, t.window_bg_alt);
                for (k, (label, value)) in rows.iter().enumerate() {
                    let y = card.y + k as i32 * 36 + 23;
                    cv.text(card.x + 16, y, label, theme::ui(13), t.text_secondary);
                    let f = theme::ui(13);
                    let w = f.width(value).min(card.w - 150);
                    cv.text_clipped(card.right() - 16 - w, y, value, f, t.text, card.w - 150);
                }
                let r = rect_of(Hit::AboutMore).unwrap();
                button(cv, r, "More Info…", ButtonStyle::Secondary, self.hover == Some(Hit::AboutMore));
            }
        }
        if let Some((msg, at)) = &self.note {
            if env.now_ms < at + 4000 {
                let f = theme::ui_bold(13);
                let w = f.width(msg) + 32;
                let r = Rect::new(c.x + (c.w - w) / 2, area.bottom() - 54, w, 34);
                cv.fill_round_rect(r, 17, 0xE025_2530);
                cv.text_centered(r, msg, f, 0xFFFF_FFFF);
            }
        }
    }

    fn ring(&self, cv: &mut Canvas, r: Rect, selected: bool, h: Hit) {
        if selected {
            cv.stroke_round_rect(r.inset(-3), 13, theme::accent());
            cv.stroke_round_rect(r.inset(-2), 12, theme::accent());
        } else if self.hover == Some(h) {
            cv.stroke_round_rect(r.inset(-2), 12, with_alpha(theme::accent(), 0x80));
        }
    }
}

/// A small white glyph on a pane's colored tile.
fn pane_glyph(cv: &mut Canvas, pane: Pane, r: Rect) {
    let (cx, cy) = r.center();
    let w = 0xFFFF_FFFF;
    match pane {
        Pane::Appearance => {
            cv.fill_circle(cx, cy, 6, w);
            cv.with_clip(Rect::new(cx, cy - 7, 8, 14), |cv| cv.fill_circle(cx, cy, 6, 0xFF5E5CE6));
            cv.fill_circle(cx, cy, 4, 0xFF5E5CE6);
            cv.with_clip(Rect::new(cx - 6, cy - 7, 6, 14), |cv| cv.fill_circle(cx, cy, 4, w));
        }
        Pane::Display => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 5, 14, 9), 2, w);
            cv.fill_rect(Rect::new(cx - 3, cy + 5, 6, 2), w);
        }
        Pane::Keyboard => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 4, 14, 9), 2, w);
            for k in 0..3 {
                cv.fill_rect(Rect::new(cx - 5 + k * 4, cy - 2, 2, 2), 0xFF8E8E93);
            }
            cv.fill_rect(Rect::new(cx - 4, cy + 2, 8, 1), 0xFF8E8E93);
        }
        Pane::DateTime => {
            cv.fill_circle(cx, cy, 7, w);
            cv.line(cx, cy, cx, cy - 5, 1, 0xFFFF9F0A);
            cv.line(cx, cy, cx + 3, cy, 1, 0xFFFF9F0A);
        }
        Pane::Sound => {
            cv.fill_rect(Rect::new(cx - 6, cy - 2, 4, 5), w);
            cv.line(cx - 2, cy - 2, cx + 2, cy - 6, 2, w);
            cv.line(cx - 2, cy + 3, cx + 2, cy + 7, 2, w);
            cv.fill_rect(Rect::new(cx + 2, cy - 6, 2, 13), w);
        }
        Pane::Notifications => {
            cv.fill_round_rect(Rect::new(cx - 6, cy - 6, 12, 12), 3, w);
            cv.fill_circle(cx + 5, cy - 5, 3, 0xFFFFD60A);
        }
        Pane::About => {
            cv.fill_circle(cx, cy - 4, 1, w);
            cv.fill_rect(Rect::new(cx - 1, cy - 1, 2, 7), w);
        }
    }
}

fn mode_preview(cv: &mut Canvas, r: Rect, dark: bool) {
    let t = if dark { &theme::DARK } else { &theme::LIGHT };
    let bg = if dark { 0xFF10_1426 } else { 0xFFBF_D6F5 };
    cv.fill_round_rect(r, 10, bg);
    let win = Rect::new(r.x + 20, r.y + 14, r.w - 40, r.h - 26);
    cv.fill_round_rect(win, 6, t.window_bg);
    cv.fill_rect(Rect::new(win.x + 6, win.y + 16, win.w - 12, 1), t.separator);
    for (i, c) in [theme::CLOSE, theme::MINIMIZE, theme::ZOOM].iter().enumerate() {
        cv.fill_circle(win.x + 9 + i as i32 * 9, win.y + 8, 3, *c);
    }
    for i in 0..3 {
        cv.fill_round_rect(
            Rect::new(win.x + 10, win.y + 26 + i * 10, win.w - 30 - i * 14, 5),
            2,
            with_alpha(t.text, 0x50),
        );
    }
    cv.fill_round_rect(Rect::new(win.x + 10, win.bottom() - 14, 26, 8), 4, theme::accent());
}

impl App for Settings {
    fn title(&self) -> String {
        "Settings".into()
    }

    fn size(&self) -> (i32, i32) {
        (840, 580)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        self.draw_sidebar(cv, area);
        self.draw_pane(cv, area, env);
    }

    fn click(&mut self, x: i32, y: i32, _area: Rect, env: &mut Env) -> bool {
        if self.pane == Pane::Keyboard && self.try_field.click(x, y, env.mods.shift, env.now_ms) {
            self.try_focused = true;
            return true;
        }
        self.try_focused = false;
        match self.hit_at(x, y) {
            Some(h) => {
                self.run(h, x, env);
                true
            }
            None => false,
        }
    }

    fn drag(&mut self, x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        if self.try_focused && self.try_field.drag(x) {
            return true;
        }
        if let Some(h) = self.dragging {
            self.slide(h, x);
            return true;
        }
        false
    }

    fn release(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        self.try_field.release();
        self.dragging = None;
        false
    }

    fn hover(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        let h = if x < 0 { None } else { self.hit_at(x, y) };
        core::mem::replace(&mut self.hover, h) != h
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if self.try_focused {
            return self.try_field.key(ev, env.now_ms).handled;
        }
        false
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let mut changed = false;
        if env.screen != self.screen {
            self.screen = env.screen;
            self.modes = prefs::display_modes();
            changed = true;
        }
        if self.try_focused {
            changed |= self.try_field.tick(env.now_ms, env.focused);
        }
        if self.pane == Pane::DateTime {
            if !self.clock_edited {
                self.sync_clock();
            }
            changed = true; // the big clock ticks
        }
        if let Some((_, at)) = self.note {
            if env.now_ms > at + 4000 {
                self.note = None;
                changed = true;
            }
        }
        // Pick up a new custom wallpaper chosen elsewhere.
        if self.pane == Pane::Appearance && theme::wallpaper() == wallpaper::CUSTOM && self.custom_thumb.is_none() {
            self.load_custom_thumb();
            changed = true;
        }
        changed
    }

    fn tick_interval(&self) -> u64 {
        if self.pane == Pane::DateTime {
            500
        } else {
            100
        }
    }
}

aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    ripple::run(Settings::new(args.get(1).map(String::as_str)))
}
