//! Aurora design tokens: colors, radii and fonts, in light and dark variants.

use super::canvas::rgb;
use super::font::{self, Face, Font};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

pub const MENUBAR_H: i32 = 30;
pub const TITLEBAR_H: i32 = 40;
pub const WINDOW_RADIUS: i32 = 12;
pub const DOCK_ICON: i32 = 48;
pub const DOCK_PAD: i32 = 9;
pub const DOCK_H: i32 = DOCK_ICON + 2 * DOCK_PAD + 4;
pub const DOCK_MARGIN: i32 = 10;

pub const ACCENT: u32 = rgb(0x7C, 0x5C, 0xFF);
pub const ACCENT_2: u32 = rgb(0x2D, 0xD4, 0xBF);
pub const CLOSE: u32 = rgb(0xFF, 0x5F, 0x57);
pub const MINIMIZE: u32 = rgb(0xFE, 0xBC, 0x2E);
pub const ZOOM: u32 = rgb(0x28, 0xC8, 0x40);

pub struct Theme {
    pub dark: bool,
    pub window_bg: u32,
    pub window_bg_alt: u32,
    pub window_border: u32,
    pub titlebar: u32,
    pub separator: u32,
    pub text: u32,
    pub text_secondary: u32,
    pub text_on_glass: u32,
    pub glass_tint: u32,
    pub panel_tint: u32,
    pub hover: u32,
    pub control_bg: u32,
    pub control_border: u32,
    pub traffic_inactive: u32,
    pub shadow: u32,
}

pub static LIGHT: Theme = Theme {
    dark: false,
    window_bg: rgb(0xFA, 0xFA, 0xFC),
    window_bg_alt: rgb(0xF0, 0xF0, 0xF5),
    window_border: 0x2A00_0000,
    titlebar: rgb(0xF3, 0xF3, 0xF7),
    separator: 0x1A00_0000,
    text: rgb(0x1D, 0x1D, 0x24),
    text_secondary: rgb(0x6E, 0x6E, 0x7A),
    text_on_glass: rgb(0x14, 0x14, 0x1C),
    glass_tint: 0xB4F6_F6FA,
    panel_tint: 0xD8F6_F6FA,
    hover: 0x1400_0000,
    control_bg: rgb(0xFF, 0xFF, 0xFF),
    control_border: 0x2600_0000,
    traffic_inactive: rgb(0xD4, 0xD4, 0xDA),
    shadow: 90,
};

pub static DARK: Theme = Theme {
    dark: true,
    window_bg: rgb(0x23, 0x23, 0x2B),
    window_bg_alt: rgb(0x1B, 0x1B, 0x22),
    window_border: 0x40FF_FFFF,
    titlebar: rgb(0x2A, 0x2A, 0x33),
    separator: 0x22FF_FFFF,
    text: rgb(0xEE, 0xEE, 0xF4),
    text_secondary: rgb(0x9A, 0x9A, 0xA8),
    text_on_glass: rgb(0xF4, 0xF4, 0xFA),
    glass_tint: 0x8C1A_1A24,
    panel_tint: 0xD01E_1E28,
    hover: 0x1CFF_FFFF,
    control_bg: rgb(0x33, 0x33, 0x3D),
    control_border: 0x30FF_FFFF,
    traffic_inactive: rgb(0x4A, 0x4A, 0x55),
    shadow: 140,
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);
static WALLPAPER: AtomicU8 = AtomicU8::new(0);

pub fn current() -> &'static Theme {
    if DARK_MODE.load(Ordering::Relaxed) {
        &DARK
    } else {
        &LIGHT
    }
}

pub fn set_dark(dark: bool) {
    DARK_MODE.store(dark, Ordering::Relaxed);
}

pub fn wallpaper() -> u8 {
    WALLPAPER.load(Ordering::Relaxed)
}

pub fn set_wallpaper(i: u8) {
    WALLPAPER.store(i, Ordering::Relaxed);
}

pub fn ui(size: u16) -> Font {
    font::get(Face::Regular, size)
}

pub fn ui_bold(size: u16) -> Font {
    font::get(Face::SemiBold, size)
}

pub fn mono(size: u16) -> Font {
    font::get(Face::Mono, size)
}
