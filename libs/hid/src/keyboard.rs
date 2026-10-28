//! Keyboards: which keys are held, from any report format.
//!
//! Boot keyboards send six key slots (an *array*: each slot holds the usage
//! of a held key); "N-key rollover" keyboards send one bit per key (a
//! *bitmap* of variable entries); media keys come from Consumer Control and
//! power keys from System Control, usually in reports of their own. All of
//! them reduce to the set of usages held in each report, and [`Keys`]
//! turns successive sets into presses and releases.

use crate::descriptor::{Descriptor, Field, Kind};
use crate::usage;
use alloc::vec::Vec;

/// Whether a field's entries are keys (rather than LEDs, axes, …).
fn is_key_field(f: &Field) -> bool {
    if f.kind != Kind::Input || f.is_constant() {
        return false;
    }
    match f.application {
        usage::KEYBOARD | usage::KEYPAD | usage::CONSUMER_CONTROL | usage::SYSTEM_CONTROL => {}
        _ => return false,
    }
    // Keys are one-bit variables or arrays of usages, on the key-like pages.
    let key_page = |u: u32| {
        matches!(usage::page(u), usage::KEYBOARD_PAGE | usage::CONSUMER_PAGE)
            || (usage::page(u) == usage::GENERIC_DESKTOP && (0x81..=0x83).contains(&usage::id(u)))
    };
    if f.is_variable() {
        f.size == 1 && (0..f.count).any(|i| f.usage(i).is_some_and(key_page))
    } else {
        f.usage(0).is_some_and(key_page) || f.usage(1).is_some_and(key_page)
    }
}

/// The usages held according to one report (`None` when the keyboard says
/// too many keys are down to tell — "phantom state").
pub fn held(d: &Descriptor, report: &[u8]) -> Option<(u8, Vec<u32>)> {
    let (id, data) = d.split(report)?;
    let mut keys = Vec::new();
    let mut any = false;
    for f in d.fields_of(Kind::Input, id).filter(|f| is_key_field(f)) {
        any = true;
        for i in 0..f.count {
            if f.is_variable() {
                if f.raw(data, i) != 0 {
                    if let Some(u) = f.usage(i) {
                        keys.push(u);
                    }
                }
            } else {
                let v = f.raw(data, i);
                match f.array_usage(v) {
                    Some(usage::KEY_ERROR_ROLLOVER) => return None,
                    // Usage 0 on any page means "nothing".
                    Some(u) if usage::id(u) != 0 => keys.push(u),
                    _ => {}
                }
            }
        }
    }
    any.then_some((id, keys))
}

/// A key going down or up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Change {
    pub usage: u32,
    pub pressed: bool,
}

/// What a keyboard held after each report id it sent, to see what changed.
#[derive(Default)]
pub struct Keys {
    held: Vec<(u8, Vec<u32>)>,
}

impl Keys {
    pub fn new() -> Keys {
        Keys::default()
    }

    /// Feeds an input report; returns the keys that went up, then down.
    pub fn feed(&mut self, d: &Descriptor, report: &[u8]) -> Vec<Change> {
        let Some((id, now)) = held(d, report) else { return Vec::new() };
        let slot = match self.held.iter().position(|h| h.0 == id) {
            Some(s) => s,
            None => {
                self.held.push((id, Vec::new()));
                self.held.len() - 1
            }
        };
        let before = core::mem::replace(&mut self.held[slot].1, now);
        let now = &self.held[slot].1;
        let mut out: Vec<Change> =
            before.iter().filter(|u| !now.contains(u)).map(|&usage| Change { usage, pressed: false }).collect();
        out.extend(now.iter().filter(|u| !before.contains(u)).map(|&usage| Change { usage, pressed: true }));
        out
    }

    /// Everything held (all report ids), e.g. to release it all on unplug.
    pub fn all_held(&self) -> Vec<u32> {
        self.held.iter().flat_map(|h| h.1.iter().copied()).collect()
    }
}

/// The output report that sets a keyboard's lights, if it has any.
pub fn led_report(d: &Descriptor, num: bool, caps: bool, scroll: bool) -> Option<Vec<u8>> {
    let mut built: Option<(u8, Vec<u8>)> = None;
    for (led, on) in [(usage::LED_NUM_LOCK, num), (usage::LED_CAPS_LOCK, caps), (usage::LED_SCROLL_LOCK, scroll)] {
        let Some((f, entry)) = d.find(Kind::Output, led) else { continue };
        if !f.is_variable() {
            continue;
        }
        let report = match &mut built {
            Some((id, r)) if *id == f.report_id => r,
            Some(_) => continue, // LEDs split across reports: keep the first
            None => {
                built = Some((f.report_id, d.new_report(Kind::Output, f.report_id)));
                &mut built.as_mut().unwrap().1
            }
        };
        let data = if d.numbered { &mut report[1..] } else { &mut report[..] };
        f.set(data, entry, on as u32);
    }
    built.map(|b| b.1)
}
