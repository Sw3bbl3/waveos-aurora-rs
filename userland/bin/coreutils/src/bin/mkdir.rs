//! mkdir DIR... — create directories.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let mut status = 0;
    for d in &args[1..] {
        if let Err(e) = aurora::fs::mkdir(&coreutils::path(d)) {
            aurora::eprintln!("mkdir: {d}: {e}");
            status = 1;
        }
    }
    status
}
