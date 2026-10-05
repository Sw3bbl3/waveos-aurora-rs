//! System preferences, persisted as `key=value` lines in `/Settings/aurora.conf`.
//!
//! The window server owns them: Settings (and other apps) change them through
//! `desktop` requests, and every change is written back to disk at once.

use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};

const PATH: &str = "/Settings/aurora.conf";

static WRITER: spin::Mutex<()> = spin::Mutex::new(());

static VALUES: IrqMutex<BTreeMap<String, String>> = IrqMutex::new(BTreeMap::new());

pub fn load() {
    let Ok(data) = crate::fs::read_all(PATH) else { return };
    let mut v = VALUES.lock();
    for line in core::str::from_utf8(&data).unwrap_or("").lines() {
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, val)) = line.split_once('=') {
            v.insert(k.trim().to_string(), val.trim().to_string());
        }
    }
}

fn save(values: &BTreeMap<String, String>) -> Result<(), isize> {
    let mut text = String::from("# WaveOS Aurora preferences — written by the system\n");
    for (k, v) in values {
        text.push_str(k);
        text.push('=');
        text.push_str(v);
        text.push('\n');
    }
    let _ = crate::fs::mkdir("/Settings");
    let temp = "/Settings/aurora.conf.pending";
    crate::fs::write_all(temp, text.as_bytes())?;
    crate::fs::lookup(temp)?.0.sync()?;
    crate::fs::rename(temp, PATH)?;
    crate::fs::lookup(PATH)?.0.sync()?;
    Ok(())
}

pub fn get(key: &str) -> Option<String> {
    VALUES.lock().get(key).cloned()
}

pub fn get_int(key: &str, default: i64) -> i64 {
    get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn get_bool(key: &str, default: bool) -> bool {
    get(key).map(|v| v == "1" || v == "true").unwrap_or(default)
}

/// Sets `key` and saves (no write if unchanged).
pub fn set(key: &str, value: &str) {
    if let Err(e) = try_set(key, value) {
        log!("gui", "could not save settings: {}", aurora_abi::err::name(e));
    }
}

/// Persist before publishing a new preference to callers.
pub fn try_set(key: &str, value: &str) -> Result<(), isize> {
    let _writer = WRITER.lock();
    let mut snapshot = VALUES.lock().clone();
    if snapshot.get(key).map(String::as_str) == Some(value) {
        return Ok(());
    }
    snapshot.insert(String::from(key), String::from(value));
    save(&snapshot)?;
    VALUES.lock().insert(String::from(key), String::from(value));
    Ok(())
}

pub fn set_int(key: &str, value: i64) {
    set(key, &alloc::format!("{value}"));
}

pub fn set_bool(key: &str, value: bool) {
    set(key, if value { "1" } else { "0" });
}
