//! Object and Object.prototype.

use super::*;
use crate::realm::PropDesc;
use alloc::format;

pub fn to_prop_desc(rt: &mut Realm, v: &Value) -> Result<PropDesc, Value> {
    let Value::Object(o) = v else {
        return Err(rt.type_error("Property description must be an object"));
    };
    let o = *o;
    let mut d = PropDesc::default();
    let field = |rt: &mut Realm, name: &str| -> Result<Option<Value>, Value> {
        if rt.has_property(o, &key(name))? {
            Ok(Some(rt.get(o, &key(name), Value::Object(o))?))
        } else {
            Ok(None)
        }
    };
    d.enumerable = field(rt, "enumerable")?.map(|v| v.truthy());
    d.configurable = field(rt, "configurable")?.map(|v| v.truthy());
    d.value = field(rt, "value")?;
    d.writable = field(rt, "writable")?.map(|v| v.truthy());
    d.get = field(rt, "get")?;
    d.set = field(rt, "set")?;
    for f in [&d.get, &d.set].into_iter().flatten() {
        if !f.is_undefined() && !rt.is_callable(f) {
            return Err(rt.type_error("Getter and setter must be functions"));
        }
    }
    if d.is_accessor() && d.is_data() {
        return Err(rt.type_error(
            "Invalid property descriptor. Cannot both specify accessors and a value or writable attribute",
        ));
    }
    Ok(d)
}

pub fn from_prop(rt: &mut Realm, p: &Prop) -> Value {
    let o = rt.new_object();
    match &p.slot {
        Slot::Data(v) => {
            rt.define(o, "value", v.clone(), DEFAULT);
            rt.define(o, "writable", Value::Bool(p.writable()), DEFAULT);
        }
        Slot::Accessor(g, s) => {
            rt.define(o, "get", g.clone(), DEFAULT);
            rt.define(o, "set", s.clone(), DEFAULT);
        }
    }
    rt.define(o, "enumerable", Value::Bool(p.enumerable()), DEFAULT);
    rt.define(o, "configurable", Value::Bool(p.configurable()), DEFAULT);
    Value::Object(o)
}

/// The TypeError for a failed defineProperty, worded as V8 does.
fn define_failed(rt: &mut Realm, o: ObjRef, k: &PropKey) -> Value {
    let n = rt.key_display(k);
    if !rt.has_own(o, k) && !rt.is_extensible(o) {
        rt.type_error(&format!("Cannot define property {n}, object is not extensible"))
    } else {
        rt.type_error(&format!("Cannot redefine property: {n}"))
    }
}

fn object_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if c.is_construct() && c.new_target != Value::Object(c.callee) {
        let op = rt.intr.object_proto;
        let p = rt.proto_from_ctor(&c.new_target, op)?;
        return Ok(Value::Object(rt.new_object_with(Some(p))));
    }
    let v = c.arg(0);
    if v.is_nullish() {
        return Ok(Value::Object(rt.new_object()));
    }
    Ok(Value::Object(rt.to_object(&v)?))
}

fn assign(rt: &mut Realm, c: &Call) -> JsResult {
    let target = rt.to_object(&c.arg(0))?;
    for src in c.args.iter().skip(1) {
        if src.is_nullish() {
            continue;
        }
        let from = rt.to_object(src)?;
        for k in rt.own_keys(from) {
            if rt.get_own_property(from, &k).is_some_and(|p| p.enumerable()) {
                let v = rt.get(from, &k, Value::Object(from))?;
                if !rt.set(target, k.clone(), v, Value::Object(target))? {
                    let d = rt.key_display(&k);
                    return Err(rt.type_error(&format!("Cannot assign to read only property '{d}'")));
                }
            }
        }
    }
    Ok(Value::Object(target))
}

fn define_properties_from(rt: &mut Realm, o: ObjRef, props: &Value) -> Result<(), Value> {
    let p = rt.to_object(props)?;
    let mut descs = Vec::new();
    for k in rt.own_keys(p) {
        if rt.get_own_property(p, &k).is_some_and(|x| x.enumerable()) {
            let dv = rt.get(p, &k, Value::Object(p))?;
            descs.push((k, to_prop_desc(rt, &dv)?));
        }
    }
    for (k, d) in descs {
        if !rt.define_own(o, k.clone(), d)? {
            return Err(define_failed(rt, o, &k));
        }
    }
    Ok(())
}

fn create(rt: &mut Realm, c: &Call) -> JsResult {
    let proto = match c.arg(0) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return Err(rt.type_error("Object prototype may only be an Object or null")),
    };
    let o = rt.new_object_with(proto);
    if !c.arg(1).is_undefined() {
        define_properties_from(rt, o, &c.arg(1))?;
    }
    Ok(Value::Object(o))
}

fn define_property(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.arg(0) else {
        return Err(rt.type_error("Object.defineProperty called on non-object"));
    };
    let k = rt.to_property_key(&c.arg(1))?;
    let d = to_prop_desc(rt, &c.arg(2))?;
    if !rt.define_own(o, k.clone(), d)? {
        return Err(define_failed(rt, o, &k));
    }
    Ok(c.arg(0))
}

fn define_properties(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.arg(0) else {
        return Err(rt.type_error("Object.defineProperties called on non-object"));
    };
    define_properties_from(rt, o, &c.arg(1))?;
    Ok(c.arg(0))
}

/// keys (0), values (1) or entries (2) of own enumerable string properties.
fn enumerable_own(rt: &mut Realm, v: &Value, kind: u8) -> JsResult {
    let o = rt.to_object(v)?;
    let mut out = Vec::new();
    for k in rt.own_keys(o) {
        let PropKey::Str(s) = &k else { continue };
        if rt.get_own_property(o, &k).is_some_and(|p| p.enumerable()) {
            match kind {
                0 => out.push(Value::String(s.clone())),
                1 => out.push(rt.get(o, &k, Value::Object(o))?),
                _ => {
                    let val = rt.get(o, &k, Value::Object(o))?;
                    let pair = rt.array_from(alloc::vec![Value::String(s.clone()), val]);
                    out.push(pair);
                }
            }
        }
    }
    Ok(rt.array_from(out))
}

fn keys(rt: &mut Realm, c: &Call) -> JsResult {
    enumerable_own(rt, &c.arg(0), 0)
}

fn values(rt: &mut Realm, c: &Call) -> JsResult {
    enumerable_own(rt, &c.arg(0), 1)
}

fn entries(rt: &mut Realm, c: &Call) -> JsResult {
    enumerable_own(rt, &c.arg(0), 2)
}

fn from_entries(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.new_object();
    let items = rt.iterate_to_vec(&c.arg(0))?;
    for item in items {
        let Value::Object(io) = item else {
            return Err(rt.type_error("Iterator value is not an entry object"));
        };
        let k = rt.get(io, &PropKey::index(0), item.clone())?;
        let v = rt.get(io, &PropKey::index(1), item.clone())?;
        let k = rt.to_property_key(&k)?;
        rt.create_data_property(o, k, v)?;
    }
    Ok(Value::Object(o))
}

fn freeze(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.arg(0) {
        rt.set_integrity(o, true)?;
    }
    Ok(c.arg(0))
}

fn seal(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.arg(0) {
        rt.set_integrity(o, false)?;
    }
    Ok(c.arg(0))
}

fn prevent_extensions(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.arg(0) {
        rt.prevent_extensions(o);
    }
    Ok(c.arg(0))
}

fn is_frozen(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(match c.arg(0) {
        Value::Object(o) => rt.test_integrity(o, true),
        _ => true,
    }))
}

fn is_sealed(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(match c.arg(0) {
        Value::Object(o) => rt.test_integrity(o, false),
        _ => true,
    }))
}

fn is_extensible(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(match c.arg(0) {
        Value::Object(o) => rt.is_extensible(o),
        _ => false,
    }))
}

fn get_prototype_of(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.arg(0))?;
    Ok(rt.proto_of(o).map(Value::Object).unwrap_or(Value::Null))
}

fn set_prototype_of(rt: &mut Realm, c: &Call) -> JsResult {
    let v = c.arg(0);
    if v.is_nullish() {
        return Err(rt.type_error("Object.setPrototypeOf called on null or undefined"));
    }
    let proto = match c.arg(1) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return Err(rt.type_error("Object prototype may only be an Object or null")),
    };
    if let Value::Object(o) = v {
        if !rt.set_prototype(o, proto) {
            return Err(rt.type_error("Cyclic __proto__ value or non-extensible object"));
        }
    }
    Ok(v)
}

fn get_own_property_descriptor(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.arg(0))?;
    let k = rt.to_property_key(&c.arg(1))?;
    match rt.get_own_property(o, &k) {
        Some(p) => Ok(from_prop(rt, &p)),
        None => Ok(Value::Undefined),
    }
}

fn get_own_property_descriptors(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.arg(0))?;
    let out = rt.new_object();
    for k in rt.own_keys(o) {
        if let Some(p) = rt.get_own_property(o, &k) {
            let d = from_prop(rt, &p);
            rt.create_data_property(out, k, d)?;
        }
    }
    Ok(Value::Object(out))
}

fn get_own_property_names(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.arg(0))?;
    let names: Vec<Value> =
        rt.own_keys(o).into_iter().filter_map(|k| matches!(k, PropKey::Str(_)).then(|| k.to_value())).collect();
    Ok(rt.array_from(names))
}

fn get_own_property_symbols(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.arg(0))?;
    let syms: Vec<Value> =
        rt.own_keys(o).into_iter().filter_map(|k| matches!(k, PropKey::Sym(_)).then(|| k.to_value())).collect();
    Ok(rt.array_from(syms))
}

fn is(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(rt.same_value(&c.arg(0), &c.arg(1))))
}

fn has_own(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.arg(0))?;
    let k = rt.to_property_key(&c.arg(1))?;
    Ok(Value::Bool(rt.has_own(o, &k)))
}

fn group_by(rt: &mut Realm, c: &Call) -> JsResult {
    let items = rt.iterate_to_vec(&c.arg(0))?;
    let f = c.arg(1);
    require_callable(rt, &f, "Object.groupBy")?;
    let out = rt.new_object_with(None);
    for (i, item) in items.into_iter().enumerate() {
        let k = rt.call(&f, Value::Undefined, &[item.clone(), Value::Number(i as f64)])?;
        let k = rt.to_property_key(&k)?;
        let existing = rt.get(out, &k, Value::Object(out))?;
        match existing {
            Value::Object(arr) => {
                let len = rt.length_of(arr)? as u32;
                rt.create_data_property(arr, PropKey::index(len), item)?;
            }
            _ => {
                let arr = rt.array_from(alloc::vec![item]);
                rt.create_data_property(out, k, arr)?;
            }
        }
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------- prototype

fn has_own_property(rt: &mut Realm, c: &Call) -> JsResult {
    let k = rt.to_property_key(&c.arg(0))?;
    let o = rt.to_object(&c.this)?;
    Ok(Value::Bool(rt.has_own(o, &k)))
}

fn is_prototype_of(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(mut v) = c.arg(0) else { return Ok(Value::Bool(false)) };
    let o = rt.to_object(&c.this)?;
    while let Some(p) = rt.proto_of(v) {
        if p == o {
            return Ok(Value::Bool(true));
        }
        v = p;
    }
    Ok(Value::Bool(false))
}

fn property_is_enumerable(rt: &mut Realm, c: &Call) -> JsResult {
    let k = rt.to_property_key(&c.arg(0))?;
    let o = rt.to_object(&c.this)?;
    Ok(Value::Bool(rt.get_own_property(o, &k).is_some_and(|p| p.enumerable())))
}

pub fn proto_to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let o = match &c.this {
        Value::Undefined => return Ok(Value::str("[object Undefined]")),
        Value::Null => return Ok(Value::str("[object Null]")),
        v => rt.to_object(v)?,
    };
    let builtin = match &rt.heap.get(o).kind {
        Kind::Array(_) => "Array",
        Kind::Arguments => "Arguments",
        Kind::Function(_) | Kind::Native(_) | Kind::Bound(_) => "Function",
        Kind::Error => "Error",
        Kind::Primitive(Value::Bool(_)) => "Boolean",
        Kind::Primitive(Value::Number(_)) => "Number",
        Kind::Primitive(Value::String(_)) => "String",
        Kind::Date(_) => "Date",
        Kind::RegExp(_) => "RegExp",
        _ => "Object",
    };
    let tag = rt.get(o, &PropKey::Sym(Sym::TO_STRING_TAG), Value::Object(o))?;
    let tag = match tag {
        Value::String(s) => s.to_rust(),
        _ => alloc::string::String::from(builtin),
    };
    Ok(Value::str(&format!("[object {tag}]")))
}

fn to_locale_string(rt: &mut Realm, c: &Call) -> JsResult {
    let f = rt.get_v(&c.this, &key("toString"))?;
    rt.call(&f, c.this.clone(), &[])
}

fn value_of(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Object(rt.to_object(&c.this)?))
}

fn proto_getter(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    Ok(rt.proto_of(o).map(Value::Object).unwrap_or(Value::Null))
}

fn proto_setter(rt: &mut Realm, c: &Call) -> JsResult {
    if c.this.is_nullish() {
        return Err(rt.type_error("Object.prototype.__proto__ called on null or undefined"));
    }
    let proto = match c.arg(0) {
        Value::Object(p) => Some(p),
        Value::Null => None,
        _ => return Ok(Value::Undefined),
    };
    if let Value::Object(o) = c.this {
        if !rt.set_prototype(o, proto) {
            return Err(rt.type_error("Cyclic __proto__ value"));
        }
    }
    Ok(Value::Undefined)
}

fn define_getter(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    require_callable(rt, &c.arg(1), "__defineGetter__")?;
    let k = rt.to_property_key(&c.arg(0))?;
    let d = PropDesc { get: Some(c.arg(1)), enumerable: Some(true), configurable: Some(true), ..PropDesc::default() };
    rt.define_own(o, k, d)?;
    Ok(Value::Undefined)
}

fn define_setter(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    require_callable(rt, &c.arg(1), "__defineSetter__")?;
    let k = rt.to_property_key(&c.arg(0))?;
    let d = PropDesc { set: Some(c.arg(1)), enumerable: Some(true), configurable: Some(true), ..PropDesc::default() };
    rt.define_own(o, k, d)?;
    Ok(Value::Undefined)
}

fn lookup_accessor(rt: &mut Realm, c: &Call, getter: bool) -> JsResult {
    let mut o = rt.to_object(&c.this)?;
    let k = rt.to_property_key(&c.arg(0))?;
    loop {
        if let Some(p) = rt.get_own_property(o, &k) {
            return Ok(match p.slot {
                Slot::Accessor(g, s) => {
                    if getter {
                        g
                    } else {
                        s
                    }
                }
                _ => Value::Undefined,
            });
        }
        match rt.proto_of(o) {
            Some(p) => o = p,
            None => return Ok(Value::Undefined),
        }
    }
}

fn lookup_getter(rt: &mut Realm, c: &Call) -> JsResult {
    lookup_accessor(rt, c, true)
}

fn lookup_setter(rt: &mut Realm, c: &Call) -> JsResult {
    lookup_accessor(rt, c, false)
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let ctor = constructor(rt, "Object", 1, object_ctor, op);
    rt.intr.object_ctor = ctor;
    for (name, len, f) in [
        ("assign", 2, assign as NativeFn),
        ("create", 2, create),
        ("defineProperty", 3, define_property),
        ("defineProperties", 2, define_properties),
        ("keys", 1, keys),
        ("values", 1, values),
        ("entries", 1, entries),
        ("fromEntries", 1, from_entries),
        ("freeze", 1, freeze),
        ("seal", 1, seal),
        ("preventExtensions", 1, prevent_extensions),
        ("isFrozen", 1, is_frozen),
        ("isSealed", 1, is_sealed),
        ("isExtensible", 1, is_extensible),
        ("getPrototypeOf", 1, get_prototype_of),
        ("setPrototypeOf", 2, set_prototype_of),
        ("getOwnPropertyDescriptor", 2, get_own_property_descriptor),
        ("getOwnPropertyDescriptors", 1, get_own_property_descriptors),
        ("getOwnPropertyNames", 1, get_own_property_names),
        ("getOwnPropertySymbols", 1, get_own_property_symbols),
        ("is", 2, is),
        ("hasOwn", 2, has_own),
        ("groupBy", 2, group_by),
    ] {
        rt.method(ctor, name, len, f);
    }
    for (name, len, f) in [
        ("hasOwnProperty", 1, has_own_property as NativeFn),
        ("isPrototypeOf", 1, is_prototype_of),
        ("propertyIsEnumerable", 1, property_is_enumerable),
        ("toString", 0, proto_to_string),
        ("toLocaleString", 0, to_locale_string),
        ("valueOf", 0, value_of),
        ("__defineGetter__", 2, define_getter),
        ("__defineSetter__", 2, define_setter),
        ("__lookupGetter__", 1, lookup_getter),
        ("__lookupSetter__", 1, lookup_setter),
    ] {
        rt.method(op, name, len, f);
    }
    let g = rt.native("get __proto__", 0, proto_getter, false);
    let s = rt.native("set __proto__", 1, proto_setter, false);
    rt.define_accessor(op, "__proto__", Value::Object(g), Value::Object(s), CONFIGURABLE);
}
