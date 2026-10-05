//! rm [-r] PATH... — remove files (and directories with -r).
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let recursive = args.iter().any(|a| a == "-r" || a == "-rf");
    let mut status = 0;
    for p in args[1..].iter().filter(|a| !a.starts_with('-')) {
        let path = coreutils::path(p);
        let r = if recursive { corekit::fs::remove_all(&path) } else { corekit::fs::remove(&path) };
        if let Err(e) = r {
            corekit::eprintln!("rm: {p}: {e}");
            status = 1;
        }
    }
    status
}
