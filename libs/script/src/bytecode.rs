//! Bytecode for the stack machine in `vm.rs`.
//!
//! Each function compiles to a `Code`. A frame's stack holds its locals
//! (`n_locals` slots, parameters first) followed by the operand stack.

use crate::value::JsStr;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Op {
    // Constants.
    Undef,
    Null,
    True,
    False,
    Int(i32),
    /// A number or string constant.
    Const(u32),
    /// The TDZ marker.
    Empty,

    // Stack shuffles.
    Pop,
    Dup,
    /// a b → a b a b
    Dup2,
    /// a b → b a
    Swap,
    /// a b c → c a b
    Rot3,
    /// a b c d → d a b c
    Rot4,
    /// a b → b
    Nip,
    /// Pushes a copy of the value n below the top (Pick(0) is Dup).
    Pick(u8),
    /// Throws a TypeError if the top value is null or undefined.
    RequireCoercible,

    // Variables. `name` operands are constant indices (for error messages).
    GetLocal(u32),
    SetLocal(u32),
    /// Throws a ReferenceError if the slot holds the TDZ marker.
    GetLocalChecked(u32, u32),
    /// Checks the slot is initialised, without reading it (before assignment).
    CheckLocal(u32, u32),
    GetScope(u16, u16),
    SetScope(u16, u16),
    GetScopeChecked(u16, u16, u32),
    CheckScope(u16, u16, u32),
    PushScope(u16),
    PopScope,
    /// Replaces the current scope with a copy (per-iteration `let` bindings).
    CopyScope,
    GetGlobal(u32),
    SetGlobal(u32),
    /// `typeof x` for a global: no ReferenceError.
    TypeofGlobal(u32),
    /// Declares a global `var` (if absent) or function (always).
    DeclareVar(u32),
    DeclareFunction(u32),
    /// Declares a global lexical binding (`let`, `const`, `class`) in TDZ.
    DeclareLexical(u32, bool),
    /// Initialises a global lexical binding.
    InitLexical(u32),
    /// Throws "Assignment to constant variable".
    ThrowConst(u32),

    // Properties.
    GetProp(u32),
    SetProp(u32),
    GetElem,
    SetElem,
    /// obj → func obj
    GetMethod(u32),
    /// obj key → func obj
    GetMethodElem,
    DeleteProp(u32),
    DeleteElem,
    In,
    InstanceOf,
    /// super.x / super[x] (the key on the stack for Elem).
    GetSuper(u32),
    GetSuperElem,
    /// value → value (super.x = value)
    SetSuper(u32),
    /// key value → value
    SetSuperElem,
    /// obj sym → value
    GetPrivate,
    /// obj sym value → value
    SetPrivate,
    /// obj sym → bool
    HasPrivate,
    /// A new private name (a unique symbol) described by the constant.
    PrivateName(u32),

    // Literals.
    NewObject,
    NewArray,
    /// arr v → arr
    ArrayPush,
    /// arr → arr
    ArrayHole,
    /// arr iterable → arr
    ArraySpread,
    /// obj v → obj
    DefineField(u32),
    /// obj key v → obj
    DefineElem,
    /// obj key fn → obj; the u8 flags: 1 = getter, 2 = setter, 4 = enumerable
    /// (object literals), 8 = method (sets the home object).
    DefineMethod(u8),
    /// obj src → obj
    CopyDataProps,
    /// src k1..kn → rest: a copy of src without those keys.
    CopyRest(u32),
    /// obj proto → obj
    SetProtoLiteral,
    /// Creates a RegExp from (pattern, flags) constants.
    RegExp(u32, u32),
    /// The frozen strings array of a tagged template (constant: site).
    Template(u32),

    // Operators.
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Shr,
    UShr,
    BitAnd,
    BitOr,
    BitXor,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    Lt,
    Le,
    Gt,
    Ge,
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Inc,
    Dec,
    ToNumeric,
    ToString,
    ToPropertyKey,

    // Control.
    Jump(u32),
    JumpIfFalse(u32),
    JumpIfTrue(u32),
    /// Keeps the value and jumps if falsy, else pops it.
    JumpIfFalseKeep(u32),
    JumpIfTrueKeep(u32),
    /// Keeps the value and jumps if not nullish, else pops it.
    JumpIfNotNullishKeep(u32),
    /// Optional chains: if the value `depth` below the top is nullish, drops
    /// depth + 1 values, pushes undefined and jumps.
    JumpIfNullish(u32, u8),
    /// Pops and jumps if the value is undefined (default values).
    JumpIfUndefined(u32),
    JumpIfNotUndefined(u32),

    // Calls.
    /// func this a1..an → result
    Call(u32),
    /// func this args → result
    CallSpread,
    /// func a1..an → object
    New(u32),
    NewSpread,
    SuperCall(u32),
    SuperCallSpread,
    Return,
    Throw,
    /// Rethrows a value saved by a finally block (or continues a
    /// generator return).
    Rethrow,

    // Functions and classes.
    Closure(u32),
    /// A class: [heritage?] → ctor proto. Operands: constructor code
    /// constant (u32::MAX for the default one), has heritage.
    Class(u32, bool),
    /// ctor → ctor: stores the instance fields function list.
    SetFields(u32),
    /// obj key fn → obj: a field initialiser (stores a field record).
    AddField(u8),
    /// Runs a constructor's field initialisers on `this`.
    InitFields,
    This,
    NewTarget,
    /// The current function object.
    Callee,
    Arguments,
    /// The arguments from index n onwards, as an array.
    Rest(u32),
    /// Converts a fresh closure's name: fn key → fn (sets `name` from the key).
    SetFunctionName(u8),

    // Exceptions.
    TryStart(u32, bool),
    TryEnd,

    // Iteration.
    GetIterator,
    GetAsyncIterator,
    /// iter → iter value, or jumps (leaving iter) when done.
    IterNext(u32),
    /// iter → iter value (undefined once done).
    IterNextOrUndef,
    /// iter → iter array (the remaining values).
    IterRest,
    /// iter → (calls return() if not done). `true`: ignore errors from
    /// return() (closing because of a throw).
    IterClose(bool),
    /// iter → (pops without closing)
    IterDrop,
    /// iter → iter result (calls next() without inspecting the result).
    IterNextRaw,
    /// For await: iter result → iter value, or jumps (leaving iter) when done.
    IterResult(u32),
    ForInStart,
    /// it → it key, or jumps (leaving it) when done.
    ForInNext(u32),

    // Generators and async functions.
    /// Suspends at the start of a generator body.
    InitialYield,
    Yield,
    /// yield*: iter received → ... (see vm).
    YieldDelegate(u32),
    Await,
    /// Async generators: awaits the yielded value.
    AsyncYield,

    /// with: obj → (jumps with the property's value when obj has `name`,
    /// else pops obj).
    WithGet(u32, u32),
    /// with: value obj → value (sets obj[name] and jumps when obj has it,
    /// else pops obj).
    WithSet(u32, u32),
    /// with: obj → func obj (jumps when obj has `name`, else pops obj).
    WithGetMethod(u32, u32),
    /// with: obj → (deletes obj[name], pushing the result, and jumps when
    /// obj has it, else pops obj).
    WithDelete(u32, u32),
    /// ToObject on the top value.
    ToObject,

    Debugger,
    Nop,
}

#[derive(Clone, Debug)]
pub enum Const {
    Num(f64),
    Str(JsStr),
    Code(Rc<Code>),
    Template(Rc<TemplateData>),
}

#[derive(Debug)]
pub struct TemplateData {
    /// Unique per call site, across every script compiled.
    pub site: u32,
    pub cooked: Vec<Option<JsStr>>,
    pub raw: Vec<JsStr>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CodeKind {
    Script,
    Normal,
    Arrow,
    Method,
    Getter,
    Setter,
    ClassConstructor,
    DerivedConstructor,
    ClassInit,
}

#[derive(Debug)]
pub struct Code {
    pub name: JsStr,
    pub ops: Vec<Op>,
    pub consts: Vec<Const>,
    pub n_locals: u32,
    /// Parameters passed straight into locals 0..n_params.
    pub n_params: u32,
    /// The `length` property.
    pub length: u32,
    pub kind: CodeKind,
    pub strict: bool,
    pub is_async: bool,
    pub is_generator: bool,
    /// Keeps all arguments (for `arguments` or rest parameters).
    pub keep_args: bool,
    /// (pc, source offset + 1) pairs, sorted by pc.
    pub lines: Vec<(u32, u32)>,
    /// Byte offsets where each line of the file starts.
    pub line_starts: Rc<[u32]>,
    /// (pc of a Call/New, constant with the callee's source text).
    pub callees: Vec<(u32, u32)>,
    pub source: Option<(Rc<str>, u32, u32)>,
    pub file: Rc<str>,
    /// For each name constant used by GetGlobal/SetGlobal: where the name
    /// was last found ([`GLOBAL_LEXICAL`] | slot, or global property
    /// index + 1) and the realm's lexical epoch then.
    pub global_cache: Vec<core::cell::Cell<(u32, u32)>>,
    /// For each name constant used as a property name: the object last
    /// found holding it (ObjRef + 1) and the entry index, with [`PROTO_HIT`]
    /// when it was the receiver's prototype. Hits are re-validated.
    pub prop_cache: Vec<core::cell::Cell<(u32, u32)>>,
    /// Set for a function whose body is compiled when first called.
    pub lazy: Option<Rc<crate::compiler::LazyFn>>,
}

impl Code {
    /// Forgets where global names were found (before running in another realm).
    pub fn reset_caches(&self) {
        let mut stack: Vec<&Code> = alloc::vec![self];
        while let Some(c) = stack.pop() {
            for cell in c.global_cache.iter().chain(&c.prop_cache) {
                cell.set((0, 0));
            }
            for k in &c.consts {
                if let Const::Code(inner) = k {
                    stack.push(inner);
                }
            }
            if let Some(real) = c.lazy.as_ref().and_then(|l| l.compiled.get()) {
                stack.push(real);
            }
        }
    }
}

/// Compiled scripts kept between pages (reloads and revisits skip parsing
/// and compiling), least recently used dropped beyond a source-size budget.
pub struct CodeCache {
    map: alloc::collections::BTreeMap<u64, (Rc<Code>, usize, u64)>,
    bytes: usize,
    clock: u64,
    pub budget: usize,
    pub hits: u64,
}

impl Default for CodeCache {
    fn default() -> Self {
        CodeCache { map: Default::default(), bytes: 0, clock: 0, budget: 48 << 20, hits: 0 }
    }
}

impl CodeCache {
    /// Scripts smaller than this compile faster than they're looked up.
    pub const MIN_SOURCE: usize = 2048;

    pub fn key(src: &str, file: &str) -> u64 {
        // FNV-1a over the file name and source.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in file.bytes().chain([0]).chain(src.bytes()) {
            h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3);
        }
        h ^ (src.len() as u64).rotate_left(32)
    }

    pub fn get(&mut self, key: u64) -> Option<Rc<Code>> {
        self.clock += 1;
        let now = self.clock;
        let e = self.map.get_mut(&key)?;
        e.2 = now;
        self.hits += 1;
        Some(e.0.clone())
    }

    pub fn insert(&mut self, key: u64, code: Rc<Code>, size: usize) {
        if size > self.budget {
            return;
        }
        self.clock += 1;
        if let Some(old) = self.map.insert(key, (code, size, self.clock)) {
            self.bytes -= old.1;
        }
        self.bytes += size;
        while self.bytes > self.budget {
            let Some((&k, _)) = self.map.iter().min_by_key(|(_, e)| e.2) else { break };
            if let Some(e) = self.map.remove(&k) {
                self.bytes -= e.1;
            }
        }
    }
}

/// Marks a property cache entry found on the receiver's prototype.
pub const PROTO_HIT: u32 = 1 << 31;

/// Marks a global cache entry as a top-level let/const/class slot.
pub const GLOBAL_LEXICAL: u32 = 1 << 31;

impl Code {
    /// A listing of the bytecode, nested functions included (for
    /// `js --dump`).
    pub fn dump(&self) -> alloc::string::String {
        use core::fmt::Write;
        let mut s = alloc::string::String::new();
        let _ = writeln!(s, "== {} ({} locals)", self.name, self.n_locals);
        for (i, op) in self.ops.iter().enumerate() {
            let _ = writeln!(s, "{i:5}  {op:?}");
        }
        for c in &self.consts {
            if let Const::Code(c) = c {
                s.push_str(&c.dump());
            }
        }
        s
    }

    /// The (line, column) of the code at `pc`, both from 1 ((0, 0): unknown).
    pub fn line_at(&self, pc: usize) -> (u32, u32) {
        let i = self.lines.partition_point(|(p, _)| *p as usize <= pc);
        if i == 0 || self.lines[i - 1].1 == 0 {
            return (0, 0);
        }
        let offset = self.lines[i - 1].1 - 1;
        let line = self.line_starts.partition_point(|&s| s <= offset).max(1);
        (line as u32, offset - self.line_starts[line - 1] + 1)
    }

    /// The source text of the callee of the call at `pc`, if recorded.
    pub fn callee_at(&self, pc: usize) -> Option<&JsStr> {
        let i = self.callees.binary_search_by_key(&(pc as u32), |(p, _)| *p).ok()?;
        Some(self.str_const(self.callees[i].1))
    }

    pub fn str_const(&self, i: u32) -> &JsStr {
        match &self.consts[i as usize] {
            Const::Str(s) => s,
            _ => panic!("constant {i} is not a string"),
        }
    }

    pub fn source_text(&self) -> Option<String> {
        self.source.as_ref().map(|(src, a, b)| String::from(&src[*a as usize..*b as usize]))
    }
}
