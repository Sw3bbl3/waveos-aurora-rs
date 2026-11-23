//! Controllers as the shell reads them: presses of named buttons, with the
//! d-pad and left stick repeating while held (slowly at first, then faster,
//! and faster still with the stick pushed all the way).
//!
//! Every connected pad counts: whichever is pressed drives navigation, and
//! the last one used decides which glyphs (Xbox, PlayStation, Nintendo) the
//! hints show. A keyboard can stand in for a controller in development
//! (Super+G, see [`Virtual`]), through the same path as a real one.

use crate::drivers::gamepad;
use crate::drivers::input::{KeyCode, KeyEvent};
use alloc::vec::Vec;
use aurora_abi::gamepad as abi;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    /// Confirm: south (Xbox A, PlayStation cross), east on Nintendo pads.
    Accept,
    /// Back: east (Xbox B, circle), south on Nintendo pads.
    Back,
    /// West and north (Xbox X and Y).
    X,
    Y,
    Lb,
    Rb,
    Lt,
    Rt,
    /// Start / Options / + (≡).
    Menu,
    /// Back / Share / − (⧉).
    View,
    /// The home button (Xbox, PS, ⌂).
    Guide,
    Up,
    Down,
    Left,
    Right,
}

impl Button {
    pub fn is_direction(self) -> bool {
        matches!(self, Button::Up | Button::Down | Button::Left | Button::Right)
    }
}

/// Bits 24–27 of the digital state: the left stick pushed up, down, left, right.
const STICK_UP: u32 = 1 << 24;
const STICK_DOWN: u32 = 1 << 25;
const STICK_LEFT: u32 = 1 << 26;
const STICK_RIGHT: u32 = 1 << 27;

/// The digital state of one pad: buttons, and the stick as directions
/// (with the driver's hysteresis against `previous`).
fn digital(s: &aurora_abi::GamepadState, previous: u32) -> u32 {
    let mut d = s.buttons & 0x00FF_FFFF;
    let (x, y) = (s.axes[0] as i32, s.axes[1] as i32);
    for (bit, value) in [(STICK_UP, -y), (STICK_DOWN, y), (STICK_LEFT, -x), (STICK_RIGHT, x)] {
        if value > gamepad::STICK_ON || (previous & bit != 0 && value > gamepad::STICK_OFF) {
            d |= bit;
        }
    }
    d
}

/// The first press repeats after this long, then every `REPEAT_START` ms,
/// speeding up by `REPEAT_STEP` each time down to `REPEAT_FASTEST`.
const REPEAT_DELAY: u64 = 380;
const REPEAT_START: u64 = 150;
const REPEAT_STEP: u64 = 14;
const REPEAT_FASTEST: u64 = 62;

pub struct Nav {
    /// Per pad: (id, family, digital state last seen).
    pads: Vec<(u32, u32, u32)>,
    /// The direction held: which, when it next repeats, how many times it has.
    held: Option<(Button, u64, u32)>,
    /// Family of the pad used last (for glyphs), and its id (for rumble).
    pub family: u32,
    pub last_pad: Option<u32>,
}

impl Nav {
    pub const fn new() -> Nav {
        Nav { pads: Vec::new(), held: None, family: abi::XBOX, last_pad: None }
    }

    /// When a held direction repeats next.
    pub fn deadline(&self) -> Option<u64> {
        self.held.map(|(_, at, _)| at)
    }

    /// Forgets what is held (entering a new screen shouldn't keep scrolling).
    pub fn release(&mut self) {
        self.held = None;
    }

    /// Reads every pad: the buttons pressed since last time, then a repeat of
    /// the held direction if it is due.
    pub fn poll(&mut self, now: u64) -> Vec<Button> {
        let mut out = Vec::new();
        let states = gamepad::poll();
        self.pads.retain(|p| states.iter().any(|(i, ..)| i.id == p.0));
        let mut deepest = 0i32;
        for (info, st, latched) in &states {
            let k = match self.pads.iter().position(|p| p.0 == info.id) {
                Some(k) => k,
                None => {
                    // A new pad: what it holds now isn't a press.
                    self.pads.push((info.id, info.family, digital(st, 0)));
                    continue;
                }
            };
            let before = self.pads[k].2;
            let now_bits = digital(st, before);
            // Pressed now, or pressed and already let go since the last look.
            let pressed = (now_bits & !before) | latched;
            self.pads[k].2 = now_bits;
            deepest = deepest.max((st.axes[0] as i32).abs().max((st.axes[1] as i32).abs()));
            if pressed == 0 {
                continue;
            }
            self.family = info.family;
            self.last_pad = Some(info.id);
            let nintendo = info.family == abi::NINTENDO;
            let map: [(u32, Button); 15] = [
                (abi::SOUTH, if nintendo { Button::Back } else { Button::Accept }),
                (abi::EAST, if nintendo { Button::Accept } else { Button::Back }),
                (abi::WEST, Button::X),
                (abi::NORTH, Button::Y),
                (abi::LEFT_SHOULDER, Button::Lb),
                (abi::RIGHT_SHOULDER, Button::Rb),
                (abi::LEFT_TRIGGER, Button::Lt),
                (abi::RIGHT_TRIGGER, Button::Rt),
                (abi::START, Button::Menu),
                (abi::BACK, Button::View),
                (abi::GUIDE, Button::Guide),
                (abi::UP | STICK_UP, Button::Up),
                (abi::DOWN | STICK_DOWN, Button::Down),
                (abi::LEFT | STICK_LEFT, Button::Left),
                (abi::RIGHT | STICK_RIGHT, Button::Right),
            ];
            for (bits, b) in map {
                if pressed & bits != 0 && !out.contains(&b) {
                    out.push(b);
                    if b.is_direction() && now_bits & bits != 0 {
                        self.held = Some((b, now + REPEAT_DELAY, 0));
                    }
                }
            }
        }
        // The held direction: still held on some pad? Then repeat when due.
        if let Some((b, at, count)) = self.held {
            let bits = match b {
                Button::Up => abi::UP | STICK_UP,
                Button::Down => abi::DOWN | STICK_DOWN,
                Button::Left => abi::LEFT | STICK_LEFT,
                _ => abi::RIGHT | STICK_RIGHT,
            };
            if !self.pads.iter().any(|p| p.2 & bits != 0) {
                self.held = None;
            } else if now >= at && !out.contains(&b) {
                let mut every = REPEAT_START.saturating_sub(REPEAT_STEP * count as u64).max(REPEAT_FASTEST);
                // The stick all the way over: faster again.
                if deepest > 30_000 {
                    every = every * 2 / 3;
                }
                self.held = Some((b, now + every, count + 1));
                out.push(b);
            }
        }
        out
    }

    /// Whether the held direction is repeating (as opposed to a fresh press).
    pub fn repeating(&self) -> bool {
        self.held.is_some_and(|(_, _, n)| n > 0)
    }
}

/// A controller made of keys, for development without one (and for virtual
/// machines whose USB can't pass a controller through): Super+G connects or
/// disconnects it, and while it is connected the arrows are the d-pad,
/// Enter is A, Escape is B, Backspace X, Tab Y, Page Up/Down the shoulder
/// buttons, Home the guide button, End Menu and Insert View. It is a real
/// controller to the rest of the system (`gamepad::add`), so everything
/// downstream runs the same code as for a USB one.
pub struct Virtual {
    pub id: Option<u32>,
    buttons: u32,
}

impl Virtual {
    pub const fn new() -> Virtual {
        Virtual { id: None, buttons: 0 }
    }

    pub fn toggle(&mut self) {
        match self.id.take() {
            Some(id) => gamepad::remove(id),
            None => {
                self.buttons = 0;
                self.id = Some(gamepad::add("Virtual Controller (keyboard)", 0, 0, abi::XBOX, None));
            }
        }
    }

    /// Takes a key while connected; true if it was a controller button.
    /// Presses count only when `capture` (Wave Play has the screen, or it
    /// is the home button), so the keyboard keeps working on the desktop;
    /// a release always reaches a button this pressed.
    pub fn key(&mut self, k: &KeyEvent, capture: bool) -> bool {
        let Some(id) = self.id else { return false };
        if k.mods.ctrl || k.mods.alt || k.mods.super_key {
            return false;
        }
        let bit = match k.code {
            KeyCode::Up => abi::UP,
            KeyCode::Down => abi::DOWN,
            KeyCode::Left => abi::LEFT,
            KeyCode::Right => abi::RIGHT,
            KeyCode::Enter => abi::SOUTH,
            KeyCode::Escape => abi::EAST,
            KeyCode::Backspace => abi::WEST,
            KeyCode::Tab => abi::NORTH,
            KeyCode::PageUp => abi::LEFT_SHOULDER,
            KeyCode::PageDown => abi::RIGHT_SHOULDER,
            KeyCode::Home => abi::GUIDE,
            KeyCode::End => abi::START,
            KeyCode::Insert => abi::BACK,
            _ => return false,
        };
        let before = self.buttons;
        if k.pressed {
            if !capture && bit != abi::GUIDE {
                return false;
            }
            self.buttons |= bit;
        } else if self.buttons & bit != 0 {
            self.buttons &= !bit;
        } else {
            return false;
        }
        if self.buttons != before {
            gamepad::update(id, &aurora_hid::gamepad::State { buttons: self.buttons, axes: [0; 4], triggers: [0; 2] });
        }
        true
    }
}
