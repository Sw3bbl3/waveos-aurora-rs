//! HID usages this crate looks for, as 32-bit "extended" usages: the usage
//! page in the high 16 bits, the usage in the low 16.

/// An extended usage from its page and id.
pub const fn u(page: u16, id: u16) -> u32 {
    (page as u32) << 16 | id as u32
}

/// The page of an extended usage.
pub const fn page(usage: u32) -> u16 {
    (usage >> 16) as u16
}

/// The id within its page.
pub const fn id(usage: u32) -> u16 {
    usage as u16
}

pub const GENERIC_DESKTOP: u16 = 0x01;
pub const KEYBOARD_PAGE: u16 = 0x07;
pub const LED_PAGE: u16 = 0x08;
pub const BUTTON_PAGE: u16 = 0x09;
pub const CONSUMER_PAGE: u16 = 0x0C;
pub const DIGITIZER_PAGE: u16 = 0x0D;

// Generic desktop: application collections.
pub const POINTER: u32 = u(GENERIC_DESKTOP, 0x01);
pub const MOUSE: u32 = u(GENERIC_DESKTOP, 0x02);
pub const JOYSTICK: u32 = u(GENERIC_DESKTOP, 0x04);
pub const GAMEPAD: u32 = u(GENERIC_DESKTOP, 0x05);
pub const KEYBOARD: u32 = u(GENERIC_DESKTOP, 0x06);
pub const KEYPAD: u32 = u(GENERIC_DESKTOP, 0x07);
pub const MULTI_AXIS: u32 = u(GENERIC_DESKTOP, 0x08);
pub const SYSTEM_CONTROL: u32 = u(GENERIC_DESKTOP, 0x80);

// Generic desktop: controls.
pub const X: u32 = u(GENERIC_DESKTOP, 0x30);
pub const Y: u32 = u(GENERIC_DESKTOP, 0x31);
pub const Z: u32 = u(GENERIC_DESKTOP, 0x32);
pub const RX: u32 = u(GENERIC_DESKTOP, 0x33);
pub const RY: u32 = u(GENERIC_DESKTOP, 0x34);
pub const RZ: u32 = u(GENERIC_DESKTOP, 0x35);
pub const SLIDER: u32 = u(GENERIC_DESKTOP, 0x36);
pub const WHEEL: u32 = u(GENERIC_DESKTOP, 0x38);
pub const HAT_SWITCH: u32 = u(GENERIC_DESKTOP, 0x39);
pub const RESOLUTION_MULTIPLIER: u32 = u(GENERIC_DESKTOP, 0x48);
pub const SYSTEM_POWER_DOWN: u32 = u(GENERIC_DESKTOP, 0x81);
pub const SYSTEM_SLEEP: u32 = u(GENERIC_DESKTOP, 0x82);
pub const SYSTEM_WAKE_UP: u32 = u(GENERIC_DESKTOP, 0x83);

// Keyboard page.
pub const KEY_ERROR_ROLLOVER: u32 = u(KEYBOARD_PAGE, 0x01);
pub const KEY_LEFT_CTRL: u32 = u(KEYBOARD_PAGE, 0xE0);
pub const KEY_RIGHT_GUI: u32 = u(KEYBOARD_PAGE, 0xE7);

// LEDs.
pub const LED_NUM_LOCK: u32 = u(LED_PAGE, 0x01);
pub const LED_CAPS_LOCK: u32 = u(LED_PAGE, 0x02);
pub const LED_SCROLL_LOCK: u32 = u(LED_PAGE, 0x03);

// Consumer page.
pub const CONSUMER_CONTROL: u32 = u(CONSUMER_PAGE, 0x01);
pub const BRIGHTNESS_UP: u32 = u(CONSUMER_PAGE, 0x6F);
pub const BRIGHTNESS_DOWN: u32 = u(CONSUMER_PAGE, 0x70);
pub const NEXT_TRACK: u32 = u(CONSUMER_PAGE, 0xB5);
pub const PREVIOUS_TRACK: u32 = u(CONSUMER_PAGE, 0xB6);
pub const STOP: u32 = u(CONSUMER_PAGE, 0xB7);
pub const PLAY_PAUSE: u32 = u(CONSUMER_PAGE, 0xCD);
pub const MUTE: u32 = u(CONSUMER_PAGE, 0xE2);
pub const VOLUME_UP: u32 = u(CONSUMER_PAGE, 0xE9);
pub const VOLUME_DOWN: u32 = u(CONSUMER_PAGE, 0xEA);
pub const AC_SEARCH: u32 = u(CONSUMER_PAGE, 0x221);
pub const AC_HOME: u32 = u(CONSUMER_PAGE, 0x223);
pub const AC_BACK: u32 = u(CONSUMER_PAGE, 0x224);
pub const AC_FORWARD: u32 = u(CONSUMER_PAGE, 0x225);
pub const AC_PAN: u32 = u(CONSUMER_PAGE, 0x238);

// Digitizers.
pub const PEN: u32 = u(DIGITIZER_PAGE, 0x02);
pub const TOUCH_SCREEN: u32 = u(DIGITIZER_PAGE, 0x04);
pub const TOUCH_PAD: u32 = u(DIGITIZER_PAGE, 0x05);
pub const DEVICE_CONFIGURATION: u32 = u(DIGITIZER_PAGE, 0x0E);
pub const FINGER: u32 = u(DIGITIZER_PAGE, 0x22);
pub const DEVICE_SETTINGS: u32 = u(DIGITIZER_PAGE, 0x23);
pub const IN_RANGE: u32 = u(DIGITIZER_PAGE, 0x32);
pub const TIP_SWITCH: u32 = u(DIGITIZER_PAGE, 0x42);
pub const CONFIDENCE: u32 = u(DIGITIZER_PAGE, 0x47);
pub const CONTACT_ID: u32 = u(DIGITIZER_PAGE, 0x51);
/// "Device Mode" / "Input Mode": 3 makes a Precision Touchpad report contacts.
pub const INPUT_MODE: u32 = u(DIGITIZER_PAGE, 0x52);
pub const CONTACT_COUNT: u32 = u(DIGITIZER_PAGE, 0x54);
pub const CONTACT_COUNT_MAXIMUM: u32 = u(DIGITIZER_PAGE, 0x55);
pub const SCAN_TIME: u32 = u(DIGITIZER_PAGE, 0x56);
pub const SURFACE_SWITCH: u32 = u(DIGITIZER_PAGE, 0x57);
pub const BUTTON_SWITCH: u32 = u(DIGITIZER_PAGE, 0x58);
pub const PAD_TYPE: u32 = u(DIGITIZER_PAGE, 0x59);

/// Button `n` (1-based) on the button page.
pub const fn button(n: u16) -> u32 {
    u(BUTTON_PAGE, n)
}
