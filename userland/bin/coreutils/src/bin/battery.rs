//! battery — charge, time remaining and power source (from ACPI).
#![no_std]
#![no_main]
extern crate alloc;
use corekit::power::{self, *};
corekit::entry!(main);

fn duration(m: u32) -> alloc::string::String {
    alloc::format!("{}:{:02}", m / 60, m % 60)
}

fn main(_args: corekit::Args) -> i32 {
    let p = power::info();
    if p.flags & ACPI == 0 {
        corekit::println!("battery: ACPI is not available");
        return 1;
    }
    if p.flags & BATTERY == 0 {
        corekit::println!("No battery.");
    } else {
        let state = if p.flags & CHARGING != 0 {
            "charging"
        } else if p.flags & DISCHARGING != 0 {
            "discharging"
        } else {
            "not charging"
        };
        let model = core::str::from_utf8(&p.model[..p.model_len as usize]).unwrap_or("");
        corekit::println!(
            "Battery:  {}%, {}{}",
            p.percent,
            state,
            if p.flags & CRITICAL != 0 { " (critical)" } else { "" }
        );
        if p.minutes > 0 {
            let what = if p.flags & CHARGING != 0 { "until full" } else { "remaining" };
            corekit::println!("Time:     {} {}", duration(p.minutes), what);
        }
        corekit::println!("Energy:   {} of {} mWh (design {} mWh)", p.remaining_mwh, p.full_mwh, p.design_mwh);
        if p.rate_mw > 0 {
            corekit::println!("Rate:     {} mW", p.rate_mw);
        }
        if !model.is_empty() {
            corekit::println!("Model:    {model}");
        }
    }
    if p.flags & AC_PRESENT != 0 {
        corekit::println!("Adapter:  {}", if p.flags & AC_ONLINE != 0 { "connected" } else { "not connected" });
    }
    if p.flags & LID_PRESENT != 0 {
        corekit::println!("Lid:      {}", if p.flags & LID_OPEN != 0 { "open" } else { "closed" });
    }
    0
}
