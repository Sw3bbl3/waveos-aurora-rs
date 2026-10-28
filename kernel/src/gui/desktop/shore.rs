//! Shore: where you arrive. The first-boot setup (create the account, then
//! choose an appearance and a wallpaper), the login screen at start-up and
//! the lock screen (Super L, Current's Lock, waking from sleep).
//!
//! While Shore is up nothing else receives input, windows aren't drawn and
//! Crest shows a padlock.

use super::bar::{self, TOP};
use super::Desktop;
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::drivers::rtc;
use crate::gui::account::{self, Account};
use crate::gui::canvas::{mix, with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::{apps, icons, prefs, theme, wallpaper};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::pref;
use lumen::math::sin;

const SHAKE_MS: u64 = 420;
const CARD_W: i32 = 520;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// First boot: no account yet.
    Setup,
    /// Start-up.
    Login,
    /// Locked during a session.
    Locked,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Field(usize),
    AutoLogin,
    Continue,
    Back,
    Submit,
    Sleep,
    Restart,
    ShutDown,
    Appearance(bool),
    Wallpaper(u8),
}

struct Field {
    label: &'static str,
    placeholder: &'static str,
    text: String,
    secret: bool,
}

impl Field {
    fn new(label: &'static str, placeholder: &'static str, secret: bool) -> Field {
        Field { label, placeholder, text: String::new(), secret }
    }
}

pub(super) struct Shore {
    pub mode: Mode,
    /// Setup: 0 = account, 1 = appearance and wallpaper.
    step: u8,
    fields: Vec<Field>,
    focus: usize,
    auto_login: bool,
    name_edited: bool,
    error: Option<&'static str>,
    tries: u32,
    shake_at: Option<u64>,
    hover: Option<Hit>,
    /// Wallpaper thumbnails for setup's second step (decoded once).
    thumbs: Vec<Option<aurora_image::Image>>,
}

impl Shore {
    pub(super) fn new(mode: Mode) -> Shore {
        let fields = match mode {
            Mode::Setup => alloc::vec![
                Field::new("Full name", "Your name", false),
                Field::new("Account name", "", false),
                Field::new("Password", "", true),
                Field::new("Confirm", "", true),
                Field::new("Hint", "Shown after three wrong tries", false),
            ],
            _ => alloc::vec![Field::new("Password", "Enter password", true)],
        };
        Shore {
            mode,
            step: 0,
            fields,
            focus: 0,
            auto_login: false,
            name_edited: false,
            error: None,
            tries: 0,
            shake_at: None,
            hover: None,
            thumbs: Vec::new(),
        }
    }
}

/// How strong a password looks, 0–4 (length and variety).
fn strength(p: &str) -> u32 {
    let classes = [
        p.chars().any(|c| c.is_ascii_lowercase()),
        p.chars().any(|c| c.is_ascii_uppercase()),
        p.chars().any(|c| c.is_ascii_digit()),
        p.chars().any(|c| !c.is_ascii_alphanumeric()),
    ]
    .iter()
    .filter(|&&b| b)
    .count() as u32;
    let len = p.chars().count() as u32;
    match len {
        0 => 0,
        1..=5 => 1,
        6..=9 => 1 + (classes >= 2) as u32,
        _ => (2 + (classes >= 2) as u32 + (classes >= 3 || len >= 14) as u32).min(4),
    }
}

impl Desktop {
    pub(super) fn locked(&self) -> bool {
        self.shore.is_some()
    }

    /// The login screen to show at start-up, if any.
    pub(super) fn startup_shore(account: Option<&Account>) -> Option<Shore> {
        match account {
            None => Some(Shore::new(Mode::Setup)),
            Some(_) if prefs::get_bool(pref::AUTO_LOGIN) => None,
            Some(_) => Some(Shore::new(Mode::Login)),
        }
    }

    /// Locks the screen (nothing happens before an account exists).
    pub(super) fn lock(&mut self) {
        if self.account.is_none() || self.shore.is_some() {
            return;
        }
        self.close_menu();
        self.close_current();
        self.launcher = None;
        if self.center_open() {
            self.toggle_center();
        }
        if self.spotlight_open() {
            self.toggle_spotlight();
        }
        self.shore = Some(Shore::new(Mode::Locked));
        self.bar_hover = None;
        self.crest_retarget();
        self.damage_all();
    }

    fn unlock(&mut self) {
        let first_boot = self.shore.as_ref().is_some_and(|s| s.mode == Mode::Setup);
        // Welcome asked to be shown again at the next sign-in.
        let again = self.shore.as_ref().is_some_and(|s| s.mode == Mode::Login) && prefs::get_bool("welcome_again");
        self.shore = None;
        self.crest_retarget();
        self.damage_all();
        if first_boot || again {
            self.open(apps::AppKind::Welcome);
        }
    }

    // ---------------------------------------------------------- layout

    fn shore_layout(&self) -> Vec<(Hit, Rect)> {
        let Some(s) = &self.shore else { return Vec::new() };
        let (w, h) = (self.w, self.h);
        let mut v = Vec::new();
        match (s.mode, s.step) {
            (Mode::Setup, 0) => {
                let card = self.setup_card();
                let col = (card.w - 80 - 14) / 2;
                let (x0, x1) = (card.x + 40, card.x + 40 + col + 14);
                let y = card.y + 290;
                v.push((Hit::Field(0), Rect::new(x0, y, col, 36)));
                v.push((Hit::Field(1), Rect::new(x1, y, col, 36)));
                v.push((Hit::Field(2), Rect::new(x0, y + 70, col, 36)));
                v.push((Hit::Field(3), Rect::new(x1, y + 70, col, 36)));
                v.push((Hit::Field(4), Rect::new(x0, y + 146, card.w - 80, 36)));
                v.push((Hit::AutoLogin, Rect::new(x0, y + 196, card.w - 80, 50)));
                v.push((Hit::Continue, Rect::new(card.right() - 40 - 112, card.bottom() - 30 - 34, 112, 34)));
            }
            (Mode::Setup, _) => {
                let card = self.setup_card();
                let cw = (card.w - 80 - 14) / 2;
                v.push((Hit::Appearance(false), Rect::new(card.x + 40, card.y + 150, cw, 92)));
                v.push((Hit::Appearance(true), Rect::new(card.x + 40 + cw + 14, card.y + 150, cw, 92)));
                let tw = (card.w - 80 - 2 * 12) / 3;
                for i in 0..wallpaper::NAMES.len() as u8 {
                    v.push((
                        Hit::Wallpaper(i),
                        Rect::new(card.x + 40 + i as i32 * (tw + 12), card.y + 296, tw, tw * 5 / 8),
                    ));
                }
                v.push((Hit::Continue, Rect::new(card.right() - 40 - 112, card.bottom() - 30 - 34, 112, 34)));
                v.push((Hit::Back, Rect::new(card.right() - 40 - 112 - 10 - 84, card.bottom() - 30 - 34, 84, 34)));
            }
            _ => {
                let field = Rect::new(w / 2 - 140, h - 196, 280, 42);
                v.push((Hit::Field(0), field));
                v.push((Hit::Submit, Rect::new(field.right() - 37, field.y + 5, 32, 32)));
                for (i, hit) in [Hit::Sleep, Hit::Restart, Hit::ShutDown].into_iter().enumerate() {
                    v.push((hit, Rect::new(34 + i as i32 * 66, h - 98, 44, 44)));
                }
            }
        }
        v
    }

    fn setup_card(&self) -> Rect {
        let step = self.shore.as_ref().map_or(0, |s| s.step);
        let h = if step == 0 { 624 } else { 520 };
        let h = h.min(self.h - 40);
        Rect::new((self.w - CARD_W) / 2, (self.h - h) / 2, CARD_W, h)
    }

    fn shore_hit(&self, x: i32, y: i32) -> Option<Hit> {
        self.shore_layout().into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h)
    }

    // ------------------------------------------------------------ input

    pub(super) fn shore_pointer(&mut self, x: i32, y: i32, pressed: bool) {
        let hit = self.shore_hit(x, y);
        if let Some(s) = self.shore.as_mut() {
            if s.hover != hit {
                s.hover = hit;
                self.damage_all();
            }
        }
        if !pressed {
            return;
        }
        match hit {
            Some(Hit::Field(i)) => {
                if let Some(s) = self.shore.as_mut() {
                    s.focus = i;
                }
            }
            Some(Hit::AutoLogin) => {
                if let Some(s) = self.shore.as_mut() {
                    s.auto_login = !s.auto_login;
                }
            }
            Some(Hit::Continue) => self.shore_continue(),
            Some(Hit::Back) => {
                if let Some(s) = self.shore.as_mut() {
                    s.step = 0;
                    s.error = None;
                }
            }
            Some(Hit::Submit) => self.shore_submit(),
            Some(Hit::Sleep) => crate::power::sleep(),
            Some(Hit::Restart) => self.power = Some(super::PowerAction::Reboot),
            Some(Hit::ShutDown) => self.power = Some(super::PowerAction::Shutdown),
            Some(Hit::Appearance(dark)) if theme::current().dark != dark => self.set_dark(dark),
            Some(Hit::Wallpaper(i)) if theme::wallpaper() != i => self.set_wallpaper(i),
            _ => {}
        }
        self.damage_all();
    }

    pub(super) fn shore_key(&mut self, k: &KeyEvent) {
        if !k.pressed {
            return;
        }
        let Some(s) = self.shore.as_mut() else { return };
        let editing = !(s.mode == Mode::Setup && s.step == 1);
        match k.code {
            KeyCode::Enter => {
                if s.mode == Mode::Setup {
                    if s.step == 0 && s.focus + 1 < s.fields.len() {
                        s.focus += 1;
                    } else {
                        self.shore_continue();
                    }
                } else {
                    self.shore_submit();
                }
            }
            KeyCode::Tab if editing => {
                let n = s.fields.len();
                s.focus = if k.mods.shift { (s.focus + n - 1) % n } else { (s.focus + 1) % n };
            }
            KeyCode::Escape if s.mode != Mode::Setup => {
                s.fields[0].text.clear();
                s.error = None;
            }
            KeyCode::Backspace if editing => {
                let f = s.focus;
                s.fields[f].text.pop();
                s.error = None;
            }
            _ => match k.ch {
                Some(c) if editing && !c.is_control() && !k.mods.ctrl && !k.mods.super_key => {
                    let f = s.focus;
                    if s.fields[f].text.chars().count() < 64 {
                        s.fields[f].text.push(c);
                    }
                    s.error = None;
                    if s.mode == Mode::Setup && f == 1 {
                        s.name_edited = true;
                    }
                    if s.mode == Mode::Setup && f == 0 && !s.name_edited {
                        let short = account::short_name(&s.fields[0].text);
                        s.fields[1].text = if s.fields[0].text.trim().is_empty() { String::new() } else { short };
                    }
                }
                _ => {}
            },
        }
        // Keep the derived account name in step after deleting from the full name.
        if let Some(s) = self.shore.as_mut() {
            if s.mode == Mode::Setup && s.step == 0 && !s.name_edited && s.focus == 0 {
                s.fields[1].text = if s.fields[0].text.trim().is_empty() {
                    String::new()
                } else {
                    account::short_name(&s.fields[0].text)
                };
            }
        }
        self.damage_all();
    }

    fn shore_continue(&mut self) {
        let Some(s) = self.shore.as_mut() else { return };
        if s.step == 0 {
            let full = s.fields[0].text.trim();
            let pass = &s.fields[2].text;
            s.error = if full.is_empty() {
                s.focus = 0;
                Some("Enter your name.")
            } else if pass.chars().count() < 4 {
                s.focus = 2;
                Some("Use at least 4 characters for the password.")
            } else if *pass != s.fields[3].text {
                s.focus = 3;
                Some("The passwords don't match.")
            } else {
                None
            };
            if s.error.is_none() {
                s.step = 1;
                s.thumbs.clear();
            } else {
                s.shake_at = Some(self.now_ms);
            }
            return;
        }
        // Create the account (the password is hashed here and never stored).
        let (full, name, pass, hint, auto) = (
            s.fields[0].text.clone(),
            s.fields[1].text.clone(),
            s.fields[2].text.clone(),
            s.fields[4].text.clone(),
            s.auto_login,
        );
        match Account::create(&full, &name, &pass, &hint) {
            Ok(a) => {
                log!("gui", "created the account {}", a.name);
                self.account = Some(a);
                if let Err(e) = prefs::set(pref::AUTO_LOGIN, if auto { "1" } else { "0" }) {
                    log!("gui", "could not save auto login: {}", aurora_abi::err::name(e));
                }
                crate::drivers::audio::play_sound("startup");
                self.unlock();
            }
            Err(e) => {
                log!("gui", "could not save the account: {}", aurora_abi::err::name(e));
                if let Some(s) = self.shore.as_mut() {
                    s.error = Some("The account couldn't be saved. Is the disk writable?");
                }
            }
        }
    }

    fn shore_submit(&mut self) {
        let Some(s) = self.shore.as_mut() else { return };
        let ok = self.account.as_ref().is_some_and(|a| a.verify(&s.fields[0].text));
        if ok {
            s.fields[0].text.clear();
            self.unlock();
            return;
        }
        s.tries += 1;
        s.fields[0].text.clear();
        s.error = Some("Wrong password. Try again.");
        s.shake_at = Some(self.now_ms);
        crate::drivers::audio::play_sound("error");
    }

    /// True while the password field shakes.
    pub(super) fn shore_moving(&self) -> bool {
        self.shore.as_ref().is_some_and(|s| s.shake_at.is_some_and(|t| self.now_ms < t + SHAKE_MS))
    }

    // --------------------------------------------------------- painting

    pub(super) fn paint_shore(&mut self, cv: &mut Canvas) {
        let Some(mode) = self.shore.as_ref().map(|s| s.mode) else { return };
        if mode == Mode::Setup {
            self.paint_setup(cv);
        } else {
            self.paint_login(cv);
        }
    }

    fn shake(&self) -> i32 {
        let Some(t) = self.shore.as_ref().and_then(|s| s.shake_at) else { return 0 };
        let dt = self.now_ms.saturating_sub(t);
        if dt >= SHAKE_MS {
            return 0;
        }
        let decay = (SHAKE_MS - dt) as i32;
        10 * sin((dt * 1024 * 4 / SHAKE_MS) as i32) / 16384 * decay / SHAKE_MS as i32
    }

    fn paint_login(&mut self, cv: &mut Canvas) {
        let t = theme::current();
        let (w, h) = (self.w, self.h);
        // A gentle shade at the bottom keeps the text readable on any picture.
        let shade = if t.dark { 0xFF04_0716 } else { 0xFFFF_FFFF };
        let (k, clip) = (cv.scale, cv.pclip());
        for y in (h * 2 / 5 * k).max(clip.y)..(h * k).min(clip.bottom()) {
            let a = (y / k - h * 2 / 5) * if t.dark { 120 } else { 70 } / (h * 3 / 5).max(1);
            for x in clip.x..clip.right() {
                let i = (y * cv.width + x) as usize;
                cv.buf[i] = mix(cv.buf[i], shade, a);
            }
        }
        let ink = if t.dark { 0xFFF4_F6FF } else { 0xFF0E_1530 };
        let sub = if t.dark { 0xB8E8_ECFA } else { 0xA80E_1530 };
        let d = rtc::now();
        const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
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
        let date = format!("{} {} {}", DAYS[d.weekday() as usize], d.day, MONTHS[(d.month.clamp(1, 12) - 1) as usize]);
        let df = theme::ui_bold(22);
        cv.text(w / 2 - df.width(&date) / 2, 146, &date, df, ink);
        let time = bar::time_text();
        let tf = theme::display(120);
        cv.text(w / 2 - tf.width(&time) / 2, 260, &time, tf, ink);

        let s = self.shore.as_ref().unwrap();
        let (initials, name, hint) = match &self.account {
            Some(a) => (a.initials(), a.full.clone(), a.hint.clone()),
            None => (String::from("?"), String::new(), String::new()),
        };
        let av = Rect::new(w / 2 - 38, h - 336, 76, 76);
        cv.fill_circle(w / 2, av.y + 38, 41, with_alpha(0xFFFF_FFFF, 0x90));
        cv.fill_round_rect_dgradient(av, 38, 0xFF4A_63FF, 0xFF8D_2CF0);
        cv.text_centered(av, &initials, theme::display(27), 0xFFFF_FFFF);
        let nf = theme::ui_bold(17);
        cv.text(w / 2 - nf.width(&name) / 2, av.bottom() + 30, &name, nf, ink);

        let layout = self.shore_layout();
        let rect = |hit: Hit| layout.iter().find(|(h, _)| *h == hit).map(|(_, r)| *r).unwrap_or_default();
        let field = rect(Hit::Field(0)).offset(self.shake(), 0);
        cv.shadow(field, 21, 14, 4, t.shadow / 4);
        cv.frosted(field, 21, t.glass_tint);
        let typed = s.fields[0].text.chars().count() as i32;
        if typed == 0 {
            cv.text(
                field.x + 18,
                field.y + 27,
                if s.mode == Mode::Locked { "Password to unlock" } else { "Enter password" },
                theme::ui(14),
                t.text_secondary,
            );
        }
        for i in 0..typed.min(20) {
            cv.fill_circle(field.x + 22 + i * 15, field.y + 21, 4, t.text_on_glass);
        }
        if (self.now_ms / 530).is_multiple_of(2) {
            let cx = field.x + 18 + typed.min(20) * 15 + if typed > 0 { 0 } else { -2 };
            cv.fill_rect(Rect::new(cx, field.y + 12, 2, 18), theme::accent_glow());
        }
        let go = rect(Hit::Submit).offset(self.shake(), 0);
        cv.fill_round_rect(go, 16, theme::accent());
        let (gx, gy) = go.center();
        cv.line(gx - 5, gy, gx + 5, gy, 2, 0xFFFF_FFFF);
        cv.line(gx + 1, gy - 4, gx + 5, gy, 2, 0xFFFF_FFFF);
        cv.line(gx + 1, gy + 4, gx + 5, gy, 2, 0xFFFF_FFFF);
        let caption = match s.error {
            Some(e) if s.tries >= 3 && !hint.is_empty() => format!("{e} Hint: {hint}"),
            Some(e) => String::from(e),
            None if s.mode == Mode::Locked => String::from("Locked · enter your password to continue"),
            None => String::from("Enter your password to wake WaveOS"),
        };
        let cf = theme::ui_bold(12);
        let cc = if s.error.is_some() { 0xFFFF_8A8A } else { sub };
        cv.text(w / 2 - cf.width(&caption) / 2, field.bottom() + 26, &caption, cf, cc);

        // Sleep, Restart and Shut Down.
        for (hit, label) in [(Hit::Sleep, "Sleep"), (Hit::Restart, "Restart"), (Hit::ShutDown, "Shut Down")] {
            let r = rect(hit);
            cv.frosted(r, 22, t.glass_tint);
            if s.hover == Some(hit) {
                cv.fill_round_rect(r, 22, t.hover);
            }
            let (cx, cy) = r.center();
            match hit {
                Hit::Sleep => {
                    cv.fill_circle(cx, cy, 8, ink);
                    cv.fill_circle(cx + 4, cy - 3, 7, cv.pixel(r.x + 3, r.y));
                }
                Hit::Restart => {
                    cv.stroke_arc(cx * 16, cy * 16, 7 * 16, 30, 128 + 64, 832, ink);
                    cv.line(cx + 6, cy - 9, cx + 7, cy - 4, 2, ink);
                    cv.line(cx + 2, cy - 4, cx + 7, cy - 4, 2, ink);
                }
                _ => {
                    let bg = cv.pixel(r.x + 4, r.y);
                    icons::power(cv, cx, cy, 16, ink, bg);
                }
            }
            let f = theme::ui_bold(12);
            cv.text(cx - f.width(label) / 2, r.bottom() + 18, label, f, ink);
        }

        // Network state, bottom right.
        let (chip, label) = self.network_chip();
        cv.frosted(chip, 15, t.glass_tint);
        cv.text(chip.x + 12, chip.y + 20, label, theme::ui_bold(12), t.text_on_glass);
    }

    fn network_chip(&self) -> (Rect, &'static str) {
        let label = if crate::net::online() { "Wired" } else { "Offline" };
        let lw = theme::ui_bold(12).width(label);
        (Rect::new(self.w - 34 - lw - 24, self.h - 72, lw + 24, 30), label)
    }

    /// Shore's glass (for partial repaints, which must take in all of it).
    pub(super) fn shore_materials(&self) -> Vec<Rect> {
        let Some(s) = &self.shore else { return Vec::new() };
        if s.mode == Mode::Setup {
            return alloc::vec![self.setup_card()];
        }
        let mut v: Vec<Rect> = self
            .shore_layout()
            .into_iter()
            .filter(|(h, _)| matches!(h, Hit::Field(0) | Hit::Sleep | Hit::Restart | Hit::ShutDown))
            .map(|(_, r)| r)
            .collect();
        v.push(self.network_chip().0);
        v
    }

    /// Where the blinking caret is: the only thing a blink repaints.
    pub(super) fn caret_area(&self) -> Option<Rect> {
        let s = self.shore.as_ref()?;
        let focus = if s.mode == Mode::Setup { s.focus } else { 0 };
        self.shore_layout().into_iter().find(|(h, _)| *h == Hit::Field(focus)).map(|(_, r)| r)
    }

    fn paint_setup(&mut self, cv: &mut Canvas) {
        let t = theme::current();
        // The blurred wallpaper under a veil.
        cv.blit_same(&self.blurred, cv.clip);
        cv.fill_rect(cv.clip, if t.dark { 0x7004_0716 } else { 0x50F0_F4FF });
        let card = self.setup_card();
        cv.shadow(card, 28, 40, 12, t.shadow);
        cv.frosted(card, 28, t.panel_tint);
        let layout = self.shore_layout();
        let rect = |hit: Hit| layout.iter().find(|(h, _)| *h == hit).map(|(_, r)| *r).unwrap_or_default();
        let s = self.shore.as_ref().unwrap();

        // Step dots.
        for i in 0..2 {
            let on = i == s.step as i32;
            let x = card.x + 40 + i * 14 + if i > s.step as i32 { 16 } else { 0 };
            if on {
                cv.fill_round_rect(Rect::new(x, card.y + 34, 22, 6), 3, theme::accent_glow());
            } else {
                cv.fill_round_rect(
                    Rect::new(x, card.y + 34, 6, 6),
                    3,
                    if i < s.step as i32 { theme::accent_ink() } else { with_alpha(t.text, 0x30) },
                );
            }
        }
        let label = format!("Step {} of 2", s.step + 1);
        let lf = theme::ui_bold(12);
        cv.text(card.right() - 40 - lf.width(&label), card.y + 42, &label, lf, t.text_secondary);
        let center = |cv: &mut Canvas, y: i32, text: &str, f: lumen::font::Font, c: u32| {
            cv.text(card.x + (card.w - f.width(text)) / 2, y, text, f, c);
        };

        if s.step == 0 {
            // Picture circle with the initials typed so far.
            let av = Rect::new(card.x + card.w / 2 - 42, card.y + 66, 84, 84);
            cv.fill_round_rect_dgradient(av, 42, 0xFF4A_63FF, 0xFF8D_2CF0);
            let initials = account::initials(&s.fields[0].text);
            cv.text_centered(
                av,
                if s.fields[0].text.trim().is_empty() { "" } else { &initials },
                theme::display(30),
                0xFFFF_FFFF,
            );
            center(cv, card.y + 190, "Make this computer yours", theme::display(22), t.text);
            let lines = [
                "One account keeps your files, settings and wallpaper. You'll use",
                "the password to log in and to approve changes.",
            ];
            for (i, l) in lines.iter().enumerate() {
                center(cv, card.y + 216 + i as i32 * 19, l, theme::ui(13), t.text_secondary);
            }
            for i in 0..5 {
                let r = rect(Hit::Field(i));
                let f = &s.fields[i];
                let focused = s.focus == i;
                let label = if i == 4 { "Hint (optional)" } else { f.label };
                cv.text(r.x, r.y - 8, label, theme::ui_bold(12), t.text_secondary);
                field(cv, r, f, focused, (self.now_ms / 530).is_multiple_of(2));
            }
            // Password strength under the password field.
            let pr = rect(Hit::Field(2));
            let st = strength(&s.fields[2].text) as i32;
            let seg = (pr.w - 12) / 4;
            for i in 0..4 {
                let c = if i < st {
                    if st >= 3 {
                        0xFF3D_DC97
                    } else if st == 2 {
                        0xFFFF_B547
                    } else {
                        0xFFFF_6B6B
                    }
                } else {
                    with_alpha(t.text, 0x22)
                };
                cv.fill_round_rect(Rect::new(pr.x + i * (seg + 4), pr.bottom() + 6, seg, 4), 2, c);
            }
            let al = rect(Hit::AutoLogin);
            cv.fill_round_rect(al, 14, with_alpha(t.text, if t.dark { 0x14 } else { 0x0C }));
            cv.text(al.x + 14, al.y + 21, "Log in automatically", theme::ui_bold(13), t.text);
            cv.text(
                al.x + 14,
                al.y + 39,
                "Skip Shore at startup. Locking still asks for the password.",
                theme::ui(11),
                t.text_secondary,
            );
            crate::gui::widgets::toggle(cv, Rect::new(al.right() - 54, al.y + 14, 42, 22), s.auto_login);
        } else {
            center(cv, card.y + 96, "Choose how it looks", theme::display(22), t.text);
            center(
                cv,
                card.y + 122,
                "Colours follow the wallpaper. You can change both in Settings.",
                theme::ui(13),
                t.text_secondary,
            );
            for dark in [false, true] {
                let r = rect(Hit::Appearance(dark));
                let on = t.dark == dark;
                cv.fill_round_rect(r, 16, if dark { 0xFF11_1527 } else { 0xFFFB_FCFF });
                cv.stroke_round_rect(r, 16, if on { theme::accent() } else { with_alpha(t.text, 0x30) });
                if on {
                    cv.stroke_round_rect(r.inset(-2), 18, theme::accent());
                }
                let ink = if dark { 0xFFF2_F4FA } else { 0xFF0E_1530 };
                cv.text(r.x + 18, r.y + 52, if dark { "Dark" } else { "Light" }, theme::ui_bold(16), ink);
                cv.text(
                    r.x + 18,
                    r.y + 72,
                    if dark { "Navy glass, night pictures" } else { "Paper glass, day pictures" },
                    theme::ui(11),
                    mix(ink, if dark { 0xFF11_1527 } else { 0xFFFB_FCFF }, 90),
                );
            }
            cv.text(card.x + 40, rect(Hit::Wallpaper(0)).y - 12, "Wallpaper", theme::ui_bold(13), t.text);
            let dark = t.dark;
            let s = self.shore.as_mut().unwrap();
            if s.thumbs.is_empty() {
                s.thumbs = (0..wallpaper::NAMES.len() as u8)
                    .map(|i| {
                        wallpaper::file(i, dark)
                            .and_then(|f| crate::fs::read_all(f).ok())
                            .and_then(|d| aurora_image::decode(&d).ok())
                            .map(|img| img.thumbnail(260, 160))
                    })
                    .collect();
            }
            for i in 0..wallpaper::NAMES.len() as u8 {
                let r = rect(Hit::Wallpaper(i));
                match s.thumbs.get(i as usize).and_then(|t| t.as_ref()) {
                    Some(img) => cv.with_clip(r.intersect(&cv.clip), |cv| {
                        cv.fill_round_rect_with(r, 12, |x, y| {
                            let sx = ((x - r.x) * img.width as i32 / r.w.max(1)) as usize;
                            let sy = ((y - r.y) * img.height as i32 / r.h.max(1)) as usize;
                            img.pixels[(sy.min(img.height as usize - 1)) * img.width as usize
                                + sx.min(img.width as usize - 1)]
                        })
                    }),
                    None => cv.fill_round_rect(r, 12, theme::WALLPAPER_TONES[i as usize].fill),
                }
                if theme::wallpaper() == i {
                    cv.stroke_round_rect(r.inset(-3), 15, theme::accent());
                    cv.stroke_round_rect(r.inset(-4), 16, theme::accent());
                }
                let name = wallpaper::NAMES[i as usize];
                cv.text(r.x + 4, r.bottom() + 20, name, theme::ui_bold(12), t.text);
                cv.fill_circle(r.x + r.w - 10, r.bottom() + 15, 5, theme::WALLPAPER_TONES[i as usize].fill);
            }
            let s = self.shore.as_ref().unwrap();
            let back = rect(Hit::Back);
            crate::gui::widgets::button(
                cv,
                back,
                "Back",
                crate::gui::widgets::ButtonStyle::Secondary,
                s.hover == Some(Hit::Back),
            );
        }
        let s = self.shore.as_ref().unwrap();
        let go = rect(Hit::Continue);
        crate::gui::widgets::button(
            cv,
            go,
            if s.step == 0 { "Continue" } else { "Start" },
            crate::gui::widgets::ButtonStyle::Primary,
            s.hover == Some(Hit::Continue),
        );
        // Security note or the error, bottom left.
        let (text, c) = match s.error {
            Some(e) => (e, 0xFFFF_6B6B),
            None if s.step == 0 => ("Stored only as a salted hash, never as text.", t.text_secondary),
            None => ("WaveOS Aurora · by WayV", t.text_secondary),
        };
        let note_x = card.x + 40 + self.shake();
        cv.text_clipped(
            note_x,
            go.y + 22,
            text,
            theme::ui_bold(12),
            c,
            go.x - note_x - 14 - if s.step == 1 { 94 } else { 0 },
        );
        let _ = TOP;
    }
}

/// A text field in the setup card.
fn field(cv: &mut Canvas, r: Rect, f: &Field, focused: bool, caret: bool) {
    let t = theme::current();
    cv.fill_round_rect(r, 11, with_alpha(t.text, if t.dark { 0x12 } else { 0x0B }));
    if focused {
        cv.stroke_round_rect(r.inset(-3), 14, with_alpha(theme::accent(), 0x50));
        cv.stroke_round_rect(r, 11, with_alpha(theme::accent(), 0xC0));
    } else {
        cv.stroke_round_rect(r, 11, with_alpha(t.text, 0x18));
    }
    let font = theme::ui(14);
    let shown: String = if f.secret { f.text.chars().map(|_| '•').collect() } else { f.text.clone() };
    if shown.is_empty() && !focused {
        cv.text(r.x + 12, r.y + 24, f.placeholder, font, t.text_secondary);
    }
    let end = cv.text_clipped(r.x + 12, r.y + 24, &shown, font, t.text, r.w - 24);
    if focused && caret {
        cv.fill_rect(Rect::new(r.x + 12 + end + 1, r.y + 9, 2, 18), theme::accent_glow());
    }
}
