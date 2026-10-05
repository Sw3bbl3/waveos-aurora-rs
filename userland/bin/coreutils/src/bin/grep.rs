//! grep [-i] [-v] PATTERN [FILE...] — print lines containing PATTERN.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::string::String;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let ignore_case = args.iter().any(|a| a == "-i");
    let invert = args.iter().any(|a| a == "-v");
    let rest: alloc::vec::Vec<String> = args[1..].iter().filter(|a| *a != "-i" && *a != "-v").cloned().collect();
    let Some(pattern) = rest.first() else {
        corekit::eprintln!("usage: grep [-i] [-v] PATTERN [FILE...]");
        return 2;
    };
    let pat = if ignore_case { pattern.to_lowercase() } else { pattern.clone() };
    let (inputs, mut status) = coreutils::inputs(&rest[1..], "grep");
    let multi = inputs.len() > 1;
    let mut found = false;
    for (name, data) in inputs {
        for line in String::from_utf8_lossy(&data).lines() {
            let hay = if ignore_case { line.to_lowercase() } else { String::from(line) };
            if hay.contains(pat.as_str()) != invert {
                found = true;
                if multi {
                    corekit::println!("{name}: {line}");
                } else {
                    corekit::println!("{line}");
                }
            }
        }
    }
    if !found && status == 0 {
        status = 1;
    }
    status
}
