//! IPv6: addresses from stateless autoconfiguration (RFC 4862) using router
//! advertisements, neighbor discovery (RFC 4861) in place of ARP, DNS
//! servers from the RDNSS option (RFC 8106), and ICMPv6 echo and errors.
//!
//! Not done: duplicate address detection, multicast listener reports (the
//! card receives all multicast), extension headers other than skipping
//! hop-by-hop, routing and destination options, and fragments.

use super::wire::*;
use super::{tcp, udp, Addr, Ip6, Stack, ETH_IP6, MTU, PROTO_TCP, PROTO_UDP, RX, WAKE};
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub const PROTO_ICMP6: u8 = 58;
pub const LOOPBACK6: Ip6 = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const ALL_NODES: Ip6 = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
const ALL_ROUTERS: Ip6 = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2];

/// One interface's IPv6 settings.
pub struct Config {
    /// Addresses with their prefix lengths and when they expire (uptime ms;
    /// `u64::MAX` = never). The link-local one comes first.
    pub addrs: Vec<(Ip6, u8, u64)>,
    /// On-link prefixes from routers: (prefix, length, expiry).
    pub prefixes: Vec<(Ip6, u8, u64)>,
    /// The default router (its link-local address) and when that expires.
    pub router: Option<(Ip6, u64)>,
    pub dns: Vec<Ip6>,
    /// Router solicitations sent, and when the last went.
    solicited: u8,
    last_solicit: u64,
}

impl Config {
    pub fn loopback() -> Config {
        Config {
            addrs: vec![(LOOPBACK6, 128, u64::MAX)],
            prefixes: Vec::new(),
            router: None,
            dns: Vec::new(),
            solicited: u8::MAX,
            last_solicit: 0,
        }
    }

    /// A card's link-local address, from its MAC (modified EUI-64).
    pub fn new(mac: [u8; 6]) -> Config {
        let mut ll = [0u8; 16];
        ll[0] = 0xFE;
        ll[1] = 0x80;
        ll[8..].copy_from_slice(&interface_id(mac));
        Config {
            addrs: vec![(ll, 64, u64::MAX)],
            prefixes: Vec::new(),
            router: None,
            dns: Vec::new(),
            solicited: 0,
            last_solicit: 0,
        }
    }

    /// A global (not link-local) address, if autoconfiguration gave one.
    pub fn global(&self) -> Option<Ip6> {
        self.addrs.iter().find(|(a, _, _)| !link_local(a) && *a != LOOPBACK6).map(|a| a.0)
    }

    fn owns(&self, a: &Ip6) -> bool {
        self.addrs.iter().any(|(x, _, _)| x == a)
    }

    fn on_link(&self, a: &Ip6) -> bool {
        link_local(a) || self.prefixes.iter().any(|(p, len, _)| same_prefix(p, a, *len))
    }
}

fn interface_id(mac: [u8; 6]) -> [u8; 8] {
    [mac[0] ^ 2, mac[1], mac[2], 0xFF, 0xFE, mac[3], mac[4], mac[5]]
}

pub fn link_local(a: &Ip6) -> bool {
    a[0] == 0xFE && a[1] & 0xC0 == 0x80
}

fn multicast(a: &Ip6) -> bool {
    a[0] == 0xFF
}

fn same_prefix(p: &Ip6, a: &Ip6, len: u8) -> bool {
    let (bytes, bits) = ((len / 8) as usize, len % 8);
    p[..bytes] == a[..bytes] && (bits == 0 || (p[bytes] ^ a[bytes]) >> (8 - bits) == 0)
}

/// ff02::1:ffXX:XXXX, where a neighbor solicitation for `a` goes.
fn solicited_node(a: &Ip6) -> Ip6 {
    let mut m = [0xFF, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xFF, 0, 0, 0];
    m[13..].copy_from_slice(&a[13..]);
    m
}

fn multicast_mac(a: &Ip6) -> [u8; 6] {
    [0x33, 0x33, a[12], a[13], a[14], a[15]]
}

fn addr6(b: &[u8], o: usize) -> Ip6 {
    b[o..o + 16].try_into().unwrap()
}

/// A packet waiting for its next hop's link-layer address.
pub struct Pending {
    iface: usize,
    hop: Ip6,
    packet: Vec<u8>,
    since: u64,
    asked: u8,
    last_ask: u64,
}

impl Stack {
    pub(super) fn ip6_input(&mut self, i: usize, p: &[u8]) {
        if p.len() < 40 || p[0] >> 4 != 6 || i >= self.ifaces.len() {
            return;
        }
        let len = be16(p, 4) as usize;
        if 40 + len > p.len() {
            return;
        }
        let (src, dst) = (addr6(p, 8), addr6(p, 24));
        let hop_limit = p[7];
        let iface = &self.ifaces[i];
        let for_us = iface.v6.owns(&dst)
            || dst == ALL_NODES
            || iface.v6.addrs.iter().any(|(a, _, _)| solicited_node(a) == dst)
            || iface.loopback();
        if !for_us {
            return;
        }
        // Skip the extension headers we can.
        let (mut next, mut off) = (p[6], 40usize);
        while matches!(next, 0 | 43 | 60) {
            if off + 8 > 40 + len {
                return;
            }
            next = p[off];
            off += (p[off + 1] as usize + 1) * 8;
        }
        if off > 40 + len {
            return;
        }
        let payload = &p[off..40 + len];
        match next {
            PROTO_ICMP6 => self.icmp6_input(i, src, dst, hop_limit, payload),
            PROTO_UDP => udp::input(self, i, src, dst, payload),
            PROTO_TCP => tcp::input(self, src, dst, payload),
            _ => {}
        }
    }

    fn icmp6_input(&mut self, i: usize, src: Ip6, dst: Ip6, hop_limit: u8, p: &[u8]) {
        if p.len() < 8 || transport_checksum(src, dst, PROTO_ICMP6, p) != 0 {
            return;
        }
        let now = crate::time::uptime_ms();
        match p[0] {
            // Destination unreachable: TCP gives up connecting.
            1 if p.len() >= 8 + 40 + 4 => {
                let inner = &p[8..];
                if inner[6] == PROTO_TCP {
                    self.tcp.unreachable(addr6(inner, 24), be16(inner, 40), be16(inner, 42));
                }
            }
            // Echo request: answer with the same payload.
            128 => {
                let mut r = p.to_vec();
                r[0] = 129;
                let reply_src = if multicast(&dst) { self.ifaces[i].v6.addrs[0].0 } else { dst };
                let _ = self.send_icmp6_from(reply_src, src, &mut r, 64);
            }
            129 => {
                if let Some(q) = self.icmp6.get_mut(&be16(p, 4)) {
                    if q.len() < 64 {
                        q.push_back((src, p.to_vec()));
                    }
                }
            }
            // Router advertisement.
            134 if hop_limit == 255 && link_local(&src) && p.len() >= 16 => self.router_advert(i, src, p, now),
            // Neighbor solicitation: answer for our addresses.
            135 if hop_limit == 255 && p.len() >= 24 => {
                let target = addr6(p, 8);
                if !self.ifaces[i].v6.owns(&target) {
                    return;
                }
                if let Some(mac) = link_option(&p[24..], 1) {
                    self.learn(src, mac, now);
                }
                let mut na = vec![0u8; 32];
                na[0] = 136;
                // Solicited and override (and router: never).
                na[4] = 0x60;
                na[8..24].copy_from_slice(&target);
                na[24] = 2;
                na[25] = 1;
                na[26..32].copy_from_slice(&self.ifaces[i].mac);
                let to = if src == [0; 16] { ALL_NODES } else { src };
                let _ = self.send_icmp6_from(target, to, &mut na, 255);
            }
            // Neighbor advertisement.
            136 if hop_limit == 255 && p.len() >= 24 => {
                let target = addr6(p, 8);
                let mac = link_option(&p[24..], 2).or_else(|| self.neighbors.get(&target).map(|n| n.0));
                if let Some(mac) = mac {
                    self.learn(target, mac, now);
                }
            }
            _ => {}
        }
    }

    fn learn(&mut self, ip: Ip6, mac: [u8; 6], now: u64) {
        self.neighbors.insert(ip, (mac, now + 600_000));
        let (ready, rest): (Vec<_>, Vec<_>) = self.pending6.drain(..).partition(|p| p.hop == ip);
        self.pending6 = rest;
        for p in ready {
            self.send_frame(p.iface, mac, ETH_IP6, &p.packet);
        }
    }

    fn router_advert(&mut self, i: usize, src: Ip6, p: &[u8], now: u64) {
        let lifetime = be16(p, 6) as u64;
        let mac = self.ifaces[i].mac;
        let mut learned = None;
        let mut dns = Vec::new();
        let v6 = &mut self.ifaces[i].v6;
        v6.router = (lifetime > 0).then(|| (src, now + lifetime * 1000));
        let mut o = 16;
        while o + 2 <= p.len() {
            let (kind, len) = (p[o], p[o + 1] as usize * 8);
            if len == 0 || o + len > p.len() {
                break;
            }
            let opt = &p[o..o + len];
            match kind {
                1 if len >= 8 => learned = Some(opt[2..8].try_into().unwrap()),
                // Prefix information.
                3 if len >= 32 => {
                    let (plen, flags) = (opt[2], opt[3]);
                    let valid = be32(opt, 4) as u64;
                    let prefix = addr6(opt, 16);
                    let until = if valid == 0xFFFF_FFFF { u64::MAX } else { now + valid * 1000 };
                    if flags & 0x80 != 0 {
                        v6.prefixes.retain(|(x, l, _)| !(x == &prefix && *l == plen));
                        if valid > 0 {
                            v6.prefixes.push((prefix, plen, until));
                        }
                    }
                    // Autonomous, /64: make an address from it.
                    if flags & 0x40 != 0 && plen == 64 && !link_local(&prefix) {
                        let mut a = prefix;
                        a[8..].copy_from_slice(&interface_id(mac));
                        v6.addrs.retain(|(x, _, _)| *x != a);
                        if valid > 0 {
                            if !v6.addrs.iter().any(|x| x.0 == a) {
                                log!("net", "eth{}: IPv6 {}/64", i - 1, ip6_str(&a));
                            }
                            v6.addrs.push((a, 64, until));
                        }
                    }
                }
                // Recursive DNS servers.
                25 if len >= 24 => {
                    for k in 0..(len - 8) / 16 {
                        dns.push(addr6(opt, 8 + k * 16));
                    }
                }
                _ => {}
            }
            o += len;
        }
        if !dns.is_empty() {
            v6.dns = dns;
        }
        if let Some(mac) = learned {
            self.learn(src, mac, now);
        }
    }

    /// Sends an ICMPv6 message (checksum filled in) from `src` to `dst`.
    fn send_icmp6_from(&mut self, src: Ip6, dst: Ip6, msg: &mut [u8], hop_limit: u8) -> Result<(), isize> {
        put16(msg, 2, 0);
        let c = transport_checksum(src, dst, PROTO_ICMP6, msg);
        put16(msg, 2, c);
        self.send_ip6_from(src, dst, PROTO_ICMP6, msg, hop_limit)
    }

    /// An echo request from a ping socket: identifier set, checksummed, sent.
    pub fn send_echo6(&mut self, dst: Ip6, id: u16, msg: &mut [u8]) -> Result<(), isize> {
        let src = self.source6(&dst).ok_or(ENETUNREACH)?;
        put16(msg, 4, id);
        self.send_icmp6_from(src, dst, msg, 64)
    }

    /// The best source address for `dst` (RFC 6724, simplified).
    pub(super) fn source6(&self, dst: &Ip6) -> Option<Addr> {
        // To ourselves: from the same address.
        if *dst == LOOPBACK6 || self.ifaces.iter().any(|f| f.v6.owns(dst)) {
            return Some(*dst);
        }
        let (i, _) = self.route6(dst)?;
        let v6 = &self.ifaces[i].v6;
        if link_local(dst) || multicast(dst) {
            return v6.addrs.first().map(|a| a.0);
        }
        v6.global()
    }

    /// The interface and next hop for `dst`.
    fn route6(&self, dst: &Ip6) -> Option<(usize, Ip6)> {
        if *dst == LOOPBACK6 || self.ifaces.iter().any(|f| f.v6.owns(dst)) {
            return Some((0, *dst));
        }
        let cards = || self.ifaces.iter().enumerate().filter(|(_, f)| !f.loopback());
        if multicast(dst) || link_local(dst) {
            return cards().next().map(|(i, _)| (i, *dst));
        }
        if let Some((i, _)) = cards().find(|(_, f)| f.v6.on_link(dst)) {
            return Some((i, *dst));
        }
        cards().find_map(|(i, f)| f.v6.router.map(|(r, _)| (i, r)))
    }

    pub(super) fn send_ip6(&mut self, dst: Ip6, proto: u8, payload: &[u8]) -> Result<(), isize> {
        let src = self.source6(&dst).ok_or(ENETUNREACH)?;
        self.send_ip6_from(src, dst, proto, payload, 64)
    }

    fn send_ip6_from(&mut self, src: Ip6, dst: Ip6, proto: u8, payload: &[u8], hop_limit: u8) -> Result<(), isize> {
        if 40 + payload.len() > MTU {
            return Err(EMSGSIZE);
        }
        let (i, hop) = self.route6(&dst).ok_or(ENETUNREACH)?;
        let mut p = vec![0u8; 40 + payload.len()];
        p[0] = 0x60;
        put16(&mut p, 4, payload.len() as u16);
        p[6] = proto;
        p[7] = hop_limit;
        p[8..24].copy_from_slice(&src);
        p[24..40].copy_from_slice(&dst);
        p[40..].copy_from_slice(payload);
        if self.ifaces[i].loopback() {
            let lo = &mut self.ifaces[0];
            lo.tx_packets += 1;
            lo.tx_bytes += p.len() as u64;
            lo.rx_packets += 1;
            lo.rx_bytes += p.len() as u64;
            RX.lock().push_back(super::Frame::Local(p));
            WAKE.wake_all();
            return Ok(());
        }
        if multicast(&dst) {
            self.send_frame(i, multicast_mac(&dst), ETH_IP6, &p);
            return Ok(());
        }
        let now = crate::time::uptime_ms();
        match self.neighbors.get(&hop) {
            Some(&(mac, _)) => self.send_frame(i, mac, ETH_IP6, &p),
            None => {
                if self.pending6.len() > 256 {
                    self.pending6.remove(0);
                }
                let first = !self.pending6.iter().any(|q| q.hop == hop);
                self.pending6.push(Pending { iface: i, hop, packet: p, since: now, asked: 1, last_ask: now });
                if first {
                    self.solicit(i, hop);
                }
            }
        }
        Ok(())
    }

    /// Asks who has `target` (to its solicited-node multicast group).
    fn solicit(&mut self, i: usize, target: Ip6) {
        let mut ns = vec![0u8; 32];
        ns[0] = 135;
        ns[8..24].copy_from_slice(&target);
        ns[24] = 1;
        ns[25] = 1;
        ns[26..32].copy_from_slice(&self.ifaces[i].mac);
        let src = self.ifaces[i].v6.addrs[0].0;
        let _ = self.send_icmp6_from(src, solicited_node(&target), &mut ns, 255);
    }

    pub(super) fn ip6_timers(&mut self, now: u64) {
        // Neighbor discovery: ask again each second, give up after three tries.
        let mut ask = Vec::new();
        for p in &mut self.pending6 {
            if now - p.last_ask >= 1000 && p.asked < 3 {
                p.asked += 1;
                p.last_ask = now;
                ask.push((p.iface, p.hop));
            }
        }
        ask.dedup();
        for (i, hop) in ask {
            self.solicit(i, hop);
        }
        self.pending6.retain(|p| now - p.since < 3500);
        self.neighbors.retain(|_, (_, until)| *until > now);
        // Routers: solicit three times at boot (4 s apart), then rely on
        // their periodic advertisements; forget what expired.
        let mut solicit = Vec::new();
        for (i, f) in self.ifaces.iter_mut().enumerate() {
            let link = f.nic.as_ref().is_some_and(|n| n.link_up());
            let v6 = &mut f.v6;
            v6.addrs.retain(|(_, _, until)| *until > now);
            v6.prefixes.retain(|(_, _, until)| *until > now);
            if v6.router.is_some_and(|(_, until)| until <= now) {
                v6.router = None;
            }
            if link && v6.router.is_none() && v6.solicited < 3 && now - v6.last_solicit >= 4000 {
                v6.solicited += 1;
                v6.last_solicit = now;
                solicit.push(i);
            }
        }
        for i in solicit {
            let mut rs = vec![0u8; 16];
            rs[0] = 133;
            rs[8] = 1;
            rs[9] = 1;
            rs[10..16].copy_from_slice(&self.ifaces[i].mac);
            let src = self.ifaces[i].v6.addrs[0].0;
            let _ = self.send_icmp6_from(src, ALL_ROUTERS, &mut rs, 255);
        }
    }
}

/// A source (1) or target (2) link-layer address option.
fn link_option(mut opts: &[u8], kind: u8) -> Option<[u8; 6]> {
    while opts.len() >= 8 {
        let len = opts[1] as usize * 8;
        if len == 0 || len > opts.len() {
            return None;
        }
        if opts[0] == kind {
            return opts[2..8].try_into().ok();
        }
        opts = &opts[len..];
    }
    None
}
