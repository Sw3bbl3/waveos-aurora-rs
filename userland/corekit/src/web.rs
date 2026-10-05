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
use nebula_web::http::{self, Connect, Error, Io, Response};
use nebula_web::Url;

pub use nebula_secure;

/// Where the trusted root certificates live.
pub const ROOTS_PATH: &str = "/System/Certificates/roots.bin";

/// Idle connections are dropped after this long (servers close them anyway).
const IDLE_MS: u64 = 15_000;
const MAX_IDLE: usize = 8;

static ROOTS: Mutex<Option<Arc<Roots>>> = Mutex::new(None);
static POOL: Mutex<Vec<Idle>> = Mutex::new(Vec::new());

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
    let ip = net::resolve(&url.host).map_err(|e| Error::Io(e.0))?;
    let tcp = TcpStream::connect(crate::abi::SockAddr::new(ip, url.port_or_default())).map_err(|e| Error::Io(e.0))?;
    tcp.socket().set_timeout(30_000);
    if !secure {
        return Ok(Conn::Plain(tcp));
    }
    let roots = roots()?;
    let config = Config { roots: &roots, now: unix_time(), random: net::random, verify: true };
    match TlsStream::connect(tcp, &url.host, &config) {
        Ok(tls) => Ok(Conn::Secure(Box::new(tls))),
        Err(nebula_secure::Error::Io(e)) => Err(Error::Io(e)),
        Err(e) => Err(Error::Secure(e.describe())),
    }
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

/// GETs `url` (following redirects) over pooled connections.
pub fn fetch(url: &Url) -> Result<Response, Error> {
    http::fetch(url, &mut Pooled)
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
