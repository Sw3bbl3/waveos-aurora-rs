//! Error, its native subclasses and AggregateError.

use super::*;
use crate::realm::ErrorKind;
use alloc::format;

fn error_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let kind = match rt.native_slots(c.callee).first() {
        Some(Value::Number(k)) => ErrorKind::ALL[*k as usize],
        _ => ErrorKind::Error,
    };
    let default = rt.intr.errors[kind as usize];
    let nt = if c.is_construct() { c.new_target.clone() } else { Value::Object(c.callee) };
    let proto = rt.proto_from_ctor(&nt, default)?;
    let o = rt.alloc(Obj::new(Some(proto), Kind::Error));
    let (msg_arg, opts_arg) =
        if kind == ErrorKind::AggregateError { (c.arg(1), c.arg(2)) } else { (c.arg(0), c.arg(1)) };
    let mut msg = alloc::string::String::new();
    if !msg_arg.is_undefined() {
        let m = rt.to_string(&msg_arg)?;
        msg = m.to_rust();
        rt.define(o, "message", Value::String(m), HIDDEN);
    }
    if let Value::Object(opts) = opts_arg {
        if rt.has_property(opts, &key("cause"))? {
            let cause = rt.get(opts, &key("cause"), opts_arg.clone())?;
            rt.define(o, "cause", cause, HIDDEN);
        }
    }
    if kind == ErrorKind::AggregateError {
        let errors = rt.iterate_to_vec(&c.arg(0))?;
        let arr = rt.array_from(errors);
        rt.define(o, "errors", arr, HIDDEN);
    }
    // The name shown in the stack is the constructor's (subclasses too).
    let name = match rt.get(o, &key("name"), Value::Object(o))? {
        Value::String(s) => s.to_rust(),
        _ => alloc::string::String::from(kind.name()),
    };
    rt.attach_stack(o, &name, &msg);
    Ok(Value::Object(o))
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_obj(rt, c, "Error.prototype.toString")?;
    let name = rt.get(o, &key("name"), c.this.clone())?;
    let name = if name.is_undefined() { alloc::string::String::from("Error") } else { rt.to_rust_string(&name)? };
    let msg = rt.get(o, &key("message"), c.this.clone())?;
    let msg = if msg.is_undefined() { alloc::string::String::new() } else { rt.to_rust_string(&msg)? };
    Ok(Value::str(&if name.is_empty() {
        msg
    } else if msg.is_empty() {
        name
    } else {
        format!("{name}: {msg}")
    }))
}

/// V8's Error.captureStackTrace(obj): sets `obj.stack`.
fn capture_stack_trace(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.arg(0) {
        let desc =
            to_string(rt, &Call { this: c.arg(0), args: Vec::new(), new_target: Value::Undefined, callee: c.callee })?;
        let d = rt.to_rust_string(&desc)?;
        rt.attach_stack(o, &d, "");
    }
    Ok(Value::Undefined)
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let error_proto = rt.new_object_with(Some(op));
    rt.intr.errors[0] = error_proto;
    let error = rt.native_with("Error", 1, error_ctor, true, alloc::vec![Value::Number(0.0)]);
    rt.define(error, "prototype", Value::Object(error_proto), 0);
    rt.define(error_proto, "constructor", Value::Object(error), HIDDEN);
    rt.define(error_proto, "name", Value::str("Error"), HIDDEN);
    rt.define(error_proto, "message", Value::str(""), HIDDEN);
    rt.method(error_proto, "toString", 0, to_string);
    rt.method(error, "captureStackTrace", 1, capture_stack_trace);
    rt.define(error, "stackTraceLimit", Value::Number(10.0), DEFAULT);
    rt.set_global("Error", Value::Object(error));
    rt.intr.error_ctors[0] = error;
    for (i, kind) in ErrorKind::ALL.iter().enumerate().skip(1) {
        let proto = rt.new_object_with(Some(error_proto));
        rt.intr.errors[i] = proto;
        let length = if *kind == ErrorKind::AggregateError { 2 } else { 1 };
        let ctor = rt.native_with(kind.name(), length, error_ctor, true, alloc::vec![Value::Number(i as f64)]);
        rt.heap.get_mut(ctor).proto = Some(error);
        rt.define(ctor, "prototype", Value::Object(proto), 0);
        rt.define(proto, "constructor", Value::Object(ctor), HIDDEN);
        rt.define(proto, "name", Value::str(kind.name()), HIDDEN);
        rt.define(proto, "message", Value::str(""), HIDDEN);
        rt.set_global(kind.name(), Value::Object(ctor));
        rt.intr.error_ctors[i] = ctor;
    }
}
