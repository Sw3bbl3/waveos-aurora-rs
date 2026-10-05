//! Preference system calls (`pref_get`/`pref_set`), the clock and display
//! calls, and what changing each preference does.

use super::server::{self, Command};
use super::settings;
use crate::drivers::{audio, display, keymap, ps2};
use alloc::string::String;
use aurora_abi::{err::*, pref};

fn valid_key(k: &str) -> bool {
    !k.is_empty() && k.len() <= 32 && k.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// Defaults for preferences that have one.
fn default(key: &str) -> Option<&'static str> {
    Some(match key {
        pref::DARK
        | pref::CLOCK_24H
        | pref::CLOCK_SECONDS
        | pref::DND
        | pref::MUTED
        | pref::REDUCE_TRANSPARENCY
        | pref::REDUCE_MOTION
        | pref::HIGH_CONTRAST
        | pref::DOCK_AUTOHIDE => "0",
        pref::GLASS => "70",
        pref::DOCK_SIZE => "48",
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
    // Read-only facts about the system, answered live.
    if key == "audio_device" {
        return audio::describe();
    }
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
        pref::GLASS if !value.parse::<u8>().is_ok_and(|v| v <= 100) => return Err(EINVAL),
        pref::DOCK_SIZE if !value.parse::<u8>().is_ok_and(|v| (32..=64).contains(&v)) => return Err(EINVAL),
        pref::REDUCE_TRANSPARENCY | pref::REDUCE_MOTION | pref::HIGH_CONTRAST | pref::DOCK_AUTOHIDE
            if value != "0" && value != "1" =>
        {
            return Err(EINVAL)
        }
        pref::KEYBOARD if !keymap::LAYOUTS.iter().any(|l| l.id == value) => return Err(EINVAL),
        pref::ACCENT if value.parse::<u8>().is_err() => return Err(EINVAL),
        _ => {}
    }
    settings::try_set(key, value)?;
    server::command(Command::Pref(String::from(key)));
    Ok(())
}

pub fn apply_appearance() {
    let flags = [pref::REDUCE_TRANSPARENCY, pref::REDUCE_MOTION, pref::HIGH_CONTRAST, pref::DOCK_AUTOHIDE]
        .iter()
        .enumerate()
        .fold(0, |flags, (i, key)| flags | ((get_bool(key) as u8) << i));
    super::theme::set_effects(get(pref::GLASS).and_then(|v| v.parse().ok()).unwrap_or(70), flags);
    super::theme::set_dock_size(get(pref::DOCK_SIZE).and_then(|v| v.parse().ok()).unwrap_or(48));
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
        pref::VOLUME | pref::MUTED => {
            let volume = get(pref::VOLUME).and_then(|v| v.parse().ok()).unwrap_or(70u32);
            audio::set_volume(volume.min(100), get_bool(pref::MUTED));
        }
        _ => {}
    }
}

/// `audio_volume(SET, …)`: the menu-bar slider and volume keys.
pub fn set_volume(volume: u32, muted: bool) {
    let _ = set(pref::VOLUME, &alloc::format!("{}", volume.min(100)));
    let _ = set(pref::MUTED, if muted { "1" } else { "0" });
    // Apply now as well, so the next `audio_volume(GET)` already reflects it.
    apply_system(pref::VOLUME);
}

pub fn apply_all_system() {
    for k in [pref::KEYBOARD, pref::REPEAT_DELAY, pref::VOLUME] {
        apply_system(k);
    }
}

/// A `key=value` option from `\aurora\boot.conf` on the EFI partition.
pub fn boot_option(key: &str) -> Option<String> {
    let data = crate::fs::read_all("/Boot/aurora/boot.conf").ok()?;
    let text = core::str::from_utf8(&data).ok()?;
    text.lines().find_map(|l| {
        let (k, v) = l.trim().split_once('=')?;
        (k.trim() == key).then(|| String::from(v.trim()))
    })
}

/// The resolution the bootloader will use next time (from boot.conf).
pub fn boot_resolution() -> Option<(u32, u32)> {
    let v = boot_option("resolution")?;
    let (w, h) = v.split_once('x')?;
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
