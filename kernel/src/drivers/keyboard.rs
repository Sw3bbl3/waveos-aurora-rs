//! PS/2 scancode set 1 decoder (US layout). The i8042 translates set 2 to set 1.

use super::input::{KeyCode, KeyEvent, Modifiers};

const NORMAL: &[u8; 0x3A] = b"\0\x1b1234567890-=\x08\tqwertyuiop[]\n\0asdfghjkl;'`\0\\zxcvbnm,./\0*\0 ";
const SHIFTED: &[u8; 0x3A] = b"\0\x1b!@#$%^&*()_+\x08\tQWERTYUIOP{}\n\0ASDFGHJKL:\"~\0|ZXCVBNM<>?\0*\0 ";

pub struct Decoder {
    extended: bool,
    mods: Modifiers,
    lshift: bool,
    rshift: bool,
}

impl Decoder {
    pub const fn new() -> Self {
        Self {
            extended: false,
            mods: Modifiers { shift: false, ctrl: false, alt: false, super_key: false, caps: false },
            lshift: false,
            rshift: false,
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
                0x35 => KeyCode::Char,      // keypad '/'
                0x2A | 0x37 => return None, // fake shifts around PrintScreen
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
                0x3B..=0x44 => KeyCode::F(sc - 0x3A),
                0x57 => KeyCode::F(11),
                0x58 => KeyCode::F(12),
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
                s if (s as usize) < NORMAL.len() && NORMAL[s as usize] != 0 => KeyCode::Char,
                0x4A | 0x4E => KeyCode::Char, // keypad - +
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
            KeyCode::Ctrl => self.mods.ctrl = pressed,
            KeyCode::Alt => self.mods.alt = pressed,
            KeyCode::Super => self.mods.super_key = pressed,
            KeyCode::CapsLock if pressed => self.mods.caps = !self.mods.caps,
            _ => {}
        }

        let ch = match code {
            KeyCode::Char if ext && sc == 0x35 => Some('/'),
            KeyCode::Char if sc == 0x4A => Some('-'),
            KeyCode::Char if sc == 0x4E => Some('+'),
            KeyCode::Char => {
                let base = NORMAL[sc as usize];
                let letter = base.is_ascii_lowercase();
                let shift = self.mods.shift ^ (letter && self.mods.caps);
                let b = if shift { SHIFTED[sc as usize] } else { base };
                Some(b as char)
            }
            KeyCode::Enter => Some('\n'),
            KeyCode::Tab => Some('\t'),
            _ => None,
        };
        Some(KeyEvent { code, ch, pressed, mods: self.mods })
    }
}
