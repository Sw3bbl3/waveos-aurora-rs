//! Page scripts are untrusted: whatever text arrives is a SyntaxError, an
//! exception or a result — never a panic in the engine. Each program runs
//! under a step budget, as Nebula runs page scripts.

use nebula_script::{Host, Realm};

struct Quiet;

impl Host for Quiet {
    fn now_ms(&mut self) -> f64 {
        1_700_000_000_000.0
    }
    fn console(&mut self, _level: &str, _line: &str) {}
}

/// Tokens that reach the corners of the grammar and the standard library.
const WORDS: &[&str] = &[
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "=>",
    "async ",
    "await ",
    "function*",
    "yield ",
    "class ",
    "extends ",
    "super",
    "new.target",
    "`${",
    "}`",
    "?.",
    "??=",
    "**",
    "...",
    "#priv",
    "static {",
    "label:",
    "0x",
    "1e999",
    "let ",
    "const ",
    "var ",
    "get ",
    "set ",
    "of ",
    "in ",
    "delete ",
    "typeof ",
    "eval(",
    "arguments",
    "Symbol.iterator",
    "new Uint8Array(",
    ".sort(",
    "JSON.parse(",
    "JSON.stringify(",
    "-1",
    "2**53",
    "NaN",
    "new Array(",
    "Reflect.",
    "Object.defineProperty(",
    ".toFixed(",
    ".toString(36)",
    "/a*b/gu",
    ".replace(",
    ".split(",
    "Promise.all(",
    ".then(",
    "throw ",
    "try{",
    "}catch(e){",
    "}finally{",
    "'use strict';",
];

#[test]
fn mutated_programs_never_panic() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/js");
    let mut seeds: Vec<Vec<u8>> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "js") && !p.to_string_lossy().contains("jquery"))
        .map(|p| std::fs::read(p).unwrap())
        .collect();
    seeds.sort();
    aurora_mutate::check_with(
        "js",
        &seeds,
        300,
        |m, seed| {
            let mut program = m.mutate(seed);
            m.splice_words(&mut program, WORDS);
            program
        },
        |program| {
            let src = String::from_utf8_lossy(program);
            let mut rt = Realm::new(Box::new(Quiet));
            rt.step_limit = 200_000;
            let _ = rt.eval(&src, "mutated.js");
            rt.run_jobs();
        },
    );
}
