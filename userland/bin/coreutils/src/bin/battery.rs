//! battery — charge, time remaining and power source (from ACPI).
#![no_std]
#![no_main]
extern crate alloc;
use aurora::power::{self, *};
aurora::entry!(main);

fn duration(m: u32) -> alloc::string::String {
    alloc::format!("{}:{:02}", m / 60, m % 60)
}

fn main(_args: aurora::Args) -> i32 {
    let p = power::info();
    if p.flags & ACPI == 0 {
        aurora::println!("battery: ACPI is not available");
        return 1;
    }
    if p.flags & BATTERY == 0 {
        aurora::println!("No battery.");
    } else {
        let state = if p.flags & CHARGING != 0 {
            "charging"
        } else if p.flags & DISCHARGING != 0 {
            "discharging"
        } else {
            "not charging"
        };
        let model = core::str::from_utf8(&p.model[..p.model_len as usize]).unwrap_or("");
        aurora::println!(
            "Battery:  {}%, {}{}",
            p.percent,
            state,
            if p.flags & CRITICAL != 0 { " (critical)" } else { "" }
        );
        if p.minutes > 0 {
            let what = if p.flags & CHARGING != 0 { "until full" } else { "remaining" };
            aurora::println!("Time:     {} {}", duration(p.minutes), what);
        }
        aurora::println!("Energy:   {} of {} mWh (design {} mWh)", p.remaining_mwh, p.full_mwh, p.design_mwh);
        if p.rate_mw > 0 {
            aurora::println!("Rate:     {} mW", p.rate_mw);
        }
        if !model.is_empty() {
            aurora::println!("Model:    {model}");
        }
    }
    if p.flags & AC_PRESENT != 0 {
        aurora::println!("Adapter:  {}", if p.flags & AC_ONLINE != 0 { "connected" } else { "not connected" });
    }
    if p.flags & LID_PRESENT != 0 {
        aurora::println!("Lid:      {}", if p.flags & LID_OPEN != 0 { "open" } else { "closed" });
    }
    0
}
