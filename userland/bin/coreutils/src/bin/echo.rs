//! echo [TEXT...] — print arguments.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    corekit::println!("{}", args[1..].join(" "));
    0
}
