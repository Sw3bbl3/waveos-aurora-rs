//! Whatever a server sends — status lines, headers, chunked and compressed
//! bodies, cookies, redirect targets — is an error or a value, never a panic.

use nebula_web::cookie::{parse_date, Jar, Source};
use nebula_web::http::{exchange, Io, Request};
use nebula_web::Url;

/// A response replayed a few bytes per read, as a slow network delivers it.
struct Replay<'a>(&'a [u8], usize);

impl Io for Replay<'_> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
        let n = buf.len().min(self.0.len()).min(7 + self.1 % 5);
        buf[..n].copy_from_slice(&self.0[..n]);
        self.0 = &self.0[n..];
        self.1 += 1;
        Ok(n)
    }
    fn write_all(&mut self, _: &[u8]) -> Result<(), isize> {
        Ok(())
    }
}

#[test]
fn corrupt_responses_never_panic() {
    // "hello", gzipped.
    let gzip =
        [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x07, 0, 0x86, 0xa6, 0x10, 0x36, 5, 0, 0, 0];
    let mut gzipped =
        format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", gzip.len()).into_bytes();
    gzipped.extend_from_slice(&gzip);
    let seeds = vec![
        b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Type: text/html; charset=utf-8\r\n\r\nhello".to_vec(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nTrailer: x\r\n\r\n"
            .to_vec(),
        b"HTTP/1.1 301 Moved Permanently\r\nLocation: /elsewhere?q=1\r\nSet-Cookie: a=b; Path=/\r\n\r\n".to_vec(),
        b"HTTP/1.0 200 OK\r\nConnection: close\r\n\r\nbody until the connection closes".to_vec(),
        gzipped,
    ];
    let url = Url::parse("http://example.com/").unwrap();
    aurora_mutate::check("http", &seeds, 4000, |response| {
        let _ = exchange(&mut Replay(response, 0), &Request::get(&url));
    });
}

#[test]
fn corrupt_urls_never_panic() {
    let seeds: Vec<Vec<u8>> = [
        "https://user:pw@example.com:8080/a/b/../c?q=1&r=%20#frag",
        "http://[::1]:80/x",
        "http://[2001:db8::1%25eth0]/",
        "file:///tmp/a%2Fb",
        "//cdn.example/x.js",
        "../../x?y#z",
        "https://xn--bcher-kva.example/%E2%82%AC",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    let base = Url::parse("https://example.com/dir/page.html?x=1").unwrap();
    aurora_mutate::check("url", &seeds, 4000, |bytes| {
        let s = String::from_utf8_lossy(bytes);
        if let Some(u) = Url::parse(&s) {
            let _ = (u.authority(), u.port_or_default(), u.join("../a?b#c"));
        }
        let _ = base.join(&s);
        let _ = nebula_web::url::percent_decode(&s, true);
    });
}

#[test]
fn corrupt_cookies_never_panic() {
    let seeds: Vec<Vec<u8>> = [
        "a=b; Path=/; Domain=example.com; Expires=Wed, 21 Oct 2026 07:28:00 GMT; Secure; HttpOnly; SameSite=Lax",
        "sid=1; Max-Age=3600",
        "x=y; expires=Sun, 06-Nov-94 08:49:37 GMT",
        "Wed Oct 21 07:28:00 2015",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    let url = Url::parse("https://www.example.com/a/b").unwrap();
    let now = 1_760_000_000;
    aurora_mutate::check("cookie", &seeds, 4000, |bytes| {
        let s = String::from_utf8_lossy(bytes);
        let _ = parse_date(&s);
        let mut jar = Jar::default();
        jar.set(&url, &s, now, Source::Http);
        let _ = jar.header(&url, now, Source::Script);
        let _ = Jar::load(&jar.save(), now);
        let _ = Jar::load(&s, now);
    });
}
