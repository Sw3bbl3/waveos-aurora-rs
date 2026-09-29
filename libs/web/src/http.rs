//! An HTTP/1.1 client over any byte stream (plain TCP, or TLS for https).
//!
//! One request per connection (`Connection: close`). Responses may have a
//! `Content-Length`, be chunked, or run until the connection closes.

use crate::url::Url;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

/// A connection: reads return 0 at end of stream.
pub trait Io {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize>;
    fn write_all(&mut self, data: &[u8]) -> Result<(), isize>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A system error (connecting, reading, TLS …): the errno.
    Io(isize),
    BadUrl,
    BadResponse,
    TooManyRedirects,
    UnsupportedScheme(String),
    TooLarge,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "error {}", e),
            Error::BadUrl => f.write_str("not a valid address"),
            Error::BadResponse => f.write_str("the server sent something that isn't HTTP"),
            Error::TooManyRedirects => f.write_str("too many redirects"),
            Error::UnsupportedScheme(s) => write!(f, "{}: addresses aren't supported", s),
            Error::TooLarge => f.write_str("the response is too large"),
        }
    }
}

pub const MAX_BODY: usize = 64 << 20;
pub const USER_AGENT: &str = "Mozilla/5.0 (WaveOS Aurora) Surf/0.6";

#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Where the response came from (after redirects).
    pub url: Url,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// The media type without parameters, lower-case ("text/html").
    pub fn content_type(&self) -> String {
        self.header("content-type").unwrap_or("").split(';').next().unwrap_or("").trim().to_ascii_lowercase()
    }

    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a Url,
    pub headers: Vec<(String, String)>,
    pub body: &'a [u8],
}

impl<'a> Request<'a> {
    pub fn get(url: &'a Url) -> Request<'a> {
        Request { method: "GET", url, headers: Vec::new(), body: &[] }
    }
}

struct Reader<'a> {
    io: &'a mut dyn Io,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

impl Reader<'_> {
    fn fill(&mut self) -> Result<bool, Error> {
        if self.eof {
            return Ok(false);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        let mut chunk = vec![0u8; 32 * 1024];
        let n = self.io.read(&mut chunk).map_err(Error::Io)?;
        if n == 0 {
            self.eof = true;
            return Ok(false);
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(true)
    }

    fn line(&mut self) -> Result<String, Error> {
        loop {
            if let Some(i) = self.buf[self.pos..].iter().position(|&b| b == b'\n') {
                let raw = &self.buf[self.pos..self.pos + i];
                let line = String::from_utf8_lossy(raw).trim_end_matches('\r').to_string();
                self.pos += i + 1;
                return Ok(line);
            }
            if self.buf.len() - self.pos > 64 * 1024 {
                return Err(Error::BadResponse);
            }
            if !self.fill()? {
                return Err(Error::BadResponse);
            }
        }
    }

    fn exact(&mut self, n: usize, out: &mut Vec<u8>) -> Result<(), Error> {
        while self.buf.len() - self.pos < n {
            if !self.fill()? {
                return Err(Error::BadResponse);
            }
        }
        out.extend_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(())
    }

    fn rest(&mut self, out: &mut Vec<u8>) -> Result<(), Error> {
        loop {
            out.extend_from_slice(&self.buf[self.pos..]);
            self.pos = self.buf.len();
            if out.len() > MAX_BODY {
                return Err(Error::TooLarge);
            }
            if !self.fill()? {
                return Ok(());
            }
        }
    }
}

/// Sends one request and reads the whole response.
pub fn send(io: &mut dyn Io, req: &Request) -> Result<Response, Error> {
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nAccept-Language: en\r\nConnection: close\r\n",
        req.method,
        if req.url.path.is_empty() { "/" } else { &req.url.path },
        req.url.authority(),
        USER_AGENT
    );
    for (k, v) in &req.headers {
        head.push_str(&format!("{}: {}\r\n", k, v));
    }
    if !req.body.is_empty() || req.method == "POST" {
        head.push_str(&format!("Content-Length: {}\r\n", req.body.len()));
    }
    head.push_str("\r\n");
    io.write_all(head.as_bytes()).map_err(Error::Io)?;
    if !req.body.is_empty() {
        io.write_all(req.body).map_err(Error::Io)?;
    }
    let mut r = Reader { io, buf: Vec::new(), pos: 0, eof: false };
    // Skip interim responses (100 Continue …).
    let (status, reason, headers) = loop {
        let status_line = r.line()?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        if !version.starts_with("HTTP/") {
            return Err(Error::BadResponse);
        }
        let status: u16 = parts.next().and_then(|s| s.parse().ok()).ok_or(Error::BadResponse)?;
        let reason = parts.next().unwrap_or("").to_string();
        let mut headers = Vec::new();
        loop {
            let line = r.line()?;
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':') {
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }
        if !(100..200).contains(&status) {
            break (status, reason, headers);
        }
    };
    let find = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.clone());
    let mut body = Vec::new();
    let no_body = req.method == "HEAD" || status == 204 || status == 304;
    if !no_body {
        if find("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
            loop {
                let size_line = r.line()?;
                let hex = size_line.split(';').next().unwrap_or("").trim();
                let size = usize::from_str_radix(hex, 16).map_err(|_| Error::BadResponse)?;
                if size == 0 {
                    // Trailers until the empty line.
                    while !r.line().unwrap_or_default().is_empty() {}
                    break;
                }
                if body.len() + size > MAX_BODY {
                    return Err(Error::TooLarge);
                }
                r.exact(size, &mut body)?;
                r.line()?;
            }
        } else if let Some(len) = find("content-length").and_then(|v| v.parse::<usize>().ok()) {
            if len > MAX_BODY {
                return Err(Error::TooLarge);
            }
            r.exact(len, &mut body)?;
        } else {
            r.rest(&mut body)?;
        }
    }
    Ok(Response { status, reason, headers, body, url: req.url.clone() })
}

/// Opens connections for [`fetch`]: plain TCP for http, TLS for https.
pub type Connector<'a> = dyn FnMut(&Url) -> Result<Box<dyn Io>, Error> + 'a;

/// GETs `url`, following up to five redirects.
pub fn fetch(url: &Url, connect: &mut Connector) -> Result<Response, Error> {
    let mut url = url.clone();
    for _ in 0..6 {
        if url.scheme != "http" && url.scheme != "https" {
            return Err(Error::UnsupportedScheme(url.scheme.clone()));
        }
        let mut io = connect(&url)?;
        let mut target = url.clone();
        target.fragment = None;
        let resp = send(io.as_mut(), &Request::get(&target))?;
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(next) = resp.header("location").and_then(|l| url.join(l)) {
                url = next;
                continue;
            }
        }
        let mut resp = resp;
        resp.url = url;
        return Ok(resp);
    }
    Err(Error::TooManyRedirects)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays a canned response, a few bytes at a time.
    struct Canned {
        data: Vec<u8>,
        pos: usize,
        sent: Vec<u8>,
    }

    impl Io for Canned {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
            let n = (self.data.len() - self.pos).min(buf.len()).min(7);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
        fn write_all(&mut self, data: &[u8]) -> Result<(), isize> {
            self.sent.extend_from_slice(data);
            Ok(())
        }
    }

    fn run(raw: &str) -> Response {
        let url = Url::parse("http://example.com/x").unwrap();
        let mut io = Canned { data: raw.as_bytes().to_vec(), pos: 0, sent: Vec::new() };
        let r = send(&mut io, &Request::get(&url)).unwrap();
        let req = String::from_utf8(io.sent).unwrap();
        assert!(req.starts_with("GET /x HTTP/1.1\r\nHost: example.com\r\n"));
        r
    }

    #[test]
    fn content_length() {
        let r = run("HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: 5\r\n\r\nhelloEXTRA");
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello");
        assert_eq!(r.content_type(), "text/html");
    }

    #[test]
    fn chunked() {
        let r = run("HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n6;x=y\r\npedia \r\n0\r\nTrailer: 1\r\n\r\n");
        assert_eq!(r.body, b"Wikipedia ");
    }

    #[test]
    fn until_close_and_interim() {
        let r = run("HTTP/1.1 100 Continue\r\n\r\nHTTP/1.0 404 Not Found\r\nServer: x\r\n\r\ngone");
        assert_eq!(r.status, 404);
        assert_eq!(r.reason, "Not Found");
        assert_eq!(r.header("SERVER"), Some("x"));
        assert_eq!(r.body, b"gone");
    }

    #[test]
    fn follows_redirects() {
        let mut hops = 0;
        let mut connect = |u: &Url| -> Result<Box<dyn Io>, Error> {
            hops += 1;
            let raw = if u.path == "/start" {
                "HTTP/1.1 302 Found\r\nLocation: /end\r\nContent-Length: 0\r\n\r\n"
            } else {
                "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"
            };
            Ok(Box::new(Canned { data: raw.as_bytes().to_vec(), pos: 0, sent: Vec::new() }))
        };
        let r = fetch(&Url::parse("http://h/start").unwrap(), &mut connect).unwrap();
        assert_eq!(r.body, b"ok");
        assert_eq!(r.url.to_string(), "http://h/end");
        assert_eq!(hops, 2);
    }
}
