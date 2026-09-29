//! sync — flush filesystem changes to disk.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    aurora::fs::sync();
    0
}
