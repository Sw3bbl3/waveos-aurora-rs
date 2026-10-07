//! String, String.prototype and the string iterator.

use super::*;
use crate::lexer::push_code_point;
use crate::value::{trim_units, JsStr};
use alloc::string::String;

fn this_str(rt: &mut Realm, c: &Call, method: &str) -> Result<JsStr, Value> {
    match &c.this {
        Value::String(s) => Ok(s.clone()),
        Value::Undefined | Value::Null => {
            Err(rt.type_error(&alloc::format!("String.prototype.{method} called on null or undefined")))
        }
        v => rt.to_string(v),
    }
}

fn string_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let s = match c.args.first() {
        None => JsStr::empty(),
        Some(Value::Symbol(sym)) if !c.is_construct() => symbol::descriptive_string(rt, *sym),
        Some(v) => rt.to_string(v)?,
    };
    if !c.is_construct() {
        return Ok(Value::String(s));
    }
    let sp = rt.intr.string_proto;
    let proto = rt.proto_from_ctor(&c.new_target, sp)?;
    Ok(Value::Object(rt.alloc(Obj::new(Some(proto), Kind::Primitive(Value::String(s))))))
}

fn from_char_code(rt: &mut Realm, c: &Call) -> JsResult {
    let mut out = Vec::with_capacity(c.args.len());
    for a in &c.args {
        out.push(crate::numconv::to_uint32(rt.to_number(a)?) as u16);
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn from_code_point(rt: &mut Realm, c: &Call) -> JsResult {
    let mut out = Vec::new();
    for a in &c.args {
        let n = rt.to_number(a)?;
        if n != libm::trunc(n) || !(0.0..=1114111.0).contains(&n) {
            let s = crate::numconv::to_string(n);
            return Err(rt.range_error(&alloc::format!("Invalid code point {s}")));
        }
        push_code_point(&mut out, n as u32);
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn raw(rt: &mut Realm, c: &Call) -> JsResult {
    let cooked = rt.to_object(&c.arg(0))?;
    let raw = rt.get(cooked, &key("raw"), Value::Object(cooked))?;
    let raw = rt.to_object(&raw)?;
    let len = rt.length_of(raw)?;
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    while (i as f64) < len {
        let seg = rt.get(raw, &PropKey::index(i), Value::Object(raw))?;
        out.extend_from_slice(rt.to_string(&seg)?.units());
        if ((i + 1) as f64) < len {
            if let Some(sub) = c.args.get(i as usize + 1) {
                out.extend_from_slice(rt.to_string(sub)?.units());
            }
        }
        i += 1;
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn value_of(rt: &mut Realm, c: &Call) -> JsResult {
    match &c.this {
        Value::String(s) => Ok(Value::String(s.clone())),
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::Primitive(Value::String(s)) => Ok(Value::String(s.clone())),
            _ => Err(rt.type_error("String.prototype.valueOf requires that 'this' be a String")),
        },
        _ => Err(rt.type_error("String.prototype.valueOf requires that 'this' be a String")),
    }
}

fn char_at(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "charAt")?;
    let i = rt.to_integer(&c.arg(0))?;
    if i < 0.0 || i >= s.len() as f64 {
        return Ok(Value::str(""));
    }
    Ok(Value::String(s.slice(i as usize, i as usize + 1)))
}

fn char_code_at(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "charCodeAt")?;
    let i = rt.to_integer(&c.arg(0))?;
    if i < 0.0 || i >= s.len() as f64 {
        return Ok(Value::Number(f64::NAN));
    }
    Ok(Value::Number(s.units()[i as usize] as f64))
}

pub fn code_point_at(u: &[u16], i: usize) -> (u32, usize) {
    let a = u[i];
    if (0xD800..0xDC00).contains(&a) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) {
        (0x10000 + (((a as u32) - 0xD800) << 10) + (u[i + 1] as u32 - 0xDC00), 2)
    } else {
        (a as u32, 1)
    }
}

fn code_point_at_fn(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "codePointAt")?;
    let i = rt.to_integer(&c.arg(0))?;
    if i < 0.0 || i >= s.len() as f64 {
        return Ok(Value::Undefined);
    }
    Ok(Value::Number(code_point_at(s.units(), i as usize).0 as f64))
}

fn at(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "at")?;
    let n = rt.to_integer(&c.arg(0))?;
    let k = if n >= 0.0 { n } else { s.len() as f64 + n };
    if k < 0.0 || k >= s.len() as f64 {
        return Ok(Value::Undefined);
    }
    Ok(Value::String(s.slice(k as usize, k as usize + 1)))
}

fn concat(rt: &mut Realm, c: &Call) -> JsResult {
    let mut s = this_str(rt, c, "concat")?;
    for a in &c.args {
        let t = rt.to_string(a)?;
        s = s.concat(&t);
    }
    Ok(Value::String(s))
}

fn not_regexp(rt: &mut Realm, v: &Value, method: &str) -> Result<(), Value> {
    if let Value::Object(o) = v {
        let m = rt.get(*o, &PropKey::Sym(Sym::MATCH), v.clone())?;
        let is_re = if m.is_undefined() { matches!(rt.heap.get(*o).kind, Kind::RegExp(_)) } else { m.truthy() };
        if is_re {
            return Err(rt.type_error(&alloc::format!(
                "First argument to String.prototype.{method} must not be a regular expression"
            )));
        }
    }
    Ok(())
}

fn includes(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "includes")?;
    not_regexp(rt, &c.arg(0), "includes")?;
    let search = rt.to_string(&c.arg(0))?;
    let pos = rt.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize;
    Ok(Value::Bool(s.find(&search, pos).is_some()))
}

fn starts_with(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "startsWith")?;
    not_regexp(rt, &c.arg(0), "startsWith")?;
    let search = rt.to_string(&c.arg(0))?;
    let pos = rt.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize;
    Ok(Value::Bool(s.units()[pos..].starts_with(search.units())))
}

fn ends_with(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "endsWith")?;
    not_regexp(rt, &c.arg(0), "endsWith")?;
    let search = rt.to_string(&c.arg(0))?;
    let end =
        if c.arg(1).is_undefined() { s.len() } else { rt.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize };
    Ok(Value::Bool(s.units()[..end].ends_with(search.units())))
}

fn index_of(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "indexOf")?;
    let search = rt.to_string(&c.arg(0))?;
    let pos = rt.to_integer(&c.arg(1))?.clamp(0.0, s.len() as f64) as usize;
    Ok(Value::Number(s.find(&search, pos).map_or(-1.0, |i| i as f64)))
}

fn last_index_of(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "lastIndexOf")?;
    let search = rt.to_string(&c.arg(0))?;
    let n = rt.to_number(&c.arg(1))?;
    let pos = if n.is_nan() { s.len() } else { crate::numconv::to_integer(n).clamp(0.0, s.len() as f64) as usize };
    Ok(Value::Number(s.rfind(&search, pos).map_or(-1.0, |i| i as f64)))
}

fn slice(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "slice")?;
    let len = s.len() as f64;
    let from = rt.relative_index(&c.arg(0), len, 0.0)?;
    let to = rt.relative_index(&c.arg(1), len, len)?;
    Ok(Value::String(if from < to { s.slice(from as usize, to as usize) } else { JsStr::empty() }))
}

fn substring(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "substring")?;
    let len = s.len() as f64;
    let a = rt.to_integer(&c.arg(0))?.clamp(0.0, len);
    let b = if c.arg(1).is_undefined() { len } else { rt.to_integer(&c.arg(1))?.clamp(0.0, len) };
    Ok(Value::String(s.slice(a.min(b) as usize, a.max(b) as usize)))
}

fn substr(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "substr")?;
    let len = s.len() as f64;
    let start = rt.relative_index(&c.arg(0), len, 0.0)?;
    let count = if c.arg(1).is_undefined() { len } else { rt.to_integer(&c.arg(1))? };
    let end = (start + count.max(0.0)).min(len);
    Ok(Value::String(if start < end { s.slice(start as usize, end as usize) } else { JsStr::empty() }))
}

fn map_case(s: &JsStr, upper: bool) -> JsStr {
    let mut out = Vec::with_capacity(s.len());
    for r in char::decode_utf16(s.units().iter().copied()) {
        match r {
            Ok(ch) => {
                let mut buf = [0u16; 2];
                if upper {
                    for u in ch.to_uppercase() {
                        out.extend_from_slice(u.encode_utf16(&mut buf));
                    }
                } else {
                    for u in ch.to_lowercase() {
                        out.extend_from_slice(u.encode_utf16(&mut buf));
                    }
                }
            }
            Err(e) => out.push(e.unpaired_surrogate()),
        }
    }
    JsStr::from_units(out)
}

fn to_lower(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "toLowerCase")?;
    Ok(Value::String(map_case(&s, false)))
}

fn to_upper(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "toUpperCase")?;
    Ok(Value::String(map_case(&s, true)))
}

fn trim_impl(rt: &mut Realm, c: &Call, start: bool, end: bool) -> JsResult {
    let s = this_str(rt, c, "trim")?;
    Ok(Value::String(JsStr::from_units(trim_units(s.units(), start, end).to_vec())))
}

fn trim(rt: &mut Realm, c: &Call) -> JsResult {
    trim_impl(rt, c, true, true)
}

fn trim_start(rt: &mut Realm, c: &Call) -> JsResult {
    trim_impl(rt, c, true, false)
}

fn trim_end(rt: &mut Realm, c: &Call) -> JsResult {
    trim_impl(rt, c, false, true)
}

fn pad(rt: &mut Realm, c: &Call, at_start: bool) -> JsResult {
    let s = this_str(rt, c, "padStart")?;
    let max = rt.to_length(&c.arg(0))?;
    if max <= s.len() as f64 {
        return Ok(Value::String(s));
    }
    let fill = if c.arg(1).is_undefined() { JsStr::from(" ") } else { rt.to_string(&c.arg(1))? };
    if fill.is_empty() {
        return Ok(Value::String(s));
    }
    if max > (1u64 << 30) as f64 {
        return Err(rt.range_error("Invalid string length"));
    }
    let need = max as usize - s.len();
    let filler: Vec<u16> = fill.units().iter().copied().cycle().take(need).collect();
    let mut out = Vec::with_capacity(max as usize);
    if at_start {
        out.extend_from_slice(&filler);
        out.extend_from_slice(s.units());
    } else {
        out.extend_from_slice(s.units());
        out.extend_from_slice(&filler);
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn pad_start(rt: &mut Realm, c: &Call) -> JsResult {
    pad(rt, c, true)
}

fn pad_end(rt: &mut Realm, c: &Call) -> JsResult {
    pad(rt, c, false)
}

fn repeat(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "repeat")?;
    let n = rt.to_integer(&c.arg(0))?;
    if n < 0.0 || n.is_infinite() {
        return Err(rt.range_error("Invalid count value"));
    }
    if s.len() as f64 * n > (1u64 << 30) as f64 {
        return Err(rt.range_error("Invalid string length"));
    }
    let mut out = Vec::with_capacity(s.len() * n as usize);
    for _ in 0..n as usize {
        out.extend_from_slice(s.units());
    }
    Ok(Value::String(JsStr::from_units(out)))
}

/// GetSubstitution: expands $$, $&, $`, $', $n, $nn and $<name>.
pub fn get_substitution(
    rt: &mut Realm,
    matched: &JsStr,
    s: &JsStr,
    pos: usize,
    captures: &[Value],
    named: &Value,
    replacement: &JsStr,
) -> Result<JsStr, Value> {
    let r = replacement.units();
    let mut out: Vec<u16> = Vec::new();
    let tail_pos = (pos + matched.len()).min(s.len());
    let m = captures.len();
    let mut i = 0;
    while i < r.len() {
        let c = r[i];
        if c != b'$' as u16 || i + 1 >= r.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = r[i + 1];
        match n {
            0x24 => {
                out.push(0x24);
                i += 2;
            }
            0x26 => {
                out.extend_from_slice(matched.units());
                i += 2;
            }
            0x60 => {
                out.extend_from_slice(&s.units()[..pos.min(s.len())]);
                i += 2;
            }
            0x27 => {
                out.extend_from_slice(&s.units()[tail_pos..]);
                i += 2;
            }
            0x30..=0x39 => {
                let d1 = (n - 0x30) as usize;
                let two = r.get(i + 2).filter(|d| (0x30..=0x39).contains(*d)).map(|d| d1 * 10 + (*d - 0x30) as usize);
                let (idx, used) = match two {
                    Some(t) if t >= 1 && t <= m => (t, 3),
                    _ => (d1, 2),
                };
                if idx >= 1 && idx <= m {
                    let cap = &captures[idx - 1];
                    if !cap.is_undefined() {
                        out.extend_from_slice(rt.to_string(cap)?.units());
                    }
                    i += used;
                } else {
                    out.push(c);
                    i += 1;
                }
            }
            0x3C if !named.is_undefined() => match r[i + 2..].iter().position(|&x| x == b'>' as u16) {
                Some(end) => {
                    let name = JsStr::from_units(r[i + 2..i + 2 + end].to_vec());
                    let Value::Object(no) = named else { unreachable!() };
                    let v = rt.get(*no, &PropKey::Str(name), named.clone())?;
                    if !v.is_undefined() {
                        out.extend_from_slice(rt.to_string(&v)?.units());
                    }
                    i += 3 + end;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(JsStr::from_units(out))
}

fn replace_impl(rt: &mut Realm, c: &Call, all: bool) -> JsResult {
    if c.this.is_nullish() {
        return Err(rt.type_error("String.prototype.replace called on null or undefined"));
    }
    let search = c.arg(0);
    let replace_value = c.arg(1);
    if !search.is_nullish() {
        if all {
            if let Value::Object(so) = &search {
                if matches!(rt.heap.get(*so).kind, Kind::RegExp(_)) {
                    let flags = rt.get(*so, &key("flags"), search.clone())?;
                    let flags = rt.to_string(&flags)?;
                    if !flags.units().contains(&(b'g' as u16)) {
                        return Err(rt.type_error("replaceAll must be called with a global RegExp"));
                    }
                }
            }
        }
        if let Some(r) = rt.get_method(&search, &PropKey::Sym(Sym::REPLACE))? {
            return rt.call(&r, search, &[c.this.clone(), replace_value]);
        }
    }
    let s = rt.to_string(&c.this)?;
    let search_s = rt.to_string(&search)?;
    let functional = rt.is_callable(&replace_value);
    let rv = if functional { JsStr::empty() } else { rt.to_string(&replace_value)? };
    let mut positions = Vec::new();
    let step = search_s.len().max(1);
    let mut from = 0;
    while let Some(p) = s.find(&search_s, from) {
        positions.push(p);
        if !all {
            break;
        }
        from = p + step;
        if from > s.len() {
            break;
        }
    }
    if positions.is_empty() {
        return Ok(Value::String(s));
    }
    let mut out: Vec<u16> = Vec::new();
    let mut end_of_last = 0;
    for p in positions {
        out.extend_from_slice(&s.units()[end_of_last..p]);
        let rep = if functional {
            let r = rt.call(
                &replace_value,
                Value::Undefined,
                &[Value::String(search_s.clone()), Value::Number(p as f64), Value::String(s.clone())],
            )?;
            rt.to_string(&r)?
        } else {
            get_substitution(rt, &search_s, &s, p, &[], &Value::Undefined, &rv)?
        };
        out.extend_from_slice(rep.units());
        end_of_last = p + search_s.len();
    }
    out.extend_from_slice(&s.units()[end_of_last..]);
    Ok(Value::String(JsStr::from_units(out)))
}

fn replace(rt: &mut Realm, c: &Call) -> JsResult {
    replace_impl(rt, c, false)
}

fn replace_all(rt: &mut Realm, c: &Call) -> JsResult {
    replace_impl(rt, c, true)
}

fn split(rt: &mut Realm, c: &Call) -> JsResult {
    if c.this.is_nullish() {
        return Err(rt.type_error("String.prototype.split called on null or undefined"));
    }
    let sep = c.arg(0);
    if !sep.is_nullish() {
        if let Some(splitter) = rt.get_method(&sep, &PropKey::Sym(Sym::SPLIT))? {
            return rt.call(&splitter, sep, &[c.this.clone(), c.arg(1)]);
        }
    }
    let s = rt.to_string(&c.this)?;
    let limit = if c.arg(1).is_undefined() { u32::MAX } else { rt.to_uint32(&c.arg(1))? };
    let r = rt.to_string(&sep)?;
    if limit == 0 {
        return Ok(rt.array_from(Vec::new()));
    }
    if sep.is_undefined() {
        return Ok(rt.array_from(alloc::vec![Value::String(s)]));
    }
    let mut parts = Vec::new();
    if r.is_empty() {
        for i in 0..s.len().min(limit as usize) {
            parts.push(Value::String(s.slice(i, i + 1)));
        }
        return Ok(rt.array_from(parts));
    }
    if s.is_empty() {
        return Ok(rt.array_from(alloc::vec![Value::String(s)]));
    }
    let mut p = 0;
    while let Some(q) = s.find(&r, p) {
        parts.push(Value::String(s.slice(p, q)));
        if parts.len() as u32 >= limit {
            return Ok(rt.array_from(parts));
        }
        p = q + r.len();
    }
    parts.push(Value::String(s.slice(p, s.len())));
    Ok(rt.array_from(parts))
}

/// match / matchAll / search with a string or regexp argument.
fn regexp_delegate(rt: &mut Realm, c: &Call, sym: Sym, flags: &str) -> JsResult {
    if c.this.is_nullish() {
        return Err(rt.type_error("String.prototype method called on null or undefined"));
    }
    let arg = c.arg(0);
    if !arg.is_nullish() {
        if sym == Sym::MATCH_ALL {
            if let Value::Object(ao) = &arg {
                if matches!(rt.heap.get(*ao).kind, Kind::RegExp(_)) {
                    let f = rt.get(*ao, &key("flags"), arg.clone())?;
                    let f = rt.to_string(&f)?;
                    if !f.units().contains(&(b'g' as u16)) {
                        return Err(rt.type_error("matchAll must be called with a global RegExp"));
                    }
                }
            }
        }
        if let Some(m) = rt.get_method(&arg, &PropKey::Sym(sym))? {
            return rt.call(&m, arg, core::slice::from_ref(&c.this));
        }
    }
    let s = rt.to_string(&c.this)?;
    let pattern = if arg.is_undefined() { JsStr::empty() } else { rt.to_string(&arg)? };
    let rx = regexp::create(rt, pattern, JsStr::from(flags), None)?;
    let m = rt.get_v(&rx, &PropKey::Sym(sym))?;
    rt.call(&m, rx, &[Value::String(s)])
}

fn match_(rt: &mut Realm, c: &Call) -> JsResult {
    regexp_delegate(rt, c, Sym::MATCH, "")
}

fn match_all(rt: &mut Realm, c: &Call) -> JsResult {
    regexp_delegate(rt, c, Sym::MATCH_ALL, "g")
}

fn search(rt: &mut Realm, c: &Call) -> JsResult {
    regexp_delegate(rt, c, Sym::SEARCH, "")
}

fn locale_compare(rt: &mut Realm, c: &Call) -> JsResult {
    let a = this_str(rt, c, "localeCompare")?.to_rust();
    let b = rt.to_string(&c.arg(0))?.to_rust();
    // Approximates collation: case-insensitive first, then lower before upper.
    let key = |s: &str| -> Vec<(char, u8)> {
        s.chars()
            .map(|ch| {
                let base = ch.to_lowercase().next().unwrap_or(ch);
                (fold_accent(base), if ch.is_uppercase() { 1 } else { 0 })
            })
            .collect()
    };
    let (ka, kb) = (key(&a), key(&b));
    let primary = ka.iter().map(|x| x.0).cmp(kb.iter().map(|x| x.0));
    let ord = primary
        .then_with(|| a.chars().map(fold_case_only).cmp(b.chars().map(fold_case_only)))
        .then_with(|| ka.iter().map(|x| x.1).cmp(kb.iter().map(|x| x.1)));
    Ok(Value::Number(match ord {
        core::cmp::Ordering::Less => -1.0,
        core::cmp::Ordering::Equal => 0.0,
        core::cmp::Ordering::Greater => 1.0,
    }))
}

fn fold_case_only(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Strips common Latin accents (é → e) for collation's primary strength.
fn fold_accent(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'ç' => 'c',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'ñ' => 'n',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'ý' | 'ÿ' => 'y',
        c => c,
    }
}

fn normalize(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "normalize")?;
    if !c.arg(0).is_undefined() {
        let f = rt.to_rust_string(&c.arg(0))?;
        if !matches!(f.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
            return Err(rt.range_error("The normalization form should be one of NFC, NFD, NFKC, NFKD."));
        }
    }
    // Without Unicode tables, strings are returned as they are.
    Ok(Value::String(s))
}

fn is_well_formed(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "isWellFormed")?;
    Ok(Value::Bool(char::decode_utf16(s.units().iter().copied()).all(|r| r.is_ok())))
}

fn to_well_formed(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "toWellFormed")?;
    let mut out = Vec::with_capacity(s.len());
    for r in char::decode_utf16(s.units().iter().copied()) {
        let ch = r.unwrap_or('\u{FFFD}');
        let mut buf = [0u16; 2];
        out.extend_from_slice(ch.encode_utf16(&mut buf));
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn iterator(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_str(rt, c, "[Symbol.iterator]")?;
    let p = rt.intr.string_iterator_proto;
    Ok(Value::Object(rt.alloc(Obj::new(Some(p), Kind::StringIterator(s, 0)))))
}

fn iter_next(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(it) = c.this else {
        return Err(rt.type_error("not a String Iterator"));
    };
    let (s, i) = match &rt.heap.get(it).kind {
        Kind::StringIterator(s, i) => (s.clone(), *i as usize),
        _ => return Err(rt.type_error("not a String Iterator")),
    };
    if i >= s.len() {
        return Ok(rt.iter_result(Value::Undefined, true));
    }
    let (_, n) = code_point_at(s.units(), i);
    if let Kind::StringIterator(_, idx) = &mut rt.heap.get_mut(it).kind {
        *idx += n as u32;
    }
    Ok(rt.iter_result(Value::String(s.slice(i, i + n)), false))
}

/// The simple HTML methods (`"x".bold()` and friends), still used on the web.
fn html(rt: &mut Realm, c: &Call, tag: &str, attr: Option<&str>) -> JsResult {
    let s = this_str(rt, c, tag)?.to_rust();
    let mut out = String::from("<");
    out.push_str(tag);
    if let Some(a) = attr {
        let v = rt.to_rust_string(&c.arg(0))?.replace('"', "&quot;");
        out.push_str(&alloc::format!(" {a}=\"{v}\""));
    }
    out.push('>');
    out.push_str(&s);
    out.push_str(&alloc::format!("</{tag}>"));
    Ok(Value::str(&out))
}

fn anchor(rt: &mut Realm, c: &Call) -> JsResult {
    html(rt, c, "a", Some("name"))
}

fn link(rt: &mut Realm, c: &Call) -> JsResult {
    html(rt, c, "a", Some("href"))
}

fn bold(rt: &mut Realm, c: &Call) -> JsResult {
    html(rt, c, "b", None)
}

fn italics(rt: &mut Realm, c: &Call) -> JsResult {
    html(rt, c, "i", None)
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let proto = rt.alloc(Obj::new(Some(op), Kind::Primitive(Value::String(JsStr::empty()))));
    rt.intr.string_proto = proto;
    let ctor = constructor(rt, "String", 1, string_ctor, proto);
    rt.method(ctor, "fromCharCode", 1, from_char_code);
    rt.method(ctor, "fromCodePoint", 1, from_code_point);
    rt.method(ctor, "raw", 1, raw);
    for (name, len, f) in [
        ("at", 1, at as NativeFn),
        ("charAt", 1, char_at),
        ("charCodeAt", 1, char_code_at),
        ("codePointAt", 1, code_point_at_fn),
        ("concat", 1, concat),
        ("endsWith", 1, ends_with),
        ("includes", 1, includes),
        ("indexOf", 1, index_of),
        ("isWellFormed", 0, is_well_formed),
        ("lastIndexOf", 1, last_index_of),
        ("localeCompare", 1, locale_compare),
        ("match", 1, match_),
        ("matchAll", 1, match_all),
        ("normalize", 0, normalize),
        ("padEnd", 1, pad_end),
        ("padStart", 1, pad_start),
        ("repeat", 1, repeat),
        ("replace", 2, replace),
        ("replaceAll", 2, replace_all),
        ("search", 1, search),
        ("slice", 2, slice),
        ("split", 2, split),
        ("startsWith", 1, starts_with),
        ("substring", 2, substring),
        ("substr", 2, substr),
        ("toLowerCase", 0, to_lower),
        ("toLocaleLowerCase", 0, to_lower),
        ("toUpperCase", 0, to_upper),
        ("toLocaleUpperCase", 0, to_upper),
        ("toString", 0, value_of),
        ("toWellFormed", 0, to_well_formed),
        ("trim", 0, trim),
        ("valueOf", 0, value_of),
        ("anchor", 1, anchor),
        ("link", 1, link),
        ("bold", 0, bold),
        ("italics", 0, italics),
    ] {
        rt.method(proto, name, len, f);
    }
    let ts = rt.method(proto, "trimStart", 0, trim_start);
    rt.define(proto, "trimLeft", Value::Object(ts), HIDDEN);
    let te = rt.method(proto, "trimEnd", 0, trim_end);
    rt.define(proto, "trimRight", Value::Object(te), HIDDEN);
    rt.method_sym(proto, Sym::ITERATOR, "[Symbol.iterator]", 0, iterator);

    let ip = rt.intr.iterator_proto;
    let sip = rt.new_object_with(Some(ip));
    rt.intr.string_iterator_proto = sip;
    rt.method(sip, "next", 0, iter_next);
    to_string_tag(rt, sip, "String Iterator");
}
