//! uptime — time since boot.
#![no_std]
#![no_main]
extern crate alloc;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    let i = aurora::process::sys_info();
    aurora::println!("up {}, {} processes", aurora::time::format_uptime(i.uptime_ms), i.processes);
    0
}
