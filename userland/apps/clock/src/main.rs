//! Clock — world clock, alarms, stopwatch and timers.
//!
//! World times are computed from the local clock and the time zone set in
//! Settings → Date & Time (standard time, no daylight saving). Alarms and
//! timers post notifications; they need Clock to be running (it can stay
//! minimized). Alarms are kept in /Settings/clock.conf.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurorakit::canvas::{with_alpha, Canvas};
use aurorakit::geom::Rect;
use aurorakit::math::sin;
use aurorakit::text::TextField;
use aurorakit::theme;
use aurorakit::widgets::{self, button, ButtonStyle};
use aurorakit::{App, Env, KeyCode, KeyEvent};
use corekit::abi::pref;
use corekit::{fs, prefs, time};

const TOOLBAR_H: i32 = 52;
const ALARMS: &str = "/Settings/clock.conf";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    World,
    Alarms,
    Stopwatch,
    Timers,
}

const TABS: [(Tab, &str); 4] =
    [(Tab::World, "World Clock"), (Tab::Alarms, "Alarms"), (Tab::Stopwatch, "Stopwatch"), (Tab::Timers, "Timers")];

/// (city, UTC offset in minutes)
const CITIES: [(&str, i32); 8] = [
    ("San Francisco", -8 * 60),
    ("New York", -5 * 60),
    ("London", 0),
    ("Paris", 60),
    ("Dubai", 4 * 60),
    ("Mumbai", 5 * 60 + 30),
    ("Tokyo", 9 * 60),
    ("Sydney", 10 * 60),
];

const PRESETS: [u64; 8] = [1, 3, 5, 10, 15, 20, 30, 60];

#[derive(Clone)]
struct Alarm {
    hour: u8,
    minute: u8,
    enabled: bool,
    label: String,
    /// Day (minutes since boot / 1440) it last rang, so it rings once.
    rang: Option<(u8, u8, u16)>,
}

struct Timer {
    total_ms: u64,
    left_ms: u64,
    running_since: Option<u64>,
    label: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Tab(usize),
    StartStop,
    LapReset,
    Preset(usize),
    TimerToggle(usize),
    TimerCancel(usize),
    AddAlarm,
    AlarmToggle(usize),
    AlarmDelete(usize),
    SheetStep(usize, i32),
    SheetSave,
    SheetCancel,
}

struct Sheet {
    hour: i32,
    minute: i32,
    label: TextField,
}

pub struct Clock {
    tab: Tab,
    hover: Option<Hit>,
    // Stopwatch.
    sw_start: Option<u64>,
    sw_accum: u64,
    laps: Vec<u64>,
    // Timers and alarms.
    timers: Vec<Timer>,
    alarms: Vec<Alarm>,
    sheet: Option<Sheet>,
    area: Rect,
    last_second: u8,
}

fn two(n: u64) -> String {
    format!("{:02}", n)
}

fn fmt_stopwatch(ms: u64) -> String {
    format!("{}:{}.{}", two(ms / 60_000), two(ms / 1000 % 60), two(ms / 10 % 100))
}

fn fmt_timer(ms: u64) -> String {
    let s = ms.div_ceil(1000);
    if s >= 3600 {
        format!("{}:{}:{}", s / 3600, two(s / 60 % 60), two(s % 60))
    } else {
        format!("{}:{}", two(s / 60), two(s % 60))
    }
}

fn fmt_time(h: u32, m: u32, h24: bool) -> String {
    if h24 {
        format!("{:02}:{:02}", h, m)
    } else {
        let hh = match h % 12 {
            0 => 12,
            x => x,
        };
        format!("{}:{:02} {}", hh, m, if h < 12 { "AM" } else { "PM" })
    }
}

/// Draws an analog clock face.
fn face(cv: &mut Canvas, cx: i32, cy: i32, r: i32, h: u32, m: u32, s: Option<u32>, dark_face: bool) {
    let t = theme::current();
    let (bg, fg) = if dark_face { (0xFF1C_1C22, 0xFFF4_F4F8) } else { (0xFFFF_FFFF, 0xFF1D_1D24) };
    cv.fill_circle(cx, cy + 2, r + 1, with_alpha(0x000000, 0x30));
    cv.fill_circle(cx, cy, r, bg);
    let _ = t;
    for k in 0..60 {
        let a = k * 1024 / 60;
        let (len, w) = if k % 5 == 0 { (r / 7, if r > 60 { 3 } else { 2 }) } else { (r / 22, 1) };
        if k % 5 != 0 && r < 60 {
            continue;
        }
        let (x0, y0) = (cx + (r - 4) * sin(a + 256) / 16384, cy + (r - 4) * sin(a) / 16384);
        let (x1, y1) = (cx + (r - 4 - len) * sin(a + 256) / 16384, cy + (r - 4 - len) * sin(a) / 16384);
        cv.line(x0, y0, x1, y1, w, with_alpha(fg, if k % 5 == 0 { 0xFF } else { 0x80 }));
    }
    let hand = |cv: &mut Canvas, turns_1024: i32, len: i32, w: i32, c: u32| {
        let a = turns_1024 - 256;
        cv.line(cx, cy, cx + len * sin(a + 256) / 16384, cy + len * sin(a) / 16384, w, c);
    };
    let hour_a = ((h % 12) * 60 + m) as i32 * 1024 / 720;
    let min_a = (m * 60 + s.unwrap_or(0)) as i32 * 1024 / 3600;
    hand(cv, hour_a, r * 50 / 100, (r / 14).max(3), fg);
    hand(cv, min_a, r * 75 / 100, (r / 20).max(2), fg);
    if let Some(s) = s {
        hand(cv, s as i32 * 1024 / 60, r * 82 / 100, 1, 0xFFFF_9500);
        cv.fill_circle(cx, cy, (r / 22).max(2), 0xFFFF_9500);
    } else {
        cv.fill_circle(cx, cy, (r / 22).max(2), fg);
    }
}

impl Clock {
    fn new() -> Self {
        let mut c = Clock {
            tab: Tab::World,
            hover: None,
            sw_start: None,
            sw_accum: 0,
            laps: Vec::new(),
            timers: Vec::new(),
            alarms: Vec::new(),
            sheet: None,
            area: Rect::new(0, 0, 780, 540),
            last_second: 99,
        };
        c.load_alarms();
        c
    }

    fn load_alarms(&mut self) {
        let text = fs::read_to_string(ALARMS).unwrap_or_default();
        self.alarms = text
            .lines()
            .filter_map(|l| {
                let mut p = l.splitn(3, '|');
                let (h, m) = p.next()?.split_once(':')?;
                let enabled = p.next()? == "1";
                let label = String::from(p.next().unwrap_or(""));
                Some(Alarm { hour: h.parse().ok()?, minute: m.parse().ok()?, enabled, label, rang: None })
            })
            .collect();
        self.alarms.sort_by_key(|a| (a.hour, a.minute));
    }

    fn save_alarms(&self) {
        let mut text = String::new();
        for a in &self.alarms {
            text.push_str(&format!("{:02}:{:02}|{}|{}\n", a.hour, a.minute, a.enabled as u8, a.label));
        }
        let _ = fs::write(ALARMS, text.as_bytes());
    }

    fn stopwatch_ms(&self) -> u64 {
        self.sw_accum + self.sw_start.map(|s| time::uptime_ms() - s).unwrap_or(0)
    }

    fn tabs(area: Rect) -> Rect {
        Rect::new(area.x + (area.w - 440) / 2, area.y + 11, 440, 30)
    }

    fn hits(&self, area: Rect) -> Vec<(Hit, Rect)> {
        let mut v = Vec::new();
        for (i, r) in widgets::segments(Self::tabs(area), TABS.len()).into_iter().enumerate() {
            v.push((Hit::Tab(i), r));
        }
        let body = Rect::new(area.x, area.y + TOOLBAR_H, area.w, area.h - TOOLBAR_H);
        if self.sheet.is_some() {
            let s = Self::sheet_rect(area);
            let fields = [(0, s.x + 60), (1, s.x + 190)];
            for (i, x) in fields {
                v.push((Hit::SheetStep(i, 1), Rect::new(x, s.y + 60, 64, 26)));
                v.push((Hit::SheetStep(i, -1), Rect::new(x, s.y + 146, 64, 26)));
            }
            v.push((Hit::SheetSave, Rect::new(s.right() - 108, s.bottom() - 48, 90, 32)));
            v.push((Hit::SheetCancel, Rect::new(s.right() - 208, s.bottom() - 48, 90, 32)));
            return v;
        }
        match self.tab {
            Tab::World => {}
            Tab::Stopwatch => {
                let y = body.y + 300;
                v.push((Hit::LapReset, Rect::new(body.x + body.w / 2 - 170, y, 150, 40)));
                v.push((Hit::StartStop, Rect::new(body.x + body.w / 2 + 20, y, 150, 40)));
            }
            Tab::Timers => {
                for i in 0..PRESETS.len() {
                    let (col, row) = (i as i32 % 4, i as i32 / 4);
                    let w = (body.w - 48 - 3 * 12) / 4;
                    v.push((Hit::Preset(i), Rect::new(body.x + 24 + col * (w + 12), body.y + 44 + row * 60, w, 50)));
                }
                for i in 0..self.timers.len().min(4) {
                    let y = body.y + 208 + i as i32 * 72;
                    v.push((Hit::TimerToggle(i), Rect::new(body.right() - 196, y + 18, 80, 32)));
                    v.push((Hit::TimerCancel(i), Rect::new(body.right() - 108, y + 18, 84, 32)));
                }
            }
            Tab::Alarms => {
                v.push((Hit::AddAlarm, Rect::new(body.right() - 140, body.y + 16, 116, 32)));
                for i in 0..self.alarms.len().min(6) {
                    let y = body.y + 64 + i as i32 * 66;
                    v.push((Hit::AlarmToggle(i), Rect::new(body.right() - 110, y + 21, 44, 24)));
                    v.push((Hit::AlarmDelete(i), Rect::new(body.right() - 56, y + 19, 30, 28)));
                }
            }
        }
        v
    }

    fn sheet_rect(area: Rect) -> Rect {
        Rect::new(area.x + (area.w - 360) / 2, area.y + TOOLBAR_H + 30, 360, 290)
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        self.hits(self.area).into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h)
    }

    fn run(&mut self, h: Hit) {
        let now = time::uptime_ms();
        match h {
            Hit::Tab(i) => self.tab = TABS[i].0,
            Hit::StartStop => match self.sw_start.take() {
                Some(s) => self.sw_accum += now - s,
                None => self.sw_start = Some(now),
            },
            Hit::LapReset => {
                if self.sw_start.is_some() {
                    self.laps.insert(0, self.stopwatch_ms());
                } else {
                    self.sw_accum = 0;
                    self.laps.clear();
                }
            }
            Hit::Preset(i) => {
                if self.timers.len() < 4 {
                    let ms = PRESETS[i] * 60_000;
                    let label = if PRESETS[i] == 60 { String::from("1 hour") } else { format!("{} min", PRESETS[i]) };
                    self.timers.push(Timer { total_ms: ms, left_ms: ms, running_since: Some(now), label });
                }
            }
            Hit::TimerToggle(i) => {
                if let Some(t) = self.timers.get_mut(i) {
                    match t.running_since.take() {
                        Some(s) => t.left_ms = t.left_ms.saturating_sub(now - s),
                        None => t.running_since = Some(now),
                    }
                }
            }
            Hit::TimerCancel(i) => {
                if i < self.timers.len() {
                    self.timers.remove(i);
                }
            }
            Hit::AddAlarm => {
                let d = time::now();
                self.sheet = Some(Sheet {
                    hour: d.hour as i32,
                    minute: (d.minute as i32 / 5 * 5 + 5) % 60,
                    label: TextField::new("", "Label (optional)"),
                });
            }
            Hit::AlarmToggle(i) => {
                if let Some(a) = self.alarms.get_mut(i) {
                    a.enabled = !a.enabled;
                    self.save_alarms();
                }
            }
            Hit::AlarmDelete(i) => {
                if i < self.alarms.len() {
                    self.alarms.remove(i);
                    self.save_alarms();
                }
            }
            Hit::SheetStep(field, d) => {
                if let Some(s) = &mut self.sheet {
                    if field == 0 {
                        s.hour = (s.hour + d).rem_euclid(24);
                    } else {
                        s.minute = (s.minute + d * 5).rem_euclid(60);
                    }
                }
            }
            Hit::SheetSave => {
                if let Some(s) = self.sheet.take() {
                    let label = String::from(s.label.text().trim()).replace('|', "/");
                    self.alarms.push(Alarm {
                        hour: s.hour as u8,
                        minute: s.minute as u8,
                        enabled: true,
                        label,
                        rang: None,
                    });
                    self.alarms.sort_by_key(|a| (a.hour, a.minute));
                    self.save_alarms();
                }
            }
            Hit::SheetCancel => self.sheet = None,
        }
    }

    fn timer_left(t: &Timer) -> u64 {
        t.left_ms.saturating_sub(t.running_since.map(|s| time::uptime_ms() - s).unwrap_or(0))
    }

    // ------------------------------------------------------------ drawing

    fn draw_world(&self, cv: &mut Canvas, body: Rect) {
        let t = theme::current();
        let d = time::now();
        let h24 = prefs::get_bool(pref::CLOCK_24H);
        let local_offset = prefs::get_int("utc_offset", 0) as i32;
        // Local: large.
        let (cx, cy) = (body.x + 130, body.y + 150);
        face(cv, cx, cy, 100, d.hour as u32, d.minute as u32, Some(d.second as u32), t.dark);
        cv.text(body.x + 260, body.y + 110, "Local time", theme::ui(14), t.text_secondary);
        cv.text(body.x + 260, body.y + 160, &fmt_time(d.hour as u32, d.minute as u32, h24), theme::ui_bold(42), t.text);
        cv.text(body.x + 260, body.y + 192, &time::format_date(&d)[..10], theme::ui(15), t.text_secondary);
        let tz = if local_offset == 0 {
            String::from("UTC")
        } else {
            format!(
                "UTC{}{}:{:02}",
                if local_offset < 0 { "−" } else { "+" },
                local_offset.abs() / 60,
                local_offset.abs() % 60
            )
        };
        cv.text(
            body.x + 260,
            body.y + 216,
            &format!("Time zone {tz} · set in Settings"),
            theme::ui(12),
            t.text_secondary,
        );

        // Cities: a grid of small cards.
        let local_min = d.hour as i32 * 60 + d.minute as i32;
        let utc_min = local_min - local_offset;
        let cols = 4;
        let cw = (body.w - 48 - (cols - 1) * 12) / cols;
        for (i, (city, off)) in CITIES.iter().enumerate() {
            let (col, row) = (i as i32 % cols, i as i32 / cols);
            let r = Rect::new(body.x + 24 + col * (cw + 12), body.y + 280 + row * 104, cw, 92);
            cv.fill_round_rect(r, 12, t.window_bg_alt);
            let m = utc_min + off;
            let day = m.div_euclid(1440);
            let m = m.rem_euclid(1440);
            face(cv, r.x + 44, r.y + 46, 30, (m / 60) as u32, (m % 60) as u32, None, t.dark);
            cv.text_clipped(r.x + 86, r.y + 30, city, theme::ui_bold(13), t.text, r.w - 94);
            cv.text(r.x + 86, r.y + 54, &fmt_time((m / 60) as u32, (m % 60) as u32, h24), theme::ui(15), t.text);
            let rel = off - local_offset;
            let when = match day {
                0 => "Today",
                d if d > 0 => "Tomorrow",
                _ => "Yesterday",
            };
            let diff = if rel == 0 {
                String::from("same time")
            } else if rel % 60 == 0 {
                format!("{}{}h", if rel > 0 { "+" } else { "−" }, rel.abs() / 60)
            } else {
                format!("{}{}h{:02}", if rel > 0 { "+" } else { "−" }, rel.abs() / 60, rel.abs() % 60)
            };
            cv.text_clipped(r.x + 86, r.y + 74, &format!("{when}, {diff}"), theme::ui(11), t.text_secondary, r.w - 94);
        }
    }

    fn draw_stopwatch(&self, cv: &mut Canvas, body: Rect) {
        let t = theme::current();
        let ms = self.stopwatch_ms();
        let (cx, cy) = (body.x + body.w / 2, body.y + 150);
        // A sweep dial: one turn per minute.
        cv.fill_circle(cx, cy, 110, t.window_bg_alt);
        let progress = (ms % 60_000) as i32 * 1024 / 60_000;
        for k in 0..120 {
            let a = k * 1024 / 120;
            let on = a <= progress && (ms > 0);
            let c = if on { theme::accent() } else { with_alpha(t.text_secondary, 0x40) };
            let (x0, y0) = (cx + 104 * sin(a) / 16384, cy - 104 * sin(a + 256) / 16384);
            let (x1, y1) = (cx + 92 * sin(a) / 16384, cy - 92 * sin(a + 256) / 16384);
            cv.line(x0, y0, x1, y1, if k % 10 == 0 { 3 } else { 1 }, c);
        }
        let text = fmt_stopwatch(ms);
        let f = theme::mono(34);
        cv.text(cx - f.width(&text) / 2, cy + 12, &text, f, t.text);
        let running = self.sw_start.is_some();
        let [lap_r, start_r] = [Hit::LapReset, Hit::StartStop]
            .map(|h| self.hits(self.area).into_iter().find(|(k, _)| *k == h).map(|(_, r)| r).unwrap());
        let lap_label = if running { "Lap" } else { "Reset" };
        button(cv, lap_r, lap_label, ButtonStyle::Secondary, self.hover == Some(Hit::LapReset));
        let (label, style) = if running { ("Stop", ButtonStyle::Danger) } else { ("Start", ButtonStyle::Primary) };
        button(cv, start_r, label, style, self.hover == Some(Hit::StartStop));
        // Laps (newest first), fastest in green, slowest in red.
        let splits: Vec<u64> =
            (0..self.laps.len()).map(|i| self.laps[i] - self.laps.get(i + 1).copied().unwrap_or(0)).collect();
        let (best, worst) =
            if splits.len() > 1 { (splits.iter().min().copied(), splits.iter().max().copied()) } else { (None, None) };
        let list = Rect::new(body.x + (body.w - 420) / 2, body.y + 360, 420, body.h - 370);
        cv.with_clip(list, |cv| {
            for (i, s) in splits.iter().enumerate() {
                let y = list.y + i as i32 * 28;
                let c = if Some(*s) == best {
                    0xFF30_B050
                } else if Some(*s) == worst {
                    0xFFFF_453A
                } else {
                    t.text
                };
                cv.text(list.x, y + 18, &format!("Lap {}", splits.len() - i), theme::ui(14), c);
                let v = fmt_stopwatch(*s);
                let f = theme::mono(14);
                cv.text(list.right() - f.width(&v), y + 18, &v, f, c);
                cv.fill_rect(Rect::new(list.x, y + 27, list.w, 1), t.separator);
            }
        });
    }

    fn draw_timers(&self, cv: &mut Canvas, body: Rect) {
        let t = theme::current();
        cv.text(body.x + 24, body.y + 30, "Start a timer", theme::ui_bold(14), t.text_secondary);
        let hits = self.hits(self.area);
        for (i, m) in PRESETS.iter().enumerate() {
            let r = hits.iter().find(|(h, _)| *h == Hit::Preset(i)).unwrap().1;
            let hovered = self.hover == Some(Hit::Preset(i));
            cv.fill_round_rect(r, 12, if hovered { t.hover } else { t.window_bg_alt });
            let label = if *m == 60 { String::from("1 hour") } else { format!("{m} min") };
            cv.text_centered(r, &label, theme::ui_bold(15), t.text);
        }
        if self.timers.is_empty() {
            let msg = "Running timers appear here. You'll get a notification when one ends.";
            cv.text_centered(Rect::new(body.x, body.y + 250, body.w, 20), msg, theme::ui(13), t.text_secondary);
        }
        for (i, tm) in self.timers.iter().enumerate().take(4) {
            let y = body.y + 208 + i as i32 * 72;
            let card = Rect::new(body.x + 24, y, body.w - 48, 64);
            cv.fill_round_rect(card, 14, t.window_bg_alt);
            // Progress ring.
            let left = Self::timer_left(tm);
            let (cx, cy) = (card.x + 36, card.y + 32);
            let frac = (left * 1024 / tm.total_ms.max(1)) as i32;
            for k in 0..64 {
                let a = k * 1024 / 64;
                let c = if a < frac { theme::accent() } else { with_alpha(t.text_secondary, 0x40) };
                cv.fill_circle(cx + 20 * sin(a) / 16384, cy - 20 * sin(a + 256) / 16384, 2, c);
            }
            let f = theme::mono(26);
            let text = fmt_timer(left);
            cv.text(card.x + 76, card.y + 42, &text, f, if left == 0 { theme::CLOSE } else { t.text });
            cv.text(card.x + 76 + f.width(&text) + 14, card.y + 40, &tm.label, theme::ui(13), t.text_secondary);
            let toggle = hits.iter().find(|(h, _)| *h == Hit::TimerToggle(i)).unwrap().1;
            let cancel = hits.iter().find(|(h, _)| *h == Hit::TimerCancel(i)).unwrap().1;
            let label = if tm.running_since.is_some() { "Pause" } else { "Resume" };
            if left > 0 {
                button(cv, toggle, label, ButtonStyle::Secondary, self.hover == Some(Hit::TimerToggle(i)));
            }
            button(
                cv,
                cancel,
                if left == 0 { "Done" } else { "Cancel" },
                ButtonStyle::Secondary,
                self.hover == Some(Hit::TimerCancel(i)),
            );
        }
    }

    fn draw_alarms(&self, cv: &mut Canvas, body: Rect) {
        let t = theme::current();
        let h24 = prefs::get_bool(pref::CLOCK_24H);
        let hits = self.hits(self.area);
        cv.text(
            body.x + 24,
            body.y + 38,
            "Alarms ring while Clock is open (it can stay minimized).",
            theme::ui(12),
            t.text_secondary,
        );
        let add = hits.iter().find(|(h, _)| *h == Hit::AddAlarm).unwrap().1;
        button(cv, add, "Add Alarm", ButtonStyle::Primary, self.hover == Some(Hit::AddAlarm));
        if self.alarms.is_empty() {
            cv.text_centered(Rect::new(body.x, body.y + 200, body.w, 24), "No alarms", theme::ui(15), t.text_secondary);
        }
        for (i, a) in self.alarms.iter().enumerate().take(6) {
            let y = body.y + 64 + i as i32 * 66;
            let card = Rect::new(body.x + 24, y, body.w - 48, 58);
            cv.fill_round_rect(card, 12, t.window_bg_alt);
            let c = if a.enabled { t.text } else { t.text_secondary };
            cv.text(card.x + 18, card.y + 38, &fmt_time(a.hour as u32, a.minute as u32, h24), theme::ui(28), c);
            let label = if a.label.is_empty() { "Alarm" } else { a.label.as_str() };
            cv.text_clipped(card.x + 190, card.y + 35, label, theme::ui(14), t.text_secondary, card.w - 330);
            let tr = hits.iter().find(|(h, _)| *h == Hit::AlarmToggle(i)).unwrap().1;
            widgets::toggle(cv, tr, a.enabled);
            let dr = hits.iter().find(|(h, _)| *h == Hit::AlarmDelete(i)).unwrap().1;
            if self.hover == Some(Hit::AlarmDelete(i)) {
                cv.fill_round_rect(dr, 7, t.hover);
            }
            let (cx, cy) = dr.center();
            cv.line(cx - 5, cy - 5, cx + 5, cy + 5, 2, t.text_secondary);
            cv.line(cx + 5, cy - 5, cx - 5, cy + 5, 2, t.text_secondary);
        }
    }

    fn draw_sheet(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let Some(s) = &mut self.sheet else { return };
        cv.fill_rect(area, with_alpha(0x000000, 0x30));
        let r = Self::sheet_rect(area);
        cv.shadow(r, 14, 24, 8, t.shadow);
        cv.fill_round_rect(r, 14, t.window_bg);
        cv.stroke_round_rect(r, 14, t.window_border);
        cv.text(r.x + 20, r.y + 34, "New Alarm", theme::ui_bold(16), t.text);
        for (x, v) in [(r.x + 60, s.hour), (r.x + 190, s.minute)] {
            let cx = x + 32;
            cv.line(cx - 7, r.y + 77, cx, r.y + 70, 2, t.text_secondary);
            cv.line(cx, r.y + 70, cx + 7, r.y + 77, 2, t.text_secondary);
            cv.text_centered(Rect::new(x, r.y + 92, 64, 50), &format!("{:02}", v), theme::mono(40), t.text);
            cv.line(cx - 7, r.y + 155, cx, r.y + 162, 2, t.text_secondary);
            cv.line(cx, r.y + 162, cx + 7, r.y + 155, 2, t.text_secondary);
        }
        cv.text_centered(Rect::new(r.x + 124, r.y + 92, 66, 50), ":", theme::mono(40), t.text);
        s.label.draw(cv, Rect::new(r.x + 20, r.y + 186, r.w - 40, 34), env.focused);
        let hits = self.hits(area);
        for (h, hr) in hits {
            match h {
                Hit::SheetSave => button(cv, hr, "Save", ButtonStyle::Primary, self.hover == Some(h)),
                Hit::SheetCancel => button(cv, hr, "Cancel", ButtonStyle::Secondary, self.hover == Some(h)),
                Hit::SheetStep(..) if self.hover == Some(h) => cv.fill_round_rect(hr, 6, with_alpha(t.text, 0x14)),
                _ => {}
            }
        }
    }

    /// Timers that ended and alarms that are due post notifications.
    fn check_alerts(&mut self) -> bool {
        let mut changed = false;
        for tm in &mut self.timers {
            if let Some(s) = tm.running_since {
                let now = time::uptime_ms();
                if now - s >= tm.left_ms {
                    tm.left_ms = 0;
                    tm.running_since = None;
                    corekit::notify::post("Timer done", &format!("Your {} timer has ended.", tm.label));
                    corekit::audio::play_system("timer");
                    changed = true;
                }
            }
        }
        let d = time::now();
        let day = d.day as u16 | (d.month as u16) << 8;
        let mut fired = false;
        for a in &mut self.alarms {
            if a.enabled && a.hour == d.hour && a.minute == d.minute && a.rang != Some((d.hour, d.minute, day)) {
                a.rang = Some((d.hour, d.minute, day));
                let label = if a.label.is_empty() { String::from("Alarm") } else { a.label.clone() };
                corekit::notify::post(&label, &format!("It's {:02}:{:02}.", d.hour, d.minute));
                corekit::audio::play_system("timer");
                fired = true;
            }
        }
        changed | fired
    }
}

impl App for Clock {
    fn title(&self) -> String {
        String::from("Clock")
    }

    fn size(&self) -> (i32, i32) {
        (800, 560)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        let t = theme::current();
        let bar = Rect::new(area.x, area.y, area.w, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(area.x, bar.bottom() - 1, area.w, 1), t.separator);
        let labels: Vec<&str> = TABS.iter().map(|(_, l)| *l).collect();
        let sel = TABS.iter().position(|(k, _)| *k == self.tab).unwrap_or(0);
        let hov = match self.hover {
            Some(Hit::Tab(i)) => Some(i),
            _ => None,
        };
        widgets::segmented(cv, Self::tabs(area), &labels, sel, hov);
        let body = Rect::new(area.x, area.y + TOOLBAR_H, area.w, area.h - TOOLBAR_H);
        match self.tab {
            Tab::World => self.draw_world(cv, body),
            Tab::Stopwatch => self.draw_stopwatch(cv, body),
            Tab::Timers => self.draw_timers(cv, body),
            Tab::Alarms => self.draw_alarms(cv, body),
        }
        if self.sheet.is_some() {
            self.draw_sheet(cv, area, env);
        }
    }

    fn click(&mut self, x: i32, y: i32, _area: Rect, env: &mut Env) -> bool {
        if let Some(s) = &mut self.sheet {
            if s.label.click(x, y, env.mods.shift, env.now_ms) {
                return true;
            }
        }
        match self.hit_at(x, y) {
            Some(h) => {
                self.run(h);
                true
            }
            None => false,
        }
    }

    fn hover(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        let h = if x < 0 { None } else { self.hit_at(x, y) };
        core::mem::replace(&mut self.hover, h) != h
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        if let Some(s) = &mut self.sheet {
            match ev.code {
                KeyCode::Enter => self.run(Hit::SheetSave),
                KeyCode::Escape => self.sheet = None,
                _ => {
                    s.label.key(ev, env.now_ms);
                }
            }
            return true;
        }
        if self.tab == Tab::Stopwatch && ev.ch == Some(' ') {
            self.run(Hit::StartStop);
            return true;
        }
        false
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let alerts = self.check_alerts();
        let caret = match &mut self.sheet {
            Some(s) => s.label.tick(env.now_ms, env.focused),
            None => false,
        };
        // Redraw only when something on screen moves.
        let second = time::now().second;
        let new_second = core::mem::replace(&mut self.last_second, second) != second;
        let moving = match self.tab {
            Tab::World => new_second,
            Tab::Stopwatch => self.sw_start.is_some(),
            Tab::Timers => new_second && self.timers.iter().any(|t| t.running_since.is_some()),
            Tab::Alarms => false,
        };
        alerts || caret || moving
    }

    fn tick_interval(&self) -> u64 {
        match self.tab {
            Tab::Stopwatch if self.sw_start.is_some() => 33,
            _ if self.sheet.is_some() => 250,
            _ => 200,
        }
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    aurorakit::run(Clock::new())
}
