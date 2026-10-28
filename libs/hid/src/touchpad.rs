//! Multi-touch touchpads (Windows Precision Touchpad protocol).
//!
//! A Precision Touchpad's input report holds one Finger collection per
//! contact it can report at once (tip switch, contact id, X, Y), the
//! number of contacts touching now, and its button. Until told otherwise
//! it acts as a plain mouse: setting the Input Mode feature to 3 makes it
//! report contacts. A pad that tracks more fingers than fit in one report
//! ("hybrid" reporting) sends the first report with the full contact
//! count and the rest with a count of 0; [`Frames`] puts them together.

use crate::descriptor::{Descriptor, Field, Kind};
use crate::usage;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    pub id: u32,
    pub x: i32,
    pub y: i32,
    /// Touching (rather than lifting off).
    pub tip: bool,
    /// The pad thinks it is a finger, not a palm.
    pub confident: bool,
}

/// What one report holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub contacts: Vec<Contact>,
    /// Contacts in this whole frame (0 in a hybrid frame's later reports).
    pub count: Option<u32>,
    pub button: bool,
}

/// A whole frame: every contact the pad tracks at one instant.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub contacts: Vec<Contact>,
    pub button: bool,
}

/// The pad's geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Info {
    pub x_min: i32,
    pub x_max: i32,
    pub y_min: i32,
    pub y_max: i32,
    /// Physical size in hundredths of a millimetre, when the descriptor says.
    pub width_hmm: Option<u32>,
    pub height_hmm: Option<u32>,
    /// Contacts one report can carry.
    pub max_contacts: u32,
}

fn is_contact_field(f: &Field) -> bool {
    f.kind == Kind::Input && !f.is_constant() && f.application == usage::TOUCH_PAD
}

/// Whether the device is a Precision Touchpad.
pub fn is_touchpad(d: &Descriptor) -> bool {
    d.fields.iter().any(|f| is_contact_field(f) && f.collection_usage == usage::FINGER && f.has_usage(usage::X))
}

/// A length in the field's physical units, in hundredths of a millimetre.
fn physical_hmm(f: &Field) -> Option<u32> {
    let span = (f.physical_max as i64 - f.physical_min as i64).abs();
    if span == 0 {
        return None;
    }
    // Length units: 0x11 centimetres (SI linear), 0x13 inches (English linear).
    let per_unit_hmm: i64 = match f.unit & 0xFF {
        0x11 => 1000,
        0x13 => 2540,
        _ => return None,
    };
    // value × 10^exponent units; exponent usually −2 or −3.
    let mut v = span * per_unit_hmm;
    let mut e = f.unit_exponent;
    while e > 0 {
        v *= 10;
        e -= 1;
    }
    while e < 0 {
        v /= 10;
        e += 1;
    }
    u32::try_from(v).ok().filter(|&v| v > 0)
}

pub fn info(d: &Descriptor) -> Option<Info> {
    let field =
        |u: u32| d.fields.iter().find(|f| is_contact_field(f) && f.collection_usage == usage::FINGER && f.has_usage(u));
    let x = field(usage::X)?;
    let y = field(usage::Y)?;
    // Finger slots in one report (a pad may track more across hybrid reports).
    let max_contacts =
        d.fields.iter().filter(|f| is_contact_field(f) && f.has_usage(usage::CONTACT_ID)).count().max(1) as u32;
    Some(Info {
        x_min: x.logical_min,
        x_max: x.logical_max,
        y_min: y.logical_min,
        y_max: y.logical_max,
        width_hmm: physical_hmm(x),
        height_hmm: physical_hmm(y),
        max_contacts,
    })
}

/// Reads a touchpad report (`None`: the report is about something else).
pub fn read(d: &Descriptor, report: &[u8]) -> Option<Report> {
    let (id, data) = d.split(report)?;
    let mut out = Report::default();
    // Contacts by their Finger collection, in descriptor order.
    let mut fingers: Vec<(u32, Contact, bool)> = Vec::new();
    let mut any = false;
    for f in d.fields_of(Kind::Input, id).filter(|f| is_contact_field(f)) {
        any = true;
        for i in 0..f.count {
            let Some(u) = (if f.is_variable() { f.usage(i) } else { None }) else { continue };
            let v = f.value(data, i);
            if f.collection_usage == usage::FINGER {
                let slot = match fingers.iter().position(|c| c.0 == f.collection) {
                    Some(s) => s,
                    None => {
                        fingers.push((f.collection, Contact { confident: true, ..Default::default() }, false));
                        fingers.len() - 1
                    }
                };
                let c = &mut fingers[slot];
                match u {
                    usage::TIP_SWITCH => c.1.tip = v != 0,
                    usage::CONFIDENCE => c.1.confident = v != 0,
                    usage::CONTACT_ID => c.1.id = v as u32,
                    usage::X => {
                        c.1.x = v;
                        c.2 = true;
                    }
                    usage::Y => c.1.y = v,
                    _ => {}
                }
            } else {
                match u {
                    usage::CONTACT_COUNT => out.count = Some(v as u32),
                    u if u == usage::button(1) => out.button |= v != 0,
                    _ => {}
                }
            }
        }
    }
    if !any {
        return None;
    }
    out.contacts = fingers.into_iter().filter(|c| c.2).map(|c| c.1).collect();
    Some(out)
}

/// The feature report that switches the pad to reporting contacts (Input
/// Mode = 3), if it has one.
pub fn input_mode_report(d: &Descriptor) -> Option<Vec<u8>> {
    let (f, entry) = d.find(Kind::Feature, usage::INPUT_MODE)?;
    let mut r = d.new_report(Kind::Feature, f.report_id);
    let data = if d.numbered { &mut r[1..] } else { &mut r[..] };
    f.set(data, entry, 3);
    Some(r)
}

/// The feature report turning on both surface and button reporting, if
/// the pad has one (some pads start with them off).
pub fn selective_reporting_report(d: &Descriptor) -> Option<Vec<u8>> {
    let (surface, se) = d.find(Kind::Feature, usage::SURFACE_SWITCH)?;
    let mut r = d.new_report(Kind::Feature, surface.report_id);
    let numbered = d.numbered;
    {
        let data = if numbered { &mut r[1..] } else { &mut r[..] };
        surface.set(data, se, 1);
    }
    if let Some((button, be)) = d.find(Kind::Feature, usage::BUTTON_SWITCH) {
        if button.report_id == surface.report_id {
            let data = if numbered { &mut r[1..] } else { &mut r[..] };
            button.set(data, be, 1);
        }
    }
    Some(r)
}

/// Assembles frames from reports (hybrid reporting spreads one over several).
#[derive(Default)]
pub struct Frames {
    pending: Frame,
    expected: u32,
}

impl Frames {
    pub fn new() -> Frames {
        Frames::default()
    }

    /// Feeds a report; returns the frame once complete.
    pub fn feed(&mut self, r: Report) -> Option<Frame> {
        match r.count {
            // A new frame begins: its first report carries the count.
            Some(n) if n > 0 || self.expected == 0 => {
                self.pending = Frame { contacts: Vec::new(), button: r.button };
                self.expected = n;
            }
            Some(_) => {}
            // No contact count at all: every report is a whole frame.
            None => {
                self.pending = Frame { contacts: Vec::new(), button: r.button };
                self.expected = r.contacts.len() as u32;
            }
        }
        self.pending.button |= r.button;
        for c in r.contacts {
            if self.pending.contacts.len() < self.expected as usize {
                self.pending.contacts.push(c);
            }
        }
        if self.pending.contacts.len() >= self.expected as usize {
            self.expected = 0;
            return Some(core::mem::take(&mut self.pending));
        }
        None
    }
}
