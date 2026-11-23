//! The displays: how many there are, where they are, and which one a
//! window is on.
//!
//! Every display is a rectangle in one space of points with the main
//! display (the one with the menu bar) at (0, 0). A window learns that it
//! moved to another display, or that displays came or went, from an
//! `event::DISPLAY` (AuroraKit calls `App::display_changed`). Programs that
//! never ask keep working as before: they see the main display's size.
//!
//! ```ignore
//! for d in corekit::display::displays() {
//!     corekit::println!("{} {}x{} at ({}, {})", d.name(), d.width, d.height, d.x, d.y);
//! }
//! ```

use crate::abi::{desktop, nr, DisplayInfo, DisplayMode};
use crate::sys::call;
use alloc::vec::Vec;

/// Every display, the main one first.
pub fn displays() -> Vec<DisplayInfo> {
    let mut buf = [DisplayInfo::default(); 16];
    let n = call(nr::DESKTOP, &[desktop::DISPLAYS as u64, buf.as_mut_ptr() as u64, buf.len() as u64]).unwrap_or(0);
    buf[..(n as usize).min(buf.len())].to_vec()
}

/// The main display.
pub fn main_display() -> Option<DisplayInfo> {
    displays().into_iter().find(|d| d.is_primary())
}

/// The display with this id (as `event::DISPLAY` and [`window_display`] give it).
pub fn display(id: u32) -> Option<DisplayInfo> {
    displays().into_iter().find(|d| d.id == id)
}

/// The id of the display one of this program's windows is on (`None`
/// before the window first appears).
pub fn window_display(window: u64) -> Option<u32> {
    call(nr::DESKTOP, &[desktop::WINDOW_DISPLAY as u64, 0, window]).ok().map(|v| v as u32)
}

/// The resolutions a display can show, the current one flagged
/// (`abi::display::CURRENT`).
pub fn modes(id: u32) -> Vec<DisplayMode> {
    let mut buf = [DisplayMode::default(); 64];
    let arg = (id as u64) << 32 | buf.len() as u64;
    let n = call(nr::DESKTOP, &[desktop::DISPLAY_MODES as u64, buf.as_mut_ptr() as u64, arg]).unwrap_or(0);
    buf[..(n as usize).min(buf.len())].to_vec()
}
