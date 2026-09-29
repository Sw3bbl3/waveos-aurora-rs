//! Keyboard types shared by the kernel's input drivers and user programs.

#[repr(u32)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyCode {
    Char = 0,
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
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    Shift,
    Ctrl,
    Alt,
    Super,
    CapsLock,
    Menu,
    PrintScreen,
    Mute,
    VolumeDown,
    VolumeUp,
    PlayPause,
    NextTrack,
    PrevTrack,
    Unknown,
}

impl KeyCode {
    /// Function key `n` (1-based).
    pub fn function(n: u8) -> KeyCode {
        const F: [KeyCode; 12] = [
            KeyCode::F1,
            KeyCode::F2,
            KeyCode::F3,
            KeyCode::F4,
            KeyCode::F5,
            KeyCode::F6,
            KeyCode::F7,
            KeyCode::F8,
            KeyCode::F9,
            KeyCode::F10,
            KeyCode::F11,
            KeyCode::F12,
        ];
        F.get((n as usize).wrapping_sub(1)).copied().unwrap_or(KeyCode::Unknown)
    }

    /// Decodes a value received over the ABI (unknown values map to `Unknown`).
    pub fn from_u32(v: u32) -> KeyCode {
        if v <= KeyCode::Unknown as u32 {
            // SAFETY: `KeyCode` is a fieldless `repr(u32)` enum with contiguous
            // discriminants 0..=Unknown, and `v` is in that range.
            unsafe { core::mem::transmute::<u32, KeyCode>(v) }
        } else {
            KeyCode::Unknown
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub super_key: bool,
    pub caps: bool,
}

impl Modifiers {
    pub fn bits(&self) -> u32 {
        self.shift as u32
            | (self.ctrl as u32) << 1
            | (self.alt as u32) << 2
            | (self.super_key as u32) << 3
            | (self.caps as u32) << 4
    }
    pub fn from_bits(b: u32) -> Self {
        Self { shift: b & 1 != 0, ctrl: b & 2 != 0, alt: b & 4 != 0, super_key: b & 8 != 0, caps: b & 16 != 0 }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct KeyEvent {
    pub code: KeyCode,
    /// The character this key produces with the current modifiers, if any.
    pub ch: Option<char>,
    pub pressed: bool,
    pub mods: Modifiers,
}

impl KeyEvent {
    /// Packs into an [`crate::Event`] of kind `KEY`.
    pub fn to_event(&self, window: u32) -> crate::Event {
        crate::Event {
            kind: crate::event::KEY,
            window,
            a: self.code as u32,
            b: self.ch.map(|c| c as u32).unwrap_or(0),
            c: self.pressed as u32,
            d: self.mods.bits(),
            ..Default::default()
        }
    }

    pub fn from_event(e: &crate::Event) -> KeyEvent {
        KeyEvent {
            code: KeyCode::from_u32(e.a),
            ch: if e.b == 0 { None } else { char::from_u32(e.b) },
            pressed: e.c != 0,
            mods: Modifiers::from_bits(e.d),
        }
    }
}
