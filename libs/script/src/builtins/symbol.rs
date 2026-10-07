//! Symbol.

use super::*;
use crate::value::{JsStr, Sym};
use alloc::format;

fn symbol_fn(rt: &mut Realm, c: &Call) -> JsResult {
    if c.is_construct() {
        return Err(rt.type_error("Symbol is not a constructor"));
    }
    let d = c.arg(0);
    let desc = if d.is_undefined() { None } else { Some(rt.to_string(&d)?) };
    Ok(Value::Symbol(rt.new_symbol(desc)))
}

fn this_symbol(rt: &mut Realm, v: &Value) -> Result<Sym, Value> {
    match v {
        Value::Symbol(s) => Ok(*s),
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::Primitive(Value::Symbol(s)) => Ok(*s),
            _ => Err(rt.type_error("not a Symbol")),
        },
        _ => Err(rt.type_error("not a Symbol")),
    }
}

pub fn descriptive_string(rt: &Realm, s: Sym) -> JsStr {
    let d = rt.symbols[s.0 as usize].description.clone().unwrap_or_else(JsStr::empty);
    JsStr::from(format!("Symbol({})", d.to_rust()))
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_symbol(rt, &c.this)?;
    Ok(Value::String(descriptive_string(rt, s)))
}

fn value_of(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Symbol(this_symbol(rt, &c.this)?))
}

fn description(rt: &mut Realm, c: &Call) -> JsResult {
    let s = this_symbol(rt, &c.this)?;
    Ok(rt.symbols[s.0 as usize].description.clone().map(Value::String).unwrap_or(Value::Undefined))
}

fn for_(rt: &mut Realm, c: &Call) -> JsResult {
    let k = rt.to_string(&c.arg(0))?;
    if let Some(s) = rt.registry.get(&k) {
        return Ok(Value::Symbol(*s));
    }
    let s = rt.new_symbol(Some(k.clone()));
    rt.registry.insert(k, s);
    Ok(Value::Symbol(s))
}

fn key_for(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Symbol(s) = c.arg(0) else {
        return Err(rt.type_error("Symbol.keyFor: argument is not a symbol"));
    };
    Ok(rt.registry.iter().find(|(_, v)| **v == s).map(|(k, _)| Value::String(k.clone())).unwrap_or(Value::Undefined))
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let proto = rt.new_object_with(Some(op));
    rt.intr.symbol_proto = proto;
    let ctor = constructor(rt, "Symbol", 0, symbol_fn, proto);
    for (i, name) in Sym::WELL_KNOWN.iter().enumerate() {
        rt.define(ctor, *name, Value::Symbol(Sym(i as u32)), 0);
    }
    rt.method(ctor, "for", 1, for_);
    rt.method(ctor, "keyFor", 1, key_for);
    rt.method(proto, "toString", 0, to_string);
    rt.method(proto, "valueOf", 0, value_of);
    rt.getter(proto, "description", description);
    let tp = rt.native("[Symbol.toPrimitive]", 1, value_of, false);
    rt.define(proto, Sym::TO_PRIMITIVE, Value::Object(tp), CONFIGURABLE);
    to_string_tag(rt, proto, "Symbol");
}
