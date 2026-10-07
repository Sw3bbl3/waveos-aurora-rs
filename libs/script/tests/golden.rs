//! Runs every `tests/js/*.js` and compares its console output with the
//! `.out` file next to it (recorded from Node). Each script runs twice: once
//! normally and once with the collector running every few allocations, to
//! catch values the collector can't see.

use nebula_script::{Host, Realm};
use std::cell::RefCell;
use std::rc::Rc;

struct Capture(Rc<RefCell<String>>);

impl Host for Capture {
    fn now_ms(&mut self) -> f64 {
        1_700_000_000_000.0
    }

    fn console(&mut self, _level: &str, line: &str) {
        let mut out = self.0.borrow_mut();
        out.push_str(line);
        out.push('\n');
    }
}

fn run(src: &str, name: &str, pressure: Option<usize>) -> String {
    let out = Rc::new(RefCell::new(String::new()));
    let mut rt = Realm::new(Box::new(Capture(out.clone())));
    if let Some(p) = pressure {
        rt.heap.set_pressure(p);
    }
    if let Err(e) = rt.eval(src, name) {
        let msg = rt.describe_error(&e);
        out.borrow_mut().push_str(&format!("Uncaught {msg}\n"));
    }
    rt.run_jobs();
    rt.report_unhandled();
    let s = out.borrow().clone();
    s
}

#[test]
fn golden_scripts() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js");
    let mut failures = Vec::new();
    let mut paths: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).collect();
    paths.sort();
    let mut count = 0;
    for path in paths {
        if path.extension().is_none_or(|e| e != "js") {
            continue;
        }
        count += 1;
        let src = std::fs::read_to_string(&path).unwrap();
        let expected = std::fs::read_to_string(path.with_extension("out")).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        for pressure in [None, Some(64)] {
            let got = run(&src, &name, pressure);
            if got != expected {
                failures.push(format!("{name} (gc pressure {pressure:?})\n--- expected\n{expected}--- got\n{got}"));
            }
        }
    }
    assert!(count > 0, "no scripts found");
    assert!(failures.is_empty(), "{} failing:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn collector_reclaims_garbage_and_keeps_live_data() {
    let out = Rc::new(RefCell::new(String::new()));
    let mut rt = Realm::new(Box::new(Capture(out.clone())));
    rt.heap.set_pressure(1000);
    let src = r#"
        const keep = [];
        for (let i = 0; i < 200000; i++) {
            const tmp = { i, list: [i, i + 1], s: "x" + i };
            if (i % 1000 === 0) keep.push(tmp);
        }
        const m = new Map(); for (let i = 0; i < 50; i++) m.set(i, { v: i });
        const closures = []; for (let i = 0; i < 100; i++) closures.push(() => i);
        console.log(keep.length, keep[199].list[1], keep[50].s, m.get(49).v, closures[99]());
    "#;
    rt.eval(src, "gc.js").unwrap();
    assert_eq!(*out.borrow(), "200 199001 x50000 49 99\n");
    assert!(rt.heap.collections > 10, "only {} collections", rt.heap.collections);
    assert!(rt.heap.live() < 20_000, "{} objects still live", rt.heap.live());
}

#[test]
fn runaway_scripts_are_stopped() {
    let out = Rc::new(RefCell::new(String::new()));
    let mut rt = Realm::new(Box::new(Capture(out.clone())));
    rt.step_limit = 1_000_000;
    let r = rt.eval("let n = 0; while (true) { try { n++; } catch (e) {} }", "loop.js");
    assert!(r.is_err());
    assert!(rt.interrupted);
}
