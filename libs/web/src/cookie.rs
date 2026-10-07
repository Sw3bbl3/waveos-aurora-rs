//! Cookies, as RFC 6265 describes them: a jar that takes `Set-Cookie`
//! headers (and `document.cookie` writes) and says which cookies go with a
//! request. Times are seconds since 1970; callers pass the current time.

use crate::url::Url;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// At most this many cookies per domain, and in all.
const PER_DOMAIN: usize = 50;
const TOTAL: usize = 3000;
/// A cookie's name and value together may be at most this long.
const MAX_SIZE: usize = 4096;

#[derive(Clone, Debug, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Lower case, without a leading dot.
    pub domain: String,
    /// Set without a Domain attribute: only for exactly this host.
    pub host_only: bool,
    pub path: String,
    /// When it expires (None: when the browser quits).
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    /// Order of creation (older cookies come first in a Cookie header).
    created: u64,
}

#[derive(Default)]
pub struct Jar {
    cookies: Vec<Cookie>,
    next: u64,
    /// Set when a persistent cookie changes (so it's worth saving).
    pub dirty: bool,
}

/// Where a cookie comes from, or is going to.
#[derive(Clone, Copy, PartialEq)]
pub enum Source {
    /// An HTTP response or request.
    Http,
    /// `document.cookie`: it can't see or set HttpOnly cookies.
    Script,
}

fn is_secure(url: &Url) -> bool {
    url.scheme == "https" || url.host == "localhost" || url.host == "127.0.0.1"
}

fn is_ip(host: &str) -> bool {
    host.starts_with('[') || (!host.is_empty() && host.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

fn domain_matches(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
            && !is_ip(host))
}

/// The request path, without its query.
fn request_path(url: &Url) -> &str {
    let p = url.path.split('?').next().unwrap_or("/");
    if p.is_empty() {
        "/"
    } else {
        p
    }
}

fn path_matches(request: &str, cookie: &str) -> bool {
    request == cookie
        || (request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/')))
}

/// The directory of the request path: "/a/b/c" → "/a/b".
fn default_path(url: &Url) -> String {
    let p = request_path(url);
    match p.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => p[..i].to_string(),
    }
}

impl Jar {
    pub const fn new() -> Jar {
        Jar { cookies: Vec::new(), next: 0, dirty: false }
    }

    /// Stores a cookie from a `Set-Cookie` header or a `document.cookie`
    /// write. Returns whether it was accepted.
    pub fn set(&mut self, url: &Url, line: &str, now: i64, source: Source) -> bool {
        if url.scheme != "http" && url.scheme != "https" {
            return false;
        }
        let mut parts = line.split(';');
        let pair = parts.next().unwrap_or("");
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n.trim(), v.trim()),
            None => ("", pair.trim()),
        };
        if (name.is_empty() && value.is_empty()) || name.len() + value.len() > MAX_SIZE {
            return false;
        }
        if name.bytes().chain(value.bytes()).any(|b| b < 0x20 && b != b'\t' || b == 0x7f) {
            return false;
        }
        let mut c = Cookie {
            name: name.to_string(),
            value: value.to_string(),
            domain: url.host.clone(),
            host_only: true,
            path: default_path(url),
            expires: None,
            secure: false,
            http_only: false,
            created: 0,
        };
        let mut max_age: Option<i64> = None;
        let mut expires: Option<i64> = None;
        for attr in parts {
            let (k, v) = match attr.split_once('=') {
                Some((k, v)) => (k.trim(), v.trim()),
                None => (attr.trim(), ""),
            };
            match k.to_ascii_lowercase().as_str() {
                "expires" => {
                    if let Some(t) = parse_date(v) {
                        expires = Some(t);
                    }
                }
                "max-age" => {
                    let digits = v.strip_prefix('-').unwrap_or(v);
                    if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                        let n = digits.parse::<i64>().unwrap_or(i64::MAX / 4).min(400 * 86400);
                        max_age = Some(if v.starts_with('-') { -1 } else { n });
                    }
                }
                "domain" => {
                    let d = v.trim_start_matches('.').to_ascii_lowercase();
                    if !d.is_empty() {
                        if !domain_matches(&url.host, &d) {
                            return false;
                        }
                        // Not for a whole top-level domain ("com").
                        if !d.contains('.') && d != url.host {
                            return false;
                        }
                        c.domain = d;
                        c.host_only = false;
                    }
                }
                "path" => {
                    if v.starts_with('/') {
                        c.path = v.to_string();
                    }
                }
                "secure" => c.secure = true,
                "httponly" => c.http_only = true,
                _ => {}
            }
        }
        c.expires = match max_age {
            Some(n) if n <= 0 => Some(i64::MIN),
            Some(n) => Some(now + n),
            None => expires,
        };
        if c.secure && !is_secure(url) {
            return false;
        }
        if source == Source::Script && c.http_only {
            return false;
        }
        if (c.name.starts_with("__Secure-") || c.name.starts_with("__Host-")) && !c.secure {
            return false;
        }
        if c.name.starts_with("__Host-") && (!c.host_only || c.path != "/") {
            return false;
        }
        let same =
            |o: &Cookie| o.name == c.name && o.domain == c.domain && o.host_only == c.host_only && o.path == c.path;
        // A cookie set over http can't replace a secure one.
        if !is_secure(url)
            && self.cookies.iter().any(|o| o.name == c.name && o.secure && domain_matches(&c.domain, &o.domain))
        {
            return false;
        }
        if let Some(i) = self.cookies.iter().position(same) {
            let old = self.cookies.remove(i);
            if source == Source::Script && old.http_only {
                self.cookies.insert(i, old);
                return false;
            }
            c.created = old.created;
            if old.expires.is_some() {
                self.dirty = true;
            }
        } else {
            c.created = self.next;
            self.next += 1;
        }
        if c.expires.is_some_and(|t| t <= now) {
            // Expired already: this deletes it.
            return true;
        }
        if c.expires.is_some() {
            self.dirty = true;
        }
        let domain = c.domain.clone();
        self.cookies.push(c);
        self.evict(&domain, now);
        true
    }

    fn evict(&mut self, domain: &str, now: i64) {
        self.remove_expired(now);
        while self.cookies.iter().filter(|c| c.domain == domain).count() > PER_DOMAIN {
            let i = self.oldest(|c| c.domain == domain);
            self.cookies.remove(i);
        }
        while self.cookies.len() > TOTAL {
            let i = self.oldest(|_| true);
            self.cookies.remove(i);
        }
    }

    fn oldest(&self, f: impl Fn(&Cookie) -> bool) -> usize {
        let mut best = 0;
        let mut at = u64::MAX;
        for (i, c) in self.cookies.iter().enumerate() {
            if f(c) && c.created < at {
                at = c.created;
                best = i;
            }
        }
        best
    }

    /// Drops cookies whose time is up.
    pub fn remove_expired(&mut self, now: i64) {
        let before = self.cookies.len();
        self.cookies.retain(|c| c.expires.is_none_or(|t| t > now));
        if self.cookies.len() != before {
            self.dirty = true;
        }
    }

    /// The cookies that go with a request to `url`, longest path first.
    pub fn matching(&self, url: &Url, now: i64, source: Source) -> Vec<&Cookie> {
        if url.scheme != "http" && url.scheme != "https" {
            return Vec::new();
        }
        let path = request_path(url);
        let secure = is_secure(url);
        let mut v: Vec<&Cookie> = self
            .cookies
            .iter()
            .filter(|c| {
                (if c.host_only { url.host == c.domain } else { domain_matches(&url.host, &c.domain) })
                    && path_matches(path, &c.path)
                    && (!c.secure || secure)
                    && (source == Source::Http || !c.http_only)
                    && c.expires.is_none_or(|t| t > now)
            })
            .collect();
        v.sort_by(|a, b| b.path.len().cmp(&a.path.len()).then(a.created.cmp(&b.created)));
        v
    }

    /// The `Cookie` header for a request (or `document.cookie`'s value).
    pub fn header(&self, url: &Url, now: i64, source: Source) -> String {
        let mut s = String::new();
        for c in self.matching(url, now, source) {
            if !s.is_empty() {
                s.push_str("; ");
            }
            if !c.name.is_empty() {
                s.push_str(&c.name);
                s.push('=');
            }
            s.push_str(&c.value);
        }
        s
    }

    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    /// Forgets every cookie.
    pub fn clear(&mut self) {
        self.dirty = self.cookies.iter().any(|c| c.expires.is_some());
        self.cookies.clear();
    }

    /// The persistent cookies, one per line, for saving.
    pub fn save(&self) -> String {
        let mut s = String::new();
        for c in &self.cookies {
            let Some(t) = c.expires else { continue };
            s.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                c.domain, c.host_only as u8, c.path, t, c.secure as u8, c.http_only as u8, c.name, c.value
            ));
        }
        s
    }

    /// Reads cookies saved by [`Jar::save`] (dropping any that have expired).
    pub fn load(text: &str, now: i64) -> Jar {
        let mut jar = Jar::new();
        for line in text.lines() {
            let f: Vec<&str> = line.splitn(8, '\t').collect();
            if f.len() != 8 {
                continue;
            }
            let Ok(t) = f[3].parse::<i64>() else { continue };
            if t <= now {
                continue;
            }
            jar.cookies.push(Cookie {
                domain: f[0].to_string(),
                host_only: f[1] == "1",
                path: f[2].to_string(),
                expires: Some(t),
                secure: f[4] == "1",
                http_only: f[5] == "1",
                name: f[6].to_string(),
                value: f[7].to_string(),
                created: jar.next,
            });
            jar.next += 1;
        }
        jar
    }
}

/// Days from 1970-01-01 to a date in the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Parses a cookie date the forgiving way RFC 6265 (section 5.1.1)
/// describes: "Wed, 21 Oct 2015 07:28:00 GMT", "Wednesday, 21-Oct-15
/// 07:28:00 GMT", "Wed Oct 21 07:28:00 2015" and the like.
pub fn parse_date(s: &str) -> Option<i64> {
    let delim = |c: char| {
        c == '\t'
            || (' '..='/').contains(&c)
            || (';'..='@').contains(&c)
            || ('['..='`').contains(&c)
            || ('{'..='~').contains(&c)
    };
    let (mut time, mut day, mut month, mut year) = (None, None, None, None);
    for tok in s.split(delim).filter(|t| !t.is_empty()) {
        let lead = |n: usize| tok.bytes().take_while(u8::is_ascii_digit).count() == n;
        if time.is_none() {
            let f: Vec<&str> = tok.split(':').collect();
            if f.len() == 3 {
                let num = |x: &str| {
                    let d: String = x.chars().take_while(char::is_ascii_digit).collect();
                    (!d.is_empty() && d.len() <= 2).then(|| d.parse::<i64>().ok()).flatten()
                };
                if let (Some(h), Some(m), Some(sec)) = (num(f[0]), num(f[1]), num(f[2])) {
                    time = Some((h, m, sec));
                    continue;
                }
            }
        }
        if day.is_none() && (lead(1) || lead(2)) {
            let d: String = tok.chars().take_while(char::is_ascii_digit).collect();
            day = d.parse::<i64>().ok();
            continue;
        }
        if month.is_none() && tok.len() >= 3 {
            let m = tok[..3].to_ascii_lowercase();
            let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
            if let Some(i) = months.iter().position(|x| *x == m) {
                month = Some(i as i64 + 1);
                continue;
            }
        }
        if year.is_none() && (2..=4).any(lead) {
            let d: String = tok.chars().take_while(char::is_ascii_digit).collect();
            year = d.parse::<i64>().ok();
            continue;
        }
    }
    let (h, mi, sec) = time?;
    let (d, m, mut y) = (day?, month?, year?);
    if (70..=99).contains(&y) {
        y += 1900;
    } else if (0..=69).contains(&y) {
        y += 2000;
    }
    if !(1..=31).contains(&d) || y < 1601 || h > 23 || mi > 59 || sec > 59 {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86400 + h * 3600 + mi * 60 + sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn dates() {
        assert_eq!(parse_date("Wed, 21 Oct 2015 07:28:00 GMT"), Some(1445412480));
        assert_eq!(parse_date("Wednesday, 21-Oct-15 07:28:00 GMT"), Some(1445412480));
        assert_eq!(parse_date("Wed Oct 21 07:28:00 2015"), Some(1445412480));
        assert_eq!(parse_date("Thu, 01 Jan 1970 00:00:00 GMT"), Some(0));
        assert_eq!(parse_date("nonsense"), None);
    }

    #[test]
    fn basics() {
        let mut j = Jar::new();
        let now = 1_000_000;
        let page = u("https://www.example.com/a/b.html?x=1");
        assert!(j.set(&page, "sid=abc; Path=/; HttpOnly", now, Source::Http));
        assert!(j.set(&page, "theme=dark", now, Source::Http));
        assert!(j.set(&page, "wide=1; Domain=.example.com; Path=/; Max-Age=60", now, Source::Http));
        assert!(!j.set(&page, "bad=1; Domain=other.com", now, Source::Http));
        assert!(!j.set(&page, "tld=1; Domain=com", now, Source::Http));
        // Longest path first, then oldest.
        assert_eq!(j.header(&page, now, Source::Http), "theme=dark; sid=abc; wide=1");
        assert_eq!(j.header(&page, now, Source::Script), "theme=dark; wide=1");
        assert_eq!(j.header(&u("https://www.example.com/"), now, Source::Http), "sid=abc; wide=1");
        assert_eq!(j.header(&u("https://api.example.com/x"), now, Source::Http), "wide=1");
        assert_eq!(j.header(&u("https://www.example.com/"), now + 61, Source::Http), "sid=abc");
        // Scripts can't overwrite HttpOnly cookies.
        assert!(!j.set(&page, "sid=evil; Path=/", now, Source::Script));
        // Expiring deletes.
        assert!(j.set(&page, "theme=; Max-Age=0", now, Source::Script));
        assert_eq!(j.header(&page, now, Source::Script), "wide=1");
    }

    #[test]
    fn secure_and_prefixes() {
        let mut j = Jar::new();
        let now = 0;
        assert!(!j.set(&u("http://example.com/"), "s=1; Secure", now, Source::Http));
        assert!(j.set(&u("https://example.com/"), "s=1; Secure", now, Source::Http));
        assert_eq!(j.header(&u("http://example.com/"), now, Source::Http), "");
        assert_eq!(j.header(&u("https://example.com/"), now, Source::Http), "s=1");
        assert!(!j.set(&u("https://example.com/"), "__Host-x=1; Secure; Path=/a", now, Source::Http));
        assert!(j.set(&u("https://example.com/"), "__Host-x=1; Secure; Path=/", now, Source::Http));
    }

    #[test]
    fn save_and_load() {
        let mut j = Jar::new();
        let page = u("https://example.com/");
        j.set(&page, "session=1", 0, Source::Http);
        j.set(&page, "keep=a\tb; Max-Age=100", 0, Source::Http);
        assert!(j.dirty);
        let saved = j.save();
        let k = Jar::load(&saved, 50);
        assert_eq!(k.header(&page, 50, Source::Http), "keep=a\tb");
        assert!(Jar::load(&saved, 200).is_empty());
    }
}
