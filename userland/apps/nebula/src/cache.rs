//! An in-memory HTTP cache shared by every tab: responses kept as long as
//! their headers allow (Cache-Control max-age, Expires, or a heuristic from
//! Last-Modified), then revalidated with If-None-Match / If-Modified-Since
//! so a 304 reuses the stored body.

use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
use corekit::sync::Mutex;
use nebula_web::http::Response;
use nebula_web::Url;

/// Total bytes kept, and the most one response may take.
const BUDGET: usize = 48 << 20;
const MAX_ENTRY: usize = 6 << 20;

struct Entry {
    url: String,
    response: Arc<Response>,
    /// Uptime (ms) until which it's fresh.
    fresh_until: u64,
    etag: Option<String>,
    last_modified: Option<String>,
    used: u64,
}

static CACHE: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

/// Hits, revalidations (304s) and misses so far.
pub static STATS: Mutex<(u32, u32, u32)> = Mutex::new((0, 0, 0));

fn directive<'a>(cc: &'a str, name: &str) -> Option<&'a str> {
    cc.split(',').map(str::trim).find_map(|d| {
        let (k, v) = d.split_once('=').unwrap_or((d, ""));
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim().trim_matches('"'))
    })
}

/// Seconds since 1970 for an HTTP date ("Sun, 06 Nov 1994 08:49:37 GMT").
fn http_date(s: &str) -> Option<i64> {
    let parts: Vec<&str> = s.split_ascii_whitespace().collect();
    let [_, day, month, year, time, ..] = parts.as_slice() else { return None };
    let month = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"]
        .iter()
        .position(|m| m.eq_ignore_ascii_case(month))? as i64
        + 1;
    let t: Vec<i64> = time.split(':').filter_map(|x| x.parse().ok()).collect();
    let [h, m, sec] = t.as_slice() else { return None };
    let days = corekit::web::nebula_secure::der::days_from_civil(year.parse().ok()?, month, day.parse().ok()?);
    Some(days * 86400 + h * 3600 + m * 60 + sec)
}

/// How long (seconds) a response may be used without asking; None: never
/// store it.
fn lifetime(r: &Response) -> Option<u64> {
    let cc = r.header("cache-control").unwrap_or("");
    if directive(cc, "no-store").is_some() || r.header("vary").is_some_and(|v| v.trim() == "*") {
        return None;
    }
    if !matches!(r.status, 200 | 203 | 204 | 300 | 301 | 404 | 410) || r.body.len() > MAX_ENTRY {
        return None;
    }
    if directive(cc, "no-cache").is_some() {
        return Some(0);
    }
    if let Some(age) = directive(cc, "max-age").and_then(|v| v.parse::<u64>().ok()) {
        return Some(age);
    }
    let date = r.header("date").and_then(http_date).unwrap_or_else(corekit::web::unix_time);
    if let Some(expires) = r.header("expires") {
        return Some(http_date(expires).map_or(0, |e| (e - date).max(0) as u64));
    }
    // A tenth of the time since it last changed, up to a day.
    let modified = r.header("last-modified").and_then(http_date)?;
    Some((((date - modified).max(0) / 10) as u64).min(86_400))
}

/// The cached response for `url` if fresh, or the validators to ask with.
pub enum Lookup {
    Fresh(Arc<Response>),
    Stale(Vec<(String, String)>),
    Missing,
}

pub fn lookup(url: &Url) -> Lookup {
    let key = url.to_string();
    let now = corekit::time::uptime_ms();
    let mut cache = CACHE.lock();
    let Some(e) = cache.iter_mut().find(|e| e.url == key) else { return Lookup::Missing };
    e.used = now;
    if now < e.fresh_until {
        STATS.lock().0 += 1;
        return Lookup::Fresh(e.response.clone());
    }
    let mut headers = Vec::new();
    if let Some(t) = &e.etag {
        headers.push((String::from("If-None-Match"), t.clone()));
    }
    if let Some(t) = &e.last_modified {
        headers.push((String::from("If-Modified-Since"), t.clone()));
    }
    if headers.is_empty() {
        Lookup::Missing
    } else {
        Lookup::Stale(headers)
    }
}

/// A 304 for `url`: the stored response, fresh again.
pub fn revalidated(url: &Url, fresh: &Response) -> Option<Arc<Response>> {
    let key = url.to_string();
    let mut cache = CACHE.lock();
    let e = cache.iter_mut().find(|e| e.url == key)?;
    let secs = lifetime(fresh).unwrap_or(0);
    e.fresh_until = corekit::time::uptime_ms() + secs * 1000;
    STATS.lock().1 += 1;
    Some(e.response.clone())
}

/// Keeps a response fetched for `url`, if its headers allow.
pub fn store(url: &Url, r: &Response) -> Arc<Response> {
    let response = Arc::new(r.clone());
    STATS.lock().2 += 1;
    let Some(secs) = lifetime(r) else { return response };
    let etag = r.header("etag").map(String::from);
    let last_modified = r.header("last-modified").map(String::from);
    if secs == 0 && etag.is_none() && last_modified.is_none() {
        return response;
    }
    let now = corekit::time::uptime_ms();
    let key = url.to_string();
    let mut cache = CACHE.lock();
    cache.retain(|e| e.url != key);
    // Least recently used first out.
    let mut total: usize = cache.iter().map(|e| e.response.body.len()).sum();
    while total + r.body.len() > BUDGET && !cache.is_empty() {
        let oldest = (0..cache.len()).min_by_key(|i| cache[*i].used).unwrap();
        total -= cache.swap_remove(oldest).response.body.len();
    }
    cache.push(Entry {
        url: key,
        response: response.clone(),
        fresh_until: now + secs * 1000,
        etag,
        last_modified,
        used: now,
    });
    response
}
