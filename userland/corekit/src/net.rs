//! Networking: TCP streams and listeners, UDP sockets, ping, DNS, and the
//! interfaces' configuration.

use crate::abi::{self, err, net, nr, NetInterface, SockAddr};
use crate::sys::call;
use crate::{Error, Result};
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub type Ip = [u8; 4];

fn addr_ptr(a: &SockAddr) -> u64 {
    a as *const SockAddr as u64
}

/// A socket file descriptor (closed on drop).
pub struct Socket {
    fd: u64,
}

impl Socket {
    fn new(kind: u64) -> Result<Socket> {
        call(nr::SOCKET, &[kind]).map(|fd| Socket { fd })
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }

    /// Timeout for waiting operations in milliseconds (0 = forever).
    pub fn set_timeout(&self, ms: u64) {
        let _ = call(nr::SOCKOPT, &[self.fd, net::OPT_TIMEOUT, ms]);
    }

    pub fn set_nonblocking(&self, on: bool) {
        let _ = call(nr::SOCKOPT, &[self.fd, net::OPT_NONBLOCK, on as u64]);
    }

    /// Bytes waiting to be read.
    pub fn readable(&self) -> usize {
        call(nr::SOCKOPT, &[self.fd, net::OPT_READABLE, 0]).unwrap_or(0) as usize
    }

    /// (local address, remote address, state).
    pub fn info(&self) -> (SockAddr, SockAddr, u64) {
        let (mut l, mut r) = (SockAddr::default(), SockAddr::default());
        let st = call(nr::SOCK_INFO, &[self.fd, &mut l as *mut SockAddr as u64, &mut r as *mut SockAddr as u64]);
        (l, r, st.unwrap_or(0))
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = call(nr::CLOSE, &[self.fd]);
    }
}

/// A TCP connection.
pub struct TcpStream {
    sock: Socket,
}

impl TcpStream {
    pub fn connect(to: SockAddr) -> Result<TcpStream> {
        TcpStream::connect_timeout(to, 0)
    }

    /// Connects, giving up after `ms` (0 = the system's default of 20 s).
    pub fn connect_timeout(to: SockAddr, ms: u64) -> Result<TcpStream> {
        let sock = Socket::new(net::TCP)?;
        sock.set_timeout(ms);
        call(nr::CONNECT, &[sock.fd, addr_ptr(&to)])?;
        sock.set_timeout(0);
        Ok(TcpStream { sock })
    }

    /// Resolves `host` (a name or dotted address) and connects to `port`.
    pub fn connect_host(host: &str, port: u16) -> Result<TcpStream> {
        let ip = resolve(host)?;
        TcpStream::connect(SockAddr::new(ip, port))
    }

    /// Whatever has arrived (0 = the other side closed).
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        crate::io::read_fd(self.sock.fd, buf)
    }

    /// Reads until `buf` is full or the connection ends; returns the bytes read.
    pub fn read_full(&mut self, buf: &mut [u8]) -> Result<usize> {
        let mut n = 0;
        while n < buf.len() {
            match self.read(&mut buf[n..])? {
                0 => break,
                k => n += k,
            }
        }
        Ok(n)
    }

    /// Everything until the other side closes.
    pub fn read_to_end(&mut self) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match self.read(&mut buf)? {
                0 => return Ok(out),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        crate::io::write_fd(self.sock.fd, data).map(|_| ())
    }

    /// No more writing: the other side reads end of file.
    pub fn shutdown(&self) {
        let _ = call(nr::SHUTDOWN, &[self.sock.fd]);
    }

    pub fn socket(&self) -> &Socket {
        &self.sock
    }

    pub fn peer(&self) -> SockAddr {
        self.sock.info().1
    }

    pub fn local(&self) -> SockAddr {
        self.sock.info().0
    }
}

impl nebula_web::http::Io for TcpStream {
    fn read(&mut self, buf: &mut [u8]) -> core::result::Result<usize, isize> {
        TcpStream::read(self, buf).map_err(|e| e.0)
    }
    fn write_all(&mut self, data: &[u8]) -> core::result::Result<(), isize> {
        TcpStream::write_all(self, data).map_err(|e| e.0)
    }
}

/// Accepts TCP connections on a port.
pub struct TcpListener {
    sock: Socket,
}

impl TcpListener {
    pub fn bind(port: u16) -> Result<TcpListener> {
        let sock = Socket::new(net::TCP)?;
        call(nr::BIND, &[sock.fd, addr_ptr(&SockAddr::new([0; 4], port))])?;
        call(nr::LISTEN, &[sock.fd, 16])?;
        Ok(TcpListener { sock })
    }

    pub fn accept(&self) -> Result<(TcpStream, SockAddr)> {
        let mut peer = SockAddr::default();
        let fd = call(nr::ACCEPT, &[self.sock.fd, &mut peer as *mut SockAddr as u64])?;
        Ok((TcpStream { sock: Socket { fd } }, peer))
    }

    pub fn socket(&self) -> &Socket {
        &self.sock
    }
}

/// UDP datagrams.
pub struct UdpSocket {
    sock: Socket,
}

impl UdpSocket {
    /// Bound to `port` (0 = any free port).
    pub fn bind(port: u16) -> Result<UdpSocket> {
        let sock = Socket::new(net::UDP)?;
        if port != 0 {
            call(nr::BIND, &[sock.fd, addr_ptr(&SockAddr::new([0; 4], port))])?;
        }
        Ok(UdpSocket { sock })
    }

    pub fn send_to(&self, data: &[u8], to: SockAddr) -> Result<usize> {
        call(nr::SENDTO, &[self.sock.fd, data.as_ptr() as u64, data.len() as u64, addr_ptr(&to)]).map(|n| n as usize)
    }

    pub fn recv_from(&self, buf: &mut [u8]) -> Result<(usize, SockAddr)> {
        let mut from = SockAddr::default();
        let n =
            call(nr::RECVFROM, &[self.sock.fd, buf.as_mut_ptr() as u64, buf.len() as u64, &mut from as *mut _ as u64])?;
        Ok((n as usize, from))
    }

    pub fn socket(&self) -> &Socket {
        &self.sock
    }
}

/// ICMP echo ("ping").
pub struct Ping {
    sock: Socket,
}

impl Ping {
    pub fn new() -> Result<Ping> {
        Socket::new(net::ICMP).map(|sock| Ping { sock })
    }

    /// Sends echo request `seq` with `payload`.
    pub fn send(&self, to: Ip, seq: u16, payload: &[u8]) -> Result<()> {
        let mut m = vec![8u8, 0, 0, 0, 0, 0];
        m.extend_from_slice(&seq.to_be_bytes());
        m.extend_from_slice(payload);
        let a = SockAddr::new(to, 0);
        call(nr::SENDTO, &[self.sock.fd, m.as_ptr() as u64, m.len() as u64, addr_ptr(&a)]).map(|_| ())
    }

    /// The next reply: (from, sequence number, payload length), or a timeout error.
    pub fn recv(&self, timeout_ms: u64) -> Result<(Ip, u16, usize)> {
        self.sock.set_timeout(timeout_ms);
        let mut buf = [0u8; 2048];
        let mut from = SockAddr::default();
        let n =
            call(nr::RECVFROM, &[self.sock.fd, buf.as_mut_ptr() as u64, buf.len() as u64, &mut from as *mut _ as u64])?
                as usize;
        if n < 8 {
            return Err(Error(err::EIO));
        }
        Ok((from.ip, u16::from_be_bytes([buf[6], buf[7]]), n - 8))
    }
}

// ------------------------------------------------------------------ config

pub fn interfaces() -> Vec<NetInterface> {
    let mut v = vec![NetInterface::default(); 16];
    let n = call(nr::NET_INFO, &[v.as_mut_ptr() as u64, v.len() as u64]).unwrap_or(0) as usize;
    v.truncate(n.min(16));
    v
}

/// The first interface with an address (not loopback).
pub fn primary() -> Option<NetInterface> {
    interfaces().into_iter().find(|i| i.flags & net::IF_LOOPBACK == 0 && i.ip != [0; 4])
}

pub fn name_of(i: &NetInterface) -> &str {
    core::str::from_utf8(&i.name[..i.name_len as usize]).unwrap_or("?")
}

pub fn driver_of(i: &NetInterface) -> &str {
    core::str::from_utf8(&i.driver[..i.driver_len as usize]).unwrap_or("?")
}

pub fn ip_string(ip: Ip) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

/// "10.0.2.15" → [10, 0, 2, 15].
pub fn parse_ip(s: &str) -> Option<Ip> {
    let mut out = [0u8; 4];
    let mut parts = s.split('.');
    for o in out.iter_mut() {
        *o = parts.next()?.parse().ok()?;
    }
    parts.next().is_none().then_some(out)
}

pub fn random(buf: &mut [u8]) {
    let _ = call(nr::GETRANDOM, &[buf.as_mut_ptr() as u64, buf.len() as u64]);
}

// ------------------------------------------------------------------- IPv6

/// An address of either family.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IpAddr {
    V4(Ip),
    V6([u8; 16]),
}

impl core::fmt::Display for IpAddr {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        match self {
            IpAddr::V4(ip) => f.write_str(&ip_string(*ip)),
            IpAddr::V6(a) => f.write_str(&ip6_string(a)),
        }
    }
}

/// "2001:db8::1" (also "::ffff:1.2.3.4" and a "%zone", ignored) → bytes.
pub fn parse_ip6(s: &str) -> Option<[u8; 16]> {
    let s = s.split('%').next()?;
    if !s.contains(':') {
        return None;
    }
    let groups = |part: &str| -> Option<Vec<u16>> {
        if part.is_empty() {
            return Some(Vec::new());
        }
        let mut out = Vec::new();
        let items: Vec<&str> = part.split(':').collect();
        for (k, g) in items.iter().enumerate() {
            if k == items.len() - 1 && g.contains('.') {
                let v4 = parse_ip(g)?;
                out.push(u16::from_be_bytes([v4[0], v4[1]]));
                out.push(u16::from_be_bytes([v4[2], v4[3]]));
            } else {
                if g.is_empty() || g.len() > 4 {
                    return None;
                }
                out.push(u16::from_str_radix(g, 16).ok()?);
            }
        }
        Some(out)
    };
    let words: Vec<u16> = match s.split_once("::") {
        Some((a, b)) => {
            let (a, b) = (groups(a)?, groups(b)?);
            if a.len() + b.len() > 7 {
                return None;
            }
            let mut w = a;
            w.resize(8 - b.len(), 0);
            w.extend(b);
            w
        }
        None => groups(s)?,
    };
    if words.len() != 8 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, w) in words.iter().enumerate() {
        out[2 * i..2 * i + 2].copy_from_slice(&w.to_be_bytes());
    }
    Some(out)
}

/// An address in either form, with or without URL brackets ("[::1]").
pub fn parse_addr(s: &str) -> Option<IpAddr> {
    let s = s.strip_prefix('[').and_then(|x| x.strip_suffix(']')).unwrap_or(s);
    parse_ip(s).map(IpAddr::V4).or_else(|| parse_ip6(s).map(IpAddr::V6))
}

/// The shortest text form (RFC 5952).
pub fn ip6_string(a: &[u8; 16]) -> String {
    let g: [u16; 8] = core::array::from_fn(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]]));
    let (mut best, mut best_len, mut i) = (8, 0, 0);
    while i < 8 {
        if g[i] == 0 {
            let start = i;
            while i < 8 && g[i] == 0 {
                i += 1;
            }
            if i - start > best_len && i - start > 1 {
                best = start;
                best_len = i - start;
            }
        } else {
            i += 1;
        }
    }
    let part = |r: core::ops::Range<usize>| g[r].iter().map(|x| format!("{x:x}")).collect::<Vec<_>>().join(":");
    if best_len == 0 {
        part(0..8)
    } else {
        format!("{}::{}", part(0..best), part(best + best_len..8))
    }
}

/// Interfaces' IPv6 configuration (in `interfaces()` order).
pub fn interfaces6() -> Vec<abi::NetInterface6> {
    let mut v = vec![abi::NetInterface6::default(); 16];
    let n = call(nr::NET_INFO6, &[v.as_mut_ptr() as u64, v.len() as u64]).unwrap_or(0) as usize;
    v.truncate(n.min(16));
    v
}

/// Whether IPv6 can reach the internet from here: a global unicast address
/// (2000::/3; not site-local fec0::/10 or unique-local fc00::/7, which can't,
/// as RFC 6724's address selection says) and a default router on some card.
pub fn ipv6_routed() -> bool {
    interfaces6().iter().skip(1).any(|i| {
        let global = i.addrs[..i.addr_count as usize].iter().any(|a| a[0] & 0xE0 == 0x20);
        global && i.router != [0; 16]
    })
}

fn addr6_ptr(a: &abi::SockAddr6) -> u64 {
    a as *const abi::SockAddr6 as u64
}

impl TcpStream {
    /// Connects to an address of either family (`ms` as in [`connect_timeout`]).
    pub fn connect_addr(to: IpAddr, port: u16, ms: u64) -> Result<TcpStream> {
        match to {
            IpAddr::V4(ip) => TcpStream::connect_timeout(SockAddr::new(ip, port), ms),
            IpAddr::V6(ip) => {
                let sock = Socket::new(net::TCP6)?;
                sock.set_timeout(ms);
                call(nr::CONNECT, &[sock.fd, addr6_ptr(&abi::SockAddr6::new(ip, port))])?;
                sock.set_timeout(0);
                Ok(TcpStream { sock })
            }
        }
    }
}

/// ICMPv6 echo.
pub struct Ping6 {
    sock: Socket,
}

impl Ping6 {
    pub fn new() -> Result<Ping6> {
        Socket::new(net::ICMP6).map(|sock| Ping6 { sock })
    }

    pub fn send(&self, to: [u8; 16], seq: u16, payload: &[u8]) -> Result<()> {
        let mut m = vec![128u8, 0, 0, 0, 0, 0];
        m.extend_from_slice(&seq.to_be_bytes());
        m.extend_from_slice(payload);
        let a = abi::SockAddr6::new(to, 0);
        call(nr::SENDTO, &[self.sock.fd, m.as_ptr() as u64, m.len() as u64, addr6_ptr(&a)]).map(|_| ())
    }

    /// The next reply: (from, sequence number, payload length).
    pub fn recv(&self, timeout_ms: u64) -> Result<([u8; 16], u16, usize)> {
        self.sock.set_timeout(timeout_ms);
        let mut buf = [0u8; 2048];
        let mut from = abi::SockAddr6::default();
        let n =
            call(nr::RECVFROM, &[self.sock.fd, buf.as_mut_ptr() as u64, buf.len() as u64, &mut from as *mut _ as u64])?
                as usize;
        if n < 8 {
            return Err(Error(err::EIO));
        }
        Ok((from.ip, u16::from_be_bytes([buf[6], buf[7]]), n - 8))
    }
}

// --------------------------------------------------------------------- DNS

/// Answers by host name: (host, addresses or error, expiry in uptime ms).
type Answer = core::result::Result<Vec<IpAddr>, isize>;
static DNS_CACHE: crate::sync::Mutex<Vec<(String, Answer, u64)>> = crate::sync::Mutex::new(Vec::new());
const DNS_CACHE_SIZE: usize = 256;

/// Resolves a host name to an IPv4 address (dotted addresses pass through).
pub fn resolve(host: &str) -> Result<Ip> {
    resolve_all(host)?
        .into_iter()
        .find_map(|a| match a {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        })
        .ok_or(Error(err::ENOENT))
}

/// Every address of `host`: IPv6 and IPv4 (A and AAAA are asked together),
/// kept for their TTL. Literal addresses, bracketed or not, pass through.
pub fn resolve_all(host: &str) -> Result<Vec<IpAddr>> {
    if let Some(a) = parse_addr(host) {
        return Ok(vec![a]);
    }
    if host == "localhost" {
        return Ok(vec![IpAddr::V4([127, 0, 0, 1])]);
    }
    let key = host.trim_end_matches('.').to_ascii_lowercase();
    let now = crate::time::uptime_ms();
    {
        let mut cache = DNS_CACHE.lock();
        cache.retain(|(_, _, until)| *until > now);
        if let Some((_, answer, _)) = cache.iter().find(|(h, _, _)| *h == key) {
            return answer.clone().map_err(Error);
        }
    }
    let answer = lookup(&key);
    // Keep answers for their TTL (30 s to an hour); "no such host" for 10 s.
    let (value, ttl): (Answer, u64) = match answer {
        Ok((addrs, ttl)) => (Ok(addrs), (ttl as u64).clamp(30, 3600) * 1000),
        Err(e) if e.0.abs() == err::ENOENT => (Err(e.0), 10_000),
        Err(e) => return Err(e),
    };
    let mut cache = DNS_CACHE.lock();
    if cache.len() >= DNS_CACHE_SIZE {
        let _ = cache.remove(0);
    }
    cache.retain(|(h, _, _)| *h != key);
    cache.push((key, value.clone(), now + ttl));
    value.map_err(Error)
}

/// Asks the configured DNS servers (IPv4 ones, else IPv6 ones from router
/// advertisements): (addresses, smallest TTL).
fn lookup(host: &str) -> Result<(Vec<IpAddr>, u32)> {
    let mut servers: Vec<IpAddr> =
        interfaces().iter().flat_map(|i| i.dns).filter(|d| *d != [0; 4]).map(IpAddr::V4).collect();
    for i in interfaces6() {
        servers.extend(i.dns[..i.dns_count as usize].iter().map(|d| IpAddr::V6(*d)));
    }
    if servers.is_empty() {
        return Err(Error(err::ENETUNREACH));
    }
    let mut last = Error(err::ENOENT);
    for server in servers {
        match query(server, host) {
            Ok(r) => return Ok(r),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn question(id: u16, host: &str, kind: u16) -> Result<Vec<u8>> {
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]); // recursion desired, one question
    for label in host.trim_end_matches('.').split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(Error(err::EINVAL));
        }
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.extend_from_slice(&[0]);
    q.extend_from_slice(&kind.to_be_bytes());
    q.extend_from_slice(&[0, 1]); // class IN
    Ok(q)
}

/// Sends the A and AAAA questions at once and collects both answers (an
/// AAAA answer that's slow once A is in isn't waited for long).
fn query(server: IpAddr, host: &str) -> Result<(Vec<IpAddr>, u32)> {
    let sock = match server {
        IpAddr::V4(_) => Socket::new(net::UDP)?,
        IpAddr::V6(_) => Socket::new(net::UDP6)?,
    };
    let send = |q: &[u8]| -> Result<()> {
        match server {
            IpAddr::V4(ip) => {
                call(nr::SENDTO, &[sock.fd, q.as_ptr() as u64, q.len() as u64, addr_ptr(&SockAddr::new(ip, 53))])
            }
            IpAddr::V6(ip) => {
                let a = abi::SockAddr6::new(ip, 53);
                call(nr::SENDTO, &[sock.fd, q.as_ptr() as u64, q.len() as u64, addr6_ptr(&a)])
            }
        }
        .map(|_| ())
    };
    let mut idb = [0u8; 4];
    random(&mut idb);
    let ids = [u16::from_be_bytes([idb[0], idb[1]]), u16::from_be_bytes([idb[2], idb[3]])];
    let questions = [question(ids[0], host, 1)?, question(ids[1], host, 28)?];
    let mut answers: [Option<Records>; 2] = [None, None];
    let mut buf = [0u8; 1500];
    for attempt in 0..3u64 {
        for (k, q) in questions.iter().enumerate() {
            if answers[k].is_none() {
                send(q)?;
            }
        }
        let deadline = crate::time::uptime_ms() + (1000 << attempt);
        loop {
            let now = crate::time::uptime_ms();
            // A is in: give AAAA a little longer only.
            let deadline = if answers[0].is_some() { deadline.min(now + 300) } else { deadline };
            if now >= deadline || answers.iter().all(|a| a.is_some()) {
                break;
            }
            sock.set_timeout(deadline - now);
            let mut from = abi::SockAddr6::default();
            let n = match call(
                nr::RECVFROM,
                &[sock.fd, buf.as_mut_ptr() as u64, buf.len() as u64, &mut from as *mut _ as u64],
            ) {
                Ok(n) => n as usize,
                Err(e) if e.is(err::ETIMEDOUT) => break,
                Err(e) => return Err(e),
            };
            if n < 12 {
                continue;
            }
            let id = u16::from_be_bytes([buf[0], buf[1]]);
            if let Some(k) = ids.iter().position(|i| *i == id) {
                answers[k] = Some(parse_answer(&buf[..n]));
            }
        }
        if answers[0].is_some() {
            break;
        }
    }
    let answered = answers.iter().any(|a| matches!(a, Some(Ok(_))));
    let mut addrs = Vec::new();
    let mut ttl = u32::MAX;
    let mut first_err = None;
    // IPv6 first, as address selection prefers it (the caller decides).
    for k in [1, 0] {
        match answers[k].take() {
            Some(Ok((a, t))) => {
                addrs.extend(a);
                ttl = ttl.min(t);
            }
            Some(Err(e)) => first_err = first_err.or(Some(e)),
            None => {}
        }
    }
    if addrs.is_empty() {
        return Err(first_err.unwrap_or(Error(if answered { err::ENOENT } else { err::ETIMEDOUT })));
    }
    Ok((addrs, ttl))
}

/// Skips a (possibly compressed) name; returns the offset after it.
fn skip_name(m: &[u8], mut i: usize) -> Option<usize> {
    loop {
        let len = *m.get(i)? as usize;
        if len == 0 {
            return Some(i + 1);
        }
        if len & 0xC0 == 0xC0 {
            return Some(i + 2);
        }
        i += 1 + len;
    }
}

/// A response's addresses and their smallest TTL (seconds).
type Records = Result<(Vec<IpAddr>, u32)>;

/// The A and AAAA records of a response.
fn parse_answer(m: &[u8]) -> Records {
    let rcode = m[3] & 0xF;
    if rcode == 3 {
        return Err(Error(err::ENOENT)); // no such name
    }
    let qd = u16::from_be_bytes([m[4], m[5]]);
    let an = u16::from_be_bytes([m[6], m[7]]);
    let mut i = 12;
    for _ in 0..qd {
        i = skip_name(m, i).ok_or(Error(err::EIO))? + 4;
    }
    let mut out = Vec::new();
    let mut ttl = u32::MAX;
    for _ in 0..an {
        i = skip_name(m, i).ok_or(Error(err::EIO))?;
        let rec = m.get(i..i + 10).ok_or(Error(err::EIO))?;
        let (kind, class) = (u16::from_be_bytes([rec[0], rec[1]]), u16::from_be_bytes([rec[2], rec[3]]));
        let len = u16::from_be_bytes([rec[8], rec[9]]) as usize;
        i += 10;
        let data = m.get(i..i + len).ok_or(Error(err::EIO))?;
        // CNAMEs come before the addresses they lead to: keep looking.
        let addr = match (kind, class, len) {
            (1, 1, 4) => Some(IpAddr::V4([data[0], data[1], data[2], data[3]])),
            (28, 1, 16) => Some(IpAddr::V6(data.try_into().unwrap())),
            _ => None,
        };
        if let Some(a) = addr {
            out.push(a);
            ttl = ttl.min(u32::from_be_bytes([rec[4], rec[5], rec[6], rec[7]]));
        }
        i += len;
    }
    Ok((out, if ttl == u32::MAX { 300 } else { ttl }))
}

/// Human-readable error for network failures.
pub fn describe(e: Error) -> &'static str {
    match e.0.abs() {
        err::ENOENT => "no such host",
        err::ENETUNREACH => "the network is not connected",
        _ => abi::err::name(e.0),
    }
}

// -------------------------------------------------------------------- Wi-Fi

/// The Wi-Fi adapter's state; `None` without Wi-Fi hardware.
pub fn wifi_status() -> Option<abi::WifiStatus> {
    let mut s = abi::WifiStatus::default();
    call(nr::WIFI_STATUS, &[&mut s as *mut _ as u64]).ok().map(|_| s)
}

/// Networks heard recently (asks for a new scan too).
pub fn wifi_networks() -> Vec<abi::WifiNetwork> {
    let mut v = vec![abi::WifiNetwork::default(); 64];
    let n = call(nr::WIFI_NETWORKS, &[v.as_mut_ptr() as u64, v.len() as u64]).unwrap_or(0) as usize;
    v.truncate(n.min(64));
    v
}

/// Starts joining a network (progress shows in [`wifi_status`]); an empty
/// password for open networks.
pub fn wifi_connect(ssid: &[u8], password: &str) -> Result<u64> {
    call(nr::WIFI_CONNECT, &[ssid.as_ptr() as u64, ssid.len() as u64, password.as_ptr() as u64, password.len() as u64])
}

pub fn wifi_disconnect() {
    let _ = call(nr::WIFI_DISCONNECT, &[]);
}

/// The SSID bytes as text.
pub fn wifi_name(ssid: &[u8]) -> String {
    String::from_utf8_lossy(ssid).into_owned()
}
