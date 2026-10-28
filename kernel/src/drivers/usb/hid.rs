//! USB HID: keyboards, mice, tablets, touchpads and game controllers.
//!
//! Each HID interface sends input reports laid out by its report
//! descriptor; `aurora_hid` parses the descriptor and reads the reports.
//! One interface may hold several devices at once (a wireless receiver's
//! keyboard, mouse and media keys share one), so a [`HidDevice`] plays every
//! role its descriptor has:
//!
//! - keys: any rollover (boot six-key or N-key bitmaps), media and power
//!   keys, held keys repeating in software, and the Caps Lock light;
//! - pointer: relative mice (buttons 4 and 5 are Back and Forward, two
//!   wheels) and absolute tablets and virtual-machine pointers;
//! - touchpad: Precision Touchpads, switched into reporting contacts, with
//!   gestures (taps, two-finger scrolling, …) from `aurora_hid::gestures`;
//! - game controller: the standard layout, for apps (`drivers::gamepad`).
//!
//! Xbox controllers aren't HID: [`attach_xbox`] speaks their protocols.

use super::{Endpoint, Interface, PipeHandler, Setup, UsbDevice, DESC_REPORT};
use crate::drivers::block::Dma;
use crate::drivers::input::{self, InputEvent, KeyCode, KeyEvent, Modifiers};
use crate::drivers::{gamepad, keymap};
use crate::sync::{IrqMutex, Mutex};
use alloc::sync::{Arc, Weak};
use alloc::vec;
use alloc::vec::Vec;
use aurora_hid::gamepad::HidGamepad;
use aurora_hid::gestures::Gestures;
use aurora_hid::{keyboard, pointer, touchpad, usage, Descriptor};

const SET_REPORT: u8 = 0x09;
const SET_IDLE: u8 = 0x0A;
const SET_PROTOCOL: u8 = 0x0B;
const REPORT_OUTPUT: u16 = 2;
const REPORT_FEATURE: u16 = 3;

/// The boot keyboard and mouse descriptors (HID 1.11, Appendix B): what a
/// device whose own descriptor we can't read sends in the boot protocol.
const BOOT_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95,
    0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05,
    0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19,
    0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
];
const BOOT_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x09, 0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x15, 0x00, 0x25,
    0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31,
    0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06, 0xC0, 0xC0,
];

// ------------------------------------------------------------------- keys

fn modifiers(held: &[u32], caps: bool) -> (Modifiers, bool) {
    let has = |id: u16| held.contains(&usage::u(usage::KEYBOARD_PAGE, id));
    let mods = Modifiers {
        shift: has(0xE1) || has(0xE5),
        ctrl: has(0xE0) || has(0xE4),
        alt: has(0xE2),
        super_key: has(0xE3) || has(0xE7),
        caps,
    };
    // Right Alt is AltGr; Ctrl+Alt works as AltGr too (as on Windows).
    let altgr = has(0xE6) || (mods.ctrl && mods.alt);
    (mods, altgr)
}

/// Non-character keys of the keyboard page.
fn special(id: u16) -> Option<(KeyCode, Option<char>)> {
    Some(match id {
        0x28 | 0x58 => (KeyCode::Enter, Some('\n')),
        0x29 => (KeyCode::Escape, None),
        0x2A => (KeyCode::Backspace, None),
        0x2B => (KeyCode::Tab, Some('\t')),
        0x39 => (KeyCode::CapsLock, None),
        0x3A..=0x45 => (KeyCode::function((id - 0x39) as u8), None),
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
        0x59..=0x61 => (KeyCode::Char, Some((b'1' + (id - 0x59) as u8) as char)),
        0x62 => (KeyCode::Char, Some('0')),
        0x63 => (KeyCode::Char, Some('.')),
        0x65 => (KeyCode::Menu, None),
        0x66 => (KeyCode::Power, None),
        0x7F => (KeyCode::Mute, None),
        0x80 => (KeyCode::VolumeUp, None),
        0x81 => (KeyCode::VolumeDown, None),
        0xE0 | 0xE4 => (KeyCode::Ctrl, None),
        0xE1 | 0xE5 => (KeyCode::Shift, None),
        0xE2 => (KeyCode::Alt, None),
        0xE3 | 0xE7 => (KeyCode::Super, None),
        _ => return None,
    })
}

/// Media, browser and power keys (consumer and system control pages).
fn media(u: u32) -> Option<KeyCode> {
    Some(match u {
        usage::VOLUME_UP => KeyCode::VolumeUp,
        usage::VOLUME_DOWN => KeyCode::VolumeDown,
        usage::MUTE => KeyCode::Mute,
        usage::PLAY_PAUSE | usage::STOP => KeyCode::PlayPause,
        usage::NEXT_TRACK => KeyCode::NextTrack,
        usage::PREVIOUS_TRACK => KeyCode::PrevTrack,
        usage::BRIGHTNESS_UP => KeyCode::BrightnessUp,
        usage::BRIGHTNESS_DOWN => KeyCode::BrightnessDown,
        usage::AC_BACK => KeyCode::Back,
        usage::AC_FORWARD => KeyCode::Forward,
        usage::SYSTEM_SLEEP => KeyCode::Sleep,
        usage::SYSTEM_POWER_DOWN => KeyCode::Power,
        _ => return None,
    })
}

/// A key going down or up as a `KeyEvent` (`None`: unknown or a dead key).
fn key_event(u: u32, pressed: bool, held: &[u32]) -> Option<KeyEvent> {
    let (mods, altgr) = modifiers(held, input::caps());
    if usage::page(u) != usage::KEYBOARD_PAGE {
        return media(u).map(|code| KeyEvent { code, ch: None, pressed, mods });
    }
    let id = usage::id(u);
    if id == 0xE6 {
        return None; // AltGr only changes the layer
    }
    if let Some((code, ch)) = special(id) {
        return Some(KeyEvent { code, ch: if pressed { ch } else { None }, pressed, mods });
    }
    if !matches!(id, 0x04..=0x27 | 0x2C..=0x38 | 0x64) {
        return None;
    }
    let ch = if !pressed {
        None
    } else if id == 0x2C {
        Some(keymap::space())
    } else {
        Some(keymap::translate(id as u8, mods.shift, mods.caps, altgr)?)
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

struct Keys {
    keys: keyboard::Keys,
    /// The held key that repeats (its usage and event), and when it next fires.
    repeat: Option<(u32, KeyEvent, u64)>,
    timing: (u64, u64),
}

impl Keys {
    fn feed(&mut self, d: &Descriptor, report: &[u8]) -> Vec<KeyEvent> {
        let changes = self.keys.feed(d, report);
        if changes.is_empty() {
            return Vec::new();
        }
        let held = self.keys.all_held();
        let now = crate::time::uptime_ms();
        let mut events = Vec::new();
        for c in changes {
            if c.pressed && c.usage == usage::u(usage::KEYBOARD_PAGE, 0x39) {
                input::toggle_caps();
            }
            let Some(e) = key_event(c.usage, c.pressed, &held) else { continue };
            events.push(e);
            if !c.pressed && self.repeat.is_some_and(|(u, ..)| u == c.usage) {
                self.repeat = None;
            }
            let repeats = !matches!(
                e.code,
                KeyCode::CapsLock
                    | KeyCode::Escape
                    | KeyCode::PrintScreen
                    | KeyCode::Mute
                    | KeyCode::PlayPause
                    | KeyCode::Shift
                    | KeyCode::Ctrl
                    | KeyCode::Alt
                    | KeyCode::Super
                    | KeyCode::Sleep
                    | KeyCode::Power
            );
            if c.pressed {
                if repeats {
                    self.timing = repeat_timing();
                    self.repeat = Some((c.usage, e, now + self.timing.0));
                } else {
                    // A new modifier changes what the held key means.
                    self.repeat = None;
                }
            }
        }
        events
    }

    fn tick(&mut self, now: u64) -> Option<(KeyEvent, u64)> {
        let (u, e, at) = self.repeat?;
        if now >= at {
            self.repeat = Some((u, e, now + self.timing.1));
            return Some((e, self.timing.1));
        }
        None
    }

    fn next_repeat(&self, now: u64) -> Option<u64> {
        self.repeat.map(|(_, _, at)| at.saturating_sub(now).max(1))
    }
}

// --------------------------------------------------------------- pointers

struct Pointer {
    buttons: u8,
    /// Back and forward (bits 0, 1), sent as keys.
    side: u8,
    remainder: (i32, i32),
}

impl Pointer {
    fn feed(&mut self, r: &pointer::Report) {
        let buttons = r.buttons & 0x07;
        if r.absolute {
            let (w, h) = input::screen_size();
            let (px, py) = input::pointer_position();
            let x = r.x.map_or(px, |x| pointer::scale(x, r.x_range, w));
            let y = r.y.map_or(py, |y| pointer::scale(y, r.y_range, h));
            input::push(InputEvent::Pointer { x, y, buttons });
        } else {
            let (dx, dy) = input::scale_motion(r.x.unwrap_or(0), r.y.unwrap_or(0), &mut self.remainder);
            if dx != 0 || dy != 0 || buttons != self.buttons {
                input::pointer_moved(dx, dy, buttons);
            }
        }
        self.buttons = buttons;
        // A HID wheel turns positive away from the user: scrolling up.
        if r.wheel != 0 || r.pan != 0 {
            input::push(InputEvent::Scroll { dx: r.pan * input::NOTCH, dy: -r.wheel * input::NOTCH });
        }
        let side = (r.buttons >> 3) & 0x3;
        let changed = side ^ self.side;
        self.side = side;
        for (bit, code) in [(1u8, KeyCode::Back), (2, KeyCode::Forward)] {
            if changed & bit != 0 {
                let e = KeyEvent { code, ch: None, pressed: side & bit != 0, mods: input::modifiers() };
                input::push(InputEvent::Key(e));
            }
        }
    }
}

// -------------------------------------------------------------- touchpads

struct Touchpad {
    frames: touchpad::Frames,
    gestures: Gestures,
    buttons: u8,
}

impl Touchpad {
    fn apply(&mut self, o: aurora_hid::gestures::Output) {
        let buttons = o.buttons & 0x07;
        if o.dx != 0 || o.dy != 0 || buttons != self.buttons {
            input::pointer_moved(o.dx, o.dy, buttons);
            self.buttons = buttons;
        }
        if o.scroll_x != 0 || o.scroll_y != 0 {
            // Pixels to 1/120 notch, at 40 px a notch.
            input::push(InputEvent::Scroll { dx: o.scroll_x * 3, dy: o.scroll_y * 3 });
        }
    }
}

// ----------------------------------------------------------------- device

/// One HID interface and whatever devices its descriptor holds.
pub struct HidDevice {
    desc: Descriptor,
    keys: Option<IrqMutex<Keys>>,
    pointer: Option<IrqMutex<Pointer>>,
    touchpad: Option<IrqMutex<Touchpad>>,
    gamepad: Option<(HidGamepad, u32)>,
    /// For setting the keyboard's lights: the bus, device and interface.
    bus: &'static super::Bus,
    dev: u8,
    interface: u8,
}

impl HidDevice {
    fn set_lights(&self, caps: bool) {
        let Some(report) = keyboard::led_report(&self.desc, false, caps, false) else { return };
        let id = if self.desc.numbered { report[0] as u16 } else { 0 };
        let mut data = report;
        let setup =
            Setup::class_interface(SET_REPORT, REPORT_OUTPUT << 8 | id, self.interface, data.len() as u16, false);
        let _ = self.bus.control(self.dev, setup, Some(&mut data));
    }
}

impl PipeHandler for HidDevice {
    fn data(&self, report: &[u8]) {
        let d = &self.desc;
        if let Some(k) = &self.keys {
            let events = k.lock().feed(d, report);
            if !events.is_empty() {
                for e in events {
                    input::push(InputEvent::Key(e));
                }
                super::kick_ticks();
            }
        }
        if let Some(t) = &self.touchpad {
            if let Some(r) = touchpad::read(d, report) {
                let mut t = t.lock();
                if let Some(frame) = t.frames.feed(r) {
                    let o = t.gestures.frame(crate::time::uptime_ms(), &frame, &input::touchpad_settings());
                    t.apply(o);
                    super::kick_ticks();
                }
                return;
            }
        }
        if let Some(p) = &self.pointer {
            if let Some(r) = pointer::read(d, report) {
                p.lock().feed(&r);
            }
        }
        if let Some((pad, id)) = &self.gamepad {
            if let Some(s) = pad.read(d, report) {
                gamepad::update(*id, &s);
            }
        }
    }

    fn tick(&self, now: u64) -> Option<u64> {
        let mut next: Option<u64> = None;
        let mut soonest = |ms: u64| next = Some(next.map_or(ms, |n: u64| n.min(ms)));
        if let Some(k) = &self.keys {
            let mut k = k.lock();
            if let Some((e, _)) = k.tick(now) {
                input::push(InputEvent::Key(e));
            }
            if let Some(ms) = k.next_repeat(now) {
                soonest(ms);
            }
        }
        if let Some(t) = &self.touchpad {
            let mut t = t.lock();
            let (out, again) = t.gestures.tick(now);
            if let Some(o) = out {
                t.apply(o);
            }
            if let Some(ms) = again {
                soonest(ms);
            }
        }
        next
    }
}

/// Keyboards, for their Caps Lock lights.
static KEYBOARDS: IrqMutex<Vec<Weak<HidDevice>>> = IrqMutex::new(Vec::new());

fn set_all_lights(caps: bool) {
    let all: Vec<Arc<HidDevice>> = {
        let mut k = KEYBOARDS.lock();
        k.retain(|w| w.strong_count() > 0);
        k.iter().filter_map(|w| w.upgrade()).collect()
    };
    for kb in all {
        kb.set_lights(caps);
    }
}

/// Reads the report descriptor (some devices need a second try).
fn report_descriptor(dev: &UsbDevice, iface: &Interface) -> Option<Vec<u8>> {
    let len = if iface.report_length == 0 { 512 } else { iface.report_length.min(4096) };
    for _ in 0..3 {
        let mut desc = vec![0u8; len as usize];
        if let Ok(n) = dev.control(Setup::get_interface_descriptor(DESC_REPORT, iface.number, len), Some(&mut desc)) {
            if n > 0 {
                desc.truncate(n);
                return Some(desc);
            }
        }
        crate::sched::sleep_ms(10);
    }
    None
}

fn set_report(dev: &UsbDevice, iface: u8, kind: u16, report: &[u8], numbered: bool) -> bool {
    let id = if numbered { report[0] as u16 } else { 0 };
    let mut data = report.to_vec();
    let setup = Setup::class_interface(SET_REPORT, kind << 8 | id, iface, data.len() as u16, false);
    dev.control(setup, Some(&mut data)).is_ok()
}

/// The roles a descriptor gives, as a name for the device list.
fn role_name(keys: bool, pointer: Option<bool>, touchpad: bool, pad: bool) -> &'static str {
    match (keys, pointer, touchpad, pad) {
        (_, _, true, _) => "touchpad",
        (_, _, _, true) => "game controller",
        (true, Some(_), ..) => "keyboard and mouse",
        (true, None, ..) => "keyboard",
        // Absolute pointers: tablets and virtual machines' pointers.
        (false, Some(true), ..) => "tablet",
        (false, Some(false), ..) => "mouse",
        _ => "HID",
    }
}

pub fn attach(dev: &UsbDevice, iface: &Interface) -> Option<&'static str> {
    let ep_in = *iface.endpoints.iter().find(|e| e.kind == 3 && e.is_in())?;
    let ep_out = iface.endpoints.iter().find(|e| e.kind == 3 && !e.is_in()).copied();
    let boot = iface.subclass == 1;
    let parsed = report_descriptor(dev, iface).and_then(|d| Descriptor::parse(&d).ok());
    let desc = match parsed {
        Some(d) => {
            if boot {
                // Firmware may have left it in the boot protocol.
                let _ = dev.control(Setup::class_interface(SET_PROTOCOL, 1, iface.number, 0, false), None);
            }
            d
        }
        // Unreadable or unparsable: boot devices still work in the boot protocol.
        None if boot && matches!(iface.protocol, 1 | 2) => {
            let _ = dev.control(Setup::class_interface(SET_PROTOCOL, 0, iface.number, 0, false), None);
            Descriptor::parse(if iface.protocol == 1 { BOOT_KEYBOARD } else { BOOT_MOUSE }).ok()?
        }
        None => return None,
    };
    let is_touchpad = touchpad::is_touchpad(&desc);
    let has_keys = keyboard::held(&desc, &desc.new_report(aurora_hid::Kind::Input, 0)).is_some()
        || desc.report_ids(aurora_hid::Kind::Input).iter().any(|&id| {
            let r = desc.new_report(aurora_hid::Kind::Input, id);
            keyboard::held(&desc, &r).is_some()
        });
    if has_keys {
        // Report only on changes (we repeat keys ourselves).
        let _ = dev.control(Setup::class_interface(SET_IDLE, 0, iface.number, 0, false), None);
    }
    let touch = if is_touchpad {
        // Switch to reporting contacts; turn on surface and button reports.
        if let Some(r) = touchpad::input_mode_report(&desc) {
            if !set_report(dev, iface.number, REPORT_FEATURE, &r, desc.numbered) {
                log!("usb", "{}: the touchpad did not take the input mode", dev.name);
            }
        }
        if let Some(r) = touchpad::selective_reporting_report(&desc) {
            set_report(dev, iface.number, REPORT_FEATURE, &r, desc.numbered);
        }
        touchpad::info(&desc).map(|info| {
            IrqMutex::new(Touchpad { frames: touchpad::Frames::new(), gestures: Gestures::new(&info), buttons: 0 })
        })
    } else {
        None
    };
    let pointer = (!is_touchpad && pointer::is_pointer(&desc))
        .then(|| IrqMutex::new(Pointer { buttons: 0, side: 0, remainder: (0, 0) }));
    let pad = HidGamepad::new(&desc, dev.desc.vendor, dev.desc.product);

    let eps: Vec<Endpoint> = core::iter::once(ep_in).chain(ep_out).collect();
    if !dev.bus.configure(dev.dev, &eps, None) {
        return None;
    }
    let pad_id = pad.as_ref().map(|p| {
        let family = match p.layout() {
            aurora_hid::gamepad::Layout::PlayStation => aurora_abi::gamepad::PLAYSTATION,
            aurora_hid::gamepad::Layout::SwitchPro => aurora_abi::gamepad::NINTENDO,
            _ => aurora_abi::gamepad::GENERIC,
        };
        let motors: Option<Arc<dyn gamepad::Rumble>> = (family == aurora_abi::gamepad::PLAYSTATION).then(|| {
            Arc::new(PlayStationMotors {
                out: OutPipe::new(dev, ep_out),
                iface: iface.number,
                product: dev.desc.product,
            }) as Arc<dyn gamepad::Rumble>
        });
        gamepad::add(&gamepad::name(&dev.name), dev.desc.vendor, dev.desc.product, family, motors)
    });
    let absolute = desc.find(aurora_hid::Kind::Input, usage::X).is_some_and(|(f, _)| !f.is_relative());
    let name = role_name(has_keys, pointer.as_ref().map(|_| absolute), touch.is_some(), pad_id.is_some());
    let device = Arc::new(HidDevice {
        keys: has_keys
            .then(|| IrqMutex::new(Keys { keys: keyboard::Keys::new(), repeat: None, timing: repeat_timing() })),
        pointer,
        touchpad: touch,
        gamepad: pad.zip(pad_id),
        bus: dev.bus,
        dev: dev.dev,
        interface: iface.number,
        desc,
    });
    if has_keys && device.desc.find(aurora_hid::Kind::Output, usage::LED_CAPS_LOCK).is_some() {
        KEYBOARDS.lock().push(Arc::downgrade(&device));
        input::add_light_sink(set_all_lights);
        input::request_lights();
    }
    // Unplugged: let go of held keys and buttons, forget the controller.
    let gone = device.clone();
    dev.bus.on_detach(
        dev.dev,
        alloc::boxed::Box::new(move || {
            if let Some(k) = &gone.keys {
                let held = k.lock().keys.all_held();
                for u in held.iter().copied() {
                    if let Some(e) = key_event(u, false, &held) {
                        input::push(InputEvent::Key(e));
                    }
                }
            }
            if gone.pointer.as_ref().is_some_and(|p| p.lock().buttons != 0)
                || gone.touchpad.as_ref().is_some_and(|t| t.lock().buttons != 0)
            {
                input::pointer_moved(0, 0, 0);
            }
            if let Some((_, id)) = gone.gamepad {
                gamepad::remove(id);
            }
        }),
    );
    dev.bus.open_pipe(dev.dev, &ep_in, device).then_some(name)
}

// ------------------------------------------------------- output reports

/// An interrupt OUT endpoint (rumble, lights), with a buffer it can reach.
struct OutPipe {
    bus: &'static super::Bus,
    dev: u8,
    ep: Option<Endpoint>,
    buf: Mutex<Option<Dma>>,
}

impl OutPipe {
    fn new(dev: &UsbDevice, ep: Option<Endpoint>) -> OutPipe {
        OutPipe { bus: dev.bus, dev: dev.dev, ep, buf: Mutex::new(dev.bus.dma(64)) }
    }

    /// Sends `data` on the OUT endpoint; false if there is none (or it failed).
    fn send(&self, data: &[u8]) -> bool {
        let Some(ep) = self.ep else { return false };
        let mut buf = self.buf.lock();
        let Some(b) = buf.as_mut() else { return false };
        let n = data.len().min(64);
        b.bytes_mut()[..n].copy_from_slice(&data[..n]);
        self.bus.transfer(self.dev, &ep, b.phys, n, 500).is_ok()
    }
}

struct PlayStationMotors {
    out: OutPipe,
    iface: u8,
    product: u16,
}

impl gamepad::Rumble for PlayStationMotors {
    fn rumble(&self, strong: u8, weak: u8) {
        let report = aurora_hid::gamepad::playstation_rumble(self.product, strong, weak);
        if !self.out.send(&report) {
            let mut data = report.clone();
            let setup = Setup::class_interface(
                SET_REPORT,
                REPORT_OUTPUT << 8 | report[0] as u16,
                self.iface,
                data.len() as u16,
                false,
            );
            let _ = self.out.bus.control(self.out.dev, setup, Some(&mut data));
        }
    }
}

// ------------------------------------------------------------------ Xbox

/// Which Xbox protocol a controller speaks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Xbox {
    X360,
    One,
}

struct XboxPad {
    kind: Xbox,
    id: u32,
    state: IrqMutex<aurora_hid::gamepad::State>,
}

impl PipeHandler for XboxPad {
    fn data(&self, r: &[u8]) {
        let mut st = self.state.lock();
        let next = match self.kind {
            Xbox::X360 => aurora_hid::gamepad::xbox360::read(r),
            Xbox::One => aurora_hid::gamepad::xbox_one::read(r, &st),
        };
        if let Some(s) = next {
            *st = s;
            drop(st);
            gamepad::update(self.id, &s);
        }
    }
}

struct XboxMotors {
    kind: Xbox,
    out: OutPipe,
    sequence: core::sync::atomic::AtomicU8,
}

impl gamepad::Rumble for XboxMotors {
    fn rumble(&self, strong: u8, weak: u8) {
        match self.kind {
            Xbox::X360 => {
                self.out.send(&aurora_hid::gamepad::xbox360::rumble(strong, weak));
            }
            Xbox::One => {
                let seq = self.sequence.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                self.out.send(&aurora_hid::gamepad::xbox_one::rumble(seq, strong, weak));
            }
        }
    }
}

/// Xbox 360 (class 0xFF/0x5D/0x01) and Xbox One/Series (0xFF/0x47/0xD0)
/// controllers.
pub fn attach_xbox(dev: &UsbDevice, iface: &Interface) -> Option<&'static str> {
    let kind = if iface.subclass == 0x5D { Xbox::X360 } else { Xbox::One };
    // The Xbox One's first interface is the controller; later ones are audio.
    if kind == Xbox::One && iface.number != 0 {
        return None;
    }
    let ep_in = *iface.endpoints.iter().find(|e| e.kind == 3 && e.is_in())?;
    let ep_out = iface.endpoints.iter().find(|e| e.kind == 3 && !e.is_in()).copied();
    let eps: Vec<Endpoint> = core::iter::once(ep_in).chain(ep_out).collect();
    if !dev.bus.configure(dev.dev, &eps, None) {
        return None;
    }
    let out = OutPipe::new(dev, ep_out);
    match kind {
        // Light the ring around the guide button for player 1.
        Xbox::X360 => out.send(&aurora_hid::gamepad::xbox360::led(1)),
        // Nothing comes until the controller is told to start.
        Xbox::One => out.send(&aurora_hid::gamepad::xbox_one::start()),
    };
    let motors: Arc<dyn gamepad::Rumble> =
        Arc::new(XboxMotors { kind, out, sequence: core::sync::atomic::AtomicU8::new(1) });
    let name = if dev.name.starts_with("USB device") { "Xbox Controller" } else { dev.name.as_str() };
    let id = gamepad::add(name, dev.desc.vendor, dev.desc.product, aurora_abi::gamepad::XBOX, Some(motors));
    dev.bus.on_detach(dev.dev, alloc::boxed::Box::new(move || gamepad::remove(id)));
    let pad = Arc::new(XboxPad { kind, id, state: IrqMutex::new(Default::default()) });
    dev.bus.open_pipe(dev.dev, &ep_in, pad).then_some("game controller")
}

#[cfg(feature = "ktest")]
pub fn parse_pointer_for_test(desc: &[u8]) -> Option<(bool, u32, u32, u32)> {
    let d = Descriptor::parse(desc).ok()?;
    let (x, xe) = d.find(aurora_hid::Kind::Input, usage::X)?;
    let (y, ye) = d.find(aurora_hid::Kind::Input, usage::Y)?;
    Some((!x.is_relative(), x.offset + xe * x.size, y.offset + ye * y.size, x.size))
}
