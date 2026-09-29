//! df — disk space of the home volume.
#![no_std]
#![no_main]
extern crate alloc;
use aurora::process::fixed_str;
aurora::entry!(main);

fn main(_: aurora::Args) -> i32 {
    let i = aurora::process::sys_info();
    let used = i.disk_total.saturating_sub(i.disk_free);
    aurora::println!("Volume: {}", fixed_str(&i.root, i.root_len));
    if i.disk_total == 0 {
        aurora::println!("(in memory — no disk)");
        return 0;
    }
    let pct = used * 100 / i.disk_total.max(1);
    aurora::println!(
        "Size {}   Used {}   Free {}   ({}% used)",
        coreutils::human_size(i.disk_total),
        coreutils::human_size(used),
        coreutils::human_size(i.disk_free),
        pct
    );
    0
}
