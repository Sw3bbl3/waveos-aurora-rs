//! DHCP client (RFC 2131): discover → offer → request → ack, then renew at
//! half the lease. Configures the interface's address, netmask, gateway and
//! DNS servers.

use super::wire::*;
use super::{Ip, Stack, ANY, PROTO_UDP};
use alloc::vec::Vec;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Init,
    Selecting,
    Requesting,
    Bound,
}

pub struct Client {
    state: State,
    xid: u32,
    /// When to send the next message (retries back off to 16 s).
    next: u64,
    tries: u32,
    offered: Ip,
    server: Ip,
    lease_until: u64,
    renew_at: u64,
}

impl Client {
    pub fn new() -> Client {
        Client { state: State::Init, xid: 0, next: 0, tries: 0, offered: ANY, server: ANY, lease_until: 0, renew_at: 0 }
    }

    pub fn bound(&self) -> bool {
        self.state == State::Bound
    }

    pub fn lease_left(&self, now: u64) -> u32 {
        (self.lease_until.saturating_sub(now) / 1000) as u32
    }
}

fn message(mac: [u8; 6], xid: u32, kind: u8, requested: Ip, server: Ip) -> Vec<u8> {
    let mut m = alloc::vec![0u8; 240];
    m[0] = 1; // boot request
    m[1] = 1; // Ethernet
    m[2] = 6;
    put32(&mut m, 4, xid);
    put16(&mut m, 10, 0x8000); // please broadcast replies (we have no address yet)
    m[28..34].copy_from_slice(&mac);
    m[236..240].copy_from_slice(&[99, 130, 83, 99]);
    m.extend_from_slice(&[53, 1, kind]);
    // Client identifier: hardware type + MAC.
    m.extend_from_slice(&[61, 7, 1]);
    m.extend_from_slice(&mac);
    if requested != ANY {
        m.extend_from_slice(&[50, 4]);
        m.extend_from_slice(&requested);
    }
    if server != ANY {
        m.extend_from_slice(&[54, 4]);
        m.extend_from_slice(&server);
    }
    m.extend_from_slice(&[12, 6]);
    m.extend_from_slice(b"waveos");
    // Subnet mask, router, DNS, domain name, lease time, server id.
    m.extend_from_slice(&[55, 6, 1, 3, 6, 15, 51, 54]);
    m.push(255);
    m
}

fn send(s: &mut Stack, i: usize, kind: u8) {
    let c = s.ifaces[i].dhcp.as_ref().unwrap();
    let (requested, server) = if kind == REQUEST { (c.offered, c.server) } else { (ANY, ANY) };
    let msg = message(s.ifaces[i].mac, c.xid, kind, requested, server);
    let d = super::udp::datagram(ANY, super::BROADCAST, 68, 67, &msg);
    s.send_broadcast(i, ANY, PROTO_UDP, &d);
}

pub fn tick(s: &mut Stack, now: u64) {
    for i in 0..s.ifaces.len() {
        let Some(c) = s.ifaces[i].dhcp.as_mut() else { continue };
        if c.state == State::Bound && now >= c.lease_until {
            log!("dhcp", "{}: lease expired", s.ifaces[i].name);
            let iface = &mut s.ifaces[i];
            iface.ip = ANY;
            iface.gateway = ANY;
            iface.dhcp.as_mut().unwrap().state = State::Init;
            continue;
        }
        if c.state == State::Bound && now < c.renew_at {
            continue;
        }
        if now < c.next {
            continue;
        }
        let kind = match c.state {
            State::Init | State::Selecting => {
                if c.state == State::Init {
                    c.xid = crate::random::u32();
                    c.tries = 0;
                    c.state = State::Selecting;
                }
                DISCOVER
            }
            // Renewing re-requests the address we hold.
            State::Bound => {
                c.xid = crate::random::u32();
                c.tries = 0;
                c.state = State::Requesting;
                REQUEST
            }
            State::Requesting => {
                if c.tries >= 4 {
                    c.state = State::Init;
                    continue;
                }
                REQUEST
            }
        };
        c.tries += 1;
        c.next = now + (2000u64 << c.tries.min(3));
        send(s, i, kind);
    }
}

pub fn input(s: &mut Stack, i: usize, m: &[u8]) {
    if m.len() < 240 || m[0] != 2 || m[236..240] != [99, 130, 83, 99] {
        return;
    }
    let mac = s.ifaces[i].mac;
    let Some(c) = s.ifaces[i].dhcp.as_mut() else { return };
    if be32(m, 4) != c.xid || m[28..34] != mac {
        return;
    }
    let yiaddr = ip_at(m, 16);
    let (mut kind, mut mask, mut router, mut server, mut lease) = (0u8, ANY, ANY, ANY, 3600u32);
    let mut dns = Vec::new();
    let mut o = 240;
    while o < m.len() {
        let code = m[o];
        if code == 255 {
            break;
        }
        if code == 0 {
            o += 1;
            continue;
        }
        let Some(&len) = m.get(o + 1) else { break };
        let v = match m.get(o + 2..o + 2 + len as usize) {
            Some(v) => v,
            None => break,
        };
        match (code, len) {
            (53, 1) => kind = v[0],
            (1, 4) => mask = ip_at(v, 0),
            (3, l) if l >= 4 => router = ip_at(v, 0),
            (6, l) if l >= 4 => dns = v.chunks_exact(4).map(|d| ip_at(d, 0)).collect(),
            (51, 4) => lease = be32(v, 0),
            (54, 4) => server = ip_at(v, 0),
            _ => {}
        }
        o += 2 + len as usize;
    }
    let now = crate::time::uptime_ms();
    match (c.state, kind) {
        (State::Selecting, OFFER) => {
            c.offered = yiaddr;
            c.server = server;
            c.state = State::Requesting;
            c.tries = 1;
            c.next = now + 2000;
            send(s, i, REQUEST);
        }
        (State::Requesting, ACK) => {
            c.state = State::Bound;
            c.lease_until = now + lease as u64 * 1000;
            c.renew_at = now + lease as u64 * 500;
            let iface = &mut s.ifaces[i];
            let fresh = iface.ip != yiaddr;
            iface.ip = yiaddr;
            iface.mask = if mask == ANY { [255, 255, 255, 0] } else { mask };
            iface.gateway = router;
            iface.dns = dns;
            if fresh {
                log!(
                    "dhcp",
                    "{}: {} / {} via {}, DNS {}, lease {} s",
                    iface.name,
                    ip_str(iface.ip),
                    ip_str(iface.mask),
                    ip_str(iface.gateway),
                    iface.dns.iter().map(|d| ip_str(*d)).collect::<Vec<_>>().join(", "),
                    lease
                );
                crate::gui::network_changed();
            }
        }
        (State::Requesting, NAK) => {
            c.state = State::Init;
            c.next = now + 1000;
            log!("dhcp", "{}: server refused the address; starting over", s.ifaces[i].name);
        }
        _ => {}
    }
}
