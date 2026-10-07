//! Time.

use crate::abi::{nr, DateTime};
use crate::sys::call;
use alloc::format;
use alloc::string::String;

/// Milliseconds since boot.
pub fn uptime_ms() -> u64 {
    call(nr::TIME_MS, &[]).unwrap_or(0)
}

pub fn sleep_ms(ms: u64) {
    let _ = call(nr::SLEEP, &[ms]);
}

pub fn now() -> DateTime {
    let mut d = DateTime::default();
    let _ = call(nr::DATETIME, &[&mut d as *mut DateTime as u64]);
    d
}

/// Days from 1970-01-01 to a calendar date (Howard Hinnant's days_from_civil).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The time zone set in Settings: minutes east of UTC.
pub fn utc_offset_minutes() -> i64 {
    crate::prefs::get_int("utc_offset", 0)
}

/// Milliseconds since the Unix epoch (UTC). The real-time clock keeps local
/// time; sub-second precision comes from the uptime counter.
pub fn unix_ms() -> f64 {
    static mut BASE: Option<(f64, u64)> = None;
    // SAFETY: programs read the clock from one thread at a time in practice;
    // a race only re-reads the clock.
    let base = unsafe { *core::ptr::addr_of!(BASE) };
    let (epoch, up) = match base {
        Some(b) => b,
        None => {
            let d = now();
            let days = days_from_civil(d.year as i64, d.month as i64, d.day as i64);
            let local = days * 86400 + d.hour as i64 * 3600 + d.minute as i64 * 60 + d.second as i64;
            let utc = local - utc_offset_minutes() * 60;
            let b = (utc as f64 * 1000.0, uptime_ms());
            unsafe { *core::ptr::addr_of_mut!(BASE) = Some(b) };
            b
        }
    };
    epoch + (uptime_ms() - up) as f64
}

pub const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

pub fn format_date(d: &DateTime) -> String {
    format!(
        "{} {} {} {:02}:{:02}:{:02} {}",
        WEEKDAYS[d.weekday as usize % 7],
        MONTHS[(d.month.clamp(1, 12) - 1) as usize],
        d.day,
        d.hour,
        d.minute,
        d.second,
        d.year
    )
}

/// Converts seconds since 2000-01-01 (file times) to a calendar date.
pub fn from_seconds(secs: u64) -> DateTime {
    let days = (secs / 86400) as i64 + 10957; // days since 1970-01-01
    let rem = secs % 86400;
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    DateTime {
        year: y as u16,
        month: m as u8,
        day: d as u8,
        hour: (rem / 3600) as u8,
        minute: (rem / 60 % 60) as u8,
        second: (rem % 60) as u8,
        weekday: ((days + 4).rem_euclid(7)) as u8,
    }
}

pub fn format_uptime(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}
