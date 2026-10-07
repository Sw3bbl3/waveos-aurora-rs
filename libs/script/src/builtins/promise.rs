//! Promise, its jobs, and the machinery behind `await`.

use super::*;
use crate::realm::Job;
use crate::vm::Resume;

fn promise_data(rt: &Realm, p: ObjRef) -> Option<&Promise> {
    match &rt.heap.get(p).kind {
        Kind::Promise(pd) => Some(pd),
        _ => None,
    }
}

pub fn is_promise(rt: &Realm, v: &Value) -> bool {
    matches!(v, Value::Object(o) if promise_data(rt, *o).is_some())
}

pub fn new_promise(rt: &mut Realm) -> ObjRef {
    let proto = rt.intr.promise_proto;
    new_promise_with_proto(rt, proto)
}

fn new_promise_with_proto(rt: &mut Realm, proto: ObjRef) -> ObjRef {
    rt.alloc(Obj::new(
        Some(proto),
        Kind::Promise(Box::new(Promise {
            state: PromiseState::Pending,
            value: Value::Undefined,
            fulfill: Vec::new(),
            reject: Vec::new(),
            handled: false,
        })),
    ))
}

fn settle(rt: &mut Realm, p: ObjRef, value: Value, fulfilled: bool) {
    let reactions = match &mut rt.heap.get_mut(p).kind {
        Kind::Promise(pd) => {
            if pd.state != PromiseState::Pending {
                return;
            }
            pd.state = if fulfilled { PromiseState::Fulfilled } else { PromiseState::Rejected };
            pd.value = value.clone();
            let f = core::mem::take(&mut pd.fulfill);
            let r = core::mem::take(&mut pd.reject);
            if fulfilled {
                f
            } else {
                r
            }
        }
        _ => return,
    };
    if !fulfilled {
        let handled = promise_data(rt, p).is_some_and(|d| d.handled);
        if !handled {
            rt.unhandled.push(p);
        }
    }
    for r in reactions {
        rt.jobs.push_back(Job::Reaction {
            reaction_handler: r.handler,
            fulfill: fulfilled,
            capability: r.capability,
            argument: value.clone(),
        });
    }
}

pub fn reject(rt: &mut Realm, p: ObjRef, reason: Value) {
    settle(rt, p, reason, false);
}

/// The promise resolve function: adopts thenables.
pub fn resolve(rt: &mut Realm, p: ObjRef, v: Value) -> Result<(), Value> {
    if v == Value::Object(p) {
        let e = rt.type_error("Chaining cycle detected for promise");
        reject(rt, p, e);
        return Ok(());
    }
    let Value::Object(o) = v else {
        settle(rt, p, v, true);
        return Ok(());
    };
    let then = match rt.get(o, &key("then"), v.clone()) {
        Ok(t) => t,
        Err(e) => {
            reject(rt, p, e);
            return Ok(());
        }
    };
    if !rt.is_callable(&then) {
        settle(rt, p, v, true);
        return Ok(());
    }
    rt.jobs.push_back(Job::Thenable { promise: p, thenable: v, then });
    Ok(())
}

/// Resolve and reject functions sharing one "already resolved" flag.
fn resolving_functions(rt: &mut Realm, p: ObjRef) -> (Value, Value) {
    let flag = rt.alloc(Obj::new(None, Kind::Wrapper(Value::Bool(false))));
    let res = rt.native_with("", 1, resolve_fn, false, alloc::vec![Value::Object(p), Value::Object(flag)]);
    let rej = rt.native_with("", 1, reject_fn, false, alloc::vec![Value::Object(p), Value::Object(flag)]);
    (Value::Object(res), Value::Object(rej))
}

fn take_flag(rt: &mut Realm, c: &Call) -> Option<ObjRef> {
    let slots = rt.native_slots(c.callee).to_vec();
    let (Value::Object(p), Value::Object(flag)) = (&slots[0], &slots[1]) else { return None };
    if let Kind::Wrapper(Value::Bool(done)) = &mut rt.heap.get_mut(*flag).kind {
        if *done {
            return None;
        }
        *done = true;
    }
    Some(*p)
}

fn resolve_fn(rt: &mut Realm, c: &Call) -> JsResult {
    if let Some(p) = take_flag(rt, c) {
        resolve(rt, p, c.arg(0))?;
    }
    Ok(Value::Undefined)
}

fn reject_fn(rt: &mut Realm, c: &Call) -> JsResult {
    if let Some(p) = take_flag(rt, c) {
        reject(rt, p, c.arg(0));
    }
    Ok(Value::Undefined)
}

pub fn run_thenable(rt: &mut Realm, promise: ObjRef, thenable: Value, then: Value) -> Result<(), Value> {
    let (res, rej) = resolving_functions(rt, promise);
    if let Err(e) = rt.call(&then, thenable, &[res, rej.clone()]) {
        rt.call(&rej, Value::Undefined, &[e])?;
    }
    Ok(())
}

pub fn run_reaction(
    rt: &mut Realm,
    handler: Value,
    fulfill: bool,
    capability: Option<(Value, Value, Value)>,
    argument: Value,
) -> Result<(), Value> {
    let result = if handler.is_undefined() {
        if fulfill {
            Ok(argument)
        } else {
            Err(argument)
        }
    } else {
        rt.call(&handler, Value::Undefined, &[argument])
    };
    match capability {
        None => result.map(|_| ()),
        Some((_, res, rej)) => {
            match result {
                Ok(v) => rt.call(&res, Value::Undefined, &[v])?,
                Err(e) => rt.call(&rej, Value::Undefined, &[e])?,
            };
            Ok(())
        }
    }
}

/// PerformPromiseThen.
pub fn perform_then(
    rt: &mut Realm,
    p: ObjRef,
    on_fulfilled: Value,
    on_rejected: Value,
    capability: Option<(Value, Value, Value)>,
) {
    let on_fulfilled = if rt.is_callable(&on_fulfilled) { on_fulfilled } else { Value::Undefined };
    let on_rejected = if rt.is_callable(&on_rejected) { on_rejected } else { Value::Undefined };
    let (state, value, was_handled) = match &mut rt.heap.get_mut(p).kind {
        Kind::Promise(pd) => {
            let h = pd.handled;
            pd.handled = true;
            (pd.state, pd.value.clone(), h)
        }
        _ => return,
    };
    match state {
        PromiseState::Pending => {
            if let Kind::Promise(pd) = &mut rt.heap.get_mut(p).kind {
                pd.fulfill.push(Reaction { capability: capability.clone(), kind_fulfill: true, handler: on_fulfilled });
                pd.reject.push(Reaction { capability, kind_fulfill: false, handler: on_rejected });
            }
        }
        PromiseState::Fulfilled => {
            rt.jobs.push_back(Job::Reaction {
                reaction_handler: on_fulfilled,
                fulfill: true,
                capability,
                argument: value,
            });
        }
        PromiseState::Rejected => {
            if !was_handled {
                rt.unhandled.retain(|x| *x != p);
            }
            rt.jobs.push_back(Job::Reaction {
                reaction_handler: on_rejected,
                fulfill: false,
                capability,
                argument: value,
            });
        }
    }
}

/// NewPromiseCapability(C): (promise, resolve, reject).
pub fn new_capability(rt: &mut Realm, c: &Value) -> Result<(Value, Value, Value), Value> {
    if *c == Value::Object(rt.intr.promise_ctor) {
        let p = new_promise(rt);
        let (res, rej) = resolving_functions(rt, p);
        return Ok((Value::Object(p), res, rej));
    }
    if !rt.is_constructor(c) {
        return Err(rt.type_error("Promise capability target is not a constructor"));
    }
    let record = rt.new_object_with(None);
    let executor = rt.native_with("", 2, capability_executor, false, alloc::vec![Value::Object(record)]);
    let p = rt.construct(c, &[Value::Object(executor)], None)?;
    let res = rt.get(record, &key("resolve"), Value::Object(record))?;
    let rej = rt.get(record, &key("reject"), Value::Object(record))?;
    if !rt.is_callable(&res) || !rt.is_callable(&rej) {
        return Err(rt.type_error("Promise resolve or reject function is not callable"));
    }
    Ok((p, res, rej))
}

fn capability_executor(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(record) = rt.native_slots(c.callee)[0] else { unreachable!() };
    for (name, v) in [("resolve", c.arg(0)), ("reject", c.arg(1))] {
        let cur = rt.get(record, &key(name), Value::Object(record))?;
        if !cur.is_undefined() {
            return Err(rt.type_error("Promise executor has already been invoked"));
        }
        rt.define(record, name, v, DEFAULT);
    }
    Ok(Value::Undefined)
}

/// PromiseResolve(C, x).
pub fn promise_resolve(rt: &mut Realm, c: &Value, x: Value) -> JsResult {
    if let Value::Object(xo) = &x {
        if promise_data(rt, *xo).is_some() {
            let ctor = rt.get(*xo, &key("constructor"), x.clone())?;
            if ctor == *c {
                return Ok(x);
            }
        }
    }
    let (p, res, _) = new_capability(rt, c)?;
    rt.call(&res, Value::Undefined, &[x])?;
    Ok(p)
}

// ---------------------------------------------------------------- await

/// Arranges for coroutine `g` to resume when `v` settles.
pub fn await_value(rt: &mut Realm, g: ObjRef, v: Value) -> Result<(), Value> {
    let ctor = Value::Object(rt.intr.promise_ctor);
    let p = promise_resolve(rt, &ctor, v)?;
    let Value::Object(po) = p else { unreachable!() };
    if let Kind::Generator(gs) = &mut rt.heap.get_mut(g).kind {
        gs.awaiting = true;
    }
    let on_ok = rt.native_with("", 1, resume_next, false, alloc::vec![Value::Object(g)]);
    let on_err = rt.native_with("", 1, resume_throw, false, alloc::vec![Value::Object(g)]);
    perform_then(rt, po, Value::Object(on_ok), Value::Object(on_err), None);
    Ok(())
}

fn resume_next(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(g) = rt.native_slots(c.callee)[0] else { unreachable!() };
    resume_coroutine(rt, g, Resume::Next, c.arg(0));
    Ok(Value::Undefined)
}

fn resume_throw(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(g) = rt.native_slots(c.callee)[0] else { unreachable!() };
    resume_coroutine(rt, g, Resume::Throw, c.arg(0));
    Ok(Value::Undefined)
}

/// Resumes an async function or async generator after an await.
pub fn resume_coroutine(rt: &mut Realm, g: ObjRef, mode: Resume, v: Value) {
    if let Kind::Generator(gs) = &mut rt.heap.get_mut(g).kind {
        gs.awaiting = false;
    }
    let r = rt.resume(g, mode, v);
    let (promise, is_gen) = match &rt.heap.get(g).kind {
        Kind::Generator(gs) => (gs.promise, gs.promise.is_none()),
        _ => return,
    };
    if is_gen {
        super::iterators::async_gen_after_resume(rt, g, r);
    } else if let (Err(e), Some(p)) = (r, promise) {
        reject(rt, p, e);
    }
}

// ---------------------------------------------------------------- the API

fn promise_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Promise constructor cannot be invoked without 'new'"));
    }
    let executor = c.arg(0);
    if !rt.is_callable(&executor) {
        return Err(rt.type_error("Promise resolver is not a function"));
    }
    let pp = rt.intr.promise_proto;
    let proto = rt.proto_from_ctor(&c.new_target, pp)?;
    let p = new_promise_with_proto(rt, proto);
    let (res, rej) = resolving_functions(rt, p);
    if let Err(e) = rt.call(&executor, Value::Undefined, &[res, rej.clone()]) {
        rt.call(&rej, Value::Undefined, &[e])?;
    }
    Ok(Value::Object(p))
}

fn this_promise(rt: &mut Realm, c: &Call) -> Result<ObjRef, Value> {
    match &c.this {
        Value::Object(o) if promise_data(rt, *o).is_some() => Ok(*o),
        _ => Err(rt.type_error("Method Promise.prototype.then called on incompatible receiver")),
    }
}

fn species_ctor(rt: &mut Realm, o: ObjRef) -> JsResult {
    let default = Value::Object(rt.intr.promise_ctor);
    let c = rt.get(o, &key("constructor"), Value::Object(o))?;
    if c.is_undefined() {
        return Ok(default);
    }
    let Value::Object(co) = c else {
        return Err(rt.type_error("The constructor property is not an object"));
    };
    let s = rt.get(co, &PropKey::Sym(Sym::SPECIES), c.clone())?;
    if s.is_nullish() {
        return Ok(default);
    }
    Ok(s)
}

fn then(rt: &mut Realm, c: &Call) -> JsResult {
    let p = this_promise(rt, c)?;
    let ctor = species_ctor(rt, p)?;
    let cap = new_capability(rt, &ctor)?;
    let result = cap.0.clone();
    perform_then(rt, p, c.arg(0), c.arg(1), Some(cap));
    Ok(result)
}

fn catch(rt: &mut Realm, c: &Call) -> JsResult {
    let then = rt.get_v(&c.this, &key("then"))?;
    rt.call(&then, c.this.clone(), &[Value::Undefined, c.arg(0)])
}

fn finally(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(p) = c.this else {
        return Err(rt.type_error("Promise.prototype.finally called on a non-object"));
    };
    let ctor = species_ctor(rt, p)?;
    let on_finally = c.arg(0);
    let (a, b) = if !rt.is_callable(&on_finally) {
        (on_finally.clone(), on_finally)
    } else {
        let f = rt.native_with("", 1, finally_then, false, alloc::vec![on_finally.clone(), ctor.clone()]);
        let r = rt.native_with("", 1, finally_catch, false, alloc::vec![on_finally, ctor]);
        (Value::Object(f), Value::Object(r))
    };
    let then = rt.get_v(&c.this, &key("then"))?;
    rt.call(&then, c.this.clone(), &[a, b])
}

fn finally_then(rt: &mut Realm, c: &Call) -> JsResult {
    let slots = rt.native_slots(c.callee).to_vec();
    let r = rt.call(&slots[0], Value::Undefined, &[])?;
    let p = promise_resolve(rt, &slots[1], r)?;
    let value_thunk = rt.native_with("", 0, return_slot, false, alloc::vec![c.arg(0)]);
    let then = rt.get_v(&p, &key("then"))?;
    rt.call(&then, p, &[Value::Object(value_thunk)])
}

fn finally_catch(rt: &mut Realm, c: &Call) -> JsResult {
    let slots = rt.native_slots(c.callee).to_vec();
    let r = rt.call(&slots[0], Value::Undefined, &[])?;
    let p = promise_resolve(rt, &slots[1], r)?;
    let thrower = rt.native_with("", 0, throw_slot, false, alloc::vec![c.arg(0)]);
    let then = rt.get_v(&p, &key("then"))?;
    rt.call(&then, p, &[Value::Object(thrower)])
}

fn return_slot(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(rt.native_slots(c.callee)[0].clone())
}

fn throw_slot(rt: &mut Realm, c: &Call) -> JsResult {
    Err(rt.native_slots(c.callee)[0].clone())
}

fn static_resolve(rt: &mut Realm, c: &Call) -> JsResult {
    if !matches!(c.this, Value::Object(_)) {
        return Err(rt.type_error("Promise.resolve called on a non-object"));
    }
    promise_resolve(rt, &c.this, c.arg(0))
}

fn static_reject(rt: &mut Realm, c: &Call) -> JsResult {
    let (p, _, rej) = new_capability(rt, &c.this)?;
    rt.call(&rej, Value::Undefined, &[c.arg(0)])?;
    Ok(p)
}

fn with_resolvers(rt: &mut Realm, c: &Call) -> JsResult {
    let (p, res, rej) = new_capability(rt, &c.this)?;
    let o = rt.new_object();
    rt.define(o, "promise", p, DEFAULT);
    rt.define(o, "resolve", res, DEFAULT);
    rt.define(o, "reject", rej, DEFAULT);
    Ok(Value::Object(o))
}

/// Shared state for Promise.all/allSettled/any: (values, remaining, resolve/reject).
fn combinator(rt: &mut Realm, c: &Call, kind: u8) -> JsResult {
    let ctor = c.this.clone();
    let (p, res, rej) = new_capability(rt, &ctor)?;
    let result = (|| -> Result<(), Value> {
        let resolve = rt.get_v(&ctor, &key("resolve"))?;
        require_callable(rt, &resolve, "Promise.resolve")?;
        let items = rt.iterate_to_vec(&c.arg(0))?;
        let values = rt.new_array(alloc::vec![Value::Undefined; items.len()]);
        // remaining starts at 1 and drops after the loop (as in the spec).
        let state = rt.new_object_with(None);
        rt.define(state, "remaining", Value::Number(items.len() as f64 + 1.0), DEFAULT);
        let n = items.len();
        for (i, item) in items.into_iter().enumerate() {
            let next = rt.call(&resolve, ctor.clone(), &[item])?;
            let slots = |rt: &mut Realm, which: u8| {
                let flag = rt.alloc(Obj::new(None, Kind::Wrapper(Value::Bool(false))));
                alloc::vec![
                    Value::Object(values),
                    Value::Number(i as f64),
                    Value::Object(state),
                    res.clone(),
                    rej.clone(),
                    Value::Object(flag),
                    Value::Number(which as f64),
                ]
            };
            let (on_ok, on_err) = match kind {
                // all: collect values, reject on first error
                0 => (
                    Value::Object({
                        let sl = slots(rt, 0);
                        rt.native_with("", 1, element, false, sl)
                    }),
                    rej.clone(),
                ),
                // allSettled: collect both
                1 => (
                    Value::Object({
                        let sl = slots(rt, 1);
                        rt.native_with("", 1, element, false, sl)
                    }),
                    Value::Object({
                        let sl = slots(rt, 2);
                        rt.native_with("", 1, element, false, sl)
                    }),
                ),
                // any: collect errors, resolve on first value
                2 => (
                    res.clone(),
                    Value::Object({
                        let sl = slots(rt, 3);
                        rt.native_with("", 1, element, false, sl)
                    }),
                ),
                // race
                _ => (res.clone(), rej.clone()),
            };
            let then = rt.get_v(&next, &key("then"))?;
            rt.call(&then, next, &[on_ok, on_err])?;
        }
        if kind <= 2 {
            finish_element(rt, values, state, &res, &rej, kind == 2, n)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        rt.call(&rej, Value::Undefined, &[e])?;
    }
    Ok(p)
}

fn finish_element(
    rt: &mut Realm,
    values: ObjRef,
    state: ObjRef,
    res: &Value,
    rej: &Value,
    any: bool,
    _n: usize,
) -> Result<(), Value> {
    let Value::Number(r) = rt.get(state, &key("remaining"), Value::Object(state))? else { return Ok(()) };
    let r = r - 1.0;
    rt.define(state, "remaining", Value::Number(r), DEFAULT);
    if r == 0.0 {
        if any {
            let ctor = Value::Object(rt.intr.error_ctors[crate::realm::ErrorKind::AggregateError as usize]);
            let err = rt.construct(&ctor, &[Value::Object(values), Value::str("All promises were rejected")], None)?;
            rt.call(rej, Value::Undefined, &[err])?;
        } else {
            rt.call(res, Value::Undefined, &[Value::Object(values)])?;
        }
    }
    Ok(())
}

fn element(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.native_slots(c.callee).to_vec();
    let Value::Object(flag) = s[5] else { unreachable!() };
    if let Kind::Wrapper(Value::Bool(done)) = &mut rt.heap.get_mut(flag).kind {
        if *done {
            return Ok(Value::Undefined);
        }
        *done = true;
    }
    let (Value::Object(values), Value::Number(i), Value::Object(state), Value::Number(which)) =
        (&s[0], &s[1], &s[2], &s[6])
    else {
        unreachable!()
    };
    let v = match *which as u8 {
        1 | 2 => {
            let o = rt.new_object();
            let fulfilled = *which as u8 == 1;
            rt.define(o, "status", Value::str(if fulfilled { "fulfilled" } else { "rejected" }), DEFAULT);
            rt.define(o, if fulfilled { "value" } else { "reason" }, c.arg(0), DEFAULT);
            Value::Object(o)
        }
        _ => c.arg(0),
    };
    rt.create_data_property(*values, PropKey::index(*i as u32), v)?;
    finish_element(rt, *values, *state, &s[3], &s[4], *which as u8 == 3, 0)?;
    Ok(Value::Undefined)
}

fn all(rt: &mut Realm, c: &Call) -> JsResult {
    combinator(rt, c, 0)
}

fn all_settled(rt: &mut Realm, c: &Call) -> JsResult {
    combinator(rt, c, 1)
}

fn any(rt: &mut Realm, c: &Call) -> JsResult {
    combinator(rt, c, 2)
}

fn race(rt: &mut Realm, c: &Call) -> JsResult {
    combinator(rt, c, 3)
}

fn species(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(c.this.clone())
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let proto = rt.new_object_with(Some(op));
    rt.intr.promise_proto = proto;
    let ctor = constructor(rt, "Promise", 1, promise_ctor, proto);
    rt.intr.promise_ctor = ctor;
    rt.method(proto, "then", 2, then);
    rt.method(proto, "catch", 1, catch);
    rt.method(proto, "finally", 1, finally);
    to_string_tag(rt, proto, "Promise");
    rt.method(ctor, "resolve", 1, static_resolve);
    rt.method(ctor, "reject", 1, static_reject);
    rt.method(ctor, "all", 1, all);
    rt.method(ctor, "allSettled", 1, all_settled);
    rt.method(ctor, "any", 1, any);
    rt.method(ctor, "race", 1, race);
    rt.method(ctor, "withResolvers", 0, with_resolvers);
    let sp = rt.native("get [Symbol.species]", 0, species, false);
    rt.define_accessor(ctor, Sym::SPECIES, Value::Object(sp), Value::Undefined, CONFIGURABLE);
}
