//! volume [LEVEL | mute | unmute] — show or change the sound volume (0–100).
#![no_std]
#![no_main]
extern crate alloc;
use aurora::audio;
aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let v = audio::volume();
    if !v.present {
        aurora::eprintln!("volume: no sound device");
        return 1;
    }
    let v = match args.get(1).map(|a| a.as_str()) {
        None => v,
        Some("mute") => audio::set_volume(v.level, true),
        Some("unmute") => audio::set_volume(v.level, false),
        Some(n) => match n.parse::<u32>() {
            Ok(level) if level <= 100 => audio::set_volume(level, false),
            _ => {
                aurora::eprintln!("volume: expected 0–100, mute or unmute");
                return 2;
            }
        },
    };
    aurora::println!(
        "volume {}%{}{}",
        v.level,
        if v.muted { " (muted)" } else { "" },
        if v.headphones { ", headphones" } else { "" }
    );
    0
}
