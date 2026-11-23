//! Game controllers as Linux devices, presented the way Linux's own
//! drivers present them, so that SDL's controller database (and every
//! game's own mapping) knows them without configuration:
//!
//! - Xbox 360 and Xbox One/Series pads as `xpad` does: `BTN_A`, `BTN_B`,
//!   `BTN_X` (the west button), `BTN_Y`, sticks ±32768, triggers on
//!   `ABS_Z`/`ABS_RZ` (0–255; 0–1023 on Xbox One), the d-pad as a hat.
//! - DualShock 4 and DualSense as `hid-playstation`: positional `BTN_SOUTH`,
//!   `BTN_EAST`, `BTN_NORTH` (triangle), `BTN_WEST` (square), sticks and
//!   triggers 0–255, digital `BTN_TL2`/`BTN_TR2` too, and the version with
//!   bit 15 set as that driver marks it.
//! - The Switch Pro Controller as `hid-nintendo`: positional face buttons,
//!   sticks ±32767, digital triggers, Capture as `BTN_Z`.
//! - Anything else in the Xbox layout, under its own name and ids.
//!
//! Every controller comes from `drivers::gamepad` (which also serves
//! WaveOS's own programs): [`connected`], [`update`] and [`disconnected`]
//! are called from there. Rumble effects (`EV_FF`) go back to it as
//! `gamepad::rumble_async`.

use super::codes::*;
use super::{add, remove, AbsInfo, Class, Device, Effect, InputId, Location, Sink, Spec};
use crate::drivers::gamepad;
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::gamepad as abi;
use aurora_abi::GamepadInfo;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
    Xbox360,
    XboxOne,
    DualShock4,
    DualSense,
    SwitchPro,
    Generic,
}

pub fn flavor(family: u32, vendor: u16, product: u16) -> Flavor {
    match (family, vendor, product) {
        (_, 0x045E, 0x02D1 | 0x02DD | 0x02E3 | 0x02EA | 0x0B00 | 0x0B12 | 0x0B13) => Flavor::XboxOne,
        (abi::XBOX, ..) => Flavor::Xbox360,
        (abi::PLAYSTATION, _, 0x0CE6 | 0x0DF2) => Flavor::DualSense,
        (abi::PLAYSTATION, ..) => Flavor::DualShock4,
        (abi::NINTENDO, ..) => Flavor::SwitchPro,
        _ => Flavor::Generic,
    }
}

/// The name Linux's driver gives the controller.
fn linux_name(f: Flavor, info: &GamepadInfo) -> String {
    let own = core::str::from_utf8(&info.name[..info.name_len as usize]).unwrap_or("Game Controller");
    let name = match (f, info.vendor, info.product) {
        // The keyboard-played controller keeps its own name.
        (_, 0, 0) => own,
        (Flavor::Xbox360, 0x045E, 0x028E) => "Microsoft X-Box 360 pad",
        (Flavor::Xbox360, 0x045E, 0x0719) => "Xbox 360 Wireless Receiver",
        (Flavor::Xbox360, ..) => "Generic X-Box pad",
        (Flavor::XboxOne, _, 0x02D1) => "Microsoft X-Box One pad",
        (Flavor::XboxOne, _, 0x02DD) => "Microsoft X-Box One pad (Firmware 2015)",
        (Flavor::XboxOne, _, 0x02E3) => "Microsoft X-Box One Elite pad",
        (Flavor::XboxOne, _, 0x02EA) => "Microsoft X-Box One S pad",
        (Flavor::XboxOne, ..) => "Microsoft Xbox Series S|X Controller",
        (Flavor::DualShock4, _, 0x05C4) => "Sony Computer Entertainment Wireless Controller",
        (Flavor::DualShock4, ..) => "Sony Interactive Entertainment Wireless Controller",
        (Flavor::DualSense, ..) => "Sony Interactive Entertainment DualSense Wireless Controller",
        (Flavor::SwitchPro, ..) => "Nintendo Switch Pro Controller",
        (Flavor::Generic, ..) => own,
    };
    String::from(name)
}

/// Standard buttons (`aurora_abi::gamepad` bits) and the code each driver uses.
fn buttons(f: Flavor) -> &'static [(u32, u16)] {
    const XPAD: &[(u32, u16)] = &[
        (abi::SOUTH, BTN_A),
        (abi::EAST, BTN_B),
        (abi::WEST, BTN_X),
        (abi::NORTH, BTN_Y),
        (abi::LEFT_SHOULDER, BTN_TL),
        (abi::RIGHT_SHOULDER, BTN_TR),
        (abi::BACK, BTN_SELECT),
        (abi::START, BTN_START),
        (abi::GUIDE, BTN_MODE),
        (abi::LEFT_STICK, BTN_THUMBL),
        (abi::RIGHT_STICK, BTN_THUMBR),
    ];
    const PLAYSTATION: &[(u32, u16)] = &[
        (abi::SOUTH, BTN_SOUTH),
        (abi::EAST, BTN_EAST),
        (abi::NORTH, BTN_NORTH),
        (abi::WEST, BTN_WEST),
        (abi::LEFT_SHOULDER, BTN_TL),
        (abi::RIGHT_SHOULDER, BTN_TR),
        (abi::LEFT_TRIGGER, BTN_TL2),
        (abi::RIGHT_TRIGGER, BTN_TR2),
        (abi::BACK, BTN_SELECT),
        (abi::START, BTN_START),
        (abi::LEFT_STICK, BTN_THUMBL),
        (abi::RIGHT_STICK, BTN_THUMBR),
        (abi::GUIDE, BTN_MODE),
    ];
    const NINTENDO: &[(u32, u16)] = &[
        (abi::EAST, BTN_EAST),
        (abi::SOUTH, BTN_SOUTH),
        (abi::WEST, BTN_WEST),
        (abi::NORTH, BTN_NORTH),
        (abi::LEFT_SHOULDER, BTN_TL),
        (abi::RIGHT_SHOULDER, BTN_TR),
        (abi::LEFT_TRIGGER, BTN_TL2),
        (abi::RIGHT_TRIGGER, BTN_TR2),
        (abi::BACK, BTN_SELECT),
        (abi::START, BTN_START),
        (abi::LEFT_STICK, BTN_THUMBL),
        (abi::RIGHT_STICK, BTN_THUMBR),
        (abi::GUIDE, BTN_MODE),
        (abi::EXTRA, BTN_Z),
    ];
    match f {
        Flavor::Xbox360 | Flavor::XboxOne | Flavor::Generic => XPAD,
        Flavor::DualShock4 | Flavor::DualSense => PLAYSTATION,
        Flavor::SwitchPro => NINTENDO,
    }
}

/// The standard button (`aurora_abi::gamepad` bit) that `code` is on a
/// controller of this kind (the d-pad's button codes too).
pub fn standard_bit(f: Flavor, code: u16) -> Option<u32> {
    match code {
        0x220 => Some(abi::UP),
        0x221 => Some(abi::DOWN),
        0x222 => Some(abi::LEFT),
        0x223 => Some(abi::RIGHT),
        _ => buttons(f).iter().find(|(_, c)| *c == code).map(|(b, _)| *b),
    }
}

/// The kind of controller a vendor makes (for devices made with uinput).
pub fn family_of_vendor(vendor: u16) -> u32 {
    match vendor {
        0x054C => abi::PLAYSTATION,
        0x057E => abi::NINTENDO,
        0x045E | 0x28DE | 0x0E6F | 0x24C6 => abi::XBOX,
        _ => abi::GENERIC,
    }
}

const STICKS: [u16; 4] = [ABS_X, ABS_Y, ABS_RX, ABS_RY];

fn stick_info(f: Flavor) -> AbsInfo {
    match f {
        Flavor::DualShock4 | Flavor::DualSense => AbsInfo::range(0, 255, 0, 0),
        Flavor::SwitchPro => AbsInfo::range(-32767, 32767, 250, 500),
        _ => AbsInfo::range(-32768, 32767, 16, 128),
    }
}

/// The triggers' range (none: they are buttons).
fn trigger_max(f: Flavor) -> Option<i32> {
    match f {
        Flavor::SwitchPro => None,
        Flavor::XboxOne => Some(1023),
        _ => Some(255),
    }
}

fn stick_value(f: Flavor, v: i16) -> i32 {
    match f {
        Flavor::DualShock4 | Flavor::DualSense => (v as i32 + 32768) >> 8,
        Flavor::SwitchPro => (v as i32).max(-32767),
        _ => v as i32,
    }
}

struct Shown {
    pad: u32,
    flavor: Flavor,
    dev: Arc<Device>,
}

static SHOWN: IrqMutex<Vec<Shown>> = IrqMutex::new(Vec::new());

/// Rumble effects to the controller's motors.
struct Motors {
    pad: u32,
}

impl Sink for Motors {
    fn play(&self, effect: &Effect, count: i32, gain: u16) {
        let (strong, weak) = effect.motors();
        let scale = |m: u16| ((m as u32 * gain as u32 / 0xFFFF) >> 8) as u8;
        if count <= 0 {
            gamepad::rumble_async(self.pad, 0, 0, 0);
            return;
        }
        let ms = effect.length() as u64 * count as u64;
        gamepad::rumble_async(self.pad, scale(strong), scale(weak), ms);
    }
}

/// A controller was connected (`location`: where, when the driver knows).
pub fn connected(info: &GamepadInfo, location: Option<Location>, version: u16) {
    let f = flavor(info.family, info.vendor, info.product);
    let (vendor, product, version) = match (info.vendor, info.product) {
        // The keyboard-played controller is an Xbox 360 pad to programs,
        // as Steam's virtual controllers are.
        (0, 0) => (0x045E, 0x028E, 0x0114),
        (v, p) => (
            v,
            p,
            match f {
                // hid-playstation marks its devices apart from hid-sony's.
                Flavor::DualShock4 | Flavor::DualSense => version | 0x8000,
                _ => version,
            },
        ),
    };
    let bustype = BUS_USB;
    let location = location.unwrap_or(Location::Virtual);
    let mut spec =
        Spec::new(&linux_name(f, info), InputId { bustype, vendor, product, version }, Class::Joystick, location);
    for &(_, code) in buttons(f) {
        spec.key(code);
    }
    let stick = stick_info(f);
    for a in STICKS {
        let mut i = stick;
        i.value = (stick.minimum + stick.maximum) / 2;
        spec.abs(a, i);
    }
    if let Some(max) = trigger_max(f) {
        spec.abs(ABS_Z, AbsInfo::range(0, max, 0, 0));
        spec.abs(ABS_RZ, AbsInfo::range(0, max, 0, 0));
    }
    spec.abs(ABS_HAT0X, AbsInfo::range(-1, 1, 0, 0));
    spec.abs(ABS_HAT0Y, AbsInfo::range(-1, 1, 0, 0));
    if info.rumble != 0 {
        for code in [FF_RUMBLE, FF_PERIODIC, FF_SQUARE, FF_TRIANGLE, FF_SINE, FF_GAIN] {
            spec.caps.set(EV_FF, code);
        }
        spec.ff_max = super::FF_EFFECTS;
        spec.sink = Some(Arc::new(Motors { pad: info.id }));
    }
    if let Location::Usb(u) = &spec.location {
        spec.phys = alloc::format!("usb-{}-{}/input{}", u.pci, u.ports, u.interface);
    }
    let dev = add(spec);
    SHOWN.lock().push(Shown { pad: info.id, flavor: f, dev });
}

pub fn disconnected(pad: u32) {
    let gone = {
        let mut s = SHOWN.lock();
        s.iter().position(|x| x.pad == pad).map(|i| s.remove(i))
    };
    if let Some(s) = gone {
        remove(&s.dev);
    }
}

/// A report from controller `pad`: its whole state.
pub fn update(pad: u32, st: &aurora_hid::gamepad::State) {
    let Some((f, dev)) = SHOWN.lock().iter().find(|s| s.pad == pad).map(|s| (s.flavor, s.dev.clone())) else {
        return;
    };
    for &(bit, code) in buttons(f) {
        dev.emit(EV_KEY, code, (st.buttons & bit != 0) as i32);
    }
    for (i, a) in STICKS.into_iter().enumerate() {
        dev.emit(EV_ABS, a, stick_value(f, st.axes[i]));
    }
    if let Some(max) = trigger_max(f) {
        let shift = if max == 1023 { 6 } else { 8 };
        dev.emit(EV_ABS, ABS_Z, (st.triggers[0] >> shift) as i32);
        dev.emit(EV_ABS, ABS_RZ, (st.triggers[1] >> shift) as i32);
    }
    let held = |bit: u32| (st.buttons & bit != 0) as i32;
    dev.emit(EV_ABS, ABS_HAT0X, held(abi::RIGHT) - held(abi::LEFT));
    dev.emit(EV_ABS, ABS_HAT0Y, held(abi::DOWN) - held(abi::UP));
    dev.sync();
}
