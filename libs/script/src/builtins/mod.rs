//! The standard library: every global object and prototype.

pub mod array;
pub mod collections;
pub mod console;
pub mod date;
pub mod error;
pub mod function;
pub mod global;
pub mod iterators;
pub mod json;
pub mod math;
pub mod number;
pub mod object;
pub mod promise;
pub mod reflect;
pub mod regexp;
pub mod string;
pub mod symbol;

use crate::heap::ObjRef;
use crate::object::*;
use crate::realm::{Call, JsResult, Realm};
use crate::value::{PropKey, Sym, Value};
use alloc::boxed::Box;
use alloc::vec::Vec;

fn noop(_: &mut Realm, _: &Call) -> JsResult {
    Ok(Value::Undefined)
}

pub fn setup(rt: &mut Realm) {
    // The two roots everything else hangs from.
    let object_proto = rt.alloc(Obj::new(None, Kind::Ordinary));
    rt.intr.object_proto = object_proto;
    let function_proto = rt.alloc(Obj::new(
        Some(object_proto),
        Kind::Native(Box::new(Native { f: noop, ctor: false, slots: Vec::new() })),
    ));
    rt.intr.function_proto = function_proto;
    rt.define(function_proto, "length", Value::Number(0.0), CONFIGURABLE);
    rt.define(function_proto, "name", Value::str(""), CONFIGURABLE);
    rt.global = rt.new_object();
    let g = rt.global;
    rt.define(g, "globalThis", Value::Object(g), HIDDEN);

    // Prototypes that others refer to while being set up.
    rt.intr.array_proto = rt.alloc(Obj::new(Some(object_proto), Kind::Ordinary));
    rt.intr.iterator_proto = rt.alloc(Obj::new(Some(object_proto), Kind::Ordinary));
    rt.intr.async_iterator_proto = rt.alloc(Obj::new(Some(object_proto), Kind::Ordinary));
    let ap = rt.intr.array_proto;
    rt.heap.get_mut(ap).kind = Kind::Array(ArrayStore { dense: Vec::new(), len: 0, len_writable: true });

    error::init(rt);
    function::init(rt);
    object::init(rt);
    symbol::init(rt);
    iterators::init(rt);
    array::init(rt);
    string::init(rt);
    number::init(rt);
    math::init(rt);
    json::init(rt);
    promise::init(rt);
    collections::init(rt);
    regexp::init(rt);
    date::init(rt);
    reflect::init(rt);
    global::init(rt);
    console::init(rt);
}

/// Creates a constructor with its prototype object, and makes it global.
pub fn constructor(rt: &mut Realm, name: &str, length: u32, f: NativeFn, proto: ObjRef) -> ObjRef {
    let c = rt.native(name, length, f, true);
    rt.define(c, "prototype", Value::Object(proto), 0);
    rt.define(proto, "constructor", Value::Object(c), HIDDEN);
    rt.set_global(name, Value::Object(c));
    c
}

/// Defines a value property on an object (non-enumerable, writable).
pub fn value(rt: &mut Realm, o: ObjRef, name: &str, v: Value) {
    rt.define(o, name, v, HIDDEN);
}

pub fn constant(rt: &mut Realm, o: ObjRef, name: &str, v: Value) {
    rt.define(o, name, v, 0);
}

pub fn to_string_tag(rt: &mut Realm, o: ObjRef, tag: &str) {
    rt.define(o, Sym::TO_STRING_TAG, Value::str(tag), CONFIGURABLE);
}

/// The object `this` refers to, if it is an object.
pub fn this_obj(rt: &mut Realm, c: &Call, method: &str) -> Result<ObjRef, Value> {
    match &c.this {
        Value::Object(o) => Ok(*o),
        _ => Err(rt.type_error(&alloc::format!("{method} called on a non-object"))),
    }
}

/// Calls `cb(value, index, obj)` style callbacks.
pub fn callback(rt: &mut Realm, f: &Value, this: &Value, args: &[Value]) -> JsResult {
    rt.call(f, this.clone(), args)
}

pub fn require_callable(rt: &mut Realm, f: &Value, what: &str) -> Result<(), Value> {
    if !rt.is_callable(f) {
        let d = rt.short_describe(f);
        return Err(rt.type_error(&alloc::format!("{d} is not a function ({what})")));
    }
    Ok(())
}

pub fn key(s: &str) -> PropKey {
    PropKey::from(s)
}
