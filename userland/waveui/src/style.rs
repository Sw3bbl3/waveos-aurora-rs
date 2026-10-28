//! The design system in code: type scale, spacing, radii and semantic
//! colours. Views take their look from here, so every app reads as WaveOS
//! and follows the appearance (light, dark, high contrast, accent colour).

use lumen::canvas::{mix, rgb};
use lumen::font::Font;
use lumen::theme;

/// Text styles, largest first. Each maps to a face and size of Manrope.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextStyle {
    /// Big numbers and the title of a page (26 px, bold).
    LargeTitle,
    /// The title of a window's content (20 px, semibold).
    Title,
    /// Section titles (16 px, semibold).
    Title2,
    /// Emphasized body text, row titles (13 px, semibold).
    Headline,
    /// Everything else (13 px).
    #[default]
    Body,
    /// Secondary explanations (12 px).
    Callout,
    /// Under a row title (12 px).
    Subheadline,
    /// Small print, section footers (11 px).
    Footnote,
    /// Labels on small controls (10 px).
    Caption,
    /// Code and terminals (13 px, monospaced).
    Monospaced,
    /// Any size, for the rare place the scale doesn't fit (a calculator's
    /// display, a clock face).
    Sized(u16, Weight),
}

/// The weight of a [`TextStyle::Sized`] face.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Weight {
    #[default]
    Regular,
    Bold,
    /// The display face (titles).
    Display,
    Mono,
}

impl TextStyle {
    pub fn font(self) -> Font {
        match self {
            TextStyle::LargeTitle => theme::display(26),
            TextStyle::Title => theme::ui_bold(20),
            TextStyle::Title2 => theme::ui_bold(16),
            TextStyle::Headline => theme::ui_bold(13),
            TextStyle::Body => theme::ui(13),
            TextStyle::Callout | TextStyle::Subheadline => theme::ui(12),
            TextStyle::Footnote => theme::ui(11),
            TextStyle::Caption => theme::ui(10),
            TextStyle::Monospaced => theme::mono(13),
            TextStyle::Sized(size, w) => match w {
                Weight::Regular => theme::ui(size),
                Weight::Bold => theme::ui_bold(size),
                Weight::Display => theme::display(size),
                Weight::Mono => theme::mono(size),
            },
        }
    }
}

/// Spacing steps (pixels). Layouts use these rather than ad-hoc numbers.
pub mod space {
    pub const XXS: i32 = 2;
    pub const XS: i32 = 4;
    pub const S: i32 = 8;
    pub const M: i32 = 12;
    pub const L: i32 = 16;
    pub const XL: i32 = 24;
    pub const XXL: i32 = 32;
}

/// Corner radii.
pub mod radius {
    /// Buttons, fields, segmented controls.
    pub const CONTROL: i32 = 8;
    /// Grouped lists, cards.
    pub const CARD: i32 = 12;
    /// Sheets and popovers.
    pub const SHEET: i32 = 16;
}

/// Standard control heights.
pub mod size {
    pub const CONTROL: i32 = 30;
    pub const CONTROL_LARGE: i32 = 38;
    pub const CONTROL_SMALL: i32 = 24;
    pub const ROW: i32 = 40;
    pub const SIDEBAR_ROW: i32 = 32;
    pub const TOOLBAR: i32 = 48;
}

/// Colours by meaning, for the current appearance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// Primary text.
    Label,
    /// Less important text.
    SecondaryLabel,
    /// Hints, placeholders, disabled text.
    TertiaryLabel,
    /// The accent (selection, primary buttons, links).
    Accent,
    /// Accent-coloured text that stays readable on the background.
    AccentText,
    /// Text on the accent colour.
    OnAccent,
    /// Destructive actions.
    Red,
    Orange,
    Yellow,
    Green,
    Teal,
    Blue,
    Purple,
    Pink,
    Gray,
    /// The window's background.
    Background,
    /// Grouped lists sit on this (a step darker than the cards).
    GroupedBackground,
    /// Cards and grouped rows.
    SecondaryBackground,
    /// Controls (fields, segmented control).
    ControlBackground,
    /// Hairlines between rows.
    Separator,
    /// A subtle fill (hover, unselected chips).
    Fill,
    /// Exactly this colour (0xAARRGGBB).
    Custom(u32),
    Clear,
}

impl Color {
    pub fn argb(self) -> u32 {
        let t = theme::current();
        let dark = t.dark;
        let pick = |light: u32, dark_c: u32| if dark { dark_c } else { light };
        match self {
            Color::Label => t.text,
            Color::SecondaryLabel => t.text_secondary,
            Color::TertiaryLabel => mix(t.text_secondary, t.window_bg, 96),
            Color::Accent => theme::accent(),
            Color::AccentText => theme::accent_ink(),
            Color::OnAccent => rgb(0xFF, 0xFF, 0xFF),
            Color::Red => pick(rgb(0xE5, 0x3E, 0x3E), rgb(0xFF, 0x6B, 0x63)),
            Color::Orange => pick(rgb(0xE8, 0x7A, 0x00), rgb(0xFF, 0x9F, 0x2E)),
            Color::Yellow => pick(rgb(0xC9, 0x9A, 0x00), rgb(0xFF, 0xD6, 0x3C)),
            Color::Green => pick(rgb(0x1F, 0x9D, 0x4B), rgb(0x40, 0xD3, 0x6A)),
            Color::Teal => pick(rgb(0x0A, 0x9C, 0x8E), rgb(0x3D, 0xE0, 0xC8)),
            Color::Blue => pick(rgb(0x1F, 0x6F, 0xEB), rgb(0x5A, 0xA2, 0xFF)),
            Color::Purple => pick(rgb(0x7C, 0x5C, 0xFF), rgb(0xA9, 0x91, 0xFF)),
            Color::Pink => pick(rgb(0xE0, 0x3E, 0x7A), rgb(0xFF, 0x6E, 0xA3)),
            Color::Gray => pick(rgb(0x8A, 0x8E, 0x9C), rgb(0x8E, 0x93, 0xA6)),
            Color::Background => t.window_bg,
            Color::GroupedBackground => t.window_bg_alt,
            Color::SecondaryBackground => pick(rgb(0xFF, 0xFF, 0xFF), rgb(0x1B, 0x20, 0x36)),
            Color::ControlBackground => t.control_bg,
            Color::Separator => t.separator,
            Color::Fill => t.hover,
            Color::Custom(c) => c,
            Color::Clear => 0,
        }
    }
}

/// Insets on each side.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Insets {
    pub top: i32,
    pub leading: i32,
    pub bottom: i32,
    pub trailing: i32,
}

impl Insets {
    pub const fn all(v: i32) -> Insets {
        Insets { top: v, leading: v, bottom: v, trailing: v }
    }
    pub const fn xy(x: i32, y: i32) -> Insets {
        Insets { top: y, leading: x, bottom: y, trailing: x }
    }
    pub fn horizontal(self) -> i32 {
        self.leading + self.trailing
    }
    pub fn vertical(self) -> i32 {
        self.top + self.bottom
    }
}

/// Alignment across a stack (or both axes in a ZStack / frame).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    Start,
    #[default]
    Center,
    End,
    /// Fill the cross axis.
    Stretch,
}
