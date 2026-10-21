//! Values: the seven JavaScript types, strings as UTF-16, and property keys.

use crate::heap::ObjRef;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{OnceCell, RefCell};
use core::fmt;

/// A JavaScript string: immutable UTF-16 code units, cheap to clone.
/// Long concatenations are ropes, flattened once when first read, so
/// building a string piece by piece (`s += x`) takes linear time.
#[derive(Clone)]
pub struct JsStr(Repr);

#[derive(Clone)]
enum Repr {
    Flat(Rc<[u16]>),
    Rope(Rc<Rope>),
}

struct Rope {
    len: usize,
    parts: RefCell<Option<(JsStr, JsStr)>>,
    flat: OnceCell<Rc<[u16]>>,
}

/// Concatenations shorter than this are copied at once.
const ROPE_MIN: usize = 256;

impl Rope {
    fn flatten(&self) -> Rc<[u16]> {
        let mut out = Vec::with_capacity(self.len);
        let mut stack: Vec<JsStr> = Vec::new();
        if let Some((l, r)) = self.parts.borrow().as_ref() {
            stack.push(r.clone());
            stack.push(l.clone());
        }
        while let Some(s) = stack.pop() {
            match &s.0 {
                Repr::Flat(u) => out.extend_from_slice(u),
                Repr::Rope(n) => match n.flat.get() {
                    Some(u) => out.extend_from_slice(u),
                    None => {
                        let p = n.parts.borrow();
                        let (l, r) = p.as_ref().expect("rope without parts");
                        stack.push(r.clone());
                        stack.push(l.clone());
                    }
                },
            }
        }
        Rc::from(out)
    }
}

impl Drop for Rope {
    // Unlinks chains iteratively: a string built from thousands of `+=`
    // would otherwise be dropped by deep recursion.
    fn drop(&mut self) {
        let mut pending: Vec<JsStr> = Vec::new();
        if let Some((l, r)) = self.parts.get_mut().take() {
            pending.push(l);
            pending.push(r);
        }
        while let Some(s) = pending.pop() {
            if let Repr::Rope(rc) = s.0 {
                if let Ok(mut node) = Rc::try_unwrap(rc) {
                    if let Some((l, r)) = node.parts.get_mut().take() {
                        pending.push(l);
                        pending.push(r);
                    }
                }
            }
        }
    }
}

impl PartialEq for JsStr {
    fn eq(&self, other: &JsStr) -> bool {
        self.len() == other.len() && self.units() == other.units()
    }
}

impl Eq for JsStr {}

impl PartialOrd for JsStr {
    fn partial_cmp(&self, other: &JsStr) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for JsStr {
    fn cmp(&self, other: &JsStr) -> core::cmp::Ordering {
        self.units().cmp(other.units())
    }
}

impl core::hash::Hash for JsStr {
    fn hash<H: core::hash::Hasher>(&self, h: &mut H) {
        self.units().hash(h)
    }
}

impl JsStr {
    pub fn empty() -> JsStr {
        JsStr(Repr::Flat(Rc::from(&[][..])))
    }

    pub fn from_units(units: Vec<u16>) -> JsStr {
        JsStr(Repr::Flat(Rc::from(units)))
    }

    pub fn units(&self) -> &[u16] {
        match &self.0 {
            Repr::Flat(u) => u,
            Repr::Rope(r) => {
                let flat = r.flat.get_or_init(|| r.flatten());
                // The parts are no longer needed once flat.
                if let Ok(mut p) = r.parts.try_borrow_mut() {
                    if p.is_some() {
                        let parts = p.take();
                        drop(p);
                        drop(parts);
                    }
                }
                flat
            }
        }
    }

    pub fn len(&self) -> usize {
        match &self.0 {
            Repr::Flat(u) => u.len(),
            Repr::Rope(r) => r.len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn concat(&self, other: &JsStr) -> JsStr {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let len = self.len() + other.len();
        if len < ROPE_MIN {
            let mut v = Vec::with_capacity(len);
            v.extend_from_slice(self.units());
            v.extend_from_slice(other.units());
            return JsStr::from_units(v);
        }
        JsStr(Repr::Rope(Rc::new(Rope {
            len,
            parts: RefCell::new(Some((self.clone(), other.clone()))),
            flat: OnceCell::new(),
        })))
    }

    pub fn slice(&self, from: usize, to: usize) -> JsStr {
        let to = to.min(self.len());
        let from = from.min(to);
        if from == 0 && to == self.len() {
            return self.clone();
        }
        JsStr::from_units(self.units()[from..to].to_vec())
    }

    /// Lossy conversion: lone surrogates become U+FFFD.
    pub fn to_rust(&self) -> String {
        char::decode_utf16(self.units().iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    }

    pub fn eq_str(&self, s: &str) -> bool {
        let mut it = self.units().iter().copied();
        for u in s.encode_utf16() {
            if it.next() != Some(u) {
                return false;
            }
        }
        it.next().is_none()
    }

    pub fn find(&self, needle: &JsStr, from: usize) -> Option<usize> {
        let (h, n) = (self.units(), needle.units());
        if n.is_empty() {
            return (from <= h.len()).then_some(from);
        }
        if n.len() > h.len() {
            return None;
        }
        (from..=h.len() - n.len()).find(|&i| &h[i..i + n.len()] == n)
    }

    pub fn rfind(&self, needle: &JsStr, from: usize) -> Option<usize> {
        let (h, n) = (self.units(), needle.units());
        if n.len() > h.len() {
            return None;
        }
        let start = from.min(h.len() - n.len());
        (0..=start).rev().find(|&i| &h[i..i + n.len()] == n)
    }

    /// The array index this string names, if it is one (canonical, < 2³² − 1).
    pub fn as_index(&self) -> Option<u32> {
        if self.len() > 10 {
            return None;
        }
        let u = self.units();
        if u.is_empty() || (u.len() > 1 && u[0] == b'0' as u16) {
            return None;
        }
        let mut n: u64 = 0;
        for &c in u {
            if !(b'0' as u16..=b'9' as u16).contains(&c) {
                return None;
            }
            n = n * 10 + (c - b'0' as u16) as u64;
        }
        (n < u32::MAX as u64).then_some(n as u32)
    }
}

impl From<&str> for JsStr {
    fn from(s: &str) -> JsStr {
        JsStr::from_units(s.encode_utf16().collect())
    }
}

impl From<String> for JsStr {
    fn from(s: String) -> JsStr {
        JsStr::from(s.as_str())
    }
}

impl fmt::Display for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.to_rust())
    }
}

impl fmt::Debug for JsStr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{:?}", self.to_rust())
    }
}

/// A symbol: an index into the realm's symbol table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Sym(pub u32);

impl Sym {
    pub const ITERATOR: Sym = Sym(0);
    pub const ASYNC_ITERATOR: Sym = Sym(1);
    pub const HAS_INSTANCE: Sym = Sym(2);
    pub const TO_PRIMITIVE: Sym = Sym(3);
    pub const TO_STRING_TAG: Sym = Sym(4);
    pub const SPECIES: Sym = Sym(5);
    pub const IS_CONCAT_SPREADABLE: Sym = Sym(6);
    pub const UNSCOPABLES: Sym = Sym(7);
    pub const MATCH: Sym = Sym(8);
    pub const MATCH_ALL: Sym = Sym(9);
    pub const REPLACE: Sym = Sym(10);
    pub const SEARCH: Sym = Sym(11);
    pub const SPLIT: Sym = Sym(12);
    pub const WELL_KNOWN: [&'static str; 13] = [
        "iterator",
        "asyncIterator",
        "hasInstance",
        "toPrimitive",
        "toStringTag",
        "species",
        "isConcatSpreadable",
        "unscopables",
        "match",
        "matchAll",
        "replace",
        "search",
        "split",
    ];
}

#[derive(Clone, PartialEq)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Number(f64),
    String(JsStr),
    Symbol(Sym),
    Object(ObjRef),
    /// Internal: an uninitialised binding (TDZ) or an array hole. Never
    /// visible to scripts.
    Empty,
}

impl Value {
    pub fn str(s: &str) -> Value {
        Value::String(JsStr::from(s))
    }

    pub fn is_nullish(&self) -> bool {
        matches!(self, Value::Undefined | Value::Null)
    }

    pub fn is_undefined(&self) -> bool {
        matches!(self, Value::Undefined)
    }

    pub fn as_object(&self) -> Option<ObjRef> {
        match self {
            Value::Object(o) => Some(*o),
            _ => None,
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Undefined | Value::Null | Value::Empty => false,
            Value::Bool(b) => *b,
            Value::Number(n) => !(*n == 0.0 || n.is_nan()),
            Value::String(s) => !s.is_empty(),
            Value::Symbol(_) | Value::Object(_) => true,
        }
    }

    /// Holes and TDZ markers read as `undefined`.
    pub fn or_undefined(self) -> Value {
        match self {
            Value::Empty => Value::Undefined,
            v => v,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Value::Undefined => f.write_str("undefined"),
            Value::Null => f.write_str("null"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::Number(n) => f.write_str(&crate::numconv::to_string(*n)),
            Value::String(s) => write!(f, "{s:?}"),
            Value::Symbol(s) => write!(f, "Symbol({})", s.0),
            Value::Object(o) => write!(f, "[object #{}]", o.0),
            Value::Empty => f.write_str("<empty>"),
        }
    }
}

impl From<f64> for Value {
    fn from(n: f64) -> Value {
        Value::Number(n)
    }
}

impl From<bool> for Value {
    fn from(b: bool) -> Value {
        Value::Bool(b)
    }
}

impl From<JsStr> for Value {
    fn from(s: JsStr) -> Value {
        Value::String(s)
    }
}

impl From<ObjRef> for Value {
    fn from(o: ObjRef) -> Value {
        Value::Object(o)
    }
}

/// A property key.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum PropKey {
    Str(JsStr),
    Sym(Sym),
}

impl PropKey {
    pub fn index(n: u32) -> PropKey {
        PropKey::Str(JsStr::from(alloc::format!("{n}")))
    }

    pub fn as_index(&self) -> Option<u32> {
        match self {
            PropKey::Str(s) => s.as_index(),
            PropKey::Sym(_) => None,
        }
    }

    pub fn is_str(&self, s: &str) -> bool {
        matches!(self, PropKey::Str(k) if k.eq_str(s))
    }

    pub fn to_value(&self) -> Value {
        match self {
            PropKey::Str(s) => Value::String(s.clone()),
            PropKey::Sym(s) => Value::Symbol(*s),
        }
    }
}

impl Default for PropKey {
    fn default() -> PropKey {
        PropKey::Str(JsStr::empty())
    }
}

impl From<&str> for PropKey {
    fn from(s: &str) -> PropKey {
        PropKey::Str(JsStr::from(s))
    }
}

impl From<JsStr> for PropKey {
    fn from(s: JsStr) -> PropKey {
        PropKey::Str(s)
    }
}

impl From<Sym> for PropKey {
    fn from(s: Sym) -> PropKey {
        PropKey::Sym(s)
    }
}

/// UTF-16 helpers.
pub fn is_js_whitespace(c: u16) -> bool {
    matches!(
        c,
        0x09 | 0x0A | 0x0B | 0x0C | 0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000
            ..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F | 0x3000 | 0xFEFF
    )
}

pub fn trim_units(u: &[u16], start: bool, end: bool) -> &[u16] {
    let mut a = 0;
    let mut b = u.len();
    if start {
        while a < b && is_js_whitespace(u[a]) {
            a += 1;
        }
    }
    if end {
        while b > a && is_js_whitespace(u[b - 1]) {
            b -= 1;
        }
    }
    &u[a..b]
}

#[cfg(test)]
mod rope_tests {
    use super::*;

    #[test]
    fn values_stay_small() {
        assert!(core::mem::size_of::<Value>() <= 24, "{}", core::mem::size_of::<Value>());
    }

    #[test]
    fn ropes_read_like_flat_strings_and_drop_deep_chains() {
        let mut s = JsStr::from("start:");
        let piece = JsStr::from("ab");
        for _ in 0..200_000 {
            s = s.concat(&piece);
        }
        assert_eq!(s.len(), 6 + 400_000);
        assert_eq!(&s.units()[..8], &"start:ab".encode_utf16().collect::<Vec<_>>()[..]);
        let mut t = JsStr::from("start:");
        for _ in 0..200_000 {
            t = t.concat(&piece);
        }
        assert!(s == t);
        // A chain dropped without ever being read.
        let mut u = JsStr::empty();
        for _ in 0..300_000 {
            u = u.concat(&piece);
        }
        drop(u);
    }
}
