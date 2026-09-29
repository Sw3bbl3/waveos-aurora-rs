//! PS/2 scancode set 1 decoder (the i8042 translates set 2 to set 1).
//!
//! Printable keys become USB HID usages, which the current keyboard layout
//! (`keymap`) turns into characters — so PS/2 and USB keyboards share layouts.

use super::input::{KeyCode, KeyEvent, Modifiers};
use super::keymap;

/// HID usage of a set-1 scancode on the main block (0 = none).
fn usage(sc: u8) -> u8 {
    match sc {
        0x02..=0x0B => 0x1E + (sc - 0x02), // 1–0
        0x0C => 0x2D,
        0x0D => 0x2E,
        0x10 => 0x14, // q
        0x11 => 0x1A,
        0x12 => 0x08,
        0x13 => 0x15,
        0x14 => 0x17,
        0x15 => 0x1C,
        0x16 => 0x18,
        0x17 => 0x0C,
        0x18 => 0x12,
        0x19 => 0x13, // p
        0x1A => 0x2F,
        0x1B => 0x30,
        0x1E => 0x04, // a
        0x1F => 0x16,
        0x20 => 0x07,
        0x21 => 0x09,
        0x22 => 0x0A,
        0x23 => 0x0B,
        0x24 => 0x0D,
        0x25 => 0x0E,
        0x26 => 0x0F, // l
        0x27 => 0x33,
        0x28 => 0x34,
        0x29 => 0x35,
        0x2B => 0x31,
        0x2C => 0x1D, // z
        0x2D => 0x1B,
        0x2E => 0x06,
        0x2F => 0x19,
        0x30 => 0x05,
        0x31 => 0x11,
        0x32 => 0x10, // m
        0x33 => 0x36,
        0x34 => 0x37,
        0x35 => 0x38,
        0x56 => 0x64, // ISO key
        _ => 0,
    }
}

pub struct Decoder {
    extended: bool,
    mods: Modifiers,
    lshift: bool,
    rshift: bool,
    lctrl: bool,
    rctrl: bool,
    /// Right Alt: the AltGr layer, not an Alt modifier.
    altgr: bool,
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            extended: false,
            mods: Modifiers { shift: false, ctrl: false, alt: false, super_key: false, caps: false },
            lshift: false,
            rshift: false,
            lctrl: false,
            rctrl: false,
            altgr: false,
        }
    }

    pub fn feed(&mut self, byte: u8) -> Option<KeyEvent> {
        if byte == 0xE0 {
            self.extended = true;
            return None;
        }
        if byte == 0xE1 || byte == 0xFA || byte == 0xFE {
            return None; // Pause prefix, ACK, resend
        }
        let ext = core::mem::replace(&mut self.extended, false);
        let pressed = byte & 0x80 == 0;
        let sc = byte & 0x7F;

        let code = if ext {
            match sc {
                0x1C => KeyCode::Enter,
                0x1D => KeyCode::Ctrl,
                0x38 => KeyCode::Alt,
                0x47 => KeyCode::Home,
                0x48 => KeyCode::Up,
                0x49 => KeyCode::PageUp,
                0x4B => KeyCode::Left,
                0x4D => KeyCode::Right,
                0x4F => KeyCode::End,
                0x50 => KeyCode::Down,
                0x51 => KeyCode::PageDown,
                0x52 => KeyCode::Insert,
                0x53 => KeyCode::Delete,
                0x5B | 0x5C => KeyCode::Super,
                0x5D => KeyCode::Menu,
                0x35 => KeyCode::Char, // keypad '/'
                0x37 => KeyCode::PrintScreen,
                0x2A | 0x36 => return None, // fake shifts around PrintScreen
                0x20 => KeyCode::Mute,
                0x2E => KeyCode::VolumeDown,
                0x30 => KeyCode::VolumeUp,
                0x22 => KeyCode::PlayPause,
                0x19 => KeyCode::NextTrack,
                0x10 => KeyCode::PrevTrack,
                _ => KeyCode::Unknown,
            }
        } else {
            match sc {
                0x01 => KeyCode::Escape,
                0x0E => KeyCode::Backspace,
                0x0F => KeyCode::Tab,
                0x1C => KeyCode::Enter,
                0x1D => KeyCode::Ctrl,
                0x2A | 0x36 => KeyCode::Shift,
                0x38 => KeyCode::Alt,
                0x3A => KeyCode::CapsLock,
                0x39 => KeyCode::Char, // space
                0x3B..=0x44 => KeyCode::function(sc - 0x3A),
                0x57 => KeyCode::F11,
                0x58 => KeyCode::F12,
                0x47 => KeyCode::Home,
                0x48 => KeyCode::Up,
                0x49 => KeyCode::PageUp,
                0x4B => KeyCode::Left,
                0x4D => KeyCode::Right,
                0x4F => KeyCode::End,
                0x50 => KeyCode::Down,
                0x51 => KeyCode::PageDown,
                0x52 => KeyCode::Insert,
                0x53 => KeyCode::Delete,
                0x37 | 0x4A | 0x4E => KeyCode::Char, // keypad * - +
                s if usage(s) != 0 => KeyCode::Char,
                _ => KeyCode::Unknown,
            }
        };

        match code {
            KeyCode::Shift => {
                if sc == 0x2A {
                    self.lshift = pressed
                } else {
                    self.rshift = pressed
                }
                self.mods.shift = self.lshift || self.rshift;
            }
            KeyCode::Ctrl => {
                if ext {
                    self.rctrl = pressed
                } else {
                    self.lctrl = pressed
                }
                self.mods.ctrl = self.lctrl || self.rctrl;
            }
            KeyCode::Alt if ext => self.altgr = pressed,
            KeyCode::Alt => self.mods.alt = pressed,
            KeyCode::Super => self.mods.super_key = pressed,
            KeyCode::CapsLock if pressed => self.mods.caps = !self.mods.caps,
            _ => {}
        }

        let ch = match code {
            KeyCode::Char if !pressed => None,
            KeyCode::Char if ext && sc == 0x35 => Some('/'),
            KeyCode::Char if sc == 0x37 => Some('*'),
            KeyCode::Char if sc == 0x4A => Some('-'),
            KeyCode::Char if sc == 0x4E => Some('+'),
            KeyCode::Char if sc == 0x39 => Some(keymap::space()),
            KeyCode::Char => {
                // Ctrl+Alt works as AltGr (as on Windows).
                let altgr = self.altgr || (self.mods.ctrl && self.mods.alt);
                match keymap::translate(usage(sc), self.mods.shift, self.mods.caps, altgr) {
                    Some(c) => Some(c),
                    // A dead key (accent waiting for a letter) produces no event.
                    None => return None,
                }
            }
            KeyCode::Enter => Some('\n'),
            KeyCode::Tab => Some('\t'),
            _ => None,
        };
        Some(KeyEvent { code, ch, pressed, mods: self.mods })
    }
}
