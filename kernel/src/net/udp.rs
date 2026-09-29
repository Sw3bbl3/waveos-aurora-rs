//! UDP: datagrams to and from bound ports (and DHCP's port 68).

use super::wire::*;
use super::{Ip, Stack, PROTO_UDP};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;

const QUEUE: usize = 128;
const EPHEMERAL: core::ops::Range<u16> = 49152..65535;

#[derive(Default)]
pub struct Port {
    /// Received datagrams: (source address, source port, payload).
    pub queue: VecDeque<(Ip, u16, Vec<u8>)>,
}

#[derive(Default)]
pub struct Table {
    pub ports: BTreeMap<u16, Port>,
    next: u16,
}

impl Table {
    /// Claims `port` (0 = any free ephemeral port).
    pub fn bind(&mut self, port: u16) -> Result<u16, isize> {
        if port != 0 {
            if self.ports.contains_key(&port) || port == 68 {
                return Err(EADDRINUSE);
            }
            self.ports.insert(port, Port::default());
            return Ok(port);
        }
        if self.next == 0 {
            self.next = EPHEMERAL.start + (crate::random::u32() % (EPHEMERAL.end - EPHEMERAL.start) as u32) as u16;
        }
        for _ in 0..(EPHEMERAL.end - EPHEMERAL.start) {
            let p = self.next;
            self.next = if p + 1 >= EPHEMERAL.end { EPHEMERAL.start } else { p + 1 };
            if !self.ports.contains_key(&p) {
                self.ports.insert(p, Port::default());
                return Ok(p);
            }
        }
        Err(EADDRINUSE)
    }

    pub fn unbind(&mut self, port: u16) {
        self.ports.remove(&port);
    }
}

pub fn input(s: &mut Stack, i: usize, src: Ip, dst: Ip, p: &[u8]) {
    if p.len() < 8 {
        return;
    }
    let len = be16(p, 4) as usize;
    if len < 8 || len > p.len() {
        return;
    }
    let p = &p[..len];
    if be16(p, 6) != 0 && transport_checksum(src, dst, PROTO_UDP, p) != 0 {
        return;
    }
    let sport = be16(p, 0);
    let dport = be16(p, 2);
    if dport == 68 {
        super::dhcp::input(s, i, &p[8..]);
        return;
    }
    if let Some(port) = s.udp.ports.get_mut(&dport) {
        if port.queue.len() < QUEUE {
            port.queue.push_back((src, sport, p[8..].to_vec()));
        }
    }
}

/// A UDP datagram with its header and checksum.
pub fn datagram(src: Ip, dst: Ip, sport: u16, dport: u16, data: &[u8]) -> Vec<u8> {
    let mut d = vec![0u8; 8 + data.len()];
    put16(&mut d, 0, sport);
    put16(&mut d, 2, dport);
    let len = d.len() as u16;
    put16(&mut d, 4, len);
    d[8..].copy_from_slice(data);
    let c = transport_checksum(src, dst, PROTO_UDP, &d);
    put16(&mut d, 6, if c == 0 { 0xFFFF } else { c });
    d
}

pub fn send(s: &mut Stack, sport: u16, dst: Ip, dport: u16, data: &[u8]) -> Result<(), isize> {
    if data.len() > super::MTU - 28 {
        return Err(EMSGSIZE);
    }
    let src = s.source_for(dst).ok_or(ENETUNREACH)?;
    let d = datagram(src, dst, sport, dport, data);
    s.send_ip(dst, PROTO_UDP, &d)
}
