//! Map, Set, WeakMap and WeakSet.

use super::*;

fn this_map(rt: &mut Realm, c: &Call, set: bool) -> Result<ObjRef, Value> {
    if let Value::Object(o) = c.this {
        match (&rt.heap.get(o).kind, set) {
            (Kind::Map(_), false) | (Kind::Set(_), true) => return Ok(o),
            _ => {}
        }
    }
    Err(rt.type_error(if set {
        "Method Set.prototype called on incompatible receiver"
    } else {
        "Method Map.prototype called on incompatible receiver"
    }))
}

fn map_mut(rt: &mut Realm, o: ObjRef) -> &mut OrderedMap {
    match &mut rt.heap.get_mut(o).kind {
        Kind::Map(m) | Kind::Set(m) => m,
        _ => unreachable!(),
    }
}

fn map_ref(rt: &Realm, o: ObjRef) -> &OrderedMap {
    match &rt.heap.get(o).kind {
        Kind::Map(m) | Kind::Set(m) => m,
        _ => unreachable!(),
    }
}

/// Fills a new collection from an iterable through its adder method.
fn fill_from(rt: &mut Realm, o: ObjRef, iterable: &Value, adder_name: &str, pairs: bool) -> Result<(), Value> {
    if iterable.is_nullish() {
        return Ok(());
    }
    let adder = rt.get(o, &key(adder_name), Value::Object(o))?;
    require_callable(rt, &adder, adder_name)?;
    let rec = rt.get_iterator(iterable)?;
    while let Some(item) = rt.iter_step(rec)? {
        let r = if pairs {
            match &item {
                Value::Object(io) => {
                    let k = rt.get(*io, &PropKey::index(0), item.clone());
                    let v = rt.get(*io, &PropKey::index(1), item.clone());
                    match (k, v) {
                        (Ok(k), Ok(v)) => rt.call(&adder, Value::Object(o), &[k, v]),
                        (Err(e), _) | (_, Err(e)) => Err(e),
                    }
                }
                _ => Err(rt.type_error("Iterator value is not an entry object")),
            }
        } else {
            rt.call(&adder, Value::Object(o), &[item])
        };
        if let Err(e) = r {
            rt.iter_close(rec, true)?;
            return Err(e);
        }
    }
    Ok(())
}

fn map_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Constructor Map requires 'new'"));
    }
    let mp = rt.intr.map_proto;
    let proto = rt.proto_from_ctor(&c.new_target, mp)?;
    let o = rt.alloc(Obj::new(Some(proto), Kind::Map(Box::default())));
    fill_from(rt, o, &c.arg(0), "set", true)?;
    Ok(Value::Object(o))
}

fn set_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Constructor Set requires 'new'"));
    }
    let sp = rt.intr.set_proto;
    let proto = rt.proto_from_ctor(&c.new_target, sp)?;
    let o = rt.alloc(Obj::new(Some(proto), Kind::Set(Box::default())));
    fill_from(rt, o, &c.arg(0), "add", false)?;
    Ok(Value::Object(o))
}

fn map_get(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_map(rt, c, false)?;
    Ok(map_ref(rt, o).get(&c.arg(0)).cloned().unwrap_or(Value::Undefined))
}

fn map_set(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_map(rt, c, false)?;
    map_mut(rt, o).set(c.arg(0), c.arg(1));
    Ok(c.this.clone())
}

fn has(rt: &mut Realm, c: &Call, set: bool) -> JsResult {
    let o = this_map(rt, c, set)?;
    Ok(Value::Bool(map_ref(rt, o).has(&c.arg(0))))
}

fn map_has(rt: &mut Realm, c: &Call) -> JsResult {
    has(rt, c, false)
}

fn set_has(rt: &mut Realm, c: &Call) -> JsResult {
    has(rt, c, true)
}

fn delete(rt: &mut Realm, c: &Call, set: bool) -> JsResult {
    let o = this_map(rt, c, set)?;
    Ok(Value::Bool(map_mut(rt, o).delete(&c.arg(0))))
}

fn map_delete(rt: &mut Realm, c: &Call) -> JsResult {
    delete(rt, c, false)
}

fn set_delete(rt: &mut Realm, c: &Call) -> JsResult {
    delete(rt, c, true)
}

fn clear(rt: &mut Realm, c: &Call, set: bool) -> JsResult {
    let o = this_map(rt, c, set)?;
    map_mut(rt, o).clear();
    Ok(Value::Undefined)
}

fn map_clear(rt: &mut Realm, c: &Call) -> JsResult {
    clear(rt, c, false)
}

fn set_clear(rt: &mut Realm, c: &Call) -> JsResult {
    clear(rt, c, true)
}

fn size(rt: &mut Realm, c: &Call, set: bool) -> JsResult {
    let o = this_map(rt, c, set)?;
    Ok(Value::Number(map_ref(rt, o).size as f64))
}

fn map_size(rt: &mut Realm, c: &Call) -> JsResult {
    size(rt, c, false)
}

fn set_size(rt: &mut Realm, c: &Call) -> JsResult {
    size(rt, c, true)
}

fn set_add(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_map(rt, c, true)?;
    let v = c.arg(0);
    map_mut(rt, o).set(v.clone(), v);
    Ok(c.this.clone())
}

fn for_each(rt: &mut Realm, c: &Call, set: bool) -> JsResult {
    let o = this_map(rt, c, set)?;
    let f = c.arg(0);
    require_callable(rt, &f, "forEach")?;
    let mut i = 0;
    loop {
        let entry = {
            let m = map_ref(rt, o);
            if i >= m.entries.len() {
                break;
            }
            m.entries[i].clone()
        };
        i += 1;
        if let Some((k, v)) = entry {
            rt.call(&f, c.arg(1), &[v, k, Value::Object(o)])?;
        }
    }
    Ok(Value::Undefined)
}

fn map_for_each(rt: &mut Realm, c: &Call) -> JsResult {
    for_each(rt, c, false)
}

fn set_for_each(rt: &mut Realm, c: &Call) -> JsResult {
    for_each(rt, c, true)
}

fn make_iter(rt: &mut Realm, c: &Call, set: bool, kind: IterKind) -> JsResult {
    let o = this_map(rt, c, set)?;
    let (p, k) = if set {
        (rt.intr.set_iterator_proto, Kind::SetIterator(o, 0, kind))
    } else {
        (rt.intr.map_iterator_proto, Kind::MapIterator(o, 0, kind))
    };
    Ok(Value::Object(rt.alloc(Obj::new(Some(p), k))))
}

fn map_keys(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, false, IterKind::Keys)
}

fn map_values(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, false, IterKind::Values)
}

fn map_entries(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, false, IterKind::Entries)
}

fn set_values(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, true, IterKind::Values)
}

fn set_entries(rt: &mut Realm, c: &Call) -> JsResult {
    make_iter(rt, c, true, IterKind::Entries)
}

fn iter_next(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(it) = c.this else {
        return Err(rt.type_error("not a Map or Set Iterator"));
    };
    let (m, i, kind, done) = match &rt.heap.get(it).kind {
        Kind::MapIterator(m, i, k) | Kind::SetIterator(m, i, k) => (*m, *i as usize, *k, *i == u32::MAX),
        _ => return Err(rt.type_error("not a Map or Set Iterator")),
    };
    if done {
        return Ok(rt.iter_result(Value::Undefined, true));
    }
    let mut j = i;
    let entry = loop {
        let map = map_ref(rt, m);
        if j >= map.entries.len() {
            break None;
        }
        if let Some(e) = &map.entries[j] {
            break Some(e.clone());
        }
        j += 1;
    };
    let next = if entry.is_some() { (j + 1) as u32 } else { u32::MAX };
    if let Kind::MapIterator(_, idx, _) | Kind::SetIterator(_, idx, _) = &mut rt.heap.get_mut(it).kind {
        *idx = next;
    }
    match entry {
        None => Ok(rt.iter_result(Value::Undefined, true)),
        Some((k, v)) => {
            let r = match kind {
                IterKind::Keys => k,
                IterKind::Values => v,
                IterKind::Entries => rt.array_from(alloc::vec![k, v]),
            };
            Ok(rt.iter_result(r, false))
        }
    }
}

fn group_by(rt: &mut Realm, c: &Call) -> JsResult {
    let items = rt.iterate_to_vec(&c.arg(0))?;
    let f = c.arg(1);
    require_callable(rt, &f, "Map.groupBy")?;
    let mp = rt.intr.map_proto;
    let out = rt.alloc(Obj::new(Some(mp), Kind::Map(Box::default())));
    for (i, item) in items.into_iter().enumerate() {
        let k = rt.call(&f, Value::Undefined, &[item.clone(), Value::Number(i as f64)])?;
        let existing = map_ref(rt, out).get(&k).cloned();
        match existing {
            Some(Value::Object(arr)) => {
                let len = rt.length_of(arr)? as u32;
                rt.create_data_property(arr, PropKey::index(len), item)?;
            }
            _ => {
                let arr = rt.array_from(alloc::vec![item]);
                map_mut(rt, out).set(k, arr);
            }
        }
    }
    Ok(Value::Object(out))
}

// ---------------------------------------------------------------- weak collections

fn this_weak(rt: &mut Realm, c: &Call, set: bool) -> Result<ObjRef, Value> {
    if let Value::Object(o) = c.this {
        match (&rt.heap.get(o).kind, set) {
            (Kind::WeakMap(_), false) | (Kind::WeakSet(_), true) => return Ok(o),
            _ => {}
        }
    }
    Err(rt.type_error("WeakMap/WeakSet method called on incompatible receiver"))
}

fn weak_key(rt: &mut Realm, v: &Value) -> Result<ObjRef, Value> {
    match v {
        Value::Object(o) => Ok(*o),
        _ => Err(rt.type_error("Invalid value used as weak map key")),
    }
}

fn weakmap_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Constructor WeakMap requires 'new'"));
    }
    let p = rt.intr.weakmap_proto;
    let proto = rt.proto_from_ctor(&c.new_target, p)?;
    let o = rt.alloc(Obj::new(Some(proto), Kind::WeakMap(Box::default())));
    fill_from(rt, o, &c.arg(0), "set", true)?;
    Ok(Value::Object(o))
}

fn weakset_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        return Err(rt.type_error("Constructor WeakSet requires 'new'"));
    }
    let p = rt.intr.weakset_proto;
    let proto = rt.proto_from_ctor(&c.new_target, p)?;
    let o = rt.alloc(Obj::new(Some(proto), Kind::WeakSet(Box::default())));
    fill_from(rt, o, &c.arg(0), "add", false)?;
    Ok(Value::Object(o))
}

fn wm_get(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, false)?;
    let Value::Object(k) = c.arg(0) else { return Ok(Value::Undefined) };
    Ok(match &rt.heap.get(o).kind {
        Kind::WeakMap(m) => m.get(&k.0).filter(|(r, _)| *r == k).map(|(_, v)| v.clone()).unwrap_or(Value::Undefined),
        _ => Value::Undefined,
    })
}

fn wm_set(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, false)?;
    let k = weak_key(rt, &c.arg(0))?;
    if let Kind::WeakMap(m) = &mut rt.heap.get_mut(o).kind {
        m.insert(k.0, (k, c.arg(1)));
    }
    Ok(c.this.clone())
}

fn wm_has(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, false)?;
    let Value::Object(k) = c.arg(0) else { return Ok(Value::Bool(false)) };
    Ok(Value::Bool(matches!(&rt.heap.get(o).kind, Kind::WeakMap(m) if m.contains_key(&k.0))))
}

fn wm_delete(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, false)?;
    let Value::Object(k) = c.arg(0) else { return Ok(Value::Bool(false)) };
    Ok(Value::Bool(match &mut rt.heap.get_mut(o).kind {
        Kind::WeakMap(m) => m.remove(&k.0).is_some(),
        _ => false,
    }))
}

fn ws_add(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, true)?;
    let k = weak_key(rt, &c.arg(0))?;
    if let Kind::WeakSet(m) = &mut rt.heap.get_mut(o).kind {
        m.insert(k.0, k);
    }
    Ok(c.this.clone())
}

fn ws_has(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, true)?;
    let Value::Object(k) = c.arg(0) else { return Ok(Value::Bool(false)) };
    Ok(Value::Bool(matches!(&rt.heap.get(o).kind, Kind::WeakSet(m) if m.contains_key(&k.0))))
}

fn ws_delete(rt: &mut Realm, c: &Call) -> JsResult {
    let o = this_weak(rt, c, true)?;
    let Value::Object(k) = c.arg(0) else { return Ok(Value::Bool(false)) };
    Ok(Value::Bool(match &mut rt.heap.get_mut(o).kind {
        Kind::WeakSet(m) => m.remove(&k.0).is_some(),
        _ => false,
    }))
}

fn species(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(c.this.clone())
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let ip = rt.intr.iterator_proto;

    let mp = rt.new_object_with(Some(op));
    rt.intr.map_proto = mp;
    let map = constructor(rt, "Map", 0, map_ctor, mp);
    rt.method(map, "groupBy", 2, group_by);
    for (name, len, f) in [
        ("get", 1, map_get as NativeFn),
        ("set", 2, map_set),
        ("has", 1, map_has),
        ("delete", 1, map_delete),
        ("clear", 0, map_clear),
        ("forEach", 1, map_for_each),
        ("keys", 0, map_keys),
        ("values", 0, map_values),
    ] {
        rt.method(mp, name, len, f);
    }
    let me = rt.method(mp, "entries", 0, map_entries);
    rt.define(mp, Sym::ITERATOR, Value::Object(me), HIDDEN);
    rt.getter(mp, "size", map_size);
    to_string_tag(rt, mp, "Map");
    let mip = rt.new_object_with(Some(ip));
    rt.intr.map_iterator_proto = mip;
    rt.method(mip, "next", 0, iter_next);
    to_string_tag(rt, mip, "Map Iterator");

    let sp = rt.new_object_with(Some(op));
    rt.intr.set_proto = sp;
    let set = constructor(rt, "Set", 0, set_ctor, sp);
    for (name, len, f) in [
        ("add", 1, set_add as NativeFn),
        ("has", 1, set_has),
        ("delete", 1, set_delete),
        ("clear", 0, set_clear),
        ("forEach", 1, set_for_each),
        ("entries", 0, set_entries),
    ] {
        rt.method(sp, name, len, f);
    }
    let sv = rt.method(sp, "values", 0, set_values);
    rt.define(sp, "keys", Value::Object(sv), HIDDEN);
    rt.define(sp, Sym::ITERATOR, Value::Object(sv), HIDDEN);
    rt.getter(sp, "size", set_size);
    to_string_tag(rt, sp, "Set");
    let sip = rt.new_object_with(Some(ip));
    rt.intr.set_iterator_proto = sip;
    rt.method(sip, "next", 0, iter_next);
    to_string_tag(rt, sip, "Set Iterator");
    for c in [map, set] {
        let g = rt.native("get [Symbol.species]", 0, species, false);
        rt.define_accessor(c, Sym::SPECIES, Value::Object(g), Value::Undefined, CONFIGURABLE);
    }

    let wmp = rt.new_object_with(Some(op));
    rt.intr.weakmap_proto = wmp;
    constructor(rt, "WeakMap", 0, weakmap_ctor, wmp);
    rt.method(wmp, "get", 1, wm_get);
    rt.method(wmp, "set", 2, wm_set);
    rt.method(wmp, "has", 1, wm_has);
    rt.method(wmp, "delete", 1, wm_delete);
    to_string_tag(rt, wmp, "WeakMap");

    let wsp = rt.new_object_with(Some(op));
    rt.intr.weakset_proto = wsp;
    constructor(rt, "WeakSet", 0, weakset_ctor, wsp);
    rt.method(wsp, "add", 1, ws_add);
    rt.method(wsp, "has", 1, ws_has);
    rt.method(wsp, "delete", 1, ws_delete);
    to_string_tag(rt, wsp, "WeakSet");
}
