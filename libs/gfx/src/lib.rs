//! Lumen graphics: the software rasterizer, fonts, design tokens, icons,
//! widgets and wallpapers shared by the Lumen Server window server (kernel) and the
//! AuroraKit toolkit (user space). Pure integer math; `no_std` + `alloc`.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod canvas;
pub mod font;
pub mod geom;
pub mod icons;
pub mod material;
pub mod math;
pub mod theme;
pub mod ttf;
/// Lyra font parsing and rasterization; `ttf` remains a compatibility alias.
pub use ttf as lyra;
pub mod ui;
pub mod wallpaper;
pub mod widgets;
