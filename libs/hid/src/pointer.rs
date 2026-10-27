//! Mice and absolute pointers (tablets, virtual-machine pointers).

use crate::descriptor::{Descriptor, Field, Kind};
use crate::usage;

/// What one report says about the pointer. Relative devices report motion;
/// absolute ones a position within their logical range.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub absolute: bool,
    /// Motion (relative) or position (absolute), when the report has them.
    pub x: Option<i32>,
    pub y: Option<i32>,
    /// The position's range, for absolute devices.
    pub x_range: (i32, i32),
    pub y_range: (i32, i32),
    /// Buttons 1–8 as bits (1 left, 2 right, 4 middle, 8 back, 16 forward).
    pub buttons: u8,
    /// Wheel notches, positive away from the user.
    pub wheel: i32,
    /// Horizontal wheel (AC Pan), positive to the right.
    pub pan: i32,
}

fn is_pointer_field(f: &Field) -> bool {
    f.kind == Kind::Input
        && !f.is_constant()
        && matches!(f.application, usage::MOUSE | usage::POINTER)
        && f.collection_usage != usage::FINGER
}

/// Whether the device is a mouse or pointer at all.
pub fn is_pointer(d: &Descriptor) -> bool {
    d.find(Kind::Input, usage::X).is_some_and(|(f, _)| is_pointer_field(f))
}

/// Reads a pointer report (`None`: the report is about something else).
pub fn read(d: &Descriptor, report: &[u8]) -> Option<Report> {
    let (id, data) = d.split(report)?;
    let mut r = Report::default();
    let mut any = false;
    for f in d.fields_of(Kind::Input, id).filter(|f| is_pointer_field(f)) {
        for i in 0..f.count {
            let u = if f.is_variable() { f.usage(i) } else { f.array_usage(f.raw(data, i)) };
            let Some(u) = u else { continue };
            any = true;
            if usage::page(u) == usage::BUTTON_PAGE {
                let n = usage::id(u);
                let down = if f.is_variable() { f.raw(data, i) != 0 } else { true };
                if (1..=8).contains(&n) && down {
                    r.buttons |= 1 << (n - 1);
                }
                continue;
            }
            if !f.is_variable() {
                continue;
            }
            let v = f.value(data, i);
            match u {
                usage::X => {
                    r.x = Some(v);
                    r.absolute = !f.is_relative();
                    r.x_range = (f.logical_min, f.logical_max);
                }
                usage::Y => {
                    r.y = Some(v);
                    r.y_range = (f.logical_min, f.logical_max);
                }
                usage::WHEEL => r.wheel = v,
                usage::AC_PAN => r.pan = v,
                _ => {}
            }
        }
    }
    any.then_some(r)
}

/// Scales an absolute coordinate to `0..extent`.
pub fn scale(v: i32, range: (i32, i32), extent: i32) -> i32 {
    let span = (range.1 as i64 - range.0 as i64).max(1);
    ((v as i64 - range.0 as i64).clamp(0, span) * (extent as i64 - 1).max(0) / span) as i32
}
