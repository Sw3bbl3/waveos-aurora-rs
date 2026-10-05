//! uptime — time since boot.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let i = corekit::process::sys_info();
    corekit::println!("up {}, {} processes", corekit::time::format_uptime(i.uptime_ms), i.processes);
    0
}
