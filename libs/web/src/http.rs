//! An HTTP/1.1 client over any byte stream (plain TCP, or TLS for https).
//!
//! Connections can be kept alive and reused through a [`Connect`] pool.
//! Responses may have a `Content-Length`, be chunked, or run until the
//! connection closes, and may be gzip- or deflate-compressed.

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

impl<T: Io + ?Sized> Io for Box<T> {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
        (**self).read(buf)
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), isize> {
        (**self).write_all(data)
    }
}

/// Opens (or reuses) connections for [`fetch`].
pub trait Connect {
    type Conn: Io;
    /// A connection to `url`'s host; `fresh` forbids reusing an idle one.
    /// Also returns whether it was reused.
    fn connect(&mut self, url: &Url, fresh: bool) -> Result<(Self::Conn, bool), Error>;
    /// Takes back a connection that can carry another request.
    fn release(&mut self, _url: &Url, _conn: Self::Conn) {}
    /// Whether connections are kept alive for reuse.
    fn pooled(&self) -> bool {
        false
    }
}

/// Any function that opens a connection is a (non-pooling) connector.
impl<F: FnMut(&Url) -> Result<Box<dyn Io>, Error>> Connect for F {
    type Conn = Box<dyn Io>;
    fn connect(&mut self, url: &Url, _fresh: bool) -> Result<(Box<dyn Io>, bool), Error> {
        Ok((self(url)?, false))
    }
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
    /// A secure connection couldn't be set up (TLS), with the reason.
    Secure(String),
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
            Error::Secure(why) => write!(f, "can't connect securely: {}", why),
        }
    }
}

pub const MAX_BODY: usize = 64 << 20;
pub const USER_AGENT: &str = "Mozilla/5.0 (WaveOS Aurora) Nebula/0.8.0";

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
    /// Ask to keep the connection open afterwards.
    pub keep_alive: bool,
}

impl<'a> Request<'a> {
    pub fn get(url: &'a Url) -> Request<'a> {
        Request { method: "GET", url, headers: Vec::new(), body: &[], keep_alive: false }
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
    exchange(io, req).map(|(r, _)| r)
}

/// Sends one request and reads the whole response; also says whether the
/// connection can carry another request.
pub fn exchange(io: &mut dyn Io, req: &Request) -> Result<(Response, bool), Error> {
    let mut head = format!(
        "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nAccept-Language: en\r\nAccept-Encoding: gzip, deflate\r\nConnection: {}\r\n",
        req.method,
        if req.url.path.is_empty() { "/" } else { &req.url.path },
        req.url.authority(),
        USER_AGENT,
        if req.keep_alive { "keep-alive" } else { "close" }
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
    let mut http11;
    let (status, reason, mut headers) = loop {
        let status_line = r.line()?;
        let mut parts = status_line.splitn(3, ' ');
        let version = parts.next().unwrap_or("");
        http11 = version != "HTTP/1.0";
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
    let mut framed = true;
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
            framed = false;
            r.rest(&mut body)?;
        }
    }
    let connection = find("connection").unwrap_or_default().to_ascii_lowercase();
    let reusable = req.keep_alive
        && framed
        && !connection.contains("close")
        && (http11 || connection.contains("keep-alive"))
        && r.pos == r.buf.len();
    // A 304 (or HEAD, or 204) names the encoding of a body it doesn't carry.
    if let Some(enc) = find("content-encoding").filter(|_| !body.is_empty()) {
        body = decode_body(body, &enc)?;
        headers.retain(|(k, _)| !k.eq_ignore_ascii_case("content-encoding"));
    }
    Ok((Response { status, reason, headers, body, url: req.url.clone() }, reusable))
}

/// Undoes a Content-Encoding (gzip, deflate).
fn decode_body(body: Vec<u8>, encoding: &str) -> Result<Vec<u8>, Error> {
    use miniz_oxide::inflate::{decompress_to_vec_with_limit, decompress_to_vec_zlib_with_limit};
    match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Ok(body),
        "gzip" | "x-gzip" => {
            let b = &body;
            if b.len() < 18 || b[0] != 0x1F || b[1] != 0x8B || b[2] != 8 {
                return Err(Error::BadResponse);
            }
            let flags = b[3];
            let mut i = 10;
            if flags & 4 != 0 {
                i += 2 + (b.get(i).copied().unwrap_or(0) as usize | (b.get(i + 1).copied().unwrap_or(0) as usize) << 8);
            }
            for bit in [8, 16] {
                if flags & bit != 0 {
                    while b.get(i).is_some_and(|c| *c != 0) {
                        i += 1;
                    }
                    i += 1;
                }
            }
            if flags & 2 != 0 {
                i += 2;
            }
            decompress_to_vec_with_limit(b.get(i..).ok_or(Error::BadResponse)?, MAX_BODY)
                .map_err(|_| Error::BadResponse)
        }
        "deflate" => decompress_to_vec_zlib_with_limit(&body, MAX_BODY)
            .or_else(|_| decompress_to_vec_with_limit(&body, MAX_BODY))
            .map_err(|_| Error::BadResponse),
        _ => Err(Error::BadResponse),
    }
}

/// What a streamed download got: the final response's status and headers
/// (its body went to the sink), and whether the body arrived whole.
#[derive(Clone, Debug)]
pub struct Streamed {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub url: Url,
    /// Bytes handed to the sink.
    pub received: u64,
    /// The body ended where the server said it would (false: the sink
    /// stopped it, or the connection broke off).
    pub complete: bool,
}

impl Streamed {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

/// Sees a streamed answer's status and headers before its body.
pub type OnHead<'a> = &'a mut dyn FnMut(u16, &[(String, String)]) -> bool;

/// GETs `url` and hands the body to `sink` as it arrives, without holding
/// it in memory, following up to five redirects. With `from` > 0 it asks
/// for the rest of the file from that byte (`Range: bytes=from-`): a server
/// that can answers 206, one that can't sends it all again with 200. The
/// body is never compressed in transit (`Accept-Encoding: identity`), so
/// byte offsets mean the same on both sides. `start` sees a successful
/// answer's status and headers before its body (to start over on a 200,
/// say) and `sink` each piece of the body; either returns false to stop
/// early (pausing or cancelling), and the connection is then dropped.
pub fn download<C: Connect + ?Sized>(
    url: &Url,
    from: u64,
    connect: &mut C,
    start: OnHead,
    sink: &mut dyn FnMut(&[u8]) -> bool,
) -> Result<Streamed, Error> {
    let mut url = url.clone();
    for _ in 0..6 {
        if url.scheme != "http" && url.scheme != "https" {
            return Err(Error::UnsupportedScheme(url.scheme.clone()));
        }
        let mut target = url.clone();
        target.fragment = None;
        let mut head = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: {}\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n",
            if target.path.is_empty() { "/" } else { &target.path },
            target.authority(),
            USER_AGENT
        );
        if from > 0 {
            head.push_str(&format!("Range: bytes={from}-\r\n"));
        }
        head.push_str("\r\n");
        let (mut conn, _) = connect.connect(&url, true)?;
        conn.write_all(head.as_bytes()).map_err(Error::Io)?;
        let mut r = Reader { io: &mut conn, buf: Vec::new(), pos: 0, eof: false };
        let (status, reason, headers) = loop {
            let status_line = r.line()?;
            let mut parts = status_line.splitn(3, ' ');
            if !parts.next().unwrap_or("").starts_with("HTTP/") {
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
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            if let Some(next) = find("location").and_then(|l| url.join(&l)) {
                url = next;
                continue;
            }
        }
        let mut out = Streamed { status, reason, headers: Vec::new(), url: url.clone(), received: 0, complete: false };
        if !(200..300).contains(&status) {
            out.headers = headers;
            return Ok(out);
        }
        if find("content-encoding").is_some_and(|e| !e.eq_ignore_ascii_case("identity")) {
            return Err(Error::BadResponse);
        }
        if !start(status, &headers) {
            out.headers = headers;
            return Ok(out);
        }
        // Hands what is buffered to the sink; false when it said stop.
        let mut give = |r: &mut Reader, n: usize, out: &mut Streamed| -> bool {
            let ok = sink(&r.buf[r.pos..r.pos + n]);
            r.pos += n;
            out.received += n as u64;
            ok
        };
        if find("transfer-encoding").is_some_and(|t| t.to_ascii_lowercase().contains("chunked")) {
            while let Ok(size_line) = r.line() {
                let hex = size_line.split(';').next().unwrap_or("").trim();
                let mut size = usize::from_str_radix(hex, 16).map_err(|_| Error::BadResponse)?;
                if size == 0 {
                    out.complete = true;
                    break;
                }
                while size > 0 {
                    if r.buf.len() == r.pos && !r.fill().unwrap_or(false) {
                        break;
                    }
                    let n = size.min(r.buf.len() - r.pos);
                    size -= n;
                    if !give(&mut r, n, &mut out) {
                        out.headers = headers;
                        return Ok(out);
                    }
                }
                if size > 0 || r.line().is_err() {
                    break;
                }
            }
        } else {
            let length = find("content-length").and_then(|v| v.parse::<u64>().ok());
            loop {
                if length.is_some_and(|l| out.received >= l) {
                    break;
                }
                if r.buf.len() == r.pos && !r.fill().unwrap_or(false) {
                    break;
                }
                let left = length.map_or(usize::MAX, |l| (l - out.received).min(usize::MAX as u64) as usize);
                let n = left.min(r.buf.len() - r.pos);
                if !give(&mut r, n, &mut out) {
                    out.headers = headers;
                    return Ok(out);
                }
            }
            // Without a length, the end of the connection ends the body.
            out.complete = length.is_none_or(|l| out.received == l);
        }
        out.headers = headers;
        return Ok(out);
    }
    Err(Error::TooManyRedirects)
}

/// GETs `url`, following up to five redirects.
pub fn fetch<C: Connect + ?Sized>(url: &Url, connect: &mut C) -> Result<Response, Error> {
    request("GET", url, &[], &[], connect)
}

/// Somewhere to keep cookies between requests.
pub trait Cookies {
    /// The `Cookie` header for a request to `url` (empty for none).
    fn header(&mut self, url: &Url) -> String;
    /// Takes a `Set-Cookie` header from `url`'s response.
    fn store(&mut self, url: &Url, set_cookie: &str);
}

/// No cookies: none sent, none kept.
pub struct NoCookies;

impl Cookies for NoCookies {
    fn header(&mut self, _: &Url) -> String {
        String::new()
    }
    fn store(&mut self, _: &Url, _: &str) {}
}

/// Sends a request, following up to five redirects as browsers do: 301,
/// 302 and 303 turn a POST into a GET; 307 and 308 repeat it.
pub fn request<C: Connect + ?Sized>(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
    connect: &mut C,
) -> Result<Response, Error> {
    request_with_cookies(method, url, headers, body, connect, &mut NoCookies)
}

/// [`request`], sending and keeping cookies at every step (redirects
/// often set them).
pub fn request_with_cookies<C: Connect + ?Sized, K: Cookies + ?Sized>(
    method: &str,
    url: &Url,
    headers: &[(String, String)],
    body: &[u8],
    connect: &mut C,
    cookies: &mut K,
) -> Result<Response, Error> {
    let mut url = url.clone();
    let mut method = String::from(method);
    let mut body = body;
    for _ in 0..6 {
        if url.scheme != "http" && url.scheme != "https" {
            return Err(Error::UnsupportedScheme(url.scheme.clone()));
        }
        let mut target = url.clone();
        target.fragment = None;
        let mut all = headers.to_vec();
        if !all.iter().any(|(k, _)| k.eq_ignore_ascii_case("cookie")) {
            let c = cookies.header(&url);
            if !c.is_empty() {
                all.push((String::from("Cookie"), c));
            }
        }
        let req = Request { method: &method, url: &target, headers: all, body, keep_alive: connect.pooled() };
        let (mut conn, reused) = connect.connect(&url, false)?;
        let (resp, reusable) = match exchange(&mut conn, &req) {
            Ok(r) => r,
            // An idle connection the server had already closed: use a new one.
            Err(Error::Io(_) | Error::BadResponse) if reused => {
                let (fresh, _) = connect.connect(&url, true)?;
                conn = fresh;
                exchange(&mut conn, &req)?
            }
            Err(e) => return Err(e),
        };
        if reusable {
            connect.release(&url, conn);
        }
        for (k, v) in &resp.headers {
            if k.eq_ignore_ascii_case("set-cookie") {
                cookies.store(&url, v);
            }
        }
        if matches!(resp.status, 301 | 302 | 303 | 307 | 308) {
            if let Some(next) = resp.header("location").and_then(|l| url.join(l)) {
                if matches!(resp.status, 301..=303) && method != "GET" && method != "HEAD" {
                    method = String::from("GET");
                    body = &[];
                }
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
    fn not_modified_with_an_encoding_has_no_body() {
        let r =
            run("HTTP/1.1 304 Not Modified\r\nContent-Encoding: gzip\r\nContent-Length: 1234\r\nETag: \"x\"\r\n\r\n");
        assert_eq!((r.status, r.body.len()), (304, 0));
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

    /// A pool over canned connections, counting how many were opened.
    struct Pool {
        responses: Vec<&'static str>,
        idle: Vec<Canned>,
        opened: usize,
    }

    impl Connect for Pool {
        type Conn = Canned;
        fn connect(&mut self, _url: &Url, fresh: bool) -> Result<(Canned, bool), Error> {
            if !fresh {
                if let Some(mut c) = self.idle.pop() {
                    // The next response arrives on the reused connection.
                    c.data.extend_from_slice(self.responses.remove(0).as_bytes());
                    return Ok((c, true));
                }
            }
            self.opened += 1;
            Ok((Canned { data: self.responses.remove(0).as_bytes().to_vec(), pos: 0, sent: Vec::new() }, false))
        }
        fn release(&mut self, _url: &Url, conn: Canned) {
            self.idle.push(conn);
        }
        fn pooled(&self) -> bool {
            true
        }
    }

    #[test]
    fn keep_alive_reuses_connections() {
        let mut pool = Pool {
            responses: alloc::vec![
                "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\na",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nb\r\n0\r\n\r\n",
                "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 1\r\n\r\nc",
                "HTTP/1.1 200 OK\r\nContent-Length: 1\r\n\r\nd",
            ],
            idle: Vec::new(),
            opened: 0,
        };
        let url = Url::parse("http://h/").unwrap();
        let bodies: Vec<Vec<u8>> = (0..4).map(|_| fetch(&url, &mut pool).unwrap().body).collect();
        assert_eq!(bodies, [b"a", b"b", b"c", b"d"]);
        // "Connection: close" on the third ends the reuse.
        assert_eq!(pool.opened, 2);
        let sent = String::from_utf8(pool.idle[0].sent.clone()).unwrap();
        assert!(sent.contains("Connection: keep-alive"));
    }

    #[test]
    fn stale_connections_are_retried() {
        let mut pool = Pool {
            responses: alloc::vec!["", "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok"],
            idle: alloc::vec![Canned { data: Vec::new(), pos: 0, sent: Vec::new() }],
            opened: 0,
        };
        let r = fetch(&Url::parse("http://h/").unwrap(), &mut pool).unwrap();
        assert_eq!(r.body, b"ok");
        assert_eq!(pool.opened, 1);
    }

    /// Downloads from canned responses, collecting the body.
    fn stream(raws: &[&str], from: u64, stop_after: usize) -> (Streamed, Vec<u8>) {
        let mut i = 0;
        let mut connect = |_: &Url| -> Result<Box<dyn Io>, Error> {
            i += 1;
            Ok(Box::new(Canned { data: raws[i - 1].as_bytes().to_vec(), pos: 0, sent: Vec::new() }))
        };
        let mut body = Vec::new();
        let mut sink = |b: &[u8]| {
            body.extend_from_slice(b);
            body.len() < stop_after
        };
        let s = download(&Url::parse("http://h/f").unwrap(), from, &mut connect, &mut |_, _| true, &mut sink).unwrap();
        (s, body)
    }

    #[test]
    fn download_streams_ranges_and_stops() {
        let (s, body) = stream(&["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789"], 0, usize::MAX);
        assert_eq!((s.status, s.complete, body.as_slice()), (200, true, &b"0123456789"[..]));
        // A resumed download: the server answers with the rest.
        let (s, body) = stream(
            &["HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 4-9/10\r\nContent-Length: 6\r\n\r\n456789"],
            4,
            usize::MAX,
        );
        assert_eq!((s.status, s.complete, body.as_slice()), (206, true, &b"456789"[..]));
        // The connection breaks off: what arrived is kept, and it says so.
        let (s, body) = stream(&["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n01234"], 0, usize::MAX);
        assert_eq!((s.complete, s.received, body.len()), (false, 5, 5));
        // Chunked, after a redirect.
        let (s, body) = stream(
            &[
                "HTTP/1.1 302 Found\r\nLocation: /g\r\nContent-Length: 0\r\n\r\n",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n4\r\nWiki\r\n6\r\npedia \r\n0\r\n\r\n",
            ],
            0,
            usize::MAX,
        );
        assert_eq!((s.complete, body.as_slice(), s.url.path.as_str()), (true, &b"Wikipedia "[..], "/g"));
        // The sink stops it (a pause).
        let (s, body) = stream(&["HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n0123456789"], 0, 3);
        assert!(!s.complete && body.len() >= 3 && body.len() < 10);
        // Errors are answers, not bodies.
        let (s, body) = stream(&["HTTP/1.1 404 Not Found\r\nContent-Length: 4\r\n\r\ngone"], 0, usize::MAX);
        assert_eq!((s.status, body.len()), (404, 0));
    }

    #[test]
    fn download_request_headers() {
        struct Spy(alloc::rc::Rc<core::cell::RefCell<Vec<u8>>>);
        impl Io for Spy {
            fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
                let r = b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                if self.0.borrow().ends_with(b"\r\n\r\n") && !self.0.borrow().starts_with(b"done") {
                    let n = r.len().min(buf.len());
                    buf[..n].copy_from_slice(&r[..n]);
                    let mut v = self.0.borrow_mut();
                    v.splice(0..0, *b"done");
                    return Ok(n);
                }
                Ok(0)
            }
            fn write_all(&mut self, data: &[u8]) -> Result<(), isize> {
                self.0.borrow_mut().extend_from_slice(data);
                Ok(())
            }
        }
        let log = alloc::rc::Rc::new(core::cell::RefCell::new(Vec::new()));
        let l = log.clone();
        let mut connect = move |_: &Url| -> Result<Box<dyn Io>, Error> { Ok(Box::new(Spy(l.clone()))) };
        let mut sink = |_: &[u8]| true;
        download(&Url::parse("http://h/f").unwrap(), 7, &mut connect, &mut |_, _| true, &mut sink).unwrap();
        let sent = String::from_utf8(log.borrow().clone()).unwrap();
        assert!(sent.contains("Range: bytes=7-\r\n") && sent.contains("Accept-Encoding: identity\r\n"), "{sent}");
    }

    #[test]
    fn gzip_and_deflate() {
        let text = b"hello hello hello compressed world";
        let raw = miniz_oxide::deflate::compress_to_vec(text, 6);
        let mut gz = alloc::vec![0x1F, 0x8B, 8, 8, 0, 0, 0, 0, 0, 3];
        gz.extend_from_slice(b"name.txt\0");
        gz.extend_from_slice(&raw);
        gz.extend_from_slice(&[0; 8]); // CRC and size (not checked)
        let mut resp =
            format!("HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\n\r\n", gz.len()).into_bytes();
        resp.extend_from_slice(&gz);
        let url = Url::parse("http://example.com/x").unwrap();
        let mut io = Canned { data: resp, pos: 0, sent: Vec::new() };
        let r = send(&mut io, &Request::get(&url)).unwrap();
        assert_eq!(r.body, text);
        assert_eq!(r.header("content-encoding"), None);
        let z = miniz_oxide::deflate::compress_to_vec_zlib(text, 6);
        let mut resp =
            format!("HTTP/1.1 200 OK\r\nContent-Encoding: deflate\r\nContent-Length: {}\r\n\r\n", z.len()).into_bytes();
        resp.extend_from_slice(&z);
        let mut io = Canned { data: resp, pos: 0, sent: Vec::new() };
        assert_eq!(send(&mut io, &Request::get(&url)).unwrap().body, text);
    }
}
