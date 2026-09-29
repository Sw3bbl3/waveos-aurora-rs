//! USB HID: keyboards (boot protocol), mice and absolute pointers such as
//! tablets and virtual-machine pointers (from their report descriptors).
//!
//! Keyboards report HID usages, which the shared layouts (`keymap`) turn
//! into characters, exactly as for PS/2. USB keyboards don't repeat keys by
//! themselves, so held keys repeat in software with the Keyboard settings.

use super::xhci::PipeHandler;
use super::{Interface, Setup, UsbDevice, DESC_REPORT};
use crate::drivers::input::{self, InputEvent, KeyCode, KeyEvent, Modifiers};
use crate::drivers::keymap;
use crate::sync::IrqMutex;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;

// ------------------------------------------------------------------ keyboard

struct KeyboardState {
    keys: [u8; 6],
    mods: u8,
    caps: bool,
    /// The held key that repeats (its usage and event), and when it next fires.
    repeat: Option<(u8, KeyEvent, u64)>,
}

struct Keyboard {
    state: IrqMutex<KeyboardState>,
}

fn modifiers(bits: u8, caps: bool) -> Modifiers {
    Modifiers {
        shift: bits & 0x22 != 0,
        ctrl: bits & 0x11 != 0,
        alt: bits & 0x04 != 0,
        super_key: bits & 0x88 != 0,
        caps,
    }
}

/// Non-character keys by usage.
fn special(usage: u8) -> Option<(KeyCode, Option<char>)> {
    Some(match usage {
        0x28 | 0x58 => (KeyCode::Enter, Some('\n')),
        0x29 => (KeyCode::Escape, None),
        0x2A => (KeyCode::Backspace, None),
        0x2B => (KeyCode::Tab, Some('\t')),
        0x39 => (KeyCode::CapsLock, None),
        0x3A..=0x45 => (KeyCode::function(usage - 0x39), None),
        0x46 => (KeyCode::PrintScreen, None),
        0x49 => (KeyCode::Insert, None),
        0x4A => (KeyCode::Home, None),
        0x4B => (KeyCode::PageUp, None),
        0x4C => (KeyCode::Delete, None),
        0x4D => (KeyCode::End, None),
        0x4E => (KeyCode::PageDown, None),
        0x4F => (KeyCode::Right, None),
        0x50 => (KeyCode::Left, None),
        0x51 => (KeyCode::Down, None),
        0x52 => (KeyCode::Up, None),
        0x54 => (KeyCode::Char, Some('/')),
        0x55 => (KeyCode::Char, Some('*')),
        0x56 => (KeyCode::Char, Some('-')),
        0x57 => (KeyCode::Char, Some('+')),
        0x59..=0x61 => (KeyCode::Char, Some((b'1' + usage - 0x59) as char)),
        0x62 => (KeyCode::Char, Some('0')),
        0x63 => (KeyCode::Char, Some('.')),
        0x65 => (KeyCode::Menu, None),
        0x7F => (KeyCode::Mute, None),
        0x80 => (KeyCode::VolumeUp, None),
        0x81 => (KeyCode::VolumeDown, None),
        _ => return None,
    })
}

/// A key press as a `KeyEvent` (`None` for unknown keys and dead keys).
fn key_event(usage: u8, pressed: bool, mods_bits: u8, caps: bool) -> Option<KeyEvent> {
    let mods = modifiers(mods_bits, caps);
    if let Some((code, ch)) = special(usage) {
        return Some(KeyEvent { code, ch: if pressed { ch } else { None }, pressed, mods });
    }
    let printable = matches!(usage, 0x04..=0x27 | 0x2C..=0x38 | 0x64);
    if !printable {
        return None;
    }
    let ch = if !pressed {
        None
    } else if usage == 0x2C {
        Some(keymap::space())
    } else {
        // Right Alt is AltGr; Ctrl+Alt works as AltGr too (as on Windows).
        let altgr = mods_bits & 0x40 != 0 || (mods.ctrl && mods.alt);
        Some(keymap::translate(usage, mods.shift, caps, altgr)?)
    };
    Some(KeyEvent { code: KeyCode::Char, ch, pressed, mods })
}

/// Repeat delay and period (ms) from the Keyboard settings.
fn repeat_timing() -> (u64, u64) {
    use aurora_abi::pref;
    let delay = crate::gui::prefs::get(pref::REPEAT_DELAY).and_then(|v| v.parse::<u64>().ok()).unwrap_or(1).min(3);
    let rate = crate::gui::prefs::get(pref::REPEAT_RATE).and_then(|v| v.parse::<u64>().ok()).unwrap_or(8).min(31);
    // As PS/2 typematic: 250–1000 ms, then 30 down to 2 characters a second.
    (250 * (delay + 1), 1000 * 31 / (930 - 28 * rate))
}

impl PipeHandler for Keyboard {
    fn data(&self, d: &[u8]) {
        if d.len() < 8 || d[2..8].iter().all(|&k| k == 0x01) {
            return; // short, or "too many keys" (rollover)
        }
        let mut st = self.state.lock();
        let now = crate::time::uptime_ms();
        let mut events = Vec::new();
        // Modifier keys come as bits.
        let changed = st.mods ^ d[0];
        for bit in 0..8 {
            if changed & (1 << bit) == 0 {
                continue;
            }
            let pressed = d[0] & (1 << bit) != 0;
            let code = match bit {
                0 | 4 => KeyCode::Ctrl,
                1 | 5 => KeyCode::Shift,
                2 => KeyCode::Alt,
                3 | 7 => KeyCode::Super,
                _ => continue, // Right Alt (AltGr) only changes the layer
            };
            events.push(KeyEvent { code, ch: None, pressed, mods: modifiers(d[0], st.caps) });
        }
        let old = st.keys;
        let new: [u8; 6] = d[2..8].try_into().unwrap();
        for &k in old.iter().filter(|&&k| k > 3 && !new.contains(&k)) {
            if let Some(e) = key_event(k, false, d[0], st.caps) {
                events.push(e);
            }
            if st.repeat.is_some_and(|(u, ..)| u == k) {
                st.repeat = None;
            }
        }
        for &k in new.iter().filter(|&&k| k > 3 && !old.contains(&k)) {
            if k == 0x39 {
                st.caps = !st.caps;
            }
            if let Some(e) = key_event(k, true, d[0], st.caps) {
                events.push(e);
                let repeats =
                    !matches!(e.code, KeyCode::CapsLock | KeyCode::Escape | KeyCode::PrintScreen | KeyCode::Mute);
                st.repeat = repeats.then_some((k, e, now + repeat_timing().0));
            }
        }
        if changed != 0 && st.repeat.is_some() {
            st.repeat = None; // a new modifier changes what the key means
        }
        st.keys = new;
        st.mods = d[0];
        drop(st);
        for e in events {
            input::push(InputEvent::Key(e));
        }
    }

    fn tick(&self, now: u64) -> Option<u64> {
        let mut st = self.state.lock();
        let (usage, e, at) = st.repeat?;
        if now >= at {
            let period = repeat_timing().1;
            st.repeat = Some((usage, e, now + period));
            drop(st);
            input::push(InputEvent::Key(e));
            return Some(period);
        }
        Some(at - now)
    }
}

// ------------------------------------------------------------ report parsing

#[derive(Clone, Copy, Debug, Default)]
struct Field {
    /// Bit offset in the report (after the report id byte, if any).
    offset: u32,
    size: u32,
    min: i32,
    max: i32,
}

impl Field {
    fn read(&self, report: &[u8]) -> i32 {
        let mut v: u32 = 0;
        for i in 0..self.size.min(32) {
            let bit = self.offset + i;
            let byte = report.get((bit / 8) as usize).copied().unwrap_or(0);
            v |= (((byte >> (bit % 8)) & 1) as u32) << i;
        }
        if self.min < 0 && self.size < 32 && v & (1 << (self.size - 1)) != 0 {
            (v | !((1u32 << self.size) - 1)) as i32 // sign-extend
        } else {
            v as i32
        }
    }
}

/// Where a pointing device puts its buttons and axes.
#[derive(Clone, Copy, Debug, Default)]
struct PointerLayout {
    report_id: Option<u8>,
    buttons: Option<Field>,
    button_count: u32,
    x: Option<Field>,
    y: Option<Field>,
    wheel: Option<Field>,
    absolute: bool,
}

/// Finds X, Y, wheel and buttons in a HID report descriptor.
fn parse_pointer(desc: &[u8]) -> Option<PointerLayout> {
    #[derive(Clone, Copy, Default)]
    struct Globals {
        page: u32,
        min: i32,
        max: i32,
        size: u32,
        count: u32,
        id: u8,
    }
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<u32> = Vec::new();
    let (mut umin, mut umax) = (None, None);
    let mut offsets = [0u32; 256];
    let mut out = PointerLayout::default();
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xFE {
            // Long item: skip.
            let len = *desc.get(i + 1)? as usize;
            i += 3 + len;
            continue;
        }
        let size = [0, 1, 2, 4][(prefix & 3) as usize];
        let data = desc.get(i + 1..i + 1 + size)?;
        let mut u = 0u32;
        for (k, b) in data.iter().enumerate() {
            u |= (*b as u32) << (8 * k);
        }
        let s = match size {
            1 => u as u8 as i8 as i32,
            2 => u as u16 as i16 as i32,
            _ => u as i32,
        };
        let (kind, tag) = ((prefix >> 2) & 3, prefix >> 4);
        match (kind, tag) {
            // Main items.
            (0, 0x8) => {
                // Input: constant (bit 0), variable (bit 1), relative (bit 2).
                let var = u & 2 != 0;
                let rel = u & 4 != 0;
                let base = offsets[g.id as usize];
                for n in 0..g.count {
                    let usage = match (usages.get(n as usize).or(usages.last()), umin) {
                        (_, Some(lo)) => lo + n.min(umax.unwrap_or(lo) - lo),
                        (Some(&x), None) => x,
                        _ => 0,
                    };
                    let page = if usage > 0xFFFF { usage >> 16 } else { g.page };
                    let usage = usage & 0xFFFF;
                    let f = Field { offset: base + n * g.size, size: g.size, min: g.min, max: g.max };
                    if u & 1 != 0 || !var {
                        continue;
                    }
                    match (page, usage) {
                        (0x01, 0x30) if out.x.is_none() => {
                            out.x = Some(f);
                            out.absolute = !rel;
                            out.report_id = (g.id != 0).then_some(g.id);
                        }
                        (0x01, 0x31) if out.y.is_none() => out.y = Some(f),
                        (0x01, 0x38) if out.wheel.is_none() => out.wheel = Some(f),
                        (0x09, 1) if out.buttons.is_none() => {
                            out.buttons = Some(f);
                            out.button_count = 1;
                        }
                        (0x09, 2..=8) => out.button_count = out.button_count.max(usage),
                        _ => {}
                    }
                }
                offsets[g.id as usize] = base + g.count * g.size;
                usages.clear();
                umin = None;
                umax = None;
            }
            (0, 0x9) | (0, 0xB) | (0, 0xA) | (0, 0xC) => {
                usages.clear();
                umin = None;
                umax = None;
            }
            // Global items.
            (1, 0x0) => g.page = u,
            (1, 0x1) => g.min = s,
            (1, 0x2) => g.max = if g.min >= 0 && s < 0 { u as i32 } else { s },
            (1, 0x7) => g.size = u,
            (1, 0x8) => g.id = u as u8,
            (1, 0x9) => g.count = u,
            (1, 0xA) => stack.push(g),
            (1, 0xB) => g = stack.pop().unwrap_or(g),
            // Local items.
            // A 4-byte usage carries its page in the high half.
            (2, 0x0) => usages.push(u),
            (2, 0x1) => umin = Some(u),
            (2, 0x2) => umax = Some(u),
            _ => {}
        }
        i += 1 + size;
    }
    (out.x.is_some() && out.y.is_some()).then_some(out)
}

// ------------------------------------------------------------------- pointer

struct Pointer {
    layout: PointerLayout,
}

impl PipeHandler for Pointer {
    fn data(&self, d: &[u8]) {
        let l = &self.layout;
        let report = match l.report_id {
            Some(id) if d.first() != Some(&id) => return,
            Some(_) => &d[1..],
            None => d,
        };
        let mut buttons = 0u8;
        // Buttons are consecutive 1-bit fields from button 1.
        if let Some(b) = l.buttons {
            for n in 0..l.button_count.min(3) {
                let f = Field { offset: b.offset + n, size: 1, min: 0, max: 1 };
                if f.read(report) != 0 {
                    buttons |= 1 << n;
                }
            }
        }
        let (Some(fx), Some(fy)) = (l.x, l.y) else { return };
        let (w, h) = (
            input::SCREEN_W.load(core::sync::atomic::Ordering::Relaxed),
            input::SCREEN_H.load(core::sync::atomic::Ordering::Relaxed),
        );
        let (x, y) = if l.absolute {
            let scale = |f: Field, v: i32, extent: i32| {
                let range = (f.max - f.min).max(1) as i64;
                (((v - f.min) as i64 * (extent - 1) as i64) / range) as i32
            };
            (scale(fx, fx.read(report), w), scale(fy, fy.read(report), h))
        } else {
            let (px, py) = input::pointer_position();
            ((px + fx.read(report)).clamp(0, w - 1), (py + fy.read(report)).clamp(0, h - 1))
        };
        // Wheel up is positive in HID; the desktop scrolls down for positive.
        let wheel = l.wheel.map_or(0, |f| -f.read(report).clamp(-127, 127)) as i8;
        input::push(InputEvent::Pointer { x, y, buttons, wheel });
    }
}

// -------------------------------------------------------------------- attach

pub fn attach(dev: &UsbDevice, iface: &Interface) -> Option<&'static str> {
    let ep = *iface.endpoints.iter().find(|e| e.kind == 3 && e.is_in())?;
    let boot = iface.subclass == 1;
    let keyboard = boot && iface.protocol == 1;
    let layout = if keyboard {
        None
    } else {
        let mut desc = vec![0u8; iface.report_length.clamp(1, 4096) as usize];
        let len = desc.len() as u16;
        let n =
            dev.control(Setup::get_interface_descriptor(DESC_REPORT, iface.number, len), Some(&mut desc)).unwrap_or(0);
        match parse_pointer(&desc[..n]) {
            Some(l) => Some(l),
            // A boot mouse whose descriptor we don't understand: use the boot format.
            None if boot && iface.protocol == 2 => {
                dev.control(Setup::class_interface(SET_PROTOCOL, 0, iface.number, 0, false), None).ok()?;
                Some(PointerLayout {
                    report_id: None,
                    buttons: Some(Field { offset: 0, size: 1, min: 0, max: 1 }),
                    button_count: 3,
                    x: Some(Field { offset: 8, size: 8, min: -127, max: 127 }),
                    y: Some(Field { offset: 16, size: 8, min: -127, max: 127 }),
                    wheel: (ep.max_packet >= 4).then_some(Field { offset: 24, size: 8, min: -127, max: 127 }),
                    absolute: false,
                })
            }
            None => return None,
        }
    };
    if keyboard {
        // Boot protocol (fixed 8-byte reports); report only on changes.
        let _ = dev.control(Setup::class_interface(SET_PROTOCOL, 0, iface.number, 0, false), None);
        let _ = dev.control(Setup::class_interface(SET_IDLE, 0, iface.number, 0, false), None);
    }
    if !dev.ctrl.configure(dev.slot, dev.speed, &[ep], None) {
        return None;
    }
    let (handler, name): (Arc<dyn PipeHandler>, &'static str) = match layout {
        None => (
            Arc::new(Keyboard {
                state: IrqMutex::new(KeyboardState { keys: [0; 6], mods: 0, caps: false, repeat: None }),
            }),
            "keyboard",
        ),
        Some(l) => {
            let name = if l.absolute { "tablet" } else { "mouse" };
            (Arc::new(Pointer { layout: l }), name)
        }
    };
    dev.ctrl.open_pipe(dev.slot, &ep, handler).then_some(name)
}

#[cfg(feature = "ktest")]
pub fn parse_pointer_for_test(desc: &[u8]) -> Option<(bool, u32, u32, u32)> {
    parse_pointer(desc).map(|l| (l.absolute, l.x.unwrap().offset, l.y.unwrap().offset, l.x.unwrap().size))
}
