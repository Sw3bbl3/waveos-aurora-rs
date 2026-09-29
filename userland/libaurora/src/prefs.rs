//! System preferences (see `abi::pref` for the keys), the clock and displays.

use crate::abi::{nr, DateTime, DisplayMode};
use crate::sys::{call, str_args};
use alloc::string::String;
use alloc::vec::Vec;

pub fn get(key: &str) -> Option<String> {
    let [kp, kl] = str_args(key);
    let mut buf = [0u8; 512];
    let n = call(nr::PREF_GET, &[kp, kl, buf.as_mut_ptr() as u64, buf.len() as u64]).ok()? as usize;
    Some(String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned())
}

pub fn get_bool(key: &str) -> bool {
    get(key).is_some_and(|v| v == "1")
}

pub fn get_int(key: &str, default: i64) -> i64 {
    get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

pub fn set(key: &str, value: &str) -> crate::Result<()> {
    let [kp, kl] = str_args(key);
    let [vp, vl] = str_args(value);
    call(nr::PREF_SET, &[kp, kl, vp, vl]).map(|_| ())
}

pub fn set_bool(key: &str, on: bool) {
    let _ = set(key, if on { "1" } else { "0" });
}

/// Sets the system clock.
pub fn set_datetime(d: &DateTime) -> crate::Result<()> {
    call(nr::SET_DATETIME, &[d as *const DateTime as u64]).map(|_| ())
}

pub fn display_modes() -> Vec<DisplayMode> {
    let mut buf = [DisplayMode::default(); 64];
    let n = call(nr::DISPLAY_MODES, &[buf.as_mut_ptr() as u64, buf.len() as u64]).unwrap_or(0) as usize;
    buf[..n.min(64)].to_vec()
}

/// Switches resolution. `Ok(true)`: now; `Ok(false)`: at the next start.
pub fn set_display(w: u32, h: u32) -> crate::Result<bool> {
    match call(nr::SET_DISPLAY, &[w as u64, h as u64]) {
        Ok(_) => Ok(true),
        Err(e) if e.is(crate::abi::err::ENOSYS) => Ok(false),
        Err(e) => Err(e),
    }
}
