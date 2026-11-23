//! `/dev/uinput`: devices a program makes (Steam Input makes a virtual
//! controller this way; remote-desktop tools a keyboard and mouse). The
//! program describes the device (`UI_SET_*BIT`, `UI_DEV_SETUP`,
//! `UI_ABS_SETUP`, or the old `struct uinput_user_dev` written), creates
//! it (`UI_DEV_CREATE`), then writes its events. The device is a real one
//! to everybody:
//!
//! - to Linux programs, a `/dev/input/eventN` (and `jsN`) like any other;
//! - to WaveOS, a game controller (in `drivers::gamepad`, so Wave Play and
//!   WaveOS programs see it), or keys and pointer motion in its own input,
//!   which go to the focused window as the keyboard's and mouse's do.
//!
//! What other programs send the device comes back to its maker as events
//! to read: force-feedback plays (`EV_FF`), lights (`EV_LED`), and the
//! `EV_UINPUT` requests to upload or erase an effect, which the maker
//! answers with `UI_BEGIN_FF_UPLOAD` / `UI_END_FF_UPLOAD` (and `_ERASE`)
//! while the uploading program waits (30 seconds at most, as on Linux).

use super::super::chardev::{self, ioc_dir, ioc_nr, ioc_size, ioc_type, put_str, CharDev};
use super::tree::Dev;
use crate::drivers::evdev::{self, codes::*, pad, AbsInfo, Caps, Class, Device, Effect, InputId, Location, Spec};
use crate::drivers::{gamepad, input};
use crate::proc::readiness;
use crate::sync::{IrqMutex, Mutex, WaitQueue};
use crate::syscall::user;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const UINPUT_VERSION: u32 = 5;
const EV_UINPUT: u16 = 0x0101;
const UI_FF_UPLOAD: u16 = 1;
const UI_FF_ERASE: u16 = 2;
/// `struct uinput_user_dev`.
const USER_DEV_SIZE: usize = 80 + 8 + 4 + 4 * 64 * 4;
/// How long an uploading program waits for the maker to answer.
const REQUEST_MS: u64 = 30_000;

enum Ask {
    Upload { effect: Effect, old: Option<Effect> },
    Erase(i16),
}

struct Request {
    id: u32,
    ask: Ask,
    retval: Option<i32>,
}

/// What the maker and its device share.
struct Shared {
    /// Events for the maker to read: (type, code, value).
    q: IrqMutex<VecDeque<(u16, u16, i32)>>,
    requests: IrqMutex<Vec<Request>>,
    next_request: AtomicU32,
    answered: WaitQueue,
    /// The maker still has its descriptor open.
    alive: AtomicBool,
}

impl Shared {
    fn key(&self) -> usize {
        readiness::key(self)
    }

    fn forward(&self, ty: u16, code: u16, value: i32) {
        {
            let mut q = self.q.lock();
            if q.len() >= 1024 {
                q.pop_front();
            }
            q.push_back((ty, code, value));
        }
        readiness::notify(self.key());
    }

    /// Asks the maker and waits for its answer.
    fn ask(&self, ask: Ask) -> Result<(), isize> {
        if !self.alive.load(Ordering::Acquire) {
            return Err(ENODEV);
        }
        let id = self.next_request.fetch_add(1, Ordering::Relaxed);
        let code = if matches!(ask, Ask::Upload { .. }) { UI_FF_UPLOAD } else { UI_FF_ERASE };
        self.requests.lock().push(Request { id, ask, retval: None });
        self.forward(EV_UINPUT, code, id as i32);
        let deadline = crate::time::uptime_ms() + REQUEST_MS;
        let answer = |s: &Self| s.requests.lock().iter().find(|r| r.id == id).and_then(|r| r.retval);
        loop {
            if let Some(r) = answer(self) {
                break self.done(id, r);
            }
            if !self.alive.load(Ordering::Acquire) || crate::time::uptime_ms() >= deadline {
                self.requests.lock().retain(|r| r.id != id);
                break Err(if self.alive.load(Ordering::Acquire) { ETIMEDOUT } else { ENODEV });
            }
            if crate::proc::interrupted() {
                self.requests.lock().retain(|r| r.id != id);
                break Err(EINTR);
            }
            self.answered.wait(100, || answer(self).is_some() || !self.alive.load(Ordering::Acquire));
        }
    }

    fn done(&self, id: u32, retval: i32) -> Result<(), isize> {
        self.requests.lock().retain(|r| r.id != id);
        if retval < 0 {
            Err(retval as isize)
        } else {
            Ok(())
        }
    }
}

/// What other programs send the device: to its maker.
struct ToMaker(Arc<Shared>);

impl evdev::Sink for ToMaker {
    fn upload(&self, effect: &Effect, old: Option<&Effect>) -> Result<(), isize> {
        self.0.ask(Ask::Upload { effect: *effect, old: old.copied() })
    }

    fn erase(&self, id: i16) -> Result<(), isize> {
        self.0.ask(Ask::Erase(id))
    }

    fn play(&self, effect: &Effect, count: i32, _gain: u16) {
        self.0.forward(EV_FF, effect.id() as u16, count);
    }

    fn written(&self, ty: u16, code: u16, value: i32) {
        self.0.forward(ty, code, value);
    }
}

/// The device as WaveOS's own input sees it.
#[derive(Default)]
struct Native {
    /// A controller: its id, kind and state.
    pad: Option<(u32, pad::Flavor, aurora_hid::gamepad::State)>,
    /// Keyboard usages held (for modifiers).
    held: Vec<u32>,
    /// Pointer: buttons held, motion and wheels of this report, position.
    buttons: u8,
    motion: (i32, i32),
    wheel: (i32, i32),
    position: (Option<i32>, Option<i32>),
}

struct Setup {
    caps: Caps,
    abs: [AbsInfo; ABS_CNT],
    id: InputId,
    name: String,
    phys: String,
    ff_max: usize,
    configured: bool,
}

pub struct Uinput {
    pid: u32,
    shared: Arc<Shared>,
    setup: Mutex<Setup>,
    dev: Mutex<Option<Arc<Device>>>,
    native: IrqMutex<Native>,
}

impl Uinput {
    pub fn new(pid: u32) -> Arc<Uinput> {
        Arc::new(Uinput {
            pid,
            shared: Arc::new(Shared {
                q: IrqMutex::new(VecDeque::new()),
                requests: IrqMutex::new(Vec::new()),
                next_request: AtomicU32::new(1),
                answered: WaitQueue::new(),
                alive: AtomicBool::new(true),
            }),
            setup: Mutex::new(Setup {
                caps: Caps::default(),
                abs: [AbsInfo::default(); ABS_CNT],
                id: InputId::default(),
                name: String::new(),
                phys: String::new(),
                ff_max: 0,
                configured: false,
            }),
            dev: Mutex::new(None),
            native: IrqMutex::new(Native::default()),
        })
    }

    fn device(&self) -> Option<Arc<Device>> {
        self.dev.lock().clone()
    }

    /// `UI_DEV_CREATE`.
    fn create(&self) -> Result<(), isize> {
        let mut slot = self.dev.lock();
        if slot.is_some() {
            return Err(EINVAL);
        }
        let s = self.setup.lock();
        if !s.configured || s.name.is_empty() {
            return Err(EINVAL);
        }
        if s.caps.ev & (1 << EV_FF) != 0 && s.ff_max == 0 {
            return Err(EINVAL);
        }
        let mut abs = Vec::new();
        for code in s.caps.abs_codes() {
            let a = s.abs[code as usize];
            if a.minimum > a.maximum {
                return Err(EINVAL);
            }
            abs.push((code, a));
        }
        let class = classify(&s.caps);
        let mut spec = Spec::new(&s.name, s.id, class, Location::Virtual);
        spec.caps = s.caps.clone();
        spec.abs = abs;
        spec.phys = s.phys.clone();
        spec.ff_max = s.ff_max;
        spec.sink = Some(Arc::new(ToMaker(self.shared.clone())));
        let (name, id) = (s.name.clone(), s.id);
        drop(s);
        let d = evdev::add(spec);
        if class == Class::Joystick {
            let family = pad::family_of_vendor(id.vendor);
            let pad_id = gamepad::add_at(&name, id.vendor, id.product, family, None, gamepad::Linux::Already);
            let flavor = pad::flavor(family, id.vendor, id.product);
            self.native.lock().pad = Some((pad_id, flavor, Default::default()));
        }
        log!("input", "uinput: process {} made \"{}\" (event{})", self.pid, name, d.num);
        *slot = Some(d);
        Ok(())
    }

    /// `UI_DEV_DESTROY`, or the maker closing its descriptor.
    fn destroy(&self) -> Result<(), isize> {
        let d = self.dev.lock().take().ok_or(EINVAL)?;
        evdev::remove(&d);
        let mut n = self.native.lock();
        if let Some((id, ..)) = n.pad.take() {
            drop(n);
            gamepad::remove(id);
            n = self.native.lock();
        }
        let held = core::mem::take(&mut n.held);
        let buttons = core::mem::take(&mut n.buttons);
        drop(n);
        for u in held.iter().copied() {
            if let Some(e) = crate::drivers::usb::hid::key_event(u, false, &held) {
                input::push(input::InputEvent::Key(e));
            }
        }
        if buttons != 0 {
            input::pointer_moved(0, 0, 0);
        }
        Ok(())
    }

    /// The old way to describe the device: `struct uinput_user_dev` written.
    fn legacy_setup(&self, data: &[u8]) -> Result<usize, isize> {
        if data.len() != USER_DEV_SIZE {
            return Err(EINVAL);
        }
        let mut s = self.setup.lock();
        let name = &data[..80];
        let end = name.iter().position(|&b| b == 0).unwrap_or(80);
        s.name = String::from_utf8_lossy(&name[..end]).into_owned();
        let u16_at = |o: usize| u16::from_le_bytes([data[o], data[o + 1]]);
        s.id = InputId { bustype: u16_at(80), vendor: u16_at(82), product: u16_at(84), version: u16_at(86) };
        s.ff_max = u32::from_le_bytes(data[88..92].try_into().unwrap()) as usize;
        let i32_at = |o: usize| i32::from_le_bytes(data[o..o + 4].try_into().unwrap());
        for (code, a) in s.abs.iter_mut().enumerate() {
            a.maximum = i32_at(92 + code * 4);
            a.minimum = i32_at(92 + 256 + code * 4);
            a.fuzz = i32_at(92 + 512 + code * 4);
            a.flat = i32_at(92 + 768 + code * 4);
        }
        s.configured = true;
        Ok(data.len())
    }

    /// One event from the maker: to the device's readers, and to WaveOS.
    fn event(&self, d: &Device, ty: u16, code: u16, value: i32) {
        if ty == EV_SYN && code == SYN_REPORT {
            d.sync();
            self.native_report(d);
            return;
        }
        d.emit(ty, code, value);
        if d.has(ty, code) {
            self.native_event(d, ty, code, value);
        }
    }

    fn native_event(&self, d: &Device, ty: u16, code: u16, value: i32) {
        let mut n = self.native.lock();
        if let Some((_, flavor, st)) = &mut n.pad {
            match ty {
                EV_KEY => {
                    if let Some(bit) = pad::standard_bit(*flavor, code) {
                        if value != 0 {
                            st.buttons |= bit;
                        } else {
                            st.buttons &= !bit;
                        }
                    }
                }
                EV_ABS => {
                    let a = d.abs_info(code, true);
                    let span = (a.maximum as i64 - a.minimum as i64).max(1);
                    let unit = ((value as i64 - a.minimum as i64) * 65535 / span).clamp(0, 65535);
                    let stick = (unit - 32768) as i16;
                    let set = |st: &mut aurora_hid::gamepad::State, neg: u32, pos: u32| {
                        st.buttons &= !(neg | pos);
                        if value < 0 {
                            st.buttons |= neg;
                        } else if value > 0 {
                            st.buttons |= pos;
                        }
                    };
                    use aurora_abi::gamepad as abi;
                    match code {
                        ABS_X => st.axes[0] = stick,
                        ABS_Y => st.axes[1] = stick,
                        ABS_RX => st.axes[2] = stick,
                        ABS_RY => st.axes[3] = stick,
                        ABS_Z | 0x0a => st.triggers[0] = unit as u16,
                        ABS_RZ | 0x09 => st.triggers[1] = unit as u16,
                        ABS_HAT0X => set(st, abi::LEFT, abi::RIGHT),
                        ABS_HAT0Y => set(st, abi::UP, abi::DOWN),
                        _ => {}
                    }
                }
                _ => {}
            }
            return;
        }
        match (ty, code) {
            (EV_KEY, BTN_LEFT..=BTN_EXTRA) => {
                let bit = 1u8 << (code - BTN_LEFT);
                let before = n.buttons;
                n.buttons = if value != 0 { n.buttons | bit } else { n.buttons & !bit };
                // Side buttons are Back and Forward to WaveOS.
                if code >= BTN_SIDE && (before ^ n.buttons) & bit != 0 {
                    let code = if code == BTN_SIDE { input::KeyCode::Back } else { input::KeyCode::Forward };
                    let e = input::KeyEvent { code, ch: None, pressed: value != 0, mods: input::modifiers() };
                    input::push(input::InputEvent::Key(e));
                }
            }
            (EV_KEY, k) if k < BTN_MISC && value != 2 => {
                let Some(u) = key_to_hid(k) else { return };
                if value != 0 {
                    if !n.held.contains(&u) {
                        n.held.push(u);
                    }
                    if k == KEY_CAPSLOCK {
                        input::toggle_caps();
                    }
                } else {
                    n.held.retain(|&h| h != u);
                }
                let mut held = n.held.clone();
                if value == 0 {
                    held.push(u);
                }
                drop(n);
                if let Some(e) = crate::drivers::usb::hid::key_event(u, value != 0, &held) {
                    input::push(input::InputEvent::Key(e));
                }
            }
            (EV_REL, REL_X) => n.motion.0 += value,
            (EV_REL, REL_Y) => n.motion.1 += value,
            (EV_REL, REL_WHEEL) => n.wheel.1 += value,
            (EV_REL, REL_HWHEEL) => n.wheel.0 += value,
            (EV_ABS, ABS_X) if d.class == Class::Tablet => n.position.0 = Some(value),
            (EV_ABS, ABS_Y) if d.class == Class::Tablet => n.position.1 = Some(value),
            _ => {}
        }
    }

    /// The end of a report: the controller's new state, or the pointer's.
    fn native_report(&self, d: &Device) {
        let mut n = self.native.lock();
        if let Some((id, _, st)) = n.pad {
            drop(n);
            gamepad::update(id, &st);
            return;
        }
        let buttons = n.buttons & 0x07;
        let motion = core::mem::take(&mut n.motion);
        let wheel = core::mem::take(&mut n.wheel);
        let position = core::mem::take(&mut n.position);
        drop(n);
        if !matches!(d.class, Class::Mouse | Class::Tablet | Class::Keyboard) {
            return;
        }
        if position.0.is_some() || position.1.is_some() {
            let (w, h) = input::screen_size();
            let (px, py) = input::pointer_position();
            let scale = |v: Option<i32>, code: u16, extent: i32, keep: i32| {
                let a = d.abs_info(code, true);
                v.map_or(keep, |v| aurora_hid::pointer::scale(v, (a.minimum, a.maximum), extent))
            };
            let (x, y) = (scale(position.0, ABS_X, w, px), scale(position.1, ABS_Y, h, py));
            input::push(input::InputEvent::Pointer { x, y, buttons });
        } else if motion != (0, 0) || d.has(EV_KEY, BTN_LEFT) {
            input::pointer_moved(motion.0, motion.1, buttons);
        }
        if wheel != (0, 0) {
            input::push(input::InputEvent::Scroll { dx: wheel.0 * input::NOTCH, dy: -wheel.1 * input::NOTCH });
        }
    }

    /// `UI_BEGIN_FF_UPLOAD` / `_ERASE`: the request's details for the maker.
    fn begin(&self, arg: u64, upload: bool) -> Result<u64, isize> {
        let id: u32 = user::get(arg)?;
        let reqs = self.shared.requests.lock();
        let r = reqs.iter().find(|r| r.id == id).ok_or(EINVAL)?;
        match (&r.ask, upload) {
            (Ask::Upload { effect, old }, true) => {
                let mut b = [0u8; 104];
                b[0..4].copy_from_slice(&id.to_le_bytes());
                b[8..56].copy_from_slice(&effect.raw);
                if let Some(o) = old {
                    b[56..104].copy_from_slice(&o.raw);
                }
                drop(reqs);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            (Ask::Erase(effect), false) => {
                let mut b = [0u8; 12];
                b[0..4].copy_from_slice(&id.to_le_bytes());
                b[8..12].copy_from_slice(&(*effect as u32).to_le_bytes());
                drop(reqs);
                user::put_bytes(arg, &b).map(|_| 0)
            }
            _ => Err(EINVAL),
        }
    }

    /// `UI_END_FF_UPLOAD` / `_ERASE`: the maker's answer.
    fn end(&self, arg: u64) -> Result<u64, isize> {
        let [id, retval]: [u32; 2] = user::get(arg)?;
        let mut reqs = self.shared.requests.lock();
        let r = reqs.iter_mut().find(|r| r.id == id).ok_or(EINVAL)?;
        r.retval = Some(retval as i32);
        drop(reqs);
        self.shared.answered.wake_all();
        Ok(0)
    }
}

/// What a device described this way is.
fn classify(c: &Caps) -> Class {
    let key = |k| c.has(EV_KEY, k);
    let abs = |a| c.has(EV_ABS, a);
    let pad_buttons = (BTN_JOYSTICK..BTN_DIGI).any(key);
    if pad_buttons && abs(ABS_X) {
        Class::Joystick
    } else if abs(ABS_MT_POSITION_X) && key(BTN_TOUCH) && key(BTN_TOOL_FINGER) {
        Class::Touchpad
    } else if abs(ABS_X) && key(BTN_LEFT) {
        Class::Tablet
    } else if c.has(EV_REL, REL_X) && key(BTN_LEFT) {
        Class::Mouse
    } else if (KEY_ESC..=KEY_CAPSLOCK).filter(|&k| key(k)).count() > 20 {
        Class::Keyboard
    } else if pad_buttons {
        Class::Joystick
    } else {
        Class::Key
    }
}

impl Drop for Uinput {
    fn drop(&mut self) {
        let _ = self.destroy();
        self.shared.alive.store(false, Ordering::Release);
        self.shared.answered.wake_all();
    }
}

impl CharDev for Uinput {
    fn read(&self, out: &mut [u8], nonblock: bool) -> Result<usize, isize> {
        if out.len() < super::evdev::EVENT_SIZE {
            return Err(EINVAL);
        }
        let events = chardev::wait(self.shared.key(), nonblock, || {
            let mut q = self.shared.q.lock();
            let n = (out.len() / super::evdev::EVENT_SIZE).min(q.len());
            Ok((n > 0).then(|| q.drain(..n).collect::<Vec<_>>()))
        })?;
        let ns = crate::time::realtime_ns();
        for (i, (ty, code, value)) in events.iter().enumerate() {
            let o = &mut out[i * 24..i * 24 + 24];
            o[0..8].copy_from_slice(&((ns / 1_000_000_000) as i64).to_le_bytes());
            o[8..16].copy_from_slice(&(((ns % 1_000_000_000) / 1000) as i64).to_le_bytes());
            o[16..18].copy_from_slice(&ty.to_le_bytes());
            o[18..20].copy_from_slice(&code.to_le_bytes());
            o[20..24].copy_from_slice(&value.to_le_bytes());
        }
        Ok(events.len() * 24)
    }

    fn write(&self, data: &[u8], _nonblock: bool) -> Result<usize, isize> {
        let Some(d) = self.device() else { return self.legacy_setup(data) };
        if data.len() < 24 {
            return Err(EINVAL);
        }
        let mut done = 0;
        for e in data.as_chunks::<24>().0 {
            let ty = u16::from_le_bytes([e[16], e[17]]);
            let code = u16::from_le_bytes([e[18], e[19]]);
            let value = i32::from_le_bytes([e[20], e[21], e[22], e[23]]);
            self.event(&d, ty, code, value);
            done += 24;
        }
        Ok(done)
    }

    fn ioctl(&self, r: u32, arg: u64) -> Result<u64, isize> {
        if ioc_type(r) != b'U' as u32 {
            return Err(ENOTTY);
        }
        let created = self.device().is_some();
        let bit = |ty: u16| -> Result<u64, isize> {
            if created {
                return Err(EINVAL);
            }
            let code = arg as u16;
            let mut s = self.setup.lock();
            match ty {
                0 if arg <= EV_MAX as u64 => s.caps.ev |= 1 << arg,
                0 => return Err(EINVAL),
                EV_REP => {}
                _ => {
                    let ev = s.caps.ev;
                    s.caps.set(ty, code);
                    // Setting a code doesn't set its type (UI_SET_EVBIT does).
                    s.caps.ev = ev;
                }
            }
            Ok(0)
        };
        match (ioc_dir(r), ioc_nr(r)) {
            // UI_DEV_CREATE, UI_DEV_DESTROY.
            (0, 1) => self.create().map(|_| 0),
            (0, 2) => self.destroy().map(|_| 0),
            // UI_DEV_SETUP: { input_id; char name[80]; u32 ff_effects_max }.
            (1, 3) => {
                if created {
                    return Err(EINVAL);
                }
                let mut b = [0u8; 92];
                user::get_bytes(arg, &mut b)?;
                let mut s = self.setup.lock();
                let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
                s.id = InputId { bustype: u16_at(0), vendor: u16_at(2), product: u16_at(4), version: u16_at(6) };
                let name = &b[8..88];
                let end = name.iter().position(|&c| c == 0).ok_or(EINVAL)?;
                s.name = String::from_utf8_lossy(&name[..end]).into_owned();
                s.ff_max = u32::from_le_bytes(b[88..92].try_into().unwrap()) as usize;
                s.configured = true;
                Ok(0)
            }
            // UI_ABS_SETUP: { u16 code; input_absinfo }.
            (1, 4) => {
                if created {
                    return Err(EINVAL);
                }
                let mut b = [0u8; 28];
                user::get_bytes(arg, &mut b)?;
                let code = u16::from_le_bytes([b[0], b[1]]) as usize;
                if code >= ABS_CNT {
                    return Err(EINVAL);
                }
                let v = |i: usize| i32::from_le_bytes(b[4 + i * 4..8 + i * 4].try_into().unwrap());
                let a = AbsInfo { value: v(0), minimum: v(1), maximum: v(2), fuzz: v(3), flat: v(4), resolution: v(5) };
                if a.minimum > a.maximum {
                    return Err(EINVAL);
                }
                let mut s = self.setup.lock();
                s.abs[code] = a;
                let ev = s.caps.ev;
                s.caps.set(EV_ABS, code as u16);
                s.caps.ev = ev | 1 << EV_ABS;
                Ok(0)
            }
            // UI_GET_SYSNAME(len), UI_GET_VERSION.
            (2, 44) => {
                let d = self.device().ok_or(ENOENT)?;
                put_str(arg, ioc_size(r), &alloc::format!("input{}", d.input))
            }
            (2, 45) => user::put(arg, &UINPUT_VERSION).map(|_| 0),
            // UI_SET_EVBIT … UI_SET_PROPBIT, UI_SET_PHYS.
            (1, 100) => bit(0),
            (1, 101) => bit(EV_KEY),
            (1, 102) => bit(EV_REL),
            (1, 103) => bit(EV_ABS),
            (1, 104) => bit(EV_MSC),
            (1, 105) => bit(EV_LED),
            (1, 106) => bit(EV_SND),
            (1, 107) => bit(EV_FF),
            (1, 108) => {
                if created {
                    return Err(EINVAL);
                }
                self.setup.lock().phys = crate::syscall::user::cstr(arg)?;
                Ok(0)
            }
            (1, 109) => bit(EV_SW),
            (1, 110) => {
                if created || arg >= PROP_CNT as u64 {
                    return Err(EINVAL);
                }
                self.setup.lock().caps.prop(arg as u16);
                Ok(0)
            }
            // UI_BEGIN_FF_UPLOAD, UI_END_FF_UPLOAD, UI_BEGIN_FF_ERASE, UI_END_FF_ERASE.
            (3, 200) => self.begin(arg, true),
            (1, 201) | (1, 203) => self.end(arg),
            (3, 202) => self.begin(arg, false),
            _ => Err(EINVAL),
        }
    }

    fn readiness(&self) -> (bool, bool, bool) {
        (!self.shared.q.lock().is_empty(), true, false)
    }

    fn wait_key(&self) -> usize {
        self.shared.key()
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        super::node_stat(Dev::Uinput)
    }
}
