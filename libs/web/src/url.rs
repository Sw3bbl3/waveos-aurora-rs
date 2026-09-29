//! URLs: parsing, resolving relative references (RFC 3986), percent-encoding.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Url {
    /// Lower-case: "http", "https", "file", "about", …
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    /// Path and query, always starting with '/' for http(s): "/a/b?x=1".
    pub path: String,
    pub fragment: Option<String>,
}

impl Url {
    pub fn parse(s: &str) -> Option<Url> {
        let s = s.trim();
        let colon = s.find(':')?;
        let scheme = s[..colon].to_ascii_lowercase();
        if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b)) {
            return None;
        }
        let rest = &s[colon + 1..];
        let (rest, fragment) = match rest.find('#') {
            Some(i) => (&rest[..i], Some(rest[i + 1..].to_string())),
            None => (rest, None),
        };
        let Some(after) = rest.strip_prefix("//") else {
            // "about:blank", "mailto:…": everything is the path.
            return Some(Url { scheme, host: String::new(), port: None, path: rest.to_string(), fragment });
        };
        let end = after.find(['/', '?']).unwrap_or(after.len());
        let authority = &after[..end];
        let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p.parse().ok()?)),
            Some((h, "")) => (h, None),
            _ => (authority, None),
        };
        let mut path = after[end..].to_string();
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        Some(Url { scheme, host: host.to_ascii_lowercase(), port, path: normalize(&path), fragment })
    }

    pub fn default_port(&self) -> u16 {
        match self.scheme.as_str() {
            "https" => 443,
            "ftp" => 21,
            _ => 80,
        }
    }

    pub fn port_or_default(&self) -> u16 {
        self.port.unwrap_or_else(|| self.default_port())
    }

    /// "host" or "host:port" when the port isn't the default (the Host header).
    pub fn authority(&self) -> String {
        match self.port {
            Some(p) if p != self.default_port() => format!("{}:{}", self.host, p),
            _ => self.host.clone(),
        }
    }

    /// Resolves `reference` (a link: absolute, "//host/x", "/x", "x", "?q", "#f") against this URL.
    pub fn join(&self, reference: &str) -> Option<Url> {
        let r = reference.trim();
        if r.is_empty() {
            let mut u = self.clone();
            u.fragment = None;
            return Some(u);
        }
        if let Some(colon) = r.find(':') {
            let scheme = &r[..colon];
            if !scheme.contains(['/', '?', '#']) && !scheme.is_empty() {
                return Url::parse(r);
            }
        }
        if let Some(rest) = r.strip_prefix("//") {
            return Url::parse(&format!("{}://{}", self.scheme, rest));
        }
        let mut u = self.clone();
        let (r, fragment) = match r.find('#') {
            Some(i) => (&r[..i], Some(r[i + 1..].to_string())),
            None => (r, None),
        };
        u.fragment = fragment;
        if r.is_empty() {
            return Some(u);
        }
        let base_path = self.path.split('?').next().unwrap_or("/");
        u.path = if r.starts_with('/') {
            normalize(r)
        } else if r.starts_with('?') {
            format!("{}{}", base_path, r)
        } else {
            let dir = &base_path[..base_path.rfind('/').map_or(0, |i| i + 1)];
            normalize(&format!("{}{}", dir, r))
        };
        Some(u)
    }
}

impl core::fmt::Display for Url {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        if self.host.is_empty() && !matches!(self.scheme.as_str(), "http" | "https" | "file") {
            write!(f, "{}:{}", self.scheme, self.path)?;
        } else {
            write!(f, "{}://{}{}", self.scheme, self.authority(), self.path)?;
        }
        if let Some(frag) = &self.fragment {
            write!(f, "#{}", frag)?;
        }
        Ok(())
    }
}

/// Removes "." and ".." segments (keeping the query).
fn normalize(path: &str) -> String {
    let (p, query) = match path.find('?') {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    };
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = p.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        let last = i + 1 == segments.len();
        match *seg {
            "." => {
                if last {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if last {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let mut joined = out.join("/");
    if !joined.starts_with('/') {
        joined.insert(0, '/');
    }
    joined + query
}

/// Decodes %XX escapes (and '+' as space when `form`).
pub fn percent_decode(s: &str, form: bool) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(core::str::from_utf8(&b[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' if form => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Percent-encodes everything but unreserved characters (for query values).
pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let u = Url::parse("HTTP://Example.COM:8080/a/b?x=1#top").unwrap();
        assert_eq!(u.scheme, "http");
        assert_eq!(u.host, "example.com");
        assert_eq!(u.port, Some(8080));
        assert_eq!(u.path, "/a/b?x=1");
        assert_eq!(u.fragment.as_deref(), Some("top"));
        assert_eq!(u.to_string(), "http://example.com:8080/a/b?x=1#top");
        let u = Url::parse("https://example.com").unwrap();
        assert_eq!(u.path, "/");
        assert_eq!(u.port_or_default(), 443);
        assert_eq!(Url::parse("about:blank").unwrap().to_string(), "about:blank");
        assert!(Url::parse("no scheme").is_none());
    }

    #[test]
    fn joins() {
        let base = Url::parse("http://a/b/c/d;p?q").unwrap();
        // RFC 3986 section 5.4.1 examples.
        let cases = [
            ("g", "http://a/b/c/g"),
            ("./g", "http://a/b/c/g"),
            ("g/", "http://a/b/c/g/"),
            ("/g", "http://a/g"),
            ("//g", "http://g/"),
            ("?y", "http://a/b/c/d;p?y"),
            ("g?y", "http://a/b/c/g?y"),
            ("#s", "http://a/b/c/d;p?q#s"),
            ("..", "http://a/b/"),
            ("../g", "http://a/b/g"),
            ("../..", "http://a/"),
            ("../../g", "http://a/g"),
            ("https://other/x", "https://other/x"),
        ];
        for (r, want) in cases {
            assert_eq!(base.join(r).unwrap().to_string(), want, "joining {r}");
        }
    }

    #[test]
    fn percent() {
        assert_eq!(percent_decode("a%20b+c", true), "a b c");
        assert_eq!(percent_decode("100%", false), "100%");
        assert_eq!(percent_encode("a b&c"), "a+b%26c");
    }
}
