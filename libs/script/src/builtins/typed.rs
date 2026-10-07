//! Binary data: ArrayBuffer, the typed arrays (%TypedArray% and its nine
//! element types) and DataView.

use super::array;
use super::*;
use alloc::format;

const MAX_BYTES: f64 = (1u64 << 31) as f64;

fn type_error(rt: &mut Realm, msg: &str) -> Value {
    rt.type_error(msg)
}

/// ToIndex.
fn to_index(rt: &mut Realm, v: &Value, what: &str) -> Result<usize, Value> {
    if v.is_undefined() {
        return Ok(0);
    }
    let n = rt.to_integer(v)?;
    if !(0.0..=9007199254740991.0).contains(&n) {
        return Err(rt.range_error(&format!("Invalid {what}")));
    }
    Ok(n as usize)
}

// ---------------------------------------------------------------- ArrayBuffer

pub fn new_buffer(rt: &mut Realm, len: usize, proto: Option<ObjRef>) -> Result<ObjRef, Value> {
    if len as f64 > MAX_BYTES {
        return Err(rt.range_error("Array buffer allocation failed"));
    }
    let proto = proto.unwrap_or(rt.intr.array_buffer_proto);
    Ok(rt.alloc(Obj::new(
        Some(proto),
        Kind::ArrayBuffer(Box::new(ArrayBuffer { bytes: alloc::vec![0; len], detached: false })),
    )))
}

fn buffer_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(type_error(rt, "Constructor ArrayBuffer requires 'new'"));
    }
    let len = to_index(rt, &c.arg(0), "array buffer length")?;
    let bp = rt.intr.array_buffer_proto;
    let proto = rt.proto_from_ctor(&c.new_target, bp)?;
    Ok(Value::Object(new_buffer(rt, len, Some(proto))?))
}

fn this_buffer(rt: &mut Realm, c: &Call, method: &str) -> Result<ObjRef, Value> {
    match &c.this {
        Value::Object(o) if matches!(rt.heap.get(*o).kind, Kind::ArrayBuffer(_)) => Ok(*o),
        _ => Err(type_error(rt, &format!("ArrayBuffer.prototype.{method} called on an incompatible receiver"))),
    }
}

fn buffer_bytes(rt: &Realm, b: ObjRef) -> (&[u8], bool) {
    match &rt.heap.get(b).kind {
        Kind::ArrayBuffer(a) => (&a.bytes, a.detached),
        _ => (&[], true),
    }
}

fn byte_length(rt: &mut Realm, c: &Call) -> JsResult {
    let b = this_buffer(rt, c, "byteLength")?;
    Ok(Value::Number(buffer_bytes(rt, b).0.len() as f64))
}

fn detached_get(rt: &mut Realm, c: &Call) -> JsResult {
    let b = this_buffer(rt, c, "detached")?;
    Ok(Value::Bool(buffer_bytes(rt, b).1))
}

fn resizable(rt: &mut Realm, c: &Call) -> JsResult {
    this_buffer(rt, c, "resizable")?;
    Ok(Value::Bool(false))
}

fn buffer_slice(rt: &mut Realm, c: &Call) -> JsResult {
    let b = this_buffer(rt, c, "slice")?;
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot perform ArrayBuffer.prototype.slice on a detached ArrayBuffer"));
    }
    let len = buffer_bytes(rt, b).0.len() as f64;
    let start = rt.relative_index(&c.arg(0), len, 0.0)?;
    let end = rt.relative_index(&c.arg(1), len, len)?;
    let count = (end - start).max(0.0) as usize;
    // The species constructor makes the new buffer.
    let ctor = species(rt, b, Value::Object(rt.intr.array_buffer_ctor))?;
    let new = rt.construct(&ctor, &[Value::Number(count as f64)], None)?;
    let Value::Object(n) = new else { return Err(type_error(rt, "species constructor returned a non-object")) };
    if !matches!(rt.heap.get(n).kind, Kind::ArrayBuffer(_)) || n == b {
        return Err(type_error(rt, "ArrayBuffer subclass returned an invalid buffer"));
    }
    if buffer_bytes(rt, n).1 || buffer_bytes(rt, n).0.len() < count {
        return Err(type_error(rt, "ArrayBuffer subclass returned a buffer that is too small"));
    }
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "ArrayBuffer was detached"));
    }
    let src: Vec<u8> = {
        let bytes = buffer_bytes(rt, b).0;
        let from = (start as usize).min(bytes.len());
        bytes[from..(from + count).min(bytes.len())].to_vec()
    };
    if let Kind::ArrayBuffer(a) = &mut rt.heap.get_mut(n).kind {
        a.bytes[..src.len()].copy_from_slice(&src);
    }
    Ok(new)
}

fn is_view(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(
        matches!(c.arg(0), Value::Object(o) if matches!(rt.heap.get(o).kind, Kind::TypedArray(_) | Kind::DataView(_))),
    ))
}

/// Detaches a buffer (for the host and test harnesses).
pub fn detach(rt: &mut Realm, b: ObjRef) -> bool {
    match &mut rt.heap.get_mut(b).kind {
        Kind::ArrayBuffer(a) => {
            a.bytes = Vec::new();
            a.detached = true;
            true
        }
        _ => false,
    }
}

fn species(rt: &mut Realm, o: ObjRef, default: Value) -> JsResult {
    let c = rt.get(o, &key("constructor"), Value::Object(o))?;
    if c.is_undefined() {
        return Ok(default);
    }
    let Value::Object(co) = c else { return Err(type_error(rt, "constructor is not an object")) };
    let s = rt.get(co, &PropKey::Sym(Sym::SPECIES), c.clone())?;
    if s.is_nullish() {
        return Ok(default);
    }
    if !rt.is_constructor(&s) {
        return Err(type_error(rt, "object.constructor[Symbol.species] is not a constructor"));
    }
    Ok(s)
}

// ---------------------------------------------------------------- typed arrays

fn kind_of_ctor(rt: &Realm, callee: ObjRef) -> TAKind {
    match rt.native_slots(callee).first() {
        Some(Value::Number(k)) => TAKind::ALL[*k as usize],
        _ => TAKind::U8,
    }
}

/// A new typed array over a fresh buffer.
fn allocate(rt: &mut Realm, kind: TAKind, len: usize, proto: ObjRef) -> Result<ObjRef, Value> {
    if (len * kind.size()) as f64 > MAX_BYTES {
        return Err(rt.range_error("Invalid typed array length"));
    }
    let buffer = new_buffer(rt, len * kind.size(), None)?;
    Ok(rt.alloc(Obj::new(Some(proto), Kind::TypedArray(Box::new(TypedArray { kind, buffer, offset: 0, length: len })))))
}

fn typed_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let kind = kind_of_ctor(rt, c.callee);
    if !c.is_construct() {
        return Err(type_error(rt, &format!("Constructor {} requires 'new'", kind.name())));
    }
    let default = rt.intr.typed_protos[kind as usize];
    let proto = rt.proto_from_ctor(&c.new_target, default)?;
    let first = c.arg(0);
    let Value::Object(src) = first else {
        let len = to_index(rt, &first, "typed array length")?;
        return Ok(Value::Object(allocate(rt, kind, len, proto)?));
    };
    // From a buffer: a view onto it.
    if matches!(rt.heap.get(src).kind, Kind::ArrayBuffer(_)) {
        let size = kind.size();
        let offset = to_index(rt, &c.arg(1), "start offset")?;
        if offset % size != 0 {
            return Err(rt.range_error(&format!("start offset of {} should be a multiple of {size}", kind.name())));
        }
        let len_arg = c.arg(2);
        let new_len = if len_arg.is_undefined() { None } else { Some(to_index(rt, &len_arg, "typed array length")?) };
        let (bytes, detached) = buffer_bytes(rt, src);
        if detached {
            return Err(type_error(rt, "Cannot construct a typed array on a detached ArrayBuffer"));
        }
        let buf_len = bytes.len();
        let length = match new_len {
            Some(l) => {
                if offset + l * size > buf_len {
                    return Err(rt.range_error(&format!("Invalid typed array length: {l}")));
                }
                l
            }
            None => {
                if buf_len % size != 0 {
                    return Err(
                        rt.range_error(&format!("byte length of {} should be a multiple of {size}", kind.name()))
                    );
                }
                if offset > buf_len {
                    return Err(rt.range_error(&format!("Start offset {offset} is outside the bounds of the buffer")));
                }
                (buf_len - offset) / size
            }
        };
        return Ok(Value::Object(rt.alloc(Obj::new(
            Some(proto),
            Kind::TypedArray(Box::new(TypedArray { kind, buffer: src, offset, length })),
        ))));
    }
    // From another typed array: a copy.
    if let Some((_, _, _, len)) = rt.typed_array(src) {
        if rt.typed_array(src).is_some_and(|t| matches!(&rt.heap.get(t.1).kind, Kind::ArrayBuffer(b) if b.detached)) {
            return Err(type_error(rt, "Cannot construct from a typed array on a detached ArrayBuffer"));
        }
        let t = allocate(rt, kind, len, proto)?;
        for i in 0..len {
            let v = rt.ta_get(src, i);
            rt.ta_put(t, i, v);
        }
        return Ok(Value::Object(t));
    }
    // From an iterable or array-like.
    let values = match rt.get_method(&first, &PropKey::Sym(Sym::ITERATOR))? {
        Some(_) => rt.iterate_to_vec(&first)?,
        None => rt.list_from_array_like(&first)?,
    };
    let t = allocate(rt, kind, values.len(), proto)?;
    for (i, v) in values.iter().enumerate() {
        let x = rt.to_number(v)?;
        if rt.ta_index(t, i as f64).is_some() {
            rt.ta_put(t, i, x);
        }
    }
    Ok(Value::Object(t))
}

fn abstract_ctor(rt: &mut Realm, _c: &Call) -> JsResult {
    Err(type_error(rt, "Abstract class TypedArray not directly constructable"))
}

/// ValidateTypedArray: `this` is a typed array whose buffer isn't detached.
fn this_ta(rt: &mut Realm, c: &Call, method: &str) -> Result<ObjRef, Value> {
    let Value::Object(o) = c.this else {
        return Err(type_error(rt, &format!("%TypedArray%.prototype.{method} called on a non-object")));
    };
    match rt.typed_array(o) {
        None => Err(type_error(rt, &format!("%TypedArray%.prototype.{method}: this is not a typed array."))),
        Some((_, b, _, _)) if matches!(&rt.heap.get(b).kind, Kind::ArrayBuffer(a) if a.detached) => {
            Err(type_error(rt, &format!("Cannot perform %TypedArray%.prototype.{method} on a detached ArrayBuffer")))
        }
        Some(_) => Ok(o),
    }
}

fn ta_len(rt: &Realm, o: ObjRef) -> usize {
    rt.typed_array(o).map_or(0, |t| t.3)
}

macro_rules! delegate {
    ($($name:ident => $target:path, $label:expr;)*) => {
        $(fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            this_ta(rt, c, $label)?;
            $target(rt, c)
        })*
    };
}

delegate! {
    ta_at => array::at, "at";
    ta_copy_within => array::copy_within, "copyWithin";
    ta_entries => array::entries, "entries";
    ta_every => array::every, "every";
    ta_find => array::find, "find";
    ta_find_index => array::find_index, "findIndex";
    ta_find_last => array::find_last, "findLast";
    ta_find_last_index => array::find_last_index, "findLastIndex";
    ta_for_each => array::for_each, "forEach";
    ta_includes => array::includes, "includes";
    ta_index_of => array::index_of, "indexOf";
    ta_join => array::join, "join";
    ta_keys => array::keys, "keys";
    ta_last_index_of => array::last_index_of, "lastIndexOf";
    ta_reduce => array::reduce, "reduce";
    ta_reduce_right => array::reduce_right, "reduceRight";
    ta_reverse => array::reverse, "reverse";
    ta_some => array::some, "some";
    ta_to_locale_string => array::to_locale_string, "toLocaleString";
    ta_values => array::values, "values";
}

fn ta_fill(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_ta(rt, c, "fill")?;
    let v = rt.to_number(&c.arg(0))?;
    let len = ta_len(rt, o) as f64;
    let start = rt.relative_index(&c.arg(1), len, 0.0)? as usize;
    let end = rt.relative_index(&c.arg(2), len, len)? as usize;
    this_ta(rt, c, "fill")?;
    for i in start..end.min(ta_len(rt, o)) {
        rt.ta_put(o, i, v);
    }
    Ok(c.this.clone())
}

/// TypedArraySpeciesCreate with a length.
fn species_create(rt: &mut Realm, exemplar: ObjRef, len: usize) -> Result<ObjRef, Value> {
    let kind = rt.typed_array(exemplar).unwrap().0;
    let default = Value::Object(rt.intr.typed_ctors[kind as usize]);
    let ctor = species(rt, exemplar, default)?;
    create_from_ctor(rt, &ctor, &[Value::Number(len as f64)], Some(len))
}

/// TypedArrayCreateFromConstructor: validates what came back.
fn create_from_ctor(rt: &mut Realm, ctor: &Value, args: &[Value], min_len: Option<usize>) -> Result<ObjRef, Value> {
    let r = rt.construct(ctor, args, None)?;
    let Value::Object(o) = r else { return Err(type_error(rt, "constructor returned a non-object")) };
    match rt.typed_array(o) {
        None => Err(type_error(rt, "constructor didn't return a typed array")),
        Some((_, b, _, len)) => {
            if matches!(&rt.heap.get(b).kind, Kind::ArrayBuffer(a) if a.detached) {
                return Err(type_error(rt, "constructor returned a typed array on a detached buffer"));
            }
            if min_len.is_some_and(|m| len < m) {
                return Err(type_error(rt, "constructor returned a typed array that is too short"));
            }
            Ok(o)
        }
    }
}

fn ta_map(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_ta(rt, c, "map")?;
    let f = c.arg(0);
    require_callable(rt, &f, "map")?;
    let len = ta_len(rt, o);
    let out = species_create(rt, o, len)?;
    for i in 0..len {
        let v = rt.get(o, &PropKey::index(i as u32), Value::Object(o))?;
        let r = rt.call(&f, c.arg(1), &[v, Value::Number(i as f64), Value::Object(o)])?;
        rt.set(out, PropKey::index(i as u32), r, Value::Object(out))?;
    }
    Ok(Value::Object(out))
}

fn ta_filter(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_ta(rt, c, "filter")?;
    let f = c.arg(0);
    require_callable(rt, &f, "filter")?;
    let len = ta_len(rt, o);
    let mut kept = Vec::new();
    for i in 0..len {
        let v = rt.get(o, &PropKey::index(i as u32), Value::Object(o))?;
        if rt.call(&f, c.arg(1), &[v.clone(), Value::Number(i as f64), Value::Object(o)])?.truthy() {
            kept.push(v);
        }
    }
    let out = species_create(rt, o, kept.len())?;
    for (i, v) in kept.into_iter().enumerate() {
        rt.set(out, PropKey::index(i as u32), v, Value::Object(out))?;
    }
    Ok(Value::Object(out))
}

fn ta_slice(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_ta(rt, c, "slice")?;
    let len = ta_len(rt, o) as f64;
    let start = rt.relative_index(&c.arg(0), len, 0.0)?;
    let end = rt.relative_index(&c.arg(1), len, len)?;
    let count = (end - start).max(0.0) as usize;
    let out = species_create(rt, o, count)?;
    if count > 0 {
        this_ta(rt, c, "slice")?;
        let end = (end as usize).min(ta_len(rt, o));
        let (sk, ok) = (rt.typed_array(o).unwrap().0, rt.typed_array(out).unwrap().0);
        let mut n = 0;
        for i in start as usize..end {
            if sk == ok {
                let v = rt.ta_get(o, i);
                rt.ta_put(out, n, v);
            } else {
                let v = rt.get(o, &PropKey::index(i as u32), Value::Object(o))?;
                rt.set(out, PropKey::index(n as u32), v, Value::Object(out))?;
            }
            n += 1;
        }
    }
    Ok(Value::Object(out))
}

fn ta_subarray(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.this else { return Err(type_error(rt, "subarray called on a non-object")) };
    let Some((kind, buffer, offset, _)) = rt.typed_array(o) else {
        return Err(type_error(rt, "subarray: this is not a typed array"));
    };
    let len = match &rt.heap.get(o).kind {
        Kind::TypedArray(t) if !matches!(&rt.heap.get(buffer).kind, Kind::ArrayBuffer(b) if b.detached) => t.length,
        _ => 0,
    } as f64;
    let begin = rt.relative_index(&c.arg(0), len, 0.0)?;
    let end = rt.relative_index(&c.arg(1), len, len)?;
    let count = (end - begin).max(0.0);
    let default = Value::Object(rt.intr.typed_ctors[kind as usize]);
    let ctor = species(rt, o, default)?;
    let args =
        [Value::Object(buffer), Value::Number((offset + begin as usize * kind.size()) as f64), Value::Number(count)];
    Ok(Value::Object(create_from_ctor(rt, &ctor, &args, None)?))
}

fn ta_set(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.this else { return Err(type_error(rt, "set called on a non-object")) };
    if rt.typed_array(o).is_none() {
        return Err(type_error(rt, "set: this is not a typed array"));
    }
    let offset = rt.to_integer(&c.arg(1))?;
    if offset < 0.0 {
        return Err(rt.range_error("offset is out of bounds"));
    }
    this_ta(rt, c, "set")?;
    let len = ta_len(rt, o);
    let src = c.arg(0);
    let values: Vec<f64> = match &src {
        Value::Object(s) if rt.typed_array(*s).is_some() => {
            let (_, b, _, slen) = rt.typed_array(*s).unwrap();
            if matches!(&rt.heap.get(b).kind, Kind::ArrayBuffer(a) if a.detached) {
                return Err(type_error(rt, "set: the source is detached"));
            }
            (0..slen).map(|i| rt.ta_get(*s, i)).collect()
        }
        _ => {
            let so = rt.to_object(&src)?;
            let slen = rt.length_of(so)? as usize;
            if offset + slen as f64 > len as f64 {
                return Err(rt.range_error("offset is out of bounds"));
            }
            let mut out = Vec::with_capacity(slen);
            for i in 0..slen {
                let v = rt.get(so, &PropKey::index(i as u32), Value::Object(so))?;
                let x = rt.to_number(&v)?;
                // Writes happen as each value is read (the target may detach).
                if let Some(t) = rt.ta_index(o, offset + i as f64) {
                    rt.ta_put(o, t, x);
                }
                out.push(x);
            }
            return Ok(Value::Undefined);
        }
    };
    if offset + values.len() as f64 > len as f64 {
        return Err(rt.range_error("offset is out of bounds"));
    }
    for (i, v) in values.into_iter().enumerate() {
        rt.ta_put(o, offset as usize + i, v);
    }
    Ok(Value::Undefined)
}

fn numeric_cmp(a: f64, b: f64) -> core::cmp::Ordering {
    use core::cmp::Ordering::*;
    match (a.is_nan(), b.is_nan()) {
        (true, true) => Equal,
        (true, false) => Greater,
        (false, true) => Less,
        _ => {
            if a < b {
                Less
            } else if a > b {
                Greater
            } else if a == 0.0 && b == 0.0 {
                // -0 sorts before +0.
                b.is_sign_negative().cmp(&a.is_sign_negative())
            } else {
                Equal
            }
        }
    }
}

fn sorted(rt: &mut Realm, o: ObjRef, cmp: &Value) -> Result<Vec<f64>, Value> {
    let len = ta_len(rt, o);
    let mut items: Vec<f64> = (0..len).map(|i| rt.ta_get(o, i)).collect();
    if cmp.is_undefined() {
        items.sort_by(|a, b| numeric_cmp(*a, *b));
        return Ok(items);
    }
    let mut vals: Vec<Value> = items.into_iter().map(Value::Number).collect();
    // A comparator: stable merge sort through the array implementation.
    array::sort_values(rt, &mut vals, cmp)?;
    Ok(vals.into_iter().map(|v| if let Value::Number(n) = v { n } else { f64::NAN }).collect())
}

fn ta_sort(rt: &mut Realm, c: &Call) -> JsResult {
    let cmp = c.arg(0);
    if !cmp.is_undefined() && !rt.is_callable(&cmp) {
        return Err(type_error(rt, "The comparison function must be either a function or undefined"));
    }
    let o = this_ta(rt, c, "sort")?;
    let items = sorted(rt, o, &cmp)?;
    for (i, v) in items.into_iter().enumerate() {
        if rt.ta_index(o, i as f64).is_some() {
            rt.ta_put(o, i, v);
        }
    }
    Ok(c.this.clone())
}

fn same_kind_copy(rt: &mut Realm, o: ObjRef, items: &[f64]) -> Result<ObjRef, Value> {
    let kind = rt.typed_array(o).unwrap().0;
    let proto = rt.intr.typed_protos[kind as usize];
    let t = allocate(rt, kind, items.len(), proto)?;
    for (i, v) in items.iter().enumerate() {
        rt.ta_put(t, i, *v);
    }
    Ok(t)
}

fn ta_to_sorted(rt: &mut Realm, c: &Call) -> JsResult {
    let cmp = c.arg(0);
    if !cmp.is_undefined() && !rt.is_callable(&cmp) {
        return Err(type_error(rt, "The comparison function must be either a function or undefined"));
    }
    let o = this_ta(rt, c, "toSorted")?;
    let items = sorted(rt, o, &cmp)?;
    Ok(Value::Object(same_kind_copy(rt, o, &items)?))
}

fn ta_to_reversed(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_ta(rt, c, "toReversed")?;
    let len = ta_len(rt, o);
    let items: Vec<f64> = (0..len).rev().map(|i| rt.ta_get(o, i)).collect();
    Ok(Value::Object(same_kind_copy(rt, o, &items)?))
}

fn ta_with(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_ta(rt, c, "with")?;
    let len = ta_len(rt, o) as f64;
    let rel = rt.to_integer(&c.arg(0))?;
    let idx = if rel >= 0.0 { rel } else { len + rel };
    let v = rt.to_number(&c.arg(1))?;
    if rt.ta_index(o, idx).is_none() {
        return Err(rt.range_error("Invalid typed array index"));
    }
    let mut items: Vec<f64> = (0..len as usize).map(|i| rt.ta_get(o, i)).collect();
    items[idx as usize] = v;
    Ok(Value::Object(same_kind_copy(rt, o, &items)?))
}

macro_rules! getter_fn {
    ($name:ident, $label:expr, |$rt:ident, $o:ident, $t:ident| $body:expr) => {
        fn $name($rt: &mut Realm, c: &Call) -> JsResult {
            let Value::Object($o) = c.this else {
                return Err(type_error($rt, concat!("%TypedArray%.prototype.", $label, " called on a non-object")));
            };
            let Some($t) = $rt.typed_array($o) else {
                return Err(type_error($rt, concat!("%TypedArray%.prototype.", $label, ": this is not a typed array")));
            };
            Ok($body)
        }
    };
}

getter_fn!(ta_buffer, "buffer", |rt, o, t| Value::Object(t.1));
getter_fn!(ta_byte_length, "byteLength", |rt, o, t| Value::Number((t.3 * t.0.size()) as f64));
getter_fn!(ta_byte_offset, "byteOffset", |rt, o, t| Value::Number(
    if t.3 == 0 && matches!(&rt.heap.get(t.1).kind, Kind::ArrayBuffer(b) if b.detached) { 0.0 } else { t.2 as f64 }
));
getter_fn!(ta_length, "length", |rt, o, t| Value::Number(t.3 as f64));

fn ta_tag(rt: &mut Realm, c: &Call) -> JsResult {
    match c.this {
        Value::Object(o) => Ok(rt.typed_array(o).map(|t| Value::str(t.0.name())).unwrap_or(Value::Undefined)),
        _ => Ok(Value::Undefined),
    }
}

fn ta_from(rt: &mut Realm, c: &Call) -> JsResult {
    if !rt.is_constructor(&c.this) {
        return Err(type_error(rt, "TypedArray.from: this is not a constructor"));
    }
    let map = c.arg(1);
    if !map.is_undefined() && !rt.is_callable(&map) {
        return Err(type_error(rt, "TypedArray.from: the map function is not callable"));
    }
    let src = c.arg(0);
    let values = match rt.get_method(&src, &PropKey::Sym(Sym::ITERATOR))? {
        Some(_) => rt.iterate_to_vec(&src)?,
        None => rt.list_from_array_like(&src)?,
    };
    let t = create_from_ctor(rt, &c.this, &[Value::Number(values.len() as f64)], Some(values.len()))?;
    for (i, v) in values.into_iter().enumerate() {
        let v = if map.is_undefined() { v } else { rt.call(&map, c.arg(2), &[v, Value::Number(i as f64)])? };
        rt.set(t, PropKey::index(i as u32), v, Value::Object(t))?;
    }
    Ok(Value::Object(t))
}

fn ta_of(rt: &mut Realm, c: &Call) -> JsResult {
    if !rt.is_constructor(&c.this) {
        return Err(type_error(rt, "TypedArray.of: this is not a constructor"));
    }
    let t = create_from_ctor(rt, &c.this, &[Value::Number(c.args.len() as f64)], Some(c.args.len()))?;
    for (i, v) in c.args.iter().enumerate() {
        rt.set(t, PropKey::index(i as u32), v.clone(), Value::Object(t))?;
    }
    Ok(Value::Object(t))
}

// ---------------------------------------------------------------- DataView

fn view_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(type_error(rt, "Constructor DataView requires 'new'"));
    }
    let Value::Object(b) = c.arg(0) else {
        return Err(type_error(rt, "First argument to DataView constructor must be an ArrayBuffer"));
    };
    if !matches!(rt.heap.get(b).kind, Kind::ArrayBuffer(_)) {
        return Err(type_error(rt, "First argument to DataView constructor must be an ArrayBuffer"));
    }
    let offset = to_index(rt, &c.arg(1), "DataView offset")?;
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot construct a DataView on a detached ArrayBuffer"));
    }
    let buf_len = buffer_bytes(rt, b).0.len();
    if offset > buf_len {
        return Err(rt.range_error(&format!("Start offset {offset} is outside the bounds of the buffer")));
    }
    let len = if c.arg(2).is_undefined() {
        buf_len - offset
    } else {
        let l = to_index(rt, &c.arg(2), "DataView length")?;
        if offset + l > buf_len {
            return Err(rt.range_error(&format!("Invalid DataView length {l}")));
        }
        l
    };
    let dp = rt.intr.data_view_proto;
    let proto = rt.proto_from_ctor(&c.new_target, dp)?;
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot construct a DataView on a detached ArrayBuffer"));
    }
    Ok(Value::Object(rt.alloc(Obj::new(Some(proto), Kind::DataView(Box::new((b, offset, len)))))))
}

fn this_view(rt: &mut Realm, c: &Call, method: &str) -> Result<(ObjRef, usize, usize), Value> {
    match &c.this {
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::DataView(d) => Ok(**d),
            _ => Err(type_error(rt, &format!("DataView.prototype.{method} called on an incompatible receiver"))),
        },
        _ => Err(type_error(rt, &format!("DataView.prototype.{method} called on an incompatible receiver"))),
    }
}

fn view_buffer(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Object(this_view(rt, c, "buffer")?.0))
}

fn view_byte_length(rt: &mut Realm, c: &Call) -> JsResult {
    let (b, _, len) = this_view(rt, c, "byteLength")?;
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot perform DataView.prototype.byteLength on a detached ArrayBuffer"));
    }
    Ok(Value::Number(len as f64))
}

fn view_byte_offset(rt: &mut Realm, c: &Call) -> JsResult {
    let (b, off, _) = this_view(rt, c, "byteOffset")?;
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot perform DataView.prototype.byteOffset on a detached ArrayBuffer"));
    }
    Ok(Value::Number(off as f64))
}

/// get<Type> / set<Type>: the element type is in slot 0.
fn view_get(rt: &mut Realm, c: &Call) -> JsResult {
    let kind = kind_of_ctor(rt, c.callee);
    let (b, off, len) = this_view(rt, c, "get")?;
    let idx = to_index(rt, &c.arg(0), "offset")?;
    let little = c.arg(1).truthy();
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot perform DataView get on a detached ArrayBuffer"));
    }
    let size = kind.size();
    if idx + size > len {
        return Err(rt.range_error("Offset is outside the bounds of the DataView"));
    }
    let mut bytes = buffer_bytes(rt, b).0[off + idx..off + idx + size].to_vec();
    if !little {
        bytes.reverse();
    }
    Ok(Value::Number(kind.read(&bytes)))
}

fn view_set(rt: &mut Realm, c: &Call) -> JsResult {
    let kind = kind_of_ctor(rt, c.callee);
    let (b, off, len) = this_view(rt, c, "set")?;
    let idx = to_index(rt, &c.arg(0), "offset")?;
    let v = rt.to_number(&c.arg(1))?;
    let little = c.arg(2).truthy();
    if buffer_bytes(rt, b).1 {
        return Err(type_error(rt, "Cannot perform DataView set on a detached ArrayBuffer"));
    }
    let size = kind.size();
    if idx + size > len {
        return Err(rt.range_error("Offset is outside the bounds of the DataView"));
    }
    let mut bytes = alloc::vec![0u8; size];
    kind.write(&mut bytes, v);
    if !little {
        bytes.reverse();
    }
    if let Kind::ArrayBuffer(a) = &mut rt.heap.get_mut(b).kind {
        a.bytes[off + idx..off + idx + size].copy_from_slice(&bytes);
    }
    Ok(Value::Undefined)
}

fn get_species(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(c.this.clone())
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;

    // ArrayBuffer.
    let bp = rt.new_object_with(Some(op));
    rt.intr.array_buffer_proto = bp;
    let bctor = constructor(rt, "ArrayBuffer", 1, buffer_ctor, bp);
    rt.intr.array_buffer_ctor = bctor;
    rt.method(bctor, "isView", 1, is_view);
    let sp = rt.native("get [Symbol.species]", 0, get_species, false);
    rt.define_accessor(bctor, Sym::SPECIES, Value::Object(sp), Value::Undefined, CONFIGURABLE);
    rt.getter(bp, "byteLength", byte_length);
    rt.getter(bp, "maxByteLength", byte_length);
    rt.getter(bp, "detached", detached_get);
    rt.getter(bp, "resizable", resizable);
    rt.method(bp, "slice", 2, buffer_slice);
    to_string_tag(rt, bp, "ArrayBuffer");

    // %TypedArray%.
    let tap = rt.new_object_with(Some(op));
    let tactor = rt.native("TypedArray", 0, abstract_ctor, true);
    rt.define(tactor, "prototype", Value::Object(tap), 0);
    rt.define(tap, "constructor", Value::Object(tactor), HIDDEN);
    rt.method(tactor, "from", 1, ta_from);
    rt.method(tactor, "of", 0, ta_of);
    let sp = rt.native("get [Symbol.species]", 0, get_species, false);
    rt.define_accessor(tactor, Sym::SPECIES, Value::Object(sp), Value::Undefined, CONFIGURABLE);
    for (name, len, f) in [
        ("at", 1, ta_at as NativeFn),
        ("copyWithin", 2, ta_copy_within),
        ("entries", 0, ta_entries),
        ("every", 1, ta_every),
        ("fill", 1, ta_fill),
        ("filter", 1, ta_filter),
        ("find", 1, ta_find),
        ("findIndex", 1, ta_find_index),
        ("findLast", 1, ta_find_last),
        ("findLastIndex", 1, ta_find_last_index),
        ("forEach", 1, ta_for_each),
        ("includes", 1, ta_includes),
        ("indexOf", 1, ta_index_of),
        ("join", 1, ta_join),
        ("keys", 0, ta_keys),
        ("lastIndexOf", 1, ta_last_index_of),
        ("map", 1, ta_map),
        ("reduce", 1, ta_reduce),
        ("reduceRight", 1, ta_reduce_right),
        ("reverse", 0, ta_reverse),
        ("set", 1, ta_set),
        ("slice", 2, ta_slice),
        ("some", 1, ta_some),
        ("sort", 1, ta_sort),
        ("subarray", 2, ta_subarray),
        ("toLocaleString", 0, ta_to_locale_string),
        ("toReversed", 0, ta_to_reversed),
        ("toSorted", 1, ta_to_sorted),
        ("with", 2, ta_with),
    ] {
        rt.method(tap, name, len, f);
    }
    let values = rt.method(tap, "values", 0, ta_values);
    rt.define(tap, Sym::ITERATOR, Value::Object(values), HIDDEN);
    // toString is shared with Array.prototype.
    let ap = rt.intr.array_proto;
    if let Ok(ts) = rt.get_str(ap, "toString") {
        rt.define(tap, "toString", ts, HIDDEN);
    }
    rt.getter(tap, "buffer", ta_buffer);
    rt.getter(tap, "byteLength", ta_byte_length);
    rt.getter(tap, "byteOffset", ta_byte_offset);
    rt.getter(tap, "length", ta_length);
    let tag = rt.native("get [Symbol.toStringTag]", 0, ta_tag, false);
    rt.define_accessor(tap, Sym::TO_STRING_TAG, Value::Object(tag), Value::Undefined, CONFIGURABLE);

    // The nine element types.
    for (i, kind) in TAKind::ALL.iter().enumerate() {
        let proto = rt.new_object_with(Some(tap));
        let ctor = rt.native_with(kind.name(), 3, typed_ctor, true, alloc::vec![Value::Number(i as f64)]);
        rt.heap.get_mut(ctor).proto = Some(tactor);
        rt.define(ctor, "prototype", Value::Object(proto), 0);
        rt.define(proto, "constructor", Value::Object(ctor), HIDDEN);
        rt.define(ctor, "BYTES_PER_ELEMENT", Value::Number(kind.size() as f64), 0);
        rt.define(proto, "BYTES_PER_ELEMENT", Value::Number(kind.size() as f64), 0);
        rt.set_global(kind.name(), Value::Object(ctor));
        rt.intr.typed_protos[i] = proto;
        rt.intr.typed_ctors[i] = ctor;
    }

    // DataView.
    let dp = rt.new_object_with(Some(op));
    rt.intr.data_view_proto = dp;
    constructor(rt, "DataView", 1, view_ctor, dp);
    rt.getter(dp, "buffer", view_buffer);
    rt.getter(dp, "byteLength", view_byte_length);
    rt.getter(dp, "byteOffset", view_byte_offset);
    for (i, kind) in TAKind::ALL.iter().enumerate() {
        if *kind == TAKind::U8C {
            continue;
        }
        let ty = kind.name().trim_end_matches("Array");
        let g = rt.native_with(&format!("get{ty}"), 1, view_get, false, alloc::vec![Value::Number(i as f64)]);
        rt.define(dp, format!("get{ty}").as_str(), Value::Object(g), HIDDEN);
        let s = rt.native_with(&format!("set{ty}"), 2, view_set, false, alloc::vec![Value::Number(i as f64)]);
        rt.define(dp, format!("set{ty}").as_str(), Value::Object(s), HIDDEN);
    }
    to_string_tag(rt, dp, "DataView");
}
