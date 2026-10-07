//! Global functions and values: parseInt, URI encoding, eval, …

use super::*;
use crate::numconv;
use crate::value::JsStr;
use alloc::string::String;

fn parse_int(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    let radix = rt.to_int32(&c.arg(1))?;
    Ok(Value::Number(numconv::parse_int(s.units(), radix)))
}

fn parse_float(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    Ok(Value::Number(numconv::parse_float(s.units())))
}

fn is_nan(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(rt.to_number(&c.arg(0))?.is_nan()))
}

fn is_finite(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(rt.to_number(&c.arg(0))?.is_finite()))
}

const URI_RESERVED: &str = ";/?:@&=+$,#";
const URI_UNESCAPED_EXTRA: &str = "-_.!~*'()";

fn encode(rt: &mut Realm, s: &JsStr, keep: &str) -> JsResult {
    let u = s.units();
    let mut out = String::new();
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        if c < 128 && ((c as u8).is_ascii_alphanumeric() || keep.contains(c as u8 as char)) {
            out.push(c as u8 as char);
            i += 1;
            continue;
        }
        let cp = if (0xD800..0xDC00).contains(&c) {
            match u.get(i + 1) {
                Some(&lo) if (0xDC00..0xE000).contains(&lo) => {
                    i += 1;
                    0x10000 + (((c as u32) - 0xD800) << 10) + (lo as u32 - 0xDC00)
                }
                _ => return Err(rt.error(crate::realm::ErrorKind::URIError, "URI malformed")),
            }
        } else if (0xDC00..0xE000).contains(&c) {
            return Err(rt.error(crate::realm::ErrorKind::URIError, "URI malformed"));
        } else {
            c as u32
        };
        let ch = char::from_u32(cp).unwrap();
        let mut buf = [0u8; 4];
        for b in ch.encode_utf8(&mut buf).bytes() {
            out.push_str(&alloc::format!("%{b:02X}"));
        }
        i += 1;
    }
    Ok(Value::str(&out))
}

fn decode(rt: &mut Realm, s: &JsStr, reserved: &str) -> JsResult {
    let u = s.units();
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0;
    let malformed = |rt: &mut Realm| rt.error(crate::realm::ErrorKind::URIError, "URI malformed");
    let hex = |u: &[u16], i: usize| -> Option<u8> {
        let h = char::from_u32(*u.get(i)? as u32)?.to_digit(16)?;
        let l = char::from_u32(*u.get(i + 1)? as u32)?.to_digit(16)?;
        Some((h * 16 + l) as u8)
    };
    while i < u.len() {
        if u[i] != b'%' as u16 {
            out.push(u[i]);
            i += 1;
            continue;
        }
        let start = i;
        let Some(b) = hex(u, i + 1) else { return Err(malformed(rt)) };
        i += 3;
        if b < 0x80 {
            if reserved.contains(b as char) {
                out.extend_from_slice(&u[start..i]);
            } else {
                out.push(b as u16);
            }
            continue;
        }
        let n = if b & 0xE0 == 0xC0 {
            2
        } else if b & 0xF0 == 0xE0 {
            3
        } else if b & 0xF8 == 0xF0 {
            4
        } else {
            return Err(malformed(rt));
        };
        let mut bytes = alloc::vec![b];
        for _ in 1..n {
            if u.get(i) != Some(&(b'%' as u16)) {
                return Err(malformed(rt));
            }
            let Some(x) = hex(u, i + 1) else { return Err(malformed(rt)) };
            if x & 0xC0 != 0x80 {
                return Err(malformed(rt));
            }
            bytes.push(x);
            i += 3;
        }
        let Ok(st) = core::str::from_utf8(&bytes) else { return Err(malformed(rt)) };
        out.extend(st.encode_utf16());
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn encode_uri(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    let keep = alloc::format!("{URI_RESERVED}{URI_UNESCAPED_EXTRA}");
    encode(rt, &s, &keep)
}

fn encode_uri_component(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    encode(rt, &s, URI_UNESCAPED_EXTRA)
}

fn decode_uri(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    decode(rt, &s, URI_RESERVED)
}

fn decode_uri_component(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    decode(rt, &s, "")
}

fn escape(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    let mut out = String::new();
    for &u in s.units() {
        if u < 128 && ((u as u8).is_ascii_alphanumeric() || "@*_+-./".contains(u as u8 as char)) {
            out.push(u as u8 as char);
        } else if u < 256 {
            out.push_str(&alloc::format!("%{u:02X}"));
        } else {
            out.push_str(&alloc::format!("%u{u:04X}"));
        }
    }
    Ok(Value::str(&out))
}

fn unescape(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_string(&c.arg(0))?;
    let u = s.units();
    let mut out = Vec::new();
    let mut i = 0;
    let hexn = |u: &[u16], i: usize, n: usize| -> Option<u16> {
        let mut v = 0u16;
        for k in 0..n {
            v = v * 16 + char::from_u32(*u.get(i + k)? as u32)?.to_digit(16)? as u16;
        }
        Some(v)
    };
    while i < u.len() {
        if u[i] == b'%' as u16 {
            if u.get(i + 1) == Some(&(b'u' as u16)) {
                if let Some(v) = hexn(u, i + 2, 4) {
                    out.push(v);
                    i += 6;
                    continue;
                }
            } else if let Some(v) = hexn(u, i + 1, 2) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(u[i]);
        i += 1;
    }
    Ok(Value::String(JsStr::from_units(out)))
}

/// Indirect eval: runs in the global scope.
fn eval(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::String(src) = c.arg(0) else { return Ok(c.arg(0)) };
    let code = rt.compile(&src.to_rust(), "eval")?;
    let g = Value::Object(rt.global);
    rt.call_code(code, g)
}

fn queue_microtask(rt: &mut Realm, c: &Call) -> JsResult {
    let f = c.arg(0);
    require_callable(rt, &f, "queueMicrotask")?;
    rt.enqueue(f, Vec::new());
    Ok(Value::Undefined)
}

pub fn init(rt: &mut Realm) {
    let g = rt.global;
    rt.define(g, "NaN", Value::Number(f64::NAN), 0);
    rt.define(g, "Infinity", Value::Number(f64::INFINITY), 0);
    rt.define(g, "undefined", Value::Undefined, 0);
    let pi = rt.method(g, "parseInt", 2, parse_int);
    let pf = rt.method(g, "parseFloat", 1, parse_float);
    let number = rt.get_str(g, "Number").unwrap();
    if let Value::Object(n) = number {
        rt.define(n, "parseInt", Value::Object(pi), HIDDEN);
        rt.define(n, "parseFloat", Value::Object(pf), HIDDEN);
    }
    rt.method(g, "isNaN", 1, is_nan);
    rt.method(g, "isFinite", 1, is_finite);
    rt.method(g, "encodeURI", 1, encode_uri);
    rt.method(g, "encodeURIComponent", 1, encode_uri_component);
    rt.method(g, "decodeURI", 1, decode_uri);
    rt.method(g, "decodeURIComponent", 1, decode_uri_component);
    rt.method(g, "escape", 1, escape);
    rt.method(g, "unescape", 1, unescape);
    let e = rt.method(g, "eval", 1, eval);
    rt.intr.eval = e;
    rt.method(g, "queueMicrotask", 1, queue_microtask);
}
