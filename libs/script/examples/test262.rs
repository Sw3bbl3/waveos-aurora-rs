//! Runs the official ECMAScript conformance suite (test262) against Pulsar.
//!
//! `cargo run --release -p nebula-script --example test262 -- <test262 dir> [filter] [--failures file] [--summary file]`
//!
//! Tests that need features Pulsar doesn't have yet (listed in UNSUPPORTED)
//! are skipped and reported separately; everything else counts.

use nebula_script::{Call, Host, JsResult, Realm, Value};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const UNSUPPORTED: &[&str] = &[
    "Proxy",
    "BigInt",
    "SharedArrayBuffer",
    "Atomics",
    "ArrayBuffer",
    "DataView",
    "TypedArray",
    "Float16Array",
    "Uint8Array",
    "resizable-arraybuffer",
    "arraybuffer-transfer",
    "Intl",
    "Temporal",
    "WeakRef",
    "FinalizationRegistry",
    "ShadowRealm",
    "tail-call-optimization",
    "decorators",
    "import-defer",
    "source-phase-imports",
    "dynamic-import",
    "import.meta",
    "json-modules",
    "import-attributes",
    "explicit-resource-management",
    "iterator-helpers",
    "iterator-sequencing",
    "set-methods",
    "Array.fromAsync",
    "regexp-modifiers",
    "regexp-duplicate-named-groups",
    "regexp-v-flag",
    "regexp-unicode-property-escapes",
    "RegExp.escape",
    "promise-try",
    "Math.sumPrecise",
    "Error.isError",
    "uint8array-base64",
    "upsert",
    "canonical-tz",
    "cross-realm",
    "IsHTMLDDA",
    "host-gc-required",
    "Atomics.pause",
    "json-parse-with-source",
    "symbols-as-weakmap-keys",
    "change-array-by-copy",
    "Symbol.species",
    "await-dictionary",
    "nonextensible-applies-to-private",
    "legacy-regexp",
];

/// Directories that are entirely out of scope.
const SKIP_DIRS: &[&str] =
    &["intl402", "staging", "/module-code/", "/import/", "/export/", "annexB/built-ins/RegExp/legacy-accessors"];

#[derive(Default)]
struct Meta {
    includes: Vec<String>,
    flags: Vec<String>,
    features: Vec<String>,
    negative: Option<(String, String)>,
}

fn parse_meta(src: &str) -> Meta {
    let mut m = Meta::default();
    let Some(start) = src.find("/*---") else { return m };
    let Some(end) = src[start..].find("---*/") else { return m };
    let yaml = &src[start + 5..start + end];
    let mut section = "";
    let mut neg_phase = String::new();
    let mut neg_type = String::new();
    for line in yaml.lines() {
        let t = line.trim();
        let list_inline = |v: &str| -> Vec<String> {
            v.trim()
                .trim_start_matches('[')
                .trim_end_matches(']')
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        };
        if let Some(v) = t.strip_prefix("includes:") {
            section = "includes";
            m.includes.extend(list_inline(v));
        } else if let Some(v) = t.strip_prefix("flags:") {
            section = "flags";
            m.flags.extend(list_inline(v));
        } else if let Some(v) = t.strip_prefix("features:") {
            section = "features";
            m.features.extend(list_inline(v));
        } else if t.starts_with("negative:") {
            section = "negative";
        } else if let Some(v) = t.strip_prefix("phase:") {
            neg_phase = v.trim().to_string();
        } else if let Some(v) = t.strip_prefix("type:") {
            neg_type = v.trim().to_string();
        } else if let Some(v) = t.strip_prefix("- ") {
            match section {
                "includes" => m.includes.push(v.trim().to_string()),
                "flags" => m.flags.push(v.trim().to_string()),
                "features" => m.features.push(v.trim().to_string()),
                _ => {}
            }
        } else if !line.starts_with(' ') && t.contains(':') {
            section = "";
        }
    }
    if !neg_type.is_empty() {
        m.negative = Some((neg_phase, neg_type));
    }
    m
}

struct Out(Rc<RefCell<Vec<String>>>);

impl Host for Out {
    fn now_ms(&mut self) -> f64 {
        1_700_000_000_000.0
    }

    fn console(&mut self, _level: &str, line: &str) {
        self.0.borrow_mut().push(line.to_string());
    }
}

fn print(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_rust_string(&c.arg(0))?;
    rt.host.console("log", &s);
    Ok(Value::Undefined)
}

fn eval_script(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_rust_string(&c.arg(0))?;
    rt.eval(&s, "evalScript")
}

fn gc(rt: &mut Realm, _c: &Call) -> JsResult {
    rt.collect();
    Ok(Value::Undefined)
}

fn install_262(rt: &mut Realm) {
    let print_fn = rt.native("print", 1, print, false);
    rt.set_global("print", Value::Object(print_fn));
    let o = rt.new_object();
    let es = rt.native("evalScript", 1, eval_script, false);
    rt.define(o, "evalScript", Value::Object(es), nebula_script::HIDDEN);
    let g = rt.native("gc", 0, gc, false);
    rt.define(o, "gc", Value::Object(g), nebula_script::HIDDEN);
    let global = Value::Object(rt.global);
    rt.define(o, "global", global, nebula_script::HIDDEN);
    rt.set_global("$262", Value::Object(o));
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Outcome {
    Pass,
    Fail,
    Skip,
}

fn error_name(rt: &mut Realm, e: &Value) -> String {
    if let Value::Object(o) = e {
        if let Ok(Value::Object(c)) = rt.get_str(*o, "constructor") {
            if let Ok(Value::String(n)) = rt.get_str(c, "name") {
                return n.to_rust();
            }
        }
    }
    String::from("(not an error object)")
}

fn run_one(harness: &BTreeMap<String, String>, src: &str, meta: &Meta, strict: bool) -> Result<(), String> {
    let out = Rc::new(RefCell::new(Vec::new()));
    let mut rt = Realm::new(Box::new(Out(out.clone())));
    rt.step_limit = 5_000_000;
    install_262(&mut rt);
    let raw = meta.flags.iter().any(|f| f == "raw");
    if !raw {
        let mut names = vec!["assert.js".to_string(), "sta.js".to_string()];
        if meta.flags.iter().any(|f| f == "async") {
            names.push("doneprintHandle.js".to_string());
        }
        names.extend(meta.includes.iter().cloned());
        for n in names {
            let Some(code) = harness.get(&n) else { return Err(format!("missing harness file {n}")) };
            if let Err(e) = rt.eval(code, &n) {
                let msg = rt.describe_error(&e);
                return Err(format!("harness {n} failed: {msg}"));
            }
        }
    }
    let source = if strict { format!("\"use strict\";\n{src}") } else { src.to_string() };
    let compiled = rt.compile(&source, "test.js");
    let result = match compiled {
        Err(e) => Err((e, true)),
        Ok(code) => rt.run_code(code).map_err(|e| (e, false)),
    };
    rt.run_jobs();
    match (&meta.negative, result) {
        (Some((phase, ty)), Err((e, at_parse))) => {
            let name = error_name(&mut rt, &e);
            let parse_phase = phase == "parse" || phase == "early" || phase == "resolution";
            if name != *ty {
                let msg = rt.describe_error(&e);
                return Err(format!("expected {ty} ({phase}), got {msg}"));
            }
            if parse_phase != at_parse {
                return Err(format!("expected {ty} at phase {phase}, got it at the other phase"));
            }
            Ok(())
        }
        (Some((phase, ty)), Ok(_)) => Err(format!("expected {ty} ({phase}), but no error")),
        (None, Err((e, _))) => {
            let msg = rt.describe_error(&e);
            Err(msg)
        }
        (None, Ok(_)) => {
            if meta.flags.iter().any(|f| f == "async") {
                let lines = out.borrow();
                if lines.iter().any(|l| l == "Test262:AsyncTestComplete") {
                    Ok(())
                } else {
                    Err(lines
                        .iter()
                        .find(|l| l.starts_with("Test262:AsyncTestFailure"))
                        .cloned()
                        .unwrap_or_else(|| "async test did not complete".to_string()))
                }
            } else {
                Ok(())
            }
        }
    }
}

fn run_test(harness: &BTreeMap<String, String>, path: &Path) -> (Outcome, String) {
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => return (Outcome::Fail, e.to_string()),
    };
    let meta = parse_meta(&src);
    if meta.flags.iter().any(|f| f == "module") {
        return (Outcome::Skip, "module".into());
    }
    if let Some(f) = meta.features.iter().find(|f| UNSUPPORTED.contains(&f.as_str())) {
        return (Outcome::Skip, f.clone());
    }
    let only_strict = meta.flags.iter().any(|f| f == "onlyStrict");
    let no_strict = meta.flags.iter().any(|f| f == "noStrict" || f == "raw");
    let mut modes = Vec::new();
    if !only_strict {
        modes.push(false);
    }
    if !no_strict {
        modes.push(true);
    }
    for strict in modes {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_one(harness, &src, &meta, strict)));
        match r {
            Ok(Ok(())) => {}
            Ok(Err(msg)) => return (Outcome::Fail, format!("[{}] {}", if strict { "strict" } else { "sloppy" }, msg)),
            Err(p) => {
                let msg = p
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                return (Outcome::Fail, format!("PANIC: {msg}"));
            }
        }
    }
    (Outcome::Pass, String::new())
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect(&p, out);
        } else if p.extension().is_some_and(|x| x == "js") && !p.to_string_lossy().contains("_FIXTURE") {
            out.push(p);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("target/test262"));
    let mut filter: Option<String> = None;
    let mut failures_file: Option<String> = None;
    let mut summary_file: Option<String> = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--failures" => {
                failures_file = args.get(i + 1).cloned();
                i += 1;
            }
            "--summary" => {
                summary_file = args.get(i + 1).cloned();
                i += 1;
            }
            f => filter = Some(f.to_string()),
        }
        i += 1;
    }
    let mut harness = BTreeMap::new();
    for e in std::fs::read_dir(root.join("harness")).expect("harness dir").flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "js") {
            harness.insert(p.file_name().unwrap().to_string_lossy().to_string(), std::fs::read_to_string(&p).unwrap());
        }
    }
    let mut tests = Vec::new();
    collect(&root.join("test/language"), &mut tests);
    collect(&root.join("test/built-ins"), &mut tests);
    collect(&root.join("test/annexB"), &mut tests);
    tests.retain(|p| {
        let s = p.to_string_lossy();
        !SKIP_DIRS.iter().any(|d| s.contains(d)) && filter.as_ref().is_none_or(|f| s.contains(f.as_str()))
    });
    tests.sort();
    let tests = Arc::new(tests);
    let harness = Arc::new(harness);
    let next = Arc::new(AtomicUsize::new(0));
    let results: Arc<Mutex<Vec<(PathBuf, Outcome, String)>>> = Arc::new(Mutex::new(Vec::new()));
    // PULSAR_TRACE=1: one worker, printing each test before it runs (to find crashes).
    let trace = std::env::var("PULSAR_TRACE").is_ok();
    let workers = if trace { 1 } else { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4) };
    let mut handles = Vec::new();
    for _ in 0..workers {
        let (tests, harness, next, results) = (tests.clone(), harness.clone(), next.clone(), results.clone());
        handles.push(
            std::thread::Builder::new()
                .stack_size(512 << 20)
                .spawn(move || loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= tests.len() {
                        break;
                    }
                    if trace {
                        eprintln!("{}", tests[i].display());
                    }
                    let (o, msg) = run_test(&harness, &tests[i]);
                    results.lock().unwrap().push((tests[i].clone(), o, msg));
                })
                .unwrap(),
        );
    }
    std::panic::set_hook(Box::new(|_| {}));
    for h in handles {
        h.join().ok();
    }
    let mut results = Arc::try_unwrap(results).unwrap().into_inner().unwrap();
    results.sort_by(|a, b| a.0.cmp(&b.0));
    // Totals per top-level area (language/expressions, built-ins/Array, …).
    let mut areas: BTreeMap<String, (usize, usize, usize)> = BTreeMap::new();
    let (mut pass, mut fail, mut skip) = (0, 0, 0);
    let mut fail_lines = Vec::new();
    for (p, o, msg) in &results {
        let rel = p.strip_prefix(root.join("test")).unwrap_or(p).to_string_lossy().to_string();
        let area: String = rel.split('/').take(2).collect::<Vec<_>>().join("/");
        let e = areas.entry(area).or_default();
        match o {
            Outcome::Pass => {
                pass += 1;
                e.0 += 1;
            }
            Outcome::Fail => {
                fail += 1;
                e.1 += 1;
                fail_lines.push(format!("{rel}: {}", msg.lines().next().unwrap_or("")));
            }
            Outcome::Skip => {
                skip += 1;
                e.2 += 1;
            }
        }
    }
    let rate = 100.0 * pass as f64 / (pass + fail).max(1) as f64;
    println!("test262: {pass} passed, {fail} failed, {skip} skipped ({rate:.1}% of tests run)");
    if let Some(f) = failures_file {
        std::fs::write(&f, fail_lines.join("\n")).unwrap();
    }
    if let Some(f) = summary_file {
        let mut s = String::new();
        s.push_str(&format!("{pass} passed, {fail} failed, {skip} skipped: {rate:.1}% of the tests run\n\n"));
        s.push_str("| Area | Passed | Failed | Skipped | Pass rate |\n|---|---:|---:|---:|---:|\n");
        for (a, (p, f, sk)) in &areas {
            let r = if p + f > 0 { format!("{:.1}%", 100.0 * *p as f64 / (*p + *f) as f64) } else { "–".into() };
            s.push_str(&format!("| {a} | {p} | {f} | {sk} | {r} |\n"));
        }
        std::fs::write(&f, s).unwrap();
    }
}
