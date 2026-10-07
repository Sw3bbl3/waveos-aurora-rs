//! console, and `inspect`: the Node-style value printer the REPL also uses.

use super::*;
use crate::numconv;
use alloc::format;
use alloc::string::String;

/// Formats a value for display, like Node's util.inspect.
pub fn inspect(rt: &mut Realm, v: &Value) -> String {
    let mut seen = Vec::new();
    inspect_inner(rt, v, 0, &mut seen, true)
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if crate::lexer::is_id_start(c) => chars.all(crate::lexer::is_id_part),
        _ => false,
    }
}

fn quote_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(q);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\\' => out.push_str("\\\\"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

fn key_text(rt: &Realm, k: &PropKey) -> String {
    match k {
        PropKey::Str(s) => {
            let s = s.to_rust();
            if is_ident(&s) {
                s
            } else {
                quote_str(&s)
            }
        }
        PropKey::Sym(s) => format!("[{}]", symbol::descriptive_string(rt, *s)),
    }
}

fn function_text(rt: &Realm, o: ObjRef) -> String {
    let name = match rt.get_own_property(o, &key("name")) {
        Some(Prop { slot: Slot::Data(Value::String(s)), .. }) => s.to_rust(),
        _ => String::new(),
    };
    let is_class = matches!(&rt.heap.get(o).kind, Kind::Function(c) if matches!(c.code.kind, crate::bytecode::CodeKind::ClassConstructor | crate::bytecode::CodeKind::DerivedConstructor));
    if is_class {
        return if name.is_empty() { String::from("[class (anonymous)]") } else { format!("[class {name}]") };
    }
    if name.is_empty() {
        String::from("[Function (anonymous)]")
    } else {
        format!("[Function: {name}]")
    }
}

fn constructor_name(rt: &mut Realm, o: ObjRef) -> Option<String> {
    let proto = rt.proto_of(o)?;
    match rt.get_own_property(proto, &key("constructor")) {
        Some(Prop { slot: Slot::Data(Value::Object(c)), .. }) => match rt.get_own_property(c, &key("name")) {
            Some(Prop { slot: Slot::Data(Value::String(s)), .. }) => Some(s.to_rust()),
            _ => None,
        },
        _ => None,
    }
}

fn join(items: Vec<String>, open: &str, close: &str, indent: usize) -> String {
    if items.is_empty() {
        return format!("{open}{close}");
    }
    let total: usize = items.iter().map(|s| s.len() + 2).sum();
    if total + indent < 72 && !items.iter().any(|s| s.contains('\n')) {
        format!("{open} {} {close}", items.join(", "))
    } else {
        let pad = " ".repeat(indent + 2);
        let body: Vec<String> = items.iter().map(|s| format!("{pad}{s}")).collect();
        format!("{open}\n{}\n{}{close}", body.join(",\n"), " ".repeat(indent))
    }
}

fn inspect_inner(rt: &mut Realm, v: &Value, depth: usize, seen: &mut Vec<ObjRef>, top: bool) -> String {
    match v {
        Value::Undefined | Value::Empty => String::from("undefined"),
        Value::Null => String::from("null"),
        Value::Bool(b) => format!("{b}"),
        Value::Number(n) => {
            if *n == 0.0 && n.is_sign_negative() {
                String::from("-0")
            } else {
                numconv::to_string(*n)
            }
        }
        Value::String(s) => {
            if top {
                s.to_rust()
            } else {
                quote_str(&s.to_rust())
            }
        }
        Value::Symbol(s) => symbol::descriptive_string(rt, *s).to_rust(),
        Value::Object(o) => inspect_object(rt, *o, depth, seen),
    }
}

fn own_entries(rt: &mut Realm, o: ObjRef, depth: usize, seen: &mut Vec<ObjRef>, skip_indices: bool) -> Vec<String> {
    let mut out = Vec::new();
    for k in rt.own_keys(o) {
        if skip_indices && k.as_index().is_some() {
            continue;
        }
        if k.is_str("length") && matches!(rt.heap.get(o).kind, Kind::Array(_)) {
            continue;
        }
        let Some(p) = rt.get_own_property(o, &k) else { continue };
        if !p.enumerable() {
            continue;
        }
        let val = match &p.slot {
            Slot::Data(x) => inspect_inner(rt, x, depth + 1, seen, false),
            Slot::Accessor(g, s) => String::from(match (g.is_undefined(), s.is_undefined()) {
                (false, false) => "[Getter/Setter]",
                (false, true) => "[Getter]",
                _ => "[Setter]",
            }),
        };
        out.push(format!("{}: {}", key_text(rt, &k), val));
    }
    out
}

fn inspect_object(rt: &mut Realm, o: ObjRef, depth: usize, seen: &mut Vec<ObjRef>) -> String {
    if seen.contains(&o) {
        return String::from("[Circular]");
    }
    let indent = depth * 2;
    // Leaf kinds first.
    match &rt.heap.get(o).kind {
        Kind::Function(_) | Kind::Native(_) | Kind::Bound(_) => return function_text(rt, o),
        Kind::Error => {
            if let Ok(Value::String(s)) = rt.get(o, &key("stack"), Value::Object(o)) {
                return s.to_rust();
            }
        }
        Kind::Date(t) => {
            let t = *t;
            return if t.is_nan() {
                String::from("Invalid Date")
            } else {
                let c = Call { this: Value::Object(o), args: Vec::new(), new_target: Value::Undefined, callee: o };
                match rt.get(o, &key("toISOString"), Value::Object(o)).and_then(|f| rt.call(&f, c.this.clone(), &[])) {
                    Ok(Value::String(s)) => s.to_rust(),
                    _ => String::from("Date"),
                }
            };
        }
        Kind::RegExp(r) => return format!("/{}/{}", r.source, r.flags),
        Kind::Primitive(p) => {
            let p = p.clone();
            let (name, text) = match &p {
                Value::String(s) => ("String", quote_str(&s.to_rust())),
                Value::Number(_) => ("Number", inspect_inner(rt, &p, depth, seen, false)),
                Value::Bool(b) => ("Boolean", format!("{b}")),
                _ => ("Symbol", inspect_inner(rt, &p, depth, seen, false)),
            };
            return format!("[{name}: {text}]");
        }
        Kind::Promise(p) => {
            let (state, value) = (p.state, p.value.clone());
            let inner = match state {
                PromiseState::Pending => String::from("<pending>"),
                PromiseState::Fulfilled => inspect_inner(rt, &value, depth + 1, seen, false),
                PromiseState::Rejected => format!("<rejected> {}", inspect_inner(rt, &value, depth + 1, seen, false)),
            };
            return format!("Promise {{ {inner} }}");
        }
        Kind::Generator(_) => return String::from("Object [Generator] {}"),
        _ => {}
    }
    if depth > 2 {
        return String::from(if matches!(rt.heap.get(o).kind, Kind::Array(_)) { "[Array]" } else { "[Object]" });
    }
    seen.push(o);
    let r = match &rt.heap.get(o).kind {
        Kind::Array(_) => {
            let len = rt.length_of(o).unwrap_or(0.0) as u32;
            let mut items = Vec::new();
            let mut holes = 0;
            for i in 0..len.min(100) {
                match rt.get_own_property(o, &PropKey::index(i)) {
                    Some(Prop { slot: Slot::Data(x), .. }) => {
                        if holes > 0 {
                            items.push(format!("<{holes} empty item{}>", if holes > 1 { "s" } else { "" }));
                            holes = 0;
                        }
                        items.push(inspect_inner(rt, &x, depth + 1, seen, false));
                    }
                    Some(_) => items.push(String::from("[Getter]")),
                    None => holes += 1,
                }
            }
            if holes > 0 {
                items.push(format!("<{holes} empty item{}>", if holes > 1 { "s" } else { "" }));
            }
            if len > 100 {
                items.push(format!("... {} more items", len - 100));
            }
            items.extend(own_entries(rt, o, depth, seen, true));
            join(items, "[", "]", indent)
        }
        Kind::Map(m) => {
            let entries: Vec<(Value, Value)> = m.entries.iter().flatten().cloned().collect();
            let size = entries.len();
            let items: Vec<String> = entries
                .iter()
                .map(|(k, v)| {
                    let ks = inspect_inner(rt, k, depth + 1, seen, false);
                    let vs = inspect_inner(rt, v, depth + 1, seen, false);
                    format!("{ks} => {vs}")
                })
                .collect();
            format!("Map({size}) {}", join(items, "{", "}", indent))
        }
        Kind::Set(m) => {
            let entries: Vec<Value> = m.entries.iter().flatten().map(|(k, _)| k.clone()).collect();
            let size = entries.len();
            let items: Vec<String> = entries.iter().map(|k| inspect_inner(rt, k, depth + 1, seen, false)).collect();
            format!("Set({size}) {}", join(items, "{", "}", indent))
        }
        Kind::WeakMap(_) => String::from("WeakMap { <items unknown> }"),
        Kind::WeakSet(_) => String::from("WeakSet { <items unknown> }"),
        _ => {
            let items = own_entries(rt, o, depth, seen, false);
            let body = join(items, "{", "}", indent);
            let tag = match rt.get(o, &PropKey::Sym(Sym::TO_STRING_TAG), Value::Object(o)) {
                Ok(Value::String(s)) => Some(s.to_rust()),
                _ => None,
            };
            match (rt.proto_of(o), constructor_name(rt, o), tag) {
                (None, _, _) => format!("[Object: null prototype] {body}"),
                (_, Some(n), Some(t)) if n != t => format!("{n} [{t}] {body}"),
                (_, None, Some(t)) => format!("Object [{t}] {body}"),
                (_, Some(n), _) if n != "Object" => format!("{n} {body}"),
                _ => body,
            }
        }
    };
    seen.pop();
    r
}

/// Formats console arguments, honouring %s %d %i %f %o %O %j %c.
pub fn format_args(rt: &mut Realm, args: &[Value]) -> String {
    let mut out = String::new();
    let mut rest = args;
    if let Some(Value::String(f)) = args.first() {
        let f = f.to_rust();
        if f.contains('%') {
            let mut it = args[1..].iter();
            let mut chars = f.chars().peekable();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                match chars.peek().copied() {
                    Some('%') => {
                        chars.next();
                        out.push('%');
                    }
                    Some(spec @ ('s' | 'd' | 'i' | 'f' | 'o' | 'O' | 'j' | 'c')) => {
                        chars.next();
                        let Some(a) = it.next() else {
                            out.push('%');
                            out.push(spec);
                            continue;
                        };
                        match spec {
                            's' => out.push_str(&inspect_inner(rt, a, 1, &mut Vec::new(), true)),
                            'd' | 'i' => {
                                let n = rt.to_number(a).unwrap_or(f64::NAN);
                                let n = if spec == 'i' { numconv::to_integer(n) } else { n };
                                out.push_str(&numconv::to_string(n));
                            }
                            'f' => out.push_str(&numconv::to_string(rt.to_number(a).unwrap_or(f64::NAN))),
                            'j' => {
                                let j = json::stringify_value(rt, a.clone(), Value::Undefined, Value::Undefined);
                                out.push_str(&match j {
                                    Ok(Value::String(s)) => s.to_rust(),
                                    _ => String::from("undefined"),
                                });
                            }
                            'c' => {}
                            _ => out.push_str(&inspect_inner(rt, a, 0, &mut Vec::new(), false)),
                        }
                    }
                    _ => out.push('%'),
                }
            }
            rest = it.as_slice();
            for a in rest {
                out.push(' ');
                out.push_str(&inspect_inner(rt, a, 0, &mut Vec::new(), true));
            }
            return out;
        }
    }
    for (i, a) in rest.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&inspect_inner(rt, a, 0, &mut Vec::new(), true));
    }
    out
}

fn emit(rt: &mut Realm, level: &str, args: &[Value]) {
    let mut line = format_args(rt, args);
    let indent = rt.console_indent;
    if indent > 0 {
        let pad = " ".repeat(indent * 2);
        line = line.lines().map(|l| format!("{pad}{l}")).collect::<Vec<_>>().join("\n");
    }
    rt.host.console(level, &line);
}

macro_rules! level {
    ($name:ident, $level:expr) => {
        fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            emit(rt, $level, &c.args);
            Ok(Value::Undefined)
        }
    };
}
level!(log, "log");
level!(info, "info");
level!(warn, "warn");
level!(error, "error");
level!(debug, "debug");

fn trace(rt: &mut Realm, c: &Call) -> JsResult {
    let mut args = alloc::vec![Value::str("Trace:")];
    args.extend(c.args.iter().cloned());
    emit(rt, "error", &args);
    Ok(Value::Undefined)
}

fn assert(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.arg(0).truthy() {
        let mut args = alloc::vec![Value::str("Assertion failed")];
        if c.args.len() > 1 {
            args[0] = Value::str("Assertion failed:");
            args.extend(c.args.iter().skip(1).cloned());
        }
        emit(rt, "error", &args);
    }
    Ok(Value::Undefined)
}

fn group(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.args.is_empty() {
        emit(rt, "log", &c.args);
    }
    rt.console_indent += 1;
    Ok(Value::Undefined)
}

fn group_end(rt: &mut Realm, _c: &Call) -> JsResult {
    rt.console_indent = rt.console_indent.saturating_sub(1);
    Ok(Value::Undefined)
}

fn label_of(rt: &mut Realm, c: &Call) -> String {
    if c.arg(0).is_undefined() {
        String::from("default")
    } else {
        rt.to_rust_string(&c.arg(0)).unwrap_or_default()
    }
}

fn time(rt: &mut Realm, c: &Call) -> JsResult {
    let l = label_of(rt, c);
    let now = rt.host.now_ms();
    rt.console_timers.insert(l, now);
    Ok(Value::Undefined)
}

fn time_end(rt: &mut Realm, c: &Call) -> JsResult {
    let l = label_of(rt, c);
    if let Some(start) = rt.console_timers.remove(&l) {
        let ms = rt.host.now_ms() - start;
        rt.host.console("log", &format!("{l}: {}ms", numconv::to_fixed(ms, 3)));
    }
    Ok(Value::Undefined)
}

fn count(rt: &mut Realm, c: &Call) -> JsResult {
    let l = label_of(rt, c);
    let n = rt.console_counts.entry(l.clone()).or_insert(0);
    *n += 1;
    let n = *n;
    rt.host.console("log", &format!("{l}: {n}"));
    Ok(Value::Undefined)
}

pub fn init(rt: &mut Realm) {
    let con = rt.new_object();
    for (name, f) in [
        ("log", log as NativeFn),
        ("info", info),
        ("warn", warn),
        ("error", error),
        ("debug", debug),
        ("trace", trace),
        ("assert", assert),
        ("dir", log),
        ("table", log),
        ("group", group),
        ("groupCollapsed", group),
        ("groupEnd", group_end),
        ("time", time),
        ("timeEnd", time_end),
        ("timeLog", time_end),
        ("count", count),
    ] {
        rt.method(con, name, 0, f);
    }
    rt.set_global("console", Value::Object(con));
}
