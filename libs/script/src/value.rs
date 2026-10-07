//! Values: the seven JavaScript types, strings as UTF-16, and property keys.

use crate::heap::ObjRef;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// A JavaScript string: immutable UTF-16 code units, cheap to clone.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct JsStr(Rc<[u16]>);

impl JsStr {
    pub fn empty() -> JsStr {
        JsStr(Rc::from(&[][..]))
    }

    pub fn from_units(units: Vec<u16>) -> JsStr {
        JsStr(Rc::from(units))
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn concat(&self, other: &JsStr) -> JsStr {
        if other.is_empty() {
            return self.clone();
        }
        if self.is_empty() {
            return other.clone();
        }
        let mut v = Vec::with_capacity(self.len() + other.len());
        v.extend_from_slice(&self.0);
        v.extend_from_slice(&other.0);
        JsStr::from_units(v)
    }

    pub fn slice(&self, from: usize, to: usize) -> JsStr {
        let to = to.min(self.len());
        let from = from.min(to);
        if from == 0 && to == self.len() {
            return self.clone();
        }
        JsStr::from_units(self.0[from..to].to_vec())
    }

    /// Lossy conversion: lone surrogates become U+FFFD.
    pub fn to_rust(&self) -> String {
        char::decode_utf16(self.0.iter().copied()).map(|c| c.unwrap_or('\u{FFFD}')).collect()
    }

    pub fn eq_str(&self, s: &str) -> bool {
        let mut it = self.0.iter().copied();
        for u in s.encode_utf16() {
            if it.next() != Some(u) {
                return false;
            }
        }
        it.next().is_none()
    }

    pub fn find(&self, needle: &JsStr, from: usize) -> Option<usize> {
        let (h, n) = (&self.0[..], &needle.0[..]);
        if n.is_empty() {
            return (from <= h.len()).then_some(from);
        }
        if n.len() > h.len() {
            return None;
        }
        (from..=h.len() - n.len()).find(|&i| &h[i..i + n.len()] == n)
    }

    pub fn rfind(&self, needle: &JsStr, from: usize) -> Option<usize> {
        let (h, n) = (&self.0[..], &needle.0[..]);
        if n.len() > h.len() {
            return None;
        }
        let start = from.min(h.len() - n.len());
        (0..=start).rev().find(|&i| &h[i..i + n.len()] == n)
    }

    /// The array index this string names, if it is one (canonical, < 2³² − 1).
    pub fn as_index(&self) -> Option<u32> {
        let u = &self.0[..];
        if u.is_empty() || u.len() > 10 || (u.len() > 1 && u[0] == b'0' as u16) {
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
