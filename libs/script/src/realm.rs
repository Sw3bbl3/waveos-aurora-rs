//! The realm: the heap, the global object, the intrinsics, and the abstract
//! operations of the specification (Get, Set, DefineOwnProperty, Call,
//! ToPrimitive, ToString, equality, iteration…).

use crate::bytecode::Code;
use crate::heap::{Heap, ObjRef};
use crate::numconv;
use crate::object::*;
use crate::value::{JsStr, PropKey, Sym, Value};
use crate::vm::Frame;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub type JsResult = Result<Value, Value>;

/// What the engine needs from its embedder.
pub trait Host {
    /// Milliseconds since the Unix epoch.
    fn now_ms(&mut self) -> f64;
    /// `console.log` and friends: one formatted line, with its level
    /// ("log", "info", "warn", "error", "debug").
    fn console(&mut self, level: &str, line: &str);
    /// A random seed (called once).
    fn seed(&mut self) -> u64 {
        0x9E37_79B9_7F4A_7C15
    }
    /// The local time zone's offset from UTC in minutes (UTC − local).
    fn timezone_offset(&mut self, _utc_ms: f64) -> f64 {
        0.0
    }
    /// Fills `buf` with random bytes for crypto.getRandomValues. Hosts with
    /// an operating-system generator should provide it; the default is not
    /// cryptographically strong.
    fn fill_random(&mut self, buf: &mut [u8]) -> bool {
        let _ = buf;
        false
    }
}

/// State the embedder keeps inside the realm (the browser's DOM bindings),
/// reachable from native functions through [`Realm::embedder`].
pub trait Embedder: core::any::Any {
    /// Every object this state keeps alive.
    fn trace(&self, out: &mut Vec<ObjRef>);
    fn as_any(&mut self) -> &mut dyn core::any::Any;
}

/// The arguments of a native function call.
pub struct Call {
    pub this: Value,
    pub args: Vec<Value>,
    /// `undefined` for a plain call; the constructor for `new`.
    pub new_target: Value,
    pub callee: ObjRef,
}

impl Call {
    pub fn arg(&self, i: usize) -> Value {
        self.args.get(i).cloned().unwrap_or(Value::Undefined)
    }

    pub fn is_construct(&self) -> bool {
        !self.new_target.is_undefined()
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum ErrorKind {
    Error,
    TypeError,
    RangeError,
    SyntaxError,
    ReferenceError,
    EvalError,
    URIError,
    AggregateError,
}

impl ErrorKind {
    pub const ALL: [ErrorKind; 8] = [
        ErrorKind::Error,
        ErrorKind::TypeError,
        ErrorKind::RangeError,
        ErrorKind::SyntaxError,
        ErrorKind::ReferenceError,
        ErrorKind::EvalError,
        ErrorKind::URIError,
        ErrorKind::AggregateError,
    ];

    pub fn name(self) -> &'static str {
        match self {
            ErrorKind::Error => "Error",
            ErrorKind::TypeError => "TypeError",
            ErrorKind::RangeError => "RangeError",
            ErrorKind::SyntaxError => "SyntaxError",
            ErrorKind::ReferenceError => "ReferenceError",
            ErrorKind::EvalError => "EvalError",
            ErrorKind::URIError => "URIError",
            ErrorKind::AggregateError => "AggregateError",
        }
    }
}

/// The built-in objects every realm starts with.
pub struct Intrinsics {
    pub object_proto: ObjRef,
    pub function_proto: ObjRef,
    pub array_proto: ObjRef,
    pub string_proto: ObjRef,
    pub number_proto: ObjRef,
    pub boolean_proto: ObjRef,
    pub symbol_proto: ObjRef,
    pub errors: [ObjRef; 8],
    pub error_ctors: [ObjRef; 8],
    pub iterator_proto: ObjRef,
    pub async_iterator_proto: ObjRef,
    pub array_iterator_proto: ObjRef,
    pub string_iterator_proto: ObjRef,
    pub map_iterator_proto: ObjRef,
    pub set_iterator_proto: ObjRef,
    pub regexp_string_iterator_proto: ObjRef,
    pub generator_proto: ObjRef,
    pub async_generator_proto: ObjRef,
    pub generator_function_proto: ObjRef,
    pub async_function_proto: ObjRef,
    pub async_generator_function_proto: ObjRef,
    pub async_from_sync_iterator_proto: ObjRef,
    pub iterator_helper_proto: ObjRef,
    pub wrap_for_valid_iterator_proto: ObjRef,
    pub promise_proto: ObjRef,
    pub array_buffer_proto: ObjRef,
    pub array_buffer_ctor: ObjRef,
    pub data_view_proto: ObjRef,
    pub typed_protos: [ObjRef; 9],
    pub typed_ctors: [ObjRef; 9],
    pub promise_ctor: ObjRef,
    pub map_proto: ObjRef,
    pub set_proto: ObjRef,
    pub weakmap_proto: ObjRef,
    pub weakset_proto: ObjRef,
    pub regexp_proto: ObjRef,
    pub regexp_ctor: ObjRef,
    pub regexp_exec: ObjRef,
    pub date_proto: ObjRef,
    pub array_ctor: ObjRef,
    pub object_ctor: ObjRef,
    pub function_ctor: ObjRef,
    pub array_values: ObjRef,
    pub array_iterator_next: ObjRef,
    pub throw_type_error: ObjRef,
    pub eval: ObjRef,
}

impl Intrinsics {
    fn all(&self) -> Vec<ObjRef> {
        let mut v = vec![
            self.object_proto,
            self.function_proto,
            self.array_proto,
            self.string_proto,
            self.number_proto,
            self.boolean_proto,
            self.symbol_proto,
            self.iterator_proto,
            self.async_iterator_proto,
            self.array_iterator_proto,
            self.string_iterator_proto,
            self.map_iterator_proto,
            self.set_iterator_proto,
            self.regexp_string_iterator_proto,
            self.generator_proto,
            self.async_generator_proto,
            self.generator_function_proto,
            self.async_function_proto,
            self.async_generator_function_proto,
            self.async_from_sync_iterator_proto,
            self.iterator_helper_proto,
            self.wrap_for_valid_iterator_proto,
            self.promise_proto,
            self.array_buffer_proto,
            self.array_buffer_ctor,
            self.data_view_proto,
            self.promise_ctor,
            self.map_proto,
            self.set_proto,
            self.weakmap_proto,
            self.weakset_proto,
            self.regexp_proto,
            self.regexp_ctor,
            self.regexp_exec,
            self.date_proto,
            self.array_ctor,
            self.object_ctor,
            self.function_ctor,
            self.array_values,
            self.array_iterator_next,
            self.throw_type_error,
            self.eval,
        ];
        v.extend_from_slice(&self.typed_protos);
        v.extend_from_slice(&self.typed_ctors);
        v.extend_from_slice(&self.errors);
        v.extend_from_slice(&self.error_ctors);
        v
    }
}

pub struct SymbolInfo {
    pub description: Option<JsStr>,
    pub private: bool,
}

/// A queued job (promise reactions and host callbacks).
pub enum Job {
    /// Call `f` with `args` (results ignored).
    Call(Value, Vec<Value>),
    /// A promise reaction job.
    Reaction { reaction_handler: Value, fulfill: bool, capability: Option<(Value, Value, Value)>, argument: Value },
    /// PromiseResolveThenableJob.
    Thenable { promise: ObjRef, thenable: Value, then: Value },
}

/// A property descriptor (Object.defineProperty).
#[derive(Clone, Default, Debug)]
pub struct PropDesc {
    pub value: Option<Value>,
    pub writable: Option<bool>,
    pub get: Option<Value>,
    pub set: Option<Value>,
    pub enumerable: Option<bool>,
    pub configurable: Option<bool>,
}

impl PropDesc {
    pub fn data(v: Value, flags: u8) -> PropDesc {
        PropDesc {
            value: Some(v),
            writable: Some(flags & WRITABLE != 0),
            enumerable: Some(flags & ENUMERABLE != 0),
            configurable: Some(flags & CONFIGURABLE != 0),
            get: None,
            set: None,
        }
    }

    pub fn is_accessor(&self) -> bool {
        self.get.is_some() || self.set.is_some()
    }

    pub fn is_data(&self) -> bool {
        self.value.is_some() || self.writable.is_some()
    }

    pub fn is_generic(&self) -> bool {
        !self.is_accessor() && !self.is_data()
    }
}

pub struct Realm {
    pub heap: Heap,
    pub stack: Vec<Value>,
    pub frames: Vec<Frame>,
    pub intr: Intrinsics,
    pub global: ObjRef,
    /// Global `let`, `const` and `class` bindings: value and constness.
    pub lexicals: BTreeMap<JsStr, (Value, bool)>,
    pub symbols: Vec<SymbolInfo>,
    pub registry: BTreeMap<JsStr, Sym>,
    pub jobs: VecDeque<Job>,
    pub host: Box<dyn Host>,
    pub templates: BTreeMap<u32, ObjRef>,
    roots: Vec<Option<Value>>,
    free_roots: Vec<usize>,
    pub(crate) run_depth: u32,
    /// Instructions run since the budget was reset; 0 limit = unlimited.
    pub steps: u64,
    pub step_limit: u64,
    /// Set when the budget runs out: the script unwinds without running
    /// catch or finally blocks.
    pub interrupted: bool,
    pub rng: u64,
    /// Promises rejected with no handler (for the host to report).
    pub unhandled: Vec<ObjRef>,
    /// The last generator suspension passed an inner result through (yield*).
    pub(crate) raw_yield: bool,
    /// Objects being joined or stringified (cycle detection).
    pub(crate) join_stack: Vec<ObjRef>,
    pub(crate) console_indent: usize,
    pub(crate) console_timers: BTreeMap<String, f64>,
    pub(crate) console_counts: BTreeMap<String, u32>,
    /// The embedder's state (see [`Embedder`]).
    pub embedder: Option<Box<dyn Embedder>>,
}

/// A persistent root: keeps a value alive while the embedder holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Root(usize);

const MAX_FRAMES: usize = 10_000;
const MAX_DEPTH: u32 = 200;

impl Realm {
    pub fn new(host: Box<dyn Host>) -> Realm {
        let placeholder = ObjRef(u32::MAX);
        let mut rt = Realm {
            heap: Heap::default(),
            stack: Vec::with_capacity(1024),
            frames: Vec::new(),
            intr: Intrinsics {
                object_proto: placeholder,
                function_proto: placeholder,
                array_proto: placeholder,
                string_proto: placeholder,
                number_proto: placeholder,
                boolean_proto: placeholder,
                symbol_proto: placeholder,
                errors: [placeholder; 8],
                error_ctors: [placeholder; 8],
                iterator_proto: placeholder,
                async_iterator_proto: placeholder,
                array_iterator_proto: placeholder,
                string_iterator_proto: placeholder,
                map_iterator_proto: placeholder,
                set_iterator_proto: placeholder,
                regexp_string_iterator_proto: placeholder,
                generator_proto: placeholder,
                async_generator_proto: placeholder,
                generator_function_proto: placeholder,
                async_function_proto: placeholder,
                async_generator_function_proto: placeholder,
                async_from_sync_iterator_proto: placeholder,
                iterator_helper_proto: placeholder,
                wrap_for_valid_iterator_proto: placeholder,
                promise_proto: placeholder,
                array_buffer_proto: placeholder,
                array_buffer_ctor: placeholder,
                data_view_proto: placeholder,
                typed_protos: [placeholder; 9],
                typed_ctors: [placeholder; 9],
                promise_ctor: placeholder,
                map_proto: placeholder,
                set_proto: placeholder,
                weakmap_proto: placeholder,
                weakset_proto: placeholder,
                regexp_proto: placeholder,
                regexp_ctor: placeholder,
                regexp_exec: placeholder,
                date_proto: placeholder,
                array_ctor: placeholder,
                object_ctor: placeholder,
                function_ctor: placeholder,
                array_values: placeholder,
                array_iterator_next: placeholder,
                throw_type_error: placeholder,
                eval: placeholder,
            },
            global: placeholder,
            lexicals: BTreeMap::new(),
            symbols: Vec::new(),
            registry: BTreeMap::new(),
            jobs: VecDeque::new(),
            host,
            templates: BTreeMap::new(),
            roots: Vec::new(),
            free_roots: Vec::new(),
            run_depth: 0,
            steps: 0,
            step_limit: 0,
            interrupted: false,
            rng: 0,
            unhandled: Vec::new(),
            raw_yield: false,
            join_stack: Vec::new(),
            console_indent: 0,
            console_timers: BTreeMap::new(),
            console_counts: BTreeMap::new(),
            embedder: None,
        };
        rt.rng = rt.host.seed() | 1;
        for name in Sym::WELL_KNOWN {
            rt.symbols.push(SymbolInfo { description: Some(JsStr::from(format!("Symbol.{name}"))), private: false });
        }
        crate::builtins::setup(&mut rt);
        rt
    }

    // ------------------------------------------------------------ running

    /// Compiles and runs a script; returns its completion value.
    pub fn eval(&mut self, src: &str, file: &str) -> JsResult {
        let code = self.compile(src, file)?;
        self.run_code(code)
    }

    pub fn compile(&mut self, src: &str, file: &str) -> Result<Rc<Code>, Value> {
        let script = crate::parser::parse_script(src).map_err(|e| self.syntax_error(&e, file))?;
        crate::compiler::compile_script(&script, Rc::from(src), Rc::from(file)).map_err(|e| self.syntax_error(&e, file))
    }

    fn syntax_error(&mut self, e: &crate::lexer::SyntaxError, file: &str) -> Value {
        let msg = if e.line > 0 { format!("{} ({}:{}:{})", e.message, file, e.line, e.col) } else { e.message.clone() };
        self.error(ErrorKind::SyntaxError, &msg)
    }

    pub fn run_code(&mut self, code: Rc<Code>) -> JsResult {
        let this = Value::Object(self.global);
        self.call_code(code, this)
    }

    /// Runs queued jobs (promise reactions, host callbacks) until none are left.
    pub fn run_jobs(&mut self) {
        while let Some(job) = self.jobs.pop_front() {
            if self.interrupted {
                self.jobs.clear();
                break;
            }
            let r = match job {
                Job::Call(f, args) => self.call(&f, Value::Undefined, &args).map(|_| ()),
                Job::Reaction { reaction_handler, fulfill, capability, argument } => {
                    crate::builtins::promise::run_reaction(self, reaction_handler, fulfill, capability, argument)
                }
                Job::Thenable { promise, thenable, then } => {
                    crate::builtins::promise::run_thenable(self, promise, thenable, then)
                }
            };
            if let Err(e) = r {
                let msg = self.describe_error(&e);
                self.host.console("error", &format!("Uncaught {msg}"));
            }
            self.maybe_collect();
        }
    }

    /// Reports promises that were rejected and never handled, once each.
    pub fn report_unhandled(&mut self) {
        let list = core::mem::take(&mut self.unhandled);
        for p in list {
            let (handled, value) = match &self.heap.get(p).kind {
                Kind::Promise(pd) => (pd.handled, pd.value.clone()),
                _ => continue,
            };
            if !handled {
                let msg = self.describe_error(&value);
                self.host.console("error", &format!("Uncaught (in promise) {msg}"));
            }
        }
    }

    /// "TypeError: x is not a function" for an error, or the value as text.
    pub fn describe_error(&mut self, e: &Value) -> String {
        if let Value::Object(o) = e {
            if matches!(self.heap.get(*o).kind, Kind::Error) {
                let stack = self.get(*o, &PropKey::from("stack"), e.clone()).unwrap_or(Value::Undefined);
                if let Value::String(s) = stack {
                    return s.to_rust();
                }
            }
        }
        match self.to_string(e) {
            Ok(s) => s.to_rust(),
            Err(_) => String::from("(an error that can't be described)"),
        }
    }

    pub fn maybe_collect(&mut self) {
        if self.run_depth == 0 && self.heap.wants_collection() {
            self.collect();
        }
    }

    pub(crate) fn maybe_collect_in_run(&mut self) {
        if self.run_depth == 1 && self.heap.wants_collection() {
            self.collect();
        }
    }

    pub fn collect(&mut self) {
        let mut roots: Vec<ObjRef> = Vec::new();
        let push = |v: &Value, roots: &mut Vec<ObjRef>| {
            if let Value::Object(o) = v {
                roots.push(*o);
            }
        };
        for v in &self.stack {
            push(v, &mut roots);
        }
        for f in &self.frames {
            f.trace(&mut roots);
        }
        roots.push(self.global);
        roots.extend(self.intr.all());
        for (v, _) in self.lexicals.values() {
            push(v, &mut roots);
        }
        for j in &self.jobs {
            match j {
                Job::Call(f, args) => {
                    push(f, &mut roots);
                    args.iter().for_each(|a| push(a, &mut roots));
                }
                Job::Reaction { reaction_handler, capability, argument, .. } => {
                    push(reaction_handler, &mut roots);
                    push(argument, &mut roots);
                    if let Some((a, b, c)) = capability {
                        push(a, &mut roots);
                        push(b, &mut roots);
                        push(c, &mut roots);
                    }
                }
                Job::Thenable { promise, thenable, then } => {
                    roots.push(*promise);
                    push(thenable, &mut roots);
                    push(then, &mut roots);
                }
            }
        }
        roots.extend(self.templates.values().copied());
        for r in self.roots.iter().flatten() {
            push(r, &mut roots);
        }
        roots.extend(self.unhandled.iter().copied());
        if let Some(e) = &self.embedder {
            e.trace(&mut roots);
        }
        self.heap.collect(roots);
    }

    /// Keeps `v` alive until `unroot`.
    pub fn root(&mut self, v: Value) -> Root {
        match self.free_roots.pop() {
            Some(i) => {
                self.roots[i] = Some(v);
                Root(i)
            }
            None => {
                self.roots.push(Some(v));
                Root(self.roots.len() - 1)
            }
        }
    }

    pub fn rooted(&self, r: Root) -> Value {
        self.roots[r.0].clone().unwrap_or(Value::Undefined)
    }

    pub fn unroot(&mut self, r: Root) {
        self.roots[r.0] = None;
        self.free_roots.push(r.0);
    }

    pub(crate) fn enter_run(&mut self) -> Result<(), Value> {
        if self.run_depth >= MAX_DEPTH || self.frames.len() >= MAX_FRAMES {
            return Err(self.error(ErrorKind::RangeError, "Maximum call stack size exceeded"));
        }
        self.run_depth += 1;
        Ok(())
    }

    pub(crate) fn check_frames(&mut self) -> Result<(), Value> {
        if self.frames.len() >= MAX_FRAMES {
            return Err(self.error(ErrorKind::RangeError, "Maximum call stack size exceeded"));
        }
        Ok(())
    }

    /// Random bytes: the host's generator, else the engine's.
    pub fn random_bytes(&mut self, buf: &mut [u8]) {
        if !self.host.fill_random(buf) {
            for b in buf.iter_mut() {
                *b = (self.random() * 256.0) as u8;
            }
        }
    }

    pub fn random(&mut self) -> f64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        let r = x.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (r >> 11) as f64 / (1u64 << 53) as f64
    }

    // ------------------------------------------------------------ allocation

    pub fn alloc(&mut self, obj: Obj) -> ObjRef {
        self.heap.alloc(obj)
    }

    pub fn new_object(&mut self) -> ObjRef {
        let p = self.intr.object_proto;
        self.alloc(Obj::new(Some(p), Kind::Ordinary))
    }

    pub fn new_object_with(&mut self, proto: Option<ObjRef>) -> ObjRef {
        self.alloc(Obj::new(proto, Kind::Ordinary))
    }

    pub fn new_array(&mut self, items: Vec<Value>) -> ObjRef {
        let p = self.intr.array_proto;
        let len = items.len() as u32;
        self.alloc(Obj::new(Some(p), Kind::Array(ArrayStore { dense: items, len, len_writable: true })))
    }

    pub fn new_symbol(&mut self, description: Option<JsStr>) -> Sym {
        self.symbols.push(SymbolInfo { description, private: false });
        Sym(self.symbols.len() as u32 - 1)
    }

    pub fn new_private(&mut self, description: JsStr) -> Sym {
        self.symbols.push(SymbolInfo { description: Some(description), private: true });
        Sym(self.symbols.len() as u32 - 1)
    }

    pub fn is_private(&self, s: Sym) -> bool {
        self.symbols[s.0 as usize].private
    }

    pub fn error(&mut self, kind: ErrorKind, msg: &str) -> Value {
        let proto = self.intr.errors[kind as usize];
        let o = self.alloc(Obj::new(Some(proto), Kind::Error));
        if !msg.is_empty() {
            self.define(o, "message", Value::str(msg), HIDDEN);
        }
        self.attach_stack(o, kind.name(), msg);
        Value::Object(o)
    }

    /// Sets `stack`: the message and the current call stack.
    pub fn attach_stack(&mut self, o: ObjRef, name: &str, msg: &str) {
        let mut s = if msg.is_empty() { String::from(name) } else { format!("{name}: {msg}") };
        for f in self.frames.iter().rev().take(10) {
            let fname = f.code.name.to_rust();
            let line = f.code.line_at(f.pc.saturating_sub(1));
            let fname = if fname.is_empty() {
                if f.code.kind == crate::bytecode::CodeKind::Script {
                    String::from("<script>")
                } else {
                    String::from("<anonymous>")
                }
            } else {
                fname
            };
            if line > 0 {
                s.push_str(&format!("\n    at {} ({}:{})", fname, f.code.file, line));
            } else {
                s.push_str(&format!("\n    at {} ({})", fname, f.code.file));
            }
        }
        self.define(o, "stack", Value::String(JsStr::from(s)), HIDDEN);
    }

    pub fn type_error(&mut self, msg: &str) -> Value {
        self.error(ErrorKind::TypeError, msg)
    }

    pub fn range_error(&mut self, msg: &str) -> Value {
        self.error(ErrorKind::RangeError, msg)
    }

    pub fn reference_error(&mut self, msg: &str) -> Value {
        self.error(ErrorKind::ReferenceError, msg)
    }

    /// Defines a data property directly (no checks).
    pub fn define(&mut self, o: ObjRef, key: impl Into<PropKey>, v: Value, flags: u8) {
        let key = key.into();
        if let Kind::Array(_) = self.heap.get(o).kind {
            let _ = self.define_own(o, key, PropDesc::data(v, flags));
            return;
        }
        self.heap.get_mut(o).props.insert(key, Prop::data(v, flags));
    }

    pub fn define_accessor(&mut self, o: ObjRef, key: impl Into<PropKey>, get: Value, set: Value, flags: u8) {
        self.heap.get_mut(o).props.insert(key.into(), Prop { slot: Slot::Accessor(get, set), flags });
    }

    /// Creates a native function object.
    pub fn native(&mut self, name: &str, length: u32, f: NativeFn, ctor: bool) -> ObjRef {
        self.native_with(name, length, f, ctor, Vec::new())
    }

    pub fn native_with(&mut self, name: &str, length: u32, f: NativeFn, ctor: bool, slots: Vec<Value>) -> ObjRef {
        let proto = self.intr.function_proto;
        let o = self.alloc(Obj::new(Some(proto), Kind::Native(Box::new(Native { f, ctor, slots }))));
        self.define(o, "length", Value::Number(length as f64), CONFIGURABLE);
        self.define(o, "name", Value::str(name), CONFIGURABLE);
        o
    }

    /// Adds a native method to `o`.
    pub fn method(&mut self, o: ObjRef, name: &str, length: u32, f: NativeFn) -> ObjRef {
        let fun = self.native(name, length, f, false);
        self.define(o, name, Value::Object(fun), HIDDEN);
        fun
    }

    pub fn method_sym(&mut self, o: ObjRef, sym: Sym, name: &str, length: u32, f: NativeFn) -> ObjRef {
        let fun = self.native(name, length, f, false);
        self.define(o, sym, Value::Object(fun), HIDDEN);
        fun
    }

    pub fn getter(&mut self, o: ObjRef, name: &str, f: NativeFn) {
        let fun = self.native(&format!("get {name}"), 0, f, false);
        self.define_accessor(o, name, Value::Object(fun), Value::Undefined, CONFIGURABLE);
    }

    pub fn native_slots(&self, callee: ObjRef) -> &[Value] {
        match &self.heap.get(callee).kind {
            Kind::Native(n) => &n.slots,
            _ => &[],
        }
    }

    pub fn array_from(&mut self, items: Vec<Value>) -> Value {
        Value::Object(self.new_array(items))
    }

    // ------------------------------------------------------------ properties

    pub fn proto_of(&self, o: ObjRef) -> Option<ObjRef> {
        self.heap.get(o).proto
    }

    // ------------------------------------------------------------ typed arrays

    /// CanonicalNumericIndexString: the number a key names, if it names one.
    pub fn numeric_key(key: &PropKey) -> Option<f64> {
        let PropKey::Str(s) = key else { return None };
        if let Some(i) = s.as_index() {
            return Some(i as f64);
        }
        if s.eq_str("-0") {
            return Some(-0.0);
        }
        let n = numconv::parse(s.units());
        if s.is_empty() || !s.eq_str(&numconv::to_string(n)) {
            return None;
        }
        Some(n)
    }

    /// For a typed array: (kind, buffer, byte offset, length), with length 0
    /// once its buffer is detached.
    pub fn typed_array(&self, o: ObjRef) -> Option<(TAKind, ObjRef, usize, usize)> {
        match &self.heap.get(o).kind {
            Kind::TypedArray(t) => {
                let detached = matches!(&self.heap.get(t.buffer).kind, Kind::ArrayBuffer(b) if b.detached);
                Some((t.kind, t.buffer, t.offset, if detached { 0 } else { t.length }))
            }
            _ => None,
        }
    }

    /// IsValidIntegerIndex, as an element index.
    pub fn ta_index(&self, o: ObjRef, n: f64) -> Option<usize> {
        let (_, _, _, len) = self.typed_array(o)?;
        if n != libm::trunc(n) || (n == 0.0 && n.is_sign_negative()) || n < 0.0 || n >= len as f64 {
            return None;
        }
        Some(n as usize)
    }

    pub fn ta_get(&self, o: ObjRef, i: usize) -> f64 {
        let (kind, buf, off, _) = self.typed_array(o).unwrap();
        let at = off + i * kind.size();
        match &self.heap.get(buf).kind {
            Kind::ArrayBuffer(b) => kind.read(&b.bytes[at..at + kind.size()]),
            _ => f64::NAN,
        }
    }

    pub fn ta_put(&mut self, o: ObjRef, i: usize, v: f64) {
        let (kind, buf, off, _) = self.typed_array(o).unwrap();
        let at = off + i * kind.size();
        if let Kind::ArrayBuffer(b) = &mut self.heap.get_mut(buf).kind {
            if at + kind.size() <= b.bytes.len() {
                kind.write(&mut b.bytes[at..at + kind.size()], v);
            }
        }
    }

    pub fn get_own_property(&self, o: ObjRef, key: &PropKey) -> Option<Prop> {
        let obj = self.heap.get(o);
        if let Kind::TypedArray(_) = obj.kind {
            if let Some(n) = Self::numeric_key(key) {
                let i = self.ta_index(o, n)?;
                return Some(Prop::data(Value::Number(self.ta_get(o, i)), DEFAULT));
            }
        }
        match &obj.kind {
            Kind::Array(a) => {
                if let Some(i) = key.as_index() {
                    if let Some(v) = a.dense.get(i as usize) {
                        if *v != Value::Empty {
                            return Some(Prop::data(v.clone(), DEFAULT));
                        }
                        return None;
                    }
                } else if key.is_str("length") {
                    return Some(Prop::data(Value::Number(a.len as f64), if a.len_writable { WRITABLE } else { 0 }));
                }
            }
            Kind::Primitive(Value::String(s)) => {
                if let Some(i) = key.as_index() {
                    if (i as usize) < s.len() {
                        return Some(Prop::data(Value::String(s.slice(i as usize, i as usize + 1)), ENUMERABLE));
                    }
                } else if key.is_str("length") {
                    return Some(Prop::data(Value::Number(s.len() as f64), 0));
                }
            }
            _ => {}
        }
        obj.props.get(key).cloned()
    }

    pub fn get(&mut self, o: ObjRef, key: &PropKey, receiver: Value) -> JsResult {
        let mut cur = o;
        loop {
            if let Kind::TypedArray(_) = self.heap.get(cur).kind {
                if let Some(n) = Self::numeric_key(key) {
                    return Ok(match self.ta_index(cur, n) {
                        Some(i) => Value::Number(self.ta_get(cur, i)),
                        None => Value::Undefined,
                    });
                }
            }
            if let Some(p) = self.get_own_property(cur, key) {
                return match p.slot {
                    Slot::Data(v) => Ok(v),
                    Slot::Accessor(g, _) => {
                        if g.is_undefined() {
                            Ok(Value::Undefined)
                        } else {
                            self.call(&g, receiver, &[])
                        }
                    }
                };
            }
            match self.heap.get(cur).proto {
                Some(p) => cur = p,
                None => return Ok(Value::Undefined),
            }
        }
    }

    pub fn get_str(&mut self, o: ObjRef, key: &str) -> JsResult {
        self.get(o, &PropKey::from(key), Value::Object(o))
    }

    /// GetV: property lookup on any value (primitives use their prototype).
    pub fn get_v(&mut self, v: &Value, key: &PropKey) -> JsResult {
        match v {
            Value::Object(o) => self.get(*o, key, v.clone()),
            Value::String(s) => {
                if let Some(i) = key.as_index() {
                    if (i as usize) < s.len() {
                        return Ok(Value::String(s.slice(i as usize, i as usize + 1)));
                    }
                } else if key.is_str("length") {
                    return Ok(Value::Number(s.len() as f64));
                }
                let p = self.intr.string_proto;
                self.get(p, key, v.clone())
            }
            Value::Number(_) => {
                let p = self.intr.number_proto;
                self.get(p, key, v.clone())
            }
            Value::Bool(_) => {
                let p = self.intr.boolean_proto;
                self.get(p, key, v.clone())
            }
            Value::Symbol(_) => {
                let p = self.intr.symbol_proto;
                self.get(p, key, v.clone())
            }
            Value::Undefined | Value::Null | Value::Empty => {
                let k = self.key_display(key);
                let what = if matches!(v, Value::Null) { "null" } else { "undefined" };
                Err(self.type_error(&format!("Cannot read properties of {what} (reading '{k}')")))
            }
        }
    }

    pub fn key_display(&self, key: &PropKey) -> String {
        match key {
            PropKey::Str(s) => s.to_rust(),
            PropKey::Sym(s) => {
                let d = self.symbols[s.0 as usize].description.clone().unwrap_or_else(JsStr::empty);
                format!("Symbol({d})")
            }
        }
    }

    /// GetMethod: undefined/null → None; non-callable → TypeError.
    pub fn get_method(&mut self, v: &Value, key: &PropKey) -> Result<Option<Value>, Value> {
        let f = self.get_v(v, key)?;
        if f.is_nullish() {
            return Ok(None);
        }
        if !self.is_callable(&f) {
            let k = self.key_display(key);
            return Err(self.type_error(&format!("{k} is not a function")));
        }
        Ok(Some(f))
    }

    pub fn define_own(&mut self, o: ObjRef, key: PropKey, desc: PropDesc) -> Result<bool, Value> {
        if let Kind::TypedArray(_) = self.heap.get(o).kind {
            if let Some(n) = Self::numeric_key(&key) {
                let Some(i) = self.ta_index(o, n) else { return Ok(false) };
                if desc.configurable == Some(false)
                    || desc.enumerable == Some(false)
                    || desc.is_accessor()
                    || desc.writable == Some(false)
                {
                    return Ok(false);
                }
                if let Some(v) = desc.value {
                    let x = self.to_number(&v)?;
                    // The conversion may have detached the buffer.
                    if let Some(i) = self.ta_index(o, i as f64) {
                        self.ta_put(o, i, x);
                    }
                }
                return Ok(true);
            }
        }
        // Arrays: `length` and indices.
        if let Kind::Array(_) = self.heap.get(o).kind {
            if key.is_str("length") {
                return self.array_set_length(o, desc);
            }
            if let Some(i) = key.as_index() {
                return self.array_define_index(o, i, desc);
            }
        }
        if let Kind::Primitive(Value::String(s)) = &self.heap.get(o).kind {
            let len = s.len();
            if key.as_index().is_some_and(|i| (i as usize) < len) || key.is_str("length") {
                let cur = self.get_own_property(o, &key).unwrap();
                return Ok(Self::compatible(&cur, &desc, self));
            }
        }
        Ok(self.ordinary_define(o, key, desc))
    }

    /// Whether `desc` can be applied to the non-configurable `cur` without change.
    fn compatible(cur: &Prop, desc: &PropDesc, rt: &Realm) -> bool {
        if desc.configurable == Some(true) || desc.enumerable.is_some_and(|e| e != cur.enumerable()) {
            return false;
        }
        match &cur.slot {
            Slot::Data(v) => {
                if desc.is_accessor() {
                    return false;
                }
                if !cur.writable() {
                    if desc.writable == Some(true) {
                        return false;
                    }
                    if let Some(nv) = &desc.value {
                        if !rt.same_value(nv, v) {
                            return false;
                        }
                    }
                }
                true
            }
            Slot::Accessor(g, s) => {
                if desc.is_data() {
                    return false;
                }
                desc.get.as_ref().is_none_or(|x| rt.same_value(x, g))
                    && desc.set.as_ref().is_none_or(|x| rt.same_value(x, s))
            }
        }
    }

    pub fn ordinary_define(&mut self, o: ObjRef, key: PropKey, desc: PropDesc) -> bool {
        let current = self.heap.get(o).props.get(&key).cloned();
        let extensible = self.heap.get(o).extensible;
        match current {
            None => {
                if !extensible {
                    return false;
                }
                let mut flags = 0;
                if desc.enumerable == Some(true) {
                    flags |= ENUMERABLE;
                }
                if desc.configurable == Some(true) {
                    flags |= CONFIGURABLE;
                }
                let slot = if desc.is_accessor() {
                    Slot::Accessor(desc.get.unwrap_or(Value::Undefined), desc.set.unwrap_or(Value::Undefined))
                } else {
                    if desc.writable == Some(true) {
                        flags |= WRITABLE;
                    }
                    Slot::Data(desc.value.unwrap_or(Value::Undefined))
                };
                self.heap.get_mut(o).props.insert(key, Prop { slot, flags });
                true
            }
            Some(cur) => {
                if !cur.configurable() && !Self::compatible(&cur, &desc, self) {
                    return false;
                }
                let mut p = cur;
                if desc.is_accessor() && matches!(p.slot, Slot::Data(_)) {
                    p.slot = Slot::Accessor(Value::Undefined, Value::Undefined);
                    p.flags &= !WRITABLE;
                } else if desc.is_data() && matches!(p.slot, Slot::Accessor(..)) {
                    p.slot = Slot::Data(Value::Undefined);
                    p.flags &= !WRITABLE;
                }
                match &mut p.slot {
                    Slot::Data(v) => {
                        if let Some(nv) = desc.value {
                            *v = nv;
                        }
                        if let Some(w) = desc.writable {
                            if w {
                                p.flags |= WRITABLE;
                            } else {
                                p.flags &= !WRITABLE;
                            }
                        }
                    }
                    Slot::Accessor(g, s) => {
                        if let Some(ng) = desc.get {
                            *g = ng;
                        }
                        if let Some(ns) = desc.set {
                            *s = ns;
                        }
                    }
                }
                if let Some(e) = desc.enumerable {
                    if e {
                        p.flags |= ENUMERABLE;
                    } else {
                        p.flags &= !ENUMERABLE;
                    }
                }
                if let Some(c) = desc.configurable {
                    if c {
                        p.flags |= CONFIGURABLE;
                    } else {
                        p.flags &= !CONFIGURABLE;
                    }
                }
                self.heap.get_mut(o).props.insert(key, p);
                true
            }
        }
    }

    /// Moves an array's dense elements into ordinary properties.
    fn array_make_sparse(&mut self, o: ObjRef) {
        let dense = match &mut self.heap.get_mut(o).kind {
            Kind::Array(a) => core::mem::take(&mut a.dense),
            _ => return,
        };
        let obj = self.heap.get_mut(o);
        // Rebuild props with index keys first (they sort first anyway).
        for (i, v) in dense.into_iter().enumerate() {
            if v != Value::Empty {
                obj.props.insert(PropKey::index(i as u32), Prop::data(v, DEFAULT));
            }
        }
    }

    fn array_define_index(&mut self, o: ObjRef, i: u32, desc: PropDesc) -> Result<bool, Value> {
        let (len, len_writable, dense_len, extensible) = match &self.heap.get(o).kind {
            Kind::Array(a) => (a.len, a.len_writable, a.dense.len(), self.heap.get(o).extensible),
            _ => unreachable!(),
        };
        if i >= len && !len_writable {
            return Ok(false);
        }
        let plain = !desc.is_accessor()
            && desc.writable != Some(false)
            && desc.enumerable != Some(false)
            && desc.configurable != Some(false);
        let idx = i as usize;
        if plain && extensible && self.heap.get(o).props.is_empty_of_indices() {
            if idx < dense_len {
                let is_new = matches!(&self.heap.get(o).kind, Kind::Array(a) if a.dense[idx] == Value::Empty);
                // A new element via a partial descriptor gets false defaults.
                if !is_new
                    || (desc.writable == Some(true) && desc.enumerable == Some(true) && desc.configurable == Some(true))
                {
                    if let Kind::Array(a) = &mut self.heap.get_mut(o).kind {
                        if let Some(v) = desc.value {
                            a.dense[idx] = v;
                        } else if is_new {
                            a.dense[idx] = Value::Undefined;
                        }
                    }
                    return Ok(true);
                }
            } else if idx <= dense_len + 1024
                && desc.writable == Some(true)
                && desc.enumerable == Some(true)
                && desc.configurable == Some(true)
            {
                if let Kind::Array(a) = &mut self.heap.get_mut(o).kind {
                    a.dense.resize(idx, Value::Empty);
                    a.dense.push(desc.value.unwrap_or(Value::Undefined));
                    if i >= a.len {
                        a.len = i + 1;
                    }
                }
                return Ok(true);
            }
        }
        // Slow path: ordinary properties.
        self.array_make_sparse(o);
        let ok = self.ordinary_define(o, PropKey::index(i), desc);
        if ok && i >= len {
            if let Kind::Array(a) = &mut self.heap.get_mut(o).kind {
                a.len = i + 1;
            }
        }
        Ok(ok)
    }

    fn array_set_length(&mut self, o: ObjRef, desc: PropDesc) -> Result<bool, Value> {
        let Some(v) = desc.value.clone() else {
            // Only attributes.
            if desc.configurable == Some(true) || desc.enumerable == Some(true) || desc.is_accessor() {
                return Ok(false);
            }
            if let Kind::Array(a) = &mut self.heap.get_mut(o).kind {
                if desc.writable == Some(true) && !a.len_writable {
                    return Ok(false);
                }
                if desc.writable == Some(false) {
                    a.len_writable = false;
                }
            }
            return Ok(true);
        };
        let n = self.to_number(&v)?;
        let new_len = numconv::to_uint32(n);
        let n2 = self.to_number(&v)?;
        if new_len as f64 != n2 {
            return Err(self.range_error("Invalid array length"));
        }
        if desc.configurable == Some(true) || desc.enumerable == Some(true) || desc.is_accessor() {
            return Ok(false);
        }
        let (len, writable) = match &self.heap.get(o).kind {
            Kind::Array(a) => (a.len, a.len_writable),
            _ => unreachable!(),
        };
        if new_len != len && !writable {
            return Ok(false);
        }
        if desc.writable == Some(true) && !writable {
            return Ok(false);
        }
        let obj = self.heap.get_mut(o);
        if new_len < len {
            // Delete elements from the end; stop at a non-configurable one.
            let mut keys: Vec<(u32, PropKey)> = obj
                .props
                .keys()
                .filter_map(|k| k.as_index().filter(|&i| i >= new_len).map(|i| (i, k.clone())))
                .collect();
            keys.sort_by_key(|k| core::cmp::Reverse(k.0));
            let mut final_len = new_len;
            for (i, k) in keys {
                if obj.props.get(&k).is_some_and(|p| !p.configurable()) {
                    final_len = i + 1;
                    break;
                }
                obj.props.remove(&k);
            }
            if let Kind::Array(a) = &mut obj.kind {
                if a.dense.len() > final_len as usize {
                    a.dense.truncate(final_len as usize);
                }
                a.len = final_len;
                if desc.writable == Some(false) {
                    a.len_writable = false;
                }
                if final_len != new_len {
                    return Ok(false);
                }
            }
        } else if let Kind::Array(a) = &mut obj.kind {
            a.len = new_len;
            if desc.writable == Some(false) {
                a.len_writable = false;
            }
        }
        Ok(true)
    }

    /// OrdinarySet, through the prototype chain.
    pub fn set(&mut self, o: ObjRef, key: PropKey, v: Value, receiver: Value) -> Result<bool, Value> {
        if let Kind::TypedArray(_) = self.heap.get(o).kind {
            if let Some(n) = Self::numeric_key(&key) {
                if receiver == Value::Object(o) {
                    // TypedArraySetElement: convert, then write if still in bounds.
                    let x = self.to_number(&v)?;
                    if let Some(i) = self.ta_index(o, n) {
                        self.ta_put(o, i, x);
                    }
                    return Ok(true);
                }
                if self.ta_index(o, n).is_none() {
                    return Ok(true);
                }
            }
        }
        // Fast path: an existing writable own data property on the receiver.
        if receiver == Value::Object(o) {
            let obj = self.heap.get_mut(o);
            if let Kind::Array(a) = &mut obj.kind {
                if let Some(i) = key.as_index() {
                    let i = i as usize;
                    if i < a.dense.len() && a.dense[i] != Value::Empty {
                        a.dense[i] = v;
                        return Ok(true);
                    }
                }
            } else if let Some(p) = obj.props.get_mut(&key) {
                if let (Slot::Data(slot), true) = (&mut p.slot, p.flags & WRITABLE != 0) {
                    *slot = v;
                    return Ok(true);
                }
            }
        }
        let mut cur = o;
        let own = loop {
            match self.get_own_property(cur, &key) {
                Some(p) => break p,
                None => match self.heap.get(cur).proto {
                    Some(p) => cur = p,
                    None => break Prop::data(Value::Undefined, DEFAULT),
                },
            }
        };
        match own.slot {
            Slot::Data(_) => {
                if !own.writable() {
                    return Ok(false);
                }
                let Value::Object(r) = receiver else { return Ok(false) };
                match self.get_own_property(r, &key) {
                    Some(existing) => {
                        if matches!(existing.slot, Slot::Accessor(..)) || !existing.writable() {
                            return Ok(false);
                        }
                        self.define_own(r, key, PropDesc { value: Some(v), ..PropDesc::default() })
                    }
                    None => self.define_own(r, key, PropDesc::data(v, DEFAULT)),
                }
            }
            Slot::Accessor(_, setter) => {
                if setter.is_undefined() {
                    return Ok(false);
                }
                self.call(&setter, receiver, &[v])?;
                Ok(true)
            }
        }
    }

    /// `obj[key] = v` for any value: throws in strict mode when it fails.
    pub fn put(&mut self, target: &Value, key: PropKey, v: Value, strict: bool) -> Result<(), Value> {
        let ok = match target {
            Value::Object(o) => self.set(*o, key.clone(), v, target.clone())?,
            Value::Undefined | Value::Null | Value::Empty => {
                let k = self.key_display(&key);
                let what = if matches!(target, Value::Null) { "null" } else { "undefined" };
                return Err(self.type_error(&format!("Cannot set properties of {what} (setting '{k}')")));
            }
            prim => {
                let proto = match prim {
                    Value::String(_) => self.intr.string_proto,
                    Value::Number(_) => self.intr.number_proto,
                    Value::Bool(_) => self.intr.boolean_proto,
                    _ => self.intr.symbol_proto,
                };
                if let Value::String(s) = prim {
                    if key.is_str("length") || key.as_index().is_some_and(|i| (i as usize) < s.len()) {
                        false
                    } else {
                        self.set(proto, key.clone(), v, prim.clone())?
                    }
                } else {
                    self.set(proto, key.clone(), v, prim.clone())?
                }
            }
        };
        if !ok && strict {
            let k = self.key_display(&key);
            return Err(self.type_error(&format!("Cannot assign to read only property '{k}'")));
        }
        Ok(())
    }

    pub fn set_str(&mut self, o: ObjRef, key: &str, v: Value) -> Result<(), Value> {
        let ok = self.set(o, PropKey::from(key), v, Value::Object(o))?;
        if !ok {
            return Err(self.type_error(&format!("Cannot assign to read only property '{key}'")));
        }
        Ok(())
    }

    /// CreateDataProperty.
    pub fn create_data_property(&mut self, o: ObjRef, key: PropKey, v: Value) -> Result<bool, Value> {
        self.define_own(o, key, PropDesc::data(v, DEFAULT))
    }

    pub fn create_data_property_or_throw(&mut self, o: ObjRef, key: PropKey, v: Value) -> Result<(), Value> {
        if !self.create_data_property(o, key.clone(), v)? {
            let k = self.key_display(&key);
            return Err(self.type_error(&format!("Cannot define property {k}")));
        }
        Ok(())
    }

    pub fn has_property(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        let mut cur = o;
        loop {
            if let Kind::TypedArray(_) = self.heap.get(cur).kind {
                if let Some(n) = Self::numeric_key(key) {
                    return Ok(self.ta_index(cur, n).is_some());
                }
            }
            if self.get_own_property(cur, key).is_some() {
                return Ok(true);
            }
            match self.heap.get(cur).proto {
                Some(p) => cur = p,
                None => return Ok(false),
            }
        }
    }

    pub fn has_own(&self, o: ObjRef, key: &PropKey) -> bool {
        self.get_own_property(o, key).is_some()
    }

    pub fn delete(&mut self, o: ObjRef, key: &PropKey) -> Result<bool, Value> {
        if let Kind::TypedArray(_) = self.heap.get(o).kind {
            if let Some(n) = Self::numeric_key(key) {
                return Ok(self.ta_index(o, n).is_none());
            }
        }
        let obj = self.heap.get_mut(o);
        match &mut obj.kind {
            Kind::Array(a) => {
                if let Some(i) = key.as_index() {
                    let i = i as usize;
                    if i < a.dense.len() {
                        if i == a.dense.len() - 1 {
                            a.dense.pop();
                        } else {
                            a.dense[i] = Value::Empty;
                        }
                        return Ok(true);
                    }
                } else if key.is_str("length") {
                    return Ok(false);
                }
            }
            Kind::Primitive(Value::String(s))
                if (key.is_str("length") || key.as_index().is_some_and(|i| (i as usize) < s.len())) =>
            {
                return Ok(false);
            }
            _ => {}
        }
        match obj.props.get(key) {
            None => Ok(true),
            Some(p) if p.configurable() => {
                obj.props.remove(key);
                Ok(true)
            }
            Some(_) => Ok(false),
        }
    }

    /// OwnPropertyKeys: indices ascending, then strings, then symbols.
    pub fn own_keys(&self, o: ObjRef) -> Vec<PropKey> {
        let obj = self.heap.get(o);
        let mut indices: Vec<u32> = Vec::new();
        let mut strings = Vec::new();
        let mut symbols = Vec::new();
        match &obj.kind {
            Kind::Array(a) => {
                for (i, v) in a.dense.iter().enumerate() {
                    if *v != Value::Empty {
                        indices.push(i as u32);
                    }
                }
            }
            Kind::Primitive(Value::String(s)) => indices.extend(0..s.len() as u32),
            Kind::TypedArray(_) => {
                let len = self.typed_array(o).map_or(0, |t| t.3);
                indices.extend(0..len as u32);
            }
            _ => {}
        }
        let dense_count = indices.len();
        for k in obj.props.keys() {
            match k {
                PropKey::Str(s) => match s.as_index() {
                    Some(i) => indices.push(i),
                    None => strings.push(k.clone()),
                },
                PropKey::Sym(s) => {
                    if !self.symbols[s.0 as usize].private {
                        symbols.push(k.clone());
                    }
                }
            }
        }
        if indices.len() > dense_count {
            indices.sort_unstable();
        }
        let mut out: Vec<PropKey> = indices.into_iter().map(PropKey::index).collect();
        if let Kind::Primitive(Value::String(_)) = &obj.kind {
            out.push(PropKey::from("length"));
        }
        if let Kind::Array(_) = &obj.kind {
            out.push(PropKey::from("length"));
        }
        out.extend(strings);
        out.extend(symbols);
        out
    }

    pub fn is_extensible(&self, o: ObjRef) -> bool {
        self.heap.get(o).extensible
    }

    pub fn prevent_extensions(&mut self, o: ObjRef) {
        if let Kind::Array(_) = self.heap.get(o).kind {
            self.array_make_sparse(o);
        }
        self.heap.get_mut(o).extensible = false;
    }

    /// Object.freeze / Object.seal.
    pub fn set_integrity(&mut self, o: ObjRef, frozen: bool) -> Result<(), Value> {
        self.prevent_extensions(o);
        if let Kind::Array(a) = &mut self.heap.get_mut(o).kind {
            if frozen {
                a.len_writable = false;
            }
        }
        let obj = self.heap.get_mut(o);
        for (_, p) in obj.props.iter_mut() {
            p.flags &= !CONFIGURABLE;
            if frozen && matches!(p.slot, Slot::Data(_)) {
                p.flags &= !WRITABLE;
            }
        }
        Ok(())
    }

    pub fn test_integrity(&self, o: ObjRef, frozen: bool) -> bool {
        if self.is_extensible(o) {
            return false;
        }
        for k in self.own_keys(o) {
            if let Some(p) = self.get_own_property(o, &k) {
                if p.configurable() {
                    return false;
                }
                if frozen && matches!(p.slot, Slot::Data(_)) && p.writable() {
                    return false;
                }
            }
        }
        true
    }

    pub fn set_prototype(&mut self, o: ObjRef, proto: Option<ObjRef>) -> bool {
        if self.heap.get(o).proto == proto {
            return true;
        }
        if !self.heap.get(o).extensible {
            return false;
        }
        // No cycles.
        let mut p = proto;
        while let Some(x) = p {
            if x == o {
                return false;
            }
            p = self.heap.get(x).proto;
        }
        self.heap.get_mut(o).proto = proto;
        true
    }

    // ------------------------------------------------------------ calls

    pub fn is_callable(&self, v: &Value) -> bool {
        match v {
            Value::Object(o) => self.heap.get(*o).is_callable(),
            _ => false,
        }
    }

    pub fn is_constructor(&self, v: &Value) -> bool {
        let Value::Object(o) = v else { return false };
        match &self.heap.get(*o).kind {
            Kind::Function(c) => c.ctor_kind != CtorKind::None,
            Kind::Native(n) => n.ctor,
            Kind::Bound(b) => self.is_constructor(&Value::Object(b.target)),
            _ => false,
        }
    }

    pub fn call(&mut self, f: &Value, this: Value, args: &[Value]) -> JsResult {
        let Value::Object(fo) = f else {
            return Err(self.type_error("value is not a function"));
        };
        let fo = *fo;
        match &self.heap.get(fo).kind {
            Kind::Native(n) => {
                let nf = n.f;
                let call = Call { this, args: args.to_vec(), new_target: Value::Undefined, callee: fo };
                self.enter_run()?;
                let r = nf(self, &call);
                self.run_depth -= 1;
                r
            }
            Kind::Function(_) => self.call_closure(fo, this, args.to_vec(), Value::Undefined),
            Kind::Bound(b) => {
                let (target, bthis) = (b.target, b.this.clone());
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                self.call(&Value::Object(target), bthis, &all)
            }
            _ => Err(self.type_error("value is not a function")),
        }
    }

    pub fn construct(&mut self, f: &Value, args: &[Value], new_target: Option<Value>) -> JsResult {
        if !self.is_constructor(f) {
            return Err(self.type_error("value is not a constructor"));
        }
        let fo = f.as_object().unwrap();
        let nt = new_target.unwrap_or_else(|| f.clone());
        match &self.heap.get(fo).kind {
            Kind::Native(n) => {
                let nf = n.f;
                let call = Call { this: Value::Undefined, args: args.to_vec(), new_target: nt, callee: fo };
                self.enter_run()?;
                let r = nf(self, &call);
                self.run_depth -= 1;
                r
            }
            Kind::Function(_) => self.construct_closure(fo, args.to_vec(), nt),
            Kind::Bound(b) => {
                let target = Value::Object(b.target);
                let mut all = b.args.clone();
                all.extend_from_slice(args);
                let nt = if nt == *f { target.clone() } else { nt };
                self.construct(&target, &all, Some(nt))
            }
            _ => Err(self.type_error("value is not a constructor")),
        }
    }

    /// The prototype for an object created by `new_target` (subclassing).
    pub fn proto_from_ctor(&mut self, new_target: &Value, default: ObjRef) -> Result<ObjRef, Value> {
        if let Value::Object(nt) = new_target {
            let p = self.get(*nt, &PropKey::from("prototype"), new_target.clone())?;
            if let Value::Object(p) = p {
                return Ok(p);
            }
        }
        Ok(default)
    }

    // ------------------------------------------------------------ conversions

    pub fn to_primitive(&mut self, v: &Value, hint: Option<&str>) -> JsResult {
        let Value::Object(o) = v else { return Ok(v.clone()) };
        let exotic = self.get(*o, &PropKey::Sym(Sym::TO_PRIMITIVE), v.clone())?;
        if !exotic.is_nullish() {
            if !self.is_callable(&exotic) {
                return Err(self.type_error("Symbol.toPrimitive is not a function"));
            }
            let h = Value::str(hint.unwrap_or("default"));
            let r = self.call(&exotic, v.clone(), &[h])?;
            if let Value::Object(_) = r {
                return Err(self.type_error("Cannot convert object to primitive value"));
            }
            return Ok(r);
        }
        let order = if hint == Some("string") { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for m in order {
            let f = self.get(*o, &PropKey::from(m), v.clone())?;
            if self.is_callable(&f) {
                let r = self.call(&f, v.clone(), &[])?;
                if !matches!(r, Value::Object(_)) {
                    return Ok(r);
                }
            }
        }
        Err(self.type_error("Cannot convert object to primitive value"))
    }

    pub fn to_number(&mut self, v: &Value) -> Result<f64, Value> {
        Ok(match v {
            Value::Number(n) => *n,
            Value::Undefined | Value::Empty => f64::NAN,
            Value::Null => 0.0,
            Value::Bool(b) => *b as u8 as f64,
            Value::String(s) => numconv::parse(s.units()),
            Value::Symbol(_) => return Err(self.type_error("Cannot convert a Symbol value to a number")),
            Value::Object(_) => {
                let p = self.to_primitive(v, Some("number"))?;
                return self.to_number(&p);
            }
        })
    }

    pub fn to_string(&mut self, v: &Value) -> Result<JsStr, Value> {
        Ok(match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => JsStr::from(numconv::to_string(*n)),
            Value::Undefined | Value::Empty => JsStr::from("undefined"),
            Value::Null => JsStr::from("null"),
            Value::Bool(true) => JsStr::from("true"),
            Value::Bool(false) => JsStr::from("false"),
            Value::Symbol(_) => return Err(self.type_error("Cannot convert a Symbol value to a string")),
            Value::Object(_) => {
                let p = self.to_primitive(v, Some("string"))?;
                return self.to_string(&p);
            }
        })
    }

    pub fn to_rust_string(&mut self, v: &Value) -> Result<String, Value> {
        Ok(self.to_string(v)?.to_rust())
    }

    pub fn to_property_key(&mut self, v: &Value) -> Result<PropKey, Value> {
        match v {
            Value::String(s) => Ok(PropKey::Str(s.clone())),
            Value::Symbol(s) => Ok(PropKey::Sym(*s)),
            Value::Number(n) => {
                let i = *n as u32;
                if i as f64 == *n && i != u32::MAX {
                    Ok(PropKey::index(i))
                } else {
                    Ok(PropKey::Str(JsStr::from(numconv::to_string(*n))))
                }
            }
            Value::Object(_) => {
                let p = self.to_primitive(v, Some("string"))?;
                self.to_property_key(&p)
            }
            other => Ok(PropKey::Str(self.to_string(other)?)),
        }
    }

    pub fn to_object(&mut self, v: &Value) -> Result<ObjRef, Value> {
        let proto = match v {
            Value::Object(o) => return Ok(*o),
            Value::Undefined | Value::Null | Value::Empty => {
                return Err(self.type_error("Cannot convert undefined or null to object"))
            }
            Value::Bool(_) => self.intr.boolean_proto,
            Value::Number(_) => self.intr.number_proto,
            Value::String(_) => self.intr.string_proto,
            Value::Symbol(_) => self.intr.symbol_proto,
        };
        Ok(self.alloc(Obj::new(Some(proto), Kind::Primitive(v.clone()))))
    }

    pub fn to_integer(&mut self, v: &Value) -> Result<f64, Value> {
        Ok(numconv::to_integer(self.to_number(v)?))
    }

    pub fn to_int32(&mut self, v: &Value) -> Result<i32, Value> {
        Ok(numconv::to_int32(self.to_number(v)?))
    }

    pub fn to_uint32(&mut self, v: &Value) -> Result<u32, Value> {
        Ok(numconv::to_uint32(self.to_number(v)?))
    }

    pub fn to_length(&mut self, v: &Value) -> Result<f64, Value> {
        let n = self.to_integer(v)?;
        Ok(n.clamp(0.0, 9007199254740991.0))
    }

    /// The length of an array-like.
    pub fn length_of(&mut self, o: ObjRef) -> Result<f64, Value> {
        if let Kind::Array(a) = &self.heap.get(o).kind {
            return Ok(a.len as f64);
        }
        let l = self.get(o, &PropKey::from("length"), Value::Object(o))?;
        self.to_length(&l)
    }

    /// Resolves a relative index argument (slice, at, …) against `len`.
    pub fn relative_index(&mut self, v: &Value, len: f64, default: f64) -> Result<f64, Value> {
        if v.is_undefined() {
            return Ok(default);
        }
        let n = self.to_integer(v)?;
        Ok(if n < 0.0 { (len + n).max(0.0) } else { n.min(len) })
    }

    pub fn typeof_str(&self, v: &Value) -> &'static str {
        match v {
            Value::Undefined | Value::Empty => "undefined",
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Symbol(_) => "symbol",
            Value::Object(o) => {
                if self.heap.get(*o).is_callable() {
                    "function"
                } else {
                    "object"
                }
            }
        }
    }

    // ------------------------------------------------------------ equality

    pub fn strict_eq(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => x == y,
            (Value::Empty, Value::Undefined) | (Value::Undefined, Value::Empty) => true,
            _ => a == b,
        }
    }

    pub fn same_value(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => {
                (x.is_nan() && y.is_nan()) || (x == y && x.is_sign_negative() == y.is_sign_negative())
            }
            _ => self.strict_eq(a, b),
        }
    }

    pub fn same_value_zero(&self, a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(x), Value::Number(y)) => (x.is_nan() && y.is_nan()) || x == y,
            _ => self.strict_eq(a, b),
        }
    }

    pub fn loose_eq(&mut self, a: &Value, b: &Value) -> Result<bool, Value> {
        Ok(match (a, b) {
            (Value::Undefined | Value::Null | Value::Empty, Value::Undefined | Value::Null | Value::Empty) => true,
            (Value::Undefined | Value::Null | Value::Empty, _) | (_, Value::Undefined | Value::Null | Value::Empty) => {
                false
            }
            (Value::Number(_), Value::String(s)) => {
                let n = numconv::parse(s.units());
                self.strict_eq(a, &Value::Number(n))
            }
            (Value::String(s), Value::Number(_)) => {
                let n = numconv::parse(s.units());
                self.strict_eq(&Value::Number(n), b)
            }
            (Value::Bool(x), _) => return self.loose_eq(&Value::Number(*x as u8 as f64), b),
            (_, Value::Bool(y)) => return self.loose_eq(a, &Value::Number(*y as u8 as f64)),
            (Value::Object(_), Value::Object(_)) => a == b,
            (Value::Object(_), _) => {
                let p = self.to_primitive(a, None)?;
                return self.loose_eq(&p, b);
            }
            (_, Value::Object(_)) => {
                let p = self.to_primitive(b, None)?;
                return self.loose_eq(a, &p);
            }
            _ => self.strict_eq(a, b),
        })
    }

    pub fn instance_of(&mut self, v: &Value, target: &Value) -> Result<bool, Value> {
        if !matches!(target, Value::Object(_)) {
            return Err(self.type_error("Right-hand side of 'instanceof' is not an object"));
        }
        if let Some(h) = self.get_method(target, &PropKey::Sym(Sym::HAS_INSTANCE))? {
            let r = self.call(&h, target.clone(), core::slice::from_ref(v))?;
            return Ok(r.truthy());
        }
        if !self.is_callable(target) {
            return Err(self.type_error("Right-hand side of 'instanceof' is not callable"));
        }
        self.ordinary_has_instance(target, v)
    }

    pub fn ordinary_has_instance(&mut self, c: &Value, v: &Value) -> Result<bool, Value> {
        let Value::Object(co) = c else { return Ok(false) };
        if !self.is_callable(c) {
            return Ok(false);
        }
        if let Kind::Bound(b) = &self.heap.get(*co).kind {
            let t = Value::Object(b.target);
            return self.instance_of(v, &t);
        }
        let Value::Object(mut o) = v else { return Ok(false) };
        let proto = self.get(*co, &PropKey::from("prototype"), c.clone())?;
        let Value::Object(p) = proto else {
            return Err(self.type_error("Function has non-object prototype in instanceof check"));
        };
        loop {
            match self.heap.get(o).proto {
                Some(x) if x == p => return Ok(true),
                Some(x) => o = x,
                None => return Ok(false),
            }
        }
    }

    pub fn is_array(&self, v: &Value) -> bool {
        matches!(v, Value::Object(o) if matches!(self.heap.get(*o).kind, Kind::Array(_)))
    }

    // ------------------------------------------------------------ iteration

    pub fn iter_result(&mut self, value: Value, done: bool) -> Value {
        let o = self.new_object();
        self.define(o, "value", value, DEFAULT);
        self.define(o, "done", Value::Bool(done), DEFAULT);
        Value::Object(o)
    }

    pub fn get_iterator(&mut self, v: &Value) -> Result<ObjRef, Value> {
        let method = self.get_v(v, &PropKey::Sym(Sym::ITERATOR))?;
        if !self.is_callable(&method) {
            let d = self.short_describe(v);
            return Err(self.type_error(&format!("{d} is not iterable")));
        }
        // Fast path: a plain array with the built-in iterator.
        if let (Value::Object(a), Value::Object(m)) = (v, &method) {
            if *m == self.intr.array_values && matches!(self.heap.get(*a).kind, Kind::Array(_)) {
                let p = self.intr.array_iterator_proto;
                let next = self.get(p, &PropKey::from("next"), Value::Object(p))?;
                if next == Value::Object(self.intr.array_iterator_next) {
                    return Ok(self.alloc(Obj::new(
                        None,
                        Kind::IterRecord(Box::new(IterRecord {
                            iter: v.clone(),
                            next,
                            done: false,
                            fast: Some((*a, 0)),
                        })),
                    )));
                }
            }
        }
        let iter = self.call(&method, v.clone(), &[])?;
        self.iter_record_from(iter)
    }

    /// Turns a fast-path array record into one over a real iterator object
    /// (for code that drives the protocol itself: yield*, for await).
    pub fn iter_materialize(&mut self, rec: ObjRef) -> Result<(), Value> {
        let fast = match &self.heap.get(rec).kind {
            Kind::IterRecord(r) => r.fast,
            _ => None,
        };
        let Some((arr, i)) = fast else { return Ok(()) };
        let p = self.intr.array_iterator_proto;
        let iter = self.alloc(Obj::new(Some(p), Kind::ArrayIterator(Value::Object(arr), i, IterKind::Values)));
        let next = self.get(iter, &PropKey::from("next"), Value::Object(iter))?;
        if let Kind::IterRecord(r) = &mut self.heap.get_mut(rec).kind {
            r.iter = Value::Object(iter);
            r.next = next;
            r.fast = None;
        }
        Ok(())
    }

    pub fn iter_record_from(&mut self, iter: Value) -> Result<ObjRef, Value> {
        if !matches!(iter, Value::Object(_)) {
            return Err(self.type_error("Result of the Symbol.iterator method is not an object"));
        }
        let next = self.get_v(&iter, &PropKey::from("next"))?;
        Ok(self.alloc(Obj::new(None, Kind::IterRecord(Box::new(IterRecord { iter, next, done: false, fast: None })))))
    }

    /// The next value, or None when done.
    pub fn iter_step(&mut self, rec: ObjRef) -> Result<Option<Value>, Value> {
        let (next, iter, fast, done) = match &self.heap.get(rec).kind {
            Kind::IterRecord(r) => (r.next.clone(), r.iter.clone(), r.fast, r.done),
            _ => return Err(self.type_error("not an iterator")),
        };
        if done {
            return Ok(None);
        }
        if let Some((arr, i)) = fast {
            let len = self.length_of(arr)? as u32;
            if i >= len {
                self.set_iter_done(rec);
                return Ok(None);
            }
            if let Kind::IterRecord(r) = &mut self.heap.get_mut(rec).kind {
                r.fast = Some((arr, i + 1));
            }
            let v = self.get(arr, &PropKey::index(i), Value::Object(arr))?;
            return Ok(Some(v));
        }
        let r = match self.call(&next, iter, &[]) {
            Ok(r) => r,
            Err(e) => {
                self.set_iter_done(rec);
                return Err(e);
            }
        };
        let Value::Object(ro) = r else {
            self.set_iter_done(rec);
            return Err(self.type_error("Iterator result is not an object"));
        };
        let d = self.get(ro, &PropKey::from("done"), r.clone())?;
        if d.truthy() {
            self.set_iter_done(rec);
            return Ok(None);
        }
        Ok(Some(self.get(ro, &PropKey::from("value"), r.clone())?))
    }

    pub fn set_iter_done(&mut self, rec: ObjRef) {
        if let Kind::IterRecord(r) = &mut self.heap.get_mut(rec).kind {
            r.done = true;
        }
    }

    pub fn iter_close(&mut self, rec: ObjRef, quiet: bool) -> Result<(), Value> {
        let (iter, fast, done) = match &self.heap.get(rec).kind {
            Kind::IterRecord(r) => (r.iter.clone(), r.fast, r.done),
            _ => return Ok(()),
        };
        if done || fast.is_some() {
            return Ok(());
        }
        self.set_iter_done(rec);
        let ret = match self.get_method(&iter, &PropKey::from("return")) {
            Ok(r) => r,
            Err(e) => return if quiet { Ok(()) } else { Err(e) },
        };
        let Some(ret) = ret else { return Ok(()) };
        match self.call(&ret, iter, &[]) {
            Ok(Value::Object(_)) => Ok(()),
            Ok(_) if !quiet => Err(self.type_error("Iterator result is not an object")),
            Err(e) if !quiet => Err(e),
            _ => Ok(()),
        }
    }

    /// All values of an iterable.
    pub fn iterate_to_vec(&mut self, v: &Value) -> Result<Vec<Value>, Value> {
        self.tick()?;
        if let Value::Object(o) = v {
            if let Kind::Array(a) = &self.heap.get(*o).kind {
                // Plain arrays with the built-in iterator: copy directly.
                if a.dense.len() == a.len as usize && !a.dense.contains(&Value::Empty) {
                    let method = self.get_v(v, &PropKey::Sym(Sym::ITERATOR))?;
                    let p = self.intr.array_iterator_proto;
                    let next = self.get(p, &PropKey::from("next"), Value::Object(p))?;
                    if method == Value::Object(self.intr.array_values)
                        && next == Value::Object(self.intr.array_iterator_next)
                    {
                        if let Kind::Array(a) = &self.heap.get(*o).kind {
                            return Ok(a.dense.clone());
                        }
                    }
                }
            }
        }
        let rec = self.get_iterator(v)?;
        let mut out = Vec::new();
        while let Some(x) = self.iter_step(rec)? {
            out.push(x);
            self.tick()?;
        }
        Ok(out)
    }

    /// A short description of a value for error messages.
    pub fn short_describe(&mut self, v: &Value) -> String {
        match v {
            Value::String(s) => format!("\"{}\"", s.to_rust()),
            Value::Object(o) => {
                if self.heap.get(*o).is_callable() {
                    let n = self.get_own_property(*o, &PropKey::from("name"));
                    if let Some(Prop { slot: Slot::Data(Value::String(s)), .. }) = n {
                        if !s.is_empty() {
                            return format!("function {}", s.to_rust());
                        }
                    }
                    String::from("function")
                } else if matches!(self.heap.get(*o).kind, Kind::Array(_)) {
                    String::from("array")
                } else {
                    String::from("object")
                }
            }
            Value::Symbol(_) => String::from("symbol"),
            other => match self.to_string(other) {
                Ok(s) => s.to_rust(),
                Err(_) => String::from("value"),
            },
        }
    }

    /// The list of an array-like's elements (Function.prototype.apply etc.).
    pub fn list_from_array_like(&mut self, v: &Value) -> Result<Vec<Value>, Value> {
        match v {
            Value::Undefined | Value::Null => Ok(Vec::new()),
            Value::Object(o) => {
                if let Kind::Array(a) = &self.heap.get(*o).kind {
                    if a.dense.len() == a.len as usize && !a.dense.contains(&Value::Empty) {
                        return Ok(a.dense.clone());
                    }
                }
                let len = self.length_of(*o)? as u32;
                let mut out = Vec::with_capacity(len.min(65536) as usize);
                for i in 0..len {
                    self.tick()?;
                    out.push(self.get(*o, &PropKey::index(i), v.clone())?);
                }
                Ok(out)
            }
            _ => Err(self.type_error("CreateListFromArrayLike called on non-object")),
        }
    }

    // ------------------------------------------------------------ globals

    pub fn set_global(&mut self, name: &str, v: Value) {
        let g = self.global;
        self.define(g, name, v, HIDDEN);
    }

    pub fn global_value(&mut self, name: &str) -> JsResult {
        if let Some((v, _)) = self.lexicals.get(&JsStr::from(name)) {
            return Ok(v.clone());
        }
        let g = self.global;
        self.get_str(g, name)
    }

    /// Schedules a function call as a job (like queueMicrotask).
    pub fn enqueue(&mut self, f: Value, args: Vec<Value>) {
        self.jobs.push_back(Job::Call(f, args));
    }
}

trait PropsExt {
    fn is_empty_of_indices(&self) -> bool;
}

impl PropsExt for Props {
    fn is_empty_of_indices(&self) -> bool {
        self.is_empty() || !self.keys().any(|k| k.as_index().is_some())
    }
}
