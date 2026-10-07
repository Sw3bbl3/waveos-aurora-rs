//! Array, Array.prototype and array iterators. The methods are generic
//! (they work on any array-like), with fast paths for dense arrays.

use super::*;
use crate::numconv;
use crate::value::JsStr;
use alloc::string::String;
use core::cmp::Ordering;

pub fn idx_key(i: f64) -> PropKey {
    if (0.0..4294967295.0).contains(&i) && i == libm::trunc(i) {
        PropKey::index(i as u32)
    } else {
        PropKey::Str(JsStr::from(numconv::to_string(i)))
    }
}

fn get_i(rt: &mut Realm, o: ObjRef, i: f64) -> JsResult {
    rt.tick()?;
    if let Kind::Array(a) = &rt.heap.get(o).kind {
        if let Some(v) = a.dense.get(i as usize) {
            if *v != Value::Empty && i == libm::trunc(i) {
                return Ok(v.clone());
            }
        }
    }
    rt.get(o, &idx_key(i), Value::Object(o))
}

fn set_i(rt: &mut Realm, o: ObjRef, i: f64, v: Value) -> Result<(), Value> {
    let k = idx_key(i);
    if !rt.set(o, k.clone(), v, Value::Object(o))? {
        let d = rt.key_display(&k);
        return Err(rt.type_error(&alloc::format!("Cannot assign to read only property '{d}'")));
    }
    Ok(())
}

fn has_i(rt: &mut Realm, o: ObjRef, i: f64) -> Result<bool, Value> {
    rt.tick()?;
    if let Kind::Array(a) = &rt.heap.get(o).kind {
        if let Some(v) = a.dense.get(i as usize) {
            if *v != Value::Empty {
                return Ok(true);
            }
        }
    }
    rt.has_property(o, &idx_key(i))
}

fn delete_i(rt: &mut Realm, o: ObjRef, i: f64) -> Result<(), Value> {
    let k = idx_key(i);
    if !rt.delete(o, &k)? {
        let d = rt.key_display(&k);
        return Err(rt.type_error(&alloc::format!("Cannot delete property '{d}'")));
    }
    Ok(())
}

fn set_length(rt: &mut Realm, o: ObjRef, len: f64) -> Result<(), Value> {
    if !rt.set(o, key("length"), Value::Number(len), Value::Object(o))? {
        return Err(rt.type_error("Cannot assign to read only property 'length'"));
    }
    Ok(())
}

fn this_len(rt: &mut Realm, c: &Call) -> Result<(ObjRef, f64), Value> {
    let o = rt.to_object(&c.this)?;
    let len = rt.length_of(o)?;
    Ok((o, len))
}

/// The dense elements of a plain array without holes, if it is one.
fn dense(rt: &Realm, o: ObjRef) -> Option<&Vec<Value>> {
    match &rt.heap.get(o).kind {
        Kind::Array(a) if a.dense.len() == a.len as usize && !a.dense.contains(&Value::Empty) => Some(&a.dense),
        _ => None,
    }
}

/// ArraySpeciesCreate.
fn species_create(rt: &mut Realm, original: ObjRef, len: f64) -> Result<ObjRef, Value> {
    if !matches!(rt.heap.get(original).kind, Kind::Array(_)) {
        return array_create(rt, len);
    }
    let mut c = rt.get(original, &key("constructor"), Value::Object(original))?;
    if let Value::Object(co) = c {
        c = rt.get(co, &PropKey::Sym(Sym::SPECIES), c.clone())?;
        if c == Value::Null {
            c = Value::Undefined;
        }
    }
    if c.is_undefined() || c == Value::Object(rt.intr.array_ctor) {
        return array_create(rt, len);
    }
    if !rt.is_constructor(&c) {
        return Err(rt.type_error("object.constructor[Symbol.species] is not a constructor"));
    }
    let r = rt.construct(&c, &[Value::Number(len)], None)?;
    r.as_object().ok_or_else(|| rt.type_error("species constructor returned a non-object"))
}

fn array_create(rt: &mut Realm, len: f64) -> Result<ObjRef, Value> {
    if len > 4294967295.0 {
        return Err(rt.range_error("Invalid array length"));
    }
    let a = rt.new_array(Vec::new());
    if len > 0.0 {
        if let Kind::Array(arr) = &mut rt.heap.get_mut(a).kind {
            arr.len = len as u32;
        }
    }
    Ok(a)
}

fn array_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let nt = if c.is_construct() { c.new_target.clone() } else { Value::Object(c.callee) };
    let ap = rt.intr.array_proto;
    let proto = rt.proto_from_ctor(&nt, ap)?;
    let a = if c.args.len() == 1 {
        match c.arg(0) {
            Value::Number(n) => {
                if numconv::to_uint32(n) as f64 != n {
                    return Err(rt.range_error("Invalid array length"));
                }
                array_create(rt, n)?
            }
            v => rt.new_array(alloc::vec![v]),
        }
    } else {
        rt.new_array(c.args.clone())
    };
    rt.heap.get_mut(a).proto = Some(proto);
    Ok(Value::Object(a))
}

fn is_array(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(rt.is_array(&c.arg(0))))
}

fn of(rt: &mut Realm, c: &Call) -> JsResult {
    let len = c.args.len() as f64;
    let a = if rt.is_constructor(&c.this) && c.this != Value::Object(rt.intr.array_ctor) {
        let r = rt.construct(&c.this, &[Value::Number(len)], None)?;
        r.as_object().unwrap()
    } else {
        return Ok(rt.array_from(c.args.clone()));
    };
    for (i, v) in c.args.iter().enumerate() {
        rt.create_data_property_or_throw(a, PropKey::index(i as u32), v.clone())?;
    }
    set_length(rt, a, len)?;
    Ok(Value::Object(a))
}

fn from(rt: &mut Realm, c: &Call) -> JsResult {
    let items = c.arg(0);
    let map = c.arg(1);
    let this_arg = c.arg(2);
    if !map.is_undefined() {
        require_callable(rt, &map, "Array.from")?;
    }
    let custom = rt.is_constructor(&c.this) && c.this != Value::Object(rt.intr.array_ctor);
    let iter_method = if items.is_nullish() { None } else { rt.get_method(&items, &PropKey::Sym(Sym::ITERATOR))? };
    let target = if custom {
        let r = rt.construct(&c.this, &[], None)?;
        r.as_object().unwrap()
    } else {
        rt.new_array(Vec::new())
    };
    let mut n: u32 = 0;
    if iter_method.is_some() {
        // Step by step: the iterator may be endless, and a throwing mapper closes it.
        let rec = rt.get_iterator(&items)?;
        loop {
            let Some(v) = rt.iter_step(rec)? else { break };
            let v =
                if map.is_undefined() { Ok(v) } else { rt.call(&map, this_arg.clone(), &[v, Value::Number(n as f64)]) };
            let r = v.and_then(|v| rt.create_data_property_or_throw(target, PropKey::index(n), v));
            if let Err(e) = r {
                rt.iter_close(rec, true)?;
                return Err(e);
            }
            n += 1;
        }
    } else {
        let o = rt.to_object(&items)?;
        let len = rt.length_of(o)?;
        while (n as f64) < len {
            let v = get_i(rt, o, n as f64)?;
            let v =
                if map.is_undefined() { v } else { rt.call(&map, this_arg.clone(), &[v, Value::Number(n as f64)])? };
            rt.create_data_property_or_throw(target, PropKey::index(n), v)?;
            n += 1;
        }
    }
    set_length(rt, target, n as f64)?;
    Ok(Value::Object(target))
}

fn push(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        let ob = rt.heap.get_mut(o);
        let ext = ob.extensible;
        if let Kind::Array(a) = &mut ob.kind {
            if ext
                && a.len_writable
                && a.dense.len() == a.len as usize
                && (a.len as usize + c.args.len()) < u32::MAX as usize
            {
                a.dense.extend(c.args.iter().cloned());
                a.len = a.dense.len() as u32;
                return Ok(Value::Number(a.len as f64));
            }
        }
    }
    let (o, len) = this_len(rt, c)?;
    if len + c.args.len() as f64 > 9007199254740991.0 {
        return Err(rt.type_error("Pushing too many elements"));
    }
    let mut n = len;
    for v in &c.args {
        set_i(rt, o, n, v.clone())?;
        n += 1.0;
    }
    set_length(rt, o, n)?;
    Ok(Value::Number(n))
}

fn pop(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        if let Kind::Array(a) = &mut rt.heap.get_mut(o).kind {
            if a.len_writable && a.dense.len() == a.len as usize && !a.dense.is_empty() {
                let v = a.dense.pop().unwrap();
                a.len -= 1;
                return Ok(v.or_undefined());
            }
        }
    }
    let (o, len) = this_len(rt, c)?;
    if len == 0.0 {
        set_length(rt, o, 0.0)?;
        return Ok(Value::Undefined);
    }
    let v = get_i(rt, o, len - 1.0)?;
    delete_i(rt, o, len - 1.0)?;
    set_length(rt, o, len - 1.0)?;
    Ok(v)
}

fn shift(rt: &mut Realm, c: &Call) -> JsResult {
    if let Value::Object(o) = c.this {
        if let Kind::Array(a) = &mut rt.heap.get_mut(o).kind {
            if a.len_writable
                && a.dense.len() == a.len as usize
                && !a.dense.is_empty()
                && !a.dense.contains(&Value::Empty)
            {
                let v = a.dense.remove(0);
                a.len -= 1;
                return Ok(v);
            }
        }
    }
    let (o, len) = this_len(rt, c)?;
    if len == 0.0 {
        set_length(rt, o, 0.0)?;
        return Ok(Value::Undefined);
    }
    let first = get_i(rt, o, 0.0)?;
    let mut k = 1.0;
    while k < len {
        if has_i(rt, o, k)? {
            let v = get_i(rt, o, k)?;
            set_i(rt, o, k - 1.0, v)?;
        } else {
            delete_i(rt, o, k - 1.0)?;
        }
        k += 1.0;
    }
    delete_i(rt, o, len - 1.0)?;
    set_length(rt, o, len - 1.0)?;
    Ok(first)
}

fn unshift(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let n = c.args.len() as f64;
    if n > 0.0 {
        if let Kind::Array(a) = &mut rt.heap.get_mut(o).kind {
            if a.len_writable && a.dense.len() == a.len as usize && !a.dense.contains(&Value::Empty) {
                let mut v = c.args.clone();
                v.append(&mut a.dense);
                a.dense = v;
                a.len = a.dense.len() as u32;
                return Ok(Value::Number(a.len as f64));
            }
        }
        let mut k = len;
        while k > 0.0 {
            let from = k - 1.0;
            let to = k + n - 1.0;
            if has_i(rt, o, from)? {
                let v = get_i(rt, o, from)?;
                set_i(rt, o, to, v)?;
            } else {
                delete_i(rt, o, to)?;
            }
            k -= 1.0;
        }
        for (j, v) in c.args.iter().enumerate() {
            set_i(rt, o, j as f64, v.clone())?;
        }
    }
    set_length(rt, o, len + n)?;
    Ok(Value::Number(len + n))
}

fn slice(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let start = rt.relative_index(&c.arg(0), len, 0.0)?;
    let end = rt.relative_index(&c.arg(1), len, len)?;
    let count = (end - start).max(0.0);
    if let Some(d) = dense(rt, o) {
        if matches!(rt.heap.get(o).proto, Some(p) if p == rt.intr.array_proto) {
            let items = d[start as usize..(start + count) as usize].to_vec();
            return Ok(rt.array_from(items));
        }
    }
    let a = species_create(rt, o, count)?;
    let mut n = 0.0;
    let mut k = start;
    while k < end {
        if has_i(rt, o, k)? {
            let v = get_i(rt, o, k)?;
            rt.create_data_property_or_throw(a, idx_key(n), v)?;
        }
        k += 1.0;
        n += 1.0;
    }
    set_length(rt, a, n)?;
    Ok(Value::Object(a))
}

fn splice(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let start = rt.relative_index(&c.arg(0), len, 0.0)?;
    let delete_count = match c.args.len() {
        0 => 0.0,
        1 => len - start,
        _ => {
            let d = rt.to_integer(&c.arg(1))?;
            d.clamp(0.0, len - start)
        }
    };
    let items: Vec<Value> = c.args.iter().skip(2).cloned().collect();
    let item_count = items.len() as f64;
    let removed = species_create(rt, o, delete_count)?;
    let mut k = 0.0;
    while k < delete_count {
        if has_i(rt, o, start + k)? {
            let v = get_i(rt, o, start + k)?;
            rt.create_data_property_or_throw(removed, idx_key(k), v)?;
        }
        k += 1.0;
    }
    set_length(rt, removed, delete_count)?;
    if item_count < delete_count {
        let mut k = start;
        while k < len - delete_count {
            let (from, to) = (k + delete_count, k + item_count);
            if has_i(rt, o, from)? {
                let v = get_i(rt, o, from)?;
                set_i(rt, o, to, v)?;
            } else {
                delete_i(rt, o, to)?;
            }
            k += 1.0;
        }
        let mut k = len;
        while k > len - delete_count + item_count {
            delete_i(rt, o, k - 1.0)?;
            k -= 1.0;
        }
    } else if item_count > delete_count {
        let mut k = len - delete_count;
        while k > start {
            let (from, to) = (k + delete_count - 1.0, k + item_count - 1.0);
            if has_i(rt, o, from)? {
                let v = get_i(rt, o, from)?;
                set_i(rt, o, to, v)?;
            } else {
                delete_i(rt, o, to)?;
            }
            k -= 1.0;
        }
    }
    for (j, v) in items.into_iter().enumerate() {
        set_i(rt, o, start + j as f64, v)?;
    }
    set_length(rt, o, len - delete_count + item_count)?;
    Ok(Value::Object(removed))
}

fn concat(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    let a = species_create(rt, o, 0.0)?;
    let mut n = 0.0;
    let mut all = alloc::vec![Value::Object(o)];
    all.extend(c.args.iter().cloned());
    for item in all {
        let spreadable = match &item {
            Value::Object(io) => {
                let s = rt.get(*io, &PropKey::Sym(Sym::IS_CONCAT_SPREADABLE), item.clone())?;
                if s.is_undefined() {
                    rt.is_array(&item)
                } else {
                    s.truthy()
                }
            }
            _ => false,
        };
        if spreadable {
            let io = item.as_object().unwrap();
            let len = rt.length_of(io)?;
            let mut k = 0.0;
            while k < len {
                if has_i(rt, io, k)? {
                    let v = get_i(rt, io, k)?;
                    rt.create_data_property_or_throw(a, idx_key(n), v)?;
                }
                k += 1.0;
                n += 1.0;
            }
        } else {
            rt.create_data_property_or_throw(a, idx_key(n), item)?;
            n += 1.0;
        }
    }
    set_length(rt, a, n)?;
    Ok(Value::Object(a))
}

pub fn join_values(rt: &mut Realm, o: ObjRef, sep: &JsStr) -> Result<JsStr, Value> {
    let len = rt.length_of(o)?;
    let mut out: Vec<u16> = Vec::new();
    let mut k = 0.0;
    while k < len {
        if k > 0.0 {
            out.extend_from_slice(sep.units());
        }
        let v = get_i(rt, o, k)?;
        if !v.is_nullish() {
            out.extend_from_slice(rt.to_string(&v)?.units());
        }
        k += 1.0;
    }
    Ok(JsStr::from_units(out))
}

fn join(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    let sep = if c.arg(0).is_undefined() { JsStr::from(",") } else { rt.to_string(&c.arg(0))? };
    // Cycles (an array containing itself) join as empty.
    if rt.join_stack.contains(&o) {
        return Ok(Value::str(""));
    }
    rt.join_stack.push(o);
    let r = join_values(rt, o, &sep);
    rt.join_stack.pop();
    Ok(Value::String(r?))
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    let j = rt.get(o, &key("join"), Value::Object(o))?;
    if rt.is_callable(&j) {
        return rt.call(&j, Value::Object(o), &[]);
    }
    object::proto_to_string(rt, c)
}

fn to_locale_string(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let mut parts = Vec::new();
    let mut k = 0.0;
    while k < len {
        let v = get_i(rt, o, k)?;
        if v.is_nullish() {
            parts.push(String::new());
        } else {
            let f = rt.get_v(&v, &key("toLocaleString"))?;
            let s = rt.call(&f, v, &[])?;
            parts.push(rt.to_rust_string(&s)?);
        }
        k += 1.0;
    }
    Ok(Value::str(&parts.join(",")))
}

fn reverse(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    if let Kind::Array(a) = &mut rt.heap.get_mut(o).kind {
        if a.dense.len() == a.len as usize && !a.dense.contains(&Value::Empty) {
            a.dense.reverse();
            return Ok(c.this.clone());
        }
    }
    let mut lower = 0.0;
    let middle = libm::floor(len / 2.0);
    while lower != middle {
        let upper = len - lower - 1.0;
        let (le, ue) = (has_i(rt, o, lower)?, has_i(rt, o, upper)?);
        let lv = if le { get_i(rt, o, lower)? } else { Value::Undefined };
        let uv = if ue { get_i(rt, o, upper)? } else { Value::Undefined };
        match (le, ue) {
            (true, true) => {
                set_i(rt, o, lower, uv)?;
                set_i(rt, o, upper, lv)?;
            }
            (false, true) => {
                set_i(rt, o, lower, uv)?;
                delete_i(rt, o, upper)?;
            }
            (true, false) => {
                delete_i(rt, o, lower)?;
                set_i(rt, o, upper, lv)?;
            }
            _ => {}
        }
        lower += 1.0;
    }
    Ok(c.this.clone())
}

fn index_of(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    if len == 0.0 {
        return Ok(Value::Number(-1.0));
    }
    let mut k = rt.relative_index(&c.arg(1), len, 0.0)?;
    let target = c.arg(0);
    while k < len {
        if has_i(rt, o, k)? {
            let v = get_i(rt, o, k)?;
            if rt.strict_eq(&v, &target) {
                return Ok(Value::Number(k));
            }
        }
        k += 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn last_index_of(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    if len == 0.0 {
        return Ok(Value::Number(-1.0));
    }
    let mut k = if c.args.len() > 1 {
        let n = rt.to_integer(&c.arg(1))?;
        if n >= 0.0 {
            n.min(len - 1.0)
        } else {
            len + n
        }
    } else {
        len - 1.0
    };
    let target = c.arg(0);
    while k >= 0.0 {
        if has_i(rt, o, k)? {
            let v = get_i(rt, o, k)?;
            if rt.strict_eq(&v, &target) {
                return Ok(Value::Number(k));
            }
        }
        k -= 1.0;
    }
    Ok(Value::Number(-1.0))
}

fn includes(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let mut k = rt.relative_index(&c.arg(1), len, 0.0)?;
    let target = c.arg(0);
    while k < len {
        let v = get_i(rt, o, k)?;
        if rt.same_value_zero(&v, &target) {
            return Ok(Value::Bool(true));
        }
        k += 1.0;
    }
    Ok(Value::Bool(false))
}

/// forEach/map/filter/some/every/find…: (kind) 0 forEach, 1 map, 2 filter,
/// 3 some, 4 every, 5 find, 6 findIndex, 7 findLast, 8 findLastIndex.
fn iterate(rt: &mut Realm, c: &Call, kind: u8) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let f = c.arg(0);
    require_callable(rt, &f, "Array.prototype callback")?;
    let this_arg = c.arg(1);
    let out = match kind {
        1 => Some(species_create(rt, o, len)?),
        2 => Some(species_create(rt, o, 0.0)?),
        _ => None,
    };
    let mut n = 0.0;
    let backwards = kind == 7 || kind == 8;
    let mut k = if backwards { len - 1.0 } else { 0.0 };
    loop {
        if (backwards && k < 0.0) || (!backwards && k >= len) {
            break;
        }
        // find* visit holes; the others skip them.
        let present = kind >= 5 || has_i(rt, o, k)?;
        if present {
            let v = get_i(rt, o, k)?;
            let r = rt.call(&f, this_arg.clone(), &[v.clone(), Value::Number(k), Value::Object(o)])?;
            match kind {
                1 => rt.create_data_property_or_throw(out.unwrap(), idx_key(k), r)?,
                2 => {
                    if r.truthy() {
                        rt.create_data_property_or_throw(out.unwrap(), idx_key(n), v)?;
                        n += 1.0;
                    }
                }
                3 if r.truthy() => return Ok(Value::Bool(true)),
                4 if !r.truthy() => return Ok(Value::Bool(false)),
                5 | 7 if r.truthy() => return Ok(v),
                6 | 8 if r.truthy() => return Ok(Value::Number(k)),
                _ => {}
            }
        }
        k += if backwards { -1.0 } else { 1.0 };
    }
    Ok(match kind {
        1 | 2 => Value::Object(out.unwrap()),
        3 => Value::Bool(false),
        4 => Value::Bool(true),
        6 | 8 => Value::Number(-1.0),
        _ => Value::Undefined,
    })
}

macro_rules! iter_fn {
    ($name:ident, $k:expr) => {
        fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            iterate(rt, c, $k)
        }
    };
}
iter_fn!(for_each, 0);
iter_fn!(map, 1);
iter_fn!(filter, 2);
iter_fn!(some, 3);
iter_fn!(every, 4);
iter_fn!(find, 5);
iter_fn!(find_index, 6);
iter_fn!(find_last, 7);
iter_fn!(find_last_index, 8);

fn reduce_impl(rt: &mut Realm, c: &Call, right: bool) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let f = c.arg(0);
    require_callable(rt, &f, "reduce")?;
    let mut k = if right { len - 1.0 } else { 0.0 };
    let step = if right { -1.0 } else { 1.0 };
    let in_range = |k: f64| if right { k >= 0.0 } else { k < len };
    let mut acc = if c.args.len() >= 2 {
        c.arg(1)
    } else {
        loop {
            if !in_range(k) {
                return Err(rt.type_error("Reduce of empty array with no initial value"));
            }
            if has_i(rt, o, k)? {
                let v = get_i(rt, o, k)?;
                k += step;
                break v;
            }
            k += step;
        }
    };
    while in_range(k) {
        if has_i(rt, o, k)? {
            let v = get_i(rt, o, k)?;
            acc = rt.call(&f, Value::Undefined, &[acc, v, Value::Number(k), Value::Object(o)])?;
        }
        k += step;
    }
    Ok(acc)
}

fn reduce(rt: &mut Realm, c: &Call) -> JsResult {
    reduce_impl(rt, c, false)
}

fn reduce_right(rt: &mut Realm, c: &Call) -> JsResult {
    reduce_impl(rt, c, true)
}

fn fill(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let start = rt.relative_index(&c.arg(1), len, 0.0)?;
    let end = rt.relative_index(&c.arg(2), len, len)?;
    let mut k = start;
    while k < end {
        set_i(rt, o, k, c.arg(0))?;
        k += 1.0;
    }
    Ok(Value::Object(o))
}

fn copy_within(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let to = rt.relative_index(&c.arg(0), len, 0.0)?;
    let from = rt.relative_index(&c.arg(1), len, 0.0)?;
    let fin = rt.relative_index(&c.arg(2), len, len)?;
    let count = (fin - from).min(len - to);
    if count <= 0.0 {
        return Ok(Value::Object(o));
    }
    let (mut f, mut t, dir) =
        if from < to && to < from + count { (from + count - 1.0, to + count - 1.0, -1.0) } else { (from, to, 1.0) };
    let mut n = count;
    while n > 0.0 {
        if has_i(rt, o, f)? {
            let v = get_i(rt, o, f)?;
            set_i(rt, o, t, v)?;
        } else {
            delete_i(rt, o, t)?;
        }
        f += dir;
        t += dir;
        n -= 1.0;
    }
    Ok(Value::Object(o))
}

fn flatten_into(
    rt: &mut Realm,
    target: ObjRef,
    src: ObjRef,
    len: f64,
    start: f64,
    depth: f64,
    map: Option<(&Value, &Value)>,
) -> Result<f64, Value> {
    let mut n = start;
    let mut k = 0.0;
    while k < len {
        if has_i(rt, src, k)? {
            let mut v = get_i(rt, src, k)?;
            if let Some((f, this_arg)) = map {
                v = rt.call(f, this_arg.clone(), &[v, Value::Number(k), Value::Object(src)])?;
            }
            if depth > 0.0 && rt.is_array(&v) {
                let vo = v.as_object().unwrap();
                let vl = rt.length_of(vo)?;
                n = flatten_into(rt, target, vo, vl, n, depth - 1.0, None)?;
            } else {
                rt.create_data_property_or_throw(target, idx_key(n), v)?;
                n += 1.0;
            }
        }
        k += 1.0;
    }
    Ok(n)
}

fn flat(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let depth = if c.arg(0).is_undefined() { 1.0 } else { rt.to_integer(&c.arg(0))?.max(0.0) };
    let a = species_create(rt, o, 0.0)?;
    flatten_into(rt, a, o, len, 0.0, depth, None)?;
    Ok(Value::Object(a))
}

fn flat_map(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let f = c.arg(0);
    require_callable(rt, &f, "flatMap")?;
    let a = species_create(rt, o, 0.0)?;
    let t = c.arg(1);
    flatten_into(rt, a, o, len, 0.0, 1.0, Some((&f, &t)))?;
    Ok(Value::Object(a))
}

fn at(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let i = rt.to_integer(&c.arg(0))?;
    let k = if i >= 0.0 { i } else { len + i };
    if k < 0.0 || k >= len {
        return Ok(Value::Undefined);
    }
    get_i(rt, o, k)
}

fn compare(rt: &mut Realm, cmp: &Value, a: &Value, b: &Value) -> Result<Ordering, Value> {
    match (a.is_undefined(), b.is_undefined()) {
        (true, true) => return Ok(Ordering::Equal),
        (true, false) => return Ok(Ordering::Greater),
        (false, true) => return Ok(Ordering::Less),
        _ => {}
    }
    if !cmp.is_undefined() {
        let r = rt.call(cmp, Value::Undefined, &[a.clone(), b.clone()])?;
        let n = rt.to_number(&r)?;
        return Ok(if n < 0.0 {
            Ordering::Less
        } else if n > 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        });
    }
    if let (Value::Number(x), Value::Number(y)) = (a, b) {
        if x == y {
            return Ok(Ordering::Equal);
        }
    }
    let sa = rt.to_string(a)?;
    let sb = rt.to_string(b)?;
    Ok(sa.units().cmp(sb.units()))
}

/// A stable merge sort whose comparisons may throw.
pub fn sort_values(rt: &mut Realm, v: &mut [Value], cmp: &Value) -> Result<(), Value> {
    let n = v.len();
    if n < 2 {
        return Ok(());
    }
    // Insertion sort for small runs, then merge.
    const RUN: usize = 16;
    let mut i = 0;
    while i < n {
        let end = (i + RUN).min(n);
        for j in i + 1..end {
            let mut k = j;
            while k > i && compare(rt, cmp, &v[k - 1], &v[k])? == Ordering::Greater {
                v.swap(k - 1, k);
                k -= 1;
            }
        }
        i = end;
    }
    let mut width = RUN;
    let mut buf: Vec<Value> = Vec::with_capacity(n);
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            if mid < hi {
                buf.clear();
                let (mut a, mut b) = (lo, mid);
                while a < mid && b < hi {
                    if compare(rt, cmp, &v[b], &v[a])? == Ordering::Less {
                        buf.push(v[b].clone());
                        b += 1;
                    } else {
                        buf.push(v[a].clone());
                        a += 1;
                    }
                }
                buf.extend_from_slice(&v[a..mid]);
                buf.extend_from_slice(&v[b..hi]);
                v[lo..hi].clone_from_slice(&buf);
            }
            lo += 2 * width;
        }
        width *= 2;
    }
    Ok(())
}

fn sorted_items(rt: &mut Realm, o: ObjRef, len: f64, cmp: &Value, skip_holes: bool) -> Result<Vec<Value>, Value> {
    let mut items = Vec::new();
    let mut k = 0.0;
    while k < len {
        if !skip_holes || has_i(rt, o, k)? {
            items.push(get_i(rt, o, k)?);
        }
        k += 1.0;
    }
    sort_values(rt, &mut items, cmp)?;
    Ok(items)
}

fn sort(rt: &mut Realm, c: &Call) -> JsResult {
    let cmp = c.arg(0);
    if !cmp.is_undefined() && !rt.is_callable(&cmp) {
        return Err(rt.type_error("The comparison function must be either a function or undefined"));
    }
    let (o, len) = this_len(rt, c)?;
    let items = sorted_items(rt, o, len, &cmp, true)?;
    let n = items.len() as f64;
    for (i, v) in items.into_iter().enumerate() {
        set_i(rt, o, i as f64, v)?;
    }
    let mut k = n;
    while k < len {
        delete_i(rt, o, k)?;
        k += 1.0;
    }
    Ok(Value::Object(o))
}

fn to_sorted(rt: &mut Realm, c: &Call) -> JsResult {
    let cmp = c.arg(0);
    if !cmp.is_undefined() && !rt.is_callable(&cmp) {
        return Err(rt.type_error("The comparison function must be either a function or undefined"));
    }
    let (o, len) = this_len(rt, c)?;
    let items = sorted_items(rt, o, len, &cmp, false)?;
    Ok(rt.array_from(items))
}

fn to_reversed(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let mut items = Vec::new();
    let mut k = len - 1.0;
    while k >= 0.0 {
        items.push(get_i(rt, o, k)?);
        k -= 1.0;
    }
    Ok(rt.array_from(items))
}

fn to_spliced(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let start = rt.relative_index(&c.arg(0), len, 0.0)?;
    let skip = match c.args.len() {
        0 => 0.0,
        1 => len - start,
        _ => rt.to_integer(&c.arg(1))?.clamp(0.0, len - start),
    };
    let mut items = Vec::new();
    let mut k = 0.0;
    while k < start {
        items.push(get_i(rt, o, k)?);
        k += 1.0;
    }
    items.extend(c.args.iter().skip(2).cloned());
    let mut k = start + skip;
    while k < len {
        items.push(get_i(rt, o, k)?);
        k += 1.0;
    }
    Ok(rt.array_from(items))
}

fn with(rt: &mut Realm, c: &Call) -> JsResult {
    let (o, len) = this_len(rt, c)?;
    let rel = rt.to_integer(&c.arg(0))?;
    let idx = if rel >= 0.0 { rel } else { len + rel };
    if idx >= len || idx < 0.0 {
        return Err(rt.range_error("Invalid index"));
    }
    let mut items = Vec::new();
    let mut k = 0.0;
    while k < len {
        items.push(if k == idx { c.arg(1) } else { get_i(rt, o, k)? });
        k += 1.0;
    }
    Ok(rt.array_from(items))
}

// ---------------------------------------------------------------- iterators

fn make_iter(rt: &mut Realm, c: &Call, kind: IterKind) -> JsResult {
    let o = rt.to_object(&c.this)?;
    let p = rt.intr.array_iterator_proto;
    Ok(Value::Object(rt.alloc(Obj::new(Some(p), Kind::ArrayIterator(Value::Object(o), 0, kind)))))
}

fn values(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, IterKind::Values)
}

fn keys(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, IterKind::Keys)
}

fn entries(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, IterKind::Entries)
}

fn iter_next(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(it) = c.this else {
        return Err(rt.type_error("not an Array Iterator"));
    };
    let (target, i, kind) = match &rt.heap.get(it).kind {
        Kind::ArrayIterator(t, i, k) => (t.clone(), *i, *k),
        _ => return Err(rt.type_error("not an Array Iterator")),
    };
    let Value::Object(o) = target else {
        return Ok(rt.iter_result(Value::Undefined, true));
    };
    let len = rt.length_of(o)?;
    if i as f64 >= len {
        if let Kind::ArrayIterator(t, _, _) = &mut rt.heap.get_mut(it).kind {
            *t = Value::Undefined;
        }
        return Ok(rt.iter_result(Value::Undefined, true));
    }
    if let Kind::ArrayIterator(_, idx, _) = &mut rt.heap.get_mut(it).kind {
        *idx += 1;
    }
    let v = match kind {
        IterKind::Keys => Value::Number(i as f64),
        IterKind::Values => get_i(rt, o, i as f64)?,
        IterKind::Entries => {
            let v = get_i(rt, o, i as f64)?;
            rt.array_from(alloc::vec![Value::Number(i as f64), v])
        }
    };
    Ok(rt.iter_result(v, false))
}

fn species(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(c.this.clone())
}

pub fn init(rt: &mut Realm) {
    let ap = rt.intr.array_proto;
    let ctor = constructor(rt, "Array", 1, array_ctor, ap);
    rt.intr.array_ctor = ctor;
    rt.method(ctor, "isArray", 1, is_array);
    rt.method(ctor, "of", 0, of);
    rt.method(ctor, "from", 1, from);
    let sp = rt.native("get [Symbol.species]", 0, species, false);
    rt.define_accessor(ctor, Sym::SPECIES, Value::Object(sp), Value::Undefined, CONFIGURABLE);
    for (name, len, f) in [
        ("at", 1, at as NativeFn),
        ("concat", 1, concat),
        ("copyWithin", 2, copy_within),
        ("entries", 0, entries),
        ("every", 1, every),
        ("fill", 1, fill),
        ("filter", 1, filter),
        ("find", 1, find),
        ("findIndex", 1, find_index),
        ("findLast", 1, find_last),
        ("findLastIndex", 1, find_last_index),
        ("flat", 0, flat),
        ("flatMap", 1, flat_map),
        ("forEach", 1, for_each),
        ("includes", 1, includes),
        ("indexOf", 1, index_of),
        ("join", 1, join),
        ("keys", 0, keys),
        ("lastIndexOf", 1, last_index_of),
        ("map", 1, map),
        ("pop", 0, pop),
        ("push", 1, push),
        ("reduce", 1, reduce),
        ("reduceRight", 1, reduce_right),
        ("reverse", 0, reverse),
        ("shift", 0, shift),
        ("slice", 2, slice),
        ("some", 1, some),
        ("sort", 1, sort),
        ("splice", 2, splice),
        ("toLocaleString", 0, to_locale_string),
        ("toReversed", 0, to_reversed),
        ("toSorted", 1, to_sorted),
        ("toSpliced", 2, to_spliced),
        ("toString", 0, to_string),
        ("unshift", 1, unshift),
        ("with", 2, with),
    ] {
        rt.method(ap, name, len, f);
    }
    let v = rt.method(ap, "values", 0, values);
    rt.define(ap, Sym::ITERATOR, Value::Object(v), HIDDEN);
    rt.intr.array_values = v;
    let unscopables = rt.new_object_with(None);
    for n in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        rt.define(unscopables, n, Value::Bool(true), DEFAULT);
    }
    rt.define(ap, Sym::UNSCOPABLES, Value::Object(unscopables), CONFIGURABLE);

    let ip = rt.intr.iterator_proto;
    let aip = rt.new_object_with(Some(ip));
    rt.intr.array_iterator_proto = aip;
    let next = rt.method(aip, "next", 0, iter_next);
    rt.intr.array_iterator_next = next;
    to_string_tag(rt, aip, "Array Iterator");
}
