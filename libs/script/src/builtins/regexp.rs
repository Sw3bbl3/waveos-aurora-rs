//! RegExp: the constructor, exec/test, and the symbol methods that
//! String.prototype.match, replace, search and split delegate to.

use super::*;
use crate::regexp::{compile, Flags};
use crate::value::JsStr;
use alloc::rc::Rc;
use alloc::string::String;

fn regexp_data(rt: &Realm, v: &Value) -> Option<(Rc<crate::regexp::Program>, JsStr, JsStr)> {
    match v {
        Value::Object(o) => match &rt.heap.get(*o).kind {
            Kind::RegExp(d) => Some((d.program.clone(), d.source.clone(), d.flags.clone())),
            _ => None,
        },
        _ => None,
    }
}

/// Creates a RegExp object (throws SyntaxError for bad patterns).
pub fn create(rt: &mut Realm, pattern: JsStr, flags: JsStr, proto: Option<ObjRef>) -> JsResult {
    let program = compile(pattern.units(), &flags.to_rust()).map_err(|e| {
        let msg = alloc::format!("Invalid regular expression: /{}/{}: {}", pattern.to_rust(), flags.to_rust(), e);
        rt.error(crate::realm::ErrorKind::SyntaxError, &msg)
    })?;
    let proto = proto.unwrap_or(rt.intr.regexp_proto);
    let o = rt.alloc(Obj::new(
        Some(proto),
        Kind::RegExp(Box::new(RegExpData { source: pattern, flags, program: Rc::new(program) })),
    ));
    rt.define(o, "lastIndex", Value::Number(0.0), WRITABLE);
    Ok(Value::Object(o))
}

fn is_regexp(rt: &mut Realm, v: &Value) -> Result<bool, Value> {
    let Value::Object(o) = v else { return Ok(false) };
    let m = rt.get(*o, &PropKey::Sym(Sym::MATCH), v.clone())?;
    if !m.is_undefined() {
        return Ok(m.truthy());
    }
    Ok(matches!(rt.heap.get(*o).kind, Kind::RegExp(_)))
}

fn regexp_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    let pattern = c.arg(0);
    let flags = c.arg(1);
    let pattern_is_re = is_regexp(rt, &pattern)?;
    if !c.is_construct() && pattern_is_re && flags.is_undefined() {
        if let Value::Object(po) = &pattern {
            let ctor = rt.get(*po, &key("constructor"), pattern.clone())?;
            if ctor == Value::Object(c.callee) {
                return Ok(pattern);
            }
        }
    }
    let (p, f) = if let Some((_, src, fl)) = regexp_data(rt, &pattern) {
        (src, if flags.is_undefined() { fl } else { rt.to_string(&flags)? })
    } else if pattern_is_re {
        let Value::Object(po) = &pattern else { unreachable!() };
        let src = rt.get(*po, &key("source"), pattern.clone())?;
        let src = rt.to_string(&src)?;
        let fl = if flags.is_undefined() {
            let f = rt.get(*po, &key("flags"), pattern.clone())?;
            rt.to_string(&f)?
        } else {
            rt.to_string(&flags)?
        };
        (src, fl)
    } else {
        (
            if pattern.is_undefined() { JsStr::empty() } else { rt.to_string(&pattern)? },
            if flags.is_undefined() { JsStr::empty() } else { rt.to_string(&flags)? },
        )
    };
    let nt = if c.is_construct() { c.new_target.clone() } else { Value::Object(c.callee) };
    let rp = rt.intr.regexp_proto;
    let proto = rt.proto_from_ctor(&nt, rp)?;
    create(rt, p, f, Some(proto))
}

fn this_regexp(rt: &mut Realm, c: &Call, method: &str) -> Result<ObjRef, Value> {
    match &c.this {
        Value::Object(o) if matches!(rt.heap.get(*o).kind, Kind::RegExp(_)) => Ok(*o),
        _ => Err(rt.type_error(&alloc::format!("RegExp.prototype.{method} requires that 'this' be a RegExp object"))),
    }
}

fn set_last_index(rt: &mut Realm, o: ObjRef, v: f64) -> Result<(), Value> {
    if !rt.set(o, key("lastIndex"), Value::Number(v), Value::Object(o))? {
        return Err(rt.type_error("Cannot assign to read only property 'lastIndex'"));
    }
    Ok(())
}

/// RegExpBuiltinExec.
pub fn builtin_exec(rt: &mut Realm, r: ObjRef, s: &JsStr) -> JsResult {
    let (program, _, _) = regexp_data(rt, &Value::Object(r)).unwrap();
    let li = rt.get(r, &key("lastIndex"), Value::Object(r))?;
    let mut last_index = rt.to_length(&li)?;
    let Flags { global, sticky, has_indices, .. } = program.flags;
    if !global && !sticky {
        last_index = 0.0;
    }
    if last_index > s.len() as f64 {
        if global || sticky {
            set_last_index(rt, r, 0.0)?;
        }
        return Ok(Value::Null);
    }
    let caps = program.exec(s.units(), last_index as usize);
    let Some(caps) = caps else {
        if global || sticky {
            set_last_index(rt, r, 0.0)?;
        }
        return Ok(Value::Null);
    };
    let (start, end) = caps[0].unwrap();
    if global || sticky {
        set_last_index(rt, r, end as f64)?;
    }
    let items: Vec<Value> =
        caps.iter().map(|g| g.map(|(a, b)| Value::String(s.slice(a, b))).unwrap_or(Value::Undefined)).collect();
    let arr = rt.new_array(items);
    rt.define(arr, "index", Value::Number(start as f64), DEFAULT);
    rt.define(arr, "input", Value::String(s.clone()), DEFAULT);
    let groups = if program.names.is_empty() {
        Value::Undefined
    } else {
        let g = rt.new_object_with(None);
        for (name, idx) in &program.names {
            let v = caps[*idx].map(|(a, b)| Value::String(s.slice(a, b))).unwrap_or(Value::Undefined);
            rt.define(g, name.as_str(), v, DEFAULT);
        }
        Value::Object(g)
    };
    rt.define(arr, "groups", groups, DEFAULT);
    if has_indices {
        let pairs: Vec<Value> = caps
            .iter()
            .map(|g| match g {
                Some((a, b)) => rt.array_from(alloc::vec![Value::Number(*a as f64), Value::Number(*b as f64)]),
                None => Value::Undefined,
            })
            .collect();
        let ind = rt.new_array(pairs);
        rt.define(arr, "indices", Value::Object(ind), DEFAULT);
    }
    Ok(Value::Object(arr))
}

/// RegExpExec: honours an overridden `exec`.
pub fn regexp_exec(rt: &mut Realm, r: ObjRef, s: &JsStr) -> JsResult {
    let exec = rt.get(r, &key("exec"), Value::Object(r))?;
    if rt.is_callable(&exec) && exec != Value::Object(rt.intr.regexp_exec) {
        let res = rt.call(&exec, Value::Object(r), &[Value::String(s.clone())])?;
        if !matches!(res, Value::Object(_) | Value::Null) {
            return Err(rt.type_error("exec result must be an object or null"));
        }
        return Ok(res);
    }
    if !matches!(rt.heap.get(r).kind, Kind::RegExp(_)) {
        return Err(rt.type_error("RegExp exec method called on an incompatible receiver"));
    }
    builtin_exec(rt, r, s)
}

fn exec(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_regexp(rt, c, "exec")?;
    let s = rt.to_string(&c.arg(0))?;
    builtin_exec(rt, r, &s)
}

fn test(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype.test")?;
    let s = rt.to_string(&c.arg(0))?;
    Ok(Value::Bool(!matches!(regexp_exec(rt, r, &s)?, Value::Null)))
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype.toString")?;
    let src = rt.get(r, &key("source"), c.this.clone())?;
    let src = rt.to_rust_string(&src)?;
    let fl = rt.get(r, &key("flags"), c.this.clone())?;
    let fl = rt.to_rust_string(&fl)?;
    Ok(Value::str(&alloc::format!("/{src}/{fl}")))
}

fn compile_fn(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_regexp(rt, c, "compile")?;
    let (p, f) = match regexp_data(rt, &c.arg(0)) {
        Some((_, src, fl)) => (src, fl),
        None => (
            if c.arg(0).is_undefined() { JsStr::empty() } else { rt.to_string(&c.arg(0))? },
            if c.arg(1).is_undefined() { JsStr::empty() } else { rt.to_string(&c.arg(1))? },
        ),
    };
    let fresh = create(rt, p, f, None)?;
    let Value::Object(fo) = fresh else { unreachable!() };
    let data = core::mem::replace(&mut rt.heap.get_mut(fo).kind, Kind::Ordinary);
    rt.heap.get_mut(r).kind = data;
    set_last_index(rt, r, 0.0)?;
    Ok(c.this.clone())
}

fn source(rt: &mut Realm, c: &Call) -> JsResult {
    if c.this == Value::Object(rt.intr.regexp_proto) {
        return Ok(Value::str("(?:)"));
    }
    let Some((_, src, _)) = regexp_data(rt, &c.this) else {
        return Err(rt.type_error("RegExp.prototype.source getter called on non-RegExp"));
    };
    if src.is_empty() {
        return Ok(Value::str("(?:)"));
    }
    let mut out = String::new();
    let mut in_class = false;
    let mut prev_backslash = false;
    for ch in src.to_rust().chars() {
        match ch {
            '/' if !prev_backslash && !in_class => out.push_str("\\/"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
        if !prev_backslash {
            if ch == '[' {
                in_class = true;
            } else if ch == ']' {
                in_class = false;
            }
        }
        prev_backslash = ch == '\\' && !prev_backslash;
    }
    Ok(Value::str(&out))
}

fn flag_getter(rt: &mut Realm, c: &Call, f: fn(&Flags) -> bool, name: &str) -> JsResult {
    if c.this == Value::Object(rt.intr.regexp_proto) {
        return Ok(Value::Undefined);
    }
    match regexp_data(rt, &c.this) {
        Some((p, _, _)) => Ok(Value::Bool(f(&p.flags))),
        None => Err(rt.type_error(&alloc::format!("RegExp.prototype.{name} getter called on non-RegExp"))),
    }
}

macro_rules! flag {
    ($fn:ident, $field:ident, $name:expr) => {
        fn $fn(rt: &mut Realm, c: &Call) -> JsResult {
            flag_getter(rt, c, |f| f.$field, $name)
        }
    };
}
flag!(global, global, "global");
flag!(ignore_case, ignore_case, "ignoreCase");
flag!(multiline, multiline, "multiline");
flag!(dot_all, dot_all, "dotAll");
flag!(unicode, unicode, "unicode");
flag!(sticky, sticky, "sticky");
flag!(has_indices, has_indices, "hasIndices");

fn unicode_sets(rt: &mut Realm, c: &Call) -> JsResult {
    match regexp_data(rt, &c.this) {
        Some((_, _, f)) => Ok(Value::Bool(f.units().contains(&(b'v' as u16)))),
        None if c.this == Value::Object(rt.intr.regexp_proto) => Ok(Value::Undefined),
        None => Err(rt.type_error("RegExp.prototype.unicodeSets getter called on non-RegExp")),
    }
}

fn flags(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype.flags")?;
    let mut out = String::new();
    for (ch, name) in [
        ('d', "hasIndices"),
        ('g', "global"),
        ('i', "ignoreCase"),
        ('m', "multiline"),
        ('s', "dotAll"),
        ('u', "unicode"),
        ('v', "unicodeSets"),
        ('y', "sticky"),
    ] {
        if rt.get(r, &key(name), c.this.clone())?.truthy() {
            out.push(ch);
        }
    }
    Ok(Value::str(&out))
}

fn advance(s: &JsStr, i: f64, unicode: bool) -> f64 {
    if !unicode || i + 1.0 >= s.len() as f64 {
        return i + 1.0;
    }
    i + super::string::code_point_at(s.units(), i as usize).1 as f64
}

fn flags_of(rt: &mut Realm, r: ObjRef) -> Result<JsStr, Value> {
    let f = rt.get(r, &key("flags"), Value::Object(r))?;
    rt.to_string(&f)
}

fn has_flag(f: &JsStr, ch: char) -> bool {
    f.units().contains(&(ch as u16))
}

fn sym_match(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype[Symbol.match]")?;
    let s = rt.to_string(&c.arg(0))?;
    let fl = flags_of(rt, r)?;
    if !has_flag(&fl, 'g') {
        return regexp_exec(rt, r, &s);
    }
    let full_unicode = has_flag(&fl, 'u') || has_flag(&fl, 'v');
    set_last_index(rt, r, 0.0)?;
    let mut matches = Vec::new();
    loop {
        let res = regexp_exec(rt, r, &s)?;
        let Value::Object(ro) = res else { break };
        let m = rt.get(ro, &PropKey::index(0), res.clone())?;
        let ms = rt.to_string(&m)?;
        if ms.is_empty() {
            let li = rt.get(r, &key("lastIndex"), Value::Object(r))?;
            let li = rt.to_length(&li)?;
            set_last_index(rt, r, advance(&s, li, full_unicode))?;
        }
        matches.push(Value::String(ms));
    }
    if matches.is_empty() {
        return Ok(Value::Null);
    }
    Ok(rt.array_from(matches))
}

fn sym_replace(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype[Symbol.replace]")?;
    let s = rt.to_string(&c.arg(0))?;
    let mut replace_value = c.arg(1);
    let functional = rt.is_callable(&replace_value);
    if !functional {
        replace_value = Value::String(rt.to_string(&replace_value)?);
    }
    let fl = flags_of(rt, r)?;
    let global = has_flag(&fl, 'g');
    let full_unicode = has_flag(&fl, 'u') || has_flag(&fl, 'v');
    if global {
        set_last_index(rt, r, 0.0)?;
    }
    let mut results = Vec::new();
    loop {
        let res = regexp_exec(rt, r, &s)?;
        let Value::Object(ro) = res else { break };
        results.push(ro);
        if !global {
            break;
        }
        let m = rt.get(ro, &PropKey::index(0), res.clone())?;
        if rt.to_string(&m)?.is_empty() {
            let li = rt.get(r, &key("lastIndex"), Value::Object(r))?;
            let li = rt.to_length(&li)?;
            set_last_index(rt, r, advance(&s, li, full_unicode))?;
        }
    }
    let mut out: Vec<u16> = Vec::new();
    let mut next = 0usize;
    for ro in results {
        let n_caps = (rt.length_of(ro)? - 1.0).max(0.0) as u32;
        let m = rt.get(ro, &PropKey::index(0), Value::Object(ro))?;
        let matched = rt.to_string(&m)?;
        let pos = rt.get(ro, &key("index"), Value::Object(ro))?;
        let pos = rt.to_integer(&pos)?.clamp(0.0, s.len() as f64) as usize;
        let mut captures = Vec::new();
        for i in 1..=n_caps {
            let cap = rt.get(ro, &PropKey::index(i), Value::Object(ro))?;
            captures.push(if cap.is_undefined() { cap } else { Value::String(rt.to_string(&cap)?) });
        }
        let named = rt.get(ro, &key("groups"), Value::Object(ro))?;
        let replacement = if functional {
            let mut args = alloc::vec![Value::String(matched.clone())];
            args.extend(captures.iter().cloned());
            args.push(Value::Number(pos as f64));
            args.push(Value::String(s.clone()));
            if !named.is_undefined() {
                args.push(named.clone());
            }
            let r = rt.call(&replace_value, Value::Undefined, &args)?;
            rt.to_string(&r)?
        } else {
            let named = if named.is_undefined() { named } else { Value::Object(rt.to_object(&named)?) };
            let Value::String(rv) = &replace_value else { unreachable!() };
            super::string::get_substitution(rt, &matched, &s, pos, &captures, &named, rv)?
        };
        if pos >= next {
            out.extend_from_slice(&s.units()[next..pos]);
            out.extend_from_slice(replacement.units());
            next = pos + matched.len();
        }
    }
    if next < s.len() {
        out.extend_from_slice(&s.units()[next..]);
    }
    Ok(Value::String(JsStr::from_units(out)))
}

fn sym_search(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype[Symbol.search]")?;
    let s = rt.to_string(&c.arg(0))?;
    let prev = rt.get(r, &key("lastIndex"), Value::Object(r))?;
    if !rt.same_value(&prev, &Value::Number(0.0)) {
        set_last_index(rt, r, 0.0)?;
    }
    let res = regexp_exec(rt, r, &s)?;
    let cur = rt.get(r, &key("lastIndex"), Value::Object(r))?;
    if !rt.same_value(&cur, &prev) {
        rt.set(r, key("lastIndex"), prev, Value::Object(r))?;
    }
    match res {
        Value::Object(ro) => rt.get(ro, &key("index"), res.clone()),
        _ => Ok(Value::Number(-1.0)),
    }
}

fn species_constructor(rt: &mut Realm, o: ObjRef, default: Value) -> JsResult {
    let c = rt.get(o, &key("constructor"), Value::Object(o))?;
    if c.is_undefined() {
        return Ok(default);
    }
    let Value::Object(co) = c else {
        return Err(rt.type_error("constructor is not an object"));
    };
    let s = rt.get(co, &PropKey::Sym(Sym::SPECIES), c.clone())?;
    if s.is_nullish() {
        return Ok(default);
    }
    if !rt.is_constructor(&s) {
        return Err(rt.type_error("species is not a constructor"));
    }
    Ok(s)
}

fn sym_split(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype[Symbol.split]")?;
    let s = rt.to_string(&c.arg(0))?;
    let default = Value::Object(rt.intr.regexp_ctor);
    let ctor = species_constructor(rt, r, default)?;
    let fl = flags_of(rt, r)?;
    let unicode = has_flag(&fl, 'u') || has_flag(&fl, 'v');
    let new_flags = if has_flag(&fl, 'y') { fl.clone() } else { fl.concat(&JsStr::from("y")) };
    let splitter = rt.construct(&ctor, &[Value::Object(r), Value::String(new_flags)], None)?;
    let Value::Object(sp) = splitter else { unreachable!() };
    let limit = if c.arg(1).is_undefined() { u32::MAX } else { rt.to_uint32(&c.arg(1))? };
    let mut parts: Vec<Value> = Vec::new();
    if limit == 0 {
        return Ok(rt.array_from(parts));
    }
    let size = s.len();
    if size == 0 {
        let z = regexp_exec(rt, sp, &s)?;
        if z.is_nullish() {
            parts.push(Value::String(s));
        }
        return Ok(rt.array_from(parts));
    }
    let mut p = 0usize;
    let mut q = 0usize;
    while q < size {
        set_last_index(rt, sp, q as f64)?;
        let z = regexp_exec(rt, sp, &s)?;
        let Value::Object(zo) = z else {
            q = advance(&s, q as f64, unicode) as usize;
            continue;
        };
        let li = rt.get(sp, &key("lastIndex"), Value::Object(sp))?;
        let e = (rt.to_length(&li)? as usize).min(size);
        if e == p {
            q = advance(&s, q as f64, unicode) as usize;
            continue;
        }
        parts.push(Value::String(s.slice(p, q)));
        if parts.len() as u32 == limit {
            return Ok(rt.array_from(parts));
        }
        p = e;
        let n_caps = (rt.length_of(zo)? - 1.0).max(0.0) as u32;
        for i in 1..=n_caps {
            let cap = rt.get(zo, &PropKey::index(i), z.clone())?;
            parts.push(cap);
            if parts.len() as u32 == limit {
                return Ok(rt.array_from(parts));
            }
        }
        q = p;
    }
    parts.push(Value::String(s.slice(p, size)));
    Ok(rt.array_from(parts))
}

fn sym_match_all(rt: &mut Realm, c: &Call) -> JsResult {
    let r = this_obj(rt, c, "RegExp.prototype[Symbol.matchAll]")?;
    let s = rt.to_string(&c.arg(0))?;
    let default = Value::Object(rt.intr.regexp_ctor);
    let ctor = species_constructor(rt, r, default)?;
    let fl = flags_of(rt, r)?;
    let matcher = rt.construct(&ctor, &[Value::Object(r), Value::String(fl.clone())], None)?;
    let Value::Object(m) = matcher else { unreachable!() };
    let li = rt.get(r, &key("lastIndex"), Value::Object(r))?;
    let li = rt.to_length(&li)?;
    set_last_index(rt, m, li)?;
    let p = rt.intr.regexp_string_iterator_proto;
    let it = rt.alloc(Obj::new(
        Some(p),
        Kind::RegExpStringIterator(Box::new((
            m,
            s,
            has_flag(&fl, 'g'),
            has_flag(&fl, 'u') || has_flag(&fl, 'v'),
            false,
        ))),
    ));
    Ok(Value::Object(it))
}

fn match_all_next(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(it) = c.this else {
        return Err(rt.type_error("not a RegExp String Iterator"));
    };
    let (r, s, global, unicode, done) = match &rt.heap.get(it).kind {
        Kind::RegExpStringIterator(b) => (b.0, b.1.clone(), b.2, b.3, b.4),
        _ => return Err(rt.type_error("not a RegExp String Iterator")),
    };
    if done {
        return Ok(rt.iter_result(Value::Undefined, true));
    }
    let set_done = |rt: &mut Realm| {
        if let Kind::RegExpStringIterator(b) = &mut rt.heap.get_mut(it).kind {
            b.4 = true;
        }
    };
    let res = regexp_exec(rt, r, &s)?;
    let Value::Object(ro) = res else {
        set_done(rt);
        return Ok(rt.iter_result(Value::Undefined, true));
    };
    if !global {
        set_done(rt);
        return Ok(rt.iter_result(res, false));
    }
    let m = rt.get(ro, &PropKey::index(0), res.clone())?;
    if rt.to_string(&m)?.is_empty() {
        let li = rt.get(r, &key("lastIndex"), Value::Object(r))?;
        let li = rt.to_length(&li)?;
        set_last_index(rt, r, advance(&s, li, unicode))?;
    }
    Ok(rt.iter_result(res, false))
}

fn species(_rt: &mut Realm, c: &Call) -> JsResult {
    Ok(c.this.clone())
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let proto = rt.new_object_with(Some(op));
    rt.intr.regexp_proto = proto;
    let ctor = constructor(rt, "RegExp", 2, regexp_ctor, proto);
    rt.intr.regexp_ctor = ctor;
    let g = rt.native("get [Symbol.species]", 0, species, false);
    rt.define_accessor(ctor, Sym::SPECIES, Value::Object(g), Value::Undefined, CONFIGURABLE);
    let ex = rt.method(proto, "exec", 1, exec);
    rt.intr.regexp_exec = ex;
    rt.method(proto, "test", 1, test);
    rt.method(proto, "toString", 0, to_string);
    rt.method(proto, "compile", 2, compile_fn);
    for (name, f) in [
        ("source", source as NativeFn),
        ("flags", flags),
        ("global", global),
        ("ignoreCase", ignore_case),
        ("multiline", multiline),
        ("dotAll", dot_all),
        ("unicode", unicode),
        ("unicodeSets", unicode_sets),
        ("sticky", sticky),
        ("hasIndices", has_indices),
    ] {
        rt.getter(proto, name, f);
    }
    rt.method_sym(proto, Sym::MATCH, "[Symbol.match]", 1, sym_match);
    rt.method_sym(proto, Sym::MATCH_ALL, "[Symbol.matchAll]", 1, sym_match_all);
    rt.method_sym(proto, Sym::REPLACE, "[Symbol.replace]", 2, sym_replace);
    rt.method_sym(proto, Sym::SEARCH, "[Symbol.search]", 1, sym_search);
    rt.method_sym(proto, Sym::SPLIT, "[Symbol.split]", 2, sym_split);

    let ip = rt.intr.iterator_proto;
    let rsi = rt.new_object_with(Some(ip));
    rt.intr.regexp_string_iterator_proto = rsi;
    rt.method(rsi, "next", 0, match_all_next);
    to_string_tag(rt, rsi, "RegExp String Iterator");
}
