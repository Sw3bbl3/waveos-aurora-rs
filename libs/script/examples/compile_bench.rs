//! Host-side compilation benchmark; does not execute downloaded scripts.
//! cargo run --release -p nebula-script --example compile_bench -- FILE.js ...
use nebula_script::{compiler, parser};
use std::{rc::Rc, time::Instant};

fn main() {
    for path in std::env::args().skip(1) {
        let src = std::fs::read_to_string(&path).expect("read script");
        if std::env::var("LAZY_LOOP").is_ok() {
            for _ in 0..30 {
                std::hint::black_box(parser::parse_script_lazy(&src).unwrap());
            }
            return;
        }
        let mut samples = Vec::new();
        for _ in 0..7 {
            let start = Instant::now();
            let ast = parser::parse_script(&src).expect("parse script");
            let parsed = start.elapsed();
            let start = Instant::now();
            let code = compiler::compile_script(&ast, Rc::from(src.as_str()), Rc::from(path.as_str()))
                .expect("compile script");
            let compiled = start.elapsed();
            std::hint::black_box(&code);
            samples.push((parsed.as_secs_f64() * 1000.0, compiled.as_secs_f64() * 1000.0));
        }
        let ast = parser::parse_script(&src).expect("parse script");
        let code = compiler::compile_script(&ast, Rc::from(src.as_str()), Rc::from(path.as_str())).unwrap();
        let (mut ops, mut consts, mut fns, mut strs, mut lines) = (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut stack = vec![code];
        while let Some(c) = stack.pop() {
            fns += 1;
            ops += c.ops.len();
            consts += c.consts.len();
            lines += c.lines.len();
            for k in &c.consts {
                match k {
                    nebula_script::bytecode::Const::Code(c) => stack.push(c.clone()),
                    nebula_script::bytecode::Const::Str(s) => strs += s.len() * 2,
                    _ => {}
                }
            }
        }
        println!(
            "functions={fns} ops={ops} (op size {}) consts={consts} string bytes={strs} line entries={lines}",
            std::mem::size_of::<nebula_script::bytecode::Op>()
        );
        let start = Instant::now();
        let toks = nebula_script::lexer::tokenize(&src).unwrap();
        println!("tokenize_ms={:.1} tokens={}", start.elapsed().as_secs_f64() * 1000.0, toks.len());
        drop(toks);
        let start = Instant::now();
        let lazy = parser::parse_script_lazy(&src).expect("lazy parse");
        let lp = start.elapsed();
        let lc = compiler::compile_script(&lazy, Rc::from(src.as_str()), Rc::from(path.as_str())).unwrap();
        println!(
            "lazy: parse_ms={:.1} compile_ms={:.1}",
            lp.as_secs_f64() * 1000.0,
            (start.elapsed() - lp).as_secs_f64() * 1000.0
        );
        std::hint::black_box(&lc);
        samples.sort_by(|a, b| (a.0 + a.1).total_cmp(&(b.0 + b.1)));
        let (parse, compile) = samples[samples.len() / 2];
        println!(
            "{path}: bytes={} parse_ms={parse:.3} compile_ms={compile:.3} total_ms={:.3}",
            src.len(),
            parse + compile
        );
    }
}
