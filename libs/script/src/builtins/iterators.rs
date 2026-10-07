//! The iterator prototypes, generators (sync and async), and the wrapper
//! that lets `for await` consume ordinary iterables.

use super::*;
use crate::vm::Resume;

fn return_this(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(c.this.clone())
}

fn this_generator(rt: &mut Realm, c: &Call, want_async: bool) -> Result<ObjRef, Value> {
    if let Value::Object(o) = c.this {
        if let Kind::Generator(g) = &rt.heap.get(o).kind {
            if g.is_async == want_async && g.promise.is_none() {
                return Ok(o);
            }
        }
    }
    Err(rt.type_error("next method called on an incompatible receiver"))
}

fn gen_state(rt: &Realm, g: ObjRef) -> GenState {
    match &rt.heap.get(g).kind {
        Kind::Generator(gs) => gs.state,
        _ => GenState::Done,
    }
}

fn set_gen_state(rt: &mut Realm, g: ObjRef, s: GenState) {
    if let Kind::Generator(gs) = &mut rt.heap.get_mut(g).kind {
        gs.state = s;
        if s == GenState::Done {
            gs.frame = None;
            gs.stack.clear();
        }
    }
}

/// A ReturnSignal's value, if `e` is one.
fn return_signal(rt: &Realm, e: &Value) -> Option<Value> {
    match e {
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::ReturnSignal(v) => Some(v.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn generator_resume(rt: &mut Realm, c: &Call, mode: Resume) -> JsResult {
    let g = this_generator(rt, c, false)?;
    let v = c.arg(0);
    match gen_state(rt, g) {
        GenState::Running => return Err(rt.type_error("Generator is already running")),
        GenState::Done => {
            return match mode {
                Resume::Next => Ok(rt.iter_result(Value::Undefined, true)),
                Resume::Return => Ok(rt.iter_result(v, true)),
                Resume::Throw => Err(v),
            }
        }
        GenState::Start if mode != Resume::Next => {
            set_gen_state(rt, g, GenState::Done);
            return match mode {
                Resume::Return => Ok(rt.iter_result(v, true)),
                _ => Err(v),
            };
        }
        _ => {}
    }
    let r = rt.resume(g, mode, v);
    let done = gen_state(rt, g) == GenState::Done;
    match r {
        Ok(v) => {
            if done {
                set_gen_state(rt, g, GenState::Done);
                Ok(rt.iter_result(v, true))
            } else if core::mem::take(&mut rt.raw_yield) {
                Ok(v)
            } else {
                Ok(rt.iter_result(v, false))
            }
        }
        Err(e) => {
            set_gen_state(rt, g, GenState::Done);
            match return_signal(rt, &e) {
                Some(v) => Ok(rt.iter_result(v, true)),
                None => Err(e),
            }
        }
    }
}

fn gen_next(rt: &mut Realm, c: &Call) -> JsResult {
    generator_resume(rt, c, Resume::Next)
}

fn gen_return(rt: &mut Realm, c: &Call) -> JsResult {
    generator_resume(rt, c, Resume::Return)
}

fn gen_throw(rt: &mut Realm, c: &Call) -> JsResult {
    generator_resume(rt, c, Resume::Throw)
}

// ---------------------------------------------------------------- async generators

fn async_gen_request(rt: &mut Realm, c: &Call, mode: u8) -> JsResult {
    let p = promise::new_promise(rt);
    let g = match this_generator(rt, c, true) {
        Ok(g) => g,
        Err(e) => {
            promise::reject(rt, p, e);
            return Ok(Value::Object(p));
        }
    };
    let (idle, awaiting) = match &mut rt.heap.get_mut(g).kind {
        Kind::Generator(gs) => {
            gs.queue.push((mode, c.arg(0), p));
            (gs.queue.len() == 1 && gs.state != GenState::Running, gs.awaiting)
        }
        _ => unreachable!(),
    };
    if idle && !awaiting {
        async_gen_drain(rt, g);
    }
    Ok(Value::Object(p))
}

fn front(rt: &Realm, g: ObjRef) -> Option<(u8, Value, ObjRef)> {
    match &rt.heap.get(g).kind {
        Kind::Generator(gs) => gs.queue.first().cloned(),
        _ => None,
    }
}

fn pop_front(rt: &mut Realm, g: ObjRef) {
    if let Kind::Generator(gs) = &mut rt.heap.get_mut(g).kind {
        if !gs.queue.is_empty() {
            gs.queue.remove(0);
        }
    }
}

/// Serves queued requests until the generator suspends at an await or yield
/// with no requests left.
fn async_gen_drain(rt: &mut Realm, g: ObjRef) {
    while let Some((mode, v, p)) = front(rt, g) {
        let mode = match mode {
            0 => Resume::Next,
            1 => Resume::Throw,
            _ => Resume::Return,
        };
        let state = gen_state(rt, g);
        if state == GenState::Start && mode != Resume::Next {
            set_gen_state(rt, g, GenState::Done);
        }
        match gen_state(rt, g) {
            GenState::Done => {
                pop_front(rt, g);
                match mode {
                    Resume::Next => {
                        let r = rt.iter_result(Value::Undefined, true);
                        let _ = promise::resolve(rt, p, r);
                    }
                    Resume::Return => {
                        let r = rt.iter_result(v, true);
                        let _ = promise::resolve(rt, p, r);
                    }
                    Resume::Throw => promise::reject(rt, p, v),
                }
            }
            GenState::Running => return,
            _ => {
                let r = rt.resume(g, mode, v);
                async_gen_after_resume(rt, g, r);
                return;
            }
        }
    }
}

/// Settles the front request after the body yielded, returned or threw.
pub fn async_gen_after_resume(rt: &mut Realm, g: ObjRef, r: JsResult) {
    let awaiting = matches!(&rt.heap.get(g).kind, Kind::Generator(gs) if gs.awaiting);
    if awaiting && r.is_ok() {
        return;
    }
    let Some((_, _, p)) = front(rt, g) else { return };
    pop_front(rt, g);
    let done = gen_state(rt, g) == GenState::Done;
    match r {
        Ok(v) => {
            let res = rt.iter_result(v, done);
            let _ = promise::resolve(rt, p, res);
        }
        Err(e) => {
            set_gen_state(rt, g, GenState::Done);
            match return_signal(rt, &e) {
                Some(v) => {
                    let res = rt.iter_result(v, true);
                    let _ = promise::resolve(rt, p, res);
                }
                None => promise::reject(rt, p, e),
            }
        }
    }
    async_gen_drain(rt, g);
}

fn async_gen_next(rt: &mut Realm, c: &Call) -> JsResult {
    async_gen_request(rt, c, 0)
}

fn async_gen_throw(rt: &mut Realm, c: &Call) -> JsResult {
    async_gen_request(rt, c, 1)
}

fn async_gen_return(rt: &mut Realm, c: &Call) -> JsResult {
    async_gen_request(rt, c, 2)
}

// ---------------------------------------------------------------- for await over sync iterables

pub fn get_async_iterator(rt: &mut Realm, v: &Value) -> Result<ObjRef, Value> {
    let method = rt.get_v(v, &PropKey::Sym(Sym::ASYNC_ITERATOR))?;
    if method.is_nullish() {
        let sync = rt.get_iterator(v)?;
        rt.iter_materialize(sync)?;
        let proto = rt.intr.async_from_sync_iterator_proto;
        let wrapper = rt.alloc(Obj::new(Some(proto), Kind::Wrapper(Value::Object(sync))));
        return rt.iter_record_from(Value::Object(wrapper));
    }
    if !rt.is_callable(&method) {
        return Err(rt.type_error("Symbol.asyncIterator is not a function"));
    }
    let iter = rt.call(&method, v.clone(), &[])?;
    rt.iter_record_from(iter)
}

fn wrapped_record(rt: &mut Realm, c: &Call) -> Result<ObjRef, Value> {
    if let Value::Object(o) = c.this {
        if let Kind::Wrapper(Value::Object(r)) = rt.heap.get(o).kind {
            return Ok(r);
        }
    }
    Err(rt.type_error("not an async-from-sync iterator"))
}

/// Turns a sync iterator result into a promise of a result whose value is awaited.
fn continuation(rt: &mut Realm, result: Value) -> JsResult {
    let Value::Object(ro) = result else {
        return Err(rt.type_error("Iterator result is not an object"));
    };
    let done = rt.get(ro, &key("done"), result.clone())?.truthy();
    let value = rt.get(ro, &key("value"), result.clone())?;
    let ctor = Value::Object(rt.intr.promise_ctor);
    let vp = promise::promise_resolve(rt, &ctor, value)?;
    let unwrap = rt.native_with("", 1, unwrap_result, false, alloc::vec![Value::Bool(done)]);
    let then = rt.get_v(&vp, &key("then"))?;
    rt.call(&then, vp, &[Value::Object(unwrap)])
}

fn unwrap_result(rt: &mut Realm, c: &Call) -> JsResult {
    let done = rt.native_slots(c.callee)[0].truthy();
    Ok(rt.iter_result(c.arg(0), done))
}

fn rejected(rt: &mut Realm, e: Value) -> JsResult {
    let p = promise::new_promise(rt);
    promise::reject(rt, p, e);
    Ok(Value::Object(p))
}

fn afs_next(rt: &mut Realm, c: &Call) -> JsResult {
    let rec = wrapped_record(rt, c)?;
    let (next, iter) = match &rt.heap.get(rec).kind {
        Kind::IterRecord(r) => (r.next.clone(), r.iter.clone()),
        _ => unreachable!(),
    };
    let args: Vec<Value> = c.args.iter().take(1).cloned().collect();
    match rt.call(&next, iter, &args).and_then(|r| continuation(rt, r)) {
        Ok(p) => Ok(p),
        Err(e) => rejected(rt, e),
    }
}

fn afs_forward(rt: &mut Realm, c: &Call, name: &str) -> JsResult {
    let rec = wrapped_record(rt, c)?;
    let iter = match &rt.heap.get(rec).kind {
        Kind::IterRecord(r) => r.iter.clone(),
        _ => unreachable!(),
    };
    let r = (|| -> JsResult {
        match rt.get_method(&iter, &key(name))? {
            Some(m) => {
                let args: Vec<Value> = c.args.iter().take(1).cloned().collect();
                let r = rt.call(&m, iter.clone(), &args)?;
                continuation(rt, r)
            }
            None => {
                if name == "return" {
                    let r = rt.iter_result(c.arg(0), true);
                    let p = promise::new_promise(rt);
                    promise::resolve(rt, p, r)?;
                    Ok(Value::Object(p))
                } else {
                    rt.iter_close(rec, true)?;
                    Err(rt.type_error("The iterator does not provide a 'throw' method"))
                }
            }
        }
    })();
    match r {
        Ok(p) => Ok(p),
        Err(e) => rejected(rt, e),
    }
}

fn afs_return(rt: &mut Realm, c: &Call) -> JsResult {
    afs_forward(rt, c, "return")
}

fn afs_throw(rt: &mut Realm, c: &Call) -> JsResult {
    afs_forward(rt, c, "throw")
}

pub fn init(rt: &mut Realm) {
    let ip = rt.intr.iterator_proto;
    rt.method_sym(ip, Sym::ITERATOR, "[Symbol.iterator]", 0, return_this);
    let aip = rt.intr.async_iterator_proto;
    rt.method_sym(aip, Sym::ASYNC_ITERATOR, "[Symbol.asyncIterator]", 0, return_this);

    let fp = rt.intr.function_proto;
    // Generators.
    let gen_proto = rt.new_object_with(Some(ip));
    rt.intr.generator_proto = gen_proto;
    rt.method(gen_proto, "next", 1, gen_next);
    rt.method(gen_proto, "return", 1, gen_return);
    rt.method(gen_proto, "throw", 1, gen_throw);
    to_string_tag(rt, gen_proto, "Generator");
    let gen_fn_proto = rt.new_object_with(Some(fp));
    rt.intr.generator_function_proto = gen_fn_proto;
    rt.define(gen_fn_proto, "prototype", Value::Object(gen_proto), CONFIGURABLE);
    rt.define(gen_proto, "constructor", Value::Object(gen_fn_proto), CONFIGURABLE);
    to_string_tag(rt, gen_fn_proto, "GeneratorFunction");

    // Async functions.
    let async_fn_proto = rt.new_object_with(Some(fp));
    rt.intr.async_function_proto = async_fn_proto;
    to_string_tag(rt, async_fn_proto, "AsyncFunction");

    // Async generators.
    let agen_proto = rt.new_object_with(Some(aip));
    rt.intr.async_generator_proto = agen_proto;
    rt.method(agen_proto, "next", 1, async_gen_next);
    rt.method(agen_proto, "return", 1, async_gen_return);
    rt.method(agen_proto, "throw", 1, async_gen_throw);
    to_string_tag(rt, agen_proto, "AsyncGenerator");
    let agen_fn_proto = rt.new_object_with(Some(fp));
    rt.intr.async_generator_function_proto = agen_fn_proto;
    rt.define(agen_fn_proto, "prototype", Value::Object(agen_proto), CONFIGURABLE);
    rt.define(agen_proto, "constructor", Value::Object(agen_fn_proto), CONFIGURABLE);
    to_string_tag(rt, agen_fn_proto, "AsyncGeneratorFunction");

    // Their (hidden) constructors, reachable through `.constructor`.
    let fctor = rt.intr.function_ctor;
    for (proto, name, kind) in [
        (gen_fn_proto, "GeneratorFunction", 2.0),
        (async_fn_proto, "AsyncFunction", 1.0),
        (agen_fn_proto, "AsyncGeneratorFunction", 3.0),
    ] {
        let c = rt.native_with(name, 1, function::function_ctor, true, alloc::vec![Value::Number(kind)]);
        rt.heap.get_mut(c).proto = Some(fctor);
        rt.define(c, "prototype", Value::Object(proto), 0);
        rt.define(proto, "constructor", Value::Object(c), CONFIGURABLE);
    }

    // for await over sync iterables.
    let afs = rt.new_object_with(Some(aip));
    rt.intr.async_from_sync_iterator_proto = afs;
    rt.method(afs, "next", 1, afs_next);
    rt.method(afs, "return", 1, afs_return);
    rt.method(afs, "throw", 1, afs_throw);
}
