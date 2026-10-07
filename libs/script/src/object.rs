//! Objects: ordered property maps, array elements and the internal kinds.

use crate::bytecode::Code;
use crate::heap::ObjRef;
use crate::realm::{Call, Realm};
use crate::value::{JsStr, PropKey, Value};
use crate::vm::Frame;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec::Vec;

pub const WRITABLE: u8 = 1;
pub const ENUMERABLE: u8 = 2;
pub const CONFIGURABLE: u8 = 4;
/// Writable, enumerable and configurable: an ordinary assigned property.
pub const DEFAULT: u8 = WRITABLE | ENUMERABLE | CONFIGURABLE;
/// Built-in methods: writable and configurable, not enumerable.
pub const HIDDEN: u8 = WRITABLE | CONFIGURABLE;

#[derive(Clone, Debug)]
pub enum Slot {
    Data(Value),
    /// Getter and setter (`Undefined` when absent).
    Accessor(Value, Value),
}

#[derive(Clone, Debug)]
pub struct Prop {
    pub slot: Slot,
    pub flags: u8,
}

impl Prop {
    pub fn data(v: Value, flags: u8) -> Prop {
        Prop { slot: Slot::Data(v), flags }
    }

    pub fn writable(&self) -> bool {
        self.flags & WRITABLE != 0
    }

    pub fn enumerable(&self) -> bool {
        self.flags & ENUMERABLE != 0
    }

    pub fn configurable(&self) -> bool {
        self.flags & CONFIGURABLE != 0
    }
}

/// An insertion-ordered property map, indexed once it grows.
#[derive(Default, Clone)]
pub struct Props {
    entries: Vec<(PropKey, Prop)>,
    index: Option<BTreeMap<PropKey, u32>>,
}

const INDEX_AT: usize = 12;

impl Props {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, key: &PropKey) -> Option<usize> {
        match &self.index {
            Some(ix) => ix.get(key).map(|&i| i as usize),
            None => self.entries.iter().position(|(k, _)| k == key),
        }
    }

    pub fn get(&self, key: &PropKey) -> Option<&Prop> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &PropKey) -> Option<&mut Prop> {
        self.position(key).map(move |i| &mut self.entries[i].1)
    }

    pub fn insert(&mut self, key: PropKey, prop: Prop) {
        if let Some(i) = self.position(&key) {
            self.entries[i].1 = prop;
            return;
        }
        let i = self.entries.len();
        if let Some(ix) = &mut self.index {
            ix.insert(key.clone(), i as u32);
        }
        self.entries.push((key, prop));
        if self.index.is_none() && self.entries.len() > INDEX_AT {
            self.reindex();
        }
    }

    fn reindex(&mut self) {
        self.index = Some(self.entries.iter().enumerate().map(|(i, (k, _))| (k.clone(), i as u32)).collect());
    }

    pub fn remove(&mut self, key: &PropKey) -> Option<Prop> {
        let i = self.position(key)?;
        let (_, p) = self.entries.remove(i);
        if self.index.is_some() {
            self.reindex();
        }
        Some(p)
    }

    /// Keys in insertion order (callers sort integer keys first).
    pub fn keys(&self) -> impl Iterator<Item = &PropKey> {
        self.entries.iter().map(|(k, _)| k)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&PropKey, &Prop)> {
        self.entries.iter().map(|(k, p)| (k, p))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&PropKey, &mut Prop)> {
        self.entries.iter_mut().map(|(k, p)| (&*k, p))
    }
}

pub type NativeFn = fn(&mut Realm, &Call) -> Result<Value, Value>;

pub struct Native {
    pub f: NativeFn,
    pub ctor: bool,
    /// Captured values (bound resolve functions, etc.).
    pub slots: Vec<Value>,
}

pub struct Closure {
    pub code: Rc<Code>,
    pub scope: Option<ObjRef>,
    /// Arrows: the captured `this` and `new.target`.
    pub this: Value,
    pub new_target: Value,
    /// Methods: the object `super` looks up from.
    pub home: Option<ObjRef>,
    /// Class constructors: instance fields to define.
    pub fields: Option<ObjRef>,
    /// Arrows and class field initialisers inside a class: the class's
    /// fields function (so `super()` inside arrows can initialise them).
    pub ctor_kind: CtorKind,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum CtorKind {
    /// Not a constructor (arrows, methods, generators, async functions).
    None,
    /// An ordinary function or base class.
    Base,
    Derived,
}

pub struct Bound {
    pub target: ObjRef,
    pub this: Value,
    pub args: Vec<Value>,
}

pub struct ArrayStore {
    /// Elements 0..dense.len(); holes are `Value::Empty`. Elements beyond are
    /// ordinary properties.
    pub dense: Vec<Value>,
    pub len: u32,
    pub len_writable: bool,
}

/// The element type of a typed array.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum TAKind {
    I8,
    U8,
    U8C,
    I16,
    U16,
    I32,
    U32,
    F32,
    F64,
}

impl TAKind {
    pub const ALL: [TAKind; 9] = [
        TAKind::I8,
        TAKind::U8,
        TAKind::U8C,
        TAKind::I16,
        TAKind::U16,
        TAKind::I32,
        TAKind::U32,
        TAKind::F32,
        TAKind::F64,
    ];

    pub fn size(self) -> usize {
        match self {
            TAKind::I8 | TAKind::U8 | TAKind::U8C => 1,
            TAKind::I16 | TAKind::U16 => 2,
            TAKind::I32 | TAKind::U32 | TAKind::F32 => 4,
            TAKind::F64 => 8,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            TAKind::I8 => "Int8Array",
            TAKind::U8 => "Uint8Array",
            TAKind::U8C => "Uint8ClampedArray",
            TAKind::I16 => "Int16Array",
            TAKind::U16 => "Uint16Array",
            TAKind::I32 => "Int32Array",
            TAKind::U32 => "Uint32Array",
            TAKind::F32 => "Float32Array",
            TAKind::F64 => "Float64Array",
        }
    }

    /// Reads an element (little-endian, as on every machine WaveOS runs on).
    pub fn read(self, b: &[u8]) -> f64 {
        match self {
            TAKind::I8 => b[0] as i8 as f64,
            TAKind::U8 | TAKind::U8C => b[0] as f64,
            TAKind::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            TAKind::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            TAKind::I32 => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            TAKind::U32 => u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            TAKind::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64,
            TAKind::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]),
        }
    }

    /// Writes a (already numeric) value, converting as the element type does.
    pub fn write(self, b: &mut [u8], v: f64) {
        use crate::numconv::{to_int32, to_uint32};
        match self {
            TAKind::I8 => b[0] = to_int32(v) as i8 as u8,
            TAKind::U8 => b[0] = to_uint32(v) as u8,
            TAKind::U8C => {
                b[0] = if v.is_nan() || v <= 0.0 {
                    0
                } else if v >= 255.0 {
                    255
                } else {
                    // Round half to even.
                    let f = libm::floor(v);
                    let r = if v - f > 0.5 || (v - f == 0.5 && f % 2.0 != 0.0) { f + 1.0 } else { f };
                    r as u8
                }
            }
            TAKind::I16 => b[..2].copy_from_slice(&(to_int32(v) as i16).to_le_bytes()),
            TAKind::U16 => b[..2].copy_from_slice(&(to_uint32(v) as u16).to_le_bytes()),
            TAKind::I32 => b[..4].copy_from_slice(&to_int32(v).to_le_bytes()),
            TAKind::U32 => b[..4].copy_from_slice(&to_uint32(v).to_le_bytes()),
            TAKind::F32 => b[..4].copy_from_slice(&(v as f32).to_le_bytes()),
            TAKind::F64 => b[..8].copy_from_slice(&v.to_le_bytes()),
        }
    }
}

pub struct TypedArray {
    pub kind: TAKind,
    pub buffer: ObjRef,
    pub offset: usize,
    pub length: usize,
}

pub struct ArrayBuffer {
    pub bytes: Vec<u8>,
    pub detached: bool,
}

/// The state of an iterator helper (`iter.map(f)`, `iter.take(3)`…).
pub struct IterHelper {
    /// map, filter, take, drop, flatMap, or a wrapper from Iterator.from.
    pub kind: HelperKind,
    /// The underlying iterator (an IterRecord).
    pub underlying: ObjRef,
    pub f: Value,
    pub counter: f64,
    /// take: how many remain; drop: how many to skip first.
    pub limit: f64,
    /// flatMap: the inner iterator being drained (an IterRecord).
    pub inner: Option<ObjRef>,
    pub done: bool,
    pub running: bool,
    pub started: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum HelperKind {
    Map,
    Filter,
    Take,
    Drop,
    FlatMap,
    Wrap,
}

pub struct IterRecord {
    pub iter: Value,
    pub next: Value,
    pub done: bool,
    /// Fast path: iterating a plain array with the built-in iterator.
    pub fast: Option<(ObjRef, u32)>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum IterKind {
    Keys,
    Values,
    Entries,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum GenState {
    Start,
    Suspended,
    Running,
    Done,
}

pub struct Generator {
    pub state: GenState,
    pub frame: Option<Frame>,
    /// The frame's saved operand stack and locals.
    pub stack: Vec<Value>,
    /// Async functions: the promise their completion settles.
    pub promise: Option<ObjRef>,
    pub is_async: bool,
    /// Async generators: queued requests (kind, value, promise).
    pub queue: Vec<(u8, Value, ObjRef)>,
    /// Suspended at an `await` (not a `yield`).
    pub awaiting: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum PromiseState {
    Pending,
    Fulfilled,
    Rejected,
}

pub struct Reaction {
    /// The derived promise (or capability) to settle; None for awaits.
    pub capability: Option<(Value, Value, Value)>,
    pub kind_fulfill: bool,
    pub handler: Value,
}

pub struct Promise {
    pub state: PromiseState,
    pub value: Value,
    pub fulfill: Vec<Reaction>,
    pub reject: Vec<Reaction>,
    pub handled: bool,
}

/// An insertion-ordered map with tombstones (live iterators keep working
/// across deletes, as the spec requires).
#[derive(Default)]
pub struct OrderedMap {
    pub entries: Vec<Option<(Value, Value)>>,
    pub index: BTreeMap<MapKey, usize>,
    pub size: usize,
}

/// A Map/Set key: SameValueZero semantics.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum MapKey {
    Undefined,
    Null,
    Bool(bool),
    Num(u64),
    NaN,
    Str(JsStr),
    Sym(u32),
    Obj(u32),
}

impl MapKey {
    pub fn of(v: &Value) -> MapKey {
        match v {
            Value::Undefined | Value::Empty => MapKey::Undefined,
            Value::Null => MapKey::Null,
            Value::Bool(b) => MapKey::Bool(*b),
            Value::Number(n) if n.is_nan() => MapKey::NaN,
            Value::Number(n) => MapKey::Num(if *n == 0.0 { 0 } else { n.to_bits() }),
            Value::String(s) => MapKey::Str(s.clone()),
            Value::Symbol(s) => MapKey::Sym(s.0),
            Value::Object(o) => MapKey::Obj(o.0),
        }
    }
}

impl OrderedMap {
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&MapKey::of(k)).and_then(|&i| self.entries[i].as_ref().map(|(_, v)| v))
    }

    pub fn has(&self, k: &Value) -> bool {
        self.index.contains_key(&MapKey::of(k))
    }

    pub fn set(&mut self, k: Value, v: Value) {
        let key = MapKey::of(&k);
        if let Some(&i) = self.index.get(&key) {
            if let Some(e) = &mut self.entries[i] {
                e.1 = v;
            }
            return;
        }
        // -0 is stored as +0 (adding 0.0 maps -0 to +0).
        let k = match k {
            Value::Number(n) => Value::Number(n + 0.0),
            k => k,
        };
        self.index.insert(key, self.entries.len());
        self.entries.push(Some((k, v)));
        self.size += 1;
    }

    pub fn delete(&mut self, k: &Value) -> bool {
        match self.index.remove(&MapKey::of(k)) {
            Some(i) => {
                self.entries[i] = None;
                self.size -= 1;
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.entries.fill(None);
        self.index.clear();
        self.size = 0;
    }
}

pub struct RegExpData {
    pub source: JsStr,
    pub flags: JsStr,
    pub program: Rc<crate::regexp::Program>,
}

pub enum Kind {
    Ordinary,
    Array(ArrayStore),
    Function(Box<Closure>),
    Native(Box<Native>),
    Bound(Box<Bound>),
    /// Boolean, Number, String and Symbol wrapper objects.
    Primitive(Value),
    Error,
    Arguments,
    Date(f64),
    RegExp(Box<RegExpData>),
    Map(Box<OrderedMap>),
    Set(Box<OrderedMap>),
    WeakMap(Box<BTreeMap<u32, (ObjRef, Value)>>),
    WeakSet(Box<BTreeMap<u32, ObjRef>>),
    Promise(Box<Promise>),
    Generator(Box<Generator>),
    /// A heap-allocated scope of captured variables.
    Scope(Vec<Value>, Option<ObjRef>),
    IterRecord(Box<IterRecord>),
    ArrayIterator(Value, u32, IterKind),
    StringIterator(JsStr, u32),
    MapIterator(ObjRef, u32, IterKind),
    SetIterator(ObjRef, u32, IterKind),
    RegExpStringIterator(Box<(ObjRef, JsStr, bool, bool, bool)>),
    ForIn(Box<(Vec<PropKey>, u32, ObjRef)>),
    /// A `return` unwinding through `finally` blocks (generator.return()).
    ReturnSignal(Value),
    /// The class fields of a constructor: (key, initialiser, is_private_method).
    Fields(Vec<(Value, Value, u8)>),
    /// An object belonging to the embedder (a DOM node, etc.).
    Host(u32, u64),
    /// An async-from-sync iterator wrapper.
    Wrapper(Value),
    IterHelper(Box<IterHelper>),
    ArrayBuffer(Box<ArrayBuffer>),
    TypedArray(Box<TypedArray>),
    /// A DataView: (buffer, offset, length).
    DataView(Box<(ObjRef, usize, usize)>),
}

pub struct Obj {
    pub proto: Option<ObjRef>,
    pub props: Props,
    pub kind: Kind,
    pub extensible: bool,
    pub(crate) mark: bool,
}

impl Obj {
    pub fn new(proto: Option<ObjRef>, kind: Kind) -> Obj {
        Obj { proto, props: Props::default(), kind, extensible: true, mark: false }
    }

    pub fn is_callable(&self) -> bool {
        matches!(self.kind, Kind::Function(_) | Kind::Native(_) | Kind::Bound(_))
    }

    /// Every value this object references, for the collector.
    pub fn trace(&self, out: &mut Vec<ObjRef>) {
        let v = |val: &Value, out: &mut Vec<ObjRef>| {
            if let Value::Object(o) = val {
                out.push(*o);
            }
        };
        if let Some(p) = self.proto {
            out.push(p);
        }
        for (_, p) in self.props.iter() {
            match &p.slot {
                Slot::Data(x) => v(x, out),
                Slot::Accessor(g, s) => {
                    v(g, out);
                    v(s, out);
                }
            }
        }
        match &self.kind {
            Kind::Array(a) => a.dense.iter().for_each(|x| v(x, out)),
            Kind::Function(c) => {
                out.extend(c.scope);
                out.extend(c.home);
                out.extend(c.fields);
                v(&c.this, out);
                v(&c.new_target, out);
            }
            Kind::Native(n) => n.slots.iter().for_each(|x| v(x, out)),
            Kind::Bound(b) => {
                out.push(b.target);
                v(&b.this, out);
                b.args.iter().for_each(|x| v(x, out));
            }
            Kind::Primitive(p) => v(p, out),
            Kind::Map(m) | Kind::Set(m) => {
                for (a, b) in m.entries.iter().flatten() {
                    v(a, out);
                    v(b, out);
                }
            }
            // Weak collections: values are traced only through live keys (see heap).
            Kind::WeakMap(_) | Kind::WeakSet(_) => {}
            Kind::Promise(p) => {
                v(&p.value, out);
                for r in p.fulfill.iter().chain(p.reject.iter()) {
                    v(&r.handler, out);
                    if let Some((a, b, c)) = &r.capability {
                        v(a, out);
                        v(b, out);
                        v(c, out);
                    }
                }
            }
            Kind::Generator(g) => {
                g.stack.iter().for_each(|x| v(x, out));
                if let Some(f) = &g.frame {
                    f.trace(out);
                }
                out.extend(g.promise);
                for (_, val, p) in &g.queue {
                    v(val, out);
                    out.push(*p);
                }
            }
            Kind::Scope(vars, parent) => {
                vars.iter().for_each(|x| v(x, out));
                out.extend(*parent);
            }
            Kind::IterRecord(r) => {
                v(&r.iter, out);
                v(&r.next, out);
                if let Some((a, _)) = r.fast {
                    out.push(a);
                }
            }
            Kind::ArrayIterator(t, _, _) => v(t, out),
            Kind::MapIterator(m, _, _) | Kind::SetIterator(m, _, _) => out.push(*m),
            Kind::RegExpStringIterator(b) => out.push(b.0),
            Kind::ForIn(f) => out.push(f.2),
            Kind::ReturnSignal(x) | Kind::Wrapper(x) => v(x, out),
            Kind::TypedArray(t) => out.push(t.buffer),
            Kind::DataView(d) => out.push(d.0),
            Kind::ArrayBuffer(_) => {}
            Kind::IterHelper(h) => {
                out.push(h.underlying);
                out.extend(h.inner);
                v(&h.f, out);
            }
            Kind::Fields(f) => {
                for (a, b, _) in f {
                    v(a, out);
                    v(b, out);
                }
            }
            Kind::Ordinary
            | Kind::Error
            | Kind::Arguments
            | Kind::Date(_)
            | Kind::RegExp(_)
            | Kind::StringIterator(..)
            | Kind::Host(..) => {}
        }
    }
}
