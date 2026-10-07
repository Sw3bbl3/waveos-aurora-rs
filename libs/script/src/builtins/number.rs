//! Number and Boolean.

use super::*;
use crate::numconv;
use alloc::string::String;

fn this_number(rt: &mut Realm, v: &Value) -> Result<f64, Value> {
    match v {
        Value::Number(n) => Ok(*n),
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::Primitive(Value::Number(n)) => Ok(*n),
            _ => Err(rt.type_error("Number.prototype method called on an incompatible receiver")),
        },
        _ => Err(rt.type_error("Number.prototype method called on an incompatible receiver")),
    }
}

fn number_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let n = if c.args.is_empty() { 0.0 } else { rt.to_number(&c.arg(0))? };
    if !c.is_construct() {
        return Ok(Value::Number(n));
    }
    let np = rt.intr.number_proto;
    let proto = rt.proto_from_ctor(&c.new_target, np)?;
    Ok(Value::Object(rt.alloc(Obj::new(Some(proto), Kind::Primitive(Value::Number(n))))))
}

fn value_of(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Number(this_number(rt, &c.this)?))
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let x = this_number(rt, &c.this)?;
    let radix = if c.arg(0).is_undefined() { 10.0 } else { rt.to_integer(&c.arg(0))? };
    if !(2.0..=36.0).contains(&radix) {
        return Err(rt.range_error("toString() radix must be between 2 and 36"));
    }
    if radix == 10.0 {
        return Ok(Value::str(&numconv::to_string(x)));
    }
    Ok(Value::str(&numconv::to_radix(x, radix as u32)))
}

/// Groups digits with commas, as `en-US` does.
fn to_locale_string(rt: &mut Realm, c: &Call) -> JsResult {
    let x = this_number(rt, &c.this)?;
    if !x.is_finite() {
        return Ok(Value::str(&numconv::to_string(x)));
    }
    let max_frac = 3;
    let s = numconv::to_fixed(x.abs(), max_frac);
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let frac = frac.trim_end_matches('0');
    let mut grouped = String::new();
    for (i, ch) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let mut out = String::new();
    if x < 0.0 && (int != "0" || !frac.is_empty()) {
        out.push('-');
    }
    out.push_str(&grouped);
    if !frac.is_empty() {
        out.push('.');
        out.push_str(frac);
    }
    Ok(Value::str(&out))
}

fn to_fixed(rt: &mut Realm, c: &Call) -> JsResult {
    let x = this_number(rt, &c.this)?;
    let f = rt.to_integer(&c.arg(0))?;
    if !(0.0..=100.0).contains(&f) {
        return Err(rt.range_error("toFixed() digits argument must be between 0 and 100"));
    }
    if !x.is_finite() || x.abs() >= 1e21 {
        return Ok(Value::str(&numconv::to_string(x)));
    }
    Ok(Value::str(&numconv::to_fixed(x, f as usize)))
}

fn to_exponential(rt: &mut Realm, c: &Call) -> JsResult {
    let x = this_number(rt, &c.this)?;
    let f = rt.to_integer(&c.arg(0))?;
    if !x.is_finite() {
        return Ok(Value::str(&numconv::to_string(x)));
    }
    if !(0.0..=100.0).contains(&f) {
        return Err(rt.range_error("toExponential() argument must be between 0 and 100"));
    }
    let f = if c.arg(0).is_undefined() { None } else { Some(f as usize) };
    Ok(Value::str(&numconv::to_exponential(x, f)))
}

fn to_precision(rt: &mut Realm, c: &Call) -> JsResult {
    let x = this_number(rt, &c.this)?;
    if c.arg(0).is_undefined() {
        return Ok(Value::str(&numconv::to_string(x)));
    }
    let p = rt.to_integer(&c.arg(0))?;
    if !x.is_finite() {
        return Ok(Value::str(&numconv::to_string(x)));
    }
    if !(1.0..=100.0).contains(&p) {
        return Err(rt.range_error("toPrecision() argument must be between 1 and 100"));
    }
    Ok(Value::str(&numconv::to_precision(x, p as usize)))
}

fn is_finite(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite())))
}

fn is_nan(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_nan())))
}

fn is_integer(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(matches!(c.arg(0), Value::Number(n) if n.is_finite() && libm::trunc(n) == n)))
}

fn is_safe_integer(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(
        matches!(c.arg(0), Value::Number(n) if n.is_finite() && libm::trunc(n) == n && n.abs() <= 9007199254740991.0),
    ))
}

// ---------------------------------------------------------------- Boolean

fn this_bool(rt: &mut Realm, v: &Value) -> Result<bool, Value> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::Primitive(Value::Bool(b)) => Ok(*b),
            _ => Err(rt.type_error("Boolean.prototype method called on an incompatible receiver")),
        },
        _ => Err(rt.type_error("Boolean.prototype method called on an incompatible receiver")),
    }
}

fn boolean_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let b = c.arg(0).truthy();
    if !c.is_construct() {
        return Ok(Value::Bool(b));
    }
    let bp = rt.intr.boolean_proto;
    let proto = rt.proto_from_ctor(&c.new_target, bp)?;
    Ok(Value::Object(rt.alloc(Obj::new(Some(proto), Kind::Primitive(Value::Bool(b))))))
}

fn bool_value_of(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(this_bool(rt, &c.this)?))
}

fn bool_to_string(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::str(if this_bool(rt, &c.this)? { "true" } else { "false" }))
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let proto = rt.alloc(Obj::new(Some(op), Kind::Primitive(Value::Number(0.0))));
    rt.intr.number_proto = proto;
    let ctor = constructor(rt, "Number", 1, number_ctor, proto);
    for (name, v) in [
        ("EPSILON", f64::EPSILON),
        ("MAX_SAFE_INTEGER", 9007199254740991.0),
        ("MIN_SAFE_INTEGER", -9007199254740991.0),
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("NaN", f64::NAN),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("POSITIVE_INFINITY", f64::INFINITY),
    ] {
        constant(rt, ctor, name, Value::Number(v));
    }
    rt.method(ctor, "isFinite", 1, is_finite);
    rt.method(ctor, "isNaN", 1, is_nan);
    rt.method(ctor, "isInteger", 1, is_integer);
    rt.method(ctor, "isSafeInteger", 1, is_safe_integer);
    for (name, len, f) in [
        ("toString", 1, to_string as NativeFn),
        ("toLocaleString", 0, to_locale_string),
        ("toFixed", 1, to_fixed),
        ("toExponential", 1, to_exponential),
        ("toPrecision", 1, to_precision),
        ("valueOf", 0, value_of),
    ] {
        rt.method(proto, name, len, f);
    }

    let bproto = rt.alloc(Obj::new(Some(op), Kind::Primitive(Value::Bool(false))));
    rt.intr.boolean_proto = bproto;
    constructor(rt, "Boolean", 1, boolean_ctor, bproto);
    rt.method(bproto, "toString", 0, bool_to_string);
    rt.method(bproto, "valueOf", 0, bool_value_of);
}
