//! Game controllers: every connected one, in one standard layout.
//!
//! ```ignore
//! for pad in corekit::gamepad::connected() {
//!     if let Some(s) = pad.state() {
//!         if s.pressed(Button::South) { jump(); }
//!         let (x, y) = s.left_stick(); // -1.0 ..= 1.0, right and down positive
//!     }
//! }
//! ```

use crate::abi::{self, gamepad as bits, nr};
use crate::sys::call;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Buttons by position, whatever is printed on them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Button {
    /// Xbox A, PlayStation cross, Nintendo B.
    South,
    /// Xbox B, PlayStation circle, Nintendo A.
    East,
    /// Xbox X, PlayStation square, Nintendo Y.
    West,
    /// Xbox Y, PlayStation triangle, Nintendo X.
    North,
    LeftShoulder,
    RightShoulder,
    LeftTrigger,
    RightTrigger,
    Back,
    Start,
    LeftStick,
    RightStick,
    Up,
    Down,
    Left,
    Right,
    Guide,
}

impl Button {
    fn bit(self) -> u32 {
        match self {
            Button::South => bits::SOUTH,
            Button::East => bits::EAST,
            Button::West => bits::WEST,
            Button::North => bits::NORTH,
            Button::LeftShoulder => bits::LEFT_SHOULDER,
            Button::RightShoulder => bits::RIGHT_SHOULDER,
            Button::LeftTrigger => bits::LEFT_TRIGGER,
            Button::RightTrigger => bits::RIGHT_TRIGGER,
            Button::Back => bits::BACK,
            Button::Start => bits::START,
            Button::LeftStick => bits::LEFT_STICK,
            Button::RightStick => bits::RIGHT_STICK,
            Button::Up => bits::UP,
            Button::Down => bits::DOWN,
            Button::Left => bits::LEFT,
            Button::Right => bits::RIGHT,
            Button::Guide => bits::GUIDE,
        }
    }

    pub const ALL: [Button; 17] = [
        Button::South,
        Button::East,
        Button::West,
        Button::North,
        Button::LeftShoulder,
        Button::RightShoulder,
        Button::LeftTrigger,
        Button::RightTrigger,
        Button::Back,
        Button::Start,
        Button::LeftStick,
        Button::RightStick,
        Button::Up,
        Button::Down,
        Button::Left,
        Button::Right,
        Button::Guide,
    ];
}

/// What the buttons are called on this kind of controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Family {
    Generic,
    Xbox,
    PlayStation,
    Nintendo,
}

impl Family {
    /// The label printed on a button ("A", "Cross", …).
    pub fn label(self, b: Button) -> &'static str {
        use Button::*;
        match (self, b) {
            (Family::PlayStation, South) => "Cross",
            (Family::PlayStation, East) => "Circle",
            (Family::PlayStation, West) => "Square",
            (Family::PlayStation, North) => "Triangle",
            (Family::PlayStation, LeftShoulder) => "L1",
            (Family::PlayStation, RightShoulder) => "R1",
            (Family::PlayStation, LeftTrigger) => "L2",
            (Family::PlayStation, RightTrigger) => "R2",
            (Family::PlayStation, Back) => "Share",
            (Family::PlayStation, Start) => "Options",
            (Family::PlayStation, Guide) => "PS",
            (Family::Nintendo, South) => "B",
            (Family::Nintendo, East) => "A",
            (Family::Nintendo, West) => "Y",
            (Family::Nintendo, North) => "X",
            (Family::Nintendo, LeftShoulder) => "L",
            (Family::Nintendo, RightShoulder) => "R",
            (Family::Nintendo, LeftTrigger) => "ZL",
            (Family::Nintendo, RightTrigger) => "ZR",
            (Family::Nintendo, Back) => "−",
            (Family::Nintendo, Start) => "+",
            (Family::Nintendo, Guide) => "Home",
            (_, South) => "A",
            (_, East) => "B",
            (_, West) => "X",
            (_, North) => "Y",
            (_, LeftShoulder) => "LB",
            (_, RightShoulder) => "RB",
            (_, LeftTrigger) => "LT",
            (_, RightTrigger) => "RT",
            (_, Back) => "View",
            (_, Start) => "Menu",
            (_, Guide) => "Guide",
            (_, LeftStick) => "Left Stick",
            (_, RightStick) => "Right Stick",
            (_, Up) => "Up",
            (_, Down) => "Down",
            (_, Left) => "Left",
            (_, Right) => "Right",
        }
    }
}

/// A connected controller.
#[derive(Clone, Debug)]
pub struct Gamepad {
    pub id: u32,
    pub name: String,
    pub vendor: u16,
    pub product: u16,
    pub family: Family,
    pub has_rumble: bool,
}

/// One reading of a controller.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct State(pub abi::GamepadState);

impl State {
    pub fn pressed(&self, b: Button) -> bool {
        self.0.buttons & b.bit() != 0
    }

    fn unit(v: i16) -> f32 {
        (v as f32 / 32767.0).clamp(-1.0, 1.0)
    }

    /// The left stick, each axis -1.0..=1.0 (right and down positive).
    pub fn left_stick(&self) -> (f32, f32) {
        (Self::unit(self.0.axes[0]), Self::unit(self.0.axes[1]))
    }

    pub fn right_stick(&self) -> (f32, f32) {
        (Self::unit(self.0.axes[2]), Self::unit(self.0.axes[3]))
    }

    /// The triggers, 0.0..=1.0.
    pub fn triggers(&self) -> (f32, f32) {
        (self.0.triggers[0] as f32 / 65535.0, self.0.triggers[1] as f32 / 65535.0)
    }

    /// Changes each time the controller reports.
    pub fn sequence(&self) -> u32 {
        self.0.sequence
    }
}

/// The controllers connected now.
pub fn connected() -> Vec<Gamepad> {
    let mut v = vec![abi::GamepadInfo::default(); 16];
    let n = call(nr::GAMEPADS, &[v.as_mut_ptr() as u64, v.len() as u64]).unwrap_or(0) as usize;
    v.truncate(n.min(16));
    v.into_iter()
        .map(|i| Gamepad {
            id: i.id,
            name: String::from_utf8_lossy(&i.name[..(i.name_len as usize).min(64)]).into(),
            vendor: i.vendor,
            product: i.product,
            family: match i.family {
                bits::XBOX => Family::Xbox,
                bits::PLAYSTATION => Family::PlayStation,
                bits::NINTENDO => Family::Nintendo,
                _ => Family::Generic,
            },
            has_rumble: i.rumble != 0,
        })
        .collect()
}

impl Gamepad {
    /// Its state now (`None` once it is unplugged).
    pub fn state(&self) -> Option<State> {
        let mut s = abi::GamepadState::default();
        call(nr::GAMEPAD_STATE, &[self.id as u64, &mut s as *mut _ as u64]).ok().map(|_| State(s))
    }

    /// Shakes it: `strong` and `weak` motors 0.0..=1.0, for `ms` milliseconds.
    pub fn rumble(&self, strong: f32, weak: f32, ms: u32) -> bool {
        let level = |v: f32| (v.clamp(0.0, 1.0) * 255.0) as u64;
        call(nr::GAMEPAD_RUMBLE, &[self.id as u64, level(strong), level(weak), ms as u64]).is_ok()
    }
}
