//! Date, with the specification's day arithmetic. Local time uses the
//! host's time-zone offset.

use super::*;
use crate::numconv;
use alloc::format;
use alloc::string::String;

const MS_PER_DAY: f64 = 86_400_000.0;
const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTH_NAMES: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

fn day(t: f64) -> f64 {
    libm::floor(t / MS_PER_DAY)
}

fn time_within_day(t: f64) -> f64 {
    let r = t % MS_PER_DAY;
    if r < 0.0 {
        r + MS_PER_DAY
    } else {
        r
    }
}

fn days_in_year(y: f64) -> f64 {
    if y % 4.0 != 0.0 {
        365.0
    } else if y % 100.0 != 0.0 {
        366.0
    } else if y % 400.0 != 0.0 {
        365.0
    } else {
        366.0
    }
}

fn day_from_year(y: f64) -> f64 {
    365.0 * (y - 1970.0) + libm::floor((y - 1969.0) / 4.0) - libm::floor((y - 1901.0) / 100.0)
        + libm::floor((y - 1601.0) / 400.0)
}

fn year_from_time(t: f64) -> f64 {
    let mut y = libm::floor(t / (MS_PER_DAY * 365.2425)) + 1970.0;
    while day_from_year(y) * MS_PER_DAY > t {
        y -= 1.0;
    }
    while day_from_year(y + 1.0) * MS_PER_DAY <= t {
        y += 1.0;
    }
    y
}

fn in_leap_year(t: f64) -> bool {
    days_in_year(year_from_time(t)) == 366.0
}

fn day_within_year(t: f64) -> f64 {
    day(t) - day_from_year(year_from_time(t))
}

fn month_starts(leap: bool) -> [f64; 13] {
    let l = leap as u8 as f64;
    [
        0.0,
        31.0,
        59.0 + l,
        90.0 + l,
        120.0 + l,
        151.0 + l,
        181.0 + l,
        212.0 + l,
        243.0 + l,
        273.0 + l,
        304.0 + l,
        334.0 + l,
        365.0 + l,
    ]
}

fn month_from_time(t: f64) -> f64 {
    let d = day_within_year(t);
    let starts = month_starts(in_leap_year(t));
    (0..12).find(|&m| d < starts[m + 1]).unwrap_or(11) as f64
}

fn date_from_time(t: f64) -> f64 {
    let d = day_within_year(t);
    let starts = month_starts(in_leap_year(t));
    d - starts[month_from_time(t) as usize] + 1.0
}

fn week_day(t: f64) -> f64 {
    let r = (day(t) + 4.0) % 7.0;
    if r < 0.0 {
        r + 7.0
    } else {
        r
    }
}

fn hour(t: f64) -> f64 {
    libm::floor(time_within_day(t) / 3_600_000.0)
}

fn min_from_time(t: f64) -> f64 {
    libm::floor(time_within_day(t) / 60_000.0) % 60.0
}

fn sec_from_time(t: f64) -> f64 {
    libm::floor(time_within_day(t) / 1000.0) % 60.0
}

fn ms_from_time(t: f64) -> f64 {
    time_within_day(t) % 1000.0
}

fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !(h.is_finite() && m.is_finite() && s.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    numconv::to_integer(h) * 3_600_000.0
        + numconv::to_integer(m) * 60_000.0
        + numconv::to_integer(s) * 1000.0
        + numconv::to_integer(ms)
}

fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !(year.is_finite() && month.is_finite() && date.is_finite()) {
        return f64::NAN;
    }
    let (y, m, dt) = (numconv::to_integer(year), numconv::to_integer(month), numconv::to_integer(date));
    let ym = y + libm::floor(m / 12.0);
    let mn = ((m % 12.0) + 12.0) % 12.0;
    if ym.abs() > 400_000.0 {
        return f64::NAN;
    }
    let t = day_from_year(ym) * MS_PER_DAY;
    let starts = month_starts(days_in_year(ym) == 366.0);
    day(t) + starts[mn as usize] + dt - 1.0
}

fn make_date(day: f64, time: f64) -> f64 {
    if !day.is_finite() || !time.is_finite() {
        return f64::NAN;
    }
    day * MS_PER_DAY + time
}

fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    numconv::to_integer(t) + 0.0
}

/// UTC − local, in milliseconds, at UTC time `t`.
fn offset_ms(rt: &mut Realm, t: f64) -> f64 {
    if !t.is_finite() {
        return 0.0;
    }
    rt.host.timezone_offset(t) * 60_000.0
}

fn local(rt: &mut Realm, t: f64) -> f64 {
    t - offset_ms(rt, t)
}

fn utc(rt: &mut Realm, local_t: f64) -> f64 {
    local_t + offset_ms(rt, local_t)
}

fn this_time(rt: &mut Realm, c: &Call) -> Result<f64, Value> {
    if let Value::Object(o) = c.this {
        if let Kind::Date(t) = rt.heap.get(o).kind {
            return Ok(t);
        }
    }
    Err(rt.type_error("this is not a Date object."))
}

fn set_this_time(rt: &mut Realm, c: &Call, t: f64) -> JsResult {
    let t = time_clip(t);
    if let Value::Object(o) = c.this {
        rt.heap.get_mut(o).kind = Kind::Date(t);
    }
    Ok(Value::Number(t))
}

// ---------------------------------------------------------------- parsing

fn parse_date(rt: &mut Realm, s: &str) -> f64 {
    let s = s.trim();
    if let Some(t) = parse_iso(rt, s) {
        return t;
    }
    parse_loose(rt, s).unwrap_or(f64::NAN)
}

fn digits(s: &[u8], i: &mut usize, n: usize) -> Option<f64> {
    if *i + n > s.len() || !s[*i..*i + n].iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let v = core::str::from_utf8(&s[*i..*i + n]).ok()?.parse::<f64>().ok()?;
    *i += n;
    Some(v)
}

/// The ISO format of the specification (and its date-only forms).
fn parse_iso(rt: &mut Realm, s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    let year = if b.first() == Some(&b'+') || b.first() == Some(&b'-') {
        let neg = b[0] == b'-';
        i = 1;
        let y = digits(b, &mut i, 6)?;
        if neg && y == 0.0 {
            return None;
        }
        if neg {
            -y
        } else {
            y
        }
    } else {
        digits(b, &mut i, 4)?
    };
    let (mut month, mut dayn) = (1.0, 1.0);
    if b.get(i) == Some(&b'-') {
        i += 1;
        month = digits(b, &mut i, 2)?;
        if b.get(i) == Some(&b'-') {
            i += 1;
            dayn = digits(b, &mut i, 2)?;
        }
    }
    let (mut h, mut m, mut sec, mut ms) = (0.0, 0.0, 0.0, 0.0);
    let mut has_time = false;
    let mut offset: Option<f64> = None;
    if b.get(i) == Some(&b'T') || b.get(i) == Some(&b't') || (b.get(i) == Some(&b' ') && b.len() > i + 3) {
        i += 1;
        has_time = true;
        h = digits(b, &mut i, 2)?;
        if b.get(i) != Some(&b':') {
            return None;
        }
        i += 1;
        m = digits(b, &mut i, 2)?;
        if b.get(i) == Some(&b':') {
            i += 1;
            sec = digits(b, &mut i, 2)?;
            if b.get(i) == Some(&b'.') || b.get(i) == Some(&b',') {
                i += 1;
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let frac = core::str::from_utf8(&b[start..i]).ok()?;
                let f = format!("0.{frac}").parse::<f64>().ok()?;
                ms = libm::floor(f * 1000.0);
            }
        }
        match b.get(i) {
            Some(b'Z') | Some(b'z') => {
                offset = Some(0.0);
                i += 1;
            }
            Some(&sign @ (b'+' | b'-')) => {
                i += 1;
                let oh = digits(b, &mut i, 2)?;
                if b.get(i) == Some(&b':') {
                    i += 1;
                }
                let om = digits(b, &mut i, 2).unwrap_or(0.0);
                let o = (oh * 60.0 + om) * 60_000.0;
                offset = Some(if sign == b'+' { o } else { -o });
            }
            _ => {}
        }
    }
    if i != b.len() {
        return None;
    }
    if !(1.0..=12.0).contains(&month) || dayn < 1.0 || dayn > 31.0 || h > 24.0 || m > 59.0 || sec > 59.0 {
        return None;
    }
    if h == 24.0 && (m != 0.0 || sec != 0.0 || ms != 0.0) {
        return None;
    }
    let t = make_date(make_day(year, month - 1.0, dayn), make_time(h, m, sec, ms));
    Some(time_clip(match offset {
        Some(o) => t - o,
        // Date-only forms are UTC; date-time forms without offset are local.
        None if has_time => utc(rt, t),
        None => t,
    }))
}

/// Formats like "Tue Mar 01 2022 10:00:00 GMT+0100", "March 1, 2022 10:00",
/// "1 Mar 2022", "2022/03/01 10:00".
fn parse_loose(rt: &mut Realm, s: &str) -> Option<f64> {
    let mut year = None;
    let mut month = None;
    let mut dayn = None;
    let (mut h, mut m, mut sec) = (0.0, 0.0, 0.0);
    let mut offset: Option<f64> = None;
    let mut pm = None;
    let cleaned: String = s.chars().map(|c| if c == ',' { ' ' } else { c }).collect();
    let mut numbers = Vec::new();
    for tok in cleaned.split_whitespace() {
        let lower = tok.to_ascii_lowercase();
        if let Some(mi) = MONTHS.iter().position(|mn| lower.starts_with(&mn.to_ascii_lowercase())) {
            month = Some(mi as f64);
            continue;
        }
        if DAYS.iter().any(|d| lower.starts_with(&d.to_ascii_lowercase())) {
            continue;
        }
        if lower == "am" || lower == "pm" {
            pm = Some(lower == "pm");
            continue;
        }
        if lower.starts_with("gmt") || lower.starts_with("utc") || lower == "z" {
            let rest = &tok[3.min(tok.len())..];
            offset = Some(parse_offset(rest).unwrap_or(0.0));
            continue;
        }
        if (tok.starts_with('+') || tok.starts_with('-')) && tok.len() >= 5 && offset.is_none() {
            offset = parse_offset(tok);
            continue;
        }
        if tok.starts_with('(') {
            break;
        }
        if tok.contains(':') {
            let parts: Vec<&str> = tok.split(':').collect();
            h = parts.first()?.parse().ok()?;
            m = parts.get(1).map_or(Some(0.0), |p| p.parse().ok())?;
            sec = parts.get(2).map_or(Some(0.0), |p| p.split('.').next().unwrap_or("0").parse().ok())?;
            continue;
        }
        if tok.contains('/') || (tok.contains('-') && tok.len() > 4) {
            let parts: Vec<f64> = tok.split(['/', '-']).filter_map(|p| p.parse().ok()).collect();
            if parts.len() == 3 {
                if parts[0] > 31.0 {
                    year = Some(parts[0]);
                    month = Some(parts[1] - 1.0);
                    dayn = Some(parts[2]);
                } else {
                    month = Some(parts[0] - 1.0);
                    dayn = Some(parts[1]);
                    year = Some(parts[2]);
                }
                continue;
            }
            return None;
        }
        let n: f64 = tok.parse().ok()?;
        numbers.push(n);
    }
    for n in numbers {
        if dayn.is_none() && n <= 31.0 && month.is_some() {
            dayn = Some(n);
        } else if year.is_none() {
            year = Some(if n < 50.0 {
                2000.0 + n
            } else if n < 100.0 {
                1900.0 + n
            } else {
                n
            });
        } else if dayn.is_none() {
            dayn = Some(n);
        }
    }
    if let Some(p) = pm {
        if p && h < 12.0 {
            h += 12.0;
        } else if !p && h == 12.0 {
            h = 0.0;
        }
    }
    let t = make_date(make_day(year?, month?, dayn.unwrap_or(1.0)), make_time(h, m, sec, 0.0));
    Some(time_clip(match offset {
        Some(o) => t - o,
        None => utc(rt, t),
    }))
}

fn parse_offset(s: &str) -> Option<f64> {
    let (sign, rest) = match s.chars().next()? {
        '+' => (1.0, &s[1..]),
        '-' => (-1.0, &s[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| c.is_ascii_digit()).collect();
    let (h, m) = match digits.len() {
        1 | 2 => (digits.parse::<f64>().ok()?, 0.0),
        4 => (digits[..2].parse::<f64>().ok()?, digits[2..].parse::<f64>().ok()?),
        _ => return None,
    };
    Some(sign * (h * 60.0 + m) * 60_000.0)
}

// ---------------------------------------------------------------- formatting

fn fmt_year(y: f64) -> String {
    if y < 0.0 {
        format!("-{:06}", -y as i64)
    } else {
        format!("{:04}", y as i64)
    }
}

fn tz_string(rt: &mut Realm, t: f64) -> String {
    let off = -offset_ms(rt, t) / 60_000.0;
    let sign = if off >= 0.0 { '+' } else { '-' };
    let a = off.abs() as i64;
    format!("GMT{}{:02}{:02}", sign, a / 60, a % 60)
}

fn date_part(t: f64) -> String {
    format!(
        "{} {} {:02} {}",
        DAYS[week_day(t) as usize],
        MONTHS[month_from_time(t) as usize],
        date_from_time(t) as i64,
        fmt_year(year_from_time(t))
    )
}

fn time_part(t: f64) -> String {
    format!("{:02}:{:02}:{:02}", hour(t) as i64, min_from_time(t) as i64, sec_from_time(t) as i64)
}

pub fn to_date_string(rt: &mut Realm, t: f64) -> String {
    if t.is_nan() {
        return String::from("Invalid Date");
    }
    let l = local(rt, t);
    format!("{} {} {}", date_part(l), time_part(l), tz_string(rt, t))
}

fn iso_string(t: f64) -> String {
    let y = year_from_time(t);
    let ys = if (0.0..=9999.0).contains(&y) {
        format!("{:04}", y as i64)
    } else if y < 0.0 {
        format!("-{:06}", -y as i64)
    } else {
        format!("+{:06}", y as i64)
    };
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        ys,
        month_from_time(t) as i64 + 1,
        date_from_time(t) as i64,
        hour(t) as i64,
        min_from_time(t) as i64,
        sec_from_time(t) as i64,
        ms_from_time(t) as i64
    )
}

// ---------------------------------------------------------------- the API

fn date_ctor(rt: &mut Realm, c: &Call) -> JsResult {
    if !c.is_construct() {
        let now = rt.host.now_ms();
        return Ok(Value::str(&to_date_string(rt, now)));
    }
    let t = match c.args.len() {
        0 => rt.host.now_ms(),
        1 => {
            let v = match &c.args[0] {
                Value::Object(o) => match rt.heap.get(*o).kind {
                    Kind::Date(t) => Value::Number(t),
                    _ => rt.to_primitive(&c.args[0], None)?,
                },
                v => v.clone(),
            };
            match v {
                Value::String(s) => parse_date(rt, &s.to_rust()),
                v => rt.to_number(&v)?,
            }
        }
        _ => {
            let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
            for (i, a) in c.args.iter().take(7).enumerate() {
                n[i] = rt.to_number(a)?;
            }
            let mut y = n[0];
            if !y.is_nan() {
                let yi = numconv::to_integer(y);
                if (0.0..=99.0).contains(&yi) {
                    y = 1900.0 + yi;
                }
            }
            let local_t = make_date(make_day(y, n[1], n[2]), make_time(n[3], n[4], n[5], n[6]));
            utc(rt, local_t)
        }
    };
    let dp = rt.intr.date_proto;
    let proto = rt.proto_from_ctor(&c.new_target, dp)?;
    Ok(Value::Object(rt.alloc(Obj::new(Some(proto), Kind::Date(time_clip(t))))))
}

fn now(rt: &mut Realm, _c: &Call) -> JsResult {
    Ok(Value::Number(libm::floor(rt.host.now_ms())))
}

fn parse(rt: &mut Realm, c: &Call) -> JsResult {
    let s = rt.to_rust_string(&c.arg(0))?;
    Ok(Value::Number(parse_date(rt, &s)))
}

fn date_utc(rt: &mut Realm, c: &Call) -> JsResult {
    let mut n = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (i, a) in c.args.iter().take(7).enumerate() {
        n[i] = rt.to_number(a)?;
    }
    let mut y = n[0];
    if !y.is_nan() {
        let yi = numconv::to_integer(y);
        if (0.0..=99.0).contains(&yi) {
            y = 1900.0 + yi;
        }
    }
    Ok(Value::Number(time_clip(make_date(make_day(y, n[1], n[2]), make_time(n[3], n[4], n[5], n[6])))))
}

/// A getter: `f` of local (or UTC) time.
fn get_part(rt: &mut Realm, c: &Call, is_local: bool, f: fn(f64) -> f64) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    let t = if is_local { local(rt, t) } else { t };
    Ok(Value::Number(f(t)))
}

macro_rules! getters {
    ($($name:ident, $local:expr, $f:expr;)*) => {
        $(fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            get_part(rt, c, $local, $f)
        })*
    };
}

getters! {
    get_full_year, true, year_from_time;
    get_utc_full_year, false, year_from_time;
    get_month, true, month_from_time;
    get_utc_month, false, month_from_time;
    get_date, true, date_from_time;
    get_utc_date, false, date_from_time;
    get_day, true, week_day;
    get_utc_day, false, week_day;
    get_hours, true, hour;
    get_utc_hours, false, hour;
    get_minutes, true, min_from_time;
    get_utc_minutes, false, min_from_time;
    get_seconds, true, sec_from_time;
    get_utc_seconds, false, sec_from_time;
    get_milliseconds, true, ms_from_time;
    get_utc_milliseconds, false, ms_from_time;
}

fn get_year(rt: &mut Realm, c: &Call) -> JsResult {
    get_part(rt, c, true, |t| year_from_time(t) - 1900.0)
}

fn get_time(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Number(this_time(rt, c)?))
}

fn get_timezone_offset(rt: &mut Realm, c: &Call) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Ok(Value::Number(f64::NAN));
    }
    Ok(Value::Number(offset_ms(rt, t) / 60_000.0))
}

fn set_time(rt: &mut Realm, c: &Call) -> JsResult {
    this_time(rt, c)?;
    let t = rt.to_number(&c.arg(0))?;
    set_this_time(rt, c, t)
}

/// Setters: which field starts the argument list (0 ms … 6 year) and how many follow.
fn set_parts(rt: &mut Realm, c: &Call, is_local: bool, first: usize, max: usize) -> JsResult {
    let t0 = this_time(rt, c)?;
    let mut args = Vec::new();
    for a in c.args.iter().take(max.max(1)) {
        args.push(rt.to_number(a)?);
    }
    if args.is_empty() {
        args.push(f64::NAN);
    }
    let t = if t0.is_nan() {
        if first == 6 {
            0.0
        } else {
            return Ok(Value::Number(f64::NAN));
        }
    } else if is_local {
        local(rt, t0)
    } else {
        t0
    };
    // Fields: year, month, date, hours, minutes, seconds, ms.
    let mut f = [
        year_from_time(t),
        month_from_time(t),
        date_from_time(t),
        hour(t),
        min_from_time(t),
        sec_from_time(t),
        ms_from_time(t),
    ];
    // `first` indexes into f in order year=0 … ms=6.
    for (i, v) in args.into_iter().enumerate() {
        if first + i < 7 {
            f[first + i] = v;
        }
    }
    let nt = make_date(make_day(f[0], f[1], f[2]), make_time(f[3], f[4], f[5], f[6]));
    let nt = if is_local { utc(rt, nt) } else { nt };
    set_this_time(rt, c, nt)
}

macro_rules! setters {
    ($($name:ident, $local:expr, $first:expr, $max:expr;)*) => {
        $(fn $name(rt: &mut Realm, c: &Call) -> JsResult {
            set_parts(rt, c, $local, $first, $max)
        })*
    };
}

setters! {
    set_full_year, true, 0, 3;
    set_utc_full_year, false, 0, 3;
    set_month, true, 1, 2;
    set_utc_month, false, 1, 2;
    set_date, true, 2, 1;
    set_utc_date, false, 2, 1;
    set_hours, true, 3, 4;
    set_utc_hours, false, 3, 4;
    set_minutes, true, 4, 3;
    set_utc_minutes, false, 4, 3;
    set_seconds, true, 5, 2;
    set_utc_seconds, false, 5, 2;
    set_milliseconds, true, 6, 1;
    set_utc_milliseconds, false, 6, 1;
}

fn to_string(rt: &mut Realm, c: &Call) -> JsResult {
    let t = this_time(rt, c)?;
    Ok(Value::str(&to_date_string(rt, t)))
}

fn to_date_only(rt: &mut Realm, c: &Call) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Ok(Value::str("Invalid Date"));
    }
    let l = local(rt, t);
    Ok(Value::str(&date_part(l)))
}

fn to_time_only(rt: &mut Realm, c: &Call) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Ok(Value::str("Invalid Date"));
    }
    let l = local(rt, t);
    let tz = tz_string(rt, t);
    Ok(Value::str(&format!("{} {}", time_part(l), tz)))
}

fn to_utc_string(rt: &mut Realm, c: &Call) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Ok(Value::str("Invalid Date"));
    }
    Ok(Value::str(&format!(
        "{}, {:02} {} {} {} GMT",
        DAYS[week_day(t) as usize],
        date_from_time(t) as i64,
        MONTHS[month_from_time(t) as usize],
        fmt_year(year_from_time(t)),
        time_part(t)
    )))
}

fn to_iso_string(rt: &mut Realm, c: &Call) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Err(rt.range_error("Invalid time value"));
    }
    Ok(Value::str(&iso_string(t)))
}

fn to_json(rt: &mut Realm, c: &Call) -> JsResult {
    let o = rt.to_object(&c.this)?;
    let tv = rt.to_primitive(&Value::Object(o), Some("number"))?;
    if let Value::Number(n) = tv {
        if !n.is_finite() {
            return Ok(Value::Null);
        }
    }
    let f = rt.get(o, &key("toISOString"), Value::Object(o))?;
    rt.call(&f, Value::Object(o), &[])
}

/// en-US style: "3/1/2022, 10:00:00 AM".
fn locale_parts(rt: &mut Realm, c: &Call, date: bool, time: bool) -> JsResult {
    let t = this_time(rt, c)?;
    if t.is_nan() {
        return Ok(Value::str("Invalid Date"));
    }
    let l = local(rt, t);
    let mut out = String::new();
    if date {
        out.push_str(&format!(
            "{}/{}/{}",
            month_from_time(l) as i64 + 1,
            date_from_time(l) as i64,
            year_from_time(l) as i64
        ));
    }
    if time {
        if date {
            out.push_str(", ");
        }
        let h = hour(l) as i64;
        let (h12, ampm) = match h {
            0 => (12, "AM"),
            1..=11 => (h, "AM"),
            12 => (12, "PM"),
            _ => (h - 12, "PM"),
        };
        out.push_str(&format!("{}:{:02}:{:02} {}", h12, min_from_time(l) as i64, sec_from_time(l) as i64, ampm));
    }
    Ok(Value::str(&out))
}

fn to_locale_string(rt: &mut Realm, c: &Call) -> JsResult {
    locale_parts(rt, c, true, true)
}

fn to_locale_date_string(rt: &mut Realm, c: &Call) -> JsResult {
    // { month: 'long' } style options are common; honour weekday/month names.
    if let Value::Object(opts) = c.arg(1) {
        let month = rt.get(opts, &key("month"), c.arg(1))?;
        if let Value::String(m) = month {
            if m.eq_str("long") || m.eq_str("short") {
                let t = this_time(rt, c)?;
                if t.is_nan() {
                    return Ok(Value::str("Invalid Date"));
                }
                let l = local(rt, t);
                let mi = month_from_time(l) as usize;
                let name = if m.eq_str("long") { MONTH_NAMES[mi] } else { MONTHS[mi] };
                return Ok(Value::str(&format!("{} {}, {}", name, date_from_time(l) as i64, year_from_time(l) as i64)));
            }
        }
    }
    locale_parts(rt, c, true, false)
}

fn to_locale_time_string(rt: &mut Realm, c: &Call) -> JsResult {
    locale_parts(rt, c, false, true)
}

fn to_primitive(rt: &mut Realm, c: &Call) -> JsResult {
    let Value::Object(o) = c.this else {
        return Err(rt.type_error("Date.prototype[Symbol.toPrimitive] called on non-object"));
    };
    let hint = rt.to_rust_string(&c.arg(0))?;
    let order = match hint.as_str() {
        "string" | "default" => ["toString", "valueOf"],
        "number" => ["valueOf", "toString"],
        _ => return Err(rt.type_error("Invalid hint")),
    };
    for m in order {
        let f = rt.get(o, &key(m), c.this.clone())?;
        if rt.is_callable(&f) {
            let r = rt.call(&f, c.this.clone(), &[])?;
            if !matches!(r, Value::Object(_)) {
                return Ok(r);
            }
        }
    }
    Err(rt.type_error("Cannot convert object to primitive value"))
}

pub fn init(rt: &mut Realm) {
    let op = rt.intr.object_proto;
    let proto = rt.new_object_with(Some(op));
    rt.intr.date_proto = proto;
    let ctor = constructor(rt, "Date", 7, date_ctor, proto);
    rt.method(ctor, "now", 0, now);
    rt.method(ctor, "parse", 1, parse);
    rt.method(ctor, "UTC", 7, date_utc);
    for (name, len, f) in [
        ("getFullYear", 0, get_full_year as NativeFn),
        ("getUTCFullYear", 0, get_utc_full_year),
        ("getYear", 0, get_year),
        ("getMonth", 0, get_month),
        ("getUTCMonth", 0, get_utc_month),
        ("getDate", 0, get_date),
        ("getUTCDate", 0, get_utc_date),
        ("getDay", 0, get_day),
        ("getUTCDay", 0, get_utc_day),
        ("getHours", 0, get_hours),
        ("getUTCHours", 0, get_utc_hours),
        ("getMinutes", 0, get_minutes),
        ("getUTCMinutes", 0, get_utc_minutes),
        ("getSeconds", 0, get_seconds),
        ("getUTCSeconds", 0, get_utc_seconds),
        ("getMilliseconds", 0, get_milliseconds),
        ("getUTCMilliseconds", 0, get_utc_milliseconds),
        ("getTime", 0, get_time),
        ("valueOf", 0, get_time),
        ("getTimezoneOffset", 0, get_timezone_offset),
        ("setTime", 1, set_time),
        ("setFullYear", 3, set_full_year),
        ("setUTCFullYear", 3, set_utc_full_year),
        ("setMonth", 2, set_month),
        ("setUTCMonth", 2, set_utc_month),
        ("setDate", 1, set_date),
        ("setUTCDate", 1, set_utc_date),
        ("setHours", 4, set_hours),
        ("setUTCHours", 4, set_utc_hours),
        ("setMinutes", 3, set_minutes),
        ("setUTCMinutes", 3, set_utc_minutes),
        ("setSeconds", 2, set_seconds),
        ("setUTCSeconds", 2, set_utc_seconds),
        ("setMilliseconds", 1, set_milliseconds),
        ("setUTCMilliseconds", 1, set_utc_milliseconds),
        ("toString", 0, to_string),
        ("toDateString", 0, to_date_only),
        ("toTimeString", 0, to_time_only),
        ("toISOString", 0, to_iso_string),
        ("toJSON", 1, to_json),
        ("toLocaleString", 0, to_locale_string),
        ("toLocaleDateString", 0, to_locale_date_string),
        ("toLocaleTimeString", 0, to_locale_time_string),
    ] {
        rt.method(proto, name, len, f);
    }
    let utc_s = rt.method(proto, "toUTCString", 0, to_utc_string);
    rt.define(proto, "toGMTString", Value::Object(utc_s), HIDDEN);
    let tp = rt.native("[Symbol.toPrimitive]", 1, to_primitive, false);
    rt.define(proto, Sym::TO_PRIMITIVE, Value::Object(tp), CONFIGURABLE);
}
