//! cpuinfo — processors, clocks and CPU features.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_args: corekit::Args) -> i32 {
    let text = corekit::sys::report(corekit::abi::report::CPU);
    let _ = corekit::io::write_fd(1, text.as_bytes());
    0
}
