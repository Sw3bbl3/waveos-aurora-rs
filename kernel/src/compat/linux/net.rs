//! Sockets for Linux programs: IPv4 and IPv6 TCP and UDP over the
//! kernel's network stack (`crate::net`), and local sockets
//! ([`super::unix`]), with Linux's addresses, messages, ancillary data
//! (`SCM_RIGHTS`: descriptors sent along) and options.

use super::fs::{install_special, Special};
use super::unix::{self, Type};
use crate::net::socket::{Kind, Socket};
use crate::proc::{Handle, Process};
use crate::syscall::user;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

const AF_UNIX: u64 = 1;
const AF_INET: u64 = 2;
const AF_INET6: u64 = 10;
const SOCK_STREAM: u64 = 1;
const SOCK_DGRAM: u64 = 2;
const SOCK_RAW: u64 = 3;
const SOCK_SEQPACKET: u64 = 5;
const SOCK_NONBLOCK: u64 = 0o4000;
const SOCK_CLOEXEC: u64 = 0o2000000;
const SOL_SOCKET: u64 = 1;
const SCM_RIGHTS: i32 = 1;
const SCM_CREDENTIALS: i32 = 2;
const SO_PASSCRED: u64 = 16;
const MSG_PEEK: u64 = 2;
const MSG_TRUNC: u64 = 0x20;
const MSG_CTRUNC: u64 = 0x8;
const MSG_DONTWAIT: u64 = 0x40;
const MSG_NOSIGNAL: u64 = 0x4000;
const MSG_CMSG_CLOEXEC: u64 = 0x4000_0000;

/// A socket descriptor of either kind.
enum Sock {
    Net(Arc<Handle>),
    Local(Arc<unix::Socket>),
}

impl Sock {
    fn net(&self) -> &Socket {
        match self {
            Sock::Net(h) => match &**h {
                Handle::Socket(s) => s,
                _ => unreachable!(),
            },
            Sock::Local(_) => unreachable!(),
        }
    }
}

fn sock(p: &Process, fd: u64) -> Result<Sock, isize> {
    let h = p.fd(fd as usize)?;
    match &*h {
        Handle::Socket(_) => Ok(Sock::Net(h.clone())),
        Handle::Special(s) => match &*s.lock() {
            Special::Unix(u) => Ok(Sock::Local(u.clone())),
            _ => Err(ENOTSOCK),
        },
        _ => Err(ENOTSOCK),
    }
}

fn nonblock(p: &Process, fd: u64, flags: u64) -> bool {
    flags & MSG_DONTWAIT != 0 || p.linux.as_ref().is_some_and(|l| l.nonblock(fd as usize))
}

// ---------------------------------------------------------------- addresses

/// A socket address as Linux programs write it.
enum Addr {
    Net(crate::net::socket::Peer),
    Local(String),
}

fn read_addr(p: &Process, ptr: u64, len: u64) -> Result<Addr, isize> {
    if !(2..=128).contains(&len) {
        return Err(EINVAL);
    }
    let mut raw = [0u8; 128];
    user::get_bytes(ptr, &mut raw[..len as usize])?;
    let family = u16::from_le_bytes([raw[0], raw[1]]) as u64;
    match family {
        AF_INET if len >= 16 => {
            let port = u16::from_be_bytes([raw[2], raw[3]]);
            Ok(Addr::Net((crate::net::v4([raw[4], raw[5], raw[6], raw[7]]), port)))
        }
        AF_INET6 if len >= 24 => {
            let port = u16::from_be_bytes([raw[2], raw[3]]);
            let mut a = [0u8; 16];
            a.copy_from_slice(&raw[8..24]);
            Ok(Addr::Net((a, port)))
        }
        AF_UNIX => {
            let path = &raw[2..len as usize];
            if path.first() == Some(&0) {
                // The abstract namespace: every byte counts.
                Ok(Addr::Local(alloc::format!("@{}", String::from_utf8_lossy(&path[1..]))))
            } else {
                let end = path.iter().position(|&b| b == 0).unwrap_or(path.len());
                let name = core::str::from_utf8(&path[..end]).map_err(|_| EINVAL)?;
                if name.is_empty() {
                    return Err(EINVAL);
                }
                Ok(Addr::Local(super::fs::absolute(p, name)))
            }
        }
        _ => Err(EAFNOSUPPORT),
    }
}

/// Writes `a` to `ptr` (at most `*len_ptr` bytes) and the full length to `len_ptr`.
fn write_addr(ptr: u64, len_ptr: u64, a: Addr, v6: bool) -> Result<(), isize> {
    if ptr == 0 || len_ptr == 0 {
        return Ok(());
    }
    let room: u32 = user::get(len_ptr)?;
    let mut raw = [0u8; 110];
    let len = match a {
        // An IPv4 socket always has IPv4 addresses (unset: 0.0.0.0).
        Addr::Net((ip, port)) => match (!v6).then(|| crate::net::as_v4(&ip).unwrap_or([0; 4])) {
            Some(v4) => {
                raw[0..2].copy_from_slice(&(AF_INET as u16).to_le_bytes());
                raw[2..4].copy_from_slice(&port.to_be_bytes());
                raw[4..8].copy_from_slice(&v4);
                16
            }
            None => {
                raw[0..2].copy_from_slice(&(AF_INET6 as u16).to_le_bytes());
                raw[2..4].copy_from_slice(&port.to_be_bytes());
                raw[8..24].copy_from_slice(&ip);
                28
            }
        },
        Addr::Local(name) => {
            raw[0..2].copy_from_slice(&(AF_UNIX as u16).to_le_bytes());
            let bytes: Vec<u8> = match name.strip_prefix('@') {
                Some(abs) => core::iter::once(0).chain(abs.bytes()).collect(),
                None => name.bytes().chain(core::iter::once(0)).collect(),
            };
            let n = bytes.len().min(108);
            raw[2..2 + n].copy_from_slice(&bytes[..n]);
            2 + n
        }
    };
    user::put_bytes(ptr, &raw[..len.min(room as usize)])?;
    user::put(len_ptr, &(len as u32))
}

// ---------------------------------------------------------------- calls

pub fn socket(p: &Process, domain: u64, ty: u64, proto: u64) -> Result<u64, isize> {
    let cloexec = ty & SOCK_CLOEXEC != 0;
    let nb = ty & SOCK_NONBLOCK != 0;
    let fd = match (domain, ty & 0xF) {
        (AF_UNIX, t) => {
            let ty = local_type(t)?;
            install_special(p, Special::Unix(unix::Socket::new(ty)), cloexec)?
        }
        (AF_INET | AF_INET6, t) => {
            let kind = match (t, proto) {
                (SOCK_STREAM, 0 | 6) => Kind::Tcp,
                (SOCK_DGRAM, 0 | 17) => Kind::Udp,
                (SOCK_DGRAM | SOCK_RAW, 1 | 58) => Kind::Icmp,
                _ => return Err(EPROTONOSUPPORT_),
            };
            let s = Socket::new(kind, domain == AF_INET6);
            if nb {
                s.option(aurora_abi::net::OPT_NONBLOCK, 1)?;
            }
            super::fs::install_handle(p, Arc::new(Handle::Socket(s)), cloexec)?
        }
        _ => return Err(EAFNOSUPPORT),
    };
    set_nonblock(p, fd, nb);
    Ok(fd)
}

/// EPROTONOSUPPORT.
const EPROTONOSUPPORT_: isize = 93;

fn local_type(t: u64) -> Result<Type, isize> {
    Ok(match t {
        SOCK_STREAM => Type::Stream,
        SOCK_DGRAM => Type::Dgram,
        SOCK_SEQPACKET => Type::SeqPacket,
        _ => return Err(EPROTONOSUPPORT_),
    })
}

fn set_nonblock(p: &Process, fd: u64, on: bool) {
    if let Some(l) = &p.linux {
        l.set_nonblock(fd as usize, on);
    }
}

pub fn socketpair(p: &Process, domain: u64, ty: u64, _proto: u64, out: u64) -> Result<u64, isize> {
    if domain != AF_UNIX {
        return Err(EOPNOTSUPP);
    }
    user::writable(out, 8)?;
    let (a, b) = unix::Socket::pair(local_type(ty & 0xF)?, p.pid);
    let cloexec = ty & SOCK_CLOEXEC != 0;
    let fa = install_special(p, Special::Unix(a), cloexec)?;
    let fb = install_special(p, Special::Unix(b), cloexec)?;
    for fd in [fa, fb] {
        set_nonblock(p, fd, ty & SOCK_NONBLOCK != 0);
    }
    user::put(out, &[fa as i32, fb as i32]).map(|_| 0)
}

pub fn bind(p: &Process, fd: u64, addr: u64, len: u64) -> Result<u64, isize> {
    match (sock(p, fd)?, read_addr(p, addr, len)?) {
        (Sock::Local(u), Addr::Local(name)) => u.bind(name).map(|_| 0),
        (s @ Sock::Net(_), Addr::Net(peer)) => s.net().bind(peer).map(|_| 0),
        _ => Err(EINVAL),
    }
}

pub fn listen(p: &Process, fd: u64, backlog: u64) -> Result<u64, isize> {
    match sock(p, fd)? {
        Sock::Local(u) => u.listen().map(|_| 0),
        s => s.net().listen(backlog.clamp(1, 128) as usize).map(|_| 0),
    }
}

pub fn connect(p: &Process, fd: u64, addr: u64, len: u64) -> Result<u64, isize> {
    match (sock(p, fd)?, read_addr(p, addr, len)?) {
        (Sock::Local(u), Addr::Local(name)) => u.connect(name, p.pid).map(|_| 0),
        (s @ Sock::Net(_), Addr::Net(peer)) => {
            let net = s.net();
            if net.connecting() {
                return Err(EALREADY);
            }
            match net.connect(peer) {
                // A non-blocking connection carries on in the background.
                Err(EAGAIN) => Err(EINPROGRESS),
                r => r.map(|_| 0),
            }
        }
        _ => Err(EAFNOSUPPORT),
    }
}

pub fn accept(p: &Process, fd: u64, addr: u64, len_ptr: u64, flags: u64) -> Result<u64, isize> {
    let cloexec = flags & SOCK_CLOEXEC != 0;
    let nb = nonblock(p, fd, 0);
    let new = match sock(p, fd)? {
        Sock::Local(u) => {
            let c = u.accept(nb, p.pid)?;
            let name = c.names().0;
            let fd = install_special(p, Special::Unix(c), cloexec)?;
            write_addr(addr, len_ptr, Addr::Local(name.unwrap_or_default()), false)?;
            fd
        }
        s => {
            let net = s.net();
            let (conn, peer) = net.accept()?;
            let v6 = conn.v6;
            let fd = super::fs::install_handle(p, Arc::new(Handle::Socket(conn)), cloexec)?;
            write_addr(addr, len_ptr, Addr::Net(peer), v6)?;
            fd
        }
    };
    set_nonblock(p, new, flags & SOCK_NONBLOCK != 0);
    Ok(new)
}

/// Gathers an `iovec` array into one buffer.
fn gather(iov: u64, count: u64) -> Result<Vec<u8>, isize> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let mut out = Vec::new();
    for i in 0..count {
        let [base, len]: [u64; 2] = user::get(iov + i * 16)?;
        if len > 0 {
            out.extend_from_slice(&user::slice(base, len)?);
        }
    }
    Ok(out)
}

/// Spreads `data` over an `iovec` array.
fn scatter(iov: u64, count: u64, data: &[u8]) -> Result<(), isize> {
    let mut at = 0;
    for i in 0..count.min(1024) {
        if at >= data.len() {
            break;
        }
        let [base, len]: [u64; 2] = user::get(iov + i * 16)?;
        let n = (len as usize).min(data.len() - at);
        user::put_bytes(base, &data[at..at + n])?;
        at += n;
    }
    Ok(())
}

fn iov_capacity(iov: u64, count: u64) -> Result<usize, isize> {
    let mut total = 0usize;
    for i in 0..count.min(1024) {
        let [_, len]: [u64; 2] = user::get(iov + i * 16)?;
        total = total.saturating_add(len as usize);
    }
    Ok(total.min(user::MAX_BUF as usize))
}

/// Sends bytes (and descriptors) on any socket.
fn send_bytes(
    p: &Process,
    fd: u64,
    data: &[u8],
    fds: Vec<Arc<Handle>>,
    to: Option<Addr>,
    flags: u64,
) -> Result<u64, isize> {
    let nb = nonblock(p, fd, flags);
    let r = match sock(p, fd)? {
        Sock::Local(u) => {
            let to = match to {
                Some(Addr::Local(n)) => Some(n),
                Some(_) => return Err(EAFNOSUPPORT),
                None => None,
            };
            u.send(data, fds, to, nb).map(|n| n as u64)
        }
        s => {
            let net = s.net();
            let r = with_nonblock(net, nb, || match to {
                Some(Addr::Net(peer)) if net.kind != Kind::Tcp => net.send_to(data, peer),
                _ => net.write(data),
            });
            r.map(|n| n as u64)
        }
    };
    if r == Err(EPIPE) && flags & MSG_NOSIGNAL == 0 {
        let info = super::signal::Info { pid: p.pid, ..Default::default() };
        super::signal::send(p, super::signal::SIGPIPE, info, Some(crate::sched::current_id()));
    }
    r
}

/// Runs `f` with the socket made non-blocking (if `nb`).
fn with_nonblock<R>(s: &Socket, nb: bool, f: impl FnOnce() -> R) -> R {
    use aurora_abi::net::OPT_NONBLOCK;
    if !nb {
        return f();
    }
    let prev = s.option(OPT_NONBLOCK, 1).unwrap_or(0);
    let r = f();
    let _ = s.option(OPT_NONBLOCK, prev);
    r
}

pub fn sendto(p: &Process, fd: u64, buf: u64, len: u64, flags: u64, addr: u64, alen: u64) -> Result<u64, isize> {
    let data = user::slice(buf, len)?;
    let to = if addr != 0 { Some(read_addr(p, addr, alen)?) } else { None };
    send_bytes(p, fd, &data, Vec::new(), to, flags)
}

/// What a receive got: the bytes, the sender, descriptors, and flags.
struct Got {
    data: Vec<u8>,
    from: Option<Addr>,
    fds: Vec<Arc<Handle>>,
    truncated: bool,
    /// The sender, for a local socket that asked for it (`SO_PASSCRED`).
    cred: Option<u32>,
}

fn recv_bytes(p: &Process, fd: u64, cap: usize, flags: u64) -> Result<(Got, bool), isize> {
    let nb = nonblock(p, fd, flags);
    match sock(p, fd)? {
        Sock::Local(u) => {
            let mut buf = alloc::vec![0u8; cap];
            let r = u.recv(&mut buf, flags & MSG_PEEK != 0, nb)?;
            buf.truncate(r.len);
            let cred = u.passcred.load(core::sync::atomic::Ordering::Relaxed).then_some(r.pid);
            Ok((Got { data: buf, from: r.from.map(Addr::Local), fds: r.fds, truncated: r.truncated, cred }, false))
        }
        s => {
            let net = s.net();
            let mut buf = alloc::vec![0u8; cap];
            let v6 = net.v6;
            let (n, from) = with_nonblock(net, nb, || {
                if net.kind == Kind::Tcp {
                    net.read(&mut buf).map(|n| (n, None))
                } else {
                    net.recv_from(&mut buf).map(|(n, peer)| (n, Some(peer)))
                }
            })?;
            buf.truncate(n);
            Ok((Got { data: buf, from: from.map(Addr::Net), fds: Vec::new(), truncated: false, cred: None }, v6))
        }
    }
}

pub fn recvfrom(p: &Process, fd: u64, buf: u64, len: u64, flags: u64, addr: u64, alen: u64) -> Result<u64, isize> {
    let (got, v6) = recv_bytes(p, fd, len.min(user::MAX_BUF) as usize, flags)?;
    user::put_bytes(buf, &got.data)?;
    if let Some(a) = got.from {
        write_addr(addr, alen, a, v6)?;
    }
    // Descriptors sent to a plain `recv` are closed (Linux drops them too).
    Ok(got.data.len() as u64)
}

/// `struct msghdr` fields.
struct Msg {
    name: u64,
    namelen: u64,
    iov: u64,
    iovlen: u64,
    control: u64,
    controllen: u64,
}

fn read_msg(ptr: u64) -> Result<Msg, isize> {
    let raw: [u64; 7] = user::get(ptr)?;
    Ok(Msg {
        name: raw[0],
        namelen: raw[1] & 0xFFFF_FFFF,
        iov: raw[2],
        iovlen: raw[3],
        control: raw[4],
        controllen: raw[5],
    })
}

pub fn sendmsg(p: &Process, fd: u64, msg_ptr: u64, flags: u64) -> Result<u64, isize> {
    let m = read_msg(msg_ptr)?;
    let data = gather(m.iov, m.iovlen)?;
    let to = if m.name != 0 && m.namelen != 0 { Some(read_addr(p, m.name, m.namelen)?) } else { None };
    // Ancillary data: descriptors to pass (SCM_RIGHTS).
    let mut fds = Vec::new();
    let mut at = 0u64;
    while m.control != 0 && at + 16 <= m.controllen {
        let [len, kind]: [u64; 2] = user::get(m.control + at)?;
        let (level, ty) = (kind as u32 as u64, (kind >> 32) as i32);
        if len < 16 || at + len > m.controllen {
            break;
        }
        if level == SOL_SOCKET && ty == SCM_RIGHTS {
            for i in 0..(len - 16) / 4 {
                let n: i32 = user::get(m.control + at + 16 + i * 4)?;
                fds.push(p.fd(n as usize)?);
            }
        }
        at += (len + 7) & !7;
    }
    if !fds.is_empty() && matches!(sock(p, fd)?, Sock::Net(_)) {
        return Err(EINVAL);
    }
    send_bytes(p, fd, &data, fds, to, flags)
}

pub fn recvmsg(p: &Process, fd: u64, msg_ptr: u64, flags: u64) -> Result<u64, isize> {
    let m = read_msg(msg_ptr)?;
    let cap = iov_capacity(m.iov, m.iovlen)?;
    let (got, v6) = recv_bytes(p, fd, cap, flags)?;
    scatter(m.iov, m.iovlen, &got.data)?;
    let mut out_flags = if got.truncated { MSG_TRUNC } else { 0 };
    // Received descriptors become new ones in this process.
    let mut used = 0u64;
    if !got.fds.is_empty() {
        let room = m.controllen.saturating_sub(16) / 4;
        let take = (got.fds.len() as u64).min(room);
        if take < got.fds.len() as u64 {
            out_flags |= MSG_CTRUNC;
        }
        if take > 0 {
            let mut nums = Vec::new();
            for h in got.fds.into_iter().take(take as usize) {
                nums.push(super::fs::install_handle(p, h, flags & MSG_CMSG_CLOEXEC != 0)? as i32);
            }
            let len = 16 + 4 * nums.len() as u64;
            user::put(m.control, &[len, SOL_SOCKET | ((SCM_RIGHTS as u64) << 32)])?;
            let bytes: Vec<u8> = nums.iter().flat_map(|n| n.to_le_bytes()).collect();
            user::put_bytes(m.control + 16, &bytes)?;
            used = (len + 7) & !7;
        }
    }
    // The sender's credentials: struct ucred { pid, uid, gid } (WebKit's
    // process launcher checks them on the socket its children report on).
    if let Some(pid) = got.cred {
        if m.controllen >= used + 32 {
            user::put(m.control + used, &[28u64, SOL_SOCKET | ((SCM_CREDENTIALS as u64) << 32)])?;
            let uid = super::UID as u32;
            user::put(m.control + used + 16, &[pid, uid, uid])?;
            used += 32;
        } else {
            out_flags |= MSG_CTRUNC;
        }
    }
    // msg_namelen, msg_controllen and msg_flags.
    if m.name != 0 {
        match got.from {
            Some(a) => {
                user::put(msg_ptr + 8, &(m.namelen as u32))?;
                write_addr(m.name, msg_ptr + 8, a, v6)?;
            }
            None => user::put(msg_ptr + 8, &0u32)?,
        }
    }
    user::put(msg_ptr + 40, &used)?;
    user::put(msg_ptr + 48, &(out_flags as u32))?;
    Ok(got.data.len() as u64)
}

/// `sendmmsg`: several messages (glibc's resolver asks for A and AAAA at once).
pub fn sendmmsg(p: &Process, fd: u64, vec: u64, count: u64, flags: u64) -> Result<u64, isize> {
    let mut sent = 0;
    for i in 0..count.min(1024) {
        let entry = vec + i * 64;
        match sendmsg(p, fd, entry, flags) {
            Ok(n) => user::put(entry + 56, &(n as u32))?,
            Err(e) if sent == 0 => return Err(e),
            Err(_) => break,
        }
        sent += 1;
    }
    Ok(sent)
}

/// `recvmmsg`: several messages; after the first, only those already waiting.
pub fn recvmmsg(p: &Process, fd: u64, vec: u64, count: u64, flags: u64) -> Result<u64, isize> {
    let mut got = 0;
    for i in 0..count.min(1024) {
        let entry = vec + i * 64;
        let f = if got > 0 { flags | MSG_DONTWAIT } else { flags };
        match recvmsg(p, fd, entry, f) {
            Ok(n) => user::put(entry + 56, &(n as u32))?,
            Err(e) if got == 0 => return Err(e),
            Err(_) => break,
        }
        got += 1;
    }
    Ok(got)
}

pub fn shutdown(p: &Process, fd: u64, how: u64) -> Result<u64, isize> {
    match sock(p, fd)? {
        Sock::Local(u) => u.shutdown(how).map(|_| 0),
        s => {
            if how != 0 {
                s.net().shutdown()?;
            }
            Ok(0)
        }
    }
}

pub fn getsockname(p: &Process, fd: u64, addr: u64, len: u64) -> Result<u64, isize> {
    match sock(p, fd)? {
        Sock::Local(u) => write_addr(addr, len, Addr::Local(u.names().0.unwrap_or_default()), false),
        s => {
            let net = s.net();
            write_addr(addr, len, Addr::Net(net.info().0), net.v6)
        }
    }
    .map(|_| 0)
}

pub fn getpeername(p: &Process, fd: u64, addr: u64, len: u64) -> Result<u64, isize> {
    match sock(p, fd)? {
        Sock::Local(u) => {
            if u.readiness().2 && u.names().1.is_none() {
                return Err(ENOTCONN);
            }
            write_addr(addr, len, Addr::Local(u.names().1.unwrap_or_default()), false)
        }
        s => {
            let net = s.net();
            let (_, remote, state) = net.info();
            if state != aurora_abi::net::STATE_ESTABLISHED && net.kind == Kind::Tcp {
                return Err(ENOTCONN);
            }
            write_addr(addr, len, Addr::Net(remote), net.v6)
        }
    }
    .map(|_| 0)
}

pub fn setsockopt(p: &Process, fd: u64, level: u64, name: u64, val: u64, len: u64) -> Result<u64, isize> {
    let s = sock(p, fd)?;
    if level == SOL_SOCKET && matches!(name, 20 | 21) && len >= 16 {
        // SO_RCVTIMEO, SO_SNDTIMEO: a timeval.
        let [sec, usec]: [i64; 2] = user::get(val)?;
        if let Sock::Net(_) = s {
            let ms = sec.max(0) as u64 * 1000 + usec.max(0) as u64 / 1000;
            s.net().option(aurora_abi::net::OPT_TIMEOUT, ms)?;
        }
    }
    if level == SOL_SOCKET && name == SO_PASSCRED && len >= 4 {
        if let Sock::Local(u) = &s {
            let on: i32 = user::get(val)?;
            u.passcred.store(on != 0, core::sync::atomic::Ordering::Relaxed);
        }
    }
    // Everything else (reuse, keep-alive, buffer sizes, no-delay…) is accepted.
    Ok(0)
}

pub fn getsockopt(p: &Process, fd: u64, level: u64, name: u64, val: u64, len_ptr: u64) -> Result<u64, isize> {
    let s = sock(p, fd)?;
    let room: u32 = user::get(len_ptr)?;
    let int = |v: i32| -> Result<u64, isize> {
        user::put_bytes(val, &v.to_le_bytes()[..(room as usize).min(4)])?;
        user::put(len_ptr, &4u32).map(|_| 0)
    };
    if level != SOL_SOCKET {
        return int(0);
    }
    match name {
        // SO_TYPE
        3 => int(match &s {
            Sock::Local(u) => match u.ty {
                Type::Stream => 1,
                Type::Dgram => 2,
                Type::SeqPacket => 5,
            },
            Sock::Net(_) => match s.net().kind {
                Kind::Tcp => 1,
                _ => 2,
            },
        }),
        // SO_ERROR
        4 => int(match &s {
            Sock::Net(_) => s.net().error().unwrap_or(0).unsigned_abs() as i32,
            Sock::Local(_) => 0,
        }),
        // SO_SNDBUF, SO_RCVBUF
        7 | 8 => int(212_992),
        // SO_PEERCRED: pid, uid, gid
        17 => {
            let pid = match &s {
                Sock::Local(u) => u.peer_pid(),
                Sock::Net(_) => 0,
            };
            let creds = [pid, super::UID as u32, super::UID as u32];
            let bytes: Vec<u8> = creds.iter().flat_map(|v| v.to_le_bytes()).collect();
            user::put_bytes(val, &bytes[..(room as usize).min(12)])?;
            user::put(len_ptr, &12u32).map(|_| 0)
        }
        // SO_ACCEPTCONN
        30 => int(matches!(&s, Sock::Local(u) if u.listening()) as i32),
        // SO_DOMAIN
        39 => int(match &s {
            Sock::Local(_) => 1,
            Sock::Net(_) => {
                if s.net().v6 {
                    10
                } else {
                    2
                }
            }
        }),
        _ => int(0),
    }
}
