//! The Iterator constructor and iterator helpers (ES2025): lazy map, filter,
//! take, drop and flatMap; eager reduce, toArray, forEach, some, every, find;
//! and Iterator.from.

use super::*;

/// GetIteratorDirect: an iterator record for an iterator object itself.
fn direct(rt: &mut Realm, o: &Value) -> Result<ObjRef, Value> {
    let next = rt.get_v(o, &key("next"))?;
    Ok(rt.alloc(Obj::new(
        None,
        Kind::IterRecord(Box::new(IterRecord { iter: o.clone(), next, done: false, fast: None })),
    )))
}

/// IteratorClose with a throw completion: closes `o`, then returns `e`.
fn close_with(rt: &mut Realm, o: &Value, e: Value) -> Value {
    if let Ok(Some(ret)) = rt.get_method(o, &key("return")) {
        let _ = rt.call(&ret, o.clone(), &[]);
    }
    e
}

fn this_iter(rt: &mut Realm, c: &Call, name: &str) -> Result<Value, Value> {
    match &c.this {
        Value::Object(_) => Ok(c.this.clone()),
        _ => Err(rt.type_error(&alloc::format!("Iterator.prototype.{name} called on non-object"))),
    }
}

/// map, filter, flatMap: (this, callable) → a helper.
fn lazy_with_fn(rt: &mut Realm, c: &Call, kind: HelperKind, name: &str) -> JsResult {
    let o = this_iter(rt, c, name)?;
    let f = c.arg(0);
    if !rt.is_callable(&f) {
        let e = rt.type_error(&alloc::format!("Iterator.prototype.{name}: the argument is not a function"));
        return Err(close_with(rt, &o, e));
    }
    let underlying = direct(rt, &o)?;
    Ok(make_helper(rt, kind, underlying, f, 0.0))
}

/// take, drop: (this, limit) → a helper.
fn lazy_with_limit(rt: &mut Realm, c: &Call, kind: HelperKind, name: &str) -> JsResult {
    let o = this_iter(rt, c, name)?;
    let n = match rt.to_number(&c.arg(0)) {
        Ok(n) => n,
        Err(e) => return Err(close_with(rt, &o, e)),
    };
    if n.is_nan() {
        let e = rt.range_error(&alloc::format!("Iterator.prototype.{name}: the limit is NaN"));
        return Err(close_with(rt, &o, e));
    }
    let n = crate::numconv::to_integer(n);
    // Infinity is allowed; finite limits past 2^53 - 1 are not.
    if n < 0.0 || (n.is_finite() && n > 9007199254740991.0) {
        let e = rt.range_error(&alloc::format!("Iterator.prototype.{name}: the limit is negative"));
        return Err(close_with(rt, &o, e));
    }
    let underlying = direct(rt, &o)?;
    Ok(make_helper(rt, kind, underlying, Value::Undefined, n))
}

fn make_helper(rt: &mut Realm, kind: HelperKind, underlying: ObjRef, f: Value, limit: f64) -> Value {
    let proto = rt.intr.iterator_helper_proto;
    Value::Object(rt.alloc(Obj::new(
        Some(proto),
        Kind::IterHelper(Box::new(IterHelper {
            kind,
            underlying,
            f,
            counter: 0.0,
            limit,
            inner: None,
            done: false,
            running: false,
            started: false,
        })),
    )))
}

fn map(rt: &mut Realm, c: &Call) -> JsResult {
    lazy_with_fn(rt, c, HelperKind::Map, "map")
}

fn filter(rt: &mut Realm, c: &Call) -> JsResult {
    lazy_with_fn(rt, c, HelperKind::Filter, "filter")
}

fn flat_map(rt: &mut Realm, c: &Call) -> JsResult {
    lazy_with_fn(rt, c, HelperKind::FlatMap, "flatMap")
}

fn take(rt: &mut Realm, c: &Call) -> JsResult {
    lazy_with_limit(rt, c, HelperKind::Take, "take")
}

fn drop(rt: &mut Realm, c: &Call) -> JsResult {
    lazy_with_limit(rt, c, HelperKind::Drop, "drop")
}

fn helper_state(rt: &mut Realm, c: &Call, wrap: bool) -> Result<ObjRef, Value> {
    if let Value::Object(o) = c.this {
        if let Kind::IterHelper(h) = &rt.heap.get(o).kind {
            if (h.kind == HelperKind::Wrap) == wrap {
                return Ok(o);
            }
        }
    }
    Err(rt.type_error("Iterator helper method called on an incompatible receiver"))
}

fn with_helper<R>(rt: &mut Realm, h: ObjRef, f: impl FnOnce(&mut IterHelper) -> R) -> R {
    match &mut rt.heap.get_mut(h).kind {
        Kind::IterHelper(s) => f(s),
        _ => unreachable!(),
    }
}

fn underlying_iter(rt: &Realm, rec: ObjRef) -> Value {
    match &rt.heap.get(rec).kind {
        Kind::IterRecord(r) => r.iter.clone(),
        _ => Value::Undefined,
    }
}

/// Calls `f(value, counter)` for a helper; a throw closes the underlying iterator.
fn call_fn(rt: &mut Realm, h: ObjRef, v: Value) -> JsResult {
    let (f, n, rec) = with_helper(rt, h, |s| {
        let n = s.counter;
        s.counter += 1.0;
        (s.f.clone(), n, s.underlying)
    });
    match rt.call(&f, Value::Undefined, &[v, Value::Number(n)]) {
        Ok(r) => Ok(r),
        Err(e) => {
            let it = underlying_iter(rt, rec);
            Err(close_with(rt, &it, e))
        }
    }
}

fn helper_next_value(rt: &mut Realm, h: ObjRef) -> Result<Option<Value>, Value> {
    let (kind, rec) = with_helper(rt, h, |s| (s.kind, s.underlying));
    match kind {
        HelperKind::Map => match rt.iter_step(rec)? {
            Some(v) => Ok(Some(call_fn(rt, h, v)?)),
            None => Ok(None),
        },
        HelperKind::Filter => loop {
            let Some(v) = rt.iter_step(rec)? else { return Ok(None) };
            if call_fn(rt, h, v.clone())?.truthy() {
                return Ok(Some(v));
            }
        },
        HelperKind::Take => {
            let remaining = with_helper(rt, h, |s| s.limit);
            if remaining == 0.0 {
                let it = underlying_iter(rt, rec);
                if let Some(ret) = rt.get_method(&it, &key("return"))? {
                    rt.call(&ret, it, &[])?;
                }
                return Ok(None);
            }
            if remaining.is_finite() {
                with_helper(rt, h, |s| s.limit -= 1.0);
            }
            rt.iter_step(rec)
        }
        HelperKind::Drop => {
            while with_helper(rt, h, |s| s.limit) > 0.0 {
                with_helper(rt, h, |s| s.limit -= 1.0);
                if rt.iter_step(rec)?.is_none() {
                    return Ok(None);
                }
            }
            rt.iter_step(rec)
        }
        HelperKind::FlatMap => loop {
            if let Some(inner) = with_helper(rt, h, |s| s.inner) {
                match rt.iter_step(inner) {
                    Ok(Some(v)) => return Ok(Some(v)),
                    Ok(None) => with_helper(rt, h, |s| s.inner = None),
                    Err(e) => {
                        let it = underlying_iter(rt, rec);
                        return Err(close_with(rt, &it, e));
                    }
                }
                continue;
            }
            let Some(v) = rt.iter_step(rec)? else { return Ok(None) };
            let mapped = call_fn(rt, h, v)?;
            match flattenable(rt, &mapped) {
                Ok(inner) => with_helper(rt, h, |s| s.inner = Some(inner)),
                Err(e) => {
                    let it = underlying_iter(rt, rec);
                    return Err(close_with(rt, &it, e));
                }
            }
        },
        HelperKind::Wrap => {
            let (iter, next) = match &rt.heap.get(rec).kind {
                Kind::IterRecord(r) => (r.iter.clone(), r.next.clone()),
                _ => unreachable!(),
            };
            let r = rt.call(&next, iter, &[])?;
            Ok(Some(r))
        }
    }
}

/// GetIteratorFlattenable (rejecting strings).
fn flattenable(rt: &mut Realm, v: &Value) -> Result<ObjRef, Value> {
    if !matches!(v, Value::Object(_)) {
        return Err(rt.type_error("flatMap mapper must return an iterable object"));
    }
    match rt.get_method(v, &PropKey::Sym(Sym::ITERATOR))? {
        Some(m) => {
            let it = rt.call(&m, v.clone(), &[])?;
            if !matches!(it, Value::Object(_)) {
                return Err(rt.type_error("Result of the Symbol.iterator method is not an object"));
            }
            direct(rt, &it)
        }
        None => direct(rt, v),
    }
}

fn helper_next(rt: &mut Realm, c: &Call) -> JsResult {
    let h = helper_state(rt, c, false)?;
    let (done, running) = with_helper(rt, h, |s| (s.done, s.running));
    if running {
        return Err(rt.type_error("Iterator helper is already running"));
    }
    if done {
        return Ok(rt.iter_result(Value::Undefined, true));
    }
    with_helper(rt, h, |s| {
        s.running = true;
        s.started = true;
    });
    let r = helper_next_value(rt, h);
    with_helper(rt, h, |s| s.running = false);
    match r {
        Ok(Some(v)) => Ok(rt.iter_result(v, false)),
        Ok(None) => {
            with_helper(rt, h, |s| s.done = true);
            Ok(rt.iter_result(Value::Undefined, true))
        }
        Err(e) => {
            with_helper(rt, h, |s| s.done = true);
            Err(e)
        }
    }
}

fn helper_return(rt: &mut Realm, c: &Call) -> JsResult {
    let h = helper_state(rt, c, false)?;
    let (done, running, rec, inner) = with_helper(rt, h, |s| (s.done, s.running, s.underlying, s.inner));
    if running {
        return Err(rt.type_error("Iterator helper is already running"));
    }
    with_helper(rt, h, |s| s.done = true);
    if !done {
        if let Some(i) = inner {
            let it = underlying_iter(rt, i);
            if let Some(ret) = rt.get_method(&it, &key("return"))? {
                let _ = rt.call(&ret, it, &[]);
            }
        }
        let it = underlying_iter(rt, rec);
        if let Some(ret) = rt.get_method(&it, &key("return"))? {
            rt.call(&ret, it, &[])?;
        }
    }
    Ok(rt.iter_result(Value::Undefined, true))
}

// ---------------------------------------------------------------- eager

/// reduce, toArray, forEach, some, every, find.
fn eager(rt: &mut Realm, c: &Call, which: u8, name: &str) -> JsResult {
    let o = this_iter(rt, c, name)?;
    let f = c.arg(0);
    if which != 1 && !rt.is_callable(&f) {
        let e = rt.type_error(&alloc::format!("Iterator.prototype.{name}: the argument is not a function"));
        return Err(close_with(rt, &o, e));
    }
    let rec = direct(rt, &o)?;
    let mut counter = 0.0;
    let mut acc = match which {
        0 if c.args.len() >= 2 => Some(c.arg(1)),
        0 => match rt.iter_step(rec)? {
            Some(v) => {
                counter = 1.0;
                Some(v)
            }
            None => return Err(rt.type_error("Reduce of empty iterator with no initial value")),
        },
        _ => None,
    };
    let mut items = Vec::new();
    while let Some(v) = rt.iter_step(rec)? {
        let r = match which {
            0 => rt.call(&f, Value::Undefined, &[acc.take().unwrap(), v.clone(), Value::Number(counter)]),
            1 => {
                items.push(v);
                counter += 1.0;
                continue;
            }
            _ => rt.call(&f, Value::Undefined, &[v.clone(), Value::Number(counter)]),
        };
        counter += 1.0;
        let r = match r {
            Ok(r) => r,
            Err(e) => return Err(close_with(rt, &o, e)),
        };
        let stop = match which {
            0 => {
                acc = Some(r);
                None
            }
            3 if r.truthy() => Some(Value::Bool(true)),
            4 if !r.truthy() => Some(Value::Bool(false)),
            5 if r.truthy() => Some(v),
            _ => None,
        };
        if let Some(result) = stop {
            if let Some(ret) = rt.get_method(&o, &key("return"))? {
                rt.call(&ret, o.clone(), &[])?;
            }
            return Ok(result);
        }
    }
    Ok(match which {
        0 => acc.unwrap(),
        1 => rt.array_from(items),
        3 => Value::Bool(false),
        4 => Value::Bool(true),
        _ => Value::Undefined,
    })
}

fn reduce(rt: &mut Realm, c: &Call) -> JsResult {
    eager(rt, c, 0, "reduce")
}

fn to_array(rt: &mut Realm, c: &Call) -> JsResult {
    eager(rt, c, 1, "toArray")
}

fn for_each(rt: &mut Realm, c: &Call) -> JsResult {
    eager(rt, c, 2, "forEach")
}

fn some(rt: &mut Realm, c: &Call) -> JsResult {
    eager(rt, c, 3, "some")
}

fn every(rt: &mut Realm, c: &Call) -> JsResult {
    eager(rt, c, 4, "every")
}

fn find(rt: &mut Realm, c: &Call) -> JsResult {
    eager(rt, c, 5, "find")
}

// ---------------------------------------------------------------- Iterator itself

fn iterator_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() || c.new_target == Value::Object(c.callee) {
        return Err(rt.type_error("Iterator is an abstract class and can't be constructed directly"));
    }
    let ip = rt.intr.iterator_proto;
    let proto = rt.proto_from_ctor(&c.new_target, ip)?;
    Ok(Value::Object(rt.new_object_with(Some(proto))))
}

fn from(rt: &mut Realm, c: &Call) -> JsResult {
    let o = c.arg(0);
    let iter = match &o {
        Value::String(_) | Value::Object(_) => match rt.get_method(&o, &PropKey::Sym(Sym::ITERATOR))? {
            Some(m) => {
                let it = rt.call(&m, o.clone(), &[])?;
                if !matches!(it, Value::Object(_)) {
                    return Err(rt.type_error("Result of the Symbol.iterator method is not an object"));
                }
                it
            }
            None => o.clone(),
        },
        _ => return Err(rt.type_error("Iterator.from requires an object or a string")),
    };
    let rec = direct(rt, &iter)?;
    // Already an Iterator: use it as it is.
    let ip = rt.intr.iterator_proto;
    if let Value::Object(io) = &iter {
        let mut p = rt.proto_of(*io);
        while let Some(x) = p {
            if x == ip {
                return Ok(iter);
            }
            p = rt.proto_of(x);
        }
    }
    let proto = rt.intr.wrap_for_valid_iterator_proto;
    Ok(Value::Object(rt.alloc(Obj::new(
        Some(proto),
        Kind::IterHelper(Box::new(IterHelper {
            kind: HelperKind::Wrap,
            underlying: rec,
            f: Value::Undefined,
            counter: 0.0,
            limit: 0.0,
            inner: None,
            done: false,
            running: false,
            started: false,
        })),
    ))))
}

fn wrap_next(rt: &mut Realm, c: &Call) -> JsResult {
    let h = helper_state(rt, c, true)?;
    let rec = with_helper(rt, h, |s| s.underlying);
    let (iter, next) = match &rt.heap.get(rec).kind {
        Kind::IterRecord(r) => (r.iter.clone(), r.next.clone()),
        _ => unreachable!(),
    };
    rt.call(&next, iter, &[])
}

fn wrap_return(rt: &mut Realm, c: &Call) -> JsResult {
    let h = helper_state(rt, c, true)?;
    let rec = with_helper(rt, h, |s| s.underlying);
    let it = underlying_iter(rt, rec);
    match rt.get_method(&it, &key("return"))? {
        Some(ret) => rt.call(&ret, it, &[]),
        None => Ok(rt.iter_result(Value::Undefined, true)),
    }
}

/// Iterator.prototype's `constructor` and @@toStringTag: accessors whose
/// setters define an own property on the receiver (never on the prototype).
fn tag_get(_rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::str("Iterator"))
}

fn ctor_get(rt: &mut Realm, _c: &Call) -> JsResult {
    rt.global_value("Iterator")
}

fn set_on_receiver(rt: &mut Realm, c: &Call, key: PropKey) -> JsResult {
    let Value::Object(o) = c.this else {
        return Err(rt.type_error("setter called on non-object"));
    };
    if o == rt.intr.iterator_proto {
        return Err(rt.type_error("Cannot assign to read only property of Iterator.prototype"));
    }
    if rt.has_own(o, &key) {
        rt.set(o, key, c.arg(0), Value::Object(o))?;
    } else {
        rt.create_data_property_or_throw(o, key, c.arg(0))?;
    }
    Ok(Value::Undefined)
}

fn tag_set(rt: &mut Realm, c: &Call) -> JsResult {
    set_on_receiver(rt, c, PropKey::Sym(Sym::TO_STRING_TAG))
}

fn ctor_set(rt: &mut Realm, c: &Call) -> JsResult {
    set_on_receiver(rt, c, key("constructor"))
}

pub fn init(rt: &mut Realm) {
    let ip = rt.intr.iterator_proto;
    let ctor = rt.native("Iterator", 0, iterator_ctor, true);
    rt.define(ctor, "prototype", Value::Object(ip), 0);
    rt.set_global("Iterator", Value::Object(ctor));
    rt.method(ctor, "from", 1, from);
    for (name, len, f) in [
        ("map", 1, map as NativeFn),
        ("filter", 1, filter),
        ("take", 1, take),
        ("drop", 1, drop),
        ("flatMap", 1, flat_map),
        ("reduce", 1, reduce),
        ("toArray", 0, to_array),
        ("forEach", 1, for_each),
        ("some", 1, some),
        ("every", 1, every),
        ("find", 1, find),
    ] {
        rt.method(ip, name, len, f);
    }
    let tg = rt.native("get [Symbol.toStringTag]", 0, tag_get, false);
    let ts = rt.native("set [Symbol.toStringTag]", 1, tag_set, false);
    rt.define_accessor(ip, Sym::TO_STRING_TAG, Value::Object(tg), Value::Object(ts), CONFIGURABLE);
    let cg = rt.native("get constructor", 0, ctor_get, false);
    let cs = rt.native("set constructor", 1, ctor_set, false);
    rt.define_accessor(ip, "constructor", Value::Object(cg), Value::Object(cs), CONFIGURABLE);

    let hp = rt.new_object_with(Some(ip));
    rt.intr.iterator_helper_proto = hp;
    rt.method(hp, "next", 0, helper_next);
    rt.method(hp, "return", 0, helper_return);
    to_string_tag(rt, hp, "Iterator Helper");

    let wp = rt.new_object_with(Some(ip));
    rt.intr.wrap_for_valid_iterator_proto = wp;
    rt.method(wp, "next", 0, wrap_next);
    rt.method(wp, "return", 0, wrap_return);
}
