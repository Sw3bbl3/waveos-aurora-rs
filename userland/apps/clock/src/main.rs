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
use corekit::abi::pref;
use corekit::{fs, prefs, time};
use waveui::lumen::canvas::{with_alpha, Canvas as Surface};
use waveui::lumen::math::sin;
use waveui::lumen::theme;
use waveui::prelude::*;
use waveui::style::Weight;

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

#[derive(Clone)]
pub enum Msg {
    Tab(usize),
    StartStop,
    LapReset,
    Preset(usize),
    TimerToggle(usize),
    TimerCancel(usize),
    AddAlarm,
    AlarmToggle(usize, bool),
    AlarmDelete(usize),
    SheetHour(i64),
    SheetMinute(i64),
    SheetLabel(String),
    SheetSave,
    SheetCancel,
}

struct Sheet {
    hour: i64,
    minute: i64,
    label: String,
}

pub struct Clock {
    tab: Tab,
    // Stopwatch.
    sw_start: Option<u64>,
    sw_accum: u64,
    laps: Vec<u64>,
    // Timers and alarms.
    timers: Vec<Timer>,
    alarms: Vec<Alarm>,
    sheet: Option<Sheet>,
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
fn face(cv: &mut Surface, cx: i32, cy: i32, r: i32, (h, m, s): (u32, u32, Option<u32>), dark_face: bool) {
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
    let hand = |cv: &mut Surface, turns_1024: i32, len: i32, w: i32, c: u32| {
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
            sw_start: None,
            sw_accum: 0,
            laps: Vec::new(),
            timers: Vec::new(),
            alarms: Vec::new(),
            sheet: None,
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

    fn timer_left(t: &Timer) -> u64 {
        t.left_ms.saturating_sub(t.running_since.map(|s| time::uptime_ms() - s).unwrap_or(0))
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

fn city_offset(rel: i32) -> String {
    if rel == 0 {
        String::from("same time")
    } else if rel % 60 == 0 {
        format!("{}{}h", if rel > 0 { "+" } else { "−" }, rel.abs() / 60)
    } else {
        format!("{}{}h{:02}", if rel > 0 { "+" } else { "−" }, rel.abs() / 60, rel.abs() % 60)
    }
}

impl Clock {
    fn world(&self, env: &Env) -> View<'_, Msg> {
        let d = time::now();
        let h24 = prefs::get_bool(pref::CLOCK_24H);
        let local = prefs::get_int("utc_offset", 0) as i32;
        let dark = env.dark;
        let tz = if local == 0 {
            String::from("UTC")
        } else {
            format!("UTC{}{}:{:02}", if local < 0 { "−" } else { "+" }, local.abs() / 60, local.abs() % 60)
        };
        let hms = (d.hour as u32, d.minute as u32, Some(d.second as u32));
        let big = HStack::new()
            .push(
                Canvas::new(move |cv, r| {
                    let (cx, cy) = r.center();
                    face(cv, cx, cy, 100, hms, dark)
                })
                .size(210, 210),
            )
            .push(
                VStack::new()
                    .push(Text::new("Local time").foreground(Color::SecondaryLabel))
                    .push(
                        Text::new(fmt_time(d.hour as u32, d.minute as u32, h24))
                            .style(TextStyle::Sized(42, Weight::Bold)),
                    )
                    .push(
                        Text::new(String::from(&time::format_date(&d)[..10]))
                            .style(TextStyle::Sized(15, Weight::Regular))
                            .foreground(Color::SecondaryLabel),
                    )
                    .push(
                        Text::new(format!("Time zone {tz} · set in Settings"))
                            .style(TextStyle::Footnote)
                            .foreground(Color::SecondaryLabel),
                    )
                    .spacing(4)
                    .alignment(Align::Start),
            )
            .spacing(32);
        // Cities: a grid of small cards.
        let utc_min = d.hour as i32 * 60 + d.minute as i32 - local;
        let mut grid = Grid::new(170).spacing(12);
        for (city, off) in CITIES {
            let m = utc_min + off;
            let day = m.div_euclid(1440);
            let m = m.rem_euclid(1440);
            let when = match day {
                0 => "Today",
                d if d > 0 => "Tomorrow",
                _ => "Yesterday",
            };
            let hm = ((m / 60) as u32, (m % 60) as u32, None);
            grid = grid.push(
                HStack::new()
                    .push(
                        Canvas::new(move |cv, r| {
                            let (cx, cy) = r.center();
                            face(cv, cx, cy, 30, hm, dark)
                        })
                        .size(64, 64),
                    )
                    .push(
                        VStack::new()
                            .push(Text::new(city).style(TextStyle::Headline))
                            .push(Text::new(fmt_time(hm.0, hm.1, h24)).style(TextStyle::Sized(15, Weight::Regular)))
                            .push(
                                Text::new(format!("{when}, {}", city_offset(off - local)))
                                    .style(TextStyle::Caption)
                                    .foreground(Color::SecondaryLabel),
                            )
                            .spacing(2)
                            .alignment(Align::Start)
                            .grow(1),
                    )
                    .spacing(10)
                    .padding(12)
                    .background_rounded(Color::GroupedBackground, 12)
                    .fill_width(),
            );
        }
        VStack::new().push(big).push(grid).spacing(24).alignment(Align::Start).padding(24).fill_width()
    }

    fn stopwatch(&self) -> View<'_, Msg> {
        let ms = self.stopwatch_ms();
        let running = self.sw_start.is_some();
        // A sweep dial: one turn per minute.
        let dial = Canvas::new(move |cv, r| {
            let t = theme::current();
            let (cx, cy) = r.center();
            cv.fill_circle(cx, cy, 110, t.window_bg_alt);
            let progress = (ms % 60_000) as i32 * 1024 / 60_000;
            for k in 0..120 {
                let a = k * 1024 / 120;
                let on = a <= progress && ms > 0;
                let c = if on { theme::accent() } else { with_alpha(t.text_secondary, 0x40) };
                let (x0, y0) = (cx + 104 * sin(a) / 16384, cy - 104 * sin(a + 256) / 16384);
                let (x1, y1) = (cx + 92 * sin(a) / 16384, cy - 92 * sin(a + 256) / 16384);
                cv.line(x0, y0, x1, y1, if k % 10 == 0 { 3 } else { 1 }, c);
            }
            let text = fmt_stopwatch(ms);
            let f = theme::mono(34);
            cv.text(cx - f.width(&text) / 2, cy + 12, &text, f, t.text);
        })
        .size(240, 240);
        // Laps (newest first), fastest in green, slowest in red.
        let splits: Vec<u64> =
            (0..self.laps.len()).map(|i| self.laps[i] - self.laps.get(i + 1).copied().unwrap_or(0)).collect();
        let (best, worst) =
            if splits.len() > 1 { (splits.iter().min().copied(), splits.iter().max().copied()) } else { (None, None) };
        let laps = splits.iter().enumerate().map(|(i, s)| {
            let c = if Some(*s) == best {
                Color::Green
            } else if Some(*s) == worst {
                Color::Red
            } else {
                Color::Label
            };
            row(
                format!("Lap {}", splits.len() - i),
                Text::new(fmt_stopwatch(*s)).style(TextStyle::Sized(14, Weight::Mono)),
            )
            .height(32)
            .foreground(c)
        });
        VStack::new()
            .push(dial)
            .push(
                HStack::new()
                    .push(
                        Button::new(if running { "Lap" } else { "Reset" })
                            .action(Msg::LapReset)
                            .control_size(ControlSize::Large)
                            .width(150),
                    )
                    .push(
                        Button::new(if running { "Stop" } else { "Start" })
                            .action(Msg::StartStop)
                            .button_style(if running { ButtonStyle::Destructive } else { ButtonStyle::Prominent })
                            .control_size(ControlSize::Large)
                            .shortcut(Shortcut::key(Key::Char(' ')))
                            .width(150),
                    )
                    .spacing(20),
            )
            .push(Section::new().extend(laps).width(420).hidden(splits.is_empty()))
            .spacing(24)
            .padding(24)
            .fill_width()
    }

    fn timers_tab(&self) -> View<'_, Msg> {
        let mut presets = Grid::new(150).spacing(12);
        for (i, m) in PRESETS.iter().enumerate() {
            let label = if *m == 60 { String::from("1 hour") } else { format!("{m} min") };
            presets = presets.push(
                Text::centered(label)
                    .style(TextStyle::Sized(15, Weight::Bold))
                    .height(50)
                    .background_rounded(Color::GroupedBackground, 12)
                    .on_tap(Msg::Preset(i))
                    .disabled(self.timers.len() >= 4),
            );
        }
        let mut v = VStack::new()
            .push(Text::new("Start a timer").style(TextStyle::Headline).foreground(Color::SecondaryLabel))
            .push(presets)
            .spacing(12)
            .alignment(Align::Stretch);
        if self.timers.is_empty() {
            v = v.push(
                Text::centered("Running timers appear here. You'll get a notification when one ends.")
                    .foreground(Color::SecondaryLabel)
                    .padding(32),
            );
        }
        for (i, tm) in self.timers.iter().enumerate() {
            let left = Self::timer_left(tm);
            let frac = (left * 1024 / tm.total_ms.max(1)) as i32;
            let ring = Canvas::new(move |cv, r| {
                let t = theme::current();
                let (cx, cy) = r.center();
                for k in 0..64 {
                    let a = k * 1024 / 64;
                    let c = if a < frac { theme::accent() } else { with_alpha(t.text_secondary, 0x40) };
                    cv.fill_circle(cx + 20 * sin(a) / 16384, cy - 20 * sin(a + 256) / 16384, 2, c);
                }
            })
            .size(48, 48);
            v =
                v.push(
                    HStack::new()
                        .push(ring)
                        .push(
                            Text::new(fmt_timer(left))
                                .style(TextStyle::Sized(26, Weight::Mono))
                                .foreground(if left == 0 { Color::Red } else { Color::Label }),
                        )
                        .push(Text::new(tm.label.clone()).foreground(Color::SecondaryLabel).grow(1))
                        .push(
                            Button::new(if tm.running_since.is_some() { "Pause" } else { "Resume" })
                                .action(Msg::TimerToggle(i))
                                .hidden(left == 0)
                                .min_width(80),
                        )
                        .push(
                            Button::new(if left == 0 { "Done" } else { "Cancel" })
                                .action(Msg::TimerCancel(i))
                                .min_width(80),
                        )
                        .spacing(14)
                        .padding_xy(14, 8)
                        .background_rounded(Color::GroupedBackground, 14)
                        .fill_width(),
                );
        }
        v.padding(24).fill_width()
    }

    fn alarms_tab(&self) -> View<'_, Msg> {
        let h24 = prefs::get_bool(pref::CLOCK_24H);
        let mut v = VStack::new()
            .push(
                HStack::new()
                    .push(
                        Text::new("Alarms ring while Clock is open (it can stay minimized).")
                            .style(TextStyle::Callout)
                            .foreground(Color::SecondaryLabel)
                            .grow(1),
                    )
                    .push(Button::new("Add Alarm").action(Msg::AddAlarm).button_style(ButtonStyle::Prominent))
                    .fill_width(),
            )
            .spacing(10)
            .alignment(Align::Stretch);
        if self.alarms.is_empty() {
            v = v.push(
                Text::centered("No alarms").style(TextStyle::Title2).foreground(Color::SecondaryLabel).padding(60),
            );
        }
        for (i, a) in self.alarms.iter().enumerate() {
            let label = if a.label.is_empty() { String::from("Alarm") } else { a.label.clone() };
            v = v.push(
                HStack::new()
                    .push(
                        Text::new(fmt_time(a.hour as u32, a.minute as u32, h24))
                            .style(TextStyle::Sized(28, Weight::Regular))
                            .foreground(if a.enabled { Color::Label } else { Color::SecondaryLabel })
                            .width(170),
                    )
                    .push(Text::new(label).foreground(Color::SecondaryLabel).grow(1))
                    .push(Toggle::new("", a.enabled).on_change(move |on| Msg::AlarmToggle(i, on)))
                    .push(Button::symbol(Symbol::Close).action(Msg::AlarmDelete(i)).help("Delete alarm"))
                    .spacing(12)
                    .padding_xy(18, 8)
                    .background_rounded(Color::GroupedBackground, 12)
                    .fill_width(),
            );
        }
        v.padding(24).fill_width()
    }
}

impl App for Clock {
    type Message = Msg;

    fn title(&self) -> String {
        String::from("Clock")
    }

    fn size(&self) -> (i32, i32) {
        (800, 560)
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        let now = time::uptime_ms();
        match msg {
            Msg::Tab(i) => self.tab = TABS[i].0,
            Msg::StartStop => match self.sw_start.take() {
                Some(s) => self.sw_accum += now - s,
                None => self.sw_start = Some(now),
            },
            Msg::LapReset => {
                if self.sw_start.is_some() {
                    self.laps.insert(0, self.stopwatch_ms());
                } else {
                    self.sw_accum = 0;
                    self.laps.clear();
                }
            }
            Msg::Preset(i) => {
                if self.timers.len() < 4 {
                    let ms = PRESETS[i] * 60_000;
                    let label = if PRESETS[i] == 60 { String::from("1 hour") } else { format!("{} min", PRESETS[i]) };
                    self.timers.push(Timer { total_ms: ms, left_ms: ms, running_since: Some(now), label });
                }
            }
            Msg::TimerToggle(i) => {
                if let Some(t) = self.timers.get_mut(i) {
                    match t.running_since.take() {
                        Some(s) => t.left_ms = t.left_ms.saturating_sub(now - s),
                        None => t.running_since = Some(now),
                    }
                }
            }
            Msg::TimerCancel(i) => {
                if i < self.timers.len() {
                    self.timers.remove(i);
                }
            }
            Msg::AddAlarm => {
                let d = time::now();
                self.sheet = Some(Sheet {
                    hour: d.hour as i64,
                    minute: (d.minute as i64 / 5 * 5 + 5) % 60,
                    label: String::new(),
                });
            }
            Msg::AlarmToggle(i, on) => {
                if let Some(a) = self.alarms.get_mut(i) {
                    a.enabled = on;
                    self.save_alarms();
                }
            }
            Msg::AlarmDelete(i) => {
                if i < self.alarms.len() {
                    self.alarms.remove(i);
                    self.save_alarms();
                }
            }
            Msg::SheetHour(h) => {
                if let Some(s) = &mut self.sheet {
                    s.hour = h;
                }
            }
            Msg::SheetMinute(m) => {
                if let Some(s) = &mut self.sheet {
                    s.minute = m;
                }
            }
            Msg::SheetLabel(l) => {
                if let Some(s) = &mut self.sheet {
                    s.label = l;
                }
            }
            Msg::SheetSave => {
                if let Some(s) = self.sheet.take() {
                    let label = String::from(s.label.trim()).replace('|', "/");
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
            Msg::SheetCancel => self.sheet = None,
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(match self.tab {
            Tab::Stopwatch if self.sw_start.is_some() => 33,
            _ => 200,
        })
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        let alerts = self.check_alerts();
        // Redraw only when something on screen moves.
        let second = time::now().second;
        let new_second = core::mem::replace(&mut self.last_second, second) != second;
        let moving = match self.tab {
            Tab::World => new_second,
            Tab::Stopwatch => self.sw_start.is_some(),
            Tab::Timers => new_second && self.timers.iter().any(|t| t.running_since.is_some()),
            Tab::Alarms => false,
        };
        alerts || moving
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        let s = self.sheet.as_ref()?;
        Some(
            VStack::new()
                .push(Text::new("New Alarm").style(TextStyle::Title2))
                .push(
                    Section::new()
                        .push(Stepper::new("Hour", s.hour, 0..=23).on_change(Msg::SheetHour).min_height(size::ROW))
                        .push(
                            Stepper::new("Minute", s.minute, 0..=59).on_change(Msg::SheetMinute).min_height(size::ROW),
                        ),
                )
                .push(
                    TextField::new("Label (optional)", s.label.clone())
                        .on_change(Msg::SheetLabel)
                        .on_submit(Msg::SheetSave)
                        .focused(),
                )
                .push(
                    HStack::new()
                        .push(Spacer::new())
                        .push(Button::new("Cancel").action(Msg::SheetCancel).shortcut(Shortcut::cancel()).min_width(84))
                        .push(
                            Button::new("Save")
                                .action(Msg::SheetSave)
                                .button_style(ButtonStyle::Prominent)
                                .shortcut(Shortcut::default_action())
                                .min_width(84),
                        )
                        .spacing(8),
                )
                .spacing(14)
                .alignment(Align::Stretch)
                .width(340),
        )
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let sel = TABS.iter().position(|(k, _)| *k == self.tab).unwrap_or(0);
        let content = match self.tab {
            Tab::World => self.world(env),
            Tab::Stopwatch => self.stopwatch(),
            Tab::Timers => self.timers_tab(),
            Tab::Alarms => self.alarms_tab(),
        };
        VStack::new()
            .push(toolbar(vec![
                Spacer::new(),
                Picker::new(TABS.iter().map(|t| t.1), sel).on_change(Msg::Tab).width(440),
                Spacer::new(),
            ]))
            .push(Divider::new())
            .push(ScrollView::new(content).id(("tab", sel)))
            .spacing(0)
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Clock::new())
}
