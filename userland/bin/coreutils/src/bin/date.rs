//! date — current date and time.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    aurora::println!("{}", aurora::time::format_date(&aurora::time::now()));
    0
}
