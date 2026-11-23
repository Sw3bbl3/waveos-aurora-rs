//! What the native drivers call to describe their keyboards, pointers and
//! touchpads as Linux devices, and to feed them: the same capabilities and
//! events Linux's `atkbd`, `psmouse`, `hid-input` and `hid-multitouch`
//! produce for such devices.

use super::codes::*;
use super::{add, AbsInfo, Class, Device, InputId, Location, Sink, Spec};
use alloc::sync::Arc;

/// A keyboard's lights: Caps Lock written by a program lights every
/// keyboard (WaveOS has one Caps Lock state for all of them).
struct Lights;

impl Sink for Lights {
    fn written(&self, ty: u16, code: u16, value: i32) {
        if ty == EV_LED && code == LED_CAPSL {
            crate::drivers::input::set_caps_light(value != 0);
        }
    }
}

/// A keyboard: every key of the keyboard page, media and power keys, the
/// three lock lights, key repeat and scan codes.
pub fn keyboard(name: &str, id: InputId, phys: &str, location: Location) -> Arc<Device> {
    let mut spec = Spec::new(name, id, Class::Keyboard, location);
    spec.phys = alloc::string::String::from(phys);
    for k in keyboard_keys() {
        spec.key(k);
    }
    spec.caps.set(EV_MSC, MSC_SCAN);
    for l in [LED_NUML, LED_CAPSL, LED_SCROLLL] {
        spec.caps.set(EV_LED, l);
    }
    spec.caps.ev |= 1 << EV_REP;
    spec.sink = Some(Arc::new(Lights));
    let d = add(spec);
    d.emit(EV_LED, LED_CAPSL, crate::drivers::input::caps() as i32);
    d.sync();
    d
}

/// A key going down or up (`scan`: the scan code, for `MSC_SCAN`).
pub fn key(d: &Device, scan: Option<i32>, code: u16, down: bool) {
    if let Some(s) = scan {
        d.emit(EV_MSC, MSC_SCAN, s);
    }
    d.emit(EV_KEY, code, down as i32);
    d.sync();
}

/// Caps Lock changed: every keyboard's `LED_CAPSL`.
pub fn caps_changed(on: bool) {
    for d in super::devices() {
        if d.class == Class::Keyboard && d.has(EV_LED, LED_CAPSL) {
            d.emit(EV_LED, LED_CAPSL, on as i32);
            d.sync();
        }
    }
}

/// PS/2 scancodes (set 1) to keys; keeps the `E0` prefix between bytes.
pub struct Ps2Keys {
    ext: bool,
}

impl Ps2Keys {
    pub const fn new() -> Ps2Keys {
        Ps2Keys { ext: false }
    }

    pub fn feed(&mut self, d: &Device, byte: u8) {
        match byte {
            0xE0 => {
                self.ext = true;
                return;
            }
            0xE1 | 0xFA | 0xFE => return,
            _ => {}
        }
        let ext = core::mem::replace(&mut self.ext, false);
        let sc = byte & 0x7F;
        // The fake shifts around Print Screen are not keys.
        if ext && matches!(sc, 0x2A | 0x36) {
            return;
        }
        if let Some(k) = ps2_key(ext, sc) {
            let scan = if ext { 0xE000 | sc as i32 } else { sc as i32 };
            key(d, Some(scan), k, byte & 0x80 == 0);
        }
    }
}

/// A mouse: relative motion, wheels and five buttons (`absolute`: a
/// tablet or a virtual machine's pointer, with axes over `x` and `y`).
pub fn pointer(
    name: &str,
    id: InputId,
    phys: &str,
    location: Location,
    absolute: Option<((i32, i32), (i32, i32))>,
) -> Arc<Device> {
    let class = if absolute.is_some() { Class::Tablet } else { Class::Mouse };
    let mut spec = Spec::new(name, id, class, location);
    spec.phys = alloc::string::String::from(phys);
    for b in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA] {
        spec.key(b);
    }
    match absolute {
        Some((x, y)) => {
            spec.abs(ABS_X, AbsInfo::range(x.0, x.1, 0, 0));
            spec.abs(ABS_Y, AbsInfo::range(y.0, y.1, 0, 0));
        }
        None => {
            spec.rel(REL_X).rel(REL_Y);
        }
    }
    spec.rel(REL_WHEEL).rel(REL_HWHEEL).rel(REL_WHEEL_HI_RES).rel(REL_HWHEEL_HI_RES);
    spec.caps.set(EV_MSC, MSC_SCAN);
    add(spec)
}

/// One pointer report: `buttons` (bit 0 left, 1 right, 2 middle, 3 side,
/// 4 extra), motion or position, and wheel notches (positive: away from
/// the user, and right).
pub fn pointer_report(d: &Device, buttons: u8, x: Option<i32>, y: Option<i32>, wheel: i32, pan: i32) {
    for (bit, code) in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE, BTN_SIDE, BTN_EXTRA].into_iter().enumerate() {
        d.emit(EV_KEY, code, (buttons >> bit & 1) as i32);
    }
    let (ax, ay) = if d.has(EV_ABS, ABS_X) { (ABS_X, ABS_Y) } else { (REL_X, REL_Y) };
    let ty = if d.has(EV_ABS, ABS_X) { EV_ABS } else { EV_REL };
    if let Some(x) = x {
        d.emit(ty, ax, x);
    }
    if let Some(y) = y {
        d.emit(ty, ay, y);
    }
    if wheel != 0 {
        d.emit(EV_REL, REL_WHEEL, wheel);
        d.emit(EV_REL, REL_WHEEL_HI_RES, wheel * 120);
    }
    if pan != 0 {
        d.emit(EV_REL, REL_HWHEEL, pan);
        d.emit(EV_REL, REL_HWHEEL_HI_RES, pan * 120);
    }
    d.sync();
}

/// Contacts a touchpad reports at once.
const SLOTS: usize = 5;

/// A touchpad: multi-touch slots (protocol B), the single-touch axes and
/// finger counts libinput and SDL read, and its button (a clickpad).
pub fn touchpad(
    name: &str,
    id: InputId,
    phys: &str,
    location: Location,
    info: &aurora_hid::touchpad::Info,
) -> Arc<Device> {
    let mut spec = Spec::new(name, id, Class::Touchpad, location);
    spec.phys = alloc::string::String::from(phys);
    let res = |span: i32, hmm: Option<u32>| hmm.filter(|&h| h >= 100).map_or(0, |h| span * 100 / h as i32);
    let rx = res(info.x_max - info.x_min, info.width_hmm);
    let ry = res(info.y_max - info.y_min, info.height_hmm);
    let axis = |min: i32, max: i32, r: i32| AbsInfo { resolution: r, ..AbsInfo::range(min, max, 0, 0) };
    spec.abs(ABS_X, axis(info.x_min, info.x_max, rx));
    spec.abs(ABS_Y, axis(info.y_min, info.y_max, ry));
    spec.abs(ABS_MT_SLOT, AbsInfo::range(0, SLOTS as i32 - 1, 0, 0));
    spec.abs(ABS_MT_POSITION_X, axis(info.x_min, info.x_max, rx));
    spec.abs(ABS_MT_POSITION_Y, axis(info.y_min, info.y_max, ry));
    spec.abs(ABS_MT_TRACKING_ID, AbsInfo::range(0, 0xFFFF, 0, 0));
    for b in [
        BTN_LEFT,
        BTN_TOUCH,
        BTN_TOOL_FINGER,
        BTN_TOOL_DOUBLETAP,
        BTN_TOOL_TRIPLETAP,
        BTN_TOOL_QUADTAP,
        BTN_TOOL_QUINTTAP,
    ] {
        spec.key(b);
    }
    spec.caps.prop(INPUT_PROP_POINTER);
    spec.caps.prop(INPUT_PROP_BUTTONPAD);
    add(spec)
}

/// Which contact is in which slot, across a touchpad's frames.
pub struct Touches {
    slots: [Option<u32>; SLOTS],
    next_id: i32,
}

impl Default for Touches {
    fn default() -> Self {
        Self::new()
    }
}

impl Touches {
    pub const fn new() -> Touches {
        Touches { slots: [None; SLOTS], next_id: 0 }
    }

    /// One frame of contacts (every finger on the pad now).
    pub fn frame(&mut self, d: &Device, frame: &aurora_hid::touchpad::Frame) {
        let down: alloc::vec::Vec<&aurora_hid::touchpad::Contact> =
            frame.contacts.iter().filter(|c| c.tip && c.confident).collect();
        // Lifted fingers leave their slots.
        for (slot, entry) in self.slots.iter_mut().enumerate() {
            if let Some(id) = *entry {
                if !down.iter().any(|c| c.id == id) {
                    *entry = None;
                    d.emit(EV_ABS, ABS_MT_SLOT, slot as i32);
                    d.emit(EV_ABS, ABS_MT_TRACKING_ID, -1);
                }
            }
        }
        for c in &down {
            let slot = match self.slots.iter().position(|s| *s == Some(c.id)) {
                Some(s) => s,
                None => {
                    let Some(free) = self.slots.iter().position(|s| s.is_none()) else { continue };
                    self.slots[free] = Some(c.id);
                    d.emit(EV_ABS, ABS_MT_SLOT, free as i32);
                    d.emit(EV_ABS, ABS_MT_TRACKING_ID, self.next_id);
                    self.next_id = (self.next_id + 1) & 0xFFFF;
                    free
                }
            };
            d.emit(EV_ABS, ABS_MT_SLOT, slot as i32);
            d.emit(EV_ABS, ABS_MT_POSITION_X, c.x);
            d.emit(EV_ABS, ABS_MT_POSITION_Y, c.y);
        }
        let n = down.len();
        d.emit(EV_KEY, BTN_TOUCH, (n > 0) as i32);
        for (count, tool) in
            [(1, BTN_TOOL_FINGER), (2, BTN_TOOL_DOUBLETAP), (3, BTN_TOOL_TRIPLETAP), (4, BTN_TOOL_QUADTAP)]
        {
            d.emit(EV_KEY, tool, (n == count) as i32);
        }
        d.emit(EV_KEY, BTN_TOOL_QUINTTAP, (n >= 5) as i32);
        if let Some(first) = down.first() {
            d.emit(EV_ABS, ABS_X, first.x);
            d.emit(EV_ABS, ABS_Y, first.y);
        }
        d.emit(EV_KEY, BTN_LEFT, frame.button as i32);
        d.sync();
    }
}
