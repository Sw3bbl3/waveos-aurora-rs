//! Function, Function.prototype and bound functions.

use super::*;
use alloc::format;
use alloc::string::String;

/// `new Function(a, b, body)` and its generator/async relatives (kind in slot 0).
pub fn function_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let kind = match rt.native_slots(c.callee).first() {
        Some(Value::Number(k)) => *k as u8,
        _ => 0,
    };
    let (is_async, is_gen) = (kind & 1 != 0, kind & 2 != 0);
    let mut params = String::new();
    let n = c.args.len();
    for (i, a) in c.args.iter().take(n.saturating_sub(1)).enumerate() {
        if i > 0 {
            params.push(',');
        }
        params.push_str(&rt.to_rust_string(a)?);
    }
    let body = if n > 0 { rt.to_rust_string(&c.args[n - 1])? } else { String::new() };
    let script = crate::parser::parse_function_parts(&params, &body, is_async, is_gen).map_err(|e| {
        let msg = e.message.clone();
        rt.error(crate::realm::ErrorKind::SyntaxError, &msg)
    })?;
    let kw = match (is_async, is_gen) {
        (false, false) => "function",
        (false, true) => "function*",
        (true, false) => "async function",
        (true, true) => "async function*",
    };
    let src = format!("({kw} anonymous({params}\n) {{\n{body}\n}})");
    let code =
        crate::compiler::compile_script(&script, alloc::rc::Rc::from(src.as_str()), alloc::rc::Rc::from("Function"))
            .map_err(|e| {
                let msg = e.message.clone();
                rt.error(crate::realm::ErrorKind::SyntaxError, &msg)
            })?;
    // Run in the global scope; the completion value is the function.
    let g = Value::Object(rt.global);
    let f = rt.call_code(code, g)?;
    if let (Value::Object(fo), true) = (&f, c.is_construct()) {
        let default = rt.heap.get(*fo).proto.unwrap();
        let p = rt.proto_from_ctor(&c.new_target, default)?;
        rt.heap.get_mut(*fo).proto = Some(p);
    }
    Ok(f)
}

fn call(rt: &mut Realm, c: &Call) -> JsResult {
    if !rt.is_callable(&c.this) {
        return Err(rt.type_error("Function.prototype.call called on a non-function"));
    }
    let args = if c.args.len() > 1 { &c.args[1..] } else { &[] };
    rt.call(&c.this, c.arg(0), args)
}

fn apply(rt: &mut Realm, c: &Call) -> JsResult {
    if !rt.is_callable(&c.this) {
        return Err(rt.type_error("Function.prototype.apply called on a non-function"));
    }
    let args = rt.list_from_array_like(&c.arg(1))?;
    rt.call(&c.this, c.arg(0), &args)
}

fn bind(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(target) = c.this else {
        return Err(rt.type_error("Bind must be called on a function"));
    };
    if !rt.is_callable(&c.this) {
        return Err(rt.type_error("Bind must be called on a function"));
    }
    let bound_args: Vec<Value> = c.args.iter().skip(1).cloned().collect();
    let n_bound = bound_args.len();
    let proto = rt.heap.get(target).proto;
    let f = rt.alloc(Obj::new(proto, Kind::Bound(Box::new(Bound { target, this: c.arg(0), args: bound_args }))));
    let mut length = 0.0;
    if rt.has_own(target, &key("length")) {
        if let Value::Number(l) = rt.get(target, &key("length"), c.this.clone())? {
            length = if l.is_infinite() { l } else { (crate::numconv::to_integer(l) - n_bound as f64).max(0.0) };
        }
    }
    rt.define(f, "length", Value::Number(length), CONFIGURABLE);
    let name = match rt.get(target, &key("name"), c.this.clone())? {
        Value::String(s) => s.to_rust(),
        _ => String::new(),
    };
    rt.define(f, "name", Value::str(&format!("bound {name}")), CONFIGURABLE);
    Ok(Value::Object(f))
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.this else {
        return Err(rt.type_error("Function.prototype.toString requires that 'this' be a Function"));
    };
    let s = match &rt.heap.get(o).kind {
        Kind::Function(cl) => match cl.code.source_text() {
            Some(src) => src,
            None => format!("function {}() {{ [native code] }}", cl.code.name),
        },
        Kind::Native(_) | Kind::Bound(_) => {
            let name = match rt.get_own_property(o, &key("name")) {
                Some(Prop { slot: Slot::Data(Value::String(s)), .. }) => s.to_rust(),
                _ => String::new(),
            };
            format!("function {name}() {{ [native code] }}")
        }
        _ => return Err(rt.type_error("Function.prototype.toString requires that 'this' be a Function")),
    };
    Ok(Value::str(&s))
}

fn has_instance(rt: &mut Realm, c: &Call) -> JsResult {
    let r = rt.ordinary_has_instance(&c.this, &c.arg(0))?;
    Ok(Value::Bool(r))
}

fn throw_type_error(rt: &mut Realm, _c: &Call) -> JsResult {
    Err(rt.type_error("'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions"))
}

pub fn init(rt: &mut Realm) {
    let fp = rt.intr.function_proto;
    let ctor = constructor(rt, "Function", 1, function_ctor, fp);
    rt.intr.function_ctor = ctor;
    rt.method(fp, "call", 1, call);
    rt.method(fp, "apply", 2, apply);
    rt.method(fp, "bind", 1, bind);
    rt.method(fp, "toString", 0, to_string);
    let hi = rt.native("[Symbol.hasInstance]", 1, has_instance, false);
    rt.define(fp, Sym::HAS_INSTANCE, Value::Object(hi), 0);
    let tte = rt.native("", 0, throw_type_error, false);
    rt.prevent_extensions(tte);
    rt.intr.throw_type_error = tte;
    let t = Value::Object(tte);
    rt.define_accessor(fp, "caller", t.clone(), t.clone(), CONFIGURABLE);
    rt.define_accessor(fp, "arguments", t.clone(), t, CONFIGURABLE);
}
