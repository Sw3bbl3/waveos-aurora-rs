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

impl aurora_web::http::Io for TcpStream {
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

// --------------------------------------------------------------------- DNS

/// Resolves a host name to an IPv4 address (dotted addresses pass through).
pub fn resolve(host: &str) -> Result<Ip> {
    if let Some(ip) = parse_ip(host) {
        return Ok(ip);
    }
    if host == "localhost" {
        return Ok([127, 0, 0, 1]);
    }
    let servers: Vec<Ip> = interfaces().iter().flat_map(|i| i.dns).filter(|d| *d != [0; 4]).collect();
    if servers.is_empty() {
        return Err(Error(err::ENETUNREACH));
    }
    let mut last = Error(err::ENOENT);
    for server in servers {
        match query(server, host) {
            Ok(ip) => return Ok(ip),
            Err(e) => last = e,
        }
    }
    Err(last)
}

fn query(server: Ip, host: &str) -> Result<Ip> {
    let sock = UdpSocket::bind(0)?;
    let mut idb = [0u8; 2];
    random(&mut idb);
    let id = u16::from_be_bytes(idb);
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
    q.extend_from_slice(&[0, 0, 1, 0, 1]); // type A, class IN
    let to = SockAddr::new(server, 53);
    let mut buf = [0u8; 1500];
    for attempt in 0..3 {
        sock.send_to(&q, to)?;
        sock.socket().set_timeout(1000 << attempt);
        loop {
            let (n, from) = match sock.recv_from(&mut buf) {
                Ok(r) => r,
                Err(e) if e.is(err::ETIMEDOUT) => break,
                Err(e) => return Err(e),
            };
            if from.ip != server || n < 12 || buf[..2] != id.to_be_bytes() {
                continue;
            }
            return parse_answer(&buf[..n]);
        }
    }
    Err(Error(err::ETIMEDOUT))
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

fn parse_answer(m: &[u8]) -> Result<Ip> {
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
    for _ in 0..an {
        i = skip_name(m, i).ok_or(Error(err::EIO))?;
        let rec = m.get(i..i + 10).ok_or(Error(err::EIO))?;
        let (kind, class) = (u16::from_be_bytes([rec[0], rec[1]]), u16::from_be_bytes([rec[2], rec[3]]));
        let len = u16::from_be_bytes([rec[8], rec[9]]) as usize;
        i += 10;
        // A CNAME's target is usually followed by its A record: keep looking.
        if kind == 1 && class == 1 && len == 4 {
            let d = m.get(i..i + 4).ok_or(Error(err::EIO))?;
            return Ok([d[0], d[1], d[2], d[3]]);
        }
        i += len;
    }
    Err(Error(err::ENOENT))
}

/// Human-readable error for network failures.
pub fn describe(e: Error) -> &'static str {
    match e.0.abs() {
        err::ENOENT => "no such host",
        err::ENETUNREACH => "the network is not connected",
        _ => abi::err::name(e.0),
    }
}
