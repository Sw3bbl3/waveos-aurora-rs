//! Input event queue shared by the PS/2 / vmmouse IRQ handlers (producers)
//! and the compositor task (consumer). Fixed-size, allocation-free, IRQ-safe.

use crate::sync::IrqMutex;
use core::sync::atomic::{AtomicI32, AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char,
    Escape,
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
    Shift,
    Ctrl,
    Alt,
    Super,
    CapsLock,
    Menu,
    Unknown,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub code: KeyCode,
    /// The character this key produces with the current modifiers, if any.
    pub ch: Option<char>,
    pub pressed: bool,
    pub mods: Modifiers,
}

pub const BUTTON_LEFT: u8 = 1;
pub const BUTTON_RIGHT: u8 = 2;
pub const BUTTON_MIDDLE: u8 = 4;

#[derive(Clone, Copy, Debug)]
pub enum InputEvent {
    Key(KeyEvent),
    /// Absolute pointer state after a movement / button change.
    Pointer {
        x: i32,
        y: i32,
        buttons: u8,
        wheel: i8,
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

/// Registers the task to wake whenever an event arrives.
pub fn set_consumer(task: crate::sched::TaskId) {
    CONSUMER.store(task, Ordering::Relaxed);
}

pub fn push(ev: InputEvent) {
    {
        let mut q = QUEUE.lock();
        // Coalesce consecutive pointer motion with identical buttons to keep the queue short.
        if q.len > 0 {
            let last = (q.head + q.len - 1) % CAPACITY;
            if let (
                Some(InputEvent::Pointer { buttons: b0, wheel: 0, .. }),
                InputEvent::Pointer { buttons: b1, wheel: 0, .. },
            ) = (q.buf[last], ev)
            {
                if b0 == b1 {
                    q.buf[last] = Some(ev);
                    return wake();
                }
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

pub fn pop() -> Option<InputEvent> {
    let mut q = QUEUE.lock();
    if q.len == 0 {
        return None;
    }
    let head = q.head;
    let ev = q.buf[head].take();
    q.head = (q.head + 1) % CAPACITY;
    q.len -= 1;
    ev
}

pub fn pending() -> bool {
    QUEUE.lock().len > 0
}
