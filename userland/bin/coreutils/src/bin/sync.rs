//! sync — flush filesystem changes to disk.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    corekit::fs::sync();
    0
}
