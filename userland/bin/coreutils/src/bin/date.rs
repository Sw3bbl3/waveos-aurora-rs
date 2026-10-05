//! date — current date and time.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    corekit::println!("{}", corekit::time::format_date(&corekit::time::now()));
    0
}
