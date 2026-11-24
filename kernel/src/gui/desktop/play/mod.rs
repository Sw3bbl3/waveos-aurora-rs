//! Wave Play: the desktop turned into a console-style layout for the couch,
//! driven by a game controller.
//!
//! Connecting a controller shows a short notice and (unless turned off in
//! Settings → Wave Play) morphs the desktop into Wave Play: the windows
//! recede, the bar and Harbor slide away, the wallpaper blurs into a backdrop
//! lit by the focused tile, and rows of games, apps and settings rise in.
//! Launching zooms the tile into the screen while the program starts, and
//! the program runs full screen; the home button brings up a quick overlay
//! over anything. Going back (the View button, the Desktop tile, or
//! unplugging the last controller) morphs everything back to exactly where
//! it was: Wave Play never moves the desktop's windows, only covers them.
//!
//! - [`library`]: where entries come from (and how to add more);
//! - [`pad`]: controller presses, repeats, and the keyboard stand-in;
//! - [`home`]: the rows, the focus and the backdrop;
//! - [`scene`]: the morph and the launch zoom, from cached layers;
//! - [`overlay`]: the quick overlay; [`glyphs`]: button symbols.
//!
//! All motion is [`lumen::spring`]: interruptible, carrying its velocity.

mod glyphs;
mod home;
mod library;
mod overlay;
mod pad;
mod scene;

/// The keyboard-played controller, for the Linux input tests.
#[cfg(feature = "ktest")]
pub(crate) use pad::Virtual as KeyboardPad;

use super::Desktop;
use crate::drivers::gamepad::{self, Change};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::apps::{self, AppKind};
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::theme;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::pref;
use lumen::spring::{self, Spring};
use lumen::windowing::CHROME_H;
use pad::Button;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Front {
    Home,
    /// A program started from Wave Play, full screen.
    Game(u32),
}

struct Toast {
    title: String,
    detail: String,
    /// Ask for a press to enter (auto-enter off).
    prompt: bool,
    slide: Spring,
    until: u64,
}

/// Frame times for the overlay's readout.
#[derive(Default)]
struct Stats {
    started_ns: u64,
    /// (finished at ms, took µs) over the last second.
    frames: Vec<(u64, u64)>,
    /// When the program in front presented, over the last second.
    presents: Vec<u64>,
    /// The transition being measured: (what, started ms, frames, total µs, slowest µs).
    measuring: Option<(&'static str, u64, u32, u64, u64)>,
}

impl Stats {
    fn measure(&mut self, what: &'static str, now: u64) {
        self.finish(now);
        self.measuring = Some((what, now, 0, 0, 0));
    }

    /// Logs the transition's frames: how many, and how long they took to draw.
    fn finish(&mut self, now: u64) {
        if let Some((what, start, n, total, worst)) = self.measuring.take() {
            log!(
                "play",
                "{}: {} frames in {} ms, drawing {} us mean, {} us slowest",
                what,
                n,
                now.saturating_sub(start),
                total / n.max(1) as u64,
                worst
            );
        }
    }
}

pub struct Play {
    /// The timeline from the desktop (0) to Wave Play (1000).
    morph: Spring,
    /// Where it is heading.
    on: bool,
    /// Entered with (or because of) a controller: leaving when the last goes.
    by_pad: bool,
    layers: Option<scene::Layers>,
    home: home::Home,
    /// Decoded artwork, kept between visits.
    cache: Vec<(library::Art, Arc<library::Picture>)>,
    overlay: Option<overlay::Overlay>,
    toast: Option<Toast>,
    launch: Option<scene::Launch>,
    shrink: Option<scene::Shrink>,
    front: Front,
    nav: pad::Nav,
    virt: pad::Virtual,
    /// Windows shown full screen: (window, where it was, library entry).
    games: Vec<(u32, Rect, usize)>,
    pointer_hidden: bool,
    /// Entering automatically, after the connection notice has been seen.
    enter_at: Option<u64>,
    stats: Stats,
    owner: u64,
    /// The program the owner's window shows (a bridge's window).
    owner_program: Option<u32>,
    /// The boot options were read (`virtual_gamepad=1` connects the keyboard controller).
    booted: bool,
    /// The home's springs were moving at the last step.
    home_moving: bool,
    /// Programs that were full screen, back to windows: hidden until the
    /// morph to the desktop lands (they redraw at their size meanwhile).
    returning: Vec<u32>,
    /// Slow motion for development: (real ms, Wave Play ms, factor) at the last change.
    slow: (u64, u64, u64),
}

impl Play {
    pub fn new() -> Play {
        Play {
            morph: Spring::new(0, spring::SCENE),
            on: false,
            by_pad: false,
            layers: None,
            home: home::Home::new(),
            cache: Vec::new(),
            overlay: None,
            toast: None,
            launch: None,
            shrink: None,
            front: Front::Home,
            nav: pad::Nav::new(),
            virt: pad::Virtual::new(),
            games: Vec::new(),
            pointer_hidden: false,
            enter_at: None,
            stats: Stats::default(),
            owner: gamepad::ANYONE,
            owner_program: None,
            booted: false,
            home_moving: false,
            returning: Vec::new(),
            slow: (0, 0, 1),
        }
    }
}

fn sounds_on() -> bool {
    crate::gui::prefs::get_bool(pref::PLAY_SOUNDS)
}

fn sound(name: &str) {
    if sounds_on() {
        crate::drivers::audio::play_sound(name);
    }
}

impl Desktop {
    fn play_geo(&self) -> home::Geo {
        home::geo(self.w, self.h)
    }

    /// Wave Play (or its overlay) owns the whole screen.
    pub(super) fn play_covers(&self) -> bool {
        self.play.on || self.play.morph.value() > 0 || self.play.overlay.as_ref().is_some_and(|o| !o.closing)
    }

    /// Something is moving that repaints the whole screen.
    pub(super) fn play_full_frames(&self) -> bool {
        let p = &self.play;
        !p.morph.settled()
            || p.launch.is_some()
            || p.shrink.is_some()
            || p.overlay.as_ref().is_some_and(|o| !o.open.settled())
            || p.home.backdrop.moving()
    }

    pub(super) fn play_moving(&self) -> bool {
        let p = &self.play;
        self.play_full_frames() || p.toast.is_some() || p.enter_at.is_some() || (self.play_covers() && p.home_moving)
    }

    /// When Wave Play wants to run next (repeats, the notice, the drift).
    pub(super) fn play_deadline(&self) -> u64 {
        let p = &self.play;
        let mut at = u64::MAX;
        if let Some(r) = p.nav.deadline() {
            at = at.min(r);
        }
        if let Some(e) = p.enter_at {
            at = at.min(self.play_real(e));
        }
        if let Some(t) = &p.toast {
            at = at.min(self.play_real(t.until));
        }
        // The backdrop drifts a pixel at a time: a few times a second is plenty.
        if self.play_covers() && p.front == Front::Home && !theme::reduce_motion() {
            at = at.min(self.now_ms + 120);
        }
        at
    }

    /// Wave Play's clock: real time, or slowed down (Super+Shift+G, for
    /// looking at the motion frame by frame). Continuous across toggles.
    fn play_now(&self) -> u64 {
        let (real, virt, slow) = self.play.slow;
        virt + self.now_ms.saturating_sub(real) / slow.max(1)
    }

    /// The real time at which Wave Play's clock reads `at`.
    fn play_real(&self, at: u64) -> u64 {
        let (real, virt, slow) = self.play.slow;
        real + at.saturating_sub(virt) * slow.max(1)
    }

    pub(super) fn play_pointer_hidden(&self) -> bool {
        self.play_covers() && self.play.pointer_hidden
    }

    // ------------------------------------------------------------ entering

    /// `Command::Play`, the menu and Seek.
    pub(super) fn play_command(&mut self, on: bool) {
        if on {
            self.play_enter(false);
        } else {
            self.play_exit();
        }
    }

    pub(super) fn play_enter(&mut self, by_pad: bool) {
        if self.shore.is_some() || self.play.on {
            return;
        }
        let now = self.play_now();
        self.play.enter_at = None;
        // The desktop's own panels close first.
        self.close_menu();
        self.launcher = None;
        self.shoal_backdrop = None;
        if self.spotlight_open() {
            self.toggle_spotlight();
        }
        if self.center_open() {
            self.toggle_center();
        }
        self.close_current();
        self.switcher = None;
        self.play.overlay = None;
        self.play.on = true;
        self.play.by_pad = by_pad || !gamepad::list().is_empty();
        self.play.front = Front::Home;
        self.play.nav.release();
        self.play.pointer_hidden = true;
        let g = self.play_geo();
        // Coming back before the morph out finished: carry on from there.
        if self.play.morph.value() <= 0 || self.play.layers.is_none() {
            self.play.layers = Some(self.play_capture_layers());
            let Play { home, cache, .. } = &mut self.play;
            home.refresh(cache);
            home.load_some(cache, 6);
            home.settle_backdrop(&g, now);
        }
        if theme::reduce_motion() {
            self.play.morph.snap(1000, now);
        } else {
            self.play.morph.set_target(1000, now);
        }
        sound("play-enter");
        self.play_haptic(90, 40, 70);
        // The connection notice has done its job.
        if let Some(t) = &mut self.play.toast {
            t.until = t.until.min(now + 350);
        }
        self.play.stats.measure("morph into Wave Play", now);
        log!("play", "entering Wave Play");
        self.damage_all();
    }

    pub(super) fn play_exit(&mut self) {
        if !self.play.on {
            return;
        }
        let now = self.play_now();
        let was_settled = self.play.morph.settled();
        self.play.on = false;
        self.play.overlay = None;
        self.play.launch = None;
        self.play.shrink = None;
        self.play.front = Front::Home;
        self.play.enter_at = None;
        // Programs started here become ordinary windows again, where they were.
        let games = core::mem::take(&mut self.play.games);
        for (id, rect, _) in &games {
            if let Some(i) = self.index_of(*id) {
                self.windows[i].rect = *rect;
                self.windows[i].animating = true;
                // Ask for a frame at the window's size now, not when it next shows.
                let c = self.windows[i].content();
                if let Some(client) = self.windows[i].app.client_id() {
                    let ev = aurora_abi::Event {
                        kind: aurora_abi::event::RESIZE,
                        x: c.w,
                        y: c.h,
                        a: crate::gui::scale() as u32,
                        ..Default::default()
                    };
                    crate::gui::server::push_event(client, ev);
                }
                self.play.returning.push(*id);
            }
        }
        self.play.home.running.clear();
        if was_settled || !games.is_empty() || self.play.layers.is_none() {
            self.play.layers = Some(self.play_capture_layers());
        }
        if theme::reduce_motion() {
            self.play.morph.snap(0, now);
        } else {
            self.play.morph.set_target(0, now);
        }
        sound("play-exit");
        self.play.stats.measure("morph back to the desktop", now);
        log!("play", "back to the desktop");
        self.damage_all();
    }

    fn play_haptic(&self, strong: u8, weak: u8, ms: u64) {
        if let Some(id) = self.play.nav.last_pad.filter(|_| sounds_on()) {
            gamepad::rumble_async(id, strong, weak, ms);
        }
    }

    // ------------------------------------------------------------ input

    /// Keys: Super+G (the keyboard controller), Super+Escape (the home
    /// button), and navigation of Wave Play's own screens.
    pub(super) fn play_key(&mut self, k: &KeyEvent) -> bool {
        if k.pressed && k.mods.super_key && k.mods.shift && matches!(k.ch, Some('g') | Some('G')) {
            // Slow motion (×8), to study the transitions.
            self.super_alone = false;
            let v = self.play_now();
            let factor = if self.play.slow.2 == 1 { 8 } else { 1 };
            self.play.slow = (self.now_ms, v, factor);
            log!("play", "slow motion {}", if factor > 1 { "on (x8)" } else { "off" });
            return true;
        }
        if k.pressed && k.mods.super_key && matches!(k.ch, Some('g') | Some('G')) {
            self.super_alone = false;
            self.play.virt.toggle();
            return true;
        }
        if k.pressed && k.mods.super_key && k.code == KeyCode::Escape {
            self.super_alone = false;
            self.play_button(Button::Guide, false);
            return true;
        }
        let capture = self.play_covers() || self.play.toast.as_ref().is_some_and(|t| t.prompt);
        if self.play.virt.key(k, capture) {
            return true;
        }
        let own_screen =
            self.play.overlay.is_some() || (self.play_covers() && self.play.front == Front::Home && self.play.on);
        if !own_screen {
            return false;
        }
        if k.code == KeyCode::Super {
            // No launcher over Wave Play.
            self.super_down = k.pressed;
            return true;
        }
        // Searching: type straight into it.
        if k.pressed
            && self.play.home.search.is_some()
            && self.play.overlay.is_none()
            && !k.mods.ctrl
            && !k.mods.super_key
        {
            let delete = k.code == KeyCode::Backspace;
            let ch = k.ch.filter(|c| c.is_ascii_alphanumeric() || *c == ' ');
            if (delete || ch.is_some()) && self.play.home.search_type(ch, delete, self.play_now()) {
                sound("play-move");
                self.damage_all();
                return true;
            }
        }
        let b = match k.code {
            KeyCode::Up => Button::Up,
            KeyCode::Down => Button::Down,
            KeyCode::Left => Button::Left,
            KeyCode::Right => Button::Right,
            KeyCode::Enter => Button::Accept,
            KeyCode::Escape | KeyCode::Backspace => Button::Back,
            KeyCode::PageUp => Button::Lb,
            KeyCode::PageDown => Button::Rb,
            KeyCode::Tab => Button::Y,
            _ if k.ch == Some(' ') => Button::Accept,
            _ => return !k.mods.super_key && !k.mods.ctrl && !k.mods.alt,
        };
        if k.pressed {
            self.play_button(b, false);
        }
        true
    }

    /// The pointer over Wave Play: shows itself when moved, picks tiles and
    /// overlay rows, and reaches a full-screen program directly.
    pub(super) fn play_pointer(&mut self, x: i32, y: i32, moved: bool, pressed: bool, released: bool) -> bool {
        if !self.play_covers() {
            return false;
        }
        if moved && self.play.pointer_hidden {
            self.play.pointer_hidden = false;
            self.damage_cursor();
        }
        let now = self.play_now();
        let (w, h) = (self.w, self.h);
        if let Some(ov) = &mut self.play.overlay {
            if pressed {
                let p = ov.panel(w, h, 1000);
                let mut accept = false;
                if !p.contains(x, y) {
                    ov.close(now);
                } else {
                    // Rows start below the header (see `Overlay::paint`).
                    let first = p.y + 44 + 26 + 30 + 22 + 14;
                    let row = (y - first).div_euclid(54);
                    if row >= 0 && (row as usize) < ov.visible().len() && (y - first) % 54 < 50 {
                        ov.selected = row as usize;
                        accept = true;
                    }
                }
                if accept {
                    self.play_button(Button::Accept, false);
                }
                self.damage_all();
            }
            return true;
        }
        if let (true, Front::Game(id)) = (self.play.on && self.play.morph.settled(), self.play.front) {
            if self.play.launch.is_none() {
                if pressed {
                    self.with_app(id, |app, area, env| app.click(x, y, area, env));
                } else if released {
                    self.with_app(id, |app, area, _| {
                        app.release(x, y, area);
                        false
                    });
                } else if moved {
                    let held = self.buttons & crate::drivers::input::BUTTON_LEFT != 0;
                    self.with_app(id, |app, area, _| if held { app.drag(x, y, area) } else { app.hover(x, y, area) });
                }
            }
            return true;
        }
        if pressed && self.play.on && self.play.front == Front::Home {
            let g = self.play_geo();
            if let Some((r, c)) = self.play.home.hit(&g, x, y) {
                if (r, c) == (self.play.home.row, self.play.home.col()) {
                    self.play_launch_focused();
                } else {
                    self.play.home.focus_at(&g, r, c, now);
                    sound("play-move");
                    self.damage_all();
                }
            }
        }
        true
    }

    /// Controllers: connections, presses and repeats. Runs every time the
    /// compositor wakes, so a press is acted on at once.
    pub(super) fn play_poll(&mut self) {
        let now = self.play_now();
        let real = self.now_ms;
        if !core::mem::replace(&mut self.play.booted, true)
            && crate::gui::prefs::boot_option("virtual_gamepad").as_deref() == Some("1")
        {
            log!("play", "boot option: connecting the keyboard controller");
            self.play.virt.toggle();
        }
        for change in gamepad::take_changes() {
            let (id, c) = match change {
                Change::Connected(id) => (id, 1),
                Change::Disconnected(id) => (id, 2),
            };
            crate::gui::server::broadcast(aurora_abi::Event {
                kind: aurora_abi::event::GAMEPAD,
                a: id,
                c,
                ..Default::default()
            });
            self.play_pad_change(change, now);
        }
        let moved = gamepad::take_moved();
        if moved {
            self.play_pad_event();
        }
        if moved || self.play.nav.deadline().is_some_and(|at| real >= at) {
            let presses = self.play.nav.poll(real);
            let repeating = self.play.nav.repeating();
            for b in presses {
                if self.play_covers() && !self.play.pointer_hidden {
                    self.play.pointer_hidden = true;
                    self.damage_cursor();
                }
                self.play_button(b, repeating && b.is_direction());
            }
        }
        self.play_update_owner();
    }

    fn play_pad_change(&mut self, change: Change, now: u64) {
        let pads = gamepad::list();
        match change {
            Change::Connected(id) => {
                let name = pads
                    .iter()
                    .find(|p| p.id == id)
                    .map(|p| String::from_utf8_lossy(&p.name[..(p.name_len as usize).min(p.name.len())]).into_owned());
                if let Some(p) = pads.iter().find(|p| p.id == id) {
                    self.play.nav.family = p.family;
                    self.play.nav.last_pad = Some(id);
                }
                let auto = crate::gui::prefs::get_bool(pref::PLAY_AUTO_ENTER);
                let (detail, prompt) = if self.play.on {
                    (name.unwrap_or_else(|| String::from("Ready to play")), false)
                } else if auto && self.shore.is_none() {
                    self.play.enter_at = Some(now + 1100);
                    (String::from("Opening Wave Play…"), false)
                } else {
                    (String::from("Press A or the home button for Wave Play"), true)
                };
                self.play_toast("Controller connected", &detail, prompt, if prompt { 6000 } else { 2600 });
                sound("play-connect");
                gamepad::rumble_async(id, 60, 120, 120);
                // Warm the artwork while the notice shows.
                let Play { home, cache, .. } = &mut self.play;
                if home.entries.is_empty() {
                    home.refresh(cache);
                }
            }
            Change::Disconnected(id) => {
                if self.play.virt.id == Some(id) {
                    self.play.virt.id = None;
                }
                if self.play.nav.last_pad == Some(id) {
                    self.play.nav.last_pad = pads.first().map(|p| p.id);
                }
                if pads.is_empty() {
                    self.play.enter_at = None;
                    self.play_toast("Controller disconnected", "Back to the desktop", false, 2600);
                    if self.play.on && self.play.by_pad {
                        self.play_exit();
                    }
                }
            }
        }
    }

    fn play_toast(&mut self, title: &str, detail: &str, prompt: bool, ms: u64) {
        let now = self.play_now();
        let mut slide = self.play.toast.take().map_or(Spring::new(0, spring::SNAPPY), |t| t.slide);
        slide.set_target(1000, now);
        if theme::reduce_motion() {
            slide.snap(1000, now);
        }
        let r = self.play_toast_rect(1060);
        self.damage(r.inset(-30));
        self.play.toast =
            Some(Toast { title: String::from(title), detail: String::from(detail), prompt, slide, until: now + ms });
    }

    /// Tells the program in front (when it may read the controllers) that one changed.
    fn play_pad_event(&self) {
        let owner = self.play.owner;
        if owner == gamepad::SHELL || owner == gamepad::ANYONE {
            return;
        }
        let front = match self.play.front {
            Front::Game(id) if self.play.on => Some(id),
            _ => self.focused_id(),
        };
        let Some(client) = front.and_then(|id| self.index_of(id)).and_then(|i| self.windows[i].app.client_id()) else {
            return;
        };
        for p in gamepad::list() {
            if let Some(st) = gamepad::state(p.id) {
                let ev = aurora_abi::Event {
                    kind: aurora_abi::event::GAMEPAD,
                    a: p.id,
                    b: st.buttons,
                    ..Default::default()
                };
                crate::gui::server::push_event(client, ev);
            }
        }
    }

    /// Who may read the controllers now.
    fn play_update_owner(&mut self) {
        let client_pid = |d: &Desktop, id: u32| {
            d.index_of(id)
                .and_then(|i| d.windows[i].app.client_id())
                .and_then(crate::gui::server::info)
                .map(|(pid, ..)| pid as u64)
        };
        let owner = if self.shore.is_some() || self.play.overlay.is_some() {
            gamepad::SHELL
        } else if self.play.on {
            match self.play.front {
                Front::Game(id) if self.play.launch.is_none() => client_pid(self, id).unwrap_or(gamepad::SHELL),
                _ => gamepad::SHELL,
            }
        } else {
            self.focused_id().and_then(|id| client_pid(self, id)).unwrap_or(gamepad::ANYONE)
        };
        // The program a bridge's window shows reads along with the bridge.
        let program = |d: &Desktop, id: Option<u32>| {
            id.and_then(|id| d.index_of(id))
                .and_then(|i| d.windows[i].app.client_id())
                .and_then(crate::gui::server::program_of)
        };
        let owner_program = match self.play.front {
            Front::Game(id) if self.play.on => program(self, Some(id)),
            _ if self.play.on => None,
            _ => program(self, self.focused_id()),
        };
        if owner != self.play.owner || owner_program != self.play.owner_program {
            self.play.owner = owner;
            self.play.owner_program = owner_program;
            gamepad::set_owners(owner, owner_program);
        }
        // Keyboards and pointers reach Linux programs through /dev/input
        // only while their window has the focus (not under the lock screen
        // or Wave Play's overlay).
        let keys = if self.shore.is_some() || self.play.overlay.is_some() {
            (None, None)
        } else {
            let id = self.focused_id();
            (id.and_then(|id| client_pid(self, id)).map(|p| p as u32), program(self, id))
        };
        crate::drivers::evdev::set_focus(keys.0, keys.1);
    }

    /// One press of a controller button (or its keyboard stand-in).
    fn play_button(&mut self, b: Button, repeat: bool) {
        if self.shore.is_some() {
            return;
        }
        let now = self.play_now();
        if b == Button::Guide {
            if let Some(ov) = &mut self.play.overlay {
                ov.close(now);
                sound("play-back");
            } else {
                self.play_open_overlay();
            }
            self.damage_all();
            return;
        }
        if self.play.overlay.as_ref().is_some_and(|o| !o.closing) {
            self.play_overlay_button(b);
            return;
        }
        if !self.play.on {
            // Changed your mind half way back to the desktop: View again returns.
            if b == Button::View && !self.play.morph.settled() {
                self.play_enter(true);
                return;
            }
            // On the desktop: the notice's prompt, then the program's keys.
            if b == Button::Accept && self.play.toast.as_ref().is_some_and(|t| t.prompt) {
                self.play.toast = None;
                self.play_enter(true);
                return;
            }
            self.play_keys_for(b);
            return;
        }
        if !self.play.morph.settled() && self.play.morph.target() == 0 {
            return;
        }
        match self.play.front {
            Front::Game(_) => self.play_keys_for(b),
            Front::Home if self.play.launch.is_some() => {}
            Front::Home => self.play_home_button(b, repeat, now),
        }
    }

    fn play_home_button(&mut self, b: Button, repeat: bool, now: u64) {
        let g = self.play_geo();
        if self.play.home.search.is_some() && !matches!(b, Button::Menu | Button::View) {
            match self.play.home.search_button(&g, b, repeat, now) {
                home::Searched::Nothing => return,
                home::Searched::Open => return self.play_launch_focused(),
                home::Searched::Bump => {
                    if !repeat {
                        sound("play-bump");
                    }
                }
                home::Searched::Closed => sound("play-back"),
                home::Searched::Moved | home::Searched::Typed => {
                    sound("play-move");
                    self.play_haptic(0, 40, 14);
                }
            }
            self.damage_all();
            return;
        }
        match b {
            Button::Accept => self.play_launch_focused(),
            Button::View => self.play_exit(),
            Button::Menu => {
                self.play_open_overlay();
                self.damage_all();
            }
            Button::Back => sound("play-bump"),
            Button::Y => {
                self.play.home.open_search(now);
                sound("play-select");
                self.damage_all();
            }
            _ => match self.play.home.go(&g, b, repeat, now) {
                Some(true) => {
                    if !repeat {
                        self.play.stats.measure("focus move", now);
                    }
                    sound("play-move");
                    self.play_haptic(0, 50, 18);
                    let band = self.play.home.row_band(&g);
                    if matches!(b, Button::Up | Button::Down | Button::Lb | Button::Rb) {
                        self.damage_all();
                    } else {
                        self.damage(band);
                        self.damage(Rect::new(0, 0, self.w, band.y + 1));
                    }
                }
                Some(false) => {
                    if !repeat {
                        sound("play-bump");
                    }
                    self.play_haptic(60, 0, 30);
                    self.damage_all();
                }
                None => {}
            },
        }
    }

    /// For programs that don't read controllers themselves: the d-pad, A and
    /// B as arrow keys, Enter and Escape (X Backspace, Y Tab, Menu Escape).
    fn play_keys_for(&mut self, b: Button) {
        let Some(id) = self.focused_id() else { return };
        let Some(i) = self.index_of(id) else { return };
        if let Some(pid) = self.windows[i].app.client_id().and_then(crate::gui::server::info).map(|(p, ..)| p) {
            if gamepad::polled_recently(pid, self.now_ms) {
                return;
            }
        }
        let (code, ch) = match b {
            Button::Up => (KeyCode::Up, None),
            Button::Down => (KeyCode::Down, None),
            Button::Left => (KeyCode::Left, None),
            Button::Right => (KeyCode::Right, None),
            Button::Accept => (KeyCode::Enter, Some('\n')),
            Button::Back | Button::Menu => (KeyCode::Escape, None),
            Button::X => (KeyCode::Backspace, None),
            Button::Y => (KeyCode::Tab, Some('\t')),
            _ => return,
        };
        for pressed in [true, false] {
            let ev = KeyEvent { code, ch, pressed, mods: Default::default() };
            self.with_app(id, |app, _, env| app.key(&ev, env));
        }
    }

    // ------------------------------------------------------------ launching

    fn play_launch_focused(&mut self) {
        let Some(e) = self.play.home.focused() else { return };
        let now = self.play_now();
        let g = self.play_geo();
        let entry = self.play.home.entries[e].clone();
        let from = self.play.home.focused_tile(&g);
        sound("play-select");
        self.play_haptic(140, 80, 90);
        // Already running: zoom back into it.
        if let Some(win) = self.play.home.is_running(e) {
            let mut l = scene::Launch::new(e, from, now);
            l.window = Some(win);
            self.play.launch = Some(l);
            self.play.front = Front::Game(win);
            self.raise(win);
            self.damage_all();
            return;
        }
        match &entry.launch {
            library::Launch::Desktop => {
                self.play_exit();
                return;
            }
            library::Launch::App(kind) => {
                let info = apps::info(*kind);
                let existing = self.windows.iter().rev().find(|w| w.app.kind() == *kind).map(|w| w.id);
                match existing.filter(|_| info.single) {
                    Some(id) => {
                        self.play.launch = Some(scene::Launch::new(e, from, now));
                        self.raise(id);
                        self.play_adopt(id);
                    }
                    None => {
                        self.play.launch = Some(scene::Launch::new(e, from, now));
                        self.open(*kind);
                    }
                }
            }
            library::Launch::Program(path, args) => {
                let args: Vec<&str> = args.iter().map(String::as_str).collect();
                self.play.launch = Some(scene::Launch::new(e, from, now));
                self.spawn_program(path, &args);
            }
            library::Launch::Settings(pane) => {
                self.play.launch = Some(scene::Launch::new(e, from, now));
                let settings = apps::info(AppKind::Settings).path;
                self.spawn_program(settings, &[pane]);
            }
        }
        library::remember(&entry.id);
        self.play.stats.measure("launch zoom", now);
        if theme::reduce_motion() {
            if let Some(l) = &mut self.play.launch {
                l.zoom.snap(1000, now);
            }
        }
        log!("play", "starting {}", entry.title);
        self.damage_all();
    }

    /// The whole screen, for a window's content (its chrome strip above it).
    fn play_fullscreen(&self) -> Rect {
        Rect::new(0, -CHROME_H, self.w, self.h + CHROME_H)
    }

    /// A window opened: if it is what Wave Play is starting (or belongs to
    /// the program in front), it is shown full screen.
    pub(super) fn play_adopt(&mut self, id: u32) {
        if !self.play.on || self.play.games.iter().any(|g| g.0 == id) {
            return;
        }
        let Some(i) = self.index_of(id) else { return };
        if self.windows[i].app.client_id().is_none() {
            return;
        }
        let now = self.play_now();
        let launching =
            self.play.launch.as_ref().filter(|l| l.window.is_none() && now < l.started + 20_000).map(|l| l.entry);
        let current = match self.play.front {
            Front::Game(g) => self.play.games.iter().find(|x| x.0 == g).map(|x| x.2),
            Front::Home => None,
        };
        let Some(entry) = launching.or(current) else { return };
        let rect = self.windows[i].rect;
        self.windows[i].rect = self.play_fullscreen();
        self.play.games.push((id, rect, entry));
        if let Some(l) = &mut self.play.launch {
            if l.window.is_none() {
                l.window = Some(id);
            }
        }
        self.play.front = Front::Game(id);
        self.play.home.running.push((entry, id));
        self.play.home.rebuild(None);
        log!("play", "showing window {} full screen", id);
        self.damage_all();
    }

    /// A window is closing (before it goes): a program shown here ends.
    pub(super) fn play_window_closing(&mut self, id: u32) {
        let Some(pos) = self.play.games.iter().position(|g| g.0 == id) else { return };
        let (_, _, entry) = self.play.games.remove(pos);
        self.play.home.running.retain(|r| r.1 != id);
        if self.play.launch.as_ref().is_some_and(|l| l.window == Some(id)) {
            self.play.launch = None;
        }
        if self.play.front == Front::Game(id) {
            // Another window of the same program (a game's launcher) takes over.
            if let Some(&(other, ..)) = self.play.games.iter().rev().find(|g| g.2 == entry) {
                self.play.front = Front::Game(other);
            } else {
                self.play.front = Front::Home;
                if self.play.shrink.is_none() {
                    self.play_shrink(Some(entry), self.scene.clone());
                }
            }
        }
        self.play.home.rebuild(None);
        self.damage_all();
    }

    fn play_shrink(&mut self, entry: Option<usize>, picture: Vec<u32>) {
        let now = self.play_now();
        let k = crate::gui::scale();
        if picture.len() != (self.w * k * self.h * k) as usize || theme::reduce_motion() {
            self.play.shrink = None;
            return;
        }
        let mut zoom = Spring::new(1000, spring::SCENE);
        zoom.set_target(0, now);
        self.play.shrink = Some(scene::Shrink { entry, picture, zoom });
        self.play.stats.measure("back home (shrink into the tile)", now);
    }

    /// Records a client's present; true when it shouldn't repaint (frozen
    /// under the overlay).
    /// The program in front, full screen with nothing of Wave Play over it
    /// (its buffer may be scanned out: see `Desktop::scanout_window`).
    pub(super) fn play_scanout(&self) -> Option<u32> {
        let p = &self.play;
        let still = p.on && p.morph.settled() && p.morph.value() >= 1000;
        if !still || p.overlay.is_some() || p.toast.is_some() || p.launch.is_some() || p.shrink.is_some() {
            return None;
        }
        let Front::Game(id) = p.front else { return None };
        let i = self.index_of(id)?;
        let w = &self.windows[i];
        (w.content() == self.screen()).then(|| w.app.client_id()).flatten()
    }

    pub(super) fn play_presented(&mut self, client: u32) -> bool {
        if let Front::Game(id) = self.play.front {
            if self.index_of(id).and_then(|i| self.windows[i].app.client_id()) == Some(client) {
                self.play.stats.presents.push(self.now_ms);
                let cut = self.now_ms.saturating_sub(1000);
                self.play.stats.presents.retain(|&t| t >= cut);
            }
        }
        self.play.overlay.is_some() || (self.play_covers() && self.play.front == Front::Home)
    }

    // ------------------------------------------------------------ overlay

    fn play_open_overlay(&mut self) {
        let now = self.play_now();
        let k = crate::gui::scale();
        let (pw, ph) = (self.w * k, self.h * k);
        let mut items = alloc::vec![overlay::Item::Resume];
        let (title, subtitle);
        match (self.play.on, self.play.front) {
            (true, Front::Game(id)) => {
                let name = self.index_of(id).map(|i| self.windows[i].app.title()).unwrap_or_default();
                items.push(overlay::Item::Home);
                items.push(overlay::Item::Close(id, name.clone()));
                title = name;
                subtitle = String::from("Playing in Wave Play");
            }
            (true, Front::Home) => {
                title = String::from("Wave Play");
                subtitle = String::from("Home");
            }
            (false, _) => {
                items.push(overlay::Item::Home);
                if let Some(id) = self.focused_id() {
                    let name = self.index_of(id).map(|i| self.windows[i].app.title()).unwrap_or_default();
                    items.push(overlay::Item::Close(id, name));
                }
                title = String::from("Desktop");
                subtitle = self.focused().map(|w| w.app.title()).unwrap_or_else(|| String::from("No window open"));
            }
        }
        items.push(overlay::Item::Volume);
        if self.play.on {
            items.push(overlay::Item::Desktop);
        }
        items.push(overlay::Item::Power);
        let sharp = if self.scene.len() == (pw * ph) as usize { self.scene.clone() } else { Vec::new() };
        let mut ov = overlay::Overlay::new(sharp, pw, ph, items, title, subtitle, now);
        if theme::reduce_motion() {
            ov.open.snap(1000, now);
        }
        self.play.overlay = Some(ov);
        self.play.stats.measure("quick overlay opening", now);
        self.play.nav.release();
        sound("play-select");
        self.play_haptic(0, 70, 30);
        self.damage_all();
    }

    fn play_overlay_button(&mut self, b: Button) {
        let now = self.play_now();
        let Some(ov) = &mut self.play.overlay else { return };
        let action = ov.press(b, now);
        let selected_volume = ov.visible().get(ov.selected) == Some(&overlay::Item::Volume);
        match action {
            overlay::Action::Nothing => return,
            overlay::Action::Moved => sound("play-move"),
            overlay::Action::Close => sound("play-back"),
            overlay::Action::Run(item) => {
                sound(if item == overlay::Item::Volume { "volume" } else { "play-select" });
                self.play_overlay_run(item, b, selected_volume);
            }
        }
        // The panel alone (its height follows the rows), unless the overlay moves.
        let (w, h) = (self.w, self.h);
        let r = self.play.overlay.as_ref().map(|o| o.panel(w, h, 1000));
        match r {
            Some(r) if self.play.overlay.as_ref().is_some_and(|o| o.open.settled()) => {
                self.damage(Rect::new(r.x, r.y, r.w, h - r.y).inset(-50))
            }
            _ => self.damage_all(),
        }
    }

    fn play_overlay_run(&mut self, item: overlay::Item, b: Button, _volume: bool) {
        use overlay::Item;
        let now = self.play_now();
        let close = |d: &mut Desktop| {
            if let Some(o) = &mut d.play.overlay {
                o.close(now);
            }
        };
        match item {
            Item::Resume => close(self),
            Item::Volume => {
                let s = crate::drivers::audio::state();
                let (vol, muted) = ((s & 0xFF) as i32, s & aurora_abi::audio::MUTED != 0);
                let step = match b {
                    Button::Left => -5,
                    Button::Right => 5,
                    _ => 0,
                };
                crate::gui::prefs::set_volume((vol + step).clamp(0, 100) as u32, muted && step <= 0);
            }
            Item::Home => {
                if self.play.on {
                    let picture = self.play.overlay.as_ref().map(|o| o.sharp.clone()).unwrap_or_default();
                    let entry = match self.play.front {
                        Front::Game(id) => self.play.games.iter().find(|g| g.0 == id).map(|g| g.2),
                        Front::Home => None,
                    };
                    self.play.front = Front::Home;
                    self.play.launch = None;
                    self.play_shrink(entry, picture);
                    self.play.home.rebuild(None);
                    close(self);
                } else {
                    self.play.overlay = None;
                    self.play_enter(true);
                }
            }
            Item::Close(id, _) => {
                let picture = self.play.overlay.as_ref().map(|o| o.sharp.clone()).unwrap_or_default();
                if self.play.on && self.play.front == Front::Game(id) {
                    let entry = self.play.games.iter().find(|g| g.0 == id).map(|g| g.2);
                    self.play.front = Front::Home;
                    self.play_shrink(entry, picture);
                }
                close(self);
                self.request_close(id);
            }
            Item::Desktop => {
                self.play.overlay = None;
                self.play_exit();
            }
            Item::Sleep => {
                self.play.overlay = None;
                crate::power::sleep();
            }
            Item::Restart => self.power = Some(super::PowerAction::Reboot),
            Item::ShutDown => self.power = Some(super::PowerAction::Shutdown),
            Item::Power | Item::Cancel => {}
        }
    }

    fn play_status(&self) -> overlay::Status {
        let now = self.now_ms;
        let recent: Vec<&(u64, u64)> = self.play.stats.frames.iter().filter(|f| f.0 + 1000 >= now).collect();
        let n = recent.len() as u64;
        let s = crate::drivers::audio::state();
        overlay::Status {
            clock: super::bar::time_text(),
            battery: super::bar::battery().map(|b| (b.percent, b.flags & aurora_abi::power::CHARGING != 0)),
            controller: gamepad::list()
                .first()
                .map(|p| String::from_utf8_lossy(&p.name[..(p.name_len as usize).min(p.name.len())]).into_owned()),
            volume: ((s & 0xFF) as u32, s & aurora_abi::audio::MUTED != 0),
            fps: n as u32,
            frame_us: recent.iter().map(|f| f.1).sum::<u64>() / n.max(1),
            worst_us: recent.iter().map(|f| f.1).max().unwrap_or(0),
            game_fps: match self.play.front {
                Front::Game(_) if self.play.on => {
                    Some(self.play.stats.presents.iter().filter(|&&t| t + 1000 >= now).count() as u32)
                }
                _ => None,
            },
        }
    }

    /// A frame is about to be drawn (`take_damage`) …
    pub(super) fn play_frame_begin(&mut self, drawing: bool) {
        self.play.stats.started_ns = if drawing { crate::time::now_ns() } else { 0 };
    }

    /// … and has reached the screen (`finish_frame`).
    pub(super) fn play_frame_end(&mut self) {
        let start = core::mem::take(&mut self.play.stats.started_ns);
        if start == 0 {
            return;
        }
        let took = crate::time::now_ns().saturating_sub(start) / 1000;
        let now = self.now_ms;
        let frames = &mut self.play.stats.frames;
        frames.push((now, took));
        if frames.len() > 240 || frames.first().is_some_and(|f| f.0 + 1500 < now) {
            frames.retain(|f| f.0 + 1000 >= now);
        }
        if let Some(m) = &mut self.play.stats.measuring {
            m.2 += 1;
            m.3 += took;
            m.4 = m.4.max(took);
        }
    }

    // ------------------------------------------------------------ time

    /// Advances Wave Play; true while something moves.
    pub(super) fn step_play(&mut self) -> bool {
        let now = self.play_now();
        let reduced = theme::reduce_motion();
        let g = self.play_geo();
        let mut busy = false;
        if let Some(at) = self.play.enter_at {
            if now >= at {
                self.play.enter_at = None;
                self.play_enter(true);
            }
        }
        // The morph.
        if reduced {
            let t = self.play.morph.target();
            self.play.morph.snap(t, now);
        }
        let was = self.play.morph.settled();
        if self.play.morph.advance(now) || !was {
            self.damage_all();
            busy = true;
        }
        if self.play.morph.settled() && self.play.layers.is_some() {
            // Arrived: the layers are no longer needed.
            self.play.layers = None;
            self.play.stats.finish(now);
            if !self.play.on {
                // The programs that were full screen land back on the desktop.
                for id in core::mem::take(&mut self.play.returning) {
                    if let Some(i) = self.index_of(id) {
                        self.windows[i].animating = false;
                        self.animate_open(id);
                    }
                }
            }
            log!("play", "{} settled", if self.play.on { "Wave Play" } else { "desktop" });
            self.damage_all();
        }
        if !self.play_covers() {
            // An overlay may still be sliding away over the desktop.
            busy |= self.play_step_overlay(now, reduced);
            self.play_step_toast(now);
            return busy || self.play.toast.is_some() || self.play.enter_at.is_some();
        }
        // Artwork, a few at a time.
        {
            let Play { home, cache, .. } = &mut self.play;
            if home.load_some(cache, 2) {
                self.damage_all();
            }
        }
        // The home.
        // The backdrop drifts only while nothing else moves, and only on
        // machines that draw a frame quickly (it is decoration, not news).
        let slow = self.play.stats.frames.last().is_some_and(|f| f.1 > 25_000);
        let still = !self.play.morph.settled() || self.play.launch.is_some() || self.play.front != Front::Home || slow;
        let (moving, full) = self.play.home.step(&g, now, reduced, still);
        self.play.home_moving = moving;
        if self.play.front == Front::Home && self.play.overlay.is_none() {
            if full {
                self.damage_all();
            } else if moving {
                let band = self.play.home.row_band(&g);
                self.damage(band);
                self.damage(Rect::new(0, 0, self.w, band.y + 1));
            }
        }
        busy |= moving;
        if !moving && self.play.stats.measuring.is_some_and(|m| m.0 == "focus move") {
            self.play.stats.finish(now);
        }
        // Launching: zoom, then hand over to the window once it has drawn.
        if let Some(mut l) = self.play.launch.take() {
            if reduced {
                let (z, r) = (l.zoom.target(), l.reveal.target());
                l.zoom.snap(z, now);
                l.reveal.snap(r, now);
            }
            l.zoom.advance(now);
            l.reveal.advance(now);
            let ready =
                l.window.and_then(|w| self.index_of(w)).is_some_and(|i| self.windows[i].awaiting_frame.is_none());
            if ready && l.zoom.value() >= 990 && l.reveal.target() == 0 {
                l.reveal.set_target(1000, now);
            }
            let done = l.reveal.settled() && l.reveal.value() >= 1000;
            let failed = l.window.is_none() && now > l.started + 15_000;
            if failed && l.zoom.target() == 1000 {
                log!("play", "nothing appeared; back home");
                l.zoom.set_target(0, now);
            }
            if !(done || (failed && l.zoom.settled())) {
                self.play.launch = Some(l);
            } else {
                self.play.stats.finish(now);
            }
            self.damage_all();
            busy = true;
        }
        if let Some(s) = &mut self.play.shrink {
            if reduced || !s.zoom.advance(now) {
                self.play.shrink = None;
                self.play.stats.finish(now);
            }
            self.damage_all();
            busy = true;
        }
        busy |= self.play_step_overlay(now, reduced);
        self.play_step_toast(now);
        busy || self.play.toast.is_some()
    }

    /// The quick overlay sliding in or out; true while it moves.
    fn play_step_overlay(&mut self, now: u64, reduced: bool) -> bool {
        let Some(ov) = &mut self.play.overlay else { return false };
        if reduced {
            let t = ov.open.target();
            ov.open.snap(t, now);
        }
        if ov.open.advance(now) {
            self.damage_all();
            return true;
        }
        if ov.closing {
            self.play.overlay = None;
            self.damage_all();
        } else if self.play.stats.measuring.is_some_and(|m| m.0.starts_with("quick")) {
            self.play.stats.finish(now);
        }
        false
    }

    fn play_step_toast(&mut self, now: u64) {
        let Some(t) = &mut self.play.toast else { return };
        if now >= t.until && t.slide.target() == 1000 {
            t.slide.set_target(0, now);
        }
        if theme::reduce_motion() {
            let v = t.slide.target();
            t.slide.snap(v, now);
        }
        let moving = t.slide.advance(now);
        let gone = !moving && t.slide.value() <= 0;
        let r = self.play_toast_rect(1060);
        self.damage(r.inset(-30));
        if gone {
            self.play.toast = None;
        }
    }

    /// Resolution or scale changed.
    pub(super) fn play_resized(&mut self) {
        let now = self.play_now();
        self.play.layers = None;
        self.play.overlay = None;
        self.play.launch = None;
        self.play.shrink = None;
        let t = self.play.morph.target();
        self.play.morph.snap(t, now);
        let full = self.play_fullscreen();
        for (id, ..) in self.play.games.clone() {
            if let Some(i) = self.index_of(id) {
                self.windows[i].rect = full;
            }
        }
        let g = self.play_geo();
        self.play.home.settle_backdrop(&g, now);
    }

    // ------------------------------------------------------------ painting

    fn play_toast_rect(&self, t: i32) -> Rect {
        let (w, h) = (420.min(self.w - 32), 64);
        let top = if self.play_covers() { 22 } else { theme::MENUBAR_H + 10 };
        Rect::new((self.w - w) / 2, spring::lerp(-h - 24, top, t), w, h)
    }

    /// An overlay sliding away, over whatever is live under it.
    fn paint_play_closing(&self, cv: &mut Canvas) {
        if let Some(ov) = self.play.overlay.as_ref().filter(|o| o.closing) {
            let status = self.play_status();
            let family = self.play.nav.family;
            crate::gui::stages::time(crate::gui::stages::Stage::Play, || {
                ov.paint(cv, self.w, self.h, family, &status, false)
            });
        }
    }

    /// Over the desktop: an overlay sliding away, and the connection notice.
    pub(super) fn paint_play_over_desktop(&self, cv: &mut Canvas) {
        self.paint_play_closing(cv);
        self.paint_play_toast(cv);
    }

    /// The connection notice (over the desktop or Wave Play).
    fn paint_play_toast(&self, cv: &mut Canvas) {
        let Some(t) = &self.play.toast else { return };
        let r = self.play_toast_rect(t.slide.value());
        if r.bottom() <= 0 || !r.inset(-30).intersects(&cv.clip) {
            return;
        }
        let th = theme::current();
        cv.shadow(r, r.h / 2, 24, 8, th.shadow / 2);
        cv.fill_round_rect(r, r.h / 2, 0xF014_181F);
        cv.stroke_round_rect(r, r.h / 2, 0x29FF_FFFF);
        glyphs::controller(cv, r.x + 38, r.y + r.h / 2 + 2, 30, 0xFFFF_FFFF);
        let right = if t.prompt { 60 } else { 24 };
        cv.text_clipped(r.x + 70, r.y + 28, &t.title, theme::ui_bold(15), 0xFFFF_FFFF, r.w - 70 - right);
        cv.text_clipped(r.x + 70, r.y + 48, &t.detail, theme::ui(13), 0xBFFF_FFFF, r.w - 70 - right);
        if t.prompt {
            glyphs::button(
                cv,
                self.play.nav.family,
                Button::Accept,
                r.right() - 36,
                r.y + r.h / 2,
                28,
                0xFF0E_1116,
                0xEBFF_FFFF,
            );
        }
    }

    /// Paints Wave Play when it owns the screen; false to paint the desktop.
    pub(super) fn play_paint(&mut self, cv: &mut Canvas) -> bool {
        if !self.play_covers() {
            return false;
        }
        let g = self.play_geo();
        let family = self.play.nav.family;
        let p = self.play.morph.value();
        use crate::gui::stages::{time as staged, Stage};
        // An overlay sliding in covers a still of the screen; sliding out,
        // it uncovers what is live (going home shrinks the game meanwhile).
        let overlay_still = self.play.overlay.as_ref().is_some_and(|o| !o.closing);
        if overlay_still {
            let status = self.play_status();
            if let Some(ov) = &self.play.overlay {
                staged(Stage::Play, || ov.paint(cv, self.w, self.h, family, &status, true));
            }
        } else if !self.play.morph.settled() || p < 1000 {
            match &self.play.layers {
                Some(layers) => self.play_paint_morph(cv, layers, &g, p),
                None => staged(Stage::Play, || self.play.home.paint(cv, &g, family, Some(p), true)),
            }
        } else {
            self.play_paint_front(cv, &g, family);
        }
        if !overlay_still {
            self.paint_play_closing(cv);
        }
        self.paint_play_toast(cv);
        true
    }

    fn play_paint_front(&mut self, cv: &mut Canvas, g: &home::Geo, family: u32) {
        let launch = self.play.launch.as_ref().map(|l| (l.entry, l.from, l.zoom.value(), l.reveal.value()));
        let covered = launch.is_some_and(|(_, _, z, r)| z >= 1000 && r <= 0);
        match self.play.front {
            Front::Game(id) if !covered && launch.is_none_or(|(_, _, z, _)| z >= 1000) => {
                cv.fill_rect(cv.clip, 0xFF00_0000);
                if let Some(i) = self.index_of(id) {
                    let content = self.windows[i].content();
                    let env = self.env(true);
                    use crate::gui::stages::{time as staged, Stage};
                    let w = &mut self.windows[i];
                    staged(Stage::App, || cv.with_clip(content, |cv| w.app.draw(cv, content, &env)));
                }
            }
            _ if !covered => crate::gui::stages::time(crate::gui::stages::Stage::Play, || {
                self.play.home.paint(cv, g, family, None, true)
            }),
            _ => {}
        }
        if let Some((entry, from, z, reveal)) = launch {
            let opacity = (255 * (1000 - reveal.clamp(0, 1000)) / 1000) as u32;
            if opacity > 0 {
                self.play_paint_card(cv, g, entry, from, z, opacity);
            }
        }
        if let Some(s) = &self.play.shrink {
            let to =
                s.entry.and_then(|e| self.play.home.tile_of(g, e)).unwrap_or_else(|| self.play.home.focused_tile(g));
            self.play_paint_shrink(cv, g, &s.picture, to, s.zoom.value());
        }
    }
}

/// Kernel self-test: the library's parsers, the keyboard controller driving
/// navigation through the gamepad layer (presses, repeats), input ownership
/// and hot-plug reports, and springs in the soft-float kernel.
#[cfg(feature = "ktest")]
pub fn self_test() {
    assert!(library::parse_for_test(), "library parsers");
    let key = |code, pressed| KeyEvent { code, ch: None, pressed, mods: Default::default() };
    let mut v = pad::Virtual::new();
    v.toggle();
    let id = v.id.expect("keyboard controller connected");
    let mut nav = pad::Nav::new();
    assert!(nav.poll(0).is_empty(), "a new controller's state is not a press");
    assert!(!v.key(&key(KeyCode::Right, true), false), "the desktop keeps its keys");
    assert!(v.key(&key(KeyCode::Right, true), true));
    assert_eq!(nav.poll(10), alloc::vec![Button::Right]);
    assert!(nav.poll(100).is_empty(), "no repeat before the delay");
    assert_eq!(nav.poll(400), alloc::vec![Button::Right], "repeats while held");
    assert!(nav.repeating());
    assert!(nav.poll(450).is_empty());
    assert_eq!(nav.poll(560), alloc::vec![Button::Right]);
    assert!(v.key(&key(KeyCode::Right, false), false), "a release reaches a held button");
    assert!(nav.poll(900).is_empty(), "released: no more repeats");
    assert!(nav.deadline().is_none());
    assert!(v.key(&key(KeyCode::Enter, true), true));
    assert_eq!(nav.poll(1000), alloc::vec![Button::Accept]);
    // A tap shorter than a frame (pressed and let go between looks) counts, without repeating.
    assert!(v.key(&key(KeyCode::Down, true), true) && v.key(&key(KeyCode::Down, false), true));
    assert_eq!(nav.poll(1100), alloc::vec![Button::Down]);
    assert!(nav.deadline().is_none());
    // While the shell has the controllers, programs read nothing pressed.
    gamepad::set_owners(gamepad::SHELL, None);
    assert_eq!(gamepad::state_for(9999, id).map(|s| s.buttons), Some(0));
    gamepad::set_owners(9999, None);
    assert_eq!(gamepad::state_for(9999, id).map(|s| s.buttons), Some(aurora_abi::gamepad::SOUTH));
    assert!(gamepad::polled_recently(9999, crate::time::uptime_ms()));
    gamepad::set_owners(gamepad::ANYONE, None);
    v.toggle();
    assert!(gamepad::state(id).is_none(), "disconnected");
    let changes = gamepad::take_changes();
    assert!(changes.contains(&Change::Connected(id)) && changes.contains(&Change::Disconnected(id)));
    let mut s = Spring::new(0, spring::SCENE);
    s.set_target(1000, 0);
    assert!(s.advance(200) && s.value() > 300 && s.value() < 1000, "{}", s.value());
    s.set_target(0, 200);
    assert!(!s.advance(5000) && s.value() == 0);

    // The SSE2 row blend the transitions use agrees with the scalar one, in
    // a task that may use SIMD registers (as the compositor does).
    use core::sync::atomic::{AtomicU8, Ordering};
    static SIMD: AtomicU8 = AtomicU8::new(0);
    lumen::simd::set_gate(crate::gui::simd_allowed);
    crate::sched::spawn_simd("play-simd-check", || {
        let a: Vec<u32> = (0..67u32).map(|i| 0xFF00_0000 | i.wrapping_mul(0x0103_0507) & 0xFF_FFFF).collect();
        let b: Vec<u32> = (0..67u32).map(|i| 0xFF00_0000 | i.wrapping_mul(0x0709_0B0D) & 0xFF_FFFF).collect();
        let mut ok = lumen::simd::enabled();
        for t in [0u32, 1, 77, 128, 200, 255, 256] {
            let mut d = alloc::vec![0u32; 67];
            // SAFETY: SIMD is enabled in this task (checked above); 67 pixels each.
            let n =
                if ok { unsafe { lumen::simd::mix_rows(d.as_mut_ptr(), a.as_ptr(), b.as_ptr(), 67, t) } } else { 0 };
            ok &= n == 64;
            for i in 0..n {
                ok &= d[i] == crate::gui::canvas::mix(a[i], b[i], t as i32) | 0xFF00_0000;
            }
        }
        SIMD.store(if ok { 1 } else { 2 }, Ordering::Release);
    });
    for _ in 0..200 {
        if SIMD.load(Ordering::Acquire) != 0 {
            break;
        }
        crate::sched::sleep_ms(10);
    }
    assert_eq!(SIMD.load(Ordering::Acquire), 1, "SSE2 mix_rows matches the scalar blend");
}
