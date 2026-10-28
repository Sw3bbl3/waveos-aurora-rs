//! Game controllers, in one standard layout whatever the device.
//!
//! The layout is the W3C "standard gamepad": face buttons by position
//! (south, east, west, north — Xbox A, B, X, Y; PlayStation cross, circle,
//! square, triangle; Nintendo B, A, Y, X), shoulders, triggers, two sticks
//! and the d-pad. Sticks are -32768..=32767 with down and right positive;
//! triggers are 0..=65535.
//!
//! HID game controllers describe their controls, but not which button is
//! which: that needs knowing the device ([`Layout`]). Xbox controllers
//! don't use HID at all; [`xbox360`] and [`xbox_one`] read their reports.

use crate::descriptor::{Descriptor, Field, Kind};
use crate::usage;
use alloc::vec::Vec;

/// Buttons, as bit numbers in [`State::buttons`].
pub mod button {
    pub const SOUTH: u32 = 0;
    pub const EAST: u32 = 1;
    pub const WEST: u32 = 2;
    pub const NORTH: u32 = 3;
    pub const LEFT_SHOULDER: u32 = 4;
    pub const RIGHT_SHOULDER: u32 = 5;
    pub const LEFT_TRIGGER: u32 = 6;
    pub const RIGHT_TRIGGER: u32 = 7;
    pub const BACK: u32 = 8;
    pub const START: u32 = 9;
    pub const LEFT_STICK: u32 = 10;
    pub const RIGHT_STICK: u32 = 11;
    pub const UP: u32 = 12;
    pub const DOWN: u32 = 13;
    pub const LEFT: u32 = 14;
    pub const RIGHT: u32 = 15;
    pub const GUIDE: u32 = 16;
    /// PlayStation touchpad click, Switch capture.
    pub const EXTRA: u32 = 17;
}

/// Stick axes in [`State::axes`].
pub mod axis {
    pub const LEFT_X: usize = 0;
    pub const LEFT_Y: usize = 1;
    pub const RIGHT_X: usize = 2;
    pub const RIGHT_Y: usize = 3;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub buttons: u32,
    pub axes: [i16; 4],
    /// Left and right trigger.
    pub triggers: [u16; 2],
}

impl State {
    pub fn pressed(&self, b: u32) -> bool {
        self.buttons & (1 << b) != 0
    }
    fn set(&mut self, b: u32, on: bool) {
        if on {
            self.buttons |= 1 << b;
        }
    }
    /// Keeps digital and analog triggers in agreement.
    fn settle_triggers(&mut self, analog: bool) {
        for (i, b) in [(0, button::LEFT_TRIGGER), (1, button::RIGHT_TRIGGER)] {
            if analog {
                self.set(b, self.triggers[i] > 0x4000);
            } else if self.pressed(b) {
                self.triggers[i] = u16::MAX;
            }
        }
    }
}

/// Which physical button a HID button number is, per kind of controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// The common layout of unknown HID pads ("DirectInput" order).
    Generic,
    /// Sony DualShock 4 and DualSense.
    PlayStation,
    /// Nintendo Switch Pro Controller (in its simple HID mode).
    SwitchPro,
}

impl Layout {
    pub fn for_device(vendor: u16, product: u16) -> Layout {
        match (vendor, product) {
            (0x054C, 0x05C4 | 0x09CC | 0x0BA0 | 0x0CE6 | 0x0DF2) => Layout::PlayStation,
            (0x057E, 0x2009) => Layout::SwitchPro,
            _ => Layout::Generic,
        }
    }

    /// The standard button for HID button `n` (1-based).
    fn button(self, n: u16) -> Option<u32> {
        use button::*;
        let map: &[u32] = match self {
            Layout::Generic => &[
                SOUTH,
                EAST,
                WEST,
                NORTH,
                LEFT_SHOULDER,
                RIGHT_SHOULDER,
                LEFT_TRIGGER,
                RIGHT_TRIGGER,
                BACK,
                START,
                LEFT_STICK,
                RIGHT_STICK,
                GUIDE,
            ],
            // Square, cross, circle, triangle, L1, R1, L2, R2, share, options, L3, R3, PS, pad.
            Layout::PlayStation => &[
                WEST,
                SOUTH,
                EAST,
                NORTH,
                LEFT_SHOULDER,
                RIGHT_SHOULDER,
                LEFT_TRIGGER,
                RIGHT_TRIGGER,
                BACK,
                START,
                LEFT_STICK,
                RIGHT_STICK,
                GUIDE,
                EXTRA,
            ],
            // B, A, Y, X (by position: south, east, west, north), L, R, ZL, ZR, −, +, LS, RS, home, capture.
            Layout::SwitchPro => &[
                SOUTH,
                EAST,
                WEST,
                NORTH,
                LEFT_SHOULDER,
                RIGHT_SHOULDER,
                LEFT_TRIGGER,
                RIGHT_TRIGGER,
                BACK,
                START,
                LEFT_STICK,
                RIGHT_STICK,
                GUIDE,
                EXTRA,
            ],
        };
        map.get((n as usize).checked_sub(1)?).copied()
    }
}

/// Whether a HID device is a game controller.
pub fn is_gamepad(d: &Descriptor) -> bool {
    d.has_application(usage::GAMEPAD) || d.has_application(usage::JOYSTICK) || d.has_application(usage::MULTI_AXIS)
}

/// Where each control is, found once from the descriptor.
#[derive(Clone, Debug)]
pub struct HidGamepad {
    layout: Layout,
    /// Report id and data of the main input report.
    report_id: u8,
    sticks: [Option<(usize, u32)>; 4],
    triggers: [Option<(usize, u32)>; 2],
    hat: Option<(usize, u32)>,
    buttons: Vec<usize>,
}

fn axis_value(f: &Field, data: &[u8], entry: u32) -> i16 {
    let v = f.value(data, entry) as i64;
    let (lo, hi) = (f.logical_min as i64, f.logical_max as i64);
    if hi <= lo {
        return 0;
    }
    ((v.clamp(lo, hi) - lo) * 65535 / (hi - lo) - 32768) as i16
}

fn trigger_value(f: &Field, data: &[u8], entry: u32) -> u16 {
    let v = f.value(data, entry) as i64;
    let (lo, hi) = (f.logical_min as i64, f.logical_max as i64);
    if hi <= lo {
        return 0;
    }
    ((v.clamp(lo, hi) - lo) * 65535 / (hi - lo)) as u16
}

impl HidGamepad {
    pub fn new(d: &Descriptor, vendor: u16, product: u16) -> Option<HidGamepad> {
        if !is_gamepad(d) {
            return None;
        }
        let app = |f: &Field| matches!(f.application, usage::GAMEPAD | usage::JOYSTICK | usage::MULTI_AXIS);
        let find = |u: u32| {
            d.fields.iter().enumerate().find_map(|(i, f)| {
                (f.kind == Kind::Input && !f.is_constant() && f.is_variable() && app(f))
                    .then(|| f.entry_of(u).map(|e| (i, e)))
                    .flatten()
            })
        };
        let x = find(usage::X)?;
        let report_id = d.fields[x.0].report_id;
        let (z, rz, rx, ry) = (find(usage::Z), find(usage::RZ), find(usage::RX), find(usage::RY));
        // Right stick on Z/Rz with triggers on Rx/Ry (PlayStation and most
        // pads), or on Rx/Ry with triggers on Z/Rz.
        let (right, triggers) = if z.is_some() && rz.is_some() { ([z, rz], [rx, ry]) } else { ([rx, ry], [z, rz]) };
        let buttons = d
            .fields
            .iter()
            .enumerate()
            .filter(|(_, f)| {
                f.kind == Kind::Input
                    && !f.is_constant()
                    && app(f)
                    && f.report_id == report_id
                    && (0..f.count.max(1)).any(|i| f.usage(i).is_some_and(|u| usage::page(u) == usage::BUTTON_PAGE))
            })
            .map(|(i, _)| i)
            .collect();
        Some(HidGamepad {
            layout: Layout::for_device(vendor, product),
            report_id,
            sticks: [Some(x), find(usage::Y), right[0], right[1]],
            triggers,
            hat: find(usage::HAT_SWITCH),
            buttons,
        })
    }

    pub fn layout(&self) -> Layout {
        self.layout
    }

    /// Reads an input report (`None`: a report about something else).
    pub fn read(&self, d: &Descriptor, report: &[u8]) -> Option<State> {
        let (id, data) = d.split(report)?;
        if id != self.report_id {
            return None;
        }
        let mut s = State::default();
        for (k, a) in self.sticks.iter().enumerate() {
            if let Some((fi, e)) = *a {
                s.axes[k] = axis_value(&d.fields[fi], data, e);
            }
        }
        let mut analog = false;
        for (k, t) in self.triggers.iter().enumerate() {
            if let Some((fi, e)) = *t {
                s.triggers[k] = trigger_value(&d.fields[fi], data, e);
                analog = true;
            }
        }
        if let Some((fi, e)) = self.hat {
            let f = &d.fields[fi];
            let v = f.raw(data, e) as i64 - f.logical_min as i64;
            let positions = (f.logical_max as i64 - f.logical_min as i64 + 1).max(1);
            // Eight positions clockwise from up (four-position hats scale up).
            if (0..positions).contains(&v) {
                let dir = v * 8 / positions;
                s.set(button::UP, matches!(dir, 7 | 0 | 1));
                s.set(button::RIGHT, matches!(dir, 1..=3));
                s.set(button::DOWN, matches!(dir, 3..=5));
                s.set(button::LEFT, matches!(dir, 5..=7));
            }
        }
        for &fi in &self.buttons {
            let f = &d.fields[fi];
            for i in 0..f.count {
                let u = if f.is_variable() {
                    (f.raw(data, i) != 0).then(|| f.usage(i)).flatten()
                } else {
                    f.array_usage(f.raw(data, i))
                };
                if let Some(b) =
                    u.filter(|&u| usage::page(u) == usage::BUTTON_PAGE).and_then(|u| self.layout.button(usage::id(u)))
                {
                    s.set(b, true);
                }
            }
        }
        s.settle_triggers(analog);
        Some(s)
    }
}

/// The rumble report for a PlayStation controller (`strong`/`weak` motors 0–255).
pub fn playstation_rumble(product: u16, strong: u8, weak: u8) -> Vec<u8> {
    if matches!(product, 0x0CE6 | 0x0DF2) {
        // DualSense output report 0x02: flags enable "compatible" vibration.
        let mut r = alloc::vec![0u8; 48];
        r[0] = 0x02;
        r[1] = 0x03;
        r[3] = weak;
        r[4] = strong;
        r
    } else {
        // DualShock 4 output report 0x05: flags, then weak, strong, light bar.
        let mut r = alloc::vec![0u8; 32];
        r[0] = 0x05;
        r[1] = 0x07;
        r[4] = weak;
        r[5] = strong;
        r[6] = 0x00;
        r[7] = 0x40;
        r[8] = 0xFF; // light bar: WaveOS blue
        r
    }
}

fn stick(lo: u8, hi: u8, invert: bool) -> i16 {
    let v = i16::from_le_bytes([lo, hi]);
    if invert {
        // Xbox sticks are up-positive; ours are down-positive.
        (-(v as i32) - 1).clamp(i16::MIN as i32, i16::MAX as i32) as i16
    } else {
        v
    }
}

/// Xbox 360 controllers (USB interface class 0xFF, subclass 0x5D, protocol 1).
pub mod xbox360 {
    use super::*;

    /// Reads a 20-byte input report.
    pub fn read(r: &[u8]) -> Option<State> {
        if r.len() < 14 || r[0] != 0x00 || r[1] < 0x14 {
            return None;
        }
        let mut s = State::default();
        let bits: [(u8, u8, u32); 15] = [
            (2, 0x01, button::UP),
            (2, 0x02, button::DOWN),
            (2, 0x04, button::LEFT),
            (2, 0x08, button::RIGHT),
            (2, 0x10, button::START),
            (2, 0x20, button::BACK),
            (2, 0x40, button::LEFT_STICK),
            (2, 0x80, button::RIGHT_STICK),
            (3, 0x01, button::LEFT_SHOULDER),
            (3, 0x02, button::RIGHT_SHOULDER),
            (3, 0x04, button::GUIDE),
            (3, 0x10, button::SOUTH),
            (3, 0x20, button::EAST),
            (3, 0x40, button::WEST),
            (3, 0x80, button::NORTH),
        ];
        for (byte, mask, b) in bits {
            s.set(b, r[byte as usize] & mask != 0);
        }
        s.triggers = [r[4] as u16 * 257, r[5] as u16 * 257];
        s.axes =
            [stick(r[6], r[7], false), stick(r[8], r[9], true), stick(r[10], r[11], false), stick(r[12], r[13], true)];
        s.settle_triggers(true);
        Some(s)
    }

    /// Rumble: strong (left) and weak (right) motors, 0–255.
    pub fn rumble(strong: u8, weak: u8) -> [u8; 8] {
        [0x00, 0x08, 0x00, strong, weak, 0x00, 0x00, 0x00]
    }

    /// Lights the ring around the guide button for player `n` (1–4).
    pub fn led(player: u8) -> [u8; 3] {
        [0x01, 0x03, 0x05 + player.clamp(1, 4)]
    }
}

/// Xbox One and Series controllers (class 0xFF, subclass 0x47, protocol
/// 0xD0): the GIP protocol.
pub mod xbox_one {
    use super::*;

    /// What the controller must be sent before it reports anything.
    pub fn start() -> [u8; 5] {
        [0x05, 0x20, 0x00, 0x01, 0x00]
    }

    /// Reads a GIP message: input (0x20) or the guide button (0x07).
    /// `previous` keeps the guide button across input messages.
    pub fn read(r: &[u8], previous: &State) -> Option<State> {
        match *r.first()? {
            0x20 if r.len() >= 18 => {
                let mut s = State { buttons: previous.buttons & (1 << button::GUIDE), ..Default::default() };
                let bits: [(usize, u8, u32); 14] = [
                    (4, 0x04, button::START),
                    (4, 0x08, button::BACK),
                    (4, 0x10, button::SOUTH),
                    (4, 0x20, button::EAST),
                    (4, 0x40, button::WEST),
                    (4, 0x80, button::NORTH),
                    (5, 0x01, button::UP),
                    (5, 0x02, button::DOWN),
                    (5, 0x04, button::LEFT),
                    (5, 0x08, button::RIGHT),
                    (5, 0x10, button::LEFT_SHOULDER),
                    (5, 0x20, button::RIGHT_SHOULDER),
                    (5, 0x40, button::LEFT_STICK),
                    (5, 0x80, button::RIGHT_STICK),
                ];
                for (byte, mask, b) in bits {
                    s.set(b, r[byte] & mask != 0);
                }
                // Triggers are 10-bit.
                let trig = |lo: u8, hi: u8| ((u16::from_le_bytes([lo, hi]) as u32).min(1023) * 65535 / 1023) as u16;
                s.triggers = [trig(r[6], r[7]), trig(r[8], r[9])];
                s.axes = [
                    stick(r[10], r[11], false),
                    stick(r[12], r[13], true),
                    stick(r[14], r[15], false),
                    stick(r[16], r[17], true),
                ];
                s.settle_triggers(true);
                Some(s)
            }
            0x07 if r.len() >= 5 => {
                let mut s = *previous;
                s.buttons &= !(1 << button::GUIDE);
                s.set(button::GUIDE, r[4] & 0x01 != 0);
                Some(s)
            }
            _ => None,
        }
    }

    /// Rumble on the main motors (0–255 each; the controller takes 0–100).
    pub fn rumble(sequence: u8, strong: u8, weak: u8) -> [u8; 13] {
        let pct = |v: u8| (v as u16 * 100 / 255) as u8;
        [0x09, 0x00, sequence, 0x09, 0x00, 0x0F, 0x00, 0x00, pct(strong), pct(weak), 0xFF, 0x00, 0x00]
    }
}
