//! touch FILE... — create empty files if missing.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::abi::open;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let mut status = 0;
    for f in &args[1..] {
        if let Err(e) = aurora::fs::File::open(&coreutils::path(f), open::WRITE | open::CREATE) {
            aurora::eprintln!("touch: {f}: {e}");
            status = 1;
        }
    }
    status
}
