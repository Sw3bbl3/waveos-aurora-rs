//! mkdir DIR... — create directories.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let mut status = 0;
    for d in &args[1..] {
        if let Err(e) = corekit::fs::mkdir(&coreutils::path(d)) {
            corekit::eprintln!("mkdir: {d}: {e}");
            status = 1;
        }
    }
    status
}
