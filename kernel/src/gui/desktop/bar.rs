//! The top of the screen, in three bubbles of Mist glass: the WayV mark and
//! the app on the left, Crest in the middle, Seek and the status ring on the
//! right.
//!
//! Crest shows the time (and the date while it is pointed at). When something
//! happens — a notification, the volume keys, the power adapter — it rises:
//! the glass deepens and its edge glows in the activity's colour. Settings ›
//! Bar & Crest can turn it off; the time, date and activities then join the
//! right bubble. The ring sums up battery and network in one glyph and opens
//! Current, the quick settings panel.

use super::Desktop;
use crate::drivers::{audio, rtc};
use crate::gui::canvas::{mix, with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::notify::Notification;
use crate::gui::theme::{self, MENUBAR_H};
use crate::gui::{apps, prefs};
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::{power, pref, PowerInfo};

/// Bubbles sit 8 points below the top of the screen and are 38 tall.
pub(super) const TOP: i32 = 8;
pub(super) const H: i32 = 38;
const NOTICE_MS: u64 = 4000;
const VOLUME_MS: u64 = 1200;
const POWER_MS: u64 = 3000;
/// Crest changes shape with a spring that overshoots once (Swell).
const RISE_MS: u64 = 260;
/// Volume keys move in sixteenths.
const STEP: u32 = 100 / 16;
const CURRENT_W: i32 = 372;
const OK: u32 = 0xFF3D_DC97;
const DANGER: u32 = 0xFFFF_6B6B;

#[derive(Clone)]
pub(super) enum Activity {
    Notice(Notification),
    Volume,
    Power,
}

#[derive(Default)]
pub(super) struct Crest {
    activity: Option<(Activity, u64)>,
    queue: VecDeque<Notification>,
    hovered: bool,
    from: (i32, i32),
    to: (i32, i32),
    changed_at: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BarHit {
    Mark,
    App,
    Crest,
    Chip,
    Seek,
    Ring,
}

#[derive(Default)]
pub(super) struct Current {
    hover: Option<CurrentHit>,
    dragging: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CurrentHit {
    Network,
    Calm,
    Appearance,
    Lock,
    Mute,
    Volume,
    BatteryLevel,
    ShowCrest,
    LockButton,
    Power,
    Settings,
}

pub(super) fn crest_on() -> bool {
    prefs::get_bool(pref::CREST)
}

pub(super) fn battery() -> Option<PowerInfo> {
    let i = crate::acpi::power_info();
    (i.flags & power::BATTERY != 0).then_some(i)
}

/// (has a network card, connected with an address, that address is over Wi-Fi)
fn network() -> (bool, Option<[u8; 4]>, bool) {
    use aurora_abi::net::IF_LOOPBACK;
    let cards: Vec<_> = crate::net::interfaces().into_iter().filter(|i| i.flags & IF_LOOPBACK == 0).collect();
    // A Wi-Fi connection the user chose shows first, like on a laptop.
    let wlan = |i: &&aurora_abi::NetInterface| i.name.starts_with(b"wlan");
    let up = cards.iter().filter(|i| i.ip != [0; 4]).max_by_key(|i| wlan(i));
    (!cards.is_empty(), up.map(|i| i.ip), up.is_some_and(|i| i.name.starts_with(b"wlan")))
}

fn volume() -> (u32, bool) {
    let s = audio::state();
    ((s & 0xFF) as u32, s & aurora_abi::audio::MUTED != 0)
}

fn duration(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// "about 4 h 10 min" style battery status.
fn battery_status(i: &PowerInfo) -> String {
    let charging = i.flags & power::CHARGING != 0;
    let on_ac = i.flags & power::AC_ONLINE != 0;
    match (charging, on_ac, i.minutes) {
        (true, _, 0) => String::from("Charging"),
        (true, _, m) => format!("{} until full", duration(m)),
        (false, true, _) if i.percent >= 95 => String::from("Fully charged"),
        (false, true, _) => String::from("Not charging"),
        (false, false, 0) => String::from("Calculating…"),
        (false, false, m) => format!("about {}", duration(m)),
    }
}

/// The time as Crest shows it: "9:41", "21:41", with seconds if asked for.
pub(super) fn time_text() -> String {
    let d = rtc::now();
    let secs = if prefs::get_bool(pref::CLOCK_SECONDS) { format!(":{:02}", d.second) } else { String::new() };
    if prefs::get_bool(pref::CLOCK_24H) {
        format!("{:02}:{:02}{}", d.hour, d.minute, secs)
    } else {
        let h = match d.hour % 12 {
            0 => 12,
            h => h,
        };
        format!("{}:{:02}{}", h, d.minute, secs)
    }
}

/// "Mon 26 Oct" (with AM/PM first on a 12-hour clock: "PM · Mon 26 Oct").
pub(super) fn date_text() -> String {
    let d = rtc::now();
    let date = format!("{} {} {}", d.weekday_name(), d.day, d.month_name());
    if prefs::get_bool(pref::CLOCK_24H) {
        date
    } else {
        format!("{} · {}", if d.hour < 12 { "AM" } else { "PM" }, date)
    }
}

/// Ease-out with one overshoot (Swell), on 0..=1000.
fn swell(t: i64) -> i64 {
    let q = t.clamp(0, 1000) - 1000; // -1000..=0
    1000 + (2400 * q * q / 1000 * q / 1000 + 1400 * q * q / 1000) / 1000
}

/// The Mist (resting) and Deep (holding an activity) glass tints.
fn deep_tint() -> u32 {
    if theme::current().dark {
        0xC406_0A1A
    } else {
        0xDCFF_FFFF
    }
}

/// The status ring: battery as an arc open at the bottom, the network in the
/// centre. `cx`, `cy` in 1/16 px; `size` is the ring's box in pixels.
pub(super) fn paint_ring(cv: &mut Canvas, cx: i32, cy: i32, size: i32, color: u32) {
    let unit = size * 16 / 22; // one design unit (the ring is drawn on a 22-unit grid), 1/16 px
    let r = 9 * unit;
    let width = if size >= 30 { 2 * unit } else { 22 * unit / 10 };
    // The arc starts at 126° and spans 80% of the circle, leaving a gap at the bottom.
    let (start, span) = (126 * 1024 / 360, 1024 * 80 / 100);
    let track = with_alpha(color, 0x48);
    cv.stroke_arc(cx, cy, r, width, start, span, track);
    if let Some(b) = battery() {
        let charging = b.flags & power::CHARGING != 0;
        let low = !charging && b.percent < 20;
        let c = if charging {
            OK
        } else if low {
            DANGER
        } else {
            color
        };
        cv.stroke_arc(cx, cy, r, width, start, span * b.percent.min(100) as i32 / 100, c);
        if charging {
            // A small bolt in the gap.
            let (x, y) = (cx / 16, cy / 16 + 7 * unit / 16);
            let u = unit / 16;
            cv.line(x + u / 2, y - u, x - u / 2, y, 1, OK);
            cv.line(x - u / 2, y, x + u / 2, y, 1, OK);
            cv.line(x + u / 2, y, x - u / 2, y + u, 1, OK);
        }
    }
    // Centre: Wi-Fi or a wired network, with a red slash when offline.
    let (card, address, wifi) = network();
    let px = |v: i32| v * unit / 16; // design units to pixels
    let (x0, y0) = (cx / 16 - px(11), cy / 16 - px(11));
    let at = |ux: i32, uy: i32| (x0 + px(ux), y0 + px(uy));
    let stroke = if size >= 30 { 2 } else { 1 };
    if wifi {
        // Three arcs over a dot, opening upwards.
        let base = (cx / 16, at(0, 15).1);
        cv.fill_circle(base.0, base.1, (px(3) / 2).max(1), color);
        for k in 1..=3 {
            let r = px(3 * k) * 16;
            cv.stroke_arc(base.0 * 16, base.1 * 16, r, 16 * stroke + 8, 640, 256, color);
        }
    } else if card {
        let c = if address.is_some() { color } else { with_alpha(color, 0x70) };
        let (a, b) = (at(9, 7), at(13, 10));
        cv.stroke_round_rect(Rect::new(a.0, a.1, (b.0 - a.0).max(3), (b.1 - a.1).max(2)), 1, c);
        let mid = (cx / 16, at(0, 10).1);
        let bar = at(0, 12).1;
        cv.line(mid.0, mid.1, mid.0, bar, stroke, c);
        cv.line(at(8, 0).0, bar, at(14, 0).0, bar, stroke, c);
        cv.line(at(8, 0).0, bar, at(8, 0).0, bar + px(2), stroke, c);
        cv.line(at(14, 0).0, bar, at(14, 0).0, bar + px(2), stroke, c);
        if address.is_none() {
            cv.line(at(7, 6).0, at(7, 6).1, at(15, 14).0, at(15, 14).1, stroke, DANGER);
        }
    }
}

/// The magnifying glass for Seek.
pub(super) fn paint_seek(cv: &mut Canvas, cx: i32, cy: i32, color: u32) {
    cv.stroke_arc((cx - 2) * 16, (cy - 2) * 16, 5 * 16 + 8, 28, 0, 1024, color);
    cv.line(cx + 2, cy + 2, cx + 6, cy + 6, 2, color);
}

fn padlock(cv: &mut Canvas, cx: i32, cy: i32, color: u32) {
    cv.fill_round_rect(Rect::new(cx - 6, cy - 2, 12, 9), 2, color);
    cv.stroke_arc(cx * 16, (cy - 3) * 16, 52, 26, 512, 512, color);
    cv.line(cx - 4, cy - 3, cx - 4, cy - 1, 2, color);
    cv.line(cx + 4, cy - 3, cx + 4, cy - 1, 2, color);
}

impl Desktop {
    // ---------------------------------------------------------- layout

    pub(super) fn app_label(&self) -> String {
        match self.focused() {
            Some(w) if w.app.kind() == apps::AppKind::Other => w.app.title(),
            Some(w) => String::from(apps::info(w.app.kind()).name),
            None => String::from("Desktop"),
        }
    }

    /// The left bubble and its two targets (the WayV mark, the app name).
    pub(super) fn left_bubble(&self) -> (Rect, Rect, Rect) {
        let name = self.app_label();
        let nw = theme::ui_bold(14).width(&name).min(self.w / 3);
        let mark = Rect::new(8 + 6, TOP + 4, 34, H - 8);
        let app = Rect::new(mark.right(), TOP + 4, nw + 16, H - 8);
        (Rect::new(8, TOP, app.right() - 8 + 8, H), mark, app)
    }

    /// The chip that stands in for Crest's activity when Crest is off.
    fn chip_width(&self) -> i32 {
        match self.crest.activity.as_ref() {
            Some((Activity::Notice(n), _)) => 26 + theme::ui_bold(12).width(&n.title).min(150) + 12,
            Some((Activity::Volume, _)) => 82,
            Some((Activity::Power, _)) => 70,
            None => 0,
        }
    }

    /// (bubble, chip, seek, ring, ring size, percent text, date/time text)
    #[allow(clippy::type_complexity)]
    pub(super) fn right_bubble(&self) -> (Rect, Option<Rect>, Rect, Rect, i32, Option<Rect>, Option<Rect>) {
        let crest_on = crest_on();
        let percent = (prefs::get_bool(pref::BATTERY_LEVEL) && battery().is_some()).then(|| {
            let f = theme::ui_bold(13);
            f.width("100%")
        });
        let ring_size = if percent.is_some() { 28 } else { 32 };
        let clock = (!crest_on).then(|| theme::ui_bold(14).width(&format!("{}  {}", date_text(), time_text())));
        let chip = (!crest_on && self.crest.activity.is_some()).then(|| self.chip_width());
        // Seek is hidden on Shore.
        let seek_w = if self.locked() { 0 } else { 26 + 12 };
        let mut w = 14 + seek_w + ring_size + 8;
        if let Some(p) = percent {
            w += 6 + p - 4;
        }
        if let Some(c) = clock {
            w += 10 + c + 6;
        }
        if let Some(c) = chip {
            w += c + 10;
        }
        let bubble = Rect::new(self.w - 8 - w, TOP, w, H);
        let mut x = bubble.x + 14;
        let chip_r = chip.map(|c| {
            let r = Rect::new(x - 6, TOP + 6, c, H - 12);
            x += c + 10 - 6;
            r
        });
        let seek = Rect::new(x, TOP + 6, 26, 26);
        x += seek_w;
        let ring = Rect::new(x, TOP + (H - ring_size) / 2, ring_size, ring_size);
        x += ring_size;
        let pct = percent.map(|p| {
            let r = Rect::new(x + 4, TOP, p, H);
            x += 6 + p - 4;
            r
        });
        let date = clock.map(|c| Rect::new(x + 10, TOP, c, H));
        (bubble, chip_r, seek, ring, ring_size, pct, date)
    }

    fn crest_target(&self) -> (i32, i32) {
        if !crest_on() {
            return (0, 0);
        }
        match self.crest.activity.as_ref().map(|(a, _)| a) {
            Some(Activity::Notice(_)) => (380.min(self.w - 40), 62),
            Some(Activity::Volume) => (220, H),
            Some(Activity::Power) => (210, H),
            None => {
                let time = theme::ui_bold(15).width(&time_text());
                let w = if self.crest.hovered || self.locked() {
                    let date = theme::ui_bold(13).width(&date_text());
                    if self.locked() {
                        92
                    } else {
                        time + 14 + date + 36
                    }
                } else {
                    time + 36
                };
                (w.max(104), H)
            }
        }
    }

    /// Crest's current (possibly animating) outline.
    pub(super) fn crest_rect(&self) -> Rect {
        let (fw, fh) = self.crest.from;
        let (tw, th) = self.crest.to;
        let t = if theme::reduce_motion() {
            1000
        } else {
            (self.now_ms.saturating_sub(self.crest.changed_at) * 1000 / RISE_MS) as i64
        };
        let e = swell(t);
        let w = (fw as i64 + (tw - fw) as i64 * e / 1000) as i32;
        let h = (fh as i64 + (th - fh) as i64 * e / 1000) as i32;
        Rect::new(self.w / 2 - w / 2, TOP, w.max(0), h.max(0))
    }

    /// Everything Crest may paint over (its largest shape plus the glow).
    pub(super) fn crest_area(&self) -> Rect {
        Rect::new(self.w / 2 - 240, 0, 480, TOP + 62 + 30)
    }

    /// Re-aims Crest's shape after its state changed.
    pub(super) fn crest_retarget(&mut self) {
        let to = self.crest_target();
        if to != self.crest.to {
            let now = self.crest_rect();
            self.crest.from = if self.crest.to == (0, 0) { (to.0 * 9 / 10, to.1) } else { (now.w, now.h) };
            self.crest.to = to;
            self.crest.changed_at = self.now_ms;
        }
        let a = self.crest_area();
        self.damage(a);
        self.damage(Rect::new(self.w / 2, 0, self.w / 2, MENUBAR_H));
    }

    pub(super) fn bar_hit(&self, x: i32, y: i32) -> Option<BarHit> {
        if y >= MENUBAR_H + 30 || self.locked() {
            return None;
        }
        let (_, mark, app) = self.left_bubble();
        if mark.contains(x, y) {
            return Some(BarHit::Mark);
        }
        if app.contains(x, y) {
            return Some(BarHit::App);
        }
        if crest_on() && self.crest_rect().contains(x, y) {
            return Some(BarHit::Crest);
        }
        let (bubble, chip, seek, ring, _, pct, date) = self.right_bubble();
        if chip.is_some_and(|c| c.contains(x, y)) {
            return Some(BarHit::Chip);
        }
        if seek.inset(-4).contains(x, y) {
            return Some(BarHit::Seek);
        }
        if ring.inset(-4).contains(x, y) || pct.is_some_and(|p| p.contains(x, y)) {
            return Some(BarHit::Ring);
        }
        if date.is_some_and(|d| d.contains(x, y)) {
            return Some(BarHit::Crest);
        }
        let _ = bubble;
        None
    }

    // ------------------------------------------------------- activities

    pub(super) fn show_activity(&mut self, a: Activity) {
        let until = self.now_ms
            + match a {
                Activity::Notice(_) => NOTICE_MS,
                Activity::Volume => VOLUME_MS,
                Activity::Power => POWER_MS,
            };
        // A notice that the volume interrupts comes back afterwards.
        if let (Activity::Volume, Some((Activity::Notice(n), _))) = (&a, &self.crest.activity) {
            self.crest.queue.push_front(n.clone());
        }
        self.crest.activity = Some((a, until));
        self.crest_retarget();
        let (b, ..) = self.right_bubble();
        self.damage(b.inset(-40));
    }

    /// A notification arrives: it rises out of Crest now, or after the one showing.
    pub(super) fn notice(&mut self, n: Notification) {
        match self.crest.activity {
            Some((Activity::Notice(_), _)) => self.crest.queue.push_back(n),
            _ => self.show_activity(Activity::Notice(n)),
        }
    }

    /// Ends activities whose time is up; true while Crest is moving.
    pub(super) fn step_bar(&mut self) -> bool {
        if let Some((_, until)) = &self.crest.activity {
            let hovered_notice = matches!(self.crest.activity, Some((Activity::Notice(_), _))) && self.crest.hovered;
            if self.now_ms >= *until && !hovered_notice {
                self.crest.activity = None;
                if let Some(n) = self.crest.queue.pop_front() {
                    self.show_activity(Activity::Notice(n));
                } else {
                    self.crest_retarget();
                }
                let (b, ..) = self.right_bubble();
                self.damage(b.inset(-80));
                self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
            }
        }
        let moving = self.now_ms < self.crest.changed_at + RISE_MS + 16;
        if moving {
            let a = self.crest_area();
            self.damage(a);
        }
        moving
    }

    pub(super) fn bar_deadline(&self) -> u64 {
        self.crest.activity.as_ref().map(|(_, until)| *until).unwrap_or(u64::MAX)
    }

    pub(super) fn crest_moving(&self) -> bool {
        self.now_ms < self.crest.changed_at + RISE_MS + 16
    }

    /// The volume keys (and media keys): change the level and show it in Crest.
    pub(super) fn volume_key(&mut self, code: crate::drivers::input::KeyCode) -> bool {
        use crate::drivers::input::KeyCode;
        let (v, muted) = volume();
        match code {
            KeyCode::Mute => prefs::set_volume(v, !muted),
            KeyCode::VolumeUp => prefs::set_volume((v + STEP).min(100), false),
            KeyCode::VolumeDown => prefs::set_volume(v.saturating_sub(STEP), false),
            _ => return false,
        }
        if code != KeyCode::Mute && !volume().1 {
            audio::play_sound("volume");
        }
        self.show_activity(Activity::Volume);
        if self.current.is_some() {
            let r = self.current_rect();
            self.damage(r);
        }
        true
    }

    /// Battery or adapter changed: plugging in shows the charge in Crest.
    pub(super) fn power_changed(&mut self) {
        let ac = crate::acpi::power_info().flags & power::AC_ONLINE != 0;
        if self.last_ac == Some(false) && ac && battery().is_some() {
            self.show_activity(Activity::Power);
        }
        self.last_ac = Some(ac);
        self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
    }

    // ------------------------------------------------------------ input

    pub(super) fn bar_hover(&mut self, x: i32, y: i32) {
        let hit = self.bar_hit(x, y);
        if hit != self.bar_hover {
            self.bar_hover = hit;
            self.damage(Rect::new(0, 0, self.w, MENUBAR_H + 4));
        }
        let over_crest = hit == Some(BarHit::Crest) && crest_on();
        if over_crest != self.crest.hovered {
            self.crest.hovered = over_crest;
            self.crest_retarget();
        }
        if let Some(c) = &self.current {
            let h = self.current_hit(x, y);
            if h != c.hover {
                self.current.as_mut().unwrap().hover = h;
                let r = self.current_rect();
                self.damage(r);
            }
        }
    }

    /// A click on the bar or in Current. True if handled.
    pub(super) fn bar_press(&mut self, x: i32, y: i32) -> bool {
        if self.current.is_some() {
            if self.current_rect().contains(x, y) {
                self.current_press(x, y);
                return true;
            }
            self.close_current();
            if self.bar_hit(x, y) == Some(BarHit::Ring) {
                return true;
            }
        }
        match self.bar_hit(x, y) {
            Some(BarHit::Ring) => {
                self.close_menu();
                if self.center_open() {
                    self.toggle_center();
                }
                self.current = Some(Current::default());
                let r = self.current_rect();
                self.damage(r.inset(-40));
                true
            }
            Some(BarHit::Seek) => {
                self.close_menu();
                self.toggle_spotlight();
                true
            }
            Some(BarHit::Crest) | Some(BarHit::Chip) => {
                if let Some((Activity::Notice(n), _)) = self.crest.activity.clone() {
                    self.crest.activity = None;
                    self.step_bar();
                    self.crest_retarget();
                    self.activate(&n);
                } else {
                    self.close_menu();
                    self.toggle_center();
                }
                true
            }
            Some(BarHit::Mark) | Some(BarHit::App) => false, // menus (shell)
            None => y < TOP + H,
        }
    }

    // --------------------------------------------------------- painting

    pub(super) fn paint_bar(&mut self, cv: &mut Canvas) {
        let t = theme::current();
        let clip = cv.clip;
        if !Rect::new(0, 0, self.w, MENUBAR_H + 40).intersects(&clip) {
            return;
        }
        let ink = t.text_on_glass;
        let menu_kind = self.menu.as_ref().map(|m| m.is_app_menu());

        // Left: the WayV mark and the app (hidden on Shore: it would tell what's open).
        let locked = self.locked();
        let (left, mark, app) = self.left_bubble();
        if !locked {
            cv.shadow(left, H / 2, 14, 4, t.shadow / 4);
            cv.frosted(left, H / 2, t.glass_tint);
            for (hit, r, open) in
                [(BarHit::Mark, mark, menu_kind == Some(false)), (BarHit::App, app, menu_kind == Some(true))]
            {
                if open || self.bar_hover == Some(hit) {
                    cv.fill_round_rect(r, 12, if open { with_alpha(ink, 0x2A) } else { t.hover });
                }
            }
            if self.mark.is_none() {
                self.mark = Some(
                    crate::fs::read_all("/System/Brand/wayv-mark.png")
                        .ok()
                        .and_then(|d| aurora_image::decode(&d).ok())
                        .map(|img| img.thumbnail(40, 15)),
                );
            }
            match self.mark.as_ref().and_then(|m| m.as_ref()) {
                Some(img) => {
                    let r = Rect::new(
                        mark.x + (mark.w - img.width as i32) / 2,
                        mark.y + (mark.h - img.height as i32) / 2,
                        img.width as i32,
                        img.height as i32,
                    );
                    cv.draw_image(&img.pixels, img.width, img.height, r, 255);
                }
                None => icons::wave(cv, Rect::new(mark.x + 6, mark.y + 9, 22, 12), 2, 2, ink),
            }
            let name = self.app_label();
            cv.text_clipped(app.x + 7, TOP + 24, &name, theme::ui_bold(14), ink, app.w - 12);
        }

        // Middle: Crest.
        if crest_on() {
            self.paint_crest(cv);
        }

        // Right: chip, Seek, the ring, the percentage and (Crest off) the date and time.
        let (bubble, chip, seek, ring, size, pct, date) = self.right_bubble();
        cv.shadow(bubble, H / 2, 14, 4, t.shadow / 4);
        cv.frosted(bubble, H / 2, t.glass_tint);
        if let Some(c) = chip {
            self.paint_chip(cv, c);
        }
        for (hit, r) in [(BarHit::Seek, seek), (BarHit::Ring, ring)] {
            let open = hit == BarHit::Ring && self.current.is_some();
            if open || self.bar_hover == Some(hit) {
                let (cx, cy) = r.center();
                let rr = r.w / 2 + 3;
                cv.fill_circle(cx, cy, rr, if open { with_alpha(ink, 0x2A) } else { t.hover });
            }
        }
        if !locked {
            let (sx, sy) = seek.center();
            paint_seek(cv, sx, sy, ink);
        }
        paint_ring(cv, ring.x * 16 + ring.w * 8, ring.y * 16 + ring.h * 8, size, ink);
        if let (Some(p), Some(b)) = (pct, battery()) {
            cv.text(p.x, TOP + 24, &format!("{}%", b.percent), theme::ui_bold(13), ink);
        }
        if let Some(d) = date {
            let text = format!("{}  {}", date_text(), time_text());
            cv.text(d.x, TOP + 25, &text, theme::ui_bold(14), ink);
        }

        self.paint_current(cv);
    }

    fn paint_crest(&self, cv: &mut Canvas) {
        let r = self.crest_rect();
        if r.w <= 0 || !self.crest_area().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        let radius = r.h.min(52) / 2;
        let ink = t.text_on_glass;
        let activity = self.crest.activity.as_ref().map(|(a, _)| a);
        match activity {
            None => {
                cv.shadow(r, radius, 14, 4, t.shadow / 4);
                cv.frosted(r, radius, t.glass_tint);
            }
            Some(a) => {
                let glow = match a {
                    Activity::Power => OK,
                    _ => theme::accent_glow(),
                };
                cv.shadow(r, radius, 20, 6, t.shadow / 2);
                cv.glow(r, radius, 12, glow, if t.dark { 70 } else { 90 });
                cv.frosted(r, radius, deep_tint());
                cv.stroke_round_rect(r, radius, with_alpha(glow, 0x90));
            }
        }
        let inner = r.intersect(&cv.clip);
        cv.with_clip(inner, |cv| match activity {
            None if self.locked() => {
                let (cx, cy) = r.center();
                padlock(cv, cx, cy, ink);
            }
            None => {
                let time = time_text();
                let tf = theme::ui_bold(15);
                if self.crest.hovered && r.w > tf.width(&time) + 60 {
                    let date = date_text();
                    let df = theme::ui_bold(13);
                    let total = tf.width(&time) + 14 + df.width(&date);
                    let x = r.x + (r.w - total) / 2;
                    cv.text(x, r.y + 25, &time, tf, ink);
                    cv.text(x + tf.width(&time) + 14, r.y + 25, &date, df, t.text_secondary);
                } else {
                    cv.text(r.x + (r.w - tf.width(&time)) / 2, r.y + 25, &time, tf, ink);
                }
            }
            Some(Activity::Volume) => {
                let (v, muted) = volume();
                icons::speaker(cv, Rect::new(r.x + 14, r.y + 10, 20, 18), (!muted).then_some(v), ink);
                let track = Rect::new(r.x + 46, r.y + r.h / 2 - 3, r.w - 64, 6);
                cv.fill_round_rect(track, 3, with_alpha(ink, 0x30));
                let level = if muted { 0 } else { v as i32 };
                if level > 0 {
                    cv.fill_round_rect(Rect::new(track.x, track.y, track.w * level / 100, 6), 3, theme::accent_glow());
                }
            }
            Some(Activity::Power) => {
                let b = battery();
                let pct = b.map(|b| b.percent).unwrap_or(100);
                cv.text(r.x + 18, r.y + 25, "Charging", theme::ui_bold(14), ink);
                let label = format!("{pct}%");
                let f = theme::ui_bold(14);
                let x = r.right() - 18 - 30 - 6 - f.width(&label);
                cv.text(x, r.y + 25, &label, f, OK);
                icons::battery(cv, Rect::new(r.right() - 48, r.y + 12, 30, 14), pct, true, OK, OK);
            }
            Some(Activity::Notice(n)) => {
                icons::draw(
                    cv,
                    super::notifications::icon_for(&n.app_path),
                    Rect::new(r.x + 12, r.y + (r.h - 36) / 2, 36, 36),
                );
                let x = r.x + 58;
                let w = r.w - 58 - 16;
                let small = theme::ui(11);
                cv.text(r.right() - 16 - small.width("now"), r.y + 25, "now", small, t.text_secondary);
                cv.text_clipped(x, r.y + 26, &n.title, theme::ui_bold(14), ink, w - 34);
                cv.text_clipped(x, r.y + 46, &n.body, theme::ui(13), t.text_secondary, w);
            }
        });
    }

    fn paint_chip(&self, cv: &mut Canvas, r: Rect) {
        let t = theme::current();
        let glow = match self.crest.activity.as_ref().map(|(a, _)| a) {
            Some(Activity::Power) => OK,
            _ => theme::accent_glow(),
        };
        cv.fill_round_rect(r, r.h / 2, with_alpha(glow, if t.dark { 0x38 } else { 0x40 }));
        let ink = if t.dark { glow } else { theme::accent_ink() };
        match self.crest.activity.as_ref().map(|(a, _)| a) {
            Some(Activity::Notice(n)) => {
                icons::draw(cv, super::notifications::icon_for(&n.app_path), Rect::new(r.x + 5, r.y + 3, 20, 20));
                cv.text_clipped(r.x + 30, r.y + 18, &n.title, theme::ui_bold(12), t.text_on_glass, r.w - 38);
            }
            Some(Activity::Volume) => {
                let (v, muted) = volume();
                icons::speaker(cv, Rect::new(r.x + 9, r.y + 5, 16, 16), (!muted).then_some(v), ink);
                let label = if muted { String::from("Muted") } else { format!("{v}%") };
                cv.text(r.x + 32, r.y + 18, &label, theme::ui_bold(12), ink);
            }
            Some(Activity::Power) => {
                let pct = battery().map(|b| b.percent).unwrap_or(100);
                cv.text(r.x + 12, r.y + 18, &format!("{pct}% ⚡"), theme::ui_bold(12), OK);
            }
            None => {}
        }
    }

    // ---------------------------------------------------------- Current

    fn current_layout(&self) -> (Rect, Vec<(CurrentHit, Rect)>) {
        let pad = 14;
        let x = self.w - 8 - CURRENT_W;
        let mut y = MENUBAR_H + 2 + pad;
        let inner_w = CURRENT_W - 2 * pad;
        let tile_w = (inner_w - 10) / 2;
        let mut hits = Vec::new();
        for (i, hit) in
            [CurrentHit::Network, CurrentHit::Calm, CurrentHit::Appearance, CurrentHit::Lock].into_iter().enumerate()
        {
            let i = i as i32;
            hits.push((hit, Rect::new(x + pad + (i % 2) * (tile_w + 10), y + (i / 2) * 72, tile_w, 62)));
        }
        y += 144;
        // Sound card: the speaker mutes, the bar sets the level.
        hits.push((CurrentHit::Mute, Rect::new(x + pad + 12, y + 34, 26, 26)));
        hits.push((CurrentHit::Volume, Rect::new(x + pad + 12, y + 34, inner_w - 24, 26)));
        y += 74 + 10;
        if battery().is_some() {
            y += 42;
            hits.push((CurrentHit::BatteryLevel, Rect::new(x + pad, y, inner_w, 42)));
            y += 42;
        }
        hits.push((CurrentHit::ShowCrest, Rect::new(x + pad, y, inner_w, 42)));
        y += 42 + 12;
        let foot = y;
        hits.push((CurrentHit::Settings, Rect::new(x + CURRENT_W - pad - 86, foot, 86, 30)));
        hits.push((CurrentHit::Power, Rect::new(x + CURRENT_W - pad - 86 - 38, foot, 30, 30)));
        hits.push((CurrentHit::LockButton, Rect::new(x + CURRENT_W - pad - 86 - 76, foot, 30, 30)));
        y += 30 + pad;
        (Rect::new(x, MENUBAR_H + 2, CURRENT_W, y - (MENUBAR_H + 2)), hits)
    }

    pub(super) fn current_rect(&self) -> Rect {
        self.current_layout().0.inset(-36)
    }

    fn current_hit(&self, x: i32, y: i32) -> Option<CurrentHit> {
        let (_, hits) = self.current_layout();
        // The mute button sits inside the volume bar: check it first.
        hits.iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| *h)
    }

    pub(super) fn close_current(&mut self) {
        if self.current.take().is_some() {
            let r = self.current_rect();
            self.damage(r);
            self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
        }
    }

    pub(super) fn current_open(&self) -> bool {
        self.current.is_some()
    }

    fn set_volume_at(&mut self, x: i32) {
        let (_, hits) = self.current_layout();
        let Some((_, bar)) = hits.iter().find(|(h, _)| *h == CurrentHit::Volume) else { return };
        let v = ((x - bar.x) * 100 / bar.w.max(1)).clamp(0, 100) as u32;
        prefs::set_volume(v, false);
        let r = self.current_rect();
        self.damage(r);
    }

    fn toggle_pref(&mut self, key: &str) {
        let on = prefs::get_bool(key);
        if let Err(e) = prefs::set(key, if on { "0" } else { "1" }) {
            crate::gui::notify::system("Couldn't save setting", aurora_abi::err::name(e));
        }
    }

    fn current_press(&mut self, x: i32, y: i32) {
        match self.current_hit(x, y) {
            Some(CurrentHit::Network) => {
                self.close_current();
                self.spawn_program(apps::info(apps::AppKind::Settings).path, &["network"]);
            }
            Some(CurrentHit::Calm) => self.toggle_pref(pref::DND),
            Some(CurrentHit::Appearance) => self.toggle_pref(pref::DARK),
            Some(CurrentHit::Lock) | Some(CurrentHit::LockButton) => {
                self.close_current();
                self.lock();
            }
            Some(CurrentHit::Mute) => {
                let (v, muted) = volume();
                prefs::set_volume(v, !muted);
            }
            Some(CurrentHit::Volume) => {
                if let Some(c) = self.current.as_mut() {
                    c.dragging = true;
                }
                self.set_volume_at(x);
            }
            Some(CurrentHit::BatteryLevel) => self.toggle_pref(pref::BATTERY_LEVEL),
            Some(CurrentHit::ShowCrest) => self.toggle_pref(pref::CREST),
            Some(CurrentHit::Power) => {
                self.close_current();
                self.open(apps::AppKind::Power);
            }
            Some(CurrentHit::Settings) => {
                self.close_current();
                self.open(apps::AppKind::Settings);
            }
            None => {}
        }
        let r = self.current_rect();
        self.damage(r);
    }

    /// Pointer motion with the button held: the volume bar follows. True if handled.
    pub(super) fn current_drag(&mut self, x: i32) -> bool {
        if self.current.as_ref().is_some_and(|c| c.dragging) {
            self.set_volume_at(x);
            return true;
        }
        false
    }

    /// Button released: a volume drag ends with the feedback sound.
    pub(super) fn current_release(&mut self) -> bool {
        if let Some(c) = self.current.as_mut() {
            if core::mem::take(&mut c.dragging) {
                if !volume().1 {
                    audio::play_sound("volume");
                }
                return true;
            }
        }
        false
    }

    fn paint_current(&self, cv: &mut Canvas) {
        let Some(c) = &self.current else { return };
        if !self.current_rect().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        let (panel, hits) = self.current_layout();
        cv.shadow(panel, 26, 34, 10, t.shadow);
        cv.frosted(panel, 26, t.panel_tint);
        let rect = |h: CurrentHit| hits.iter().find(|(k, _)| *k == h).map(|(_, r)| *r).unwrap_or_default();
        let card = if t.dark { 0x1AFF_FFFF } else { 0x0F0E_1530 };
        let tile_on = |cv: &mut Canvas,
                       r: Rect,
                       on: bool,
                       hovered: bool,
                       title: &str,
                       sub: &str,
                       glyph: &dyn Fn(&mut Canvas, i32, i32, u32)| {
            cv.fill_round_rect(r, 18, if hovered { mix(card, t.text, 18) } else { card });
            let (cx, cy) = (r.x + 30, r.y + r.h / 2);
            cv.fill_circle(cx, cy, 18, if on { theme::accent() } else { with_alpha(t.text, 0x22) });
            glyph(cv, cx, cy, if on { 0xFFFF_FFFF } else { t.text });
            cv.text(r.x + 58, r.y + 28, title, theme::ui_bold(13), t.text);
            cv.text_clipped(r.x + 58, r.y + 45, sub, theme::ui(12), t.text_secondary, r.w - 66);
        };
        let (card_present, address, wifi) = network();
        let net_sub = match (card_present, address) {
            (false, _) => String::from("No network card"),
            (true, None) => String::from("Not connected"),
            (true, Some(_)) if wifi => {
                format!("Wi-Fi · {}", crate::drivers::wifi::connected_network().unwrap_or_default())
            }
            (true, Some(a)) => format!("Wired · {}.{}.{}.{}", a[0], a[1], a[2], a[3]),
        };
        let hov = |h: CurrentHit| c.hover == Some(h);
        tile_on(
            cv,
            rect(CurrentHit::Network),
            address.is_some(),
            hov(CurrentHit::Network),
            "Network",
            &net_sub,
            &|cv, x, y, col| paint_ring(cv, x * 16, y * 16, 22, col),
        );
        let calm = prefs::get_bool(pref::DND);
        tile_on(
            cv,
            rect(CurrentHit::Calm),
            calm,
            hov(CurrentHit::Calm),
            "Calm",
            if calm { "On" } else { "Off" },
            &|cv, x, y, col| {
                // A crescent moon.
                cv.fill_circle(x, y, 7, col);
                cv.fill_circle(x + 4, y - 3, 6, if calm { theme::accent() } else { mix(card, t.text, 34) });
            },
        );
        tile_on(
            cv,
            rect(CurrentHit::Appearance),
            t.dark,
            hov(CurrentHit::Appearance),
            "Appearance",
            if t.dark { "Dark" } else { "Light" },
            &|cv, x, y, col| {
                cv.stroke_arc(x * 16, y * 16, 7 * 16, 26, 0, 1024, col);
                cv.with_clip(Rect::new(x - 8, y - 8, 8, 17), |cv| cv.fill_circle(x, y, 7, col));
            },
        );
        tile_on(cv, rect(CurrentHit::Lock), false, hov(CurrentHit::Lock), "Lock", "Super L", &|cv, x, y, col| {
            padlock(cv, x, y + 1, col)
        });

        // Sound.
        let sound = Rect::new(panel.x + 14, rect(CurrentHit::Volume).y - 34, panel.w - 28, 74);
        cv.fill_round_rect(sound, 18, card);
        cv.text(sound.x + 14, sound.y + 22, "Sound", theme::ui_bold(12), t.text_secondary);
        let bar = rect(CurrentHit::Volume);
        let (v, muted) = volume();
        cv.fill_round_rect(bar, 13, with_alpha(t.text, 0x26));
        let fill_c = if t.dark { 0xFFF2_F4FA } else { theme::accent() };
        let level = if muted { 0 } else { v as i32 };
        let fw = (bar.w * level / 100).max(26);
        cv.fill_round_rect(Rect::new(bar.x, bar.y, fw, bar.h), 13, fill_c);
        let knob_ink = if t.dark { 0xFF0E_1530 } else { 0xFFFF_FFFF };
        icons::speaker(cv, Rect::new(bar.x + 6, bar.y + 5, 16, 16), (!muted).then_some(v), knob_ink);
        if !audio::present() {
            cv.text(bar.right() - 90, sound.y + 22, "No sound device", theme::ui(11), t.text_secondary);
        }

        // Battery and the bar options.
        let row = |cv: &mut Canvas, r: Rect, label: &str, on: bool| {
            cv.text(r.x + 14, r.y + 26, label, theme::ui(13), t.text);
            crate::gui::widgets::toggle(cv, Rect::new(r.right() - 54, r.y + 10, 42, 22), on);
        };
        let toggles_top =
            if battery().is_some() { rect(CurrentHit::BatteryLevel).y - 42 } else { rect(CurrentHit::ShowCrest).y };
        let group =
            Rect::new(panel.x + 14, toggles_top, panel.w - 28, rect(CurrentHit::ShowCrest).bottom() - toggles_top);
        cv.fill_round_rect(group, 18, card);
        if let Some(b) = battery() {
            let r = Rect::new(group.x, group.y, group.w, 42);
            icons::battery(
                cv,
                Rect::new(r.x + 14, r.y + 14, 26, 14),
                b.percent,
                b.flags & power::CHARGING != 0,
                t.text,
                t.text,
            );
            cv.text(r.x + 50, r.y + 26, &format!("{}%", b.percent), theme::ui_bold(13), t.text);
            cv.text(r.x + 92, r.y + 26, &battery_status(&b), theme::ui(12), t.text_secondary);
            cv.fill_rect(Rect::new(r.x + 14, r.bottom(), r.w - 28, 1), t.separator);
            row(cv, rect(CurrentHit::BatteryLevel), "Show battery level", prefs::get_bool(pref::BATTERY_LEVEL));
            let s = rect(CurrentHit::BatteryLevel);
            cv.fill_rect(Rect::new(s.x + 14, s.bottom(), s.w - 28, 1), t.separator);
        }
        row(cv, rect(CurrentHit::ShowCrest), "Show Crest", crest_on());

        // Footer: who is logged in, Lock, Power and Settings.
        let foot = rect(CurrentHit::Settings);
        let av = Rect::new(panel.x + 16, foot.y, 30, 30);
        cv.fill_round_rect_dgradient(av, 15, 0xFF4A_63FF, 0xFF8D_2CF0);
        let (initials, name) = match &self.account {
            Some(a) => (a.initials(), a.full.clone()),
            None => (String::from("?"), String::from("WaveOS")),
        };
        cv.text_centered(av, &initials, theme::ui_bold(11), 0xFFFF_FFFF);
        cv.text_clipped(
            av.right() + 10,
            foot.y + 20,
            &name,
            theme::ui_bold(13),
            t.text,
            rect(CurrentHit::LockButton).x - av.right() - 18,
        );
        for h in [CurrentHit::LockButton, CurrentHit::Power, CurrentHit::Settings] {
            let r = rect(h);
            cv.fill_round_rect(r, 15, if hov(h) { mix(card, t.text, 30) } else { mix(card, t.text, 14) });
            let (cx, cy) = r.center();
            match h {
                CurrentHit::LockButton => padlock(cv, cx, cy + 1, t.text),
                CurrentHit::Power => {
                    let bg = cv.buf[(r.y * cv.width + r.x + 4) as usize];
                    icons::power(cv, cx, cy, 14, t.text, bg);
                }
                _ => cv.text_centered(r, "Settings…", theme::ui_bold(12), t.text),
            }
        }
        let _ = Icon::Settings;
    }
}
