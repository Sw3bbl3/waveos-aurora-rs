//! Touchpad gestures: from frames of contacts to pointer motion, clicks
//! and scrolling, the way people expect a laptop's trackpad to behave.
//!
//! - One finger moves the pointer, faster the faster it moves.
//! - Tapping clicks (with tap to click on): one finger is a click, two a
//!   secondary click, three a middle click. Tapping and then touching
//!   again within a moment drags, until that finger lifts.
//! - Pressing the pad clicks; pressing with two fingers on it is a
//!   secondary click.
//! - Two fingers scroll, in either direction, and the content keeps
//!   gliding briefly after a quick flick.
//!
//! Integer arithmetic throughout (the kernel has no floating point):
//! distances in micrometres, motion in 1/65536 pixels.

use crate::touchpad::{Contact, Frame, Info};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    pub tap_to_click: bool,
    /// Content follows the fingers (as on a touch screen).
    pub natural_scrolling: bool,
    /// Tracking speed, 1 (slow) to 10 (fast).
    pub speed: u8,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings { tap_to_click: true, natural_scrolling: true, speed: 5 }
    }
}

/// What a frame (or a tick) changed. Scrolling is in pixels, positive
/// meaning the view moves down or right (as a mouse wheel turned down).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Output {
    pub dx: i32,
    pub dy: i32,
    /// Buttons held: 1 primary, 2 secondary, 4 middle.
    pub buttons: u8,
    pub scroll_x: i32,
    pub scroll_y: i32,
}

/// The longest touch that still counts as a tap, and how far it may move.
const TAP_MS: u64 = 200;
const TAP_SLOP_UM: i64 = 1_500;
/// How long a tap's click lasts, and how soon touching again starts a drag.
const TAP_CLICK_MS: u64 = 60;
const TAP_DRAG_MS: u64 = 220;
/// Assumed size of a pad that doesn't say (a common laptop pad).
const DEFAULT_WIDTH_UM: i64 = 100_000;
const DEFAULT_HEIGHT_UM: i64 = 65_000;
/// Fixed point: 1/65536 pixel.
const ONE: i64 = 65536;
/// Momentum: per-tick decay (out of 256) and the speed below which it stops.
const GLIDE_DECAY: i64 = 240;
const GLIDE_MIN: i64 = 40 * ONE; // 40 px/s
const TICK_MS: u64 = 16;

#[derive(Clone, Copy, Debug)]
struct Track {
    id: u32,
    x: i64,
    y: i64,
    start_x: i64,
    start_y: i64,
}

pub struct Gestures {
    x_min: i64,
    y_min: i64,
    /// Micrometres per device unit, as a fraction num/den.
    um_x: (i64, i64),
    um_y: (i64, i64),
    fingers: Vec<Track>,
    /// The current touch: when it began, the most fingers it had, how far
    /// any finger strayed, and whether it scrolled.
    touch_start: u64,
    touch_max: usize,
    touch_moved: i64,
    touch_scrolled: bool,
    last_frame: u64,
    /// Sub-pixel remainders.
    rem: (i64, i64),
    scroll_rem: (i64, i64),
    /// Buttons from the pad's own switch and from taps.
    pressed: u8,
    tap: u8,
    /// When a tap's click lets go.
    tap_release: Option<u64>,
    /// Until when touching again turns a one-finger tap into a drag.
    drag_until: Option<u64>,
    dragging: bool,
    /// Scroll velocity (fixed-point px per second) for gliding after a flick.
    velocity: (i64, i64),
    gliding: bool,
}

impl Gestures {
    pub fn new(info: &Info) -> Gestures {
        let span_x = (info.x_max as i64 - info.x_min as i64).max(1);
        let span_y = (info.y_max as i64 - info.y_min as i64).max(1);
        let width = info.width_hmm.map(|w| w as i64 * 10).unwrap_or(DEFAULT_WIDTH_UM);
        let height = info.height_hmm.map(|h| h as i64 * 10).unwrap_or(DEFAULT_HEIGHT_UM);
        Gestures {
            x_min: info.x_min as i64,
            y_min: info.y_min as i64,
            um_x: (width, span_x),
            um_y: (height, span_y),
            fingers: Vec::new(),
            touch_start: 0,
            touch_max: 0,
            touch_moved: 0,
            touch_scrolled: false,
            last_frame: 0,
            rem: (0, 0),
            scroll_rem: (0, 0),
            pressed: 0,
            tap: 0,
            tap_release: None,
            drag_until: None,
            dragging: false,
            velocity: (0, 0),
            gliding: false,
        }
    }

    fn um(&self, c: &Contact) -> (i64, i64) {
        ((c.x as i64 - self.x_min) * self.um_x.0 / self.um_x.1, (c.y as i64 - self.y_min) * self.um_y.0 / self.um_y.1)
    }

    fn buttons(&self) -> u8 {
        self.pressed | self.tap
    }

    /// Pointer gain (fixed-point px per micrometre) for a finger moving at
    /// `speed_um_ms` (= mm/s).
    fn gain(speed_um_ms: i64, settings: &Settings) -> i64 {
        // 1.4–5 px per mm when slow (speed 1–10, 3 px at 5), up to 4× that
        // for a quick swipe (from 160 mm/s on).
        let base = ONE * (10 + 4 * settings.speed.clamp(1, 10) as i64) / 10_000;
        let accel = 256 + (speed_um_ms * 256 / 160).min(3 * 256);
        base * accel / 256
    }

    /// Feeds a frame received at `now` (ms).
    pub fn frame(&mut self, now: u64, frame: &Frame, s: &Settings) -> Output {
        let mut out = Output::default();
        let dt = now.saturating_sub(self.last_frame).clamp(1, 100) as i64;
        self.last_frame = now;
        let touching: Vec<Contact> = frame.contacts.iter().filter(|c| c.tip && c.confident).copied().collect();

        // A touch begins: stop gliding; touching again just after a tap drags.
        if self.fingers.is_empty() && !touching.is_empty() {
            self.touch_start = now;
            self.touch_max = 0;
            self.touch_moved = 0;
            self.touch_scrolled = false;
            self.gliding = false;
            self.velocity = (0, 0);
            if self.drag_until.take().is_some_and(|t| now <= t) {
                self.tap_release = None;
                self.dragging = true;
            }
        }
        self.touch_max = self.touch_max.max(touching.len());

        // Motion of the fingers that were already down.
        let mut moves: Vec<(i64, i64)> = Vec::new();
        let mut next: Vec<Track> = Vec::new();
        for c in &touching {
            let (x, y) = self.um(c);
            match self.fingers.iter().find(|t| t.id == c.id) {
                Some(t) => {
                    moves.push((x - t.x, y - t.y));
                    let strayed = (x - t.start_x).abs().max((y - t.start_y).abs());
                    self.touch_moved = self.touch_moved.max(strayed);
                    next.push(Track { id: c.id, x, y, start_x: t.start_x, start_y: t.start_y });
                }
                None => next.push(Track { id: c.id, x, y, start_x: x, start_y: y }),
            }
        }
        self.fingers = next;

        // Pressing the pad: a secondary click with two fingers on it.
        if frame.button && self.pressed == 0 {
            self.pressed = if touching.len() >= 2 { 2 } else { 1 };
        } else if !frame.button {
            self.pressed = 0;
        }

        match (touching.len(), moves.len()) {
            (1, 1) => {
                let (mx, my) = moves[0];
                let speed = mx.abs().max(my.abs()) / dt;
                let g = Self::gain(speed, s);
                self.rem.0 += mx * g;
                self.rem.1 += my * g;
                out.dx = (self.rem.0 / ONE) as i32;
                out.dy = (self.rem.1 / ONE) as i32;
                self.rem.0 %= ONE;
                self.rem.1 %= ONE;
            }
            (2, 2) if self.pressed == 0 => {
                // Two fingers moving together scroll: about 10 px per mm.
                let mx = (moves[0].0 + moves[1].0) / 2;
                let my = (moves[0].1 + moves[1].1) / 2;
                let sign = if s.natural_scrolling { -1 } else { 1 };
                let (sx, sy) = (sign * mx * ONE / 100, sign * my * ONE / 100);
                if mx.abs().max(my.abs()) > 100 {
                    self.touch_scrolled = true;
                }
                self.scroll_rem.0 += sx;
                self.scroll_rem.1 += sy;
                out.scroll_x = (self.scroll_rem.0 / ONE) as i32;
                out.scroll_y = (self.scroll_rem.1 / ONE) as i32;
                self.scroll_rem.0 %= ONE;
                self.scroll_rem.1 %= ONE;
                // Velocity, smoothed, for gliding after the fingers lift.
                self.velocity.0 = (self.velocity.0 + sx * 1000 / dt) / 2;
                self.velocity.1 = (self.velocity.1 + sy * 1000 / dt) / 2;
            }
            _ => {}
        }

        // The touch ended.
        if touching.is_empty() && self.touch_max > 0 {
            let quick = now.saturating_sub(self.touch_start) <= TAP_MS;
            if self.dragging {
                self.dragging = false;
                self.tap = 0;
            } else if s.tap_to_click && quick && self.touch_moved <= TAP_SLOP_UM && !self.touch_scrolled {
                self.tap = match self.touch_max {
                    1 => 1,
                    2 => 2,
                    _ => 4,
                };
                self.tap_release = Some(now + TAP_CLICK_MS);
                if self.tap == 1 {
                    self.drag_until = Some(now + TAP_CLICK_MS + TAP_DRAG_MS);
                }
            } else if self.touch_scrolled && self.velocity.0.abs().max(self.velocity.1.abs()) > GLIDE_MIN * 4 {
                self.gliding = true;
            }
            self.touch_max = 0;
        }
        if self.dragging {
            self.tap = 1;
        }
        out.buttons = self.buttons();
        out
    }

    /// Time-driven work: ending a tap's click, gliding. Returns what changed
    /// (if anything) and how long until it wants to run again.
    pub fn tick(&mut self, now: u64) -> (Option<Output>, Option<u64>) {
        let mut out = None;
        if self.tap_release.is_some_and(|at| now >= at) {
            self.tap_release = None;
            if !self.dragging && self.tap != 0 {
                self.tap = 0;
                out = Some(Output { buttons: self.buttons(), ..Default::default() });
            }
        }
        if self.drag_until.is_some_and(|t| now > t) {
            self.drag_until = None;
        }
        if self.gliding {
            self.velocity.0 = self.velocity.0 * GLIDE_DECAY / 256;
            self.velocity.1 = self.velocity.1 * GLIDE_DECAY / 256;
            if self.velocity.0.abs().max(self.velocity.1.abs()) < GLIDE_MIN {
                self.gliding = false;
            } else {
                self.scroll_rem.0 += self.velocity.0 * TICK_MS as i64 / 1000;
                self.scroll_rem.1 += self.velocity.1 * TICK_MS as i64 / 1000;
                let o = out.get_or_insert(Output { buttons: self.buttons(), ..Default::default() });
                o.scroll_x = (self.scroll_rem.0 / ONE) as i32;
                o.scroll_y = (self.scroll_rem.1 / ONE) as i32;
                self.scroll_rem.0 %= ONE;
                self.scroll_rem.1 %= ONE;
            }
        }
        let next = if self.gliding { Some(TICK_MS) } else { self.tap_release.map(|at| at.saturating_sub(now).max(1)) };
        (out, next)
    }

    /// Whether the user is touching the pad or it is still gliding.
    pub fn busy(&self) -> bool {
        !self.fingers.is_empty() || self.gliding || self.tap_release.is_some()
    }
}
