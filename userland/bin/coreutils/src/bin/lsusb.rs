//! lsusb — USB devices.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(_args: aurora::Args) -> i32 {
    let text = aurora::sys::report(aurora::abi::report::USB);
    let _ = aurora::io::write_fd(1, text.as_bytes());
    0
}
