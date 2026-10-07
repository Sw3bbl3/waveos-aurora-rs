//! JSON.parse and JSON.stringify.

use super::*;
use crate::numconv;
use crate::value::JsStr;
use alloc::string::String;

struct JsonParser<'a> {
    s: &'a [u16],
    pos: usize,
}

impl<'a> JsonParser<'a> {
    fn ws(&mut self) {
        while self.pos < self.s.len() && matches!(self.s[self.pos], 0x20 | 0x09 | 0x0A | 0x0D) {
            self.pos += 1;
        }
    }

    fn err(&self, rt: &mut Realm) -> Value {
        let msg = if self.pos >= self.s.len() {
            String::from("Unexpected end of JSON input")
        } else {
            let ch = char::from_u32(self.s[self.pos] as u32).unwrap_or('?');
            alloc::format!("Unexpected token '{ch}' in JSON at position {}", self.pos)
        };
        rt.error(crate::realm::ErrorKind::SyntaxError, &msg)
    }

    fn lit(&mut self, word: &str) -> bool {
        let w: Vec<u16> = word.encode_utf16().collect();
        if self.s[self.pos..].starts_with(&w) {
            self.pos += w.len();
            true
        } else {
            false
        }
    }

    fn value(&mut self, rt: &mut Realm, depth: u32) -> JsResult {
        if depth > 1000 {
            return Err(rt.range_error("JSON nesting too deep"));
        }
        self.ws();
        let Some(&c) = self.s.get(self.pos) else { return Err(self.err(rt)) };
        match c {
            0x7B => {
                self.pos += 1;
                let o = rt.new_object();
                self.ws();
                if self.s.get(self.pos) == Some(&0x7D) {
                    self.pos += 1;
                    return Ok(Value::Object(o));
                }
                loop {
                    self.ws();
                    if self.s.get(self.pos) != Some(&0x22) {
                        return Err(self.err(rt));
                    }
                    let k = self.string(rt)?;
                    self.ws();
                    if self.s.get(self.pos) != Some(&0x3A) {
                        return Err(self.err(rt));
                    }
                    self.pos += 1;
                    let v = self.value(rt, depth + 1)?;
                    rt.create_data_property(o, PropKey::Str(k), v)?;
                    self.ws();
                    match self.s.get(self.pos) {
                        Some(0x2C) => self.pos += 1,
                        Some(0x7D) => {
                            self.pos += 1;
                            return Ok(Value::Object(o));
                        }
                        _ => return Err(self.err(rt)),
                    }
                }
            }
            0x5B => {
                self.pos += 1;
                let mut items = Vec::new();
                self.ws();
                if self.s.get(self.pos) == Some(&0x5D) {
                    self.pos += 1;
                    return Ok(rt.array_from(items));
                }
                loop {
                    items.push(self.value(rt, depth + 1)?);
                    self.ws();
                    match self.s.get(self.pos) {
                        Some(0x2C) => self.pos += 1,
                        Some(0x5D) => {
                            self.pos += 1;
                            return Ok(rt.array_from(items));
                        }
                        _ => return Err(self.err(rt)),
                    }
                }
            }
            0x22 => Ok(Value::String(self.string(rt)?)),
            0x74 if self.lit("true") => Ok(Value::Bool(true)),
            0x66 if self.lit("false") => Ok(Value::Bool(false)),
            0x6E if self.lit("null") => Ok(Value::Null),
            0x2D | 0x30..=0x39 => self.number(rt),
            _ => Err(self.err(rt)),
        }
    }

    fn number(&mut self, rt: &mut Realm) -> JsResult {
        let start = self.pos;
        let d = |p: &Self| p.s.get(p.pos).is_some_and(|c| (0x30..=0x39).contains(c));
        if self.s[self.pos] == 0x2D {
            self.pos += 1;
        }
        if !d(self) {
            return Err(self.err(rt));
        }
        if self.s[self.pos] == 0x30 {
            self.pos += 1;
        } else {
            while d(self) {
                self.pos += 1;
            }
        }
        if self.s.get(self.pos) == Some(&0x2E) {
            self.pos += 1;
            if !d(self) {
                return Err(self.err(rt));
            }
            while d(self) {
                self.pos += 1;
            }
        }
        if matches!(self.s.get(self.pos), Some(0x65 | 0x45)) {
            self.pos += 1;
            if matches!(self.s.get(self.pos), Some(0x2B | 0x2D)) {
                self.pos += 1;
            }
            if !d(self) {
                return Err(self.err(rt));
            }
            while d(self) {
                self.pos += 1;
            }
        }
        Ok(Value::Number(numconv::parse(&self.s[start..self.pos])))
    }

    fn string(&mut self, rt: &mut Realm) -> Result<JsStr, Value> {
        self.pos += 1;
        let mut out = Vec::new();
        loop {
            let Some(&c) = self.s.get(self.pos) else { return Err(self.err(rt)) };
            self.pos += 1;
            match c {
                0x22 => return Ok(JsStr::from_units(out)),
                0x5C => {
                    let Some(&e) = self.s.get(self.pos) else { return Err(self.err(rt)) };
                    self.pos += 1;
                    out.push(match e {
                        0x22 => 0x22,
                        0x5C => 0x5C,
                        0x2F => 0x2F,
                        0x62 => 0x08,
                        0x66 => 0x0C,
                        0x6E => 0x0A,
                        0x72 => 0x0D,
                        0x74 => 0x09,
                        0x75 => {
                            let mut v = 0u32;
                            for _ in 0..4 {
                                let h = self
                                    .s
                                    .get(self.pos)
                                    .and_then(|&u| char::from_u32(u as u32))
                                    .and_then(|ch| ch.to_digit(16));
                                let Some(h) = h else { return Err(self.err(rt)) };
                                v = v * 16 + h;
                                self.pos += 1;
                            }
                            v as u16
                        }
                        _ => {
                            self.pos -= 1;
                            return Err(self.err(rt));
                        }
                    });
                }
                0..=0x1F => {
                    self.pos -= 1;
                    return Err(self.err(rt));
                }
                _ => out.push(c),
            }
        }
    }
}

fn internalize(rt: &mut Realm, holder: ObjRef, name: PropKey, reviver: &Value) -> JsResult {
    let val = rt.get(holder, &name, Value::Object(holder))?;
    if let Value::Object(o) = val {
        let keys: Vec<PropKey> = if rt.is_array(&val) {
            let len = rt.length_of(o)? as u32;
            (0..len).map(PropKey::index).collect()
        } else {
            rt.own_keys(o)
                .into_iter()
                .filter(|k| matches!(k, PropKey::Str(_)) && rt.get_own_property(o, k).is_some_and(|p| p.enumerable()))
                .collect()
        };
        for k in keys {
            let nv = internalize(rt, o, k.clone(), reviver)?;
            if nv.is_undefined() {
                rt.delete(o, &k)?;
            } else {
                rt.create_data_property(o, k, nv)?;
            }
        }
    }
    let val = rt.get(holder, &name, Value::Object(holder))?;
    rt.call(reviver, Value::Object(holder), &[name.to_value(), val])
}

fn parse(rt: &mut Realm, c: &Call) -> JsResult {
    let text = rt.to_string(&c.arg(0))?;
    let mut p = JsonParser { s: text.units(), pos: 0 };
    let v = p.value(rt, 0)?;
    p.ws();
    if p.pos < text.len() {
        return Err(p.err(rt));
    }
    let reviver = c.arg(1);
    if rt.is_callable(&reviver) {
        let root = rt.new_object();
        rt.create_data_property(root, key(""), v)?;
        return internalize(rt, root, key(""), &reviver);
    }
    Ok(v)
}

pub fn quote(s: &JsStr) -> Vec<u16> {
    let u = s.units();
    let mut out = Vec::with_capacity(u.len() + 2);
    out.push(0x22);
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        match c {
            0x22 => out.extend_from_slice(&[0x5C, 0x22]),
            0x5C => out.extend_from_slice(&[0x5C, 0x5C]),
            0x08 => out.extend_from_slice(&[0x5C, 0x62]),
            0x0C => out.extend_from_slice(&[0x5C, 0x66]),
            0x0A => out.extend_from_slice(&[0x5C, 0x6E]),
            0x0D => out.extend_from_slice(&[0x5C, 0x72]),
            0x09 => out.extend_from_slice(&[0x5C, 0x74]),
            0..=0x1F => out.extend(alloc::format!("\\u{c:04x}").encode_utf16()),
            0xD800..=0xDBFF if i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) => {
                out.push(c);
                out.push(u[i + 1]);
                i += 1;
            }
            0xD800..=0xDFFF => out.extend(alloc::format!("\\u{c:04x}").encode_utf16()),
            _ => out.push(c),
        }
        i += 1;
    }
    out.push(0x22);
    out
}

struct Stringifier {
    replacer: Value,
    allow: Option<Vec<PropKey>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
}

impl Stringifier {
    fn property(&mut self, rt: &mut Realm, holder: ObjRef, k: &PropKey, out: &mut Vec<u16>) -> Result<bool, Value> {
        let mut v = rt.get(holder, k, Value::Object(holder))?;
        if matches!(v, Value::Object(_)) {
            let to_json = rt.get_v(&v, &key("toJSON"))?;
            if rt.is_callable(&to_json) {
                v = rt.call(&to_json, v, &[k.to_value()])?;
            }
        }
        if !self.replacer.is_undefined() {
            v = rt.call(&self.replacer.clone(), Value::Object(holder), &[k.to_value(), v])?;
        }
        if let Value::Object(o) = &v {
            match &rt.heap.get(*o).kind {
                Kind::Primitive(Value::Number(_)) => v = Value::Number(rt.to_number(&v)?),
                Kind::Primitive(Value::String(_)) => v = Value::String(rt.to_string(&v)?),
                Kind::Primitive(Value::Bool(b)) => v = Value::Bool(*b),
                _ => {}
            }
        }
        match &v {
            Value::Null => out.extend("null".encode_utf16()),
            Value::Bool(b) => out.extend(if *b { "true" } else { "false" }.encode_utf16()),
            Value::String(s) => out.extend(quote(s)),
            Value::Number(n) => {
                if n.is_finite() {
                    out.extend(numconv::to_string(*n).encode_utf16())
                } else {
                    out.extend("null".encode_utf16())
                }
            }
            Value::Object(o) if !rt.is_callable(&v) => {
                if rt.join_stack.contains(o) {
                    return Err(rt.type_error("Converting circular structure to JSON"));
                }
                rt.join_stack.push(*o);
                let r = if rt.is_array(&v) { self.array(rt, *o, out) } else { self.object(rt, *o, out) };
                rt.join_stack.pop();
                r?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }

    fn object(&mut self, rt: &mut Realm, o: ObjRef, out: &mut Vec<u16>) -> Result<(), Value> {
        let stepback = self.indent.clone();
        self.indent.extend_from_slice(&self.gap.clone());
        let keys = match &self.allow {
            Some(a) => a.clone(),
            None => rt
                .own_keys(o)
                .into_iter()
                .filter(|k| matches!(k, PropKey::Str(_)) && rt.get_own_property(o, k).is_some_and(|p| p.enumerable()))
                .collect(),
        };
        out.push(0x7B);
        let mut first = true;
        for k in keys {
            let mark = out.len();
            if !first {
                out.push(0x2C);
            }
            if !self.gap.is_empty() {
                out.push(0x0A);
                out.extend_from_slice(&self.indent);
            }
            let PropKey::Str(ks) = &k else { continue };
            out.extend(quote(ks));
            out.push(0x3A);
            if !self.gap.is_empty() {
                out.push(0x20);
            }
            if self.property(rt, o, &k, out)? {
                first = false;
            } else {
                out.truncate(mark);
            }
        }
        if !first && !self.gap.is_empty() {
            out.push(0x0A);
            out.extend_from_slice(&stepback);
        }
        out.push(0x7D);
        self.indent = stepback;
        Ok(())
    }

    fn array(&mut self, rt: &mut Realm, o: ObjRef, out: &mut Vec<u16>) -> Result<(), Value> {
        let stepback = self.indent.clone();
        self.indent.extend_from_slice(&self.gap.clone());
        let len = rt.length_of(o)? as u32;
        out.push(0x5B);
        for i in 0..len {
            if i > 0 {
                out.push(0x2C);
            }
            if !self.gap.is_empty() {
                out.push(0x0A);
                out.extend_from_slice(&self.indent);
            }
            if !self.property(rt, o, &PropKey::index(i), out)? {
                out.extend("null".encode_utf16());
            }
        }
        if len > 0 && !self.gap.is_empty() {
            out.push(0x0A);
            out.extend_from_slice(&stepback);
        }
        out.push(0x5D);
        self.indent = stepback;
        Ok(())
    }
}

pub fn stringify_value(rt: &mut Realm, v: Value, replacer: Value, space: Value) -> JsResult {
    let mut s = Stringifier { replacer: Value::Undefined, allow: None, gap: Vec::new(), indent: Vec::new() };
    if rt.is_callable(&replacer) {
        s.replacer = replacer;
    } else if rt.is_array(&replacer) {
        let items = rt.list_from_array_like(&replacer)?;
        let mut allow: Vec<PropKey> = Vec::new();
        for it in items {
            let k = match &it {
                Value::String(x) => Some(PropKey::Str(x.clone())),
                Value::Number(_) => Some(PropKey::Str(rt.to_string(&it)?)),
                Value::Object(o)
                    if matches!(rt.heap.get(*o).kind, Kind::Primitive(Value::String(_) | Value::Number(_))) =>
                {
                    Some(PropKey::Str(rt.to_string(&it)?))
                }
                _ => None,
            };
            if let Some(k) = k {
                if !allow.contains(&k) {
                    allow.push(k);
                }
            }
        }
        s.allow = Some(allow);
    }
    let space = match &space {
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::Primitive(Value::Number(_)) => Value::Number(rt.to_number(&space)?),
            Kind::Primitive(Value::String(_)) => Value::String(rt.to_string(&space)?),
            _ => space.clone(),
        },
        _ => space.clone(),
    };
    match space {
        Value::Number(n) => s.gap = alloc::vec![0x20; numconv::to_integer(n).clamp(0.0, 10.0) as usize],
        Value::String(g) => s.gap = g.units()[..g.len().min(10)].to_vec(),
        _ => {}
    }
    let wrapper = rt.new_object();
    rt.create_data_property(wrapper, key(""), v)?;
    let mut out = Vec::new();
    let saved = rt.join_stack.len();
    let r = s.property(rt, wrapper, &key(""), &mut out);
    rt.join_stack.truncate(saved);
    if r? {
        Ok(Value::String(JsStr::from_units(out)))
    } else {
        Ok(Value::Undefined)
    }
}

fn stringify(rt: &mut Realm, c: &Call) -> JsResult {
    stringify_value(rt, c.arg(0), c.arg(1), c.arg(2))
}

pub fn init(rt: &mut Realm) {
    let j = rt.new_object();
    rt.method(j, "parse", 2, parse);
    rt.method(j, "stringify", 3, stringify);
    to_string_tag(rt, j, "JSON");
    rt.set_global("JSON", Value::Object(j));
}
