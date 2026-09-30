//! Connections for `aurora_web`'s HTTP client: plain TCP for `http:`, and
//! TLS 1.3 (verified against the system's root CAs) for `https:`.

use crate::net::{self, TcpStream};
use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use aurora_tls::{Config, Roots, TlsStream};
use aurora_web::http::{Error, Io};
use aurora_web::Url;

pub use aurora_tls;

/// Where the trusted root certificates live.
pub const ROOTS_PATH: &str = "/System/Certificates/roots.bin";

static ROOTS: Mutex<Option<Arc<Roots>>> = Mutex::new(None);

/// Seconds since 1970 by the system clock.
pub fn unix_time() -> i64 {
    let d = crate::time::now();
    let days = aurora_tls::der::days_from_civil(d.year as i64, d.month as i64, d.day as i64);
    days * 86400 + d.hour as i64 * 3600 + d.minute as i64 * 60 + d.second as i64
}

/// Connects to `url`'s host: the connector for `aurora_web::http::fetch`.
pub fn connect(url: &Url) -> Result<Box<dyn Io>, Error> {
    let secure = match url.scheme.as_str() {
        "http" => false,
        "https" => true,
        other => return Err(Error::UnsupportedScheme(String::from(other))),
    };
    let ip = net::resolve(&url.host).map_err(|e| Error::Io(e.0))?;
    let tcp = TcpStream::connect(crate::abi::SockAddr::new(ip, url.port_or_default())).map_err(|e| Error::Io(e.0))?;
    tcp.socket().set_timeout(30_000);
    if !secure {
        return Ok(Box::new(tcp));
    }
    let roots = roots()?;
    let config = Config { roots: &roots, now: unix_time(), random: net::random, verify: true };
    match TlsStream::connect(tcp, &url.host, &config) {
        Ok(tls) => Ok(Box::new(tls)),
        Err(aurora_tls::Error::Io(e)) => Err(Error::Io(e)),
        Err(e) => Err(Error::Secure(e.describe())),
    }
}

/// The system's trusted root CAs (loaded once).
pub fn roots() -> Result<Arc<Roots>, Error> {
    let mut cached = ROOTS.lock();
    if cached.is_none() {
        let file = crate::fs::read(ROOTS_PATH).map_err(|_| Error::Secure(String::from("no trusted certificates are installed")))?;
        *cached = Some(Arc::new(Roots::parse(&file)));
    }
    Ok(cached.as_ref().unwrap().clone())
}
