//! Game controllers: the ones USB drivers found, in the standard layout,
//! for apps to read (`gamepads`, `gamepad_state`) and shake
//! (`gamepad_rumble`), and for the desktop (Wave Play) to navigate with.
//!
//! - **Hot-plug.** Connections and disconnections queue [`Change`]s and wake
//!   the compositor, which shows them and may enter or leave Wave Play.
//! - **Waking the compositor.** Reports arrive hundreds of times a second
//!   with sticks that never sit still; only a *meaningful* change (a button,
//!   or a stick crossing into or out of a direction) wakes it. Repeats are
//!   the compositor's own timers.
//! - **Who reads the pads.** While the shell has the controllers (Wave
//!   Play's home or its overlay) or another window is focused, programs
//!   read a neutral pad, so a game doesn't fire behind a menu. The
//!   compositor names the owner ([`set_owner`]).
//! - **Pad-aware programs.** [`polled_recently`] says whether a process has
//!   been reading pads itself; for those that don't, the desktop turns the
//!   d-pad, A and B into arrow keys, Enter and Escape.
//! - **Rumble** goes to a small task of its own ([`rumble_async`]): USB
//!   transfers wait, and the compositor must not.

use crate::sync::{IrqMutex, WaitQueue};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::{GamepadInfo, GamepadState};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

/// A controller's rumble motors (the driver sends the report).
pub trait Rumble: Send + Sync {
    /// Strong (low-frequency) and weak (high-frequency) motor, 0–255.
    fn rumble(&self, strong: u8, weak: u8);
}

struct Pad {
    info: GamepadInfo,
    state: GamepadState,
    motors: Option<Arc<dyn Rumble>>,
    /// When the current rumble stops.
    rumble_until: Option<u64>,
    /// The state as last reported to the compositor, reduced to what
    /// navigation needs (buttons and stick directions).
    digital: u32,
    /// Buttons and directions pressed since the desktop last looked, so a
    /// tap shorter than a frame is still seen.
    latched: u32,
}

/// A controller came or went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Connected(u32),
    Disconnected(u32),
}

static PADS: IrqMutex<Vec<Pad>> = IrqMutex::new(Vec::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
static CHANGES: IrqMutex<Vec<Change>> = IrqMutex::new(Vec::new());
/// A button or stick direction changed since the compositor last looked.
static MOVED: AtomicBool = AtomicBool::new(false);
static CONSUMER: AtomicU64 = AtomicU64::new(u64::MAX);
/// Which process may read the pads: [`ANYONE`], [`SHELL`] (nobody), or a pid.
static OWNER: AtomicU64 = AtomicU64::new(ANYONE);
pub const ANYONE: u64 = u64::MAX;
pub const SHELL: u64 = 0;
/// (pid, uptime ms) of the programs that read pads lately.
static READERS: IrqMutex<[(u32, u64); 8]> = IrqMutex::new([(0, 0); 8]);

/// Past this, a stick counts as pushed in a direction (half way), and below
/// [`STICK_OFF`] as back in the middle: the gap keeps a resting thumb from
/// flickering between the two.
pub const STICK_ON: i32 = 16_000;
pub const STICK_OFF: i32 = 11_000;

/// Registers the task to wake when a controller changes meaningfully.
pub fn set_consumer(task: crate::sched::TaskId) {
    CONSUMER.store(task, Ordering::Relaxed);
}

fn wake() {
    let id = CONSUMER.load(Ordering::Relaxed);
    if id != u64::MAX {
        crate::sched::wake(id);
    }
}

/// Whether there is something for the compositor: a change of buttons or
/// directions, or a controller connected or disconnected.
pub fn pending() -> bool {
    MOVED.load(Ordering::Relaxed) || !CHANGES.lock().is_empty()
}

/// Takes the "something moved" flag.
pub fn take_moved() -> bool {
    MOVED.swap(false, Ordering::AcqRel)
}

pub fn take_changes() -> Vec<Change> {
    core::mem::take(&mut *CHANGES.lock())
}

/// Registers a controller; returns its id.
pub fn add(name: &str, vendor: u16, product: u16, family: u32, motors: Option<Arc<dyn Rumble>>) -> u32 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut info = GamepadInfo { id, vendor, product, family, rumble: motors.is_some() as u32, ..Default::default() };
    let n = name.len().min(info.name.len());
    info.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    info.name_len = n as u32;
    PADS.lock().push(Pad { info, state: GamepadState::default(), motors, rumble_until: None, digital: 0, latched: 0 });
    CHANGES.lock().push(Change::Connected(id));
    log!("gamepad", "{} connected (controller {})", name, id);
    wake();
    id
}

pub fn remove(id: u32) {
    let gone = {
        let mut pads = PADS.lock();
        let before = pads.len();
        pads.retain(|p| p.info.id != id);
        before != pads.len()
    };
    if gone {
        CHANGES.lock().push(Change::Disconnected(id));
        log!("gamepad", "controller {} disconnected", id);
        wake();
    }
}

/// Buttons plus four bits for the left stick's direction (bits 24–27: up,
/// down, left, right), with hysteresis against `previous`.
fn digital(s: &GamepadState, previous: u32) -> u32 {
    let mut d = s.buttons & 0x00FF_FFFF;
    let (x, y) = (s.axes[0] as i32, s.axes[1] as i32);
    for (bit, value, sign) in [(24, y, -1), (25, y, 1), (26, x, -1), (27, x, 1)] {
        let v = value * sign;
        let was = previous & 1 << bit != 0;
        if v > STICK_ON || (was && v > STICK_OFF) {
            d |= 1 << bit;
        }
    }
    d
}

/// A new report: the controller's whole state now.
pub fn update(id: u32, s: &aurora_hid::gamepad::State) {
    let mut moved = false;
    if let Some(p) = PADS.lock().iter_mut().find(|p| p.info.id == id) {
        // aurora_hid's button numbers are the ABI's bit positions.
        p.state.buttons = s.buttons;
        p.state.axes = s.axes;
        p.state.triggers = s.triggers;
        p.state.sequence = p.state.sequence.wrapping_add(1);
        let d = digital(&p.state, p.digital);
        if d != p.digital {
            p.latched |= d & !p.digital;
            p.digital = d;
            moved = true;
        }
    }
    if moved {
        MOVED.store(true, Ordering::Release);
        wake();
    }
}

pub fn list() -> Vec<GamepadInfo> {
    PADS.lock().iter().map(|p| p.info).collect()
}

pub fn state(id: u32) -> Option<GamepadState> {
    PADS.lock().iter().find(|p| p.info.id == id).map(|p| p.state)
}

/// Every controller's state for the desktop, with what was pressed since
/// it last asked (in the digital layout: buttons, then the left stick's
/// directions in bits 24–27).
pub fn poll() -> Vec<(GamepadInfo, GamepadState, u32)> {
    PADS.lock().iter_mut().map(|p| (p.info, p.state, core::mem::take(&mut p.latched))).collect()
}

/// Says who may read the pads (see [`ANYONE`], [`SHELL`]).
pub fn set_owner(owner: u64) {
    OWNER.store(owner, Ordering::Relaxed);
}

/// `gamepad_state` for process `pid`: a neutral pad (same sequence, nothing
/// pressed) while it isn't the one the user is playing with.
pub fn state_for(pid: u32, id: u32) -> Option<GamepadState> {
    let st = state(id)?;
    let now = crate::time::uptime_ms();
    {
        let mut readers = READERS.lock();
        let slot = readers.iter().position(|r| r.0 == pid).unwrap_or_else(|| {
            // The reader seen longest ago makes room.
            (0..readers.len()).min_by_key(|&i| readers[i].1).unwrap_or(0)
        });
        readers[slot] = (pid, now);
    }
    let owner = OWNER.load(Ordering::Relaxed);
    if owner == ANYONE || owner == pid as u64 {
        Some(st)
    } else {
        Some(GamepadState { sequence: st.sequence, ..Default::default() })
    }
}

/// Whether process `pid` read a controller in the last two seconds.
pub fn polled_recently(pid: u32, now: u64) -> bool {
    READERS.lock().iter().any(|&(p, at)| p == pid && pid != 0 && now.saturating_sub(at) < 2000)
}

/// Shakes a controller for `ms` (0: until told otherwise, at most 10 s).
pub fn rumble(id: u32, strong: u8, weak: u8, ms: u64) -> Result<(), isize> {
    use aurora_abi::err::*;
    let motors = {
        let mut pads = PADS.lock();
        let p = pads.iter_mut().find(|p| p.info.id == id).ok_or(ENODEV)?;
        let m = p.motors.clone().ok_or(ENOSYS)?;
        let on = strong != 0 || weak != 0;
        p.rumble_until = on.then(|| crate::time::uptime_ms() + if ms == 0 { 10_000 } else { ms.min(10_000) });
        m
    };
    motors.rumble(strong, weak);
    crate::drivers::usb::kick_ticks();
    Ok(())
}

/// Rumble requests waiting for the haptics task: (controller, strong, weak, ms).
static HAPTICS: IrqMutex<Vec<(u32, u8, u8, u64)>> = IrqMutex::new(Vec::new());
static HAPTICS_WAKE: WaitQueue = WaitQueue::new();
static HAPTICS_TASK: AtomicBool = AtomicBool::new(false);

/// Like [`rumble`], without waiting for the controller: for the compositor.
/// Controllers without motors are skipped.
pub fn rumble_async(id: u32, strong: u8, weak: u8, ms: u64) {
    if !PADS.lock().iter().any(|p| p.info.id == id && p.motors.is_some()) {
        return;
    }
    {
        let mut q = HAPTICS.lock();
        // A newer request for the same controller replaces one not yet sent.
        q.retain(|r| r.0 != id);
        if q.len() < 8 {
            q.push((id, strong, weak, ms));
        }
    }
    if !HAPTICS_TASK.swap(true, Ordering::AcqRel) {
        crate::sched::spawn("pad-haptics", || loop {
            HAPTICS_WAKE.wait(60_000, || !HAPTICS.lock().is_empty());
            let batch = core::mem::take(&mut *HAPTICS.lock());
            for (id, strong, weak, ms) in batch {
                let _ = rumble(id, strong, weak, ms);
            }
        });
    }
    HAPTICS_WAKE.wake_all();
}

/// Stops rumbles whose time is up; returns ms until the next one is due.
pub fn tick(now: u64) -> Option<u64> {
    let mut due: Vec<Arc<dyn Rumble>> = Vec::new();
    let mut next: Option<u64> = None;
    for p in PADS.lock().iter_mut() {
        match p.rumble_until {
            Some(t) if t <= now => {
                p.rumble_until = None;
                due.extend(p.motors.clone());
            }
            Some(t) => next = Some(next.map_or(t - now, |n| n.min(t - now))),
            None => {}
        }
    }
    for m in due {
        m.rumble(0, 0);
    }
    next
}

/// A display name from the USB strings, without the vendor's noise.
pub fn name(product: &str) -> String {
    let n = product.trim();
    if n.is_empty() {
        String::from("Game Controller")
    } else {
        String::from(n)
    }
}
