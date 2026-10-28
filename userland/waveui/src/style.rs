//! The design system in code: type scale, spacing, radii and semantic
//! colours. Views take their look from here, so every app reads as WaveOS
//! and follows the appearance (light, dark, high contrast, accent colour).

use lumen::canvas::{mix, rgb, with_alpha};
use lumen::font::Font;
use lumen::theme;

/// Text styles, largest first. Each maps to a face and size of Manrope
/// (the WaveOS type scale: Display 56, Title 1 30, Title 2 22, Headline 15,
/// Body 14, Callout 13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TextStyle {
    /// Big numbers and the title of a page (30 px, extra bold).
    LargeTitle,
    /// The title of a window's content (22 px, bold).
    Title,
    /// Section titles (17 px, bold).
    Title2,
    /// Emphasized body text, row titles (15 px, bold).
    Headline,
    /// Everything else (14 px).
    #[default]
    Body,
    /// Secondary explanations (13 px).
    Callout,
    /// Under a row title (12 px).
    Subheadline,
    /// Small print, section footers (12 px).
    Footnote,
    /// Labels on small controls, status lines (11 px, bold).
    Caption,
    /// Small capitals over a group: "PLACES" (11 px, bold; see [`crate::Text::label`]).
    Label,
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
    /// Thin numerals (a calculator's display, a timer).
    Light,
}

impl TextStyle {
    pub fn font(self) -> Font {
        match self {
            TextStyle::LargeTitle => theme::display(30),
            TextStyle::Title => theme::ui_bold(22),
            TextStyle::Title2 => theme::ui_bold(17),
            TextStyle::Headline => theme::ui_bold(15),
            TextStyle::Body => theme::ui(14),
            TextStyle::Callout => theme::ui(13),
            TextStyle::Subheadline | TextStyle::Footnote => theme::ui(12),
            TextStyle::Caption | TextStyle::Label => theme::ui_bold(11),
            TextStyle::Monospaced => theme::mono(13),
            TextStyle::Sized(size, w) => match w {
                Weight::Regular => theme::ui(size),
                Weight::Bold => theme::ui_bold(size),
                Weight::Display => theme::display(size),
                Weight::Mono => theme::mono(size),
                Weight::Light => theme::light(size),
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

/// Corner radii. (Buttons, search fields and switches are capsules.)
pub mod radius {
    /// Text fields.
    pub const CONTROL: i32 = 11;
    /// Sidebar items and selected rows.
    pub const ITEM: i32 = 10;
    /// Grouped lists, cards.
    pub const CARD: i32 = 14;
    /// Sheets, popovers and windows.
    pub const SHEET: i32 = 18;
}

/// Standard control heights.
pub mod size {
    pub const CONTROL: i32 = 32;
    pub const CONTROL_LARGE: i32 = 40;
    pub const CONTROL_SMALL: i32 = 26;
    /// Text fields.
    pub const FIELD: i32 = 34;
    pub const ROW: i32 = 42;
    pub const SIDEBAR_ROW: i32 = 32;
    pub const TOOLBAR: i32 = 54;
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
    /// A subtle fill: cards, fields (6% of the text colour).
    Fill,
    /// A stronger fill: buttons, segmented controls, chips (10%).
    SecondaryFill,
    /// The strongest fill: switch and slider tracks (16%).
    TertiaryFill,
    /// Exactly this colour (0xAARRGGBB).
    Custom(u32),
    Clear,
}

impl Color {
    pub fn argb(self) -> u32 {
        let t = theme::current();
        let dark = t.dark;
        let pick = |light: u32, dark_c: u32| if dark { dark_c } else { light };
        // The text colour at an opacity: fills and hairlines.
        let ink = |light_a: u8, dark_a: u8| with_alpha(t.text, if dark { dark_a } else { light_a });
        match self {
            Color::Label => t.text,
            Color::SecondaryLabel => mix(t.window_bg, t.text, if dark { 174 } else { 169 }),
            Color::TertiaryLabel => mix(t.window_bg, t.text, if dark { 133 } else { 158 }),
            Color::Accent => theme::accent(),
            Color::AccentText => theme::accent_ink(),
            Color::OnAccent => rgb(0xFF, 0xFF, 0xFF),
            Color::Red => pick(rgb(0xD9, 0x3A, 0x3A), rgb(0xFF, 0x6B, 0x6B)),
            Color::Orange => pick(rgb(0xB8, 0x6E, 0x00), rgb(0xFF, 0xB5, 0x47)),
            Color::Yellow => pick(rgb(0xC9, 0x9A, 0x00), rgb(0xFF, 0xD6, 0x3C)),
            Color::Green => pick(rgb(0x12, 0x8A, 0x5A), rgb(0x3D, 0xDC, 0x97)),
            Color::Teal => pick(rgb(0x0A, 0x9C, 0x8E), rgb(0x3D, 0xE0, 0xC8)),
            Color::Blue => pick(rgb(0x1F, 0x6F, 0xEB), rgb(0x4A, 0x9B, 0xFF)),
            Color::Purple => pick(rgb(0x7C, 0x5C, 0xFF), rgb(0xA9, 0x91, 0xFF)),
            Color::Pink => pick(rgb(0xE0, 0x3E, 0x7A), rgb(0xFF, 0x6E, 0xA3)),
            Color::Gray => pick(rgb(0x8A, 0x8E, 0x9C), rgb(0x88, 0x92, 0xB0)),
            Color::Background => t.window_bg,
            Color::GroupedBackground => t.window_bg_alt,
            Color::SecondaryBackground => mix(t.window_bg, t.text, if dark { 15 } else { 11 }),
            Color::ControlBackground => ink(11, 15),
            Color::Separator => ink(20, 20),
            Color::Fill => ink(11, 15),
            Color::SecondaryFill => ink(19, 26),
            Color::TertiaryFill => ink(31, 41),
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
