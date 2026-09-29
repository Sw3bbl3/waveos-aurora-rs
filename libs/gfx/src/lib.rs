//! Aurora graphics: the software rasterizer, fonts, design tokens, icons,
//! widgets and wallpapers shared by the Crest window server (kernel) and the
//! Ripple toolkit (user space). Pure integer math; `no_std` + `alloc`.

#![no_std]

extern crate alloc;

pub mod canvas;
pub mod font;
pub mod geom;
pub mod icons;
pub mod math;
pub mod theme;
pub mod wallpaper;
pub mod widgets;
