//! Connections for `nebula_web`'s HTTP client: plain TCP for `http:`, and
//! TLS 1.3/1.2 (verified against the system's root CAs) for `https:`, kept
//! alive in a small pool shared by the process's threads.

use crate::net::{self, TcpStream};
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use nebula_secure::{Config, Roots, TlsStream};
use nebula_web::cookie::{Jar, Source};
use nebula_web::http::{self, Connect, Cookies, Error, Io, Response};
use nebula_web::Url;

pub use nebula_secure;

/// Where the trusted root certificates live.
pub const ROOTS_PATH: &str = "/System/Certificates/roots.bin";

/// Idle connections are dropped after this long (servers close them anyway).
const IDLE_MS: u64 = 15_000;
const MAX_IDLE: usize = 16;

static ROOTS: Mutex<Option<Arc<Roots>>> = Mutex::new(None);
static POOL: Mutex<Vec<Idle>> = Mutex::new(Vec::new());
static JAR: Mutex<Jar> = Mutex::new(Jar::new());

/// Where connection time goes, for the process (see [`stats`]).
#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    /// New connections, and requests that reused a pooled one.
    pub opened: u32,
    pub reused: u32,
    pub dns_ms: u64,
    pub connect_ms: u64,
    pub tls_ms: u64,
}

static STATS: Mutex<Stats> = Mutex::new(Stats { opened: 0, reused: 0, dns_ms: 0, connect_ms: 0, tls_ms: 0 });

/// The connection counters so far (they only grow).
pub fn stats() -> Stats {
    *STATS.lock()
}

struct Idle {
    key: String,
    since: u64,
    conn: Conn,
}

/// An open connection: plain or secure.
pub enum Conn {
    Plain(TcpStream),
    Secure(Box<TlsStream<TcpStream>>),
}

impl Io for Conn {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, isize> {
        match self {
            Conn::Plain(s) => Io::read(s, buf),
            Conn::Secure(s) => s.read(buf),
        }
    }
    fn write_all(&mut self, data: &[u8]) -> Result<(), isize> {
        match self {
            Conn::Plain(s) => Io::write_all(s, data),
            Conn::Secure(s) => s.write_all(data),
        }
    }
}

/// Seconds since 1970 by the system clock.
pub fn unix_time() -> i64 {
    let d = crate::time::now();
    let days = nebula_secure::der::days_from_civil(d.year as i64, d.month as i64, d.day as i64);
    days * 86400 + d.hour as i64 * 3600 + d.minute as i64 * 60 + d.second as i64
}

fn key(url: &Url) -> String {
    format!("{}://{}:{}", url.scheme, url.host, url.port_or_default())
}

/// Opens a new connection to `url`'s host.
pub fn open(url: &Url) -> Result<Conn, Error> {
    let secure = match url.scheme.as_str() {
        "http" => false,
        "https" => true,
        other => return Err(Error::UnsupportedScheme(String::from(other))),
    };
    let t0 = crate::time::uptime_ms();
    let addrs = net::resolve_all(&url.host).map_err(|e| Error::Io(e.0))?;
    let t1 = crate::time::uptime_ms();
    let tcp = connect_any(&addrs, url.port_or_default()).map_err(|e| Error::Io(e.0))?;
    tcp.socket().set_timeout(30_000);
    let t2 = crate::time::uptime_ms();
    {
        let mut s = STATS.lock();
        s.opened += 1;
        s.dns_ms += t1 - t0;
        s.connect_ms += t2 - t1;
    }
    if !secure {
        return Ok(Conn::Plain(tcp));
    }
    let roots = roots()?;
    let config = Config { roots: &roots, now: unix_time(), random: net::random, verify: true };
    let tls = TlsStream::connect(tcp, &url.host, &config);
    STATS.lock().tls_ms += crate::time::uptime_ms() - t2;
    match tls {
        Ok(tls) => Ok(Conn::Secure(Box::new(tls))),
        Err(nebula_secure::Error::Io(e)) => Err(Error::Io(e)),
        Err(e) => Err(Error::Secure(e.describe())),
    }
}

/// Until when (uptime ms) IPv6 is skipped after it failed outright.
static V6_BROKEN_UNTIL: Mutex<u64> = Mutex::new(0);

/// Connects to the first address that answers, Happy Eyeballs style
/// (RFC 8305, sequential): IPv6 first when this machine has a routed IPv6
/// address, given 300 ms before IPv4 is tried; a hard IPv6 failure (no
/// route, refused, unreachable) skips IPv6 for ten minutes.
fn connect_any(addrs: &[net::IpAddr], port: u16) -> Result<TcpStream, crate::Error> {
    use net::IpAddr;
    let now = crate::time::uptime_ms();
    let v6_ok = *V6_BROKEN_UNTIL.lock() <= now && net::ipv6_routed();
    let v6: Vec<IpAddr> = addrs.iter().copied().filter(|a| matches!(a, IpAddr::V6(_))).collect();
    let v4: Vec<IpAddr> = addrs.iter().copied().filter(|a| matches!(a, IpAddr::V4(_))).collect();
    let mut last = crate::Error(crate::abi::err::ENOENT);
    if v6_ok {
        if let Some(&a) = v6.first() {
            let wait = if v4.is_empty() { 0 } else { 300 };
            match TcpStream::connect_addr(a, port, wait) {
                Ok(s) => return Ok(s),
                Err(e) => {
                    if !e.is(crate::abi::err::ETIMEDOUT) {
                        *V6_BROKEN_UNTIL.lock() = now + 600_000;
                    }
                    last = e;
                }
            }
        }
    }
    let rest = if v4.is_empty() && !v6_ok { &v6[..] } else { &v4[..] };
    for (k, &a) in rest.iter().enumerate() {
        // Every address but the last gets 10 s; the last the system default.
        let wait = if k + 1 < rest.len() { 10_000 } else { 0 };
        match TcpStream::connect_addr(a, port, wait) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Connects to `url`'s host: a one-off connector for `nebula_web::http::fetch`.
pub fn connect(url: &Url) -> Result<Box<dyn Io>, Error> {
    open(url).map(|c| Box::new(c) as Box<dyn Io>)
}

/// The shared keep-alive pool.
pub struct Pooled;

impl Connect for Pooled {
    type Conn = Conn;

    fn connect(&mut self, url: &Url, fresh: bool) -> Result<(Conn, bool), Error> {
        if !fresh {
            let k = key(url);
            let now = crate::time::uptime_ms();
            let mut pool = POOL.lock();
            pool.retain(|i| now - i.since < IDLE_MS);
            if let Some(p) = pool.iter().rposition(|i| i.key == k) {
                STATS.lock().reused += 1;
                return Ok((pool.remove(p).conn, true));
            }
        }
        open(url).map(|c| (c, false))
    }

    fn release(&mut self, url: &Url, conn: Conn) {
        let mut pool = POOL.lock();
        if pool.len() >= MAX_IDLE {
            pool.remove(0);
        }
        pool.push(Idle { key: key(url), since: crate::time::uptime_ms(), conn });
    }

    fn pooled(&self) -> bool {
        true
    }
}

/// The process's cookies: sent with, and kept from, [`fetch`] and
/// [`request`]. Starts empty; an app that wants them kept loads and saves
/// them itself.
pub fn cookies() -> crate::sync::MutexGuard<'static, Jar> {
    JAR.lock()
}

/// The process's cookie jar, as the HTTP client sees it.
pub struct SharedCookies;

impl Cookies for SharedCookies {
    fn header(&mut self, url: &Url) -> String {
        cookies().header(url, unix_time(), Source::Http)
    }
    fn store(&mut self, url: &Url, set_cookie: &str) {
        cookies().set(url, set_cookie, unix_time(), Source::Http);
    }
}

/// GETs `url` (following redirects) over pooled connections.
pub fn fetch(url: &Url) -> Result<Response, Error> {
    request("GET", url, &[], &[])
}

/// Sends any request (following redirects) over pooled connections, with
/// the process's cookies.
pub fn request(method: &str, url: &Url, headers: &[(String, String)], body: &[u8]) -> Result<Response, Error> {
    http::request_with_cookies(method, url, headers, body, &mut Pooled, &mut SharedCookies)
}

/// The system's trusted root CAs (loaded once).
pub fn roots() -> Result<Arc<Roots>, Error> {
    let mut cached = ROOTS.lock();
    if cached.is_none() {
        let file = crate::fs::read(ROOTS_PATH)
            .map_err(|_| Error::Secure(String::from("no trusted certificates are installed")))?;
        *cached = Some(Arc::new(Roots::parse(&file)));
    }
    Ok(cached.as_ref().unwrap().clone())
}
