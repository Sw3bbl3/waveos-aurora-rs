//! USB HID for WaveOS: report descriptors and what keyboards, mice,
//! touchpads and game controllers say in their reports.
//!
//! A HID device describes its reports in a *report descriptor*: which
//! controls (usages) it has, how many bits each takes and where it sits.
//! [`descriptor::Descriptor`] parses that once; the decoders then read any
//! report against it:
//!
//! - [`keyboard`]: keys held (any rollover), media and system keys, and the
//!   output report that lights Caps Lock;
//! - [`pointer`]: mice and absolute pointers — buttons, motion, wheel and
//!   horizontal wheel;
//! - [`touchpad`]: multi-touch (Precision Touchpad) contacts, and
//!   [`gestures`], which turns them into pointer motion, taps, clicks and
//!   two-finger scrolling;
//! - [`gamepad`]: HID game controllers and the Xbox protocols, in one
//!   standard layout.
//!
//! No hardware access happens here (the kernel's USB driver feeds it), so
//! all of it is tested on the host against real devices' descriptors.

#![no_std]

extern crate alloc;

pub mod descriptor;
pub mod gamepad;
pub mod gestures;
pub mod keyboard;
pub mod pointer;
pub mod touchpad;
pub mod usage;

pub use descriptor::{Descriptor, Field, Kind};
