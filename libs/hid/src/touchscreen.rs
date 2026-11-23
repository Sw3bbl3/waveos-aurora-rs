//! Touch screens (HID digitizers, Touch Screen application collection).
//!
//! A touch screen reports the same Finger contacts a Precision Touchpad
//! does, but at absolute positions on the display. WaveOS drives the pointer
//! with the first finger down: it moves to where the finger is and holds the
//! primary button while it touches, so tapping clicks and sliding drags.
//! Further fingers are ignored until that one lifts.

use crate::descriptor::Descriptor;
use crate::touchpad::{self, Frames, Info, Report};
use crate::usage;

pub fn is_touchscreen(d: &Descriptor) -> bool {
    touchpad::has_contacts(d, usage::TOUCH_SCREEN)
}

pub fn info(d: &Descriptor) -> Option<Info> {
    touchpad::info_of(d, usage::TOUCH_SCREEN)
}

pub fn read(d: &Descriptor, report: &[u8]) -> Option<Report> {
    touchpad::read_of(d, report, usage::TOUCH_SCREEN)
}

/// Where the pointer goes: a position in the screen's logical range.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Touch {
    pub x: i32,
    pub y: i32,
    /// The finger is on the screen (the primary button is held).
    pub down: bool,
}

/// Follows the primary finger across reports.
#[derive(Default)]
pub struct Touches {
    frames: Frames,
    primary: Option<u32>,
    last: Option<(i32, i32)>,
}

impl Touches {
    pub fn new() -> Touches {
        Touches::default()
    }

    /// Feeds a report; returns the pointer's new state once a frame is
    /// complete and something changed.
    pub fn feed(&mut self, r: Report) -> Option<Touch> {
        let frame = self.frames.feed(r)?;
        let touching = |id: u32| frame.contacts.iter().find(|c| c.id == id && c.tip && c.confident);
        if let Some(id) = self.primary {
            if let Some(c) = touching(id) {
                self.last = Some((c.x, c.y));
                return Some(Touch { x: c.x, y: c.y, down: true });
            }
            // Lifted: let go where it was.
            self.primary = None;
            let (x, y) = self.last?;
            return Some(Touch { x, y, down: false });
        }
        let c = frame.contacts.iter().find(|c| c.tip && c.confident)?;
        self.primary = Some(c.id);
        self.last = Some((c.x, c.y));
        Some(Touch { x: c.x, y: c.y, down: true })
    }
}
