//! Input event queue shared by the input drivers (PS/2, vmmouse, USB HID —
//! the producers) and the compositor task (the consumer). Fixed-size,
//! allocation-free, IRQ-safe.
//!
//! Also what all keyboards share: Caps Lock is one state, whichever
//! keyboard toggled it, and every keyboard's light follows it.

use crate::sync::{IrqMutex, WaitQueue};
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

pub use aurora_abi::input::{KeyCode, KeyEvent, Modifiers};

pub const BUTTON_LEFT: u8 = 1;
pub const BUTTON_RIGHT: u8 = 2;
pub const BUTTON_MIDDLE: u8 = 4;

/// One wheel notch, in the units of [`InputEvent::Scroll`].
pub const NOTCH: i32 = 120;

#[derive(Clone, Copy, Debug)]
pub enum InputEvent {
    Key(KeyEvent),
    /// Absolute pointer state after a movement / button change.
    Pointer {
        x: i32,
        y: i32,
        buttons: u8,
    },
    /// Scrolling in 1/120 notch (positive: down, right). Wheels move whole
    /// notches; touchpads anything.
    Scroll {
        dx: i32,
        dy: i32,
    },
}

const CAPACITY: usize = 256;

struct Queue {
    buf: [Option<InputEvent>; CAPACITY],
    head: usize,
    len: usize,
}

static QUEUE: IrqMutex<Queue> = IrqMutex::new(Queue { buf: [None; CAPACITY], head: 0, len: 0 });
static CONSUMER: AtomicU64 = AtomicU64::new(u64::MAX);
pub static SCREEN_W: AtomicI32 = AtomicI32::new(1024);
pub static SCREEN_H: AtomicI32 = AtomicI32::new(768);

pub fn set_screen_size(w: i32, h: i32) {
    SCREEN_W.store(w, Ordering::Relaxed);
    SCREEN_H.store(h, Ordering::Relaxed);
}

pub fn screen_size() -> (i32, i32) {
    (SCREEN_W.load(Ordering::Relaxed), SCREEN_H.load(Ordering::Relaxed))
}

/// Registers the task to wake whenever an event arrives.
pub fn set_consumer(task: crate::sched::TaskId) {
    CONSUMER.store(task, Ordering::Relaxed);
}

/// Where the pointer is (relative devices move it from here).
static POINTER: AtomicU64 = AtomicU64::new(0);

pub fn pointer_position() -> (i32, i32) {
    let v = POINTER.load(Ordering::Relaxed);
    if v == 0 {
        return (SCREEN_W.load(Ordering::Relaxed) / 2, SCREEN_H.load(Ordering::Relaxed) / 2);
    }
    // Bit 63 only marks "set"; coordinates are never negative.
    (((v >> 32) & 0x7FFF_FFFF) as i32, v as u32 as i32)
}

/// Moves the pointer by `dx`, `dy` (clamped to the screen) with `buttons`
/// held: for relative devices (mice, touchpads).
pub fn pointer_moved(dx: i32, dy: i32, buttons: u8) {
    let (x, y) = pointer_position();
    let (w, h) = screen_size();
    push(InputEvent::Pointer { x: (x + dx).clamp(0, w - 1), y: (y + dy).clamp(0, h - 1), buttons });
}

pub fn push(ev: InputEvent) {
    if let InputEvent::Pointer { x, y, .. } = ev {
        POINTER.store((x as u32 as u64) << 32 | y as u32 as u64 | 1 << 63, Ordering::Relaxed);
    }
    {
        let mut q = QUEUE.lock();
        // Coalesce consecutive motion with identical buttons, and
        // consecutive scrolling, to keep the queue short.
        if q.len > 0 {
            let last = (q.head + q.len - 1) % CAPACITY;
            match (q.buf[last], ev) {
                (Some(InputEvent::Pointer { buttons: b0, .. }), InputEvent::Pointer { buttons: b1, .. })
                    if b0 == b1 =>
                {
                    q.buf[last] = Some(ev);
                    return wake();
                }
                (Some(InputEvent::Scroll { dx: x0, dy: y0 }), InputEvent::Scroll { dx, dy }) => {
                    q.buf[last] = Some(InputEvent::Scroll { dx: x0 + dx, dy: y0 + dy });
                    return wake();
                }
                _ => {}
            }
        }
        if q.len == CAPACITY {
            q.head = (q.head + 1) % CAPACITY;
            q.len -= 1;
        }
        let tail = (q.head + q.len) % CAPACITY;
        q.buf[tail] = Some(ev);
        q.len += 1;
    }
    wake();
}

fn wake() {
    let id = CONSUMER.load(Ordering::Relaxed);
    if id != u64::MAX {
        crate::sched::wake(id);
    }
}

/// Modifier keys as of the last key event handed to the consumer, so pointer
/// events carry the modifiers held when they are processed (Shift+click, …).
static MODIFIERS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

pub fn modifiers() -> Modifiers {
    Modifiers::from_bits(MODIFIERS.load(Ordering::Relaxed))
}

pub fn pop() -> Option<InputEvent> {
    let mut q = QUEUE.lock();
    if q.len == 0 {
        return None;
    }
    let head = q.head;
    let ev = q.buf[head].take();
    q.head = (q.head + 1) % CAPACITY;
    q.len -= 1;
    if let Some(InputEvent::Key(k)) = ev {
        MODIFIERS.store(k.mods.bits(), Ordering::Relaxed);
    }
    ev
}

pub fn pending() -> bool {
    QUEUE.lock().len > 0
}

// ------------------------------------------------------- pointer settings

/// Mouse speed 1–10 and touchpad settings, from the preferences (kept here
/// so interrupt handlers can read them).
static POINTER_SPEED: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(5);
static TRACKING_SPEED: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(5);
static TAP_TO_CLICK: AtomicBool = AtomicBool::new(true);
static NATURAL_SCROLLING: AtomicBool = AtomicBool::new(true);

pub fn set_pointer_settings(pointer_speed: u8, tracking_speed: u8, tap_to_click: bool, natural: bool) {
    POINTER_SPEED.store(pointer_speed.clamp(1, 10), Ordering::Relaxed);
    TRACKING_SPEED.store(tracking_speed.clamp(1, 10), Ordering::Relaxed);
    TAP_TO_CLICK.store(tap_to_click, Ordering::Relaxed);
    NATURAL_SCROLLING.store(natural, Ordering::Relaxed);
}

/// The touchpad settings as the gesture engine takes them.
pub fn touchpad_settings() -> aurora_hid::gestures::Settings {
    aurora_hid::gestures::Settings {
        tap_to_click: TAP_TO_CLICK.load(Ordering::Relaxed),
        natural_scrolling: NATURAL_SCROLLING.load(Ordering::Relaxed),
        speed: TRACKING_SPEED.load(Ordering::Relaxed),
    }
}

/// Mouse motion scaled by the speed setting (5: one pixel per count),
/// keeping the fractions in `rem` (1/64 px).
pub fn scale_motion(dx: i32, dy: i32, rem: &mut (i32, i32)) -> (i32, i32) {
    let f = (2 + POINTER_SPEED.load(Ordering::Relaxed) as i32) * 64 / 7;
    rem.0 += dx * f;
    rem.1 += dy * f;
    let out = (rem.0 / 64, rem.1 / 64);
    rem.0 %= 64;
    rem.1 %= 64;
    out
}

// ---------------------------------------------------------------- Caps Lock

static CAPS: AtomicBool = AtomicBool::new(false);

pub fn caps() -> bool {
    CAPS.load(Ordering::Relaxed)
}

/// Caps Lock was pressed on some keyboard: flips it for all of them and
/// updates every keyboard's light. Returns the new state.
pub fn toggle_caps() -> bool {
    let on = !CAPS.fetch_xor(true, Ordering::AcqRel);
    request_lights();
    on
}

/// Something that shows the lock state (a keyboard driver).
pub type LightSink = fn(caps: bool);

static SINKS: IrqMutex<[Option<LightSink>; 4]> = IrqMutex::new([None; 4]);
static LIGHTS_WAKE: WaitQueue = WaitQueue::new();
static LIGHTS_DUE: AtomicBool = AtomicBool::new(false);
static LIGHTS_TASK: AtomicBool = AtomicBool::new(false);

/// Registers a driver's function that sets its keyboards' lights (run on a
/// task of its own: talking to keyboards may wait).
pub fn add_light_sink(sink: LightSink) {
    let mut s = SINKS.lock();
    if !s.contains(&Some(sink)) {
        if let Some(slot) = s.iter_mut().find(|x| x.is_none()) {
            *slot = Some(sink);
        }
    }
}

/// Starts the task that sets keyboard lights (once tasks can run).
pub fn start() {
    if !LIGHTS_TASK.swap(true, Ordering::AcqRel) {
        crate::sched::spawn("input-lights", lights_task);
    }
}

/// Asks for the lights to be brought up to date (any context).
pub fn request_lights() {
    LIGHTS_DUE.store(true, Ordering::Release);
    LIGHTS_WAKE.wake_all();
}

fn lights_task() {
    loop {
        LIGHTS_WAKE.wait(10_000, || LIGHTS_DUE.load(Ordering::Acquire));
        if !LIGHTS_DUE.swap(false, Ordering::AcqRel) {
            continue;
        }
        let sinks = *SINKS.lock();
        for sink in sinks.into_iter().flatten() {
            sink(caps());
        }
    }
}
