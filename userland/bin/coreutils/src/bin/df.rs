//! df — disk space of the home volume and of any USB volumes.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::format;
use alloc::string::String;
use aurora::process::fixed_str;
aurora::entry!(main);

fn row(name: &str, (total, free): (u64, u64)) {
    let used = total.saturating_sub(free);
    aurora::println!(
        "{:<22} {:>9} {:>9} {:>9} {:>4}%",
        name,
        coreutils::human_size(total),
        coreutils::human_size(used),
        coreutils::human_size(free),
        used * 100 / total.max(1)
    );
}

fn main(_: aurora::Args) -> i32 {
    let i = aurora::process::sys_info();
    aurora::println!("{:<22} {:>9} {:>9} {:>9} {:>5}", "Volume", "Size", "Used", "Free", "Use");
    match aurora::fs::space("/") {
        Ok(s) if s.0 > 0 => row("Home (/)", s),
        _ => aurora::println!("{:<22} (in memory — no disk)", "Home (/)"),
    }
    if let Ok(s) = aurora::fs::space("/Boot") {
        row("EFI Boot (/Boot)", s);
    }
    for e in aurora::fs::read_dir("/Volumes").unwrap_or_default() {
        let path: String = format!("/Volumes/{}", e.name);
        if let Ok(s) = aurora::fs::space(&path) {
            row(&e.name, s);
        }
    }
    aurora::println!("\nHome volume: {}", fixed_str(&i.root, i.root_len));
    0
}
