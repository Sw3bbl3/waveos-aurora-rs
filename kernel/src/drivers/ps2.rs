//! i8042 PS/2 controller: keyboard (IRQ1) and mouse (IRQ12).
//!
//! Under QEMU/VMware the VMware "vmmouse" backdoor is used when present: it
//! reports an absolute pointer, so the guest cursor tracks the host cursor
//! without grabbing. It still signals through IRQ12.

use super::input::{self, InputEvent};
use super::keyboard::Decoder;
use super::vmmouse;
use crate::sync::IrqMutex;
use core::sync::atomic::{AtomicBool, Ordering};
use x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const CMD: u16 = 0x64;

static KEYBOARD: IrqMutex<Decoder> = IrqMutex::new(Decoder::new());
static MOUSE: IrqMutex<MouseState> = IrqMutex::new(MouseState::new());
static VMMOUSE: AtomicBool = AtomicBool::new(false);

struct MouseState {
    packet: [u8; 4],
    idx: usize,
    size: usize,
    x: i32,
    y: i32,
    buttons: u8,
}

impl MouseState {
    const fn new() -> Self {
        Self { packet: [0; 4], idx: 0, size: 3, x: 0, y: 0, buttons: 0 }
    }
}

fn status() -> u8 {
    unsafe { Port::<u8>::new(STATUS).read() }
}

fn wait_write() -> bool {
    (0..100_000).any(|_| status() & 0x02 == 0)
}

fn wait_read() -> bool {
    (0..100_000).any(|_| status() & 0x01 != 0)
}

fn command(c: u8) {
    wait_write();
    unsafe { Port::<u8>::new(CMD).write(c) };
}

fn write_data(b: u8) {
    wait_write();
    unsafe { Port::<u8>::new(DATA).write(b) };
}

fn read_data() -> Option<u8> {
    wait_read().then(|| unsafe { Port::<u8>::new(DATA).read() })
}

fn flush() {
    for _ in 0..64 {
        if status() & 0x01 == 0 {
            break;
        }
        unsafe { Port::<u8>::new(DATA).read() };
    }
}

/// Sends a byte to the mouse (port 2) and waits for its ACK.
fn mouse_cmd(b: u8) -> bool {
    command(0xD4);
    write_data(b);
    read_data() == Some(0xFA)
}

pub fn init() {
    command(0xAD); // disable keyboard port
    command(0xA7); // disable mouse port
    flush();

    command(0x20);
    let mut config = read_data().unwrap_or(0);
    config |= 0x01 | 0x02 | 0x40; // IRQ1, IRQ12, set-1 translation
    config &= !(0x10 | 0x20); // enable both clocks
    command(0x60);
    write_data(config);

    command(0xAE);
    command(0xA8);

    // Keyboard: enable scanning.
    write_data(0xF4);
    let _ = read_data();

    // Mouse: defaults, try IntelliMouse (scroll wheel), enable streaming.
    let mut size = 3;
    if mouse_cmd(0xF6) {
        for rate in [200u8, 100, 80] {
            mouse_cmd(0xF3);
            mouse_cmd(rate);
        }
        if mouse_cmd(0xF2) && read_data() == Some(3) {
            size = 4;
        }
        mouse_cmd(0xF4);
    }
    flush();
    {
        let mut m = MOUSE.lock();
        m.size = size;
        m.x = input::SCREEN_W.load(Ordering::Relaxed) / 2;
        m.y = input::SCREEN_H.load(Ordering::Relaxed) / 2;
    }

    let vm = vmmouse::init();
    VMMOUSE.store(vm, Ordering::Relaxed);
    log!(
        "ps2",
        "keyboard ready; mouse {}{}",
        if size == 4 { "with scroll wheel" } else { "3-button" },
        if vm { ", VMware absolute pointer enabled" } else { "" }
    );
}

pub fn keyboard_irq() {
    let byte = unsafe { Port::<u8>::new(DATA).read() };
    if let Some(ev) = KEYBOARD.lock().feed(byte) {
        input::push(InputEvent::Key(ev));
    }
}

pub fn mouse_irq() {
    let byte = unsafe { Port::<u8>::new(DATA).read() };
    if VMMOUSE.load(Ordering::Relaxed) {
        while let Some(p) = vmmouse::poll() {
            let w = input::SCREEN_W.load(Ordering::Relaxed);
            let h = input::SCREEN_H.load(Ordering::Relaxed);
            let x = (p.x as i64 * w as i64 / 0xFFFF) as i32;
            let y = (p.y as i64 * h as i64 / 0xFFFF) as i32;
            input::push(InputEvent::Pointer { x, y, buttons: p.buttons, wheel: p.wheel });
        }
        return;
    }

    let mut m = MOUSE.lock();
    if m.idx == 0 && byte & 0x08 == 0 {
        return; // out of sync; wait for a byte with the always-one bit
    }
    let i = m.idx;
    m.packet[i] = byte;
    m.idx += 1;
    if m.idx < m.size {
        return;
    }
    m.idx = 0;
    let [b0, b1, b2, b3] = m.packet;
    if b0 & 0xC0 != 0 {
        return; // overflow
    }
    let dx = b1 as i32 - (((b0 as i32) << 4) & 0x100);
    let dy = b2 as i32 - (((b0 as i32) << 3) & 0x100);
    let w = input::SCREEN_W.load(Ordering::Relaxed);
    let h = input::SCREEN_H.load(Ordering::Relaxed);
    m.x = (m.x + dx).clamp(0, w - 1);
    m.y = (m.y - dy).clamp(0, h - 1);
    m.buttons = b0 & 0x07;
    let wheel = if m.size == 4 { ((b3 << 4) as i8) >> 4 } else { 0 };
    input::push(InputEvent::Pointer { x: m.x, y: m.y, buttons: m.buttons, wheel });
}

/// Sets the keyboard's auto-repeat: `delay` 0–3 (250–1000 ms), `rate` 0 (30/s) – 31 (2/s).
pub fn set_typematic(delay: u8, rate: u8) {
    x86_64::instructions::interrupts::without_interrupts(|| {
        write_data(0xF3);
        let _ = read_data(); // ACK
        write_data((delay & 3) << 5 | (rate & 31));
        let _ = read_data();
    });
}
