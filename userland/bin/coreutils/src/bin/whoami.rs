//! whoami — the account this shell runs as.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    corekit::println!("{}", corekit::users::user_name(corekit::users::uid()));
    0
}
