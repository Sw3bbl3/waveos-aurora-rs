//! ls [-l] [DIR...] — list directory contents.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::{fs, println};
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let long = args.iter().any(|a| a == "-l");
    let mut dirs: alloc::vec::Vec<&str> =
        args[1..].iter().filter(|a| !a.starts_with('-')).map(|s| s.as_str()).collect();
    if dirs.is_empty() {
        dirs.push(".");
    }
    let mut status = 0;
    for (i, d) in dirs.iter().enumerate() {
        let p = coreutils::path(d);
        if dirs.len() > 1 {
            println!("{}{}:", if i > 0 { "\n" } else { "" }, p);
        }
        match fs::read_dir(&p) {
            Ok(entries) => {
                for e in entries {
                    if long {
                        let t = corekit::time::from_seconds(e.mtime);
                        let size =
                            if e.is_dir { alloc::string::String::from("-") } else { coreutils::human_size(e.size) };
                        println!(
                            "{} {:>9}  {} {:>2} {:02}:{:02}  {}{}",
                            if e.is_dir { "d" } else { "-" },
                            size,
                            corekit::time::MONTHS[(t.month.clamp(1, 12) - 1) as usize],
                            t.day,
                            t.hour,
                            t.minute,
                            e.name,
                            if e.is_dir { "/" } else { "" }
                        );
                    } else {
                        println!("{}{}", e.name, if e.is_dir { "/" } else { "" });
                    }
                }
            }
            Err(e) => {
                corekit::eprintln!("ls: {d}: {e}");
                status = 1;
            }
        }
    }
    status
}
