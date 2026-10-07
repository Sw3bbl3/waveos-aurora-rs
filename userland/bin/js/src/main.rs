//! js — run JavaScript with Pulsar, the engine inside Nebula.
//!
//!   js                  an interactive prompt (.exit or Ctrl+D leaves)
//!   js FILE.js          run a file
//!   js -e CODE          evaluate CODE and print the result

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::string::String;
use corekit::{eprintln, print, println};
use nebula_script::{inspect, Host, Realm, Value};

corekit::entry!(main);

struct OsHost;

impl Host for OsHost {
    fn now_ms(&mut self) -> f64 {
        corekit::time::unix_ms()
    }

    fn console(&mut self, level: &str, line: &str) {
        if level == "error" || level == "warn" {
            eprintln!("{line}");
        } else {
            println!("{line}");
        }
    }

    fn seed(&mut self) -> u64 {
        let mut b = [0u8; 8];
        corekit::net::random(&mut b);
        u64::from_le_bytes(b)
    }

    fn timezone_offset(&mut self, _utc_ms: f64) -> f64 {
        -(corekit::time::utc_offset_minutes() as f64)
    }
}

/// Runs code; prints uncaught errors. Returns the completion value.
fn run(rt: &mut Realm, src: &str, name: &str) -> Option<Value> {
    let r = rt.eval(src, name);
    rt.run_jobs();
    rt.report_unhandled();
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            let msg = rt.describe_error(&e);
            eprintln!("Uncaught {msg}");
            None
        }
    }
}

/// Whether a compile error means "keep typing" (an unfinished statement).
fn incomplete(rt: &mut Realm, src: &str) -> bool {
    match rt.compile(src, "repl") {
        Ok(_) => false,
        Err(e) => {
            let msg = rt.describe_error(&e);
            msg.contains("end of input") || msg.contains("unterminated") || msg.contains("expected '}'")
        }
    }
}

fn repl(rt: &mut Realm) -> i32 {
    println!("Pulsar JavaScript — type .exit or press Ctrl+D to leave.");
    let mut pending = String::new();
    let mut line = String::new();
    let mut buf = [0u8; 512];
    print!("> ");
    loop {
        let n = match corekit::io::read_fd(0, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for ch in String::from_utf8_lossy(&buf[..n]).chars() {
            if ch != '\n' {
                line.push(ch);
                continue;
            }
            let text = core::mem::take(&mut line);
            if pending.is_empty() && text.trim() == ".exit" {
                return 0;
            }
            if !pending.is_empty() {
                pending.push('\n');
            }
            pending.push_str(&text);
            if incomplete(rt, &pending) {
                print!("... ");
                continue;
            }
            let src = core::mem::take(&mut pending);
            if !src.trim().is_empty() {
                if let Some(v) = run(rt, &src, "repl") {
                    if !v.is_undefined() {
                        println!("{}", inspect(rt, &v));
                    }
                }
            }
            print!("> ");
        }
    }
    println!();
    0
}

fn main(args: corekit::Args) -> i32 {
    let mut rt = Realm::new(Box::new(OsHost));
    match args.get(1).map(String::as_str) {
        None => repl(&mut rt),
        Some("-e") => {
            let Some(code) = args.get(2) else {
                eprintln!("js: -e needs some code");
                return 2;
            };
            match run(&mut rt, code, "-e") {
                Some(v) => {
                    if !v.is_undefined() {
                        println!("{}", inspect(&mut rt, &v));
                    }
                    0
                }
                None => 1,
            }
        }
        Some("-h" | "--help") => {
            println!("usage: js [FILE.js | -e CODE]");
            0
        }
        Some(path) => {
            let full = corekit::fs::resolve(&corekit::fs::cwd(), path);
            let src = match corekit::fs::read_to_string(&full) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("js: {path}: {e}");
                    return 1;
                }
            };
            if run(&mut rt, &src, path).is_some() {
                0
            } else {
                1
            }
        }
    }
}
