//! Aurora design tokens: colors, radii and fonts, in light and dark variants.

use crate::canvas::rgb;
use crate::font::{self, Face, Font};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// The band the top bubbles live in (8 + 38 + 8).
pub const MENUBAR_H: i32 = 54;
pub const TITLEBAR_H: i32 = 40;
pub const WINDOW_RADIUS: i32 = 18;
pub const DOCK_ICON: i32 = 48;
pub const DOCK_PAD: i32 = 9;
pub const DOCK_H: i32 = DOCK_ICON + 2 * DOCK_PAD + 4;
pub const DOCK_MARGIN: i32 = 10;

/// Aurora violet, the accent of WayV's mark (also the fallback accent).
pub const ACCENT: u32 = rgb(0x7C, 0x5C, 0xFF);

/// Accent choices offered in Settings: (name, colour). The first, "Wallpaper",
/// takes the accent from the wallpaper (see [`accent`]); its colour here is a
/// placeholder.
pub const ACCENTS: [(&str, u32); 8] = [
    ("Wallpaper", rgb(0x2F, 0x62, 0xF5)),
    ("Violet", ACCENT),
    ("Blue", rgb(0x0A, 0x84, 0xFF)),
    ("Teal", rgb(0x14, 0xB8, 0xA6)),
    ("Green", rgb(0x30, 0xB0, 0x50)),
    ("Orange", rgb(0xFF, 0x8A, 0x00)),
    ("Pink", rgb(0xF0, 0x3E, 0x7A)),
    ("Graphite", rgb(0x7A, 0x7A, 0x86)),
];

/// The accent tones a wallpaper provides: `fill` carries white text at 4.5:1
/// (buttons, selection), `glow` is the waterline and Crest edge, and `ink`
/// is accent-coloured text on dark and on light surfaces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Tones {
    pub fill: u32,
    pub glow: u32,
    pub ink_dark: u32,
    pub ink_light: u32,
}

/// Tones of the built-in wallpapers, by index (Tide, Aurora, Dunes).
pub const WALLPAPER_TONES: [Tones; 3] = [
    Tones {
        fill: rgb(0x2F, 0x62, 0xF5),
        glow: rgb(0x6F, 0xE3, 0xFF),
        ink_dark: rgb(0x86, 0xB4, 0xFF),
        ink_light: rgb(0x1F, 0x4F, 0xD6),
    },
    Tones {
        fill: rgb(0x0A, 0x7A, 0x69),
        glow: rgb(0x3D, 0xF5, 0xC8),
        ink_dark: rgb(0x5E, 0xF0, 0xCF),
        ink_light: rgb(0x08, 0x75, 0x63),
    },
    Tones {
        fill: rgb(0xC8, 0x45, 0x2C),
        glow: rgb(0xFF, 0xB3, 0x8A),
        ink_dark: rgb(0xFF, 0x9B, 0x7A),
        ink_light: rgb(0xA8, 0x3A, 0x24),
    },
];

static ACCENT_INDEX: AtomicU8 = AtomicU8::new(0);
/// Tones sampled from the user's own picture (see [`tones_from_picture`]).
static CUSTOM_TONES: spin::Mutex<Tones> = spin::Mutex::new(Tones {
    fill: ACCENT,
    glow: rgb(0xB9, 0xA8, 0xFF),
    ink_dark: rgb(0xB0, 0x9C, 0xFF),
    ink_light: rgb(0x5A, 0x3C, 0xE0),
});

/// The tones in effect: the wallpaper's, or a fixed accent the user picked.
pub fn tones() -> Tones {
    let i = ACCENT_INDEX.load(Ordering::Relaxed) as usize % ACCENTS.len();
    if i == 0 {
        let wp = WALLPAPER.load(Ordering::Relaxed);
        return match WALLPAPER_TONES.get(wp as usize) {
            Some(t) => *t,
            None => *CUSTOM_TONES.lock(),
        };
    }
    let fill = ACCENTS[i].1;
    Tones { fill, glow: mix_white(fill, 110), ink_dark: mix_white(fill, 90), ink_light: fill }
}

fn mix_white(c: u32, amount: u32) -> u32 {
    let ch = |s: u32| {
        let v = (c >> s) & 0xFF;
        (v + (255 - v) * amount / 255) << s
    };
    0xFF00_0000 | ch(16) | ch(8) | ch(0)
}

/// The accent colour for selection, focus rings and primary buttons.
pub fn accent() -> u32 {
    tones().fill
}

/// The waterline and Crest glow.
pub fn accent_glow() -> u32 {
    tones().glow
}

/// Accent-coloured text and glyphs on the current appearance.
pub fn accent_ink() -> u32 {
    let t = tones();
    if DARK_MODE.load(Ordering::Relaxed) {
        t.ink_dark
    } else {
        t.ink_light
    }
}

/// The waterline colour: the glow on dark, the fill on light (the glow is too pale there).
pub fn waterline() -> u32 {
    if DARK_MODE.load(Ordering::Relaxed) {
        accent_glow()
    } else {
        accent()
    }
}

pub fn accent_index() -> u8 {
    ACCENT_INDEX.load(Ordering::Relaxed)
}

pub fn set_accent(i: u8) {
    ACCENT_INDEX.store(i % ACCENTS.len() as u8, Ordering::Relaxed);
}

/// Uses tones sampled from a custom wallpaper picture.
pub fn set_custom_tones(t: Tones) {
    *CUSTOM_TONES.lock() = t;
}

fn luminance(c: u32) -> u32 {
    // Rec. 709 weights on gamma-encoded channels, 0..=255: close enough to pick a shade.
    (((c >> 16) & 255) * 54 + ((c >> 8) & 255) * 183 + (c & 255) * 19) >> 8
}

/// The adaptive accent: a coarse sample of the picture, weighted by colourfulness,
/// gives a hue; four tones are derived from it, darkening the fill until white
/// text on it is readable. Greyscale pictures fall back to WayV violet.
pub fn tones_from_picture(px: &[u32], w: i32, h: i32) -> Tones {
    let (mut r, mut g, mut b, mut weight) = (0u64, 0u64, 0u64, 0u64);
    let (cols, rows) = (64, 40);
    for j in 0..rows {
        for i in 0..cols {
            let x = (i * w / cols).min(w - 1);
            let y = (j * h / rows).min(h - 1);
            let Some(&p) = px.get((y * w + x) as usize) else { continue };
            let (pr, pg, pb) = ((p >> 16) & 255, (p >> 8) & 255, p & 255);
            let chroma = pr.max(pg).max(pb) - pr.min(pg).min(pb);
            let wgt = (chroma * chroma) as u64;
            r += pr as u64 * wgt;
            g += pg as u64 * wgt;
            b += pb as u64 * wgt;
            weight += wgt;
        }
    }
    if weight < 64 * 64 * 200 {
        return tones_from_fill(ACCENT);
    }
    let (r, g, b) = ((r / weight) as u32, (g / weight) as u32, (b / weight) as u32);
    // Stretch to full saturation, then shade.
    let (lo, hi) = (r.min(g).min(b), r.max(g).max(b).max(1));
    let span = (hi - lo).max(1);
    let pure = |c: u32| (c - lo) * 255 / span;
    let hue = rgb(pure(r) as u8, pure(g) as u8, pure(b) as u8);
    let shade = |c: u32, k: u32| {
        let ch = |s: u32| (((c >> s) & 255) * k / 255) << s;
        0xFF00_0000 | ch(16) | ch(8) | ch(0)
    };
    // White text needs the fill's luminance at or below about 0.38 (4.5:1).
    let mut k = 230;
    while k > 60 && luminance(shade(hue, k)) > 97 {
        k -= 5;
    }
    tones_from_fill(shade(hue, k))
}

/// The four tones from a readable fill colour (apps receive only the fill).
pub fn tones_from_fill(fill: u32) -> Tones {
    let fill = fill | 0xFF00_0000;
    Tones { fill, glow: mix_white(fill, 150), ink_dark: mix_white(fill, 120), ink_light: fill }
}

/// The custom picture's fill colour (0 when none was sampled).
pub fn custom_fill() -> u32 {
    CUSTOM_TONES.lock().fill & 0x00FF_FFFF
}

pub const ACCENT_2: u32 = rgb(0x2D, 0xD4, 0xBF);
/// Destructive red (close buttons, Danger).
pub const CLOSE: u32 = rgb(0xFF, 0x5F, 0x57);
pub const MINIMIZE: u32 = rgb(0xFE, 0xBC, 0x2E);
pub const ZOOM: u32 = rgb(0x28, 0xC8, 0x40);

#[derive(Clone, Copy)]
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

// Living water: a deep navy night and a cool paper day. Glass tints are the
// Mist material; `panel_tint` is Shallows (menus, popovers, Current).
pub static LIGHT: Theme = Theme {
    dark: false,
    window_bg: rgb(0xFB, 0xFC, 0xFF),
    window_bg_alt: rgb(0xF1, 0xF4, 0xFC),
    window_border: 0x1A0E_1530,
    titlebar: rgb(0xF3, 0xF6, 0xFD),
    separator: 0x140E_1530,
    text: rgb(0x0E, 0x15, 0x30),
    text_secondary: rgb(0x5F, 0x64, 0x76),
    text_on_glass: rgb(0x0E, 0x15, 0x30),
    glass_tint: 0x8CFF_FFFF,
    panel_tint: 0xF5FF_FFFF,
    hover: 0x130E_1530,
    control_bg: rgb(0xFF, 0xFF, 0xFF),
    control_border: 0x1F0E_1530,
    traffic_inactive: rgb(0xD4, 0xD8, 0xE4),
    shadow: 80,
};

pub static DARK: Theme = Theme {
    dark: true,
    window_bg: rgb(0x11, 0x15, 0x27),
    window_bg_alt: rgb(0x17, 0x1C, 0x32),
    window_border: 0x1CFF_FFFF,
    titlebar: rgb(0x17, 0x1C, 0x32),
    separator: 0x16FF_FFFF,
    text: rgb(0xF2, 0xF4, 0xFA),
    text_secondary: rgb(0xA3, 0xA7, 0xB7),
    text_on_glass: rgb(0xF2, 0xF4, 0xFA),
    glass_tint: 0x7510_162E,
    panel_tint: 0xF51E_243C,
    hover: 0x1AFF_FFFF,
    control_bg: rgb(0x1F, 0x24, 0x3A),
    control_border: 0x24FF_FFFF,
    traffic_inactive: rgb(0x3A, 0x40, 0x58),
    shadow: 150,
};

static DARK_MODE: AtomicBool = AtomicBool::new(false);
static WALLPAPER: AtomicU8 = AtomicU8::new(0);
static GLASS: AtomicU8 = AtomicU8::new(70);
static EFFECTS: AtomicU8 = AtomicU8::new(0);
static DOCK_SIZE: AtomicU8 = AtomicU8::new(48);

pub fn set_effects(glass: u8, flags: u8) {
    GLASS.store(glass.min(100), Ordering::Relaxed);
    EFFECTS.store(flags, Ordering::Relaxed);
}
pub fn glass_intensity() -> u8 {
    GLASS.load(Ordering::Relaxed)
}
pub fn reduce_transparency() -> bool {
    EFFECTS.load(Ordering::Relaxed) & 1 != 0
}
pub fn reduce_motion() -> bool {
    EFFECTS.load(Ordering::Relaxed) & 2 != 0
}
pub fn high_contrast() -> bool {
    EFFECTS.load(Ordering::Relaxed) & 4 != 0
}
pub fn dock_autohide() -> bool {
    EFFECTS.load(Ordering::Relaxed) & 8 != 0
}
pub fn set_dock_size(size: u8) {
    DOCK_SIZE.store(size.clamp(32, 64), Ordering::Relaxed);
}
pub fn dock_icon_size() -> i32 {
    DOCK_SIZE.load(Ordering::Relaxed) as i32
}
pub fn dock_height() -> i32 {
    dock_icon_size() + 2 * DOCK_PAD + 4
}

static LIGHT_CONTRAST: Theme = Theme {
    text: 0xff080810,
    text_secondary: 0xff303040,
    window_border: 0xff454555,
    control_border: 0xff454555,
    ..LIGHT
};
static DARK_CONTRAST: Theme = Theme {
    text: 0xffffffff,
    text_secondary: 0xffe0e0ef,
    window_border: 0xffaaaaaf,
    control_border: 0xffaaaaaf,
    ..DARK
};

pub fn current() -> &'static Theme {
    if high_contrast() {
        return if DARK_MODE.load(Ordering::Relaxed) { &DARK_CONTRAST } else { &LIGHT_CONTRAST };
    }
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

/// Display weight for large numerals and titles (the Shore clock, About).
pub fn display(size: u16) -> Font {
    font::get(Face::Bold, size)
}

pub fn mono(size: u16) -> Font {
    font::get(Face::Mono, size)
}
