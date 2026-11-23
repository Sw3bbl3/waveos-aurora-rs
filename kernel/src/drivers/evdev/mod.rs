//! Input devices the way Linux describes them: one [`Device`] per keyboard,
//! pointer, touchpad and game controller, with Linux's event codes, what
//! it can send (capability bitmaps), its current state, and the programs
//! reading it ([`Client`]s, and joystick-API readers, [`JoyClient`]s).
//!
//! The native drivers keep feeding WaveOS's own input (`drivers::input`,
//! `drivers::gamepad`) and, beside it, their device here: [`Device::emit`]
//! a change, [`Device::sync`] at the end of each report. The Linux layer
//! (`compat::linux::input`) shows the devices as `/dev/input/event*` and
//! `/dev/input/js*`, with sysfs and udev records around them.
//!
//! - **Frames.** Events are delivered a report at a time: a client sees a
//!   report only once its `SYN_REPORT` is in, as on Linux. A client that
//!   falls [`QUEUE`] events behind loses what it has not read and gets
//!   `SYN_DROPPED` and the newest event instead (Linux's rule).
//! - **Who reads what.** Keyboards, pointers and touchpads reach only the
//!   process whose window has the keyboard focus ([`set_focus`]);
//!   controllers follow Wave Play's rule (`gamepad::may_read`). A client
//!   that may not read sees the device at rest: when it loses the focus it
//!   is sent releases for whatever was held (sticks centred, fingers
//!   lifted), and when it gains it, the state as it is.
//! - **Grabs** (`EVIOCGRAB`) make one client the only reader among
//!   programs; WaveOS's own input is not affected, so no program can take
//!   the keyboard away from the desktop.
//! - **Key repeat** is generated here (value 2) for devices with `EV_REP`.
//! - **Hot-plug**: devices coming and going are told to the Linux layer
//!   (`compat::linux::input`: uevents and inotify).

pub mod codes;
pub mod hidraw;
pub mod pad;
pub mod sources;

use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;
use codes::*;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// Events a client may have waiting (a power of two, as Linux's buffers).
pub const QUEUE: usize = 256;
/// Effects a device with force feedback keeps (Linux's memless limit).
pub const FF_EFFECTS: usize = 16;
/// Multi-touch slots kept per device, and the per-slot codes
/// (`ABS_MT_TOUCH_MAJOR` to `ABS_MT_TRACKING_ID`).
const MT_SLOTS: usize = 16;
const MT_CODES: usize = (ABS_MT_TRACKING_ID - ABS_MT_FIRST + 1) as usize;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputId {
    pub bustype: u16,
    pub vendor: u16,
    pub product: u16,
    pub version: u16,
}

/// `struct input_absinfo`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AbsInfo {
    pub value: i32,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
    pub resolution: i32,
}

impl AbsInfo {
    pub const fn range(minimum: i32, maximum: i32, fuzz: i32, flat: i32) -> AbsInfo {
        AbsInfo { value: 0, minimum, maximum, fuzz, flat, resolution: 0 }
    }
}

/// What a device can send, by event type (the `EVIOCGBIT` bitmaps).
#[derive(Clone, Debug, Default)]
pub struct Caps {
    pub ev: u32,
    pub key: [u64; KEY_CNT / 64],
    pub rel: u64,
    pub abs: u64,
    pub msc: u64,
    pub led: u64,
    pub snd: u64,
    pub ff: [u64; 2],
    pub sw: u64,
    pub prop: u64,
}

impl Caps {
    /// Adds event `code` of type `ty` (and the type itself).
    pub fn set(&mut self, ty: u16, code: u16) {
        self.ev |= 1 << ty;
        let c = code as usize;
        match ty {
            EV_KEY if c < KEY_CNT => self.key[c / 64] |= 1 << (c % 64),
            EV_REL if c < REL_CNT => self.rel |= 1 << c,
            EV_ABS if c < ABS_CNT => self.abs |= 1 << c,
            EV_MSC if c < MSC_CNT => self.msc |= 1 << c,
            EV_LED if c < LED_CNT => self.led |= 1 << c,
            EV_SND if c < SND_CNT => self.snd |= 1 << c,
            EV_FF if c < FF_CNT => self.ff[c / 64] |= 1 << (c % 64),
            EV_SW if c < SW_CNT => self.sw |= 1 << c,
            _ => {}
        }
    }

    pub fn prop(&mut self, p: u16) {
        self.prop |= 1 << p;
    }

    pub fn has(&self, ty: u16, code: u16) -> bool {
        let c = code as usize;
        if ty > EV_MAX || self.ev & (1 << ty) == 0 {
            return false;
        }
        match ty {
            EV_SYN => true,
            EV_KEY => c < KEY_CNT && self.key[c / 64] & (1 << (c % 64)) != 0,
            EV_REL => c < REL_CNT && self.rel & (1 << c) != 0,
            EV_ABS => c < ABS_CNT && self.abs & (1 << c) != 0,
            EV_MSC => c < MSC_CNT && self.msc & (1 << c) != 0,
            EV_LED => c < LED_CNT && self.led & (1 << c) != 0,
            EV_SND => c < SND_CNT && self.snd & (1 << c) != 0,
            EV_FF => c < FF_CNT && self.ff[c / 64] & (1 << (c % 64)) != 0,
            EV_SW => c < SW_CNT && self.sw & (1 << c) != 0,
            EV_REP => true,
            _ => false,
        }
    }

    /// Keys in `range` it has.
    pub fn keys(&self, from: u16, to: u16) -> impl Iterator<Item = u16> + '_ {
        (from..to).filter(move |&k| self.has(EV_KEY, k))
    }

    pub fn abs_codes(&self) -> impl Iterator<Item = u16> + '_ {
        (0..ABS_CNT as u16).filter(move |&a| self.abs & (1 << a) != 0)
    }
}

/// What a device is, for udev's `ID_INPUT_*` properties and for who may
/// read it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Keyboard,
    Mouse,
    Touchpad,
    /// An absolute pointer (a tablet, a virtual machine's pointer).
    Tablet,
    Joystick,
    /// Buttons only (power, media).
    Key,
}

/// Where a USB device is, for sysfs and the stable names.
#[derive(Clone, Debug, Default)]
pub struct UsbPlace {
    /// Bus number (1-based, as Linux counts).
    pub bus: u8,
    /// Port path: the root port, then hub ports ("1", "1.3").
    pub ports: String,
    pub interface: u8,
    pub vendor: u16,
    pub product: u16,
    /// `bcdDevice`.
    pub version: u16,
    pub maker: String,
    pub model: String,
    pub serial: String,
    /// The host controller's PCI address ("0000:00:04.0"), if known.
    pub pci: String,
    /// The HID device's number on the HID bus (`0003:VVVV:PPPP.NNNN`).
    pub hid: u32,
}

/// Where a device is attached.
#[derive(Clone, Debug)]
pub enum Location {
    /// Made in software (`/dev/uinput`, the keyboard-played controller).
    Virtual,
    /// The PS/2 controller's port 0 (keyboard) or 1 (mouse).
    Serio(u8),
    Usb(UsbPlace),
    /// HID over I²C, by its ACPI name.
    I2c(String),
}

/// One force-feedback effect as uploaded (`struct ff_effect`, 48 bytes).
#[derive(Clone, Copy, Debug)]
pub struct Effect {
    pub raw: [u8; 48],
}

impl Effect {
    pub fn kind(&self) -> u16 {
        u16::from_le_bytes([self.raw[0], self.raw[1]])
    }
    pub fn id(&self) -> i16 {
        i16::from_le_bytes([self.raw[2], self.raw[3]])
    }
    pub fn set_id(&mut self, id: i16) {
        self.raw[2..4].copy_from_slice(&id.to_le_bytes());
    }
    /// How long one play lasts (ms; 0: until stopped).
    pub fn length(&self) -> u16 {
        u16::from_le_bytes([self.raw[10], self.raw[11]])
    }
    /// Strong and weak motor, 0–0xFFFF: a rumble's magnitudes, or a
    /// periodic effect's magnitude on both (as Linux's memless drivers).
    pub fn motors(&self) -> (u16, u16) {
        let u = |o: usize| u16::from_le_bytes([self.raw[o], self.raw[o + 1]]);
        match self.kind() {
            FF_RUMBLE => (u(16), u(18)),
            FF_PERIODIC => {
                let m = (i16::from_le_bytes([self.raw[20], self.raw[21]]) as i32).unsigned_abs().min(0x7FFF) as u16;
                (m * 2, m * 2)
            }
            _ => (0, 0),
        }
    }
}

/// What a device does with what programs send it.
pub trait Sink: Send + Sync {
    /// A new or changed effect (`old`: the one it replaces).
    fn upload(&self, _effect: &Effect, _old: Option<&Effect>) -> Result<(), isize> {
        Ok(())
    }
    fn erase(&self, _id: i16) -> Result<(), isize> {
        Ok(())
    }
    /// Effect `effect` starts (`count` plays) or stops (0), at `gain` (0–0xFFFF).
    fn play(&self, _effect: &Effect, _count: i32, _gain: u16) {}
    /// A light (`EV_LED`), a sound, the gain, or anything else written.
    fn written(&self, _ty: u16, _code: u16, _value: i32) {}
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    /// Monotonic time (ns).
    pub ns: u64,
    pub ty: u16,
    pub code: u16,
    pub value: i32,
}

struct State {
    key: [u64; KEY_CNT / 64],
    led: u64,
    sw: u64,
    abs: [AbsInfo; ABS_CNT],
    /// Each slot's tracking id (touchpads), for releasing them.
    mt: [i32; MT_SLOTS],
    /// Each slot's multi-touch values (`ABS_MT_FIRST` on).
    mt_values: [[i32; MT_CODES]; MT_SLOTS],
    /// The slot values go to, and the last slot sent.
    mt_slot: i32,
    mt_sent: i32,
    rep: [u32; 2],
    /// The report being assembled.
    frame: Vec<(u16, u16, i32)>,
    clients: Vec<Weak<Client>>,
    joy: Vec<Weak<JoyClient>>,
    grab: Option<u64>,
    effects: [Option<(Effect, u64)>; FF_EFFECTS],
    gain: u16,
    /// The repeating key and its timer.
    repeat: Option<(u16, crate::timer::Handle)>,
    gone: bool,
}

pub struct Device {
    /// `eventN`.
    pub num: u32,
    /// `inputN` (never reused).
    pub input: u32,
    /// `jsN`, for game controllers.
    pub js: Option<u32>,
    pub name: String,
    pub phys: String,
    pub uniq: String,
    pub id: InputId,
    pub class: Class,
    pub caps: Caps,
    pub location: Location,
    /// Effects it can hold (0: no force feedback).
    pub ff_max: usize,
    sink: Option<Arc<dyn Sink>>,
    state: IrqMutex<State>,
}

/// Everything about a new device; [`add`] makes it.
pub struct Spec {
    pub name: String,
    pub phys: String,
    pub uniq: String,
    pub id: InputId,
    pub class: Class,
    pub caps: Caps,
    pub abs: Vec<(u16, AbsInfo)>,
    pub location: Location,
    pub ff_max: usize,
    pub sink: Option<Arc<dyn Sink>>,
}

impl Spec {
    pub fn new(name: &str, id: InputId, class: Class, location: Location) -> Spec {
        Spec {
            name: String::from(name),
            phys: String::new(),
            uniq: String::new(),
            id,
            class,
            caps: Caps::default(),
            abs: Vec::new(),
            location,
            ff_max: 0,
            sink: None,
        }
    }

    pub fn key(&mut self, code: u16) -> &mut Spec {
        self.caps.set(EV_KEY, code);
        self
    }

    pub fn rel(&mut self, code: u16) -> &mut Spec {
        self.caps.set(EV_REL, code);
        self
    }

    pub fn abs(&mut self, code: u16, info: AbsInfo) -> &mut Spec {
        self.caps.set(EV_ABS, code);
        self.abs.push((code, info));
        self
    }
}

static DEVICES: IrqMutex<Vec<Arc<Device>>> = IrqMutex::new(Vec::new());
static NEXT_INPUT: AtomicU32 = AtomicU32::new(0);
static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// Devices coming and going: the Linux layer's files change, and programs
/// hear of it (task context, outside the locks).
fn announce(d: &Arc<Device>, added: bool) {
    crate::compat::linux::input::device_changed(d, added);
}

/// The lowest number not in `used`.
fn lowest_free(used: impl Iterator<Item = u32>) -> u32 {
    let mut taken: Vec<u32> = used.collect();
    taken.sort_unstable();
    let mut n = 0;
    for t in taken {
        if t == n {
            n += 1;
        } else if t > n {
            break;
        }
    }
    n
}

/// Registers a device: it appears as `/dev/input/eventN` (and `jsN` for a
/// game controller).
pub fn add(spec: Spec) -> Arc<Device> {
    let mut abs = [AbsInfo::default(); ABS_CNT];
    for (code, info) in &spec.abs {
        if (*code as usize) < ABS_CNT {
            abs[*code as usize] = *info;
        }
    }
    let mut caps = spec.caps;
    caps.ev |= 1 << EV_SYN;
    if spec.ff_max > 0 {
        caps.ev |= 1 << EV_FF;
    }
    let rep = if caps.ev & (1 << EV_REP) != 0 { [250, 33] } else { [0, 0] };
    let joystick = spec.class == Class::Joystick && caps.has(EV_ABS, ABS_X);
    let d = {
        let mut all = DEVICES.lock();
        let num = lowest_free(all.iter().map(|d| d.num));
        let js = joystick.then(|| lowest_free(all.iter().filter_map(|d| d.js)));
        let d = Arc::new(Device {
            num,
            input: NEXT_INPUT.fetch_add(1, Ordering::Relaxed),
            js,
            name: spec.name,
            phys: spec.phys,
            uniq: spec.uniq,
            id: spec.id,
            class: spec.class,
            caps,
            location: spec.location,
            ff_max: spec.ff_max.min(FF_EFFECTS),
            sink: spec.sink,
            state: IrqMutex::new(State {
                key: [0; KEY_CNT / 64],
                led: 0,
                sw: 0,
                abs,
                mt: [-1; MT_SLOTS],
                mt_values: [[0, 0, 0, 0, 0, 0, 0, 0, 0, -1]; MT_SLOTS],
                mt_slot: 0,
                mt_sent: 0,
                rep,
                frame: Vec::with_capacity(64),
                clients: Vec::new(),
                joy: Vec::new(),
                grab: None,
                effects: [None; FF_EFFECTS],
                gain: 0xFFFF,
                repeat: None,
                gone: false,
            }),
        });
        all.push(d.clone());
        d
    };
    log!(
        "input",
        "{} is input{} (event{}{})",
        d.name,
        d.input,
        d.num,
        d.js.map(|j| alloc::format!(", js{j}")).unwrap_or_default()
    );
    announce(&d, true);
    d
}

/// Unregisters a device: readers get `ENODEV` (after a last report
/// letting go of what was held).
pub fn remove(d: &Arc<Device>) {
    d.release_all();
    let found = {
        let mut all = DEVICES.lock();
        let before = all.len();
        all.retain(|x| !Arc::ptr_eq(x, d));
        before != all.len()
    };
    let wake = {
        let mut s = d.state.lock();
        s.gone = true;
        if let Some((_, h)) = s.repeat.take() {
            crate::timer::cancel(h);
        }
        let mut keys: Vec<usize> = s.clients.iter().filter_map(|c| c.upgrade()).map(|c| c.key()).collect();
        keys.extend(s.joy.iter().filter_map(|c| c.upgrade()).map(|c| c.key()));
        keys
    };
    for k in wake {
        crate::proc::readiness::notify(k);
    }
    if found {
        announce(d, false);
    }
}

pub fn devices() -> Vec<Arc<Device>> {
    DEVICES.lock().clone()
}

pub fn by_num(num: u32) -> Option<Arc<Device>> {
    DEVICES.lock().iter().find(|d| d.num == num).cloned()
}

pub fn by_js(js: u32) -> Option<Arc<Device>> {
    DEVICES.lock().iter().find(|d| d.js == Some(js)).cloned()
}

// ------------------------------------------------------------------- focus

/// The processes whose window has the keyboard focus (the window's owner
/// and the program it shows, for windows a bridge draws), or none.
static FOCUS: [AtomicU64; 2] = [AtomicU64::new(NOBODY), AtomicU64::new(NOBODY)];
const NOBODY: u64 = u64::MAX;

/// Says which processes have the keyboard focus (`None`: the desktop or
/// the lock screen has it); devices then tell their readers.
pub fn set_focus(owner: Option<u32>, program: Option<u32>) {
    let o = owner.map_or(NOBODY, |p| p as u64);
    let g = program.map_or(NOBODY, |p| p as u64);
    let changed = FOCUS[0].swap(o, Ordering::AcqRel) != o;
    let changed = FOCUS[1].swap(g, Ordering::AcqRel) != g || changed;
    if changed {
        refocus();
    }
}

/// Whether process `pid` has the keyboard focus.
pub fn has_focus(pid: u32) -> bool {
    FOCUS.iter().any(|f| f.load(Ordering::Acquire) == pid as u64)
}

/// Brings every reader up to date after the focus or the controllers'
/// owner changed.
pub fn refocus() {
    for d in devices() {
        d.refocus();
    }
}

// -------------------------------------------------------------- the device

impl Device {
    /// Whether process `pid` may read this device now (0: the kernel).
    pub fn may_read(&self, pid: u32) -> bool {
        if pid == 0 {
            return true;
        }
        match self.class {
            Class::Joystick => crate::drivers::gamepad::may_read(pid),
            // Unfocused only for a sandboxed app the person let monitor input.
            _ => has_focus(pid) || crate::sandbox::monitors_input(pid),
        }
    }

    pub fn has(&self, ty: u16, code: u16) -> bool {
        self.caps.has(ty, code)
    }

    pub fn gone(&self) -> bool {
        self.state.lock().gone
    }

    /// One change; delivered at the next [`Device::sync`]. Repeated values
    /// and what the device can't send are dropped, as Linux's input core does.
    pub fn emit(&self, ty: u16, code: u16, value: i32) {
        if !self.has(ty, code) && ty != EV_SYN {
            return;
        }
        let mut s = self.state.lock();
        if s.gone {
            return;
        }
        let c = code as usize;
        match ty {
            EV_KEY => {
                let held = s.key[c / 64] & (1 << (c % 64)) != 0;
                if value != 2 && held == (value != 0) {
                    return;
                }
                if value != 2 {
                    s.key[c / 64] ^= 1 << (c % 64);
                    self.autorepeat(&mut s, code, value != 0);
                }
            }
            EV_ABS => {
                // Multi-touch values are kept per slot, and the slot is
                // sent only before a value of another slot than the last
                // (Linux's input core does the same).
                if code == ABS_MT_SLOT {
                    if (0..MT_SLOTS as i32).contains(&value) {
                        s.mt_slot = value;
                    }
                    return;
                }
                if (ABS_MT_FIRST..=ABS_MT_TRACKING_ID).contains(&code) {
                    let (slot, i) = (s.mt_slot as usize, (code - ABS_MT_FIRST) as usize);
                    if s.mt_values[slot][i] == value {
                        return;
                    }
                    s.mt_values[slot][i] = value;
                    if code == ABS_MT_TRACKING_ID {
                        s.mt[slot] = value;
                    }
                    if s.mt_sent != s.mt_slot {
                        s.mt_sent = s.mt_slot;
                        s.abs[ABS_MT_SLOT as usize].value = s.mt_slot;
                        let slot = s.mt_slot;
                        s.frame.push((EV_ABS, ABS_MT_SLOT, slot));
                    }
                    s.frame.push((ty, code, value));
                    return;
                }
                if s.abs[c].value == value {
                    return;
                }
                s.abs[c].value = value;
            }
            EV_REL if value == 0 => return,
            EV_LED | EV_SW => {
                let bits = if ty == EV_LED { &mut s.led } else { &mut s.sw };
                if (*bits & (1 << c) != 0) == (value != 0) {
                    return;
                }
                *bits ^= 1 << c;
            }
            _ => {}
        }
        s.frame.push((ty, code, value));
    }

    /// Ends a report: readers get what changed since the last one.
    pub fn sync(&self) {
        let now = crate::time::now_ns();
        let (keys, joys, restart) = {
            let mut s = self.state.lock();
            if s.frame.is_empty() {
                return;
            }
            let frame = core::mem::take(&mut s.frame);
            let mut keys = Vec::new();
            let grab = s.grab;
            for c in s.clients.iter().filter_map(|c| c.upgrade()) {
                if grab.is_some_and(|g| g != c.id) || !c.focused.load(Ordering::Acquire) {
                    continue;
                }
                let mut q = c.q.lock();
                for &(ty, code, value) in &frame {
                    q.push(Event { ns: now, ty, code, value });
                }
                q.push(Event { ns: now, ty: EV_SYN, code: SYN_REPORT, value: 0 });
                keys.push(c.key());
            }
            let mut joys = Vec::new();
            let mut restart = Vec::new();
            for j in s.joy.iter().filter_map(|j| j.upgrade()) {
                if !j.focused.load(Ordering::Acquire) {
                    continue;
                }
                match j.feed(&frame) {
                    Fed::Nothing => {}
                    Fed::Some => joys.push(j.key()),
                    Fed::Overflow => restart.push(j),
                }
            }
            s.frame = frame;
            s.frame.clear();
            (keys, joys, restart)
        };
        for j in restart {
            j.startup();
            crate::proc::readiness::notify(j.key());
        }
        for k in keys.into_iter().chain(joys) {
            crate::proc::readiness::notify(k);
        }
    }

    /// Lets go of every key and contact (unplugged).
    fn release_all(&self) {
        let held: Vec<u16> = {
            let s = self.state.lock();
            (0..KEY_CNT as u16).filter(|&k| s.key[k as usize / 64] & (1 << (k % 64)) != 0).collect()
        };
        for k in held {
            self.emit(EV_KEY, k, 0);
        }
        self.sync();
    }

    // --------------------------------------------------------- key repeat

    fn autorepeat(&self, s: &mut State, code: u16, down: bool) {
        if self.caps.ev & (1 << EV_REP) == 0 || s.rep[REP_DELAY] == 0 || s.rep[REP_PERIOD] == 0 {
            return;
        }
        if let Some((_, h)) = s.repeat.take() {
            crate::timer::cancel(h);
        }
        if down {
            let h = crate::timer::call_after_ms(s.rep[REP_DELAY] as u64, repeat_due, self.num as u64, "evdev-repeat");
            s.repeat = Some((code, h));
        }
    }

    pub fn repeat_rate(&self) -> [u32; 2] {
        self.state.lock().rep
    }

    pub fn set_repeat_rate(&self, rep: [u32; 2]) {
        let mut s = self.state.lock();
        s.rep = rep;
        if let Some((_, h)) = s.repeat.take() {
            crate::timer::cancel(h);
        }
    }

    // -------------------------------------------------------------- state

    /// Key bitmap, as seen by a client that may (or may not) read.
    pub fn key_bits(&self, focused: bool) -> [u64; KEY_CNT / 64] {
        if focused {
            self.state.lock().key
        } else {
            [0; KEY_CNT / 64]
        }
    }

    pub fn led_bits(&self) -> u64 {
        self.state.lock().led
    }

    pub fn sw_bits(&self) -> u64 {
        self.state.lock().sw
    }

    pub fn abs_info(&self, code: u16, focused: bool) -> AbsInfo {
        let s = self.state.lock();
        let mut a = s.abs[code as usize % ABS_CNT];
        if !focused {
            a.value = self.rest_value(code, &a);
        }
        a
    }

    pub fn set_abs_info(&self, code: u16, info: AbsInfo) {
        if (code as usize) < ABS_CNT && code != ABS_MT_SLOT {
            self.state.lock().abs[code as usize] = info;
        }
    }

    /// Each slot's value of `code` (`EVIOCGMTSLOTS`).
    pub fn mt_values(&self, code: u16) -> Vec<i32> {
        let s = self.state.lock();
        match code {
            ABS_MT_FIRST..=ABS_MT_TRACKING_ID => {
                s.mt_values.iter().map(|v| v[(code - ABS_MT_FIRST) as usize]).collect()
            }
            _ => alloc::vec![0; MT_SLOTS],
        }
    }

    /// Where an axis rests: a controller's sticks in the middle and its
    /// triggers let go; anything else keeps its place.
    fn rest_value(&self, code: u16, a: &AbsInfo) -> i32 {
        if self.class != Class::Joystick {
            return if code == ABS_MT_TRACKING_ID { -1 } else { a.value };
        }
        match code {
            ABS_Z | ABS_RZ => a.minimum,
            _ => (a.minimum + a.maximum) / 2,
        }
    }

    /// Events taking a reader from the device's state to rest (`to_rest`),
    /// or from rest to the state.
    fn transition(&self, to_rest: bool) -> Vec<(u16, u16, i32)> {
        let s = self.state.lock();
        let mut out = Vec::new();
        for k in 0..KEY_CNT as u16 {
            if s.key[k as usize / 64] & (1 << (k % 64)) != 0 {
                out.push((EV_KEY, k, !to_rest as i32));
            }
        }
        for code in self.caps.abs_codes() {
            if (ABS_MT_SLOT..=ABS_MT_TRACKING_ID).contains(&code) {
                continue;
            }
            let a = &s.abs[code as usize];
            let rest = self.rest_value(code, a);
            if rest != a.value {
                out.push((EV_ABS, code, if to_rest { rest } else { a.value }));
            }
        }
        if self.has(EV_ABS, ABS_MT_TRACKING_ID) {
            for (slot, &id) in s.mt.iter().enumerate() {
                if id >= 0 {
                    out.push((EV_ABS, ABS_MT_SLOT, slot as i32));
                    out.push((EV_ABS, ABS_MT_TRACKING_ID, if to_rest { -1 } else { id }));
                }
            }
            if s.mt.iter().any(|&id| id >= 0) {
                // Back to the slot the device's next values assume.
                out.push((EV_ABS, ABS_MT_SLOT, s.mt_sent));
            }
        }
        out
    }

    /// Readers whose right to read changed get the transition.
    fn refocus(&self) {
        let (clients, joys) = {
            let s = self.state.lock();
            (
                s.clients.iter().filter_map(|c| c.upgrade()).collect::<Vec<_>>(),
                s.joy.iter().filter_map(|j| j.upgrade()).collect::<Vec<_>>(),
            )
        };
        let now = crate::time::now_ns();
        for c in clients {
            let may = self.may_read(c.pid);
            if c.focused.swap(may, Ordering::AcqRel) == may {
                continue;
            }
            let events = self.transition(!may);
            if !events.is_empty() {
                let mut q = c.q.lock();
                for (ty, code, value) in events {
                    q.push(Event { ns: now, ty, code, value });
                }
                q.push(Event { ns: now, ty: EV_SYN, code: SYN_REPORT, value: 0 });
                drop(q);
                crate::proc::readiness::notify(c.key());
            }
        }
        for j in joys {
            let may = self.may_read(j.pid);
            if j.focused.swap(may, Ordering::AcqRel) != may {
                let events = self.transition(!may);
                match j.feed(&events) {
                    Fed::Nothing => continue,
                    Fed::Some => {}
                    Fed::Overflow => j.startup(),
                }
                crate::proc::readiness::notify(j.key());
            }
        }
    }

    // ------------------------------------------------------------ readers

    /// A new reader (`/dev/input/eventN` opened by process `pid`).
    pub fn open(self: &Arc<Self>, pid: u32) -> Result<Arc<Client>, isize> {
        let mut s = self.state.lock();
        if s.gone {
            return Err(ENODEV);
        }
        let c = Arc::new(Client {
            dev: self.clone(),
            pid,
            id: NEXT_CLIENT.fetch_add(1, Ordering::Relaxed),
            focused: AtomicBool::new(self.may_read(pid)),
            q: IrqMutex::new(Queue::new()),
        });
        s.clients.retain(|w| w.strong_count() > 0);
        s.clients.push(Arc::downgrade(&c));
        Ok(c)
    }

    /// A joystick-API reader (`/dev/input/jsN`).
    pub fn open_js(self: &Arc<Self>, pid: u32) -> Result<Arc<JoyClient>, isize> {
        if self.js.is_none() {
            return Err(ENODEV);
        }
        let j = Arc::new(JoyClient::new(self.clone(), pid));
        let mut s = self.state.lock();
        if s.gone {
            return Err(ENODEV);
        }
        s.joy.retain(|w| w.strong_count() > 0);
        s.joy.push(Arc::downgrade(&j));
        Ok(j)
    }

    /// `EVIOCGRAB`: `on` makes `client` the only reader; off lets go.
    pub fn grab(&self, client: &Client, on: bool) -> Result<(), isize> {
        let mut s = self.state.lock();
        match (s.grab, on) {
            (Some(_), true) => Err(EBUSY),
            (None, true) => {
                s.grab = Some(client.id);
                Ok(())
            }
            (Some(g), false) if g == client.id => {
                s.grab = None;
                Ok(())
            }
            (_, false) => Err(EINVAL),
        }
    }

    // ---------------------------------------------------- what is written

    /// An event a program wrote to the device (`write` on its node, or a
    /// device made with uinput forwarding to its maker).
    pub fn inject(&self, client: &Client, ty: u16, code: u16, value: i32) {
        // An effect is played by its id, not by a code the device lists.
        let playing = ty == EV_FF && self.caps.ev & (1 << EV_FF) != 0 && code != FF_GAIN;
        if !self.has(ty, code) && !playing {
            return;
        }
        match ty {
            EV_FF if code == FF_GAIN => {
                let gain = value.clamp(0, 0xFFFF) as u16;
                self.state.lock().gain = gain;
                if let Some(sink) = &self.sink {
                    sink.written(ty, code, gain as i32);
                }
            }
            EV_FF => {
                let (effect, gain) = {
                    let s = self.state.lock();
                    match s.effects.get(code as usize).copied().flatten() {
                        Some((e, _)) => (e, s.gain),
                        None => return,
                    }
                };
                if let Some(sink) = &self.sink {
                    sink.play(&effect, value, gain);
                }
            }
            EV_LED => {
                self.emit(ty, code, value);
                self.sync();
                if let Some(sink) = &self.sink {
                    sink.written(ty, code, value);
                }
            }
            EV_SND => {
                if let Some(sink) = &self.sink {
                    sink.written(ty, code, value);
                }
            }
            // Anything else the device sends: its readers see it, as on Linux.
            EV_SYN if code == SYN_REPORT => self.sync(),
            EV_KEY | EV_REL | EV_ABS | EV_MSC | EV_SW if client.focused.load(Ordering::Acquire) => {
                self.emit(ty, code, value)
            }
            _ => {}
        }
    }

    /// `EVIOCSFF`: uploads `effect` (id -1: a new one); returns its id.
    pub fn upload(&self, client: &Client, mut effect: Effect) -> Result<i16, isize> {
        if self.ff_max == 0 {
            return Err(EINVAL);
        }
        if !self.has(EV_FF, effect.kind()) {
            return Err(EINVAL);
        }
        let (id, old) = {
            let mut s = self.state.lock();
            let id = effect.id();
            if id == -1 {
                let free = s.effects[..self.ff_max].iter().position(|e| e.is_none()).ok_or(ENOSPC)?;
                // Reserve the slot while the sink looks at it.
                effect.set_id(free as i16);
                s.effects[free] = Some((effect, client.id));
                (free as i16, None)
            } else {
                let slot = s.effects.get(id as usize).filter(|_| (id as usize) < self.ff_max).ok_or(EINVAL)?;
                match slot {
                    Some((old, owner)) if *owner == client.id => (id, Some(*old)),
                    Some(_) => return Err(EACCES),
                    None => return Err(EINVAL),
                }
            }
        };
        let r = match &self.sink {
            Some(sink) => sink.upload(&effect, old.as_ref()),
            None => Ok(()),
        };
        let mut s = self.state.lock();
        match r {
            Ok(()) => {
                s.effects[id as usize] = Some((effect, client.id));
                Ok(id)
            }
            Err(e) => {
                if old.is_none() {
                    s.effects[id as usize] = None;
                }
                Err(e)
            }
        }
    }

    /// `EVIOCRMFF`.
    pub fn erase(&self, client: &Client, id: i32) -> Result<(), isize> {
        let effect = {
            let s = self.state.lock();
            match s.effects.get(id as usize).filter(|_| (0..self.ff_max as i32).contains(&id)) {
                Some(Some((e, owner))) if *owner == client.id => *e,
                Some(Some(_)) => return Err(EACCES),
                _ => return Err(EINVAL),
            }
        };
        if let Some(sink) = &self.sink {
            sink.play(&effect, 0, 0);
            sink.erase(id as i16)?;
        }
        self.state.lock().effects[id as usize] = None;
        Ok(())
    }

    /// A reader closed: its effects stop and go, its grab ends.
    fn closed(&self, client_id: u64) {
        let mine: Vec<Effect> = {
            let mut s = self.state.lock();
            if s.grab == Some(client_id) {
                s.grab = None;
            }
            s.clients.retain(|w| w.strong_count() > 0);
            let mut v = Vec::new();
            for slot in s.effects.iter_mut() {
                if slot.is_some_and(|(_, o)| o == client_id) {
                    v.push(slot.take().unwrap().0);
                }
            }
            v
        };
        if let Some(sink) = &self.sink {
            for e in mine {
                sink.play(&e, 0, 0);
                let _ = sink.erase(e.id());
            }
        }
    }
}

/// The key-repeat timer: `num` is the device.
fn repeat_due(num: u64) {
    let Some(d) = DEVICES.lock().iter().find(|d| d.num as u64 == num).cloned() else { return };
    let code = {
        let mut s = d.state.lock();
        let Some((code, _)) = s.repeat else { return };
        if s.key[code as usize / 64] & (1 << (code % 64)) == 0 || s.gone {
            s.repeat = None;
            return;
        }
        let h = crate::timer::call_after_ms(s.rep[REP_PERIOD].max(1) as u64, repeat_due, num, "evdev-repeat");
        s.repeat = Some((code, h));
        code
    };
    d.emit(EV_KEY, code, 2);
    d.sync();
}

// ------------------------------------------------------------------ readers

/// A reader's queue, with Linux's packet rule: events after the last
/// `SYN_REPORT` aren't readable yet.
pub struct Queue {
    buf: VecDeque<Event>,
    /// Events at the front that make whole reports.
    complete: usize,
}

impl Queue {
    fn new() -> Queue {
        Queue { buf: VecDeque::with_capacity(QUEUE), complete: 0 }
    }

    fn push(&mut self, e: Event) {
        if self.buf.len() == QUEUE {
            // Too far behind: what wasn't read is lost; the reader learns
            // so (SYN_DROPPED) and starts again from the newest event.
            self.buf.clear();
            self.buf.push_back(Event { ns: e.ns, ty: EV_SYN, code: SYN_DROPPED, value: 0 });
            self.complete = 0;
        }
        self.buf.push_back(e);
        if e.ty == EV_SYN && e.code == SYN_REPORT {
            self.complete = self.buf.len();
        }
    }

    pub fn ready(&self) -> bool {
        self.complete > 0
    }

    /// Takes up to `max` readable events.
    pub fn take(&mut self, max: usize) -> Vec<Event> {
        let n = max.min(self.complete);
        self.complete -= n;
        self.buf.drain(..n).collect()
    }

    /// Drops queued events of type `ty` (what `EVIOCGKEY` and friends do:
    /// the state read replaces them).
    fn flush(&mut self, ty: u16) {
        let before = self.complete;
        let mut kept = 0;
        let mut i = 0;
        self.buf.retain(|e| {
            let keep = e.ty != ty || i >= before;
            if keep && i < before {
                kept += 1;
            }
            i += 1;
            keep
        });
        self.complete = kept;
    }
}

/// A program reading `/dev/input/eventN`.
pub struct Client {
    pub dev: Arc<Device>,
    pub pid: u32,
    id: u64,
    /// May read now (else it sees the device at rest).
    pub focused: AtomicBool,
    pub q: IrqMutex<Queue>,
}

impl Client {
    pub fn key(&self) -> usize {
        crate::proc::readiness::key(self)
    }

    /// `EVIOCGKEY` and friends: the state, and no queued events of `ty`.
    pub fn flush(&self, ty: u16) {
        self.q.lock().flush(ty);
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.dev.closed(self.id);
    }
}

// ---------------------------------------------------------------- joysticks

/// `struct js_event`.
#[derive(Clone, Copy, Debug)]
pub struct JsEvent {
    pub time: u32,
    pub value: i16,
    pub kind: u8,
    pub number: u8,
}

pub const JS_EVENT_BUTTON: u8 = 1;
pub const JS_EVENT_AXIS: u8 = 2;
pub const JS_EVENT_INIT: u8 = 0x80;
const JS_QUEUE: usize = 64;

enum Fed {
    Nothing,
    Some,
    Overflow,
}

/// The joystick API's view of a device: buttons and axes numbered from 0,
/// axes scaled to ±32767 through each axis's correction.
pub struct JoyState {
    /// Axis number → abs code, button number → key code.
    pub axes: Vec<u16>,
    pub buttons: Vec<u16>,
    /// Correction per axis: [lower flat, upper flat, scale below, scale above].
    pub corr: Vec<[i32; 4]>,
    /// The last value each axis was reported at (corrected).
    values: Vec<i16>,
    q: VecDeque<JsEvent>,
}

pub struct JoyClient {
    pub dev: Arc<Device>,
    pub pid: u32,
    pub focused: AtomicBool,
    pub st: IrqMutex<JoyState>,
}

/// Linux's default correction for an axis.
pub fn default_corr(a: &AbsInfo) -> [i32; 4] {
    let mid = (a.maximum + a.minimum) / 2;
    let half = (a.maximum - a.minimum) / 2 - 2 * a.flat;
    let scale = if half > 0 { (1 << 29) / half } else { 0 };
    [mid - a.flat, mid + a.flat, scale, scale]
}

fn correct(v: i32, c: &[i32; 4]) -> i16 {
    let out = if v > c[1] {
        ((c[3] as i64 * (v - c[1]) as i64) >> 14) as i32
    } else if v < c[0] {
        ((c[2] as i64 * (v - c[0]) as i64) >> 14) as i32
    } else {
        0
    };
    out.clamp(-32767, 32767) as i16
}

impl JoyClient {
    fn new(dev: Arc<Device>, pid: u32) -> JoyClient {
        let axes: Vec<u16> = dev.caps.abs_codes().collect();
        let buttons: Vec<u16> = dev.caps.keys(BTN_MISC, KEY_CNT as u16).chain(dev.caps.keys(0, BTN_MISC)).collect();
        let corr = axes.iter().map(|&a| default_corr(&dev.abs_info(a, true))).collect();
        let focused = dev.may_read(pid);
        let j = JoyClient {
            dev,
            pid,
            focused: AtomicBool::new(focused),
            st: IrqMutex::new(JoyState {
                values: alloc::vec![0; axes.len()],
                axes,
                buttons,
                corr,
                q: VecDeque::with_capacity(JS_QUEUE),
            }),
        };
        j.startup();
        j
    }

    pub fn key(&self) -> usize {
        crate::proc::readiness::key(self)
    }

    fn now_ms() -> u32 {
        crate::time::uptime_ms() as u32
    }

    /// The opening events: every button and axis as it is (`JS_EVENT_INIT`).
    pub fn startup(&self) {
        let focused = self.focused.load(Ordering::Acquire);
        let keys = self.dev.key_bits(focused);
        let mut st = self.st.lock();
        let st = &mut *st;
        st.q.clear();
        let t = Self::now_ms();
        for (i, &k) in st.buttons.iter().enumerate() {
            let on = keys[k as usize / 64] & (1 << (k % 64)) != 0;
            st.q.push_back(JsEvent {
                time: t,
                value: on as i16,
                kind: JS_EVENT_BUTTON | JS_EVENT_INIT,
                number: i as u8,
            });
        }
        for (i, &a) in st.axes.iter().enumerate() {
            let v = correct(self.dev.abs_info(a, focused).value, &st.corr[i]);
            st.values[i] = v;
            st.q.push_back(JsEvent { time: t, value: v, kind: JS_EVENT_AXIS | JS_EVENT_INIT, number: i as u8 });
        }
    }

    /// Evdev events to joystick events. On overflow the caller starts the
    /// reader over with the whole state ([`JoyClient::startup`]), as Linux
    /// does (not here: the device may be locked).
    fn feed(&self, events: &[(u16, u16, i32)]) -> Fed {
        let mut any = false;
        {
            let mut st = self.st.lock();
            let t = Self::now_ms();
            for &(ty, code, value) in events {
                let ev = match ty {
                    EV_KEY if code >= BTN_MISC && value != 2 => {
                        let Some(n) = st.buttons.iter().position(|&b| b == code) else { continue };
                        JsEvent { time: t, value: (value != 0) as i16, kind: JS_EVENT_BUTTON, number: n as u8 }
                    }
                    EV_ABS => {
                        let Some(n) = st.axes.iter().position(|&a| a == code) else { continue };
                        let v = correct(value, &st.corr[n]);
                        if st.values[n] == v {
                            continue;
                        }
                        st.values[n] = v;
                        JsEvent { time: t, value: v, kind: JS_EVENT_AXIS, number: n as u8 }
                    }
                    _ => continue,
                };
                if st.q.len() == JS_QUEUE {
                    return Fed::Overflow;
                }
                st.q.push_back(ev);
                any = true;
            }
        }
        if any {
            Fed::Some
        } else {
            Fed::Nothing
        }
    }

    pub fn ready(&self) -> bool {
        !self.st.lock().q.is_empty()
    }

    pub fn take(&self, max: usize) -> Vec<JsEvent> {
        let mut st = self.st.lock();
        let n = max.min(st.q.len());
        st.q.drain(..n).collect()
    }

    /// `JSIOCSCORR`: new corrections (the values are re-reported through them).
    pub fn set_corr(&self, corr: Vec<[i32; 4]>) {
        let mut st = self.st.lock();
        for (i, c) in corr.into_iter().enumerate().take(st.corr.len()) {
            st.corr[i] = c;
        }
    }

    /// `JSIOCSAXMAP` / `JSIOCSBTNMAP`: renumbers axes or buttons.
    pub fn set_map(&self, axes: Option<Vec<u16>>, buttons: Option<Vec<u16>>) {
        let mut st = self.st.lock();
        if let Some(a) = axes {
            let n = st.axes.len();
            st.axes = a.into_iter().take(n).collect();
        }
        if let Some(b) = buttons {
            let n = st.buttons.len();
            st.buttons = b.into_iter().take(n).collect();
        }
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    codes::self_test();
    let mut q = Queue::new();
    for i in 0..QUEUE + 3 {
        q.push(Event { ns: i as u64, ty: EV_KEY, code: 30, value: (i % 2) as i32 });
    }
    assert!(!q.ready(), "no report is complete yet");
    q.push(Event { ns: 0, ty: EV_SYN, code: SYN_REPORT, value: 0 });
    let all = q.take(usize::MAX);
    assert_eq!(all[0].code, SYN_DROPPED, "overflow is reported first");
    assert_eq!(all.last().unwrap().code, SYN_REPORT);
    assert!(all.len() < 8, "what was not read is gone");
    let a = AbsInfo::range(-32768, 32767, 16, 128);
    let c = default_corr(&a);
    assert_eq!(correct(0, &c), 0);
    assert_eq!(correct(32767, &c), 32767);
    assert!(correct(-32768, &c) <= -32766);
    assert_eq!(lowest_free([0, 1, 3].into_iter()), 2);
}
