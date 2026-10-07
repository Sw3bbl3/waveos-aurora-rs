//! The interpreter: a loop over bytecode with an explicit frame stack.
//!
//! Calls from JavaScript to JavaScript push frames without recursing in
//! Rust. Native code that calls back into JavaScript (and generators, which
//! resume their saved frame) runs a nested loop up to a "boundary" frame.
//! Generators and async functions suspend by moving their frame and its
//! slice of the stack into the generator object.

use crate::bytecode::{Code, CodeKind, Const, Op, GLOBAL_LEXICAL};
use crate::heap::ObjRef;
use crate::numconv;
use crate::object::*;
use crate::realm::{ErrorKind, JsResult, Realm};
use crate::value::{JsStr, PropKey, Sym, Value};
use alloc::boxed::Box;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug)]
pub struct Handler {
    pub pc: u32,
    /// Operand stack height relative to the frame base.
    pub sp: u32,
    pub scope: Option<ObjRef>,
    pub finally: bool,
}

/// Where a new frame's arguments are.
enum Args {
    Owned(Vec<Value>),
    /// On the stack from this index, with the callee and `this` just below.
    OnStack(usize),
}

/// Where a global name lives (see [`Code::global_cache`]).
#[derive(Clone, Copy)]
enum GlobalAt {
    Lexical(usize),
    Property(usize),
}

pub struct Frame {
    pub code: Rc<Code>,
    pub func: Value,
    pub pc: usize,
    pub base: usize,
    /// The stack height to go back to when the frame ends (below `base`
    /// when the callee and `this` were left under the arguments).
    pub floor: usize,
    pub this: Value,
    pub new_target: Value,
    pub scope: Option<ObjRef>,
    pub handlers: Vec<Handler>,
    pub args: Vec<Value>,
    /// Return to the Rust caller when this frame finishes.
    pub boundary: bool,
    pub construct: bool,
    pub generator: Option<ObjRef>,
    /// Set when resuming a `yield*`: 0 next, 1 throw, 2 return.
    pub resume_mode: u8,
}

impl Frame {
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        let mut v = |x: &Value| {
            if let Value::Object(o) = x {
                out.push(*o);
            }
        };
        v(&self.func);
        v(&self.this);
        v(&self.new_target);
        for a in &self.args {
            v(a);
        }
        out.extend(self.scope);
        for h in &self.handlers {
            out.extend(h.scope);
        }
        out.extend(self.generator);
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Resume {
    Next,
    Throw,
    Return,
}

impl Realm {
    // ------------------------------------------------------------ entry points

    pub(crate) fn call_code(&mut self, code: Rc<Code>, this: Value) -> JsResult {
        self.check_frames()?;
        let base = self.stack.len();
        for _ in 0..code.n_locals {
            self.stack.push(Value::Undefined);
        }
        self.frames.push(Frame {
            code,
            func: Value::Undefined,
            pc: 0,
            base,
            floor: base,
            this,
            new_target: Value::Undefined,
            scope: None,
            handlers: Vec::new(),
            args: Vec::new(),
            boundary: true,
            construct: false,
            generator: None,
            resume_mode: 0,
        });
        self.run(None)
    }

    pub(crate) fn call_closure(&mut self, f: ObjRef, this: Value, args: Vec<Value>, new_target: Value) -> JsResult {
        match self.push_frame(f, this, args, new_target, false, true)? {
            Some(v) => Ok(v),
            None => self.run(None),
        }
    }

    /// Calls closure `f` (not a generator or async function) from native
    /// code, its arguments copied straight onto the stack.
    pub(crate) fn call_closure_slice(&mut self, f: ObjRef, this: Value, args: &[Value]) -> JsResult {
        self.stack.push(Value::Object(f));
        self.stack.push(this.clone());
        let start = self.stack.len();
        self.stack.extend_from_slice(args);
        match self.push_frame_from(f, this, Args::OnStack(start), Value::Undefined, false, true) {
            Ok(Some(v)) => Ok(v),
            Ok(None) => self.run(None),
            Err(e) => {
                self.stack.truncate(start - 2);
                Err(e)
            }
        }
    }

    pub(crate) fn construct_closure(&mut self, f: ObjRef, args: Vec<Value>, new_target: Value) -> JsResult {
        match self.push_frame(f, Value::Undefined, args, new_target, true, true)? {
            Some(v) => Ok(v),
            None => self.run(None),
        }
    }

    /// Pushes a frame for closure `f`. Returns Some(result) when the call
    /// completed already (generators and async functions), else None.
    fn push_frame(
        &mut self,
        f: ObjRef,
        this: Value,
        args: Vec<Value>,
        new_target: Value,
        construct: bool,
        boundary: bool,
    ) -> Result<Option<Value>, Value> {
        self.push_frame_from(f, this, Args::Owned(args), new_target, construct, boundary)
    }

    fn push_frame_from(
        &mut self,
        f: ObjRef,
        this: Value,
        args: Args,
        new_target: Value,
        construct: bool,
        boundary: bool,
    ) -> Result<Option<Value>, Value> {
        self.check_frames()?;
        self.tick()?;
        let (code, scope, cthis, cnt) = match &self.heap.get(f).kind {
            Kind::Function(c) => (c.code.clone(), c.scope, c.this.clone(), c.new_target.clone()),
            _ => unreachable!(),
        };
        if !construct && matches!(code.kind, CodeKind::ClassConstructor | CodeKind::DerivedConstructor) {
            let n = code.name.to_rust();
            return Err(self.type_error(&format!("Class constructor {n} cannot be invoked without 'new'")));
        }
        let (this, new_target) = if code.kind == CodeKind::Arrow {
            (cthis, cnt)
        } else if construct {
            if code.kind == CodeKind::DerivedConstructor {
                (Value::Empty, new_target)
            } else {
                let op = self.intr.object_proto;
                let proto = self.proto_from_ctor(&new_target, op)?;
                (Value::Object(self.new_object_with(Some(proto))), new_target)
            }
        } else if code.strict {
            (this, Value::Undefined)
        } else {
            let t = match this {
                Value::Undefined | Value::Null | Value::Empty => Value::Object(self.global),
                Value::Object(_) => this,
                prim => Value::Object(self.to_object(&prim)?),
            };
            (t, Value::Undefined)
        };
        let n_params = code.n_params as usize;
        let n_locals = code.n_locals as usize;
        let (base, floor, args) = match args {
            Args::Owned(args) => {
                let base = self.stack.len();
                for i in 0..n_locals {
                    self.stack.push(if i < n_params {
                        args.get(i).cloned().unwrap_or(Value::Undefined)
                    } else {
                        Value::Undefined
                    });
                }
                (base, base, args)
            }
            // The arguments become the parameters where they are; the
            // callee and `this` below them go when the frame ends.
            Args::OnStack(start) => {
                let kept = if code.keep_args { self.stack[start..].to_vec() } else { Vec::new() };
                let argc = self.stack.len() - start;
                self.stack.truncate(start + argc.min(n_params));
                self.stack.resize(start + n_locals, Value::Undefined);
                (start, start - 2, kept)
            }
        };
        let is_gen = code.is_generator;
        let is_async = code.is_async;
        let mut frame = Frame {
            code: code.clone(),
            func: Value::Object(f),
            pc: 0,
            base,
            floor,
            this,
            new_target,
            scope,
            handlers: Vec::new(),
            args: if code.keep_args { args } else { Vec::new() },
            boundary,
            construct,
            generator: None,
            resume_mode: 0,
        };
        if is_gen || is_async {
            // A coroutine: run its prologue now, in a nested loop.
            frame.boundary = true;
            let proto = if is_gen {
                let default = if is_async { self.intr.async_generator_proto } else { self.intr.generator_proto };
                let p = self.get(f, &self.names.prototype.clone(), Value::Object(f))?;
                Some(match p {
                    Value::Object(p) => p,
                    _ => default,
                })
            } else {
                None
            };
            let promise = if is_async && !is_gen { Some(crate::builtins::promise::new_promise(self)) } else { None };
            let gen = self.alloc(Obj::new(
                proto,
                Kind::Generator(Box::new(Generator {
                    state: GenState::Running,
                    frame: None,
                    stack: Vec::new(),
                    promise,
                    is_async,
                    queue: Vec::new(),
                    awaiting: false,
                })),
            ));
            frame.generator = Some(gen);
            self.frames.push(frame);
            let r = self.run(None);
            if let Some(p) = promise {
                // An async function: its promise settles when the body ends.
                match r {
                    Ok(_) => {}
                    Err(e) => crate::builtins::promise::reject(self, p, e),
                }
                return Ok(Some(Value::Object(p)));
            }
            r?;
            return Ok(Some(Value::Object(gen)));
        }
        self.frames.push(frame);
        Ok(None)
    }

    /// Resumes a suspended generator or async function. Returns what it
    /// yielded or returned (the generator's state tells which).
    pub(crate) fn resume(&mut self, gen: ObjRef, mode: Resume, value: Value) -> JsResult {
        let (mut frame, saved, was_start) = match &mut self.heap.get_mut(gen).kind {
            Kind::Generator(g) => {
                let was_start = g.state == GenState::Start;
                g.state = GenState::Running;
                (g.frame.take().expect("generator has no frame"), core::mem::take(&mut g.stack), was_start)
            }
            _ => unreachable!(),
        };
        self.check_frames()?;
        frame.base = self.stack.len();
        frame.floor = frame.base;
        frame.boundary = true;
        self.stack.extend(saved);
        let at_delegate = matches!(frame.code.ops.get(frame.pc), Some(Op::YieldDelegate(_)));
        self.frames.push(frame);
        if at_delegate {
            self.stack.push(value);
            self.frames.last_mut().unwrap().resume_mode = mode as u8;
            return self.run(None);
        }
        match mode {
            Resume::Next => {
                if !was_start {
                    self.stack.push(value);
                }
                self.run(None)
            }
            Resume::Throw => self.run(Some(value)),
            Resume::Return => {
                let sig = self.alloc(Obj::new(None, Kind::ReturnSignal(value)));
                self.run(Some(Value::Object(sig)))
            }
        }
    }

    // ------------------------------------------------------------ the loop

    /// Runs frames until the innermost boundary frame finishes (or a
    /// coroutine suspends). `pending`: an exception to throw first.
    pub(crate) fn run(&mut self, pending: Option<Value>) -> JsResult {
        self.enter_run()?;
        let mut exc = pending;
        let r = loop {
            if let Some(e) = exc.take() {
                if let Err(e) = self.unwind(e) {
                    break Err(e);
                }
            }
            match self.exec() {
                Ok(v) => break Ok(v),
                Err(e) => exc = Some(e),
            }
        };
        self.run_depth -= 1;
        r
    }

    /// Finds a handler for `exc`: Ok if one was found (execution continues
    /// there), Err when the exception leaves the boundary frame.
    fn unwind(&mut self, exc: Value) -> Result<(), Value> {
        let is_return = matches!(&exc, Value::Object(o) if matches!(self.heap.get(*o).kind, Kind::ReturnSignal(_)));
        loop {
            let interrupted = self.interrupted;
            let Some(frame) = self.frames.last_mut() else { return Err(exc) };
            if !interrupted {
                while let Some(h) = frame.handlers.pop() {
                    if is_return && !h.finally {
                        continue;
                    }
                    let base = frame.base;
                    frame.scope = h.scope;
                    frame.pc = h.pc as usize;
                    self.stack.truncate(base + h.sp as usize);
                    self.stack.push(exc);
                    return Ok(());
                }
            }
            let frame = self.frames.pop().unwrap();
            self.stack.truncate(frame.floor);
            if let Some(g) = frame.generator {
                if let Kind::Generator(gs) = &mut self.heap.get_mut(g).kind {
                    gs.state = GenState::Done;
                }
            }
            if frame.boundary {
                return Err(exc);
            }
        }
    }

    fn pop(&mut self) -> Value {
        self.stack.pop().unwrap()
    }

    fn peek(&self, n: usize) -> &Value {
        &self.stack[self.stack.len() - 1 - n]
    }

    fn frame(&self) -> &Frame {
        self.frames.last().unwrap()
    }

    fn frame_mut(&mut self) -> &mut Frame {
        self.frames.last_mut().unwrap()
    }

    fn cstr(&self, i: u32) -> JsStr {
        self.frame().code.str_const(i).clone()
    }

    fn ckey(&self, i: u32) -> PropKey {
        PropKey::Str(self.cstr(i))
    }

    fn strict(&self) -> bool {
        self.frame().code.strict
    }

    /// Counts one unit of work against the step budget.
    pub fn tick(&mut self) -> Result<(), Value> {
        self.steps += 1;
        if self.step_limit != 0 && self.steps > self.step_limit {
            self.interrupted = true;
            return Err(self.range_error("Script took too long and was stopped"));
        }
        if self.interrupted {
            return Err(self.range_error("Script took too long and was stopped"));
        }
        Ok(())
    }

    fn scope_at(&self, depth: u16) -> ObjRef {
        let mut s = self.frame().scope.expect("no scope");
        for _ in 0..depth {
            s = match &self.heap.get(s).kind {
                Kind::Scope(_, p) => p.expect("scope chain too short"),
                _ => unreachable!(),
            };
        }
        s
    }

    fn scope_get(&self, depth: u16, i: u16) -> Value {
        let s = self.scope_at(depth);
        match &self.heap.get(s).kind {
            Kind::Scope(v, _) => v[i as usize].clone(),
            _ => unreachable!(),
        }
    }

    fn scope_set(&mut self, depth: u16, i: u16, v: Value) {
        let s = self.scope_at(depth);
        if let Kind::Scope(vars, _) = &mut self.heap.get_mut(s).kind {
            vars[i as usize] = v;
        }
    }

    fn tdz_error(&mut self, name: u32) -> Value {
        let n = self.cstr(name).to_rust();
        self.reference_error(&format!("Cannot access '{n}' before initialization"))
    }

    /// The current function's home object (for `super`).
    fn home_object(&self) -> Option<ObjRef> {
        match &self.frame().func {
            Value::Object(f) => match &self.heap.get(*f).kind {
                Kind::Function(c) => c.home,
                _ => None,
            },
            _ => None,
        }
    }

    fn this_value(&mut self) -> JsResult {
        let t = self.frame().this.clone();
        if t == Value::Empty {
            return Err(self.reference_error("Must call super constructor in derived class before accessing 'this'"));
        }
        Ok(t)
    }

    /// Executes until the boundary frame returns (Ok) or an exception is
    /// thrown (Err, to be unwound by the caller).
    fn exec(&mut self) -> JsResult {
        loop {
            let (op, base) = {
                let f = self.frames.last_mut().unwrap();
                let op = f.code.ops[f.pc];
                f.pc += 1;
                (op, f.base)
            };
            match op {
                Op::Undef => self.stack.push(Value::Undefined),
                Op::Null => self.stack.push(Value::Null),
                Op::True => self.stack.push(Value::Bool(true)),
                Op::False => self.stack.push(Value::Bool(false)),
                Op::Int(i) => self.stack.push(Value::Number(i as f64)),
                Op::Empty => self.stack.push(Value::Empty),
                Op::Const(i) => {
                    let v = match &self.frame().code.consts[i as usize] {
                        Const::Num(n) => Value::Number(*n),
                        Const::Str(s) => Value::String(s.clone()),
                        _ => unreachable!(),
                    };
                    self.stack.push(v);
                }
                Op::Pop => {
                    self.stack.pop();
                }
                Op::Dup => {
                    let v = self.peek(0).clone();
                    self.stack.push(v);
                }
                Op::Dup2 => {
                    let a = self.peek(1).clone();
                    let b = self.peek(0).clone();
                    self.stack.push(a);
                    self.stack.push(b);
                }
                Op::Swap => {
                    let n = self.stack.len();
                    self.stack.swap(n - 1, n - 2);
                }
                Op::Rot3 => {
                    let v = self.pop();
                    let n = self.stack.len();
                    self.stack.insert(n - 2, v);
                }
                Op::Rot4 => {
                    let v = self.pop();
                    let n = self.stack.len();
                    self.stack.insert(n - 3, v);
                }
                Op::Nip => {
                    let v = self.pop();
                    *self.stack.last_mut().unwrap() = v;
                }
                Op::Pick(n) => {
                    let v = self.peek(n as usize).clone();
                    self.stack.push(v);
                }
                Op::RequireCoercible => {
                    if self.peek(0).is_nullish() {
                        let v = if matches!(self.peek(0), Value::Null) { "null" } else { "undefined" };
                        return Err(self.type_error(&format!("Cannot destructure '{v}' as it is {v}.")));
                    }
                }
                Op::GetLocal(i) => {
                    let v = self.stack[base + i as usize].clone();
                    self.stack.push(v);
                }
                Op::SetLocal(i) => {
                    let v = self.peek(0).clone();
                    self.stack[base + i as usize] = v;
                }
                Op::GetLocalChecked(i, name) => {
                    let v = self.stack[base + i as usize].clone();
                    if v == Value::Empty {
                        return Err(self.tdz_error(name));
                    }
                    self.stack.push(v);
                }
                Op::CheckLocal(i, name) => {
                    if self.stack[base + i as usize] == Value::Empty {
                        return Err(self.tdz_error(name));
                    }
                }
                Op::GetScope(d, i) => {
                    let v = self.scope_get(d, i);
                    self.stack.push(v);
                }
                Op::SetScope(d, i) => {
                    let v = self.peek(0).clone();
                    self.scope_set(d, i, v);
                }
                Op::GetScopeChecked(d, i, name) => {
                    let v = self.scope_get(d, i);
                    if v == Value::Empty {
                        return Err(self.tdz_error(name));
                    }
                    self.stack.push(v);
                }
                Op::CheckScope(d, i, name) => {
                    if self.scope_get(d, i) == Value::Empty {
                        return Err(self.tdz_error(name));
                    }
                }
                Op::PushScope(n) => {
                    let parent = self.frame().scope;
                    let s = self.alloc(Obj::new(None, Kind::Scope(vec![Value::Empty; n as usize], parent)));
                    self.frame_mut().scope = Some(s);
                }
                Op::PopScope => {
                    let s = self.frame().scope.unwrap();
                    let parent = match &self.heap.get(s).kind {
                        Kind::Scope(_, p) => *p,
                        _ => unreachable!(),
                    };
                    self.frame_mut().scope = parent;
                }
                Op::CopyScope => {
                    let s = self.frame().scope.unwrap();
                    let (vars, parent) = match &self.heap.get(s).kind {
                        Kind::Scope(v, p) => (v.clone(), *p),
                        _ => unreachable!(),
                    };
                    let n = self.alloc(Obj::new(None, Kind::Scope(vars, parent)));
                    self.frame_mut().scope = Some(n);
                }
                Op::GetGlobal(name) => {
                    let v = self.get_global(name)?;
                    self.stack.push(v);
                }
                Op::SetGlobal(name) => {
                    let v = self.peek(0).clone();
                    self.set_global_binding(name, v)?;
                }
                Op::TypeofGlobal(name) => {
                    let key = self.cstr(name);
                    let v = match self.lexicals.get(&key).map(|&i| &self.lex[i as usize].0) {
                        Some(Value::Empty) => return Err(self.tdz_error(name)),
                        Some(v) => v.clone(),
                        None => {
                            let g = self.global;
                            let k = PropKey::Str(key);
                            if self.has_property(g, &k)? {
                                self.get(g, &k, Value::Object(g))?
                            } else {
                                Value::Undefined
                            }
                        }
                    };
                    let t = self.typeof_str(&v);
                    self.stack.push(Value::str(t));
                }
                Op::DeclareVar(name) => {
                    let key = self.cstr(name);
                    if self.lexicals.contains_key(&key) {
                        let n = key.to_rust();
                        return Err(
                            self.error(ErrorKind::SyntaxError, &format!("Identifier '{n}' has already been declared"))
                        );
                    }
                    let g = self.global;
                    let k = PropKey::Str(key);
                    if !self.has_own(g, &k) {
                        self.define(g, k, Value::Undefined, WRITABLE | ENUMERABLE);
                    }
                }
                Op::DeclareFunction(name) => {
                    let f = self.pop();
                    let key = self.cstr(name);
                    let g = self.global;
                    let k = PropKey::Str(key);
                    match self.get_own_property(g, &k) {
                        Some(p) if !p.configurable() => {
                            self.set(g, k, f, Value::Object(g))?;
                        }
                        _ => self.define(g, k, f, WRITABLE | ENUMERABLE),
                    }
                }
                Op::DeclareLexical(name, is_const) => {
                    let key = self.cstr(name);
                    let g = self.global;
                    let exists = self.lexicals.contains_key(&key)
                        || self.get_own_property(g, &PropKey::Str(key.clone())).is_some_and(|p| !p.configurable());
                    if exists {
                        let n = key.to_rust();
                        return Err(
                            self.error(ErrorKind::SyntaxError, &format!("Identifier '{n}' has already been declared"))
                        );
                    }
                    self.lexicals.insert(key, self.lex.len() as u32);
                    self.lex.push((Value::Empty, is_const));
                    self.lex_epoch += 1;
                }
                Op::InitLexical(name) => {
                    let v = self.peek(0).clone();
                    let key = self.cstr(name);
                    if let Some(&i) = self.lexicals.get(&key) {
                        self.lex[i as usize].0 = v;
                    }
                }
                Op::ThrowConst(_) => {
                    return Err(self.type_error("Assignment to constant variable."));
                }

                // -------------------------------------------- properties
                Op::GetProp(name) => {
                    let key = self.ckey(name);
                    let obj = self.peek(0).clone();
                    let v = self.get_v(&obj, &key)?;
                    *self.stack.last_mut().unwrap() = v;
                }
                Op::SetProp(name) => {
                    let key = self.ckey(name);
                    let v = self.pop();
                    let obj = self.peek(0).clone();
                    let strict = self.strict();
                    self.put(&obj, key, v.clone(), strict)?;
                    *self.stack.last_mut().unwrap() = v;
                }
                Op::GetElem => {
                    let v = self.get_elem()?;
                    self.stack.pop();
                    *self.stack.last_mut().unwrap() = v;
                }
                Op::SetElem => {
                    let v = self.peek(0).clone();
                    let key = self.peek(1).clone();
                    let obj = self.peek(2).clone();
                    if !self.fast_set_elem(&obj, &key, &v) {
                        let k = self.to_property_key(&key)?;
                        let strict = self.strict();
                        self.put(&obj, k, v.clone(), strict)?;
                    }
                    self.stack.truncate(self.stack.len() - 3);
                    self.stack.push(v);
                }
                Op::GetMethod(name) => {
                    let key = self.ckey(name);
                    let obj = self.peek(0).clone();
                    let f = self.get_v(&obj, &key)?;
                    let n = self.stack.len();
                    self.stack[n - 1] = f;
                    self.stack.push(obj);
                }
                Op::GetMethodElem => {
                    let f = self.get_elem()?;
                    let obj = self.peek(1).clone();
                    let n = self.stack.len();
                    self.stack[n - 2] = f;
                    self.stack[n - 1] = obj;
                }
                Op::DeleteProp(name) => {
                    let key = self.ckey(name);
                    let obj = self.pop();
                    let r = self.delete_value(&obj, &key)?;
                    self.stack.push(Value::Bool(r));
                }
                Op::DeleteElem => {
                    let key = self.pop();
                    let obj = self.pop();
                    if obj.is_nullish() {
                        return Err(self.type_error("Cannot convert undefined or null to object"));
                    }
                    let k = self.to_property_key(&key)?;
                    let r = self.delete_value(&obj, &k)?;
                    self.stack.push(Value::Bool(r));
                }
                Op::In => {
                    let obj = self.pop();
                    let key = self.pop();
                    let Value::Object(o) = obj else {
                        let k = self.short_describe(&key);
                        let d = self.short_describe(&obj);
                        return Err(self.type_error(&format!("Cannot use 'in' operator to search for {k} in {d}")));
                    };
                    let k = self.to_property_key(&key)?;
                    let r = self.has_property(o, &k)?;
                    self.stack.push(Value::Bool(r));
                }
                Op::InstanceOf => {
                    let target = self.pop();
                    let v = self.pop();
                    let r = self.instance_of(&v, &target)?;
                    self.stack.push(Value::Bool(r));
                }
                Op::GetSuper(name) => {
                    let key = self.ckey(name);
                    let v = self.super_get(key)?;
                    self.stack.push(v);
                }
                Op::GetSuperElem => {
                    let k = self.pop();
                    let key = self.to_property_key(&k)?;
                    let v = self.super_get(key)?;
                    self.stack.push(v);
                }
                Op::SetSuper(name) => {
                    let key = self.ckey(name);
                    let v = self.peek(0).clone();
                    self.super_set(key, v)?;
                }
                Op::SetSuperElem => {
                    let v = self.pop();
                    let k = self.pop();
                    let key = self.to_property_key(&k)?;
                    self.super_set(key, v.clone())?;
                    self.stack.push(v);
                }
                Op::GetPrivate => {
                    let sym = self.pop();
                    let obj = self.pop();
                    let v = self.private_get(&obj, &sym)?;
                    self.stack.push(v);
                }
                Op::SetPrivate => {
                    let v = self.pop();
                    let sym = self.pop();
                    let obj = self.pop();
                    self.private_set(&obj, &sym, v.clone())?;
                    self.stack.push(v);
                }
                Op::HasPrivate => {
                    let sym = self.pop();
                    let obj = self.pop();
                    let Value::Object(o) = obj else {
                        return Err(
                            self.type_error("Cannot use 'in' operator to search for a private field in a non-object")
                        );
                    };
                    let Value::Symbol(s) = sym else { unreachable!() };
                    let r = self.heap.get(o).props.get(&PropKey::Sym(s)).is_some();
                    self.stack.push(Value::Bool(r));
                }
                Op::PrivateName(desc) => {
                    let d = self.cstr(desc);
                    let s = self.new_private(d);
                    self.stack.push(Value::Symbol(s));
                }

                // -------------------------------------------- literals
                Op::NewObject => {
                    let o = self.new_object();
                    self.stack.push(Value::Object(o));
                }
                Op::NewArray => {
                    let a = self.new_array(Vec::new());
                    self.stack.push(Value::Object(a));
                }
                Op::ArrayPush => {
                    let v = self.pop();
                    let Value::Object(a) = *self.peek(0) else { unreachable!() };
                    if let Kind::Array(arr) = &mut self.heap.get_mut(a).kind {
                        arr.dense.push(v);
                        arr.len += 1;
                    }
                }
                Op::ArrayHole => {
                    let Value::Object(a) = *self.peek(0) else { unreachable!() };
                    if let Kind::Array(arr) = &mut self.heap.get_mut(a).kind {
                        arr.dense.push(Value::Empty);
                        arr.len += 1;
                    }
                }
                Op::ArraySpread => {
                    let it = self.peek(0).clone();
                    let items = self.iterate_to_vec(&it)?;
                    self.stack.pop();
                    let Value::Object(a) = *self.peek(0) else { unreachable!() };
                    if let Kind::Array(arr) = &mut self.heap.get_mut(a).kind {
                        arr.len += items.len() as u32;
                        arr.dense.extend(items);
                    }
                }
                Op::DefineField(name) => {
                    let key = self.ckey(name);
                    let v = self.pop();
                    let Value::Object(o) = *self.peek(0) else { unreachable!() };
                    self.create_data_property(o, key, v)?;
                }
                Op::DefineElem => {
                    let v = self.pop();
                    let k = self.pop();
                    let key = self.to_property_key(&k)?;
                    let Value::Object(o) = *self.peek(0) else { unreachable!() };
                    if !self.create_data_property(o, key.clone(), v)? {
                        let d = self.key_display(&key);
                        return Err(self.type_error(&format!("Cannot redefine property: {d}")));
                    }
                }
                Op::DefineMethod(flags) => {
                    let f = self.pop();
                    let k = self.pop();
                    let key = self.to_property_key(&k)?;
                    let Value::Object(o) = *self.peek(0) else { unreachable!() };
                    self.define_method(o, key, f, flags)?;
                }
                Op::CopyDataProps => {
                    let src = self.pop();
                    let Value::Object(target) = *self.peek(0) else { unreachable!() };
                    self.copy_data_props(target, &src, &[])?;
                }
                Op::CopyRest(n) => {
                    let n = n as usize;
                    let keys: Vec<Value> = self.stack.split_off(self.stack.len() - n);
                    let src = self.pop();
                    let mut excluded = Vec::new();
                    for k in &keys {
                        excluded.push(self.to_property_key(k)?);
                    }
                    let target = self.new_object();
                    self.copy_data_props(target, &src, &excluded)?;
                    self.stack.push(Value::Object(target));
                }
                Op::SetProtoLiteral => {
                    let p = self.pop();
                    let Value::Object(o) = *self.peek(0) else { unreachable!() };
                    match p {
                        Value::Object(p) => {
                            self.set_prototype(o, Some(p));
                        }
                        Value::Null => {
                            self.set_prototype(o, None);
                        }
                        _ => {}
                    }
                }
                Op::RegExp(p, f) => {
                    let pattern = self.cstr(p);
                    let flags = self.cstr(f);
                    let r = crate::builtins::regexp::create(self, pattern, flags, None)?;
                    self.stack.push(r);
                }
                Op::Template(i) => {
                    let data = match &self.frame().code.consts[i as usize] {
                        Const::Template(t) => t.clone(),
                        _ => unreachable!(),
                    };
                    let v = self.template_object(&data)?;
                    self.stack.push(v);
                }

                // -------------------------------------------- operators
                Op::Add => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (&a, &b) {
                        (Value::Number(x), Value::Number(y)) => Value::Number(x + y),
                        (Value::String(x), Value::String(y)) => Value::String(x.concat(y)),
                        _ => self.add_slow(a, b)?,
                    };
                    self.stack.push(r);
                }
                Op::Sub | Op::Mul | Op::Div | Op::Mod | Op::Exp => {
                    let b = self.pop();
                    let a = self.pop();
                    let (x, y) = match (&a, &b) {
                        (Value::Number(x), Value::Number(y)) => (*x, *y),
                        _ => {
                            let x = self.to_number(&a)?;
                            (x, self.to_number(&b)?)
                        }
                    };
                    let r = match op {
                        Op::Sub => x - y,
                        Op::Mul => x * y,
                        Op::Div => x / y,
                        Op::Mod => js_mod(x, y),
                        _ => js_pow(x, y),
                    };
                    self.stack.push(Value::Number(r));
                }
                Op::Shl | Op::Shr | Op::UShr | Op::BitAnd | Op::BitOr | Op::BitXor => {
                    let b = self.pop();
                    let a = self.pop();
                    let x = self.to_number(&a)?;
                    let y = self.to_number(&b)?;
                    let (xi, yu) = (numconv::to_int32(x), numconv::to_uint32(y));
                    let r = match op {
                        Op::Shl => xi.wrapping_shl(yu & 31) as f64,
                        Op::Shr => (xi >> (yu & 31)) as f64,
                        Op::UShr => (numconv::to_uint32(x) >> (yu & 31)) as f64,
                        Op::BitAnd => (xi & numconv::to_int32(y)) as f64,
                        Op::BitOr => (xi | numconv::to_int32(y)) as f64,
                        _ => (xi ^ numconv::to_int32(y)) as f64,
                    };
                    self.stack.push(Value::Number(r));
                }
                Op::Eq | Op::Ne => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = self.loose_eq(&a, &b)?;
                    self.stack.push(Value::Bool(r == matches!(op, Op::Eq)));
                }
                Op::StrictEq | Op::StrictNe => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = self.strict_eq(&a, &b);
                    self.stack.push(Value::Bool(r == matches!(op, Op::StrictEq)));
                }
                Op::Lt | Op::Le | Op::Gt | Op::Ge => {
                    let b = self.pop();
                    let a = self.pop();
                    let r = match (&a, &b) {
                        (Value::Number(x), Value::Number(y)) => match op {
                            Op::Lt => x < y,
                            Op::Le => x <= y,
                            Op::Gt => x > y,
                            _ => x >= y,
                        },
                        _ => self.compare_slow(op, a, b)?,
                    };
                    self.stack.push(Value::Bool(r));
                }
                Op::Neg => {
                    let a = self.pop();
                    let x = self.to_number(&a)?;
                    self.stack.push(Value::Number(-x));
                }
                Op::Plus | Op::ToNumeric => {
                    let a = self.pop();
                    let x = match a {
                        Value::Number(n) => n,
                        a => self.to_number(&a)?,
                    };
                    self.stack.push(Value::Number(x));
                }
                Op::Not => {
                    let a = self.pop();
                    self.stack.push(Value::Bool(!a.truthy()));
                }
                Op::BitNot => {
                    let a = self.pop();
                    let x = self.to_number(&a)?;
                    self.stack.push(Value::Number(!numconv::to_int32(x) as f64));
                }
                Op::Typeof => {
                    let a = self.pop();
                    let t = self.typeof_str(&a);
                    self.stack.push(Value::str(t));
                }
                Op::Inc | Op::Dec => {
                    let Value::Number(x) = self.pop() else { unreachable!() };
                    self.stack.push(Value::Number(if matches!(op, Op::Inc) { x + 1.0 } else { x - 1.0 }));
                }
                Op::ToString => {
                    let a = self.pop();
                    let s = match a {
                        Value::String(s) => s,
                        a => self.to_string(&a)?,
                    };
                    self.stack.push(Value::String(s));
                }
                Op::ToPropertyKey => {
                    let a = self.pop();
                    let k = self.to_property_key(&a)?;
                    self.stack.push(k.to_value());
                }

                // -------------------------------------------- control
                Op::Jump(t) => {
                    if (t as usize) < self.frame().pc {
                        self.tick()?;
                        self.maybe_collect_in_run();
                    }
                    self.frame_mut().pc = t as usize;
                }
                Op::JumpIfFalse(t) => {
                    if !self.pop().truthy() {
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::JumpIfTrue(t) => {
                    if self.pop().truthy() {
                        if (t as usize) < self.frame().pc {
                            self.tick()?;
                        }
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::JumpIfFalseKeep(t) => {
                    if !self.peek(0).truthy() {
                        self.frame_mut().pc = t as usize;
                    } else {
                        self.stack.pop();
                    }
                }
                Op::JumpIfTrueKeep(t) => {
                    if self.peek(0).truthy() {
                        self.frame_mut().pc = t as usize;
                    } else {
                        self.stack.pop();
                    }
                }
                Op::JumpIfNotNullishKeep(t) => {
                    if !self.peek(0).is_nullish() {
                        self.frame_mut().pc = t as usize;
                    } else {
                        self.stack.pop();
                    }
                }
                Op::JumpIfNullish(t, depth) => {
                    if self.peek(depth as usize).is_nullish() {
                        let n = self.stack.len() - depth as usize - 1;
                        self.stack.truncate(n);
                        self.stack.push(Value::Undefined);
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::JumpIfUndefined(t) => {
                    if self.pop().is_undefined() {
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::JumpIfNotUndefined(t) => {
                    if !self.peek(0).is_undefined() {
                        self.frame_mut().pc = t as usize;
                    }
                }

                // -------------------------------------------- calls
                Op::Call(argc) => {
                    self.tick()?;
                    self.maybe_collect_in_run();
                    let start = self.stack.len() - argc as usize;
                    let f = self.stack[start - 2].clone();
                    if let Some(r) = self.call_from_stack(f, start)? {
                        self.stack.push(r);
                    }
                }
                Op::CallSpread => {
                    self.tick()?;
                    let arr = self.pop();
                    let args = self.list_from_array_like(&arr)?;
                    let start = self.stack.len();
                    self.stack.extend(args);
                    let f = self.stack[start - 2].clone();
                    if let Some(r) = self.call_from_stack(f, start)? {
                        self.stack.push(r);
                    }
                }
                Op::New(argc) => {
                    self.tick()?;
                    self.maybe_collect_in_run();
                    let n = argc as usize;
                    let start = self.stack.len() - n;
                    let f = self.stack[start - 1].clone();
                    if let Some(r) = self.new_from_stack(f, start)? {
                        self.stack.push(r);
                    }
                }
                Op::NewSpread => {
                    self.tick()?;
                    let arr = self.pop();
                    let args = self.list_from_array_like(&arr)?;
                    let start = self.stack.len();
                    self.stack.extend(args);
                    let f = self.stack[start - 1].clone();
                    if let Some(r) = self.new_from_stack(f, start)? {
                        self.stack.push(r);
                    }
                }
                Op::SuperCall(argc) => {
                    let args = self.stack.split_off(self.stack.len() - argc as usize);
                    let r = self.super_call(args)?;
                    self.stack.push(r);
                }
                Op::SuperCallSpread => {
                    let arr = self.pop();
                    let args = self.list_from_array_like(&arr)?;
                    let r = self.super_call(args)?;
                    self.stack.push(r);
                }
                Op::Return => {
                    let v = self.pop();
                    if let Some(v) = self.do_return(v)? {
                        return Ok(v);
                    }
                }
                Op::Throw => {
                    let v = self.pop();
                    return Err(v);
                }
                Op::Rethrow => {
                    let v = self.pop();
                    return Err(v);
                }

                // -------------------------------------------- functions
                Op::Closure(i) => {
                    let code = match &self.frame().code.consts[i as usize] {
                        Const::Code(c) => c.clone(),
                        _ => unreachable!(),
                    };
                    let f = self.make_closure(code)?;
                    self.stack.push(Value::Object(f));
                }
                Op::Class(i, has_heritage) => {
                    let code = match &self.frame().code.consts[i as usize] {
                        Const::Code(c) => c.clone(),
                        _ => unreachable!(),
                    };
                    let heritage = if has_heritage { Some(self.pop()) } else { None };
                    let (ctor, proto) = self.make_class(code, heritage)?;
                    self.stack.push(Value::Object(ctor));
                    self.stack.push(Value::Object(proto));
                }
                Op::SetFields(_) => {}
                Op::AddField(flags) => {
                    let init = self.pop();
                    let k = self.pop();
                    let key = self.to_property_key(&k)?.to_value();
                    let Value::Object(ctor) = *self.peek(0) else { unreachable!() };
                    self.add_field(ctor, key, init, flags);
                }
                Op::InitFields => {
                    let f = self.frame().func.clone();
                    let this = self.frame().this.clone();
                    if let (Value::Object(f), Value::Object(t)) = (f, this) {
                        self.init_fields(f, t)?;
                    }
                }
                Op::This => {
                    let t = self.this_value()?;
                    self.stack.push(t);
                }
                Op::NewTarget => {
                    let t = self.frame().new_target.clone();
                    self.stack.push(t);
                }
                Op::Callee => {
                    let f = self.frame().func.clone();
                    self.stack.push(f);
                }
                Op::Arguments => {
                    let a = self.arguments_object();
                    self.stack.push(Value::Object(a));
                }
                Op::Rest(n) => {
                    let rest: Vec<Value> = self.frame().args.iter().skip(n as usize).cloned().collect();
                    let a = self.new_array(rest);
                    self.stack.push(Value::Object(a));
                }
                Op::SetFunctionName(_) => {
                    let f = self.peek(0).clone();
                    let k = self.peek(1).clone();
                    if let Value::Object(fo) = f {
                        let key = self.to_property_key(&k)?;
                        self.set_function_name(fo, &key, "");
                    }
                }

                // -------------------------------------------- exceptions
                Op::TryStart(t, finally) => {
                    let sp = (self.stack.len() - base) as u32;
                    let scope = self.frame().scope;
                    self.frame_mut().handlers.push(Handler { pc: t, sp, scope, finally });
                }
                Op::TryEnd => {
                    self.frame_mut().handlers.pop();
                }

                // -------------------------------------------- iteration
                Op::GetIterator => {
                    let v = self.peek(0).clone();
                    let rec = self.get_iterator(&v)?;
                    *self.stack.last_mut().unwrap() = Value::Object(rec);
                }
                Op::GetAsyncIterator => {
                    let v = self.peek(0).clone();
                    let rec = crate::builtins::iterators::get_async_iterator(self, &v)?;
                    *self.stack.last_mut().unwrap() = Value::Object(rec);
                }
                Op::IterNext(t) => {
                    let Value::Object(rec) = *self.peek(0) else { unreachable!() };
                    match self.iter_step(rec)? {
                        Some(v) => self.stack.push(v),
                        None => self.frame_mut().pc = t as usize,
                    }
                }
                Op::IterNextOrUndef => {
                    let Value::Object(rec) = *self.peek(0) else { unreachable!() };
                    let v = self.iter_step(rec)?.unwrap_or(Value::Undefined);
                    self.stack.push(v);
                }
                Op::IterRest => {
                    let Value::Object(rec) = *self.peek(0) else { unreachable!() };
                    let mut items = Vec::new();
                    while let Some(v) = self.iter_step(rec)? {
                        items.push(v);
                    }
                    let a = self.new_array(items);
                    self.stack.push(Value::Object(a));
                }
                Op::IterClose(quiet) => {
                    let Value::Object(rec) = *self.peek(0) else { unreachable!() };
                    self.iter_close(rec, quiet)?;
                    self.stack.pop();
                }
                Op::IterDrop => {
                    self.stack.pop();
                }
                Op::IterNextRaw => {
                    let Value::Object(rec) = *self.peek(0) else { unreachable!() };
                    let (next, iter) = match &self.heap.get(rec).kind {
                        Kind::IterRecord(r) => (r.next.clone(), r.iter.clone()),
                        _ => unreachable!(),
                    };
                    let r = self.call(&next, iter, &[])?;
                    self.stack.push(r);
                }
                Op::IterResult(t) => {
                    let r = self.pop();
                    let Value::Object(ro) = r else {
                        return Err(self.type_error("Iterator result is not an object"));
                    };
                    let done = self.get(ro, &self.names.done.clone(), r.clone())?;
                    if done.truthy() {
                        let Value::Object(rec) = *self.peek(0) else { unreachable!() };
                        self.set_iter_done(rec);
                        self.frame_mut().pc = t as usize;
                    } else {
                        let v = self.get(ro, &self.names.value.clone(), r.clone())?;
                        self.stack.push(v);
                    }
                }
                Op::ForInStart => {
                    let v = self.pop();
                    let it = self.for_in_start(&v)?;
                    self.stack.push(Value::Object(it));
                }
                Op::ForInNext(t) => {
                    let Value::Object(it) = *self.peek(0) else { unreachable!() };
                    match self.for_in_next(it)? {
                        Some(k) => self.stack.push(k),
                        None => self.frame_mut().pc = t as usize,
                    }
                }

                // -------------------------------------------- coroutines
                Op::InitialYield => {
                    let g = self.frame().generator.unwrap();
                    self.suspend(g, GenState::Start);
                    return Ok(Value::Undefined);
                }
                Op::Yield => {
                    let v = self.pop();
                    let g = self.frame().generator.unwrap();
                    self.suspend(g, GenState::Suspended);
                    return Ok(v);
                }
                Op::AsyncYield => {
                    let v = self.pop();
                    let g = self.frame().generator.unwrap();
                    self.suspend(g, GenState::Suspended);
                    return Ok(v);
                }
                Op::YieldDelegate(t) => {
                    if let Some(v) = self.yield_delegate(t)? {
                        return Ok(v);
                    }
                }
                Op::Await => {
                    let v = self.pop();
                    let g = self.frame().generator.expect("await outside an async function");
                    crate::builtins::promise::await_value(self, g, v)?;
                    self.suspend(g, GenState::Suspended);
                    return Ok(Value::Undefined);
                }
                Op::ToObject => {
                    let v = self.pop();
                    let o = self.to_object(&v)?;
                    self.stack.push(Value::Object(o));
                }
                Op::WithGet(name, t) => {
                    let Value::Object(o) = self.pop() else { unreachable!() };
                    let key = self.ckey(name);
                    if self.with_has(o, &key)? {
                        let v = self.get(o, &key, Value::Object(o))?;
                        self.stack.push(v);
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::WithSet(name, t) => {
                    let Value::Object(o) = self.pop() else { unreachable!() };
                    let key = self.ckey(name);
                    if self.with_has(o, &key)? {
                        let v = self.peek(0).clone();
                        let strict = self.strict();
                        self.put(&Value::Object(o), key, v, strict)?;
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::WithDelete(name, t) => {
                    let Value::Object(o) = self.pop() else { unreachable!() };
                    let key = self.ckey(name);
                    if self.with_has(o, &key)? {
                        let ok = self.delete(o, &key)?;
                        self.stack.push(Value::Bool(ok));
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::WithGetMethod(name, t) => {
                    let Value::Object(o) = self.pop() else { unreachable!() };
                    let key = self.ckey(name);
                    if self.with_has(o, &key)? {
                        let f = self.get(o, &key, Value::Object(o))?;
                        self.stack.push(f);
                        self.stack.push(Value::Object(o));
                        self.frame_mut().pc = t as usize;
                    }
                }
                Op::Debugger | Op::Nop => {}
            }
        }
    }

    // ------------------------------------------------------------ helpers

    fn get_elem(&mut self) -> JsResult {
        let key = self.peek(0).clone();
        let obj = self.peek(1).clone();
        // Fast paths: array[index], string[index].
        if let Value::Number(n) = key {
            let i = n as usize;
            if i as f64 == n {
                match &obj {
                    Value::Object(o) => {
                        if let Kind::Array(a) = &self.heap.get(*o).kind {
                            if let Some(v) = a.dense.get(i) {
                                if *v != Value::Empty {
                                    return Ok(v.clone());
                                }
                            }
                        }
                    }
                    Value::String(s) if i < s.len() => {
                        return Ok(Value::String(s.slice(i, i + 1)));
                    }
                    _ => {}
                }
            }
        }
        if obj.is_nullish() {
            let k = self.to_property_key(&key)?;
            return self.get_v(&obj, &k);
        }
        let k = self.to_property_key(&key)?;
        self.get_v(&obj, &k)
    }

    fn fast_set_elem(&mut self, obj: &Value, key: &Value, v: &Value) -> bool {
        let (Value::Object(o), Value::Number(n)) = (obj, key) else { return false };
        let i = *n as usize;
        if i as f64 != *n {
            return false;
        }
        let ob = self.heap.get_mut(*o);
        let extensible = ob.extensible;
        if let Kind::Array(a) = &mut ob.kind {
            if i < a.dense.len() && a.dense[i] != Value::Empty {
                a.dense[i] = v.clone();
                return true;
            }
            if i == a.dense.len()
                && a.dense.len() == a.len as usize
                && extensible
                && a.len_writable
                && ob.props.is_empty()
            {
                // Appending: only if no prototype defines that index.
                a.dense.push(v.clone());
                a.len += 1;
                return true;
            }
        }
        false
    }

    /// How to name a callee in an error: its source text, else its value.
    fn callee_text(&mut self, f: &Value) -> String {
        let f_ = self.frame();
        match f_.code.callee_at(f_.pc - 1) {
            Some(t) => t.to_rust(),
            None => self.short_describe(f),
        }
    }

    /// Whether a `with` object provides `key` (honouring @@unscopables).
    fn with_has(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        if !self.has_property(o, key)? {
            return Ok(false);
        }
        let u = self.get(o, &PropKey::Sym(Sym::UNSCOPABLES), Value::Object(o))?;
        if let Value::Object(uo) = u {
            if self.get(uo, key, u.clone())?.truthy() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn delete_value(&mut self, obj: &Value, key: &PropKey) -> Result<bool, Value> {
        let o = self.to_object(obj)?;
        let r = self.delete(o, key)?;
        if !r && self.strict() {
            let k = self.key_display(key);
            return Err(self.type_error(&format!("Cannot delete property '{k}'")));
        }
        Ok(r)
    }

    /// Where a global name was found last time, if that still holds: a
    /// lexical slot, or the global object's own data property at an index.
    fn global_cached(&self, name: u32) -> Option<GlobalAt> {
        let code = &self.frame().code;
        let (at, epoch) = code.global_cache[name as usize].get();
        if at & GLOBAL_LEXICAL != 0 {
            return Some(GlobalAt::Lexical((at & !GLOBAL_LEXICAL) as usize));
        }
        if at == 0 || epoch != self.lex_epoch {
            return None;
        }
        let i = at as usize - 1;
        let (k, p) = self.heap.get(self.global).props.entry_at(i)?;
        match (k, &p.slot) {
            (PropKey::Str(s), Slot::Data(_)) if s == code.str_const(name) => Some(GlobalAt::Property(i)),
            _ => None,
        }
    }

    /// Finds a global name the slow way, remembering where.
    fn global_lookup(&mut self, name: u32) -> Option<GlobalAt> {
        let key = self.cstr(name);
        let cell = |rt: &Self, v| rt.frame().code.global_cache[name as usize].set(v);
        if let Some(&i) = self.lexicals.get(&key) {
            cell(self, (GLOBAL_LEXICAL | i, 0));
            return Some(GlobalAt::Lexical(i as usize));
        }
        let i = self.heap.get(self.global).props.index_of(&PropKey::Str(key))?;
        if let Slot::Data(_) = self.heap.get(self.global).props.entry_at(i)?.1.slot {
            cell(self, (i as u32 + 1, self.lex_epoch));
            return Some(GlobalAt::Property(i));
        }
        None
    }

    fn get_global(&mut self, name: u32) -> JsResult {
        match self.global_cached(name).or_else(|| self.global_lookup(name)) {
            Some(GlobalAt::Lexical(i)) => {
                let v = &self.lex[i].0;
                if *v == Value::Empty {
                    return Err(self.tdz_error(name));
                }
                return Ok(v.clone());
            }
            Some(GlobalAt::Property(i)) => {
                if let Some((_, p)) = self.heap.get(self.global).props.entry_at(i) {
                    if let Slot::Data(v) = &p.slot {
                        return Ok(v.clone());
                    }
                }
            }
            None => {}
        }
        let g = self.global;
        let k = self.ckey(name);
        if self.has_property(g, &k)? {
            return self.get(g, &k, Value::Object(g));
        }
        let n = self.cstr(name).to_rust();
        Err(self.reference_error(&format!("{n} is not defined")))
    }

    fn set_global_binding(&mut self, name: u32, v: Value) -> Result<(), Value> {
        match self.global_cached(name).or_else(|| self.global_lookup(name)) {
            Some(GlobalAt::Lexical(i)) => {
                let (cur, is_const) = &self.lex[i];
                if *cur == Value::Empty {
                    return Err(self.tdz_error(name));
                }
                if *is_const {
                    return Err(self.type_error("Assignment to constant variable."));
                }
                self.lex[i].0 = v;
                return Ok(());
            }
            Some(GlobalAt::Property(i)) => {
                let g = self.global;
                if let Some(p) = self.heap.get_mut(g).props.entry_at_mut(i) {
                    if p.writable() {
                        if let Slot::Data(slot) = &mut p.slot {
                            *slot = v;
                            return Ok(());
                        }
                    }
                }
            }
            None => {}
        }
        let key = self.cstr(name);
        let g = self.global;
        let k = PropKey::Str(key);
        let strict = self.strict();
        if strict && !self.has_property(g, &k)? {
            let n = self.cstr(name).to_rust();
            return Err(self.reference_error(&format!("{n} is not defined")));
        }
        self.put(&Value::Object(g), k, v, strict)
    }

    fn add_slow(&mut self, a: Value, b: Value) -> JsResult {
        let pa = self.to_primitive(&a, None)?;
        let pb = self.to_primitive(&b, None)?;
        if matches!(pa, Value::String(_)) || matches!(pb, Value::String(_)) {
            let sa = self.to_string(&pa)?;
            let sb = self.to_string(&pb)?;
            return Ok(Value::String(sa.concat(&sb)));
        }
        let x = self.to_number(&pa)?;
        let y = self.to_number(&pb)?;
        Ok(Value::Number(x + y))
    }

    fn compare_slow(&mut self, op: Op, a: Value, b: Value) -> Result<bool, Value> {
        let pa = self.to_primitive(&a, Some("number"))?;
        let pb = self.to_primitive(&b, Some("number"))?;
        if let (Value::String(x), Value::String(y)) = (&pa, &pb) {
            let o = x.units().cmp(y.units());
            return Ok(match op {
                Op::Lt => o.is_lt(),
                Op::Le => o.is_le(),
                Op::Gt => o.is_gt(),
                _ => o.is_ge(),
            });
        }
        let x = self.to_number(&pa)?;
        let y = self.to_number(&pb)?;
        Ok(match op {
            Op::Lt => x < y,
            Op::Le => x <= y,
            Op::Gt => x > y,
            _ => x >= y,
        })
    }

    /// Calls `f` with the arguments at stack[start..]; `func this` sit just
    /// below. Pushes a frame for closures (returns None), else the result.
    fn call_from_stack(&mut self, f: Value, start: usize) -> Result<Option<Value>, Value> {
        if let Value::Object(fo) = f {
            if let Kind::Function(c) = &self.heap.get(fo).kind {
                if !c.code.is_generator && !c.code.is_async {
                    let this = self.stack[start - 1].clone();
                    return self.push_frame_from(fo, this, Args::OnStack(start), Value::Undefined, false, false);
                }
            }
        }
        if !self.is_callable(&f) {
            let what = self.callee_text(&f);
            return Err(self.type_error(&format!("{what} is not a function")));
        }
        let args: Vec<Value> = self.stack.split_off(start);
        let this = self.stack.pop().unwrap();
        self.stack.pop();
        self.call_owned(&f, this, args).map(Some)
    }

    fn new_from_stack(&mut self, f: Value, start: usize) -> Result<Option<Value>, Value> {
        if !self.is_constructor(&f) {
            let d = self.callee_text(&f);
            return Err(self.type_error(&format!("{d} is not a constructor")));
        }
        let args: Vec<Value> = self.stack.split_off(start);
        self.stack.pop();
        if let Value::Object(fo) = f {
            if let Kind::Function(_) = &self.heap.get(fo).kind {
                return self.push_frame(fo, Value::Undefined, args, f.clone(), true, false);
            }
        }
        self.construct(&f, &args, None).map(Some)
    }

    /// Pops the current frame with return value `v`. Returns Some(v) when the
    /// frame was a boundary (the run loop should return).
    fn do_return(&mut self, v: Value) -> Result<Option<Value>, Value> {
        let mut v = v;
        let (construct, kind, this) = {
            let f = self.frame();
            (f.construct, f.code.kind, f.this.clone())
        };
        if construct && !matches!(v, Value::Object(_)) {
            if kind == CodeKind::DerivedConstructor && !v.is_undefined() {
                return Err(self.type_error("Derived constructors may only return object or undefined"));
            }
            if this == Value::Empty {
                return Err(self.reference_error("Must call super constructor in derived class before returning"));
            }
            v = this;
        }
        let frame = self.frames.pop().unwrap();
        self.stack.truncate(frame.floor);
        if let Some(g) = frame.generator {
            let promise = match &mut self.heap.get_mut(g).kind {
                Kind::Generator(gs) => {
                    gs.state = GenState::Done;
                    gs.promise
                }
                _ => None,
            };
            if let Some(p) = promise {
                crate::builtins::promise::resolve(self, p, v.clone())?;
            }
        }
        if frame.boundary {
            return Ok(Some(v));
        }
        self.stack.push(v);
        Ok(None)
    }

    /// Moves the current (coroutine) frame into its generator object.
    fn suspend(&mut self, g: ObjRef, state: GenState) {
        let frame = self.frames.pop().unwrap();
        let saved = self.stack.split_off(frame.base);
        if let Kind::Generator(gs) = &mut self.heap.get_mut(g).kind {
            gs.state = state;
            gs.frame = Some(frame);
            gs.stack = saved;
        }
    }

    /// `yield*`. Stack: iterRecord received. Returns Some(value) to suspend.
    fn yield_delegate(&mut self, done_target: u32) -> Result<Option<Value>, Value> {
        let received = self.pop();
        let Value::Object(rec) = *self.peek(0) else { unreachable!() };
        let mode = core::mem::take(&mut self.frame_mut().resume_mode);
        self.iter_materialize(rec)?;
        let (iter, next) = match &self.heap.get(rec).kind {
            Kind::IterRecord(r) => (r.iter.clone(), r.next.clone()),
            _ => unreachable!(),
        };
        let g = self.frame().generator.unwrap();
        let result = match mode {
            0 => self.call(&next, iter.clone(), &[received])?,
            1 => match self.get_method(&iter, &PropKey::from("throw"))? {
                Some(t) => self.call(&t, iter.clone(), &[received])?,
                None => {
                    self.iter_close(rec, false)?;
                    return Err(self.type_error("The iterator does not provide a 'throw' method"));
                }
            },
            _ => match self.get_method(&iter, &PropKey::from("return"))? {
                Some(r) => self.call(&r, iter.clone(), &[received])?,
                None => {
                    let sig = self.alloc(Obj::new(None, Kind::ReturnSignal(received)));
                    return Err(Value::Object(sig));
                }
            },
        };
        let Value::Object(ro) = result else {
            return Err(self.type_error("Iterator result is not an object"));
        };
        let done = self.get(ro, &self.names.done.clone(), result.clone())?;
        if done.truthy() {
            let v = self.get(ro, &self.names.value.clone(), result.clone())?;
            if mode == 2 {
                let sig = self.alloc(Obj::new(None, Kind::ReturnSignal(v)));
                return Err(Value::Object(sig));
            }
            self.stack.pop();
            self.stack.push(v);
            self.frame_mut().pc = done_target as usize;
            return Ok(None);
        }
        // Suspend, re-running this instruction on resume. The inner
        // result object is passed through unchanged.
        self.frame_mut().pc -= 1;
        self.raw_yield = true;
        self.suspend(g, GenState::Suspended);
        Ok(Some(result))
    }

    fn super_base(&mut self) -> Result<Option<ObjRef>, Value> {
        let Some(home) = self.home_object() else {
            return Err(self.error(ErrorKind::SyntaxError, "'super' keyword unexpected here"));
        };
        Ok(self.heap.get(home).proto)
    }

    fn super_get(&mut self, key: PropKey) -> JsResult {
        let this = self.this_value()?;
        match self.super_base()? {
            Some(p) => self.get(p, &key, this),
            None => {
                let k = self.key_display(&key);
                Err(self.type_error(&format!("Cannot read properties of null (reading '{k}')")))
            }
        }
    }

    fn super_set(&mut self, key: PropKey, v: Value) -> Result<(), Value> {
        let this = self.this_value()?;
        match self.super_base()? {
            Some(p) => {
                let ok = self.set(p, key.clone(), v, this)?;
                if !ok && self.strict() {
                    let k = self.key_display(&key);
                    return Err(self.type_error(&format!("Cannot assign to read only property '{k}'")));
                }
                Ok(())
            }
            None => Err(self.type_error("Cannot set properties of null")),
        }
    }

    fn super_call(&mut self, args: Vec<Value>) -> JsResult {
        let Value::Object(ctor) = self.frame().func.clone() else {
            return Err(self.error(ErrorKind::SyntaxError, "'super' keyword unexpected here"));
        };
        // Inside an arrow, `super()` refers to the enclosing constructor.
        let ctor = match &self.heap.get(ctor).kind {
            Kind::Function(c) if c.code.kind == CodeKind::Arrow => match c.home {
                Some(h) => match self.get_own_property(h, &self.names.constructor.clone()) {
                    Some(Prop { slot: Slot::Data(Value::Object(c)), .. }) => c,
                    _ => ctor,
                },
                None => ctor,
            },
            _ => ctor,
        };
        let parent = self.heap.get(ctor).proto.map(Value::Object).unwrap_or(Value::Null);
        if !self.is_constructor(&parent) {
            return Err(self.type_error("Super constructor is not a constructor"));
        }
        let nt = self.frame().new_target.clone();
        let result = self.construct(&parent, &args, Some(nt))?;
        if self.frame().this != Value::Empty {
            return Err(self.reference_error("Super constructor may only be called once"));
        }
        self.frame_mut().this = result.clone();
        if let Value::Object(r) = result {
            self.init_fields(ctor, r)?;
        }
        Ok(result)
    }

    pub(crate) fn make_closure(&mut self, code: Rc<Code>) -> Result<ObjRef, Value> {
        let scope = self.frame().scope;
        let (this, nt, home) = if code.kind == CodeKind::Arrow {
            let f = self.frame();
            (f.this.clone(), f.new_target.clone(), self.home_object())
        } else {
            (Value::Undefined, Value::Undefined, None)
        };
        let (proto, ctor_kind) = match (code.is_async, code.is_generator) {
            (false, true) => (self.intr.generator_function_proto, CtorKind::None),
            (true, false) => (self.intr.async_function_proto, CtorKind::None),
            (true, true) => (self.intr.async_generator_function_proto, CtorKind::None),
            (false, false) => {
                (self.intr.function_proto, if code.kind == CodeKind::Normal { CtorKind::Base } else { CtorKind::None })
            }
        };
        let name = code.name.clone();
        let length = code.length;
        let is_gen = code.is_generator;
        let is_async = code.is_async;
        let f = self.alloc(Obj::new(
            Some(proto),
            Kind::Function(Box::new(Closure { code, scope, this, new_target: nt, home, fields: None, ctor_kind })),
        ));
        self.define(f, "length", Value::Number(length as f64), CONFIGURABLE);
        self.define(f, "name", Value::String(name), CONFIGURABLE);
        if ctor_kind == CtorKind::Base {
            let p = self.new_object();
            self.define(p, "constructor", Value::Object(f), HIDDEN);
            self.define(f, "prototype", Value::Object(p), WRITABLE);
        } else if is_gen {
            let pp = if is_async { self.intr.async_generator_proto } else { self.intr.generator_proto };
            let p = self.new_object_with(Some(pp));
            self.define(f, "prototype", Value::Object(p), WRITABLE);
        }
        Ok(f)
    }

    fn make_class(&mut self, code: Rc<Code>, heritage: Option<Value>) -> Result<(ObjRef, ObjRef), Value> {
        let (proto_parent, ctor_parent) = match heritage {
            None => (Some(self.intr.object_proto), self.intr.function_proto),
            Some(Value::Null) => (None, self.intr.function_proto),
            Some(h) => {
                if !self.is_constructor(&h) {
                    let d = self.short_describe(&h);
                    return Err(self.type_error(&format!("Class extends value {d} is not a constructor or null")));
                }
                let ho = h.as_object().unwrap();
                let pp = self.get(ho, &self.names.prototype.clone(), h.clone())?;
                let pp = match pp {
                    Value::Object(p) => Some(p),
                    Value::Null => None,
                    _ => return Err(self.type_error("Class extends value does not have valid prototype property")),
                };
                (pp, ho)
            }
        };
        let proto = self.new_object_with(proto_parent);
        let scope = self.frame().scope;
        let ctor_kind = if code.kind == CodeKind::DerivedConstructor { CtorKind::Derived } else { CtorKind::Base };
        let name = code.name.clone();
        let length = code.length;
        let ctor = self.alloc(Obj::new(
            Some(ctor_parent),
            Kind::Function(Box::new(Closure {
                code,
                scope,
                this: Value::Undefined,
                new_target: Value::Undefined,
                home: Some(proto),
                fields: None,
                ctor_kind,
            })),
        ));
        self.define(ctor, "length", Value::Number(length as f64), CONFIGURABLE);
        self.define(ctor, "name", Value::String(name), CONFIGURABLE);
        self.define(ctor, "prototype", Value::Object(proto), 0);
        self.define(proto, "constructor", Value::Object(ctor), HIDDEN);
        Ok((ctor, proto))
    }

    fn define_method(&mut self, o: ObjRef, key: PropKey, f: Value, flags: u8) -> Result<(), Value> {
        let Value::Object(fo) = f else { unreachable!() };
        if flags & 8 != 0 {
            if let Kind::Function(c) = &mut self.heap.get_mut(fo).kind {
                c.home = Some(o);
            }
        }
        if flags & 16 != 0 {
            let prefix = match flags & 3 {
                1 => "get",
                2 => "set",
                _ => "",
            };
            self.set_function_name(fo, &key, prefix);
        }
        let enumerable = if flags & 4 != 0 { ENUMERABLE } else { 0 };
        let desc = match flags & 3 {
            1 => crate::realm::PropDesc {
                get: Some(f),
                enumerable: Some(enumerable != 0),
                configurable: Some(true),
                ..Default::default()
            },
            2 => crate::realm::PropDesc {
                set: Some(f),
                enumerable: Some(enumerable != 0),
                configurable: Some(true),
                ..Default::default()
            },
            _ => crate::realm::PropDesc::data(f, WRITABLE | CONFIGURABLE | enumerable),
        };
        if !self.define_own(o, key.clone(), desc)? {
            let k = self.key_display(&key);
            return Err(self.type_error(&format!("Cannot redefine property: {k}")));
        }
        Ok(())
    }

    pub(crate) fn set_function_name(&mut self, f: ObjRef, key: &PropKey, prefix: &str) {
        let base = match key {
            PropKey::Str(s) => s.to_rust(),
            PropKey::Sym(s) => {
                let info = &self.symbols[s.0 as usize];
                match &info.description {
                    Some(d) if info.private => d.to_rust(),
                    Some(d) => format!("[{}]", d.to_rust()),
                    None => String::new(),
                }
            }
        };
        let name = if prefix.is_empty() { base } else { format!("{prefix} {base}") };
        self.define(f, "name", Value::str(&name), CONFIGURABLE);
    }

    fn add_field(&mut self, ctor: ObjRef, key: Value, init: Value, flags: u8) {
        let (fields, home) = match &self.heap.get(ctor).kind {
            Kind::Function(c) => (c.fields, c.home),
            _ => return,
        };
        if let Value::Object(fo) = &init {
            if let Kind::Function(c) = &mut self.heap.get_mut(*fo).kind {
                c.home = home;
            }
        }
        let fields = match fields {
            Some(f) => f,
            None => {
                let f = self.alloc(Obj::new(None, Kind::Fields(Vec::new())));
                if let Kind::Function(c) = &mut self.heap.get_mut(ctor).kind {
                    c.fields = Some(f);
                }
                f
            }
        };
        if let Kind::Fields(list) = &mut self.heap.get_mut(fields).kind {
            list.push((key, init, flags));
        }
    }

    pub(crate) fn init_fields(&mut self, ctor: ObjRef, this: ObjRef) -> Result<(), Value> {
        let fields = match &self.heap.get(ctor).kind {
            Kind::Function(c) => c.fields,
            _ => None,
        };
        let Some(fields) = fields else { return Ok(()) };
        let list = match &self.heap.get(fields).kind {
            Kind::Fields(l) => l.clone(),
            _ => return Ok(()),
        };
        // Private methods first, then fields in order.
        for (key, f, flags) in list.iter().filter(|x| x.2 & 1 != 0) {
            let Value::Symbol(s) = key else { continue };
            let k = PropKey::Sym(*s);
            let existing = self.heap.get(this).props.get(&k).cloned();
            let prop = match flags & 6 {
                2 => Prop {
                    slot: Slot::Accessor(
                        f.clone(),
                        match existing {
                            Some(Prop { slot: Slot::Accessor(_, s), .. }) => s,
                            _ => Value::Undefined,
                        },
                    ),
                    flags: 0,
                },
                4 => Prop {
                    slot: Slot::Accessor(
                        match existing {
                            Some(Prop { slot: Slot::Accessor(g, _), .. }) => g,
                            _ => Value::Undefined,
                        },
                        f.clone(),
                    ),
                    flags: 0,
                },
                _ => {
                    if existing.is_some() {
                        return Err(self.type_error("Cannot initialize private methods twice on the same object"));
                    }
                    Prop::data(f.clone(), 0)
                }
            };
            self.heap.get_mut(this).props.insert(k, prop);
        }
        for (key, init, _) in list.iter().filter(|x| x.2 & 1 == 0) {
            let v = if init.is_undefined() { Value::Undefined } else { self.call(init, Value::Object(this), &[])? };
            match key {
                Value::Symbol(s) if self.is_private(*s) => {
                    let k = PropKey::Sym(*s);
                    if self.heap.get(this).props.get(&k).is_some() {
                        return Err(self.type_error("Cannot initialize a private field twice on the same object"));
                    }
                    self.heap.get_mut(this).props.insert(k, Prop::data(v, WRITABLE));
                }
                k => {
                    let pk = self.to_property_key(k)?;
                    self.create_data_property_or_throw(this, pk, v)?;
                }
            }
        }
        Ok(())
    }

    fn private_get(&mut self, obj: &Value, sym: &Value) -> JsResult {
        let Value::Symbol(s) = sym else { unreachable!() };
        let k = PropKey::Sym(*s);
        let p = match obj {
            Value::Object(o) => self.heap.get(*o).props.get(&k).cloned(),
            _ => None,
        };
        match p {
            Some(Prop { slot: Slot::Data(v), .. }) => Ok(v),
            Some(Prop { slot: Slot::Accessor(g, _), .. }) => {
                if g.is_undefined() {
                    return Err(self.type_error("'#x' was defined without a getter"));
                }
                self.call(&g, obj.clone(), &[])
            }
            None => {
                let d = self.symbols[s.0 as usize].description.clone().unwrap_or_else(JsStr::empty).to_rust();
                Err(self.type_error(&format!(
                    "Cannot read private member {d} from an object whose class did not declare it"
                )))
            }
        }
    }

    fn private_set(&mut self, obj: &Value, sym: &Value, v: Value) -> Result<(), Value> {
        let Value::Symbol(s) = sym else { unreachable!() };
        let k = PropKey::Sym(*s);
        let o = match obj {
            Value::Object(o) => *o,
            _ => return Err(self.type_error("Cannot write private member to a non-object")),
        };
        let p = self.heap.get(o).props.get(&k).cloned();
        match p {
            Some(Prop { slot: Slot::Data(_), flags }) => {
                if flags & WRITABLE == 0 {
                    return Err(self.type_error("Private method is not writable"));
                }
                self.heap.get_mut(o).props.insert(k, Prop::data(v, flags));
                Ok(())
            }
            Some(Prop { slot: Slot::Accessor(_, setter), .. }) => {
                if setter.is_undefined() {
                    return Err(self.type_error("'#x' was defined without a setter"));
                }
                self.call(&setter, obj.clone(), &[v])?;
                Ok(())
            }
            None => {
                let d = self.symbols[s.0 as usize].description.clone().unwrap_or_else(JsStr::empty).to_rust();
                Err(self.type_error(&format!(
                    "Cannot write private member {d} to an object whose class did not declare it"
                )))
            }
        }
    }

    pub(crate) fn copy_data_props(&mut self, target: ObjRef, src: &Value, excluded: &[PropKey]) -> Result<(), Value> {
        if src.is_nullish() {
            return Ok(());
        }
        let from = self.to_object(src)?;
        for k in self.own_keys(from) {
            if excluded.contains(&k) {
                continue;
            }
            if let Some(p) = self.get_own_property(from, &k) {
                if p.enumerable() {
                    let v = self.get(from, &k, Value::Object(from))?;
                    self.create_data_property(target, k, v)?;
                }
            }
        }
        Ok(())
    }

    fn template_object(&mut self, data: &crate::bytecode::TemplateData) -> JsResult {
        if let Some(&t) = self.templates.get(&data.site) {
            return Ok(Value::Object(t));
        }
        let cooked: Vec<Value> =
            data.cooked.iter().map(|c| c.clone().map(Value::String).unwrap_or(Value::Undefined)).collect();
        let raw: Vec<Value> = data.raw.iter().map(|r| Value::String(r.clone())).collect();
        let raw_arr = self.new_array(raw);
        self.set_integrity(raw_arr, true)?;
        let arr = self.new_array(cooked);
        self.define(arr, "raw", Value::Object(raw_arr), 0);
        self.set_integrity(arr, true)?;
        self.templates.insert(data.site, arr);
        Ok(Value::Object(arr))
    }

    fn arguments_object(&mut self) -> ObjRef {
        let (args, func, strict) = {
            let f = self.frame();
            (f.args.clone(), f.func.clone(), f.code.strict)
        };
        let p = self.intr.object_proto;
        let o = self.alloc(Obj::new(Some(p), Kind::Arguments));
        for (i, a) in args.iter().enumerate() {
            self.define(o, PropKey::index(i as u32), a.clone(), DEFAULT);
        }
        self.define(o, "length", Value::Number(args.len() as f64), HIDDEN);
        let values = Value::Object(self.intr.array_values);
        self.define(o, Sym::ITERATOR, values, HIDDEN);
        if strict {
            let t = Value::Object(self.intr.throw_type_error);
            self.define_accessor(o, "callee", t.clone(), t, 0);
        } else {
            self.define(o, "callee", func, HIDDEN);
        }
        o
    }

    fn for_in_start(&mut self, v: &Value) -> Result<ObjRef, Value> {
        let mut keys: Vec<PropKey> = Vec::new();
        let obj = if v.is_nullish() {
            self.new_object()
        } else {
            let o = self.to_object(v)?;
            let mut seen: alloc::collections::BTreeSet<PropKey> = alloc::collections::BTreeSet::new();
            let mut cur = Some(o);
            while let Some(c) = cur {
                for k in self.own_keys(c) {
                    if let PropKey::Sym(_) = k {
                        continue;
                    }
                    if !seen.insert(k.clone()) {
                        continue;
                    }
                    if self.get_own_property(c, &k).is_some_and(|p| p.enumerable()) {
                        keys.push(k);
                    }
                }
                cur = self.heap.get(c).proto;
            }
            o
        };
        Ok(self.alloc(Obj::new(None, Kind::ForIn(Box::new((keys, 0, obj))))))
    }

    fn for_in_next(&mut self, it: ObjRef) -> Result<Option<Value>, Value> {
        loop {
            let (key, obj) = match &mut self.heap.get_mut(it).kind {
                Kind::ForIn(f) => {
                    if f.1 as usize >= f.0.len() {
                        return Ok(None);
                    }
                    f.1 += 1;
                    (f.0[f.1 as usize - 1].clone(), f.2)
                }
                _ => unreachable!(),
            };
            // Skip keys deleted during the loop.
            if self.has_property(obj, &key)? {
                return Ok(Some(key.to_value()));
            }
        }
    }
}

fn js_mod(x: f64, y: f64) -> f64 {
    // Small integers (the usual case) without a call to fmod. The result
    // takes x's sign, so 0 from a negative x is -0.
    const SAFE: f64 = 9007199254740992.0;
    if x.abs() < SAFE && y.abs() < SAFE && y != 0.0 && x == (x as i64) as f64 && y == (y as i64) as f64 {
        let r = (x as i64 % y as i64) as f64;
        return if r == 0.0 && x.is_sign_negative() { -0.0 } else { r };
    }
    if y.is_infinite() && x.is_finite() {
        return x;
    }
    libm::fmod(x, y)
}

pub fn js_pow(x: f64, y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    if y == 0.0 {
        return 1.0;
    }
    if (x == 1.0 || x == -1.0) && y.is_infinite() {
        return f64::NAN;
    }
    libm::pow(x, y)
}
