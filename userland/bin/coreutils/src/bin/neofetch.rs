//! neofetch — system summary with the Aurora logo.
#![no_std]
#![no_main]
extern crate alloc;
use corekit::process::fixed_str;
corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let i = corekit::process::sys_info();
    let logo = [
        "     ~~~~~~~~      ",
        "   ~~        ~~    ",
        "  ~   ~~~~~~   ~   ",
        " ~  ~~      ~~  ~  ",
        "  ~~   ~~~~   ~~   ",
        "     ~~    ~~      ",
        "   ~~~~~~~~~~~~    ",
    ];
    let info = [
        alloc::string::String::from("aurora@waveos"),
        alloc::string::String::from("-------------"),
        alloc::format!("OS: WaveOS Aurora {}", fixed_str(&i.version, i.version_len)),
        alloc::string::String::from("Kernel: Aster (hybrid, x86_64)"),
        alloc::format!("Uptime: {}", corekit::time::format_uptime(i.uptime_ms)),
        alloc::format!("Processes: {}", i.processes),
        alloc::format!("Resolution: {}x{}", i.screen_w, i.screen_h),
        alloc::format!("CPU: {}", fixed_str(&i.cpu, i.cpu_len)),
        alloc::format!("Memory: {} MiB / {} MiB", i.mem_used >> 20, i.mem_total >> 20),
        alloc::format!("Home: {}", fixed_str(&i.root, i.root_len)),
    ];
    for n in 0..info.len().max(logo.len()) {
        corekit::println!(
            "{} {}",
            logo.get(n).copied().unwrap_or("                   "),
            info.get(n).map(|s| s.as_str()).unwrap_or("")
        );
    }
    0
}
