//! mem — memory usage.
#![no_std]
#![no_main]
extern crate alloc;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let i = corekit::process::sys_info();
    corekit::println!("RAM    {:>6} MiB total  {:>6} MiB used", i.mem_total >> 20, i.mem_used >> 20);
    corekit::println!("Heap   {:>6} MiB total  {:>6} KiB used", i.heap_size >> 20, i.heap_used >> 10);
    if i.disk_total > 0 {
        corekit::println!("Disk   {:>6} MiB total  {:>6} MiB free", i.disk_total >> 20, i.disk_free >> 20);
    }
    0
}
