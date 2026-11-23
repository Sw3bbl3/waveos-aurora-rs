//! Linux's input event codes (`linux/input-event-codes.h`), the numbers
//! every program reading `/dev/input` expects, and the tables that turn
//! WaveOS's key numbers (USB HID usages, PS/2 scancodes) into them.

// Event types.
pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_REL: u16 = 0x02;
pub const EV_ABS: u16 = 0x03;
pub const EV_MSC: u16 = 0x04;
pub const EV_SW: u16 = 0x05;
pub const EV_LED: u16 = 0x11;
pub const EV_SND: u16 = 0x12;
pub const EV_REP: u16 = 0x14;
pub const EV_FF: u16 = 0x15;
pub const EV_MAX: u16 = 0x1f;

pub const SYN_REPORT: u16 = 0;
pub const SYN_DROPPED: u16 = 3;

// Bitmap sizes (codes per type).
pub const KEY_CNT: usize = 0x300;
pub const REL_CNT: usize = 0x10;
pub const ABS_CNT: usize = 0x40;
pub const MSC_CNT: usize = 0x08;
pub const SW_CNT: usize = 0x11;
pub const LED_CNT: usize = 0x10;
pub const SND_CNT: usize = 0x08;
pub const FF_CNT: usize = 0x80;
pub const PROP_CNT: usize = 0x20;

pub const REL_X: u16 = 0x00;
pub const REL_Y: u16 = 0x01;
pub const REL_HWHEEL: u16 = 0x06;
pub const REL_WHEEL: u16 = 0x08;
pub const REL_WHEEL_HI_RES: u16 = 0x0b;
pub const REL_HWHEEL_HI_RES: u16 = 0x0c;

pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;
pub const ABS_MT_SLOT: u16 = 0x2f;
pub const ABS_MT_FIRST: u16 = 0x30;
pub const ABS_MT_POSITION_X: u16 = 0x35;
pub const ABS_MT_POSITION_Y: u16 = 0x36;
pub const ABS_MT_TRACKING_ID: u16 = 0x39;

pub const MSC_SCAN: u16 = 0x04;

pub const LED_NUML: u16 = 0x00;
pub const LED_CAPSL: u16 = 0x01;
pub const LED_SCROLLL: u16 = 0x02;

pub const REP_DELAY: usize = 0;
pub const REP_PERIOD: usize = 1;

pub const FF_RUMBLE: u16 = 0x50;
pub const FF_PERIODIC: u16 = 0x51;
pub const FF_SQUARE: u16 = 0x58;
pub const FF_TRIANGLE: u16 = 0x59;
pub const FF_SINE: u16 = 0x5a;
pub const FF_GAIN: u16 = 0x60;

pub const INPUT_PROP_POINTER: u16 = 0x00;
pub const INPUT_PROP_BUTTONPAD: u16 = 0x02;

// Buttons.
pub const BTN_MISC: u16 = 0x100;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
pub const BTN_SIDE: u16 = 0x113;
pub const BTN_EXTRA: u16 = 0x114;
pub const BTN_JOYSTICK: u16 = 0x120;
pub const BTN_SOUTH: u16 = 0x130;
pub const BTN_EAST: u16 = 0x131;
/// Also `BTN_X`: what xpad calls the Xbox X button (which sits west).
pub const BTN_NORTH: u16 = 0x133;
/// Also `BTN_Y`.
pub const BTN_WEST: u16 = 0x134;
pub const BTN_A: u16 = 0x130;
pub const BTN_B: u16 = 0x131;
pub const BTN_X: u16 = 0x133;
pub const BTN_Y: u16 = 0x134;
pub const BTN_Z: u16 = 0x135;
pub const BTN_TL: u16 = 0x136;
pub const BTN_TR: u16 = 0x137;
pub const BTN_TL2: u16 = 0x138;
pub const BTN_TR2: u16 = 0x139;
pub const BTN_SELECT: u16 = 0x13a;
pub const BTN_START: u16 = 0x13b;
pub const BTN_MODE: u16 = 0x13c;
pub const BTN_THUMBL: u16 = 0x13d;
pub const BTN_THUMBR: u16 = 0x13e;
pub const BTN_DIGI: u16 = 0x140;
pub const BTN_TOOL_FINGER: u16 = 0x145;
pub const BTN_TOOL_QUINTTAP: u16 = 0x148;
pub const BTN_TOUCH: u16 = 0x14a;
pub const BTN_TOOL_DOUBLETAP: u16 = 0x14d;
pub const BTN_TOOL_TRIPLETAP: u16 = 0x14e;
pub const BTN_TOOL_QUADTAP: u16 = 0x14f;

// Keys used by name.
pub const KEY_ESC: u16 = 1;
pub const KEY_CAPSLOCK: u16 = 58;
pub const KEY_BACK: u16 = 158;
pub const KEY_FORWARD: u16 = 159;
pub const KEY_MAX: u16 = 0x2ff;

// Bus types (`struct input_id`).
pub const BUS_USB: u16 = 0x03;
pub const BUS_I8042: u16 = 0x11;
pub const BUS_I2C: u16 = 0x18;

/// USB HID keyboard page (0x07) usage to Linux key (what `hid-input` maps).
const HID_KEYBOARD: [u16; 0xE8] = {
    let mut t = [0u16; 0xE8];
    // Letters a–z.
    let letters =
        [30, 48, 46, 32, 18, 33, 34, 35, 23, 36, 37, 38, 50, 49, 24, 25, 16, 19, 31, 20, 22, 47, 17, 45, 21, 44];
    let mut i = 0;
    while i < 26 {
        t[0x04 + i] = letters[i];
        i += 1;
    }
    // 1–9, 0.
    i = 0;
    while i < 10 {
        t[0x1E + i] = 2 + i as u16;
        i += 1;
    }
    let rest: &[(usize, u16)] = &[
        (0x28, 28),  // Enter
        (0x29, 1),   // Escape
        (0x2A, 14),  // Backspace
        (0x2B, 15),  // Tab
        (0x2C, 57),  // Space
        (0x2D, 12),  // -
        (0x2E, 13),  // =
        (0x2F, 26),  // [
        (0x30, 27),  // ]
        (0x31, 43),  // backslash
        (0x32, 43),  // non-US #
        (0x33, 39),  // ;
        (0x34, 40),  // '
        (0x35, 41),  // `
        (0x36, 51),  // ,
        (0x37, 52),  // .
        (0x38, 53),  // /
        (0x39, 58),  // Caps Lock
        (0x3A, 59),  // F1
        (0x3B, 60),  //
        (0x3C, 61),  //
        (0x3D, 62),  //
        (0x3E, 63),  //
        (0x3F, 64),  //
        (0x40, 65),  //
        (0x41, 66),  //
        (0x42, 67),  //
        (0x43, 68),  // F10
        (0x44, 87),  // F11
        (0x45, 88),  // F12
        (0x46, 99),  // Print Screen (SysRq)
        (0x47, 70),  // Scroll Lock
        (0x48, 119), // Pause
        (0x49, 110), // Insert
        (0x4A, 102), // Home
        (0x4B, 104), // Page Up
        (0x4C, 111), // Delete
        (0x4D, 107), // End
        (0x4E, 109), // Page Down
        (0x4F, 106), // Right
        (0x50, 105), // Left
        (0x51, 108), // Down
        (0x52, 103), // Up
        (0x53, 69),  // Num Lock
        (0x54, 98),  // KP /
        (0x55, 55),  // KP *
        (0x56, 74),  // KP -
        (0x57, 78),  // KP +
        (0x58, 96),  // KP Enter
        (0x59, 79),  // KP 1
        (0x5A, 80),  //
        (0x5B, 81),  //
        (0x5C, 75),  //
        (0x5D, 76),  //
        (0x5E, 77),  //
        (0x5F, 71),  //
        (0x60, 72),  //
        (0x61, 73),  // KP 9
        (0x62, 82),  // KP 0
        (0x63, 83),  // KP .
        (0x64, 86),  // ISO key (102ND)
        (0x65, 127), // Menu (Compose)
        (0x66, 116), // Power
        (0x67, 117), // KP =
        (0x68, 183), // F13
        (0x69, 184),
        (0x6A, 185),
        (0x6B, 186),
        (0x6C, 187),
        (0x6D, 188),
        (0x6E, 189),
        (0x6F, 190),
        (0x70, 191),
        (0x71, 192),
        (0x72, 193),
        (0x73, 194), // F24
        (0x74, 134), // Open
        (0x75, 138), // Help
        (0x76, 130), // Props
        (0x77, 132), // Front
        (0x78, 128), // Stop
        (0x79, 129), // Again
        (0x7A, 131), // Undo
        (0x7B, 137), // Cut
        (0x7C, 133), // Copy
        (0x7D, 135), // Paste
        (0x7E, 136), // Find
        (0x7F, 113), // Mute
        (0x80, 115), // Volume Up
        (0x81, 114), // Volume Down
        (0x85, 121), // KP ,
        (0x87, 89),  // Ro
        (0x88, 93),  // Katakana/Hiragana
        (0x89, 124), // Yen
        (0x8A, 92),  // Henkan
        (0x8B, 94),  // Muhenkan
        (0x8C, 95),  // KP JP ,
        (0x90, 122), // Hangeul
        (0x91, 123), // Hanja
        (0x92, 90),  // Katakana
        (0x93, 91),  // Hiragana
        (0x94, 85),  // Zenkaku/Hankaku
    ];
    i = 0;
    while i < rest.len() {
        t[rest[i].0] = rest[i].1;
        i += 1;
    }
    let mods: [u16; 8] = [29, 42, 56, 125, 97, 54, 100, 126];
    i = 0;
    while i < 8 {
        t[0xE0 + i] = mods[i];
        i += 1;
    }
    t
};

/// The Linux key for HID usage `u` (page << 16 | id): the keyboard page,
/// the consumer page's media and browser keys, system power keys.
pub fn hid_key(u: u32) -> Option<u16> {
    let (page, id) = ((u >> 16) as u16, u as u16);
    let k = match page {
        0x07 => *HID_KEYBOARD.get(id as usize)?,
        0x0C => match id {
            0x6F => 225,  // brightness up
            0x70 => 224,  // brightness down
            0xB5 => 163,  // next song
            0xB6 => 165,  // previous song
            0xB7 => 166,  // stop
            0xCD => 164,  // play/pause
            0xE2 => 113,  // mute
            0xE9 => 115,  // volume up
            0xEA => 114,  // volume down
            0x18A => 155, // mail
            0x192 => 140, // calculator
            0x221 => 217, // search
            0x223 => 172, // home page
            0x224 => KEY_BACK,
            0x225 => KEY_FORWARD,
            0x227 => 173, // refresh
            _ => 0,
        },
        0x01 => match id {
            0x81 => 116, // power
            0x82 => 142, // sleep
            0x83 => 143, // wake up
            _ => 0,
        },
        _ => 0,
    };
    (k != 0).then_some(k)
}

/// The Linux key for a PS/2 set-1 scancode (`ext`: after an `E0` prefix).
/// Without the prefix, Linux's key numbers are the scancodes themselves.
pub fn ps2_key(ext: bool, sc: u8) -> Option<u16> {
    if !ext {
        return matches!(sc, 0x01..=0x58).then_some(sc as u16);
    }
    Some(match sc {
        0x1C => 96,  // KP Enter
        0x1D => 97,  // right Ctrl
        0x35 => 98,  // KP /
        0x37 => 99,  // Print Screen
        0x38 => 100, // right Alt
        0x47 => 102, // Home
        0x48 => 103, // Up
        0x49 => 104, // Page Up
        0x4B => 105, // Left
        0x4D => 106, // Right
        0x4F => 107, // End
        0x50 => 108, // Down
        0x51 => 109, // Page Down
        0x52 => 110, // Insert
        0x53 => 111, // Delete
        0x5B => 125, // left Meta
        0x5C => 126, // right Meta
        0x5D => 127, // Menu
        0x20 => 113, // Mute
        0x2E => 114, // Volume Down
        0x30 => 115, // Volume Up
        0x22 => 164, // Play/Pause
        0x19 => 163, // Next
        0x10 => 165, // Previous
        0x6A => KEY_BACK,
        0x69 => KEY_FORWARD,
        0x5F => 142, // Sleep
        0x5E => 116, // Power
        _ => return None,
    })
}

/// The HID usage (keyboard page) of Linux key `k`, for keys a program
/// types through `/dev/uinput` into WaveOS's own input.
pub fn key_to_hid(k: u16) -> Option<u32> {
    if k == 43 {
        return Some(0x07_0031);
    }
    (0x04..0xE8u32).find(|&id| HID_KEYBOARD[id as usize] == k).map(|id| 0x07_0000 | id)
}

/// All the keys a keyboard has, for its `EV_KEY` bitmap: those of the
/// keyboard page, the media keys and the power keys.
pub fn keyboard_keys() -> impl Iterator<Item = u16> {
    let media = [225, 224, 163, 165, 166, 164, 113, 115, 114, 155, 140, 217, 172, KEY_BACK, KEY_FORWARD, 173, 142, 143];
    HID_KEYBOARD.into_iter().filter(|&k| k != 0).chain(media)
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    const KEY_A: u16 = 30;
    assert_eq!(hid_key(0x07_0004), Some(KEY_A));
    assert_eq!(hid_key(0x07_0039), Some(KEY_CAPSLOCK));
    assert_eq!(hid_key(0x07_00E1), Some(42), "left shift");
    assert_eq!(hid_key(0x0C_00E9), Some(115), "volume up");
    assert_eq!(ps2_key(false, 0x1E), Some(KEY_A));
    assert_eq!(ps2_key(true, 0x48), Some(103), "up");
    assert_eq!(key_to_hid(KEY_A), Some(0x07_0004));
    assert_eq!(key_to_hid(28), Some(0x07_0028), "enter");
}
