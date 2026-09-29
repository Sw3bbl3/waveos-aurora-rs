//! Preference system calls (`pref_get`/`pref_set`), the clock and display
//! calls, and what changing each preference does.

use super::server::{self, Command};
use super::settings;
use crate::drivers::{display, keymap, ps2};
use alloc::string::String;
use aurora_abi::{err::*, pref};

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 32 && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Defaults for preferences that have one.
fn default(key: &str) -> Option<&'static str> {
    Some(match key {
        pref::DARK | pref::CLOCK_24H | pref::CLOCK_SECONDS | pref::DND | pref::MUTED => "0",
        pref::CLOCK_DATE => "1",
        pref::ACCENT => "0",
        pref::KEYBOARD => "us",
        pref::REPEAT_DELAY => "1",
        pref::REPEAT_RATE => "8",
        pref::VOLUME => "70",
        _ => return None,
    })
}

pub fn get(key: &str) -> Option<String> {
    settings::get(key).or_else(|| default(key).map(String::from))
}

pub fn get_bool(key: &str) -> bool {
    get(key).is_some_and(|v| v == "1")
}

/// `pref_set`: stores the value and hands it to the compositor to apply.
pub fn set(key: &str, value: &str) -> Result<(), isize> {
    if !valid_key(key) || value.len() > 256 || value.contains('\n') {
        return Err(EINVAL);
    }
    match key {
        pref::KEYBOARD if !keymap::LAYOUTS.iter().any(|l| l.id == value) => return Err(EINVAL),
        pref::ACCENT if value.parse::<u8>().is_err() => return Err(EINVAL),
        _ => {}
    }
    settings::set(key, value);
    server::command(Command::Pref(String::from(key)));
    Ok(())
}

/// Applies preferences that live in drivers (at boot and when changed).
pub fn apply_system(key: &str) {
    match key {
        pref::KEYBOARD => {
            let id = get(pref::KEYBOARD).unwrap_or_default();
            keymap::set(&id);
        }
        pref::REPEAT_DELAY | pref::REPEAT_RATE => {
            let delay = get(pref::REPEAT_DELAY).and_then(|v| v.parse().ok()).unwrap_or(1u8);
            let rate = get(pref::REPEAT_RATE).and_then(|v| v.parse().ok()).unwrap_or(8u8);
            ps2::set_typematic(delay.min(3), rate.min(31));
        }
        _ => {}
    }
}

pub fn apply_all_system() {
    for k in [pref::KEYBOARD, pref::REPEAT_DELAY] {
        apply_system(k);
    }
}

/// The resolution the bootloader will use next time (from boot.conf).
pub fn boot_resolution() -> Option<(u32, u32)> {
    let data = crate::fs::read_all("/Boot/aurora/boot.conf").ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    let v = text.lines().find_map(|l| l.trim().strip_prefix("resolution="))?;
    let (w, h) = v.trim().split_once('x')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// Writes (or updates) `resolution=` in boot.conf.
pub fn set_boot_resolution(w: u32, h: u32) -> Result<(), isize> {
    let path = "/Boot/aurora/boot.conf";
    let old = crate::fs::read_all(path).unwrap_or_default();
    let mut text = String::new();
    for line in core::str::from_utf8(&old).unwrap_or("").lines() {
        if !line.trim().starts_with("resolution=") {
            text.push_str(line);
            text.push('\n');
        }
    }
    if text.is_empty() {
        text.push_str("# WaveOS Aurora boot options — written by Settings\n");
    }
    text.push_str(&alloc::format!("resolution={w}x{h}\n"));
    crate::fs::write_all(path, text.as_bytes())
}

/// `set_display(w, h)`: switch now when possible, otherwise at the next start.
pub fn set_display(w: u32, h: u32) -> Result<u64, isize> {
    if display::live() {
        server::command(Command::SetResolution(w, h));
        Ok(0)
    } else {
        set_boot_resolution(w, h)?;
        Err(ENOSYS)
    }
}
