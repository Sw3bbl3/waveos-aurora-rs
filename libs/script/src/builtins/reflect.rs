//! Reflect.

use super::*;

fn target(rt: &mut Realm, c: &Call, what: &str) -> Result<ObjRef, Value> {
    match c.arg(0) {
        Value::Object(o) => Ok(o),
        _ => Err(rt.type_error(&alloc::format!("Reflect.{what} called on non-object"))),
    }
}

fn apply(rt: &mut Realm, c: &Call) -> JsResult {
    let f = c.arg(0);
    require_callable(rt, &f, "Reflect.apply")?;
    let args = rt.list_from_array_like(&c.arg(2))?;
    rt.call(&f, c.arg(1), &args)
}

fn construct(rt: &mut Realm, c: &Call) -> JsResult {
    let f = c.arg(0);
    if !rt.is_constructor(&f) {
        return Err(rt.type_error("Reflect.construct target is not a constructor"));
    }
    let nt = if c.args.len() > 2 { c.arg(2) } else { f.clone() };
    if !rt.is_constructor(&nt) {
        return Err(rt.type_error("Reflect.construct newTarget is not a constructor"));
    }
    let args = rt.list_from_array_like(&c.arg(1))?;
    rt.construct(&f, &args, Some(nt))
}

fn define_property(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "defineProperty")?;
    let k = rt.to_property_key(&c.arg(1))?;
    let d = object::to_prop_desc(rt, &c.arg(2))?;
    Ok(Value::Bool(rt.define_own(o, k, d)?))
}

fn delete_property(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "deleteProperty")?;
    let k = rt.to_property_key(&c.arg(1))?;
    Ok(Value::Bool(rt.delete(o, &k)?))
}

fn get(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "get")?;
    let k = rt.to_property_key(&c.arg(1))?;
    let receiver = if c.args.len() > 2 { c.arg(2) } else { c.arg(0) };
    rt.get(o, &k, receiver)
}

fn set(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "set")?;
    let k = rt.to_property_key(&c.arg(1))?;
    let receiver = if c.args.len() > 3 { c.arg(3) } else { c.arg(0) };
    Ok(Value::Bool(rt.set(o, k, c.arg(2), receiver)?))
}

fn get_own_property_descriptor(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "getOwnPropertyDescriptor")?;
    let k = rt.to_property_key(&c.arg(1))?;
    Ok(match rt.get_own_property(o, &k) {
        Some(p) => object::from_prop(rt, &p),
        None => Value::Undefined,
    })
}

fn get_prototype_of(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "getPrototypeOf")?;
    Ok(rt.proto_of(o).map(Value::Object).unwrap_or(Value::Null))
}

fn set_prototype_of(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "setPrototypeOf")?;
    let p = match c.arg(1) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return Err(rt.type_error("Object prototype may only be an Object or null")),
    };
    Ok(Value::Bool(rt.set_prototype(o, p)))
}

fn has(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "has")?;
    let k = rt.to_property_key(&c.arg(1))?;
    Ok(Value::Bool(rt.has_property(o, &k)?))
}

fn is_extensible(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "isExtensible")?;
    Ok(Value::Bool(rt.is_extensible(o)))
}

fn own_keys(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "ownKeys")?;
    let keys: Vec<Value> = rt.own_keys(o).iter().map(|k| k.to_value()).collect();
    Ok(rt.array_from(keys))
}

fn prevent_extensions(rt: &mut Realm, c: &Call) -> JsResult {
    let o = target(rt, c, "preventExtensions")?;
    rt.prevent_extensions(o);
    Ok(Value::Bool(true))
}

pub fn init(rt: &mut Realm) {
    let r = rt.new_object();
    for (name, len, f) in [
        ("apply", 3, apply as NativeFn),
        ("construct", 2, construct),
        ("defineProperty", 3, define_property),
        ("deleteProperty", 2, delete_property),
        ("get", 2, get),
        ("set", 3, set),
        ("getOwnPropertyDescriptor", 2, get_own_property_descriptor),
        ("getPrototypeOf", 1, get_prototype_of),
        ("setPrototypeOf", 2, set_prototype_of),
        ("has", 2, has),
        ("isExtensible", 1, is_extensible),
        ("ownKeys", 1, own_keys),
        ("preventExtensions", 1, prevent_extensions),
    ] {
        rt.method(r, name, len, f);
    }
    to_string_tag(rt, r, "Reflect");
    rt.set_global("Reflect", Value::Object(r));
}
