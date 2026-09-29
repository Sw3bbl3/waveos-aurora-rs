//! Shared helpers for the Aurora command-line tools.

#![no_std]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use aurora::fs;

/// Resolves a path argument against the working directory.
pub fn path(arg: &str) -> String {
    fs::resolve(&fs::cwd(), arg)
}

/// Reads all of stdin (fd 0) until EOF.
pub fn read_stdin() -> Vec<u8> {
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = aurora::io::read_fd(0, &mut buf) {
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

/// Input from the named files, or stdin when none are given.
pub fn inputs(files: &[String], tool: &str) -> (Vec<(String, Vec<u8>)>, i32) {
    if files.is_empty() {
        return (alloc::vec![(String::from("-"), read_stdin())], 0);
    }
    let mut out = Vec::new();
    let mut status = 0;
    for f in files {
        match fs::read(&path(f)) {
            Ok(d) => out.push((f.clone(), d)),
            Err(e) => {
                aurora::eprintln!("{tool}: {f}: {e}");
                status = 1;
            }
        }
    }
    (out, status)
}

pub fn human_size(n: u64) -> String {
    match n {
        0..=1023 => alloc::format!("{n} B"),
        1024..=1_048_575 => alloc::format!("{:.1} KB", n as f64 / 1024.0),
        _ => alloc::format!("{:.1} MB", n as f64 / 1048576.0),
    }
}
