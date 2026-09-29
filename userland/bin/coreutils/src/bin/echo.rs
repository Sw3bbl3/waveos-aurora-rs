//! echo [TEXT...] — print arguments.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    aurora::println!("{}", args[1..].join(" "));
    0
}
