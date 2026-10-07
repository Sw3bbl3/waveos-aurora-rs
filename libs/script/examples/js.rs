//! Runs a JavaScript file, or a REPL, on the host: `cargo run --example js [file.js]`.

use nebula_script::{inspect, Host, Realm, Value};
use std::io::{BufRead, Write};

struct StdHost;

impl Host for StdHost {
    fn now_ms(&mut self) -> f64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64() * 1000.0
    }

    fn console(&mut self, level: &str, line: &str) {
        if level == "error" || level == "warn" {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }
}

fn main() {
    let mut rt = Realm::new(Box::new(StdHost));
    let args: Vec<String> = std::env::args().collect();
    if let Some(path) = args.get(1) {
        let src = std::fs::read_to_string(path).expect("read file");
        let r = rt.eval(&src, path);
        rt.run_jobs();
        rt.report_unhandled();
        if let Err(e) = r {
            let msg = rt.describe_error(&e);
            eprintln!("Uncaught {msg}");
            std::process::exit(1);
        }
        return;
    }
    let stdin = std::io::stdin();
    print!("> ");
    std::io::stdout().flush().ok();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        match rt.eval(&line, "repl") {
            Ok(Value::Undefined) => {}
            Ok(v) => println!("{}", inspect(&mut rt, &v)),
            Err(e) => {
                let msg = rt.describe_error(&e);
                println!("Uncaught {msg}");
            }
        }
        rt.run_jobs();
        rt.report_unhandled();
        print!("> ");
        std::io::stdout().flush().ok();
    }
}
