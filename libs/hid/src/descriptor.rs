//! HID report descriptors (HID 1.11, section 6.2.2).
//!
//! A descriptor is a stream of items. *Global* items (usage page, logical
//! range, report size and count, report id) persist until changed; *local*
//! items (usages) apply to the next *main* item, which declares a field
//! in an input, output or feature report, or opens or closes a collection.
//! We keep one [`Field`] per input/output/feature main item: where its
//! entries sit in the report, their range, and which usage each one has.
//!
//! Descriptors come from devices, so nothing in one is trusted: every
//! length, count and size is bounded, and a malformed descriptor is an
//! error, not a panic.

use crate::usage;
use alloc::vec::Vec;

/// Which report a field belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Input,
    Output,
    Feature,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// An item runs past the end of the descriptor.
    Truncated,
    /// More fields, entries or nesting than any real device has.
    TooLarge,
    /// Inconsistent: e.g. an End Collection with no collection open.
    Malformed,
}

/// Bits of a main item's data.
pub mod flags {
    pub const CONSTANT: u32 = 1 << 0;
    pub const VARIABLE: u32 = 1 << 1;
    pub const RELATIVE: u32 = 1 << 2;
    pub const NULL_STATE: u32 = 1 << 6;
}

/// One input, output or feature item: `count` entries of `size` bits.
#[derive(Clone, Debug)]
pub struct Field {
    pub kind: Kind,
    /// 0 when the device doesn't number its reports.
    pub report_id: u8,
    /// Bit offset of the first entry, after the report id byte (if any).
    pub offset: u32,
    pub size: u32,
    pub count: u32,
    pub flags: u32,
    pub logical_min: i32,
    pub logical_max: i32,
    pub physical_min: i32,
    pub physical_max: i32,
    pub unit: u32,
    pub unit_exponent: i32,
    /// Usages as inclusive ranges of extended usages (a single usage is a
    /// range of one). Variable fields take them in order, one per entry
    /// (the last repeats); array entries hold an index into them.
    usages: Vec<(u32, u32)>,
    /// The outermost application collection's usage (Keyboard, Mouse, …).
    pub application: u32,
    /// The innermost enclosing collection: a number unique in the
    /// descriptor, and its usage (a touchpad's fingers are each a Finger
    /// collection).
    pub collection: u32,
    pub collection_usage: u32,
}

impl Field {
    pub fn is_constant(&self) -> bool {
        self.flags & flags::CONSTANT != 0
    }
    pub fn is_variable(&self) -> bool {
        self.flags & flags::VARIABLE != 0
    }
    pub fn is_array(&self) -> bool {
        !self.is_variable()
    }
    pub fn is_relative(&self) -> bool {
        self.flags & flags::RELATIVE != 0
    }

    /// The `n`th usage of the field's list (0-based), if there is one.
    fn nth_usage(&self, mut n: u32) -> Option<u32> {
        for &(first, last) in &self.usages {
            let len = last - first + 1;
            if n < len {
                return Some(first + n);
            }
            n -= len;
        }
        None
    }

    fn usage_count(&self) -> u32 {
        self.usages.iter().map(|&(a, b)| b - a + 1).fold(0u32, |s, n| s.saturating_add(n))
    }

    /// The usage of variable entry `i` (the last usage repeats).
    pub fn usage(&self, i: u32) -> Option<u32> {
        let n = self.usage_count();
        if n == 0 {
            return None;
        }
        self.nth_usage(i.min(n - 1))
    }

    /// Whether any entry of this field has usage `u`.
    pub fn has_usage(&self, u: u32) -> bool {
        self.usages.iter().any(|&(a, b)| (a..=b).contains(&u))
    }

    /// The entry with usage `u` (variable fields).
    pub fn entry_of(&self, u: u32) -> Option<u32> {
        (0..self.count).find(|&i| self.usage(i) == Some(u))
    }

    /// The usage an array entry's value stands for (`None`: nothing pressed
    /// or out of range).
    pub fn array_usage(&self, value: u32) -> Option<u32> {
        let v = value as i64 - self.logical_min as i64;
        if v < 0 || value as i64 > self.logical_max as i64 {
            return None;
        }
        self.nth_usage(v as u32)
    }

    /// The raw bits of entry `i` in a report's data.
    pub fn raw(&self, data: &[u8], i: u32) -> u32 {
        bits(data, self.offset + i * self.size, self.size)
    }

    /// Entry `i` as a number: sign-extended when the logical range is.
    pub fn value(&self, data: &[u8], i: u32) -> i32 {
        let raw = self.raw(data, i);
        if self.logical_min < 0 && self.size > 0 && self.size < 32 && raw & (1 << (self.size - 1)) != 0 {
            (raw | !((1u32 << self.size) - 1)) as i32
        } else {
            raw as i32
        }
    }

    /// Writes entry `i` into a report being built.
    pub fn set(&self, data: &mut [u8], i: u32, value: u32) {
        let start = self.offset + i * self.size;
        for b in 0..self.size.min(32) {
            let bit = start + b;
            let Some(byte) = data.get_mut((bit / 8) as usize) else { return };
            if value >> b & 1 != 0 {
                *byte |= 1 << (bit % 8);
            } else {
                *byte &= !(1 << (bit % 8));
            }
        }
    }
}

/// `size` bits (at most 32) starting at bit `offset`, little-endian.
pub fn bits(data: &[u8], offset: u32, size: u32) -> u32 {
    let mut v = 0u32;
    for b in 0..size.min(32) {
        let bit = offset + b;
        let byte = data.get((bit / 8) as usize).copied().unwrap_or(0);
        v |= (((byte >> (bit % 8)) & 1) as u32) << b;
    }
    v
}

/// A parsed report descriptor.
#[derive(Clone, Debug, Default)]
pub struct Descriptor {
    pub fields: Vec<Field>,
    /// Whether reports start with a report id byte.
    pub numbered: bool,
}

const MAX_FIELDS: usize = 1024;
const MAX_COUNT: u32 = 4096;
const MAX_DEPTH: usize = 16;
const MAX_USAGES: usize = 1024;

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    physical_min: i32,
    physical_max: i32,
    unit: u32,
    unit_exponent: i32,
    size: u32,
    count: u32,
    report_id: u8,
}

/// A local usage as declared: with its own page (4-byte form), or awaiting
/// the page in effect at the main item.
#[derive(Clone, Copy)]
enum Local {
    Usage(u32, bool),
    Min(u32, bool),
    Max(u32, bool),
}

impl Descriptor {
    pub fn parse(desc: &[u8]) -> Result<Descriptor, Error> {
        let mut out = Descriptor::default();
        let mut g = Globals::default();
        let mut stack: Vec<Globals> = Vec::new();
        let mut locals: Vec<Local> = Vec::new();
        // Open collections: (id, usage).
        let mut collections: Vec<(u32, u32)> = Vec::new();
        let mut application = 0u32;
        let mut next_collection = 1u32;
        // Bit offsets so far, per (kind, report id).
        let mut offsets: Vec<(Kind, u8, u32)> = Vec::new();
        let mut i = 0usize;
        while i < desc.len() {
            let prefix = desc[i];
            if prefix == 0xFE {
                // Long item: no defined meaning; skip it.
                let len = *desc.get(i + 1).ok_or(Error::Truncated)? as usize;
                i += 3 + len;
                continue;
            }
            let size = [0usize, 1, 2, 4][(prefix & 3) as usize];
            let data = desc.get(i + 1..i + 1 + size).ok_or(Error::Truncated)?;
            i += 1 + size;
            let mut unsigned = 0u32;
            for (k, b) in data.iter().enumerate() {
                unsigned |= (*b as u32) << (8 * k);
            }
            let signed = match size {
                1 => unsigned as u8 as i8 as i32,
                2 => unsigned as u16 as i16 as i32,
                _ => unsigned as i32,
            };
            let (kind, tag) = ((prefix >> 2) & 3, prefix >> 4);
            match (kind, tag) {
                // ----- main items
                (0, 0x8) | (0, 0x9) | (0, 0xB) => {
                    let field_kind = match tag {
                        0x8 => Kind::Input,
                        0x9 => Kind::Output,
                        _ => Kind::Feature,
                    };
                    if g.count > MAX_COUNT || g.size > 32 && unsigned & flags::CONSTANT == 0 {
                        return Err(Error::TooLarge);
                    }
                    let id = g.report_id;
                    let slot = match offsets.iter().position(|o| o.0 == field_kind && o.1 == id) {
                        Some(s) => s,
                        None => {
                            offsets.push((field_kind, id, 0));
                            offsets.len() - 1
                        }
                    };
                    let offset = offsets[slot].2;
                    let total = g.size.saturating_mul(g.count);
                    offsets[slot].2 = offset.checked_add(total).filter(|&o| o <= 8 * 4096).ok_or(Error::TooLarge)?;
                    if out.fields.len() >= MAX_FIELDS {
                        return Err(Error::TooLarge);
                    }
                    let mut logical_max = g.logical_max;
                    // A non-negative minimum means the maximum was meant unsigned.
                    if g.logical_min >= 0 && logical_max < 0 && g.size < 32 {
                        logical_max = (logical_max as u32 & ((1u64 << g.size.max(1)) - 1) as u32) as i32;
                    }
                    let (collection, collection_usage) = collections.last().copied().unwrap_or((0, 0));
                    out.fields.push(Field {
                        kind: field_kind,
                        report_id: id,
                        offset,
                        size: g.size,
                        count: g.count,
                        flags: unsigned,
                        logical_min: g.logical_min,
                        logical_max,
                        physical_min: g.physical_min,
                        physical_max: g.physical_max,
                        unit: g.unit,
                        unit_exponent: g.unit_exponent,
                        usages: resolve(&locals, g.page),
                        application,
                        collection,
                        collection_usage,
                    });
                    locals.clear();
                }
                (0, 0xA) => {
                    if collections.len() >= MAX_DEPTH {
                        return Err(Error::TooLarge);
                    }
                    let usage = resolve(&locals, g.page).first().map(|r| r.0).unwrap_or(0);
                    if collections.is_empty() || unsigned == 1 && application == 0 {
                        application = usage;
                    }
                    collections.push((next_collection, usage));
                    next_collection += 1;
                    locals.clear();
                }
                (0, 0xC) => {
                    collections.pop().ok_or(Error::Malformed)?;
                    if collections.is_empty() {
                        application = 0;
                    }
                    locals.clear();
                }
                (0, _) => locals.clear(),
                // ----- global items
                (1, 0x0) => g.page = unsigned as u16,
                (1, 0x1) => g.logical_min = signed,
                (1, 0x2) => g.logical_max = signed,
                (1, 0x3) => g.physical_min = signed,
                (1, 0x4) => g.physical_max = signed,
                (1, 0x5) => {
                    // A 4-bit two's complement exponent.
                    g.unit_exponent = if unsigned & 0x8 != 0 { (unsigned | !0xF) as i32 } else { unsigned as i32 & 0xF }
                }
                (1, 0x6) => g.unit = unsigned,
                (1, 0x7) => g.size = unsigned,
                (1, 0x8) => {
                    if unsigned == 0 || unsigned > 255 {
                        return Err(Error::Malformed);
                    }
                    g.report_id = unsigned as u8;
                    out.numbered = true;
                }
                (1, 0x9) => g.count = unsigned,
                (1, 0xA) => {
                    if stack.len() >= MAX_DEPTH {
                        return Err(Error::TooLarge);
                    }
                    stack.push(g);
                }
                (1, 0xB) => g = stack.pop().ok_or(Error::Malformed)?,
                // ----- local items
                (2, 0x0..=0x2) => {
                    if locals.len() >= MAX_USAGES {
                        return Err(Error::TooLarge);
                    }
                    let full = size == 4;
                    locals.push(match tag {
                        0x0 => Local::Usage(unsigned, full),
                        0x1 => Local::Min(unsigned, full),
                        _ => Local::Max(unsigned, full),
                    });
                }
                _ => {}
            }
        }
        if !collections.is_empty() {
            // Some devices omit the last End Collection; the fields are still good.
            collections.clear();
        }
        Ok(out)
    }

    /// The application collections the device has (Keyboard, Mouse, …).
    pub fn applications(&self) -> Vec<u32> {
        let mut apps: Vec<u32> = Vec::new();
        for f in &self.fields {
            if f.application != 0 && !apps.contains(&f.application) {
                apps.push(f.application);
            }
        }
        apps
    }

    pub fn has_application(&self, app: u32) -> bool {
        self.fields.iter().any(|f| f.application == app)
    }

    /// Report ids used by the fields of `kind` (`0` when unnumbered).
    pub fn report_ids(&self, kind: Kind) -> Vec<u8> {
        let mut ids: Vec<u8> = Vec::new();
        for f in self.fields.iter().filter(|f| f.kind == kind) {
            if !ids.contains(&f.report_id) {
                ids.push(f.report_id);
            }
        }
        ids
    }

    /// Bytes of report `id` of `kind`, without the id byte.
    pub fn report_len(&self, kind: Kind, id: u8) -> usize {
        let bits = self
            .fields
            .iter()
            .filter(|f| f.kind == kind && f.report_id == id)
            .map(|f| f.offset + f.size * f.count)
            .max()
            .unwrap_or(0);
        bits.div_ceil(8) as usize
    }

    /// Splits a received input report into its id and data.
    pub fn split<'a>(&self, report: &'a [u8]) -> Option<(u8, &'a [u8])> {
        if self.numbered {
            let (&id, data) = report.split_first()?;
            Some((id, data))
        } else {
            Some((0, report))
        }
    }

    /// Fields of report `id` of `kind`.
    pub fn fields_of(&self, kind: Kind, id: u8) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(move |f| f.kind == kind && f.report_id == id)
    }

    /// The first field of `kind` with usage `u`, and the entry that has it.
    pub fn find(&self, kind: Kind, u: u32) -> Option<(&Field, u32)> {
        self.fields.iter().filter(|f| f.kind == kind && !f.is_constant()).find_map(|f| {
            if f.is_variable() {
                f.entry_of(u).map(|e| (f, e))
            } else {
                f.has_usage(u).then_some((f, 0))
            }
        })
    }

    /// A zeroed report `id` of `kind`, with its id byte when numbered.
    pub fn new_report(&self, kind: Kind, id: u8) -> Vec<u8> {
        let mut r = alloc::vec![0u8; self.report_len(kind, id) + self.numbered as usize];
        if self.numbered {
            r[0] = id;
        }
        r
    }
}

/// Turns a main item's local usages into ranges of extended usages, with
/// `page` for the ones declared without their own.
fn resolve(locals: &[Local], page: u16) -> Vec<(u32, u32)> {
    let full = |v: u32, own: bool| if own { v } else { usage::u(page, v as u16) };
    let mut out = Vec::new();
    let mut min: Option<u32> = None;
    for l in locals {
        match *l {
            Local::Usage(v, own) => out.push((full(v, own), full(v, own))),
            Local::Min(v, own) => min = Some(full(v, own)),
            Local::Max(v, own) => {
                if let Some(m) = min.take() {
                    let max = full(v, own);
                    if max >= m {
                        out.push((m, max));
                    }
                }
            }
        }
    }
    out
}
