//! touch FILE... — create empty files if missing.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::abi::open;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let mut status = 0;
    for f in &args[1..] {
        if let Err(e) = corekit::fs::File::open(&coreutils::path(f), open::WRITE | open::CREATE) {
            corekit::eprintln!("touch: {f}: {e}");
            status = 1;
        }
    }
    status
}
