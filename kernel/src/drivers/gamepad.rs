//! Game controllers: the ones USB drivers found, in the standard layout,
//! for apps to read (`gamepads`, `gamepad_state`) and shake
//! (`gamepad_rumble`).

use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::{GamepadInfo, GamepadState};
use core::sync::atomic::{AtomicU32, Ordering};

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
}

static PADS: IrqMutex<Vec<Pad>> = IrqMutex::new(Vec::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);

/// Registers a controller; returns its id.
pub fn add(name: &str, vendor: u16, product: u16, family: u32, motors: Option<Arc<dyn Rumble>>) -> u32 {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut info = GamepadInfo { id, vendor, product, family, rumble: motors.is_some() as u32, ..Default::default() };
    let n = name.len().min(info.name.len());
    info.name[..n].copy_from_slice(&name.as_bytes()[..n]);
    info.name_len = n as u32;
    PADS.lock().push(Pad { info, state: GamepadState::default(), motors, rumble_until: None });
    log!("gamepad", "{} connected (controller {})", name, id);
    id
}

pub fn remove(id: u32) {
    PADS.lock().retain(|p| p.info.id != id);
}

/// A new report: the controller's whole state now.
pub fn update(id: u32, s: &aurora_hid::gamepad::State) {
    if let Some(p) = PADS.lock().iter_mut().find(|p| p.info.id == id) {
        // aurora_hid's button numbers are the ABI's bit positions.
        p.state.buttons = s.buttons;
        p.state.axes = s.axes;
        p.state.triggers = s.triggers;
        p.state.sequence = p.state.sequence.wrapping_add(1);
    }
}

pub fn list() -> Vec<GamepadInfo> {
    PADS.lock().iter().map(|p| p.info).collect()
}

pub fn state(id: u32) -> Option<GamepadState> {
    PADS.lock().iter().find(|p| p.info.id == id).map(|p| p.state)
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
