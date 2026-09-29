//! Sockets: what a program's socket file descriptor refers to.
//!
//! Blocking calls wait for the network task's next batch of work (`GEN`
//! changes) and re-check, until their condition holds, the socket's timeout
//! passes, or the process is interrupted.

use super::{tcp, udp, with, Ip, Stack, ANY, PROTO_ICMP};
use crate::sync::Mutex;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use aurora_abi::err::*;
use aurora_abi::SockAddr;
use core::sync::atomic::Ordering;

/// Connecting gives up after this long when the socket has no timeout.
const CONNECT_TIMEOUT: u64 = 20_000;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Tcp,
    Udp,
    Icmp,
}

struct Inner {
    tcp: Option<tcp::ConnId>,
    listening: bool,
    /// Local port (TCP: from `bind`, before `listen`; UDP: bound port).
    port: Option<u16>,
    icmp_id: Option<u16>,
    peer: Option<(Ip, u16)>,
    timeout: u64,
    nonblock: bool,
}

pub struct Socket {
    pub kind: Kind,
    inner: Mutex<Inner>,
}

fn addr(a: &SockAddr) -> (Ip, u16) {
    (a.ip, a.port)
}

fn sockaddr((ip, port): (Ip, u16)) -> SockAddr {
    SockAddr::new(ip, port)
}

impl Socket {
    pub fn new(kind: Kind) -> Socket {
        let mut inner =
            Inner { tcp: None, listening: false, port: None, icmp_id: None, peer: None, timeout: 0, nonblock: false };
        if kind == Kind::Icmp {
            let id = with(|s| {
                let mut id = crate::random::u32() as u16;
                while s.icmp.contains_key(&id) {
                    id = id.wrapping_add(1);
                }
                s.icmp.insert(id, VecDeque::new());
                id
            });
            inner.icmp_id = Some(id);
        }
        Socket { kind, inner: Mutex::new(inner) }
    }

    /// Waits until `ready` holds (checked with the stack locked).
    fn wait(&self, timeout: u64, nonblock: bool, mut ready: impl FnMut(&mut Stack) -> bool) -> Result<(), isize> {
        let deadline = (timeout != 0).then(|| crate::time::uptime_ms() + timeout);
        loop {
            let gen = super::GEN.load(Ordering::Acquire);
            if with(&mut ready) {
                return Ok(());
            }
            if nonblock {
                return Err(EAGAIN);
            }
            if crate::proc::interrupted() {
                return Err(EINTR);
            }
            let left = match deadline {
                Some(d) => {
                    let now = crate::time::uptime_ms();
                    if now >= d {
                        return Err(ETIMEDOUT);
                    }
                    d - now
                }
                None => 1000,
            };
            super::CHANGED
                .wait(left.min(100), || super::GEN.load(Ordering::Acquire) != gen || crate::proc::interrupted());
        }
    }

    fn settings(&self) -> (u64, bool) {
        let i = self.inner.lock();
        (i.timeout, i.nonblock)
    }

    pub fn bind(&self, a: &SockAddr) -> Result<(), isize> {
        let mut i = self.inner.lock();
        if i.port.is_some() || i.tcp.is_some() {
            return Err(EINVAL);
        }
        match self.kind {
            Kind::Udp => i.port = Some(with(|s| s.udp.bind(a.port))?),
            Kind::Tcp => {
                if a.port == 0 {
                    return Err(EINVAL);
                }
                i.port = Some(a.port);
            }
            Kind::Icmp => return Err(EINVAL),
        }
        Ok(())
    }

    pub fn connect(&self, a: &SockAddr) -> Result<(), isize> {
        let remote = addr(a);
        match self.kind {
            Kind::Tcp => {
                let id = {
                    let mut i = self.inner.lock();
                    if i.tcp.is_some() {
                        return Err(EISCONN);
                    }
                    let id = with(|s| tcp::connect(s, remote))?;
                    i.tcp = Some(id);
                    id
                };
                let (timeout, nonblock) = self.settings();
                let timeout = if timeout == 0 { CONNECT_TIMEOUT } else { timeout };
                self.wait(timeout, nonblock, |s| {
                    tcp::state(s, id).is_none_or(|(st, ..)| st != tcp::State::SynSent && st != tcp::State::SynReceived)
                })?;
                match with(|s| tcp::state(s, id)) {
                    Some((_, _, _, Some(e))) => Err(e),
                    Some((tcp::State::Established | tcp::State::CloseWait, ..)) => Ok(()),
                    _ => Err(ECONNREFUSED),
                }
            }
            Kind::Udp => {
                let mut i = self.inner.lock();
                if i.port.is_none() {
                    i.port = Some(with(|s| s.udp.bind(0))?);
                }
                i.peer = Some(remote);
                Ok(())
            }
            Kind::Icmp => {
                self.inner.lock().peer = Some(remote);
                Ok(())
            }
        }
    }

    pub fn listen(&self, backlog: usize) -> Result<(), isize> {
        if self.kind != Kind::Tcp {
            return Err(EINVAL);
        }
        let mut i = self.inner.lock();
        let port = i.port.ok_or(EINVAL)?;
        if i.tcp.is_some() {
            return Err(EISCONN);
        }
        i.tcp = Some(with(|s| tcp::listen(s, port, backlog))?);
        i.listening = true;
        Ok(())
    }

    pub fn accept(&self) -> Result<(Socket, SockAddr), isize> {
        let (listener, timeout, nonblock) = {
            let i = self.inner.lock();
            if !i.listening {
                return Err(EINVAL);
            }
            (i.tcp.unwrap(), i.timeout, i.nonblock)
        };
        let mut got = None;
        self.wait(timeout, nonblock, |s| {
            got = tcp::accept(s, listener);
            got.is_some()
        })?;
        let id = got.unwrap();
        let remote = with(|s| tcp::state(s, id).map(|t| t.2)).unwrap_or((ANY, 0));
        let sock = Socket::new(Kind::Tcp);
        sock.inner.lock().tcp = Some(id);
        Ok((sock, sockaddr(remote)))
    }

    /// TCP: queues all of `data` (waiting for room). UDP/ICMP: one datagram to the peer.
    pub fn write(&self, data: &[u8]) -> Result<usize, isize> {
        if self.kind != Kind::Tcp {
            let peer = self.inner.lock().peer.ok_or(ENOTCONN)?;
            return self.send_to(data, &sockaddr(peer));
        }
        let (id, timeout, nonblock) = {
            let i = self.inner.lock();
            (i.tcp.ok_or(ENOTCONN)?, i.timeout, i.nonblock)
        };
        let mut done = 0;
        while done < data.len() {
            let n = with(|s| tcp::send(s, id, &data[done..]))?;
            done += n;
            if done < data.len() {
                match self.wait(timeout, nonblock, |s| tcp::write_ready(s, id)) {
                    Ok(()) => {}
                    Err(e) if done > 0 && (e == EAGAIN || e == EINTR || e == ETIMEDOUT) => break,
                    Err(e) => return Err(e),
                }
            }
        }
        Ok(done)
    }

    /// TCP: whatever has arrived (0 = the peer finished). UDP/ICMP: one datagram.
    pub fn read(&self, out: &mut [u8]) -> Result<usize, isize> {
        if self.kind != Kind::Tcp {
            return self.recv_from(out).map(|(n, _)| n);
        }
        let (id, timeout, nonblock) = {
            let i = self.inner.lock();
            (i.tcp.ok_or(ENOTCONN)?, i.timeout, i.nonblock)
        };
        loop {
            match with(|s| tcp::recv(s, id, out)) {
                Err(EAGAIN) => self.wait(timeout, nonblock, |s| tcp::read_ready(s, id))?,
                r => return r,
            }
        }
    }

    pub fn send_to(&self, data: &[u8], to: &SockAddr) -> Result<usize, isize> {
        let dst = addr(to);
        match self.kind {
            Kind::Udp => {
                let port = {
                    let mut i = self.inner.lock();
                    match i.port {
                        Some(p) => p,
                        None => {
                            let p = with(|s| s.udp.bind(0))?;
                            i.port = Some(p);
                            p
                        }
                    }
                };
                with(|s| udp::send(s, port, dst.0, dst.1, data))?;
                super::poke();
                Ok(data.len())
            }
            Kind::Icmp => {
                if data.len() < 8 || data[0] != 8 {
                    return Err(EINVAL); // only echo requests
                }
                let id = self.inner.lock().icmp_id.unwrap();
                let mut m = data.to_vec();
                m[4..6].copy_from_slice(&id.to_be_bytes());
                m[2] = 0;
                m[3] = 0;
                let c = super::wire::checksum(&m);
                m[2..4].copy_from_slice(&c.to_be_bytes());
                with(|s| s.send_ip(dst.0, PROTO_ICMP, &m))?;
                super::poke();
                Ok(data.len())
            }
            Kind::Tcp => Err(EINVAL),
        }
    }

    pub fn recv_from(&self, out: &mut [u8]) -> Result<(usize, SockAddr), isize> {
        let (timeout, nonblock) = self.settings();
        let (port, icmp) = {
            let i = self.inner.lock();
            (i.port, i.icmp_id)
        };
        let mut got: Option<(Ip, u16, Vec<u8>)> = None;
        self.wait(timeout, nonblock, |s| {
            got = match (self.kind, port, icmp) {
                (Kind::Udp, Some(p), _) => s.udp.ports.get_mut(&p).and_then(|q| q.queue.pop_front()),
                (Kind::Icmp, _, Some(id)) => s.icmp.get_mut(&id).and_then(|q| q.pop_front()).map(|(ip, m)| (ip, 0, m)),
                _ => None,
            };
            got.is_some()
        })?;
        let (ip, port, data) = got.unwrap();
        let n = data.len().min(out.len());
        out[..n].copy_from_slice(&data[..n]);
        Ok((n, SockAddr::new(ip, port)))
    }

    pub fn shutdown(&self) -> Result<(), isize> {
        let id = self.inner.lock().tcp.ok_or(ENOTCONN)?;
        with(|s| tcp::shutdown(s, id));
        Ok(())
    }

    pub fn option(&self, opt: u64, value: u64) -> Result<u64, isize> {
        use aurora_abi::net::*;
        let mut i = self.inner.lock();
        match opt {
            OPT_TIMEOUT => Ok(core::mem::replace(&mut i.timeout, value)),
            OPT_NONBLOCK => Ok(core::mem::replace(&mut i.nonblock, value != 0) as u64),
            OPT_READABLE => {
                let (tcp, port, icmp) = (i.tcp, i.port, i.icmp_id);
                drop(i);
                Ok(with(|s| match (self.kind, tcp, port, icmp) {
                    (Kind::Tcp, Some(id), ..) => tcp::readable(s, id) as u64,
                    (Kind::Udp, _, Some(p), _) => {
                        s.udp.ports.get(&p).and_then(|q| q.queue.front()).map_or(0, |d| d.2.len() as u64)
                    }
                    (Kind::Icmp, _, _, Some(id)) => {
                        s.icmp.get(&id).and_then(|q| q.front()).map_or(0, |d| d.1.len() as u64)
                    }
                    _ => 0,
                }))
            }
            _ => Err(EINVAL),
        }
    }

    /// (local, remote, state) for `sock_info`.
    pub fn info(&self) -> (SockAddr, SockAddr, u64) {
        use aurora_abi::net::*;
        let i = self.inner.lock();
        match (self.kind, i.tcp) {
            (Kind::Tcp, Some(id)) => {
                let listening = i.listening;
                drop(i);
                with(|s| match tcp::state(s, id) {
                    Some((st, local, remote, _)) => {
                        let state = match st {
                            _ if listening => STATE_LISTEN,
                            tcp::State::SynSent | tcp::State::SynReceived => STATE_CONNECTING,
                            tcp::State::Established => STATE_ESTABLISHED,
                            tcp::State::Closed => STATE_CLOSED,
                            _ => STATE_CLOSING,
                        };
                        (sockaddr(local), sockaddr(remote), state)
                    }
                    None => (SockAddr::default(), SockAddr::default(), STATE_CLOSED),
                })
            }
            _ => (SockAddr::new(ANY, i.port.unwrap_or(0)), i.peer.map(sockaddr).unwrap_or_default(), STATE_CLOSED),
        }
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let i = self.inner.lock();
        let (tcp, port, icmp, kind) = (i.tcp, i.port, i.icmp_id, self.kind);
        drop(i);
        with(|s| {
            if let Some(id) = tcp {
                tcp::close(s, id);
            }
            if kind == Kind::Udp {
                if let Some(p) = port {
                    s.udp.unbind(p);
                }
            }
            if let Some(id) = icmp {
                s.icmp.remove(&id);
            }
        });
        super::poke();
    }
}
