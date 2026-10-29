//! Settings — appearance, display, keyboard, trackpad and mouse, game
//! controllers, network, date and time, sound, notifications, the desktop,
//! login, accessibility, and information about this computer.
//!
//! Start with a pane name (`Settings.elf keyboard`) or a setting
//! (`Settings.elf keyboard/repeat-rate`) to open it, as Seek does.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use aurora_image::Image as Picture;
use corekit::abi::{display, pref, DateTime, DisplayMode};
use corekit::gamepad::{Button as Pad, Gamepad, State as PadState};
use corekit::prefs;
use waveui::lumen::canvas::{mix, with_alpha, Canvas as Surface};
use waveui::lumen::geom::Rect;
use waveui::lumen::icons::{self, Icon};
use waveui::lumen::{theme, wallpaper};
use waveui::prelude::*;
use waveui::style::Weight;
use waveui::Request;

mod search;

const THUMB_W: i32 = 132;
const THUMB_H: i32 = 84;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Pane {
    Appearance,
    Display,
    Keyboard,
    Trackpad,
    Mouse,
    Controllers,
    Network,
    DateTime,
    Sound,
    Notifications,
    Desktop,
    Bar,
    Login,
    Accessibility,
    About,
}

const PANES: [(Pane, &str, &str, u32); 15] = [
    (Pane::Appearance, "Appearance", "appearance", 0xFF5E5CE6),
    (Pane::Display, "Display", "display", 0xFF0A84FF),
    (Pane::Keyboard, "Keyboard", "keyboard", 0xFF8E8E93),
    (Pane::Trackpad, "Trackpad", "trackpad", 0xFF64D2FF),
    (Pane::Mouse, "Mouse", "mouse", 0xFF6E6E80),
    (Pane::Controllers, "Game Controllers", "controllers", 0xFF34C759),
    (Pane::Network, "Network", "network", 0xFF30D158),
    (Pane::DateTime, "Date & Time", "datetime", 0xFFFF9F0A),
    (Pane::Sound, "Sound", "sound", 0xFFFF375F),
    (Pane::Notifications, "Notifications", "notifications", 0xFFFF453A),
    (Pane::Desktop, "Desktop & Harbor", "desktop", 0xFF5856D6),
    (Pane::Bar, "Bar & Crest", "bar", 0xFF2F62F5),
    (Pane::Login, "Login", "login", 0xFF8E5CF5),
    (Pane::Accessibility, "Accessibility", "accessibility", 0xFF147EFB),
    (Pane::About, "About", "about", 0xFF30B0C7),
];

const LAYOUTS: [(&str, &str); 6] =
    [("us", "U.S."), ("uk", "British"), ("de", "German"), ("fr", "French"), ("es", "Spanish"), ("se", "Swedish")];

#[derive(Clone)]
pub enum Msg {
    Pane(Pane),
    Search(String),
    SearchMove(i32),
    SearchOpen,
    OpenSetting(usize),
    SetDark(bool),
    Accent(usize),
    Wallpaper(u8),
    CustomWallpaper,
    Mode(usize),
    Scale(usize),
    Layout(&'static str),
    Try(String),
    Toggle(&'static str, bool),
    Int(&'static str, i64),
    Clock(usize, i64),
    SetClock,
    Zone(i64),
    WifiChoose(usize),
    WifiPassword(String),
    WifiJoin,
    WifiCancel,
    WifiDisconnect,
    Rumble(u32),
    OpenAbout,
    ClearNote,
}

pub struct Settings {
    pane: Pane,
    query: String,
    selected: usize,
    /// The setting a search or deep link pointed at, to focus once shown.
    target: Option<&'static str>,
    /// Wallpaper pictures, day and night.
    thumbs: Vec<[Rc<Vec<u32>>; 2]>,
    custom_thumb: Option<Rc<Picture>>,
    modes: Vec<DisplayMode>,
    note: Option<String>,
    try_text: String,
    /// Date and time being edited: year, month, day, hour, minute.
    clock: [i64; 5],
    clock_edited: bool,
    info: corekit::abi::SysInfo,
    /// Wi-Fi: the adapter's state and the networks around (refreshed each second).
    wifi: Option<corekit::abi::WifiStatus>,
    wifi_nets: Vec<corekit::abi::WifiNetwork>,
    /// The protected network whose password is being typed.
    wifi_chosen: Option<Vec<u8>>,
    wifi_password: String,
    pads: Vec<(Gamepad, Option<PadState>)>,
    now: corekit::abi::DateTime,
}

fn days_in(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl Settings {
    pub fn new(start: Option<&str>) -> Self {
        let thumbs = (0..wallpaper::NAMES.len() as u8)
            .map(|i| [Rc::new(wallpaper_thumb(i, false)), Rc::new(wallpaper_thumb(i, true))])
            .collect();
        let mut s = Self {
            pane: Pane::Appearance,
            query: String::new(),
            selected: 0,
            target: None,
            thumbs,
            custom_thumb: None,
            modes: prefs::display_modes(),
            note: None,
            try_text: String::new(),
            clock: [0; 5],
            clock_edited: false,
            info: corekit::process::sys_info(),
            wifi: None,
            wifi_nets: Vec::new(),
            wifi_chosen: None,
            wifi_password: String::new(),
            pads: Vec::new(),
            now: corekit::time::now(),
        };
        s.load_custom_thumb();
        s.sync_clock();
        if let Some(start) = start {
            if let Some(i) = search::SETTINGS.iter().position(|e| e.id == start) {
                s.jump_setting(i);
            } else if let Some(p) = PANES.iter().find(|p| p.2 == start.split('/').next().unwrap_or(start)) {
                s.pane = p.0;
            }
        }
        s.refresh();
        s
    }

    fn load_custom_thumb(&mut self) {
        self.custom_thumb = prefs::get("wallpaper_image")
            .and_then(|p| corekit::fs::read(&p).ok())
            .and_then(|d| aurora_image::decode(&d).ok())
            .map(|img| Rc::new(img.thumbnail(THUMB_W as u32 * 2, THUMB_H as u32 * 2)));
    }

    fn sync_clock(&mut self) {
        let d = corekit::time::now();
        self.clock = [d.year as i64, d.month as i64, d.day as i64, d.hour as i64, d.minute as i64];
    }

    fn jump_setting(&mut self, i: usize) {
        let s = search::SETTINGS[i];
        self.pane = s.pane;
        self.query.clear();
        self.target = Some(s.id);
    }

    /// Re-reads what the open pane shows.
    fn refresh(&mut self) {
        match self.pane {
            Pane::Display => self.modes = prefs::display_modes(),
            Pane::DateTime => {
                self.now = corekit::time::now();
                if !self.clock_edited {
                    self.sync_clock();
                }
            }
            Pane::Network => {
                self.wifi = corekit::net::wifi_status();
                if self.wifi.is_some() {
                    self.wifi_nets = corekit::net::wifi_networks();
                }
            }
            Pane::Controllers => {
                self.pads = corekit::gamepad::connected()
                    .into_iter()
                    .map(|p| {
                        let s = p.state();
                        (p, s)
                    })
                    .collect();
            }
            Pane::About => self.info = corekit::process::sys_info(),
            _ => {}
        }
    }

    fn flash(&mut self, cx: &mut Context<Msg>, msg: String) {
        self.note = Some(msg);
        cx.cancel(|m| matches!(m, Msg::ClearNote));
        cx.after(4000, Msg::ClearNote);
    }

    fn set(&mut self, cx: &mut Context<Msg>, key: &str, value: &str) {
        if let Err(e) = prefs::set(key, value) {
            self.flash(cx, format!("Could not save: {e}"));
        }
    }

    /// Starts joining the network named `ssid` (with the typed password, if any).
    fn wifi_join(&mut self, cx: &mut Context<Msg>, ssid: &[u8], password: &str) {
        match corekit::net::wifi_connect(ssid, password) {
            Ok(_) => {
                self.wifi_chosen = None;
                self.wifi_password.clear();
            }
            Err(e) => self.flash(cx, format!("Could not join: {e}")),
        }
        self.wifi = corekit::net::wifi_status();
    }
}

// ------------------------------------------------------------------ views

/// A page: its title, then its sections.
fn page<'a>(title: &str, sections: Vec<View<'a, Msg>>) -> View<'a, Msg> {
    VStack::new()
        .push(Text::new(title).style(TextStyle::Title).padding_insets(Insets { bottom: 2, ..Default::default() }))
        .extend(sections)
        .spacing(18)
        .alignment(Align::Start)
        .padding_insets(Insets { top: 26, leading: 34, bottom: 30, trailing: 34 })
        .fill_width()
}

fn titled<'a>(title: &str, rows: Vec<View<'a, Msg>>) -> View<'a, Msg> {
    Section::titled(title, Section::new().extend(rows))
}

fn card<'a>(rows: Vec<View<'a, Msg>>) -> View<'a, Msg> {
    Section::new().extend(rows).fill_width()
}

fn footer<'a>(text: &str) -> View<'a, Msg> {
    Text::paragraph(text).style(TextStyle::Callout).foreground(Color::SecondaryLabel).padding_xy(4, 0)
}

/// A switch bound to a yes/no preference.
fn pref_toggle<'a>(id: &'static str, label: &str, key: &'static str) -> View<'a, Msg> {
    Toggle::new(label, prefs::get_bool(key)).on_change(move |on| Msg::Toggle(key, on)).id(id).min_height(size::ROW)
}

/// A labelled slider over an integer preference.
#[allow(clippy::too_many_arguments)]
fn pref_slider<'a>(
    id: &'static str,
    label: &str,
    key: &'static str,
    value: i64,
    min: i64,
    max: i64,
    step: i64,
    ends: Option<(&str, &str)>,
) -> View<'a, Msg> {
    let slider = Slider::new(value as f32, min as f32..=max as f32)
        .step(step as f32)
        .on_change(move |v: f32| Msg::Int(key, v as i64))
        .id(id)
        .grow(1);
    let mut controls = HStack::new().spacing(10);
    match ends {
        Some((lo, hi)) => {
            controls = controls
                .push(Text::new(lo).style(TextStyle::Footnote).foreground(Color::SecondaryLabel))
                .push(slider)
                .push(Text::new(hi).style(TextStyle::Footnote).foreground(Color::SecondaryLabel))
        }
        None => controls = controls.push(slider),
    }
    HStack::new().push(Text::new(label).width(170)).push(controls.grow(1)).spacing(12).min_height(44).fill_width()
}

/// The small coloured tile of a pane, with its glyph.
fn pane_tile<'a>(pane: Pane, color: u32) -> View<'a, Msg> {
    Canvas::new(move |cv, r| {
        cv.fill_round_rect(r, 7, color);
        pane_glyph(cv, pane, r, color);
    })
    .size(22, 22)
}

impl Settings {
    fn sidebar(&self) -> View<'_, Msg> {
        let items = PANES.iter().map(|&(pane, label, _, color)| {
            HStack::new()
                .push(pane_tile(pane, color))
                .push(Text::new(label).style(TextStyle::Sized(14, waveui::style::Weight::Bold)).grow(1))
                .spacing(10)
                .padding_xy(10, 0)
                .height(size::SIDEBAR_ROW)
                .fill_width()
                .corner_radius(radius::ITEM)
                .selected(self.query.trim().is_empty() && pane == self.pane)
                .on_tap(Msg::Pane(pane))
        });
        // The system itself heads the list: its mark, name and version.
        let info = corekit::process::sys_info();
        let version = corekit::process::fixed_str(&info.version, info.version_len);
        let header = HStack::new()
            .push(Image::app_icon(Icon::Aurora, 40))
            .push(
                VStack::new()
                    .push(Text::new("WaveOS Aurora").style(TextStyle::Sized(14, waveui::style::Weight::Bold)))
                    .push(
                        Text::new(format!("Version {version}"))
                            .style(TextStyle::Caption)
                            .foreground(Color::TertiaryLabel),
                    )
                    .spacing(1)
                    .alignment(Align::Start),
            )
            .spacing(10)
            .padding_insets(Insets { top: 8, leading: 8, bottom: 2, trailing: 8 });
        VStack::new()
            .push(header)
            .push(
                TextField::search("Seek in Settings", self.query.clone())
                    .on_change(Msg::Search)
                    .on_submit(Msg::SearchOpen)
                    .shortcut(Shortcut::ctrl('f'))
                    .id("search"),
            )
            .push(ScrollView::new(VStack::with(items.collect()).spacing(1).fill_width()))
            .spacing(10)
            .alignment(Align::Stretch)
            .padding_insets(Insets { top: 14, leading: 10, bottom: 10, trailing: 10 })
            .fill()
    }

    fn results(&self) -> View<'_, Msg> {
        let matches = search::matches(&self.query);
        let title = match matches.len() {
            1 => String::from("1 matching setting"),
            n => format!("{n} matching settings"),
        };
        let mut v = VStack::new().push(Text::new(title).style(TextStyle::Title2)).spacing(6).alignment(Align::Stretch);
        if matches.is_empty() {
            v = v.push(footer("Try a setting name, such as motion, volume, or keyboard."));
        }
        for (k, &i) in matches.iter().enumerate() {
            let s = search::SETTINGS[i];
            let pane = PANES.iter().find(|p| p.0 == s.pane).map(|p| p.1).unwrap_or("");
            v = v.push(
                VStack::new()
                    .push(Text::new(s.label).style(TextStyle::Headline))
                    .push(Text::new(pane).style(TextStyle::Footnote).foreground(Color::SecondaryLabel))
                    .spacing(2)
                    .alignment(Align::Start)
                    .padding_xy(12, 8)
                    .fill_width()
                    .selected(k == self.selected)
                    .on_tap(Msg::OpenSetting(i)),
            );
        }
        ScrollView::new(v.padding(24).fill_width())
    }

    fn appearance(&self, env: &Env) -> View<'_, Msg> {
        let mode = |dark: bool| {
            let selected = env.dark == dark;
            VStack::new()
                .push(
                    Canvas::new(move |cv, r| {
                        mode_preview(cv, r.inset(3), dark);
                        ring(cv, r.inset(3), selected);
                    })
                    .size(THUMB_W + 6, THUMB_H + 6),
                )
                .push(Text::new(if dark { "Dark" } else { "Light" }))
                .spacing(6)
                .on_tap(Msg::SetDark(dark))
                .corner_radius(12)
                .id(if dark { "appearance/dark" } else { "appearance/light" })
        };
        let accent = theme::accent_index() as usize;
        let accents = HStack::with(
            theme::ACCENTS
                .iter()
                .enumerate()
                .map(|(i, (name, color))| {
                    let (color, on) = (*color, i == accent);
                    Canvas::new(move |cv, r| {
                        let (cx, cy) = r.center();
                        if on {
                            cv.fill_circle(cx, cy, 14, with_alpha(color, 0x50));
                        }
                        cv.fill_circle(cx, cy, 11, color);
                        if on {
                            cv.fill_circle(cx, cy, 4, 0xFFFF_FFFF);
                        }
                    })
                    .size(30, 30)
                    .on_tap(Msg::Accent(i))
                    .corner_radius(15)
                    .help(*name)
                })
                .collect(),
        )
        .spacing(8)
        .push(
            Text::new(theme::ACCENTS.get(accent).map(|a| a.0).unwrap_or(""))
                .foreground(Color::SecondaryLabel)
                .padding_xy(6, 0),
        )
        .id("appearance/accent");
        let current = theme::wallpaper();
        let mut walls = Grid::new(THUMB_W + 6).spacing(14);
        for i in 0..wallpaper::NAMES.len() as u8 {
            let img = self.thumbs[i as usize][env.dark as usize].clone();
            let on = current == i;
            walls = walls.push(
                VStack::new()
                    .push(
                        Canvas::new(move |cv, r| {
                            let r = Rect::new(r.x + 3, r.y + 3, THUMB_W, THUMB_H);
                            cv.fill_round_rect_with(r, 10, |px, py| img[((py - r.y) * THUMB_W + (px - r.x)) as usize]);
                            ring(cv, r, on);
                        })
                        .size(THUMB_W + 6, THUMB_H + 6),
                    )
                    .push(Text::new(wallpaper::NAMES[i as usize]))
                    .spacing(6)
                    .on_tap(Msg::Wallpaper(i))
                    .corner_radius(12),
            );
        }
        if let Some(img) = &self.custom_thumb {
            let img = img.clone();
            let on = current == wallpaper::CUSTOM;
            walls = walls.push(
                VStack::new()
                    .push(
                        Canvas::new(move |cv, r| {
                            let r = Rect::new(r.x + 3, r.y + 3, THUMB_W, THUMB_H);
                            cv.with_clip(r, |cv| {
                                let s = (r.w as u32 * img.height / img.width.max(1)) as i32;
                                let fill = if s >= r.h {
                                    Rect::new(r.x, r.y + (r.h - s) / 2, r.w, s)
                                } else {
                                    let w = (r.h as u32 * img.width / img.height.max(1)) as i32;
                                    Rect::new(r.x + (r.w - w) / 2, r.y, w, r.h)
                                };
                                cv.draw_image(&img.pixels, img.width, img.height, fill, 255);
                            });
                            ring(cv, r, on);
                        })
                        .size(THUMB_W + 6, THUMB_H + 6),
                    )
                    .push(Text::new("Your Picture"))
                    .spacing(6)
                    .on_tap(Msg::CustomWallpaper)
                    .corner_radius(12),
            );
        }
        page(
            "Appearance",
            vec![
                Section::titled("Mode", HStack::new().push(mode(false)).push(mode(true)).spacing(20)),
                Section::titled("Accent colour", accents),
                Section::titled("Wallpaper", walls.id("appearance/wallpaper").fill_width()),
                footer("Tip: choose Set as Wallpaper in Lens or Files to use any picture."),
            ],
        )
    }

    fn display_pane(&self) -> View<'_, Msg> {
        let live = self.modes.iter().any(|m| m.flags & display::LIVE != 0);
        let follows = self.modes.iter().any(|m| m.flags & display::FOLLOWS_WINDOW != 0);
        let sub = if follows {
            "Changes apply right away. In a virtual machine, WaveOS also follows the window: make it larger or full screen for more room."
        } else if live {
            "Changes apply right away; you'll be asked to keep them."
        } else {
            "This display is set up by the firmware: changes apply when you restart."
        };
        let current = self.modes.iter().find(|m| m.flags & display::CURRENT != 0);
        let summary = current.map(|m| {
            card(vec![row_detail(
                "Showing",
                format!("{} × {} pixels", m.width, m.height),
                Text::new(aspect(m.width, m.height)).foreground(Color::SecondaryLabel),
            )])
        });
        // Largest first, as the screen's own list is.
        let rows = self.modes.iter().enumerate().rev().map(|(i, m)| {
            let mut tags = Vec::new();
            let current = m.flags & display::CURRENT != 0;
            if m.flags & display::PREFERRED != 0 {
                tags.push("Recommended");
            }
            if current {
                tags.push("In use");
            }
            if m.flags & display::AT_BOOT != 0 && !current {
                tags.push("After restart");
            }
            HStack::new()
                .push(Text::new(format!("{} × {}", m.width, m.height)).grow(1))
                .push(Text::new(tags.join(" · ")).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                .push(radio(current))
                .spacing(12)
                .min_height(38)
                .fill_width()
                .on_tap(Msg::Mode(i))
        });
        let mut sections = vec![footer(sub)];
        sections.extend(summary);
        let scale = prefs::get_int(pref::DISPLAY_SCALE, 0).clamp(0, 2) as usize;
        sections.push(Section::titled(
            "Scale",
            card(vec![row(
                "Draw text and controls at",
                Picker::new(["Automatic", "1×", "2×"], scale)
                    .on_change(Msg::Scale)
                    .picker_style(PickerStyle::Segmented),
            )])
            .id("display/scale"),
        ));
        sections.push(footer(
            "Automatic uses 2× on screens of 2560 × 1440 and up, so text stays the same size and becomes sharper.",
        ));
        sections.push(Section::titled("Resolution", card(rows.collect()).id("display/resolution")));
        page("Display", sections)
    }

    fn keyboard(&self) -> View<'_, Msg> {
        let current = prefs::get(pref::KEYBOARD).unwrap_or_default();
        let layouts = LAYOUTS.iter().map(|&(id, name)| {
            row(
                name,
                if current == id { Image::symbol(Symbol::Check).foreground(Color::Accent) } else { View::empty() },
            )
            .on_tap(Msg::Layout(id))
        });
        let delay = prefs::get_int(pref::REPEAT_DELAY, 1).clamp(0, 3);
        let rate = prefs::get_int(pref::REPEAT_RATE, 8).clamp(0, 31);
        page(
            "Keyboard",
            vec![
                Section::titled("Layout", card(layouts.collect()).id("keyboard/layout")),
                titled(
                    "Key repeat",
                    vec![
                        pref_slider(
                            "keyboard/repeat-delay",
                            "Delay until repeat",
                            pref::REPEAT_DELAY,
                            3 - delay,
                            0,
                            3,
                            1,
                            Some(("Long", "Short")),
                        ),
                        pref_slider(
                            "keyboard/repeat-rate",
                            "Repeat speed",
                            pref::REPEAT_RATE,
                            31 - rate,
                            0,
                            31,
                            1,
                            Some(("Slow", "Fast")),
                        ),
                    ],
                ),
                TextField::new("Type here to try your layout", self.try_text.clone())
                    .on_change(Msg::Try)
                    .id("keyboard/test"),
            ],
        )
    }

    fn trackpad(&self) -> View<'_, Msg> {
        let speed = prefs::get_int(pref::TRACKING_SPEED, 5).clamp(1, 10);
        page(
            "Trackpad",
            vec![
                card(vec![
                    pref_toggle("trackpad/tap-to-click", "Tap to click", pref::TAP_TO_CLICK),
                    pref_toggle("trackpad/natural-scrolling", "Natural scrolling", pref::NATURAL_SCROLLING),
                    pref_slider(
                        "trackpad/tracking-speed",
                        "Tracking speed",
                        pref::TRACKING_SPEED,
                        speed,
                        1,
                        10,
                        1,
                        Some(("Slow", "Fast")),
                    ),
                ]),
                footer(
                    "Tap with one finger to click and two to secondary-click; tap, then drag to move \
                     things. Scroll with two fingers: with natural scrolling the content follows your \
                     fingers, as on a phone.",
                ),
            ],
        )
    }

    fn mouse(&self) -> View<'_, Msg> {
        let speed = prefs::get_int(pref::POINTER_SPEED, 5).clamp(1, 10);
        page(
            "Mouse",
            vec![
                card(vec![pref_slider(
                    "mouse/pointer-speed",
                    "Pointer speed",
                    pref::POINTER_SPEED,
                    speed,
                    1,
                    10,
                    1,
                    Some(("Slow", "Fast")),
                )]),
                footer(
                    "The side buttons of five-button mice go back and forward in NetSurf and Files. \
                     Scroll wheels scroll smoothly a notch at a time.",
                ),
            ],
        )
    }

    fn controllers(&self) -> View<'_, Msg> {
        let mut sections = Vec::new();
        if self.pads.is_empty() {
            sections.push(
                card(vec![VStack::new()
                    .push(Image::symbol(Symbol::Gamepad).symbol_size(36).foreground(Color::SecondaryLabel))
                    .push(Text::new("No game controllers").style(TextStyle::Headline))
                    .push(
                        Text::centered("Connect an Xbox, PlayStation or Switch Pro controller, or any USB gamepad.")
                            .style(TextStyle::Callout)
                            .foreground(Color::SecondaryLabel),
                    )
                    .spacing(8)
                    .padding(28)
                    .fill_width()])
                .id("controllers/list"),
            );
        }
        for (k, (pad, state)) in self.pads.iter().enumerate() {
            let family = match pad.family {
                corekit::gamepad::Family::Xbox => "Xbox controller",
                corekit::gamepad::Family::PlayStation => "PlayStation controller",
                corekit::gamepad::Family::Nintendo => "Nintendo controller",
                corekit::gamepad::Family::Generic => "Game controller",
            };
            let state = *state;
            let fam = pad.family;
            let mut rows = vec![
                row_detail(
                    pad.name.clone(),
                    format!("{family} · {:04x}:{:04x}", pad.vendor, pad.product),
                    if pad.has_rumble {
                        Button::new("Test Vibration").action(Msg::Rumble(pad.id))
                    } else {
                        View::empty()
                    },
                ),
                Canvas::new(move |cv, r| pad_picture(cv, r, state, fam)).height(150).fill_width(),
            ];
            if state.is_none() {
                rows.push(footer("Waiting for the controller to report…"));
            }
            let s = card(rows);
            sections.push(if k == 0 { s.id("controllers/list") } else { s });
        }
        sections.push(footer(
            "Press buttons and move the sticks to try them. Games read controllers with the \
             WaveSDK's gamepad module.",
        ));
        page("Game Controllers", sections)
    }

    fn network(&self) -> View<'_, Msg> {
        use corekit::abi::wifi as w;
        let mut sections = Vec::new();
        let text = |b: &[u8], n: u32| String::from_utf8_lossy(&b[..(n as usize).min(b.len())]).into_owned();
        if let Some(st) = self.wifi {
            let (ssid, message, driver) =
                (text(&st.ssid, st.ssid_len), text(&st.message, st.message_len), text(&st.driver, st.driver_len));
            let (status, color) = match st.state {
                w::CONNECTED => (format!("Connected to {ssid}"), Color::Green),
                w::CONNECTING => (format!("{ssid} · {message}"), Color::Orange),
                w::FAILED => (message, Color::Red),
                w::UNAVAILABLE => (message, Color::Gray),
                _ => (String::from("Not connected"), Color::Gray),
            };
            sections.push(
                card(vec![HStack::new()
                    .push(status_dot(color))
                    .push(
                        VStack::new()
                            .push(Text::new("Wi-Fi").style(TextStyle::Headline))
                            .push(Text::new(status).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                            .spacing(2)
                            .alignment(Align::Start)
                            .grow(1),
                    )
                    .push(if st.state == w::CONNECTED {
                        Button::new("Disconnect").action(Msg::WifiDisconnect)
                    } else {
                        Text::new(driver).style(TextStyle::Callout).foreground(Color::SecondaryLabel)
                    })
                    .spacing(12)
                    .min_height(56)
                    .fill_width()])
                .id("network/wifi"),
            );
            if st.state != w::UNAVAILABLE {
                let mut rows = Vec::new();
                if self.wifi_nets.is_empty() {
                    rows.push(row("Looking for networks…", ProgressView::spinner()));
                }
                for (i, n) in self.wifi_nets.iter().take(8).enumerate() {
                    let raw = &n.ssid[..n.ssid_len as usize];
                    let name = corekit::net::wifi_name(raw);
                    let joined = st.state == w::CONNECTED && name == ssid;
                    let label = match n.security {
                        w::OPEN => "Open",
                        w::WPA2 => "WPA2",
                        w::WPA2_WPA3 => "WPA2/WPA3",
                        _ => "Not supported",
                    };
                    let unsupported = n.security == w::UNSUPPORTED;
                    rows.push(
                        HStack::new()
                            .push(Image::symbol(Symbol::Check).foreground(Color::Accent).opacity(if joined {
                                255
                            } else {
                                0
                            }))
                            .push(Image::symbol(Symbol::Wifi).foreground(Color::SecondaryLabel))
                            .push(
                                Text::new(name)
                                    .foreground(if unsupported { Color::SecondaryLabel } else { Color::Label })
                                    .grow(1),
                            )
                            .push(Text::new(label).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                            .spacing(10)
                            .min_height(40)
                            .fill_width()
                            .on_tap(Msg::WifiChoose(i)),
                    );
                    if self.wifi_chosen.as_deref() == Some(raw) {
                        rows.push(
                            HStack::new()
                                .push(
                                    TextField::secure("Password", self.wifi_password.clone())
                                        .on_change(Msg::WifiPassword)
                                        .on_submit(Msg::WifiJoin)
                                        .focused()
                                        .grow(1),
                                )
                                .push(Button::new("Cancel").action(Msg::WifiCancel))
                                .push(Button::new("Join").action(Msg::WifiJoin).button_style(ButtonStyle::Prominent))
                                .spacing(8)
                                .padding_xy(0, 8)
                                .fill_width(),
                        );
                    }
                }
                sections.push(titled("Networks", rows));
            }
        }
        let ifaces: Vec<_> =
            corekit::net::interfaces().into_iter().filter(|i| i.flags & corekit::abi::net::IF_LOOPBACK == 0).collect();
        if ifaces.is_empty() && self.wifi.is_none() {
            sections.push(footer("No network adapter was found."));
        }
        for i in &ifaces {
            let ip = |a: [u8; 4]| if a == [0; 4] { String::from("—") } else { corekit::net::ip_string(a) };
            let wireless = corekit::net::name_of(i).starts_with("wlan");
            let (status, color) = match (i.flags & corekit::abi::net::IF_LINK != 0, i.ip != [0; 4]) {
                (true, true) => ("Connected", Color::Green),
                (true, false) => ("Getting an address…", Color::Orange),
                (false, _) if wireless => ("Not connected", Color::Gray),
                (false, _) => ("Cable unplugged", Color::Red),
            };
            let kind = if wireless { "Wi-Fi" } else { "Ethernet" };
            let mac = i.mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
            let dns: Vec<String> =
                i.dns.iter().filter(|d| **d != [0; 4]).map(|d| corekit::net::ip_string(*d)).collect();
            let lease = if i.lease_secs == 0 {
                String::from("—")
            } else {
                format!("{} h {} min left", i.lease_secs / 3600, i.lease_secs / 60 % 60)
            };
            let details = [
                ("IP address", ip(i.ip)),
                ("Subnet mask", ip(i.netmask)),
                ("Router", ip(i.gateway)),
                ("DNS servers", if dns.is_empty() { String::from("—") } else { dns.join(", ") }),
                ("Hardware address", mac),
                ("DHCP lease", lease),
                ("Received", format!("{} ({} packets)", human(i.rx_bytes), i.rx_packets)),
                ("Sent", format!("{} ({} packets)", human(i.tx_bytes), i.tx_packets)),
            ];
            let mut rows = vec![HStack::new()
                .push(status_dot(color))
                .push(
                    VStack::new()
                        .push(Text::new(format!("{kind} ({})", corekit::net::name_of(i))).style(TextStyle::Headline))
                        .push(Text::new(status).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                        .spacing(2)
                        .alignment(Align::Start)
                        .grow(1),
                )
                .push(Text::new(corekit::net::driver_of(i)).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                .spacing(12)
                .min_height(56)
                .fill_width()];
            rows.extend(details.into_iter().map(|(k, v)| {
                row(k, Text::new(v).foreground(Color::SecondaryLabel)).height(34).foreground(Color::Label)
            }));
            sections.push(card(rows));
        }
        sections.push(footer(
            "Addresses come from the network's DHCP server. Test the connection with `ping` or `fetch` \
             in Terminal, or browse with NetSurf.",
        ));
        page("Network", sections)
    }

    fn date_time(&self) -> View<'_, Msg> {
        let d = self.now;
        let time = if prefs::get_bool(pref::CLOCK_24H) {
            format!("{:02}:{:02}:{:02}", d.hour, d.minute, d.second)
        } else {
            let h = match d.hour % 12 {
                0 => 12,
                h => h,
            };
            format!("{}:{:02}:{:02} {}", h, d.minute, d.second, if d.hour < 12 { "AM" } else { "PM" })
        };
        let c = self.clock;
        let fields = [
            ("Year", 2000, 2099),
            ("Month", 1, 12),
            ("Day", 1, days_in(c[0], c[1])),
            ("Hour", 0, 23),
            ("Minute", 0, 59),
        ];
        let mut set_rows: Vec<View<'_, Msg>> = fields
            .iter()
            .enumerate()
            .map(|(i, &(label, lo, hi))| {
                Stepper::new(label, c[i], lo..=hi).on_change(move |v: i64| Msg::Clock(i, v)).min_height(size::ROW)
            })
            .collect();
        set_rows.push(
            HStack::new()
                .push(Spacer::new())
                .push(
                    Button::new("Set")
                        .action(Msg::SetClock)
                        .button_style(if self.clock_edited { ButtonStyle::Prominent } else { ButtonStyle::Bordered })
                        .min_width(80),
                )
                .min_height(48)
                .fill_width(),
        );
        let off = prefs::get_int("utc_offset", 0);
        let zone = if off == 0 {
            String::from("UTC")
        } else {
            format!("UTC{}{}:{:02}", if off < 0 { "−" } else { "+" }, off.abs() / 60, off.abs() % 60)
        };
        page(
            "Date & Time",
            vec![
                VStack::new()
                    .push(Text::new(time).style(TextStyle::Sized(36, Weight::Bold)))
                    .push(Text::new(corekit::time::format_date(&d)).foreground(Color::SecondaryLabel))
                    .spacing(2)
                    .alignment(Align::Start),
                card(vec![
                    pref_toggle("datetime/24-hour", "24-hour time", pref::CLOCK_24H),
                    pref_toggle("datetime/seconds", "Show seconds in the bar", pref::CLOCK_SECONDS),
                    pref_toggle("datetime/date", "Show the date in the bar", pref::CLOCK_DATE),
                ]),
                Section::titled("Set date and time", card(set_rows).id("datetime/set-clock")),
                card(vec![row(
                    "Time zone",
                    HStack::new()
                        .push(Text::new(zone).style(TextStyle::Headline))
                        .push(Button::symbol(Symbol::Minus).action(Msg::Zone(off - 30)).help("30 minutes west"))
                        .push(Button::symbol(Symbol::Plus).action(Msg::Zone(off + 30)).help("30 minutes east"))
                        .spacing(4),
                )
                .id("datetime/time-zone")]),
            ],
        )
    }

    fn sound(&self) -> View<'_, Msg> {
        let vol = prefs::get_int(pref::VOLUME, 70).clamp(0, 100);
        let out = prefs::get("audio_device").unwrap_or_else(|| String::from("No output device found"));
        page(
            "Sound",
            vec![card(vec![
                pref_slider("sound/volume", "Volume", pref::VOLUME, vol, 0, 100, 5, None),
                pref_toggle("sound/mute", "Mute", pref::MUTED),
                row("Output", Text::new(out).foreground(Color::SecondaryLabel)),
            ])],
        )
    }

    fn notifications(&self) -> View<'_, Msg> {
        page(
            "Notifications",
            vec![
                card(vec![pref_toggle("notifications/do-not-disturb", "Calm", pref::DND)]),
                footer(
                    "While Calm is on, notifications don't rise from Crest. They still drift into Drift: \
                     click Crest at the top of the screen to see them.",
                ),
            ],
        )
    }

    fn desktop(&self) -> View<'_, Msg> {
        let dock = prefs::get_int(pref::DOCK_SIZE, 48).clamp(32, 64);
        let glass = prefs::get_int(pref::GLASS, 70).clamp(0, 100);
        page(
            "Desktop & Harbor",
            vec![
                titled(
                    "Harbor",
                    vec![
                        pref_slider(
                            "desktop/dock-size",
                            "Size",
                            pref::DOCK_SIZE,
                            dock,
                            32,
                            64,
                            2,
                            Some(("Small", "Large")),
                        ),
                        pref_toggle("desktop/dock-autohide", "Hide Harbor until pointed at", pref::DOCK_AUTOHIDE),
                    ],
                ),
                titled(
                    "Liquid glass",
                    vec![pref_slider(
                        "desktop/glass",
                        "Intensity",
                        pref::GLASS,
                        glass,
                        0,
                        100,
                        5,
                        Some(("Clear", "Frosted")),
                    )],
                ),
                footer("Changes apply across the desktop immediately."),
            ],
        )
    }

    fn bar(&self) -> View<'_, Msg> {
        let crest = prefs::get_bool(pref::CREST);
        let battery = prefs::get_bool(pref::BATTERY_LEVEL);
        page(
            "Bar & Crest",
            vec![
                Canvas::new(move |cv, r| bar_preview(cv, r, crest, battery)).height(120).fill_width(),
                card(vec![
                    pref_toggle("bar/crest", "Show Crest", pref::CREST),
                    pref_toggle("bar/battery", "Show battery level", pref::BATTERY_LEVEL),
                ]),
                footer(
                    "Crest sits at the top centre and shows the time; point at it for the date, click it for \
                     Drift. Without Crest, the time and date move into the right bubble. The battery level \
                     appears next to the status ring on computers with a battery.",
                ),
            ],
        )
    }

    fn login(&self) -> View<'_, Msg> {
        let name = prefs::get("account_name").unwrap_or_else(|| String::from("This computer"));
        let initials: String =
            name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase();
        let avatar = Canvas::new(move |cv, r| {
            cv.fill_round_rect_dgradient(r, r.w / 2, theme::accent(), theme::ACCENT_2);
            cv.text_centered(r, &initials, theme::ui_bold(18), 0xFFFF_FFFF);
        })
        .size(52, 52);
        page(
            "Login",
            vec![
                card(vec![HStack::new()
                    .push(avatar)
                    .push(
                        VStack::new()
                            .push(Text::new(name).style(TextStyle::Title2))
                            .push(Text::new("Owner · password protected").foreground(Color::SecondaryLabel))
                            .spacing(2)
                            .alignment(Align::Start),
                    )
                    .push(Spacer::new())
                    .spacing(16)
                    .padding_xy(0, 14)
                    .fill_width()]),
                card(vec![pref_toggle("login/auto", "Log in automatically", pref::AUTO_LOGIN)]),
                footer(
                    "With automatic login, WaveOS opens straight to the desktop when it starts. Lock Screen \
                     (Super L) and waking from sleep still ask for your password.",
                ),
            ],
        )
    }

    fn accessibility(&self) -> View<'_, Msg> {
        let item = |id: &'static str, label: &str, detail: &str, key: &'static str| {
            row_detail(label, detail, Toggle::new("", prefs::get_bool(key)).on_change(move |on| Msg::Toggle(key, on)))
                .id(id)
        };
        page(
            "Accessibility",
            vec![
                titled(
                    "Display",
                    vec![
                        item(
                            "accessibility/transparency",
                            "Reduce transparency",
                            "Use solid surfaces for easier reading.",
                            pref::REDUCE_TRANSPARENCY,
                        ),
                        item(
                            "accessibility/contrast",
                            "Increase contrast",
                            "Stronger text, borders and control outlines.",
                            pref::HIGH_CONTRAST,
                        ),
                    ],
                ),
                titled(
                    "Motion",
                    vec![item(
                        "accessibility/motion",
                        "Reduce motion",
                        "Arrange windows and open sheets without animated transitions.",
                        pref::REDUCE_MOTION,
                    )],
                ),
            ],
        )
    }

    fn about(&self, env: &Env) -> View<'_, Msg> {
        let i = &self.info;
        let version = format!("Version {}", corekit::process::fixed_str(&i.version, i.version_len));
        let hero = Canvas::new(move |cv, r| {
            cv.fill_round_rect_dgradient(r, 20, mix(theme::accent(), 0xFF06_0A1A, 150), theme::accent());
            let icon = Rect::new(r.x + 30, r.y + 26, 96, 96);
            cv.glow(icon, 24, 24, 0xFFFF_FFFF, 40);
            icons::draw(cv, Icon::Aurora, icon);
            cv.text(r.x + 148, r.y + 66, "WaveOS Aurora", theme::display(28), 0xFFFF_FFFF);
            cv.text(r.x + 150, r.y + 92, &version, theme::ui(14), 0xE6FF_FFFF);
            cv.text(r.x + 150, r.y + 116, "Made by WayV", theme::ui_bold(13), 0xCCFF_FFFF);
        })
        .height(148)
        .fill_width();
        let rows = [
            ("Processor", corekit::process::fixed_str(&i.cpu, i.cpu_len)),
            ("Memory", format!("{} MiB", i.mem_total >> 20)),
            ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
            ("Startup disk", corekit::process::fixed_str(&i.root, i.root_len)),
            ("Available", format!("{} MB of {} MB", i.disk_free >> 20, i.disk_total >> 20)),
        ];
        page(
            "About",
            vec![
                hero,
                card(
                    rows.into_iter()
                        .map(|(k, v)| row(k, Text::new(v).foreground(Color::SecondaryLabel)).height(36))
                        .collect(),
                )
                .id("about/info"),
                HStack::new()
                    .push(Spacer::new())
                    .push(Button::new("More Info…").action(Msg::OpenAbout))
                    .push(Spacer::new())
                    .fill_width(),
            ],
        )
    }
}

fn status_dot<'a>(c: Color) -> View<'a, Msg> {
    Canvas::new(move |cv, r| {
        let (x, y) = r.center();
        cv.fill_circle(x, y, 6, c.argb());
    })
    .size(16, 16)
}

/// "16:10", "16:9", … for a resolution.
fn aspect(w: u32, h: u32) -> String {
    fn gcd(a: u32, b: u32) -> u32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let g = gcd(w, h).max(1);
    match (w / g, h / g) {
        (8, 5) => "16:10".into(),
        (683, 384) | (85, 48) => "16:9".into(),
        (a, b) => format!("{a}:{b}"),
    }
}

fn radio<'a>(on: bool) -> View<'a, Msg> {
    Canvas::new(move |cv, r| {
        let t = theme::current();
        let (cx, cy) = r.center();
        if on {
            cv.fill_circle(cx, cy, 8, theme::accent());
            cv.fill_circle(cx, cy, 3, 0xFFFF_FFFF);
        } else {
            cv.fill_circle(cx, cy, 8, t.control_border);
            cv.fill_circle(cx, cy, 7, t.control_bg);
        }
    })
    .size(18, 18)
}

fn ring(cv: &mut Surface, r: Rect, selected: bool) {
    if selected {
        cv.stroke_round_rect(r.inset(-3), 13, theme::accent());
        cv.stroke_round_rect(r.inset(-2), 12, theme::accent());
    }
}

impl App for Settings {
    type Message = Msg;

    fn title(&self) -> String {
        "Settings".into()
    }

    fn size(&self) -> (i32, i32) {
        (1140, 728)
    }

    fn started(&mut self, cx: &mut Context<Msg>) {
        if let Some(id) = self.target.take() {
            cx.focus_ring(id);
        }
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Pane(p) => {
                self.pane = p;
                self.query.clear();
                self.clock_edited = false;
                self.refresh();
            }
            Msg::Search(q) => {
                self.query = q;
                self.selected = 0;
            }
            Msg::SearchMove(d) => {
                let n = search::matches(&self.query).len();
                if n > 0 {
                    self.selected = (self.selected as i32 + d).clamp(0, n as i32 - 1) as usize;
                }
            }
            Msg::SearchOpen => {
                if let Some(&i) = search::matches(&self.query).get(self.selected) {
                    cx.send(Msg::OpenSetting(i));
                }
            }
            Msg::OpenSetting(i) => {
                self.jump_setting(i);
                self.refresh();
                if let Some(id) = self.target.take() {
                    cx.focus_ring(id);
                }
            }
            Msg::SetDark(d) => cx.request(Request::SetDark(d)),
            Msg::Accent(i) => self.set(cx, pref::ACCENT, &format!("{i}")),
            Msg::Wallpaper(i) => cx.request(Request::SetWallpaper(i)),
            Msg::CustomWallpaper => {
                if let Some(p) = prefs::get("wallpaper_image") {
                    cx.request(Request::SetWallpaperImage(p));
                }
            }
            Msg::Scale(i) => {
                if let Err(e) = prefs::set(pref::DISPLAY_SCALE, &format!("{i}")) {
                    self.flash(cx, format!("Couldn't change the scale: {e}"));
                }
            }
            Msg::Mode(i) => {
                if let Some(m) = self.modes.get(i).copied() {
                    match prefs::set_display(m.width, m.height) {
                        Ok(true) => {}
                        Ok(false) => {
                            self.flash(cx, format!("{} × {} will be used when you restart", m.width, m.height))
                        }
                        Err(e) => self.flash(cx, format!("Couldn't change the resolution: {e}")),
                    }
                    self.modes = prefs::display_modes();
                }
            }
            Msg::Layout(id) => self.set(cx, pref::KEYBOARD, id),
            Msg::Try(t) => self.try_text = t,
            Msg::Toggle(key, on) => self.set(cx, key, if on { "1" } else { "0" }),
            Msg::Int(key, v) => {
                // Repeat delay and rate run the other way round on screen.
                let v = match key {
                    pref::REPEAT_DELAY => 3 - v,
                    pref::REPEAT_RATE => 31 - v,
                    _ => v,
                };
                self.set(cx, key, &format!("{v}"));
            }
            Msg::Clock(i, v) => {
                self.clock[i] = v;
                self.clock[2] = self.clock[2].min(days_in(self.clock[0], self.clock[1]));
                self.clock_edited = true;
            }
            Msg::SetClock => {
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
                        self.flash(cx, String::from("The date and time were set"));
                    }
                    Err(e) => self.flash(cx, format!("Couldn't set the clock: {e}")),
                }
            }
            Msg::Zone(v) => self.set(cx, "utc_offset", &format!("{}", v.clamp(-12 * 60, 14 * 60))),
            Msg::WifiChoose(i) => {
                let Some(n) = self.wifi_nets.get(i).copied() else { return };
                let ssid = n.ssid[..n.ssid_len as usize].to_vec();
                match n.security {
                    corekit::abi::wifi::OPEN => self.wifi_join(cx, &ssid, ""),
                    corekit::abi::wifi::UNSUPPORTED => {
                        self.flash(cx, String::from("WaveOS can join open and WPA2 Personal networks"))
                    }
                    _ => {
                        if self.wifi_chosen.as_deref() != Some(&ssid[..]) {
                            self.wifi_password.clear();
                        }
                        self.wifi_chosen = Some(ssid);
                    }
                }
            }
            Msg::WifiPassword(p) => self.wifi_password = p,
            Msg::WifiJoin => {
                let Some(ssid) = self.wifi_chosen.clone() else { return };
                if !(8..=63).contains(&self.wifi_password.len()) {
                    self.flash(cx, String::from("The password must be 8 to 63 characters"));
                    return;
                }
                let password = self.wifi_password.clone();
                self.wifi_join(cx, &ssid, &password);
            }
            Msg::WifiCancel => {
                self.wifi_chosen = None;
                self.wifi_password.clear();
            }
            Msg::WifiDisconnect => {
                corekit::net::wifi_disconnect();
                self.wifi = corekit::net::wifi_status();
            }
            Msg::Rumble(id) => {
                if let Some((pad, _)) = self.pads.iter().find(|(p, _)| p.id == id) {
                    pad.rumble(0.8, 0.5, 400);
                }
            }
            Msg::OpenAbout => cx.open_app("About"),
            Msg::ClearNote => self.note = None,
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        if self.query.trim().is_empty() {
            return None;
        }
        match ev.code {
            KeyCode::Up => Some(Msg::SearchMove(-1)),
            KeyCode::Down => Some(Msg::SearchMove(1)),
            KeyCode::Escape => Some(Msg::Search(String::new())),
            _ => None,
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        match self.pane {
            Pane::Controllers => Some(50),
            Pane::DateTime | Pane::Network => Some(1000),
            Pane::Appearance | Pane::About => Some(2000),
            _ => None,
        }
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        match self.pane {
            Pane::Controllers => {
                let before: Vec<_> = self.pads.iter().map(|(p, s)| (p.id, s.map(|s| s.sequence()))).collect();
                self.refresh();
                let after: Vec<_> = self.pads.iter().map(|(p, s)| (p.id, s.map(|s| s.sequence()))).collect();
                before != after
            }
            // Pick up a new custom wallpaper chosen elsewhere.
            Pane::Appearance => {
                let missing = theme::wallpaper() == wallpaper::CUSTOM && self.custom_thumb.is_none();
                if missing {
                    self.load_custom_thumb();
                }
                missing
            }
            _ => {
                self.refresh();
                true
            }
        }
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let detail = if !self.query.trim().is_empty() {
            self.results()
        } else {
            let content = match self.pane {
                Pane::Appearance => self.appearance(env),
                Pane::Display => self.display_pane(),
                Pane::Keyboard => self.keyboard(),
                Pane::Trackpad => self.trackpad(),
                Pane::Mouse => self.mouse(),
                Pane::Controllers => self.controllers(),
                Pane::Network => self.network(),
                Pane::DateTime => self.date_time(),
                Pane::Sound => self.sound(),
                Pane::Notifications => self.notifications(),
                Pane::Desktop => self.desktop(),
                Pane::Bar => self.bar(),
                Pane::Login => self.login(),
                Pane::Accessibility => self.accessibility(),
                Pane::About => self.about(env),
            };
            ScrollView::new(content).id(("pane", self.pane))
        };
        let note = self.note.as_ref().map(|n| {
            VStack::new()
                .push(Spacer::new())
                .push(
                    Text::new(n.clone())
                        .style(TextStyle::Headline)
                        .foreground(Color::Custom(0xFFFF_FFFF))
                        .padding_xy(16, 8)
                        .background_rounded(Color::Custom(0xE025_2530), 17),
                )
                .padding(20)
                .fill()
        });
        SplitView::new(self.sidebar(), ZStack::with(vec![detail]).push_if(note).fill()).sidebar_width(250)
    }
}

/// A small white glyph on a pane's coloured tile.
fn pane_glyph(cv: &mut Surface, pane: Pane, r: Rect, tile: u32) {
    let (cx, cy) = r.center();
    let w = 0xFFFF_FFFF;
    match pane {
        Pane::Appearance => {
            cv.fill_circle(cx, cy, 6, w);
            cv.with_clip(Rect::new(cx, cy - 7, 8, 14), |cv| cv.fill_circle(cx, cy, 6, tile));
            cv.fill_circle(cx, cy, 4, tile);
            cv.with_clip(Rect::new(cx - 6, cy - 7, 6, 14), |cv| cv.fill_circle(cx, cy, 4, w));
        }
        Pane::Display => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 5, 14, 9), 2, w);
            cv.fill_rect(Rect::new(cx - 3, cy + 5, 6, 2), w);
        }
        Pane::Keyboard => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 4, 14, 9), 2, w);
            for k in 0..3 {
                cv.fill_rect(Rect::new(cx - 5 + k * 4, cy - 2, 2, 2), tile);
            }
            cv.fill_rect(Rect::new(cx - 4, cy + 2, 8, 1), tile);
        }
        Pane::Trackpad => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 6, 14, 12), 3, w);
            cv.fill_rect(Rect::new(cx - 5, cy + 2, 10, 1), tile);
            cv.fill_rect(Rect::new(cx, cy + 2, 1, 3), tile);
        }
        Pane::Mouse => {
            cv.fill_round_rect(Rect::new(cx - 5, cy - 7, 10, 14), 5, w);
            cv.fill_rect(Rect::new(cx, cy - 6, 1, 4), tile);
        }
        Pane::Controllers => {
            cv.fill_round_rect(Rect::new(cx - 8, cy - 4, 16, 9), 4, w);
            cv.fill_rect(Rect::new(cx - 6, cy, 4, 1), tile);
            cv.fill_rect(Rect::new(cx - 4, cy - 2, 1, 5), tile);
            cv.fill_circle(cx + 4, cy, 1, tile);
        }
        Pane::Network => {
            cv.line(cx - 5, cy + 4, cx, cy - 4, 2, w);
            cv.line(cx + 5, cy + 4, cx, cy - 4, 2, w);
            cv.line(cx - 5, cy + 4, cx + 5, cy + 4, 2, w);
            for (x, y) in [(cx, cy - 4), (cx - 5, cy + 4), (cx + 5, cy + 4)] {
                cv.fill_circle(x, y, 2, w);
            }
        }
        Pane::DateTime => {
            cv.fill_circle(cx, cy, 7, w);
            cv.line(cx, cy, cx, cy - 5, 1, tile);
            cv.line(cx, cy, cx + 3, cy, 1, tile);
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
        Pane::Bar => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 6, 14, 5), 2, w);
            cv.fill_round_rect(Rect::new(cx - 3, cy - 5, 6, 3), 1, tile);
            cv.fill_rect(Rect::new(cx - 7, cy + 2, 14, 1), w);
            cv.fill_rect(Rect::new(cx - 7, cy + 5, 9, 1), w);
        }
        Pane::Login => {
            cv.fill_circle(cx, cy - 3, 3, w);
            cv.fill_round_rect(Rect::new(cx - 6, cy + 2, 12, 6), 3, w);
        }
        Pane::Desktop => {
            cv.fill_round_rect(Rect::new(cx - 7, cy - 6, 14, 10), 2, w);
            cv.fill_round_rect(Rect::new(cx - 5, cy + 5, 10, 2), 1, w);
        }
        Pane::Accessibility => {
            cv.fill_circle(cx, cy - 5, 2, w);
            cv.fill_rect(Rect::new(cx - 6, cy - 2, 12, 2), w);
            cv.fill_rect(Rect::new(cx - 1, cy - 2, 2, 5), w);
            cv.line(cx - 1, cy + 3, cx - 3, cy + 7, 2, w);
            cv.line(cx + 1, cy + 3, cx + 3, cy + 7, 2, w);
        }
        Pane::About => {
            cv.fill_circle(cx, cy - 4, 1, w);
            cv.fill_rect(Rect::new(cx - 1, cy - 1, 2, 7), w);
        }
    }
}

/// A controller drawn with what is pressed and where the sticks are.
fn pad_picture(cv: &mut Surface, r: Rect, state: Option<PadState>, family: corekit::gamepad::Family) {
    let t = theme::current();
    let body = Rect::new(r.x + (r.w - 300) / 2, r.y + 10, 300, 130);
    let shell = if t.dark { mix(t.window_bg, 0xFFFF_FFFF, 24) } else { mix(t.window_bg, 0xFF00_0000, 14) };
    cv.fill_round_rect(body, 52, shell);
    let s = state.unwrap_or_default();
    let on = |b: Pad| s.pressed(b);
    let lit = |b: bool| if b { theme::accent() } else { t.control_bg };
    let ink = |b: bool| if b { 0xFFFF_FFFF } else { t.text };
    // Shoulders and triggers.
    let (lt, rt) = s.triggers();
    for (x, sh, tr, v) in [
        (body.x + 34, Pad::LeftShoulder, Pad::LeftTrigger, lt),
        (body.right() - 94, Pad::RightShoulder, Pad::RightTrigger, rt),
    ] {
        let bar = Rect::new(x, r.y, 60, 6);
        cv.fill_round_rect(bar, 3, t.control_bg);
        cv.fill_round_rect(Rect::new(x, r.y, (60.0 * v) as i32, 6), 3, theme::accent());
        if on(tr) && v == 0.0 {
            cv.fill_round_rect(bar, 3, theme::accent());
        }
        cv.fill_round_rect(Rect::new(x, body.y - 2, 60, 8), 4, lit(on(sh)));
    }
    // D-pad.
    let (dx, dy) = (body.x + 70, body.y + 66);
    for (b, rr) in [
        (Pad::Up, Rect::new(dx - 7, dy - 22, 14, 16)),
        (Pad::Down, Rect::new(dx - 7, dy + 6, 14, 16)),
        (Pad::Left, Rect::new(dx - 22, dy - 7, 16, 14)),
        (Pad::Right, Rect::new(dx + 6, dy - 7, 16, 14)),
    ] {
        cv.fill_round_rect(rr, 3, lit(on(b)));
    }
    // Face buttons, labelled for this controller.
    let (fx, fy) = (body.right() - 70, body.y + 66);
    let f = theme::ui_bold(10);
    for (b, x, y) in
        [(Pad::North, fx, fy - 18), (Pad::South, fx, fy + 18), (Pad::West, fx - 18, fy), (Pad::East, fx + 18, fy)]
    {
        cv.fill_circle(x, y, 10, lit(on(b)));
        let label = family.label(b);
        let short: String = label.chars().take(2).collect();
        cv.text_centered(Rect::new(x - 10, y - 10, 20, 20), &short, f, ink(on(b)));
    }
    // Back, guide, start.
    let (cx, cy) = (body.x + body.w / 2, body.y + 40);
    cv.fill_round_rect(Rect::new(cx - 38, cy - 4, 18, 8), 4, lit(on(Pad::Back)));
    cv.fill_round_rect(Rect::new(cx + 20, cy - 4, 18, 8), 4, lit(on(Pad::Start)));
    cv.fill_circle(cx, cy, 8, lit(on(Pad::Guide)));
    // Sticks.
    for ((sx, sy), (x, y), click) in [
        (s.left_stick(), (body.x + 112, body.y + 100), Pad::LeftStick),
        (s.right_stick(), (body.right() - 112, body.y + 100), Pad::RightStick),
    ] {
        cv.fill_circle(x, y, 18, t.control_bg);
        cv.stroke_round_rect(Rect::new(x - 18, y - 18, 36, 36), 18, t.control_border);
        cv.fill_circle(x + (sx * 10.0) as i32, y + (sy * 10.0) as i32, 10, lit(on(click) || sx != 0.0 || sy != 0.0));
    }
}

fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{}.{} GB", n >> 30, ((n % (1 << 30)) * 10) >> 30),
        n if n >= 1 << 20 => format!("{}.{} MB", n >> 20, ((n % (1 << 20)) * 10) >> 20),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} bytes"),
    }
}

fn mode_preview(cv: &mut Surface, r: Rect, dark: bool) {
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

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    if args.get(1).is_some_and(|s| s == "--self-check") {
        for (i, entry) in search::SETTINGS.iter().enumerate() {
            if search::SETTINGS.iter().filter(|s| s.id == entry.id).count() != 1 {
                return 1;
            }
            if !search::matches(entry.label).contains(&i) {
                return 2;
            }
            let launched = Settings::new(Some(entry.id));
            if launched.pane != entry.pane || launched.target != Some(entry.id) {
                return 3;
            }
        }
        let report = format!("SETTINGS CHECK PASS: {} indexed controls and deep links", search::SETTINGS.len());
        corekit::io::log(&report);
        corekit::eprintln!("{}", report);
        return 0;
    }
    waveui::run(Settings::new(args.get(1).map(String::as_str)))
}

/// A small picture of the bar: the left bubble, Crest (or the time on the
/// right) and the status ring (with the battery level).
fn bar_preview(cv: &mut Surface, r: Rect, crest: bool, battery: bool) {
    let t = theme::current();
    cv.fill_round_rect_vgradient(r, 16, mix(theme::accent(), 0xFF06_0A1A, 170), theme::accent());
    let pill = |cv: &mut Surface, p: Rect| {
        cv.fill_round_rect(p, p.h / 2, if t.dark { 0x8010_162E } else { 0xA0FF_FFFF });
    };
    let ink = if t.dark { 0xFFF2_F4FA } else { 0xFF0E_1530 };
    let y = r.y + 14;
    let left = Rect::new(r.x + 14, y, 96, 28);
    pill(cv, left);
    cv.fill_circle(left.x + 15, left.y + 14, 6, theme::accent());
    cv.text(left.x + 28, left.y + 19, "Desktop", theme::ui_bold(12), ink);
    let f = theme::ui_bold(13);
    if crest {
        let c = Rect::new(r.x + r.w / 2 - 40, y, 80, 28);
        pill(cv, c);
        cv.text_centered(c, "10:30", f, ink);
    }
    let rw = if crest { 64 } else { 172 } + if battery { 34 } else { 0 };
    let right = Rect::new(r.right() - 14 - rw, y, rw, 28);
    pill(cv, right);
    let mut x = right.right() - 22;
    cv.stroke_round_rect(Rect::new(x - 8, right.y + 6, 16, 16), 8, ink);
    // Seek.
    let sx = if crest { right.x + 18 } else { x - 28 - if battery { 34 } else { 0 } };
    cv.stroke_round_rect(Rect::new(sx - 6, right.y + 7, 10, 10), 5, ink);
    cv.line(sx + 2, right.y + 15, sx + 6, right.y + 19, 2, ink);
    if battery {
        x -= 34;
        cv.text(x - 4, right.y + 19, "82%", theme::ui_bold(11), ink);
    }
    if !crest {
        let label = "Mon 26 Oct  10:30";
        cv.text(right.x + 12, right.y + 19, label, theme::ui_bold(11), ink);
    }
    let note = if crest { "Crest on: the time sits in the middle" } else { "Crest off: the time moves to the right" };
    cv.text_centered(Rect::new(r.x, r.bottom() - 34, r.w, 20), note, theme::ui_bold(13), 0xFFFF_FFFF);
}

/// The small picture of built-in wallpaper `i` (pre-scaled in the system
/// image), or a drawn stand-in.
fn wallpaper_thumb(i: u8, dark: bool) -> Vec<u32> {
    let path = wallpaper::file(i, dark).map(|p| p.replace("/System/Wallpapers/", "/System/Wallpapers/Thumbnails/"));
    path.and_then(|p| corekit::fs::read(&p).ok())
        .and_then(|d| aurora_image::decode(&d).ok())
        .map(|img| img.thumbnail(THUMB_W as u32, THUMB_H as u32))
        .filter(|img| img.width == THUMB_W as u32 && img.height == THUMB_H as u32)
        .map(|img| img.pixels)
        .unwrap_or_else(|| wallpaper::generate(i, THUMB_W, THUMB_H))
}
