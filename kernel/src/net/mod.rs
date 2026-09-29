//! The network stack (IPv4): Ethernet, ARP, IP, ICMP, UDP, TCP and DHCP,
//! with sockets for programs.
//!
//! Network drivers hand received frames to [`receive`] (from their interrupt
//! handlers); the `net` task processes them, and runs the timers (TCP
//! retransmission, ARP retries, DHCP). All protocol state lives in one
//! [`Stack`] behind a mutex that the task and socket system calls share.
//! Tasks waiting on sockets sleep on [`CHANGED`], which the task wakes after
//! each batch of work.

pub mod dhcp;
pub mod socket;
pub mod tcp;
pub mod udp;
pub mod wire;

use crate::sync::{IrqMutex, Mutex, WaitQueue};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;
use wire::*;

pub type Ip = [u8; 4];
pub const ANY: Ip = [0; 4];
pub const BROADCAST: Ip = [255; 4];
pub const LOOPBACK: Ip = [127, 0, 0, 1];
const BROADCAST_MAC: [u8; 6] = [0xFF; 6];

pub const PROTO_ICMP: u8 = 1;
pub const PROTO_TCP: u8 = 6;
pub const PROTO_UDP: u8 = 17;

const ETH_ARP: u16 = 0x0806;
const ETH_IP: u16 = 0x0800;
pub const MTU: usize = 1500;

/// A network card, as drivers present it.
pub trait Nic: Send + Sync {
    /// The driver, e.g. "virtio-net" or "Intel e1000".
    fn driver(&self) -> String;
    fn mac(&self) -> [u8; 6];
    fn link_up(&self) -> bool;
    /// Transmits one Ethernet frame (without CRC). False if the card is busy.
    fn send(&self, frame: &[u8]) -> bool;
    /// Called by the network task every few milliseconds: cards without an
    /// interrupt collect received frames here.
    fn poll(&self) {}
}

pub struct Interface {
    pub name: String,
    nic: Option<Arc<dyn Nic>>,
    pub mac: [u8; 6],
    pub ip: Ip,
    pub mask: Ip,
    pub gateway: Ip,
    pub dns: Vec<Ip>,
    pub dhcp: Option<dhcp::Client>,
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

impl Interface {
    fn loopback(&self) -> bool {
        self.nic.is_none()
    }
    fn in_subnet(&self, dst: Ip) -> bool {
        self.ip != ANY && (0..4).all(|k| dst[k] & self.mask[k] == self.ip[k] & self.mask[k])
    }
    fn subnet_broadcast(&self) -> Ip {
        core::array::from_fn(|k| self.ip[k] | !self.mask[k])
    }
}

/// An IP packet waiting for its next hop's hardware address.
struct Pending {
    iface: usize,
    hop: Ip,
    packet: Vec<u8>,
    since: u64,
    asked: u8,
    last_ask: u64,
}

pub struct Stack {
    pub ifaces: Vec<Interface>,
    arp: BTreeMap<Ip, ([u8; 6], u64)>,
    pending: Vec<Pending>,
    ip_id: u16,
    pub udp: udp::Table,
    pub tcp: tcp::Table,
    /// ICMP echo sockets by identifier: replies (source, ICMP message).
    pub icmp: BTreeMap<u16, VecDeque<(Ip, Vec<u8>)>>,
}

enum Frame {
    Ether(usize, Vec<u8>),
    /// An IP packet we sent to ourselves.
    Local(Vec<u8>),
}

static STACK: Mutex<Option<Stack>> = Mutex::new(None);
static RX: IrqMutex<VecDeque<Frame>> = IrqMutex::new(VecDeque::new());
static WAKE: WaitQueue = WaitQueue::new();
/// Woken whenever the stack has processed something: socket waiters re-check.
pub static CHANGED: WaitQueue = WaitQueue::new();
/// Counts those batches, so a waiter can tell whether anything happened.
pub static GEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Runs `f` with the stack (which exists once `init` ran).
pub fn with<R>(f: impl FnOnce(&mut Stack) -> R) -> R {
    let mut g = STACK.lock();
    f(g.as_mut().expect("network stack not initialized"))
}

pub fn init() {
    let lo = Interface {
        name: String::from("lo"),
        nic: None,
        mac: [0; 6],
        ip: LOOPBACK,
        mask: [255, 0, 0, 0],
        gateway: ANY,
        dns: Vec::new(),
        dhcp: None,
        rx_packets: 0,
        tx_packets: 0,
        rx_bytes: 0,
        tx_bytes: 0,
    };
    *STACK.lock() = Some(Stack {
        ifaces: vec![lo],
        arp: BTreeMap::new(),
        pending: Vec::new(),
        ip_id: crate::random::u32() as u16,
        udp: udp::Table::default(),
        tcp: tcp::Table::default(),
        icmp: BTreeMap::new(),
    });
    crate::sched::spawn("net", task);
}

/// A driver found a card: it becomes `eth<n>` and asks DHCP for an address.
pub fn register(nic: Arc<dyn Nic>) -> usize {
    let mac = nic.mac();
    let driver = nic.driver();
    let index = with(|s| {
        let n = s.ifaces.iter().filter(|i| !i.loopback()).count();
        s.ifaces.push(Interface {
            name: alloc::format!("eth{n}"),
            nic: Some(nic),
            mac,
            ip: ANY,
            mask: ANY,
            gateway: ANY,
            dns: Vec::new(),
            dhcp: Some(dhcp::Client::new()),
            rx_packets: 0,
            tx_packets: 0,
            rx_bytes: 0,
            tx_bytes: 0,
        });
        s.ifaces.len() - 1
    });
    log!("net", "eth{}: {} ({})", index - 1, mac_str(mac), driver);
    WAKE.wake_all();
    index
}

/// A received Ethernet frame (drivers call this, also from interrupt handlers).
pub fn receive(iface: usize, frame: Vec<u8>) {
    let mut q = RX.lock();
    if q.len() < 1024 {
        q.push_back(Frame::Ether(iface, frame));
    }
    drop(q);
    WAKE.wake_all();
}

/// Asks the `net` task to run soon (after queuing data to send, etc.).
pub fn poke() {
    WAKE.wake_all();
}

fn task() {
    loop {
        WAKE.wait(10, || !RX.lock().is_empty());
        let nics: Vec<Arc<dyn Nic>> = with(|s| s.ifaces.iter().filter_map(|f| f.nic.clone()).collect());
        for n in nics {
            n.poll();
        }
        let frames: Vec<Frame> = RX.lock().drain(..).collect();
        let now = crate::time::uptime_ms();
        with(|s| {
            for f in frames {
                match f {
                    Frame::Ether(i, data) => s.ether_input(i, &data),
                    Frame::Local(p) => s.ip_input(0, &p),
                }
            }
            s.timers(now);
        });
        GEN.fetch_add(1, core::sync::atomic::Ordering::AcqRel);
        CHANGED.wake_all();
    }
}

impl Stack {
    // ---------------------------------------------------------------- input

    fn ether_input(&mut self, i: usize, f: &[u8]) {
        if f.len() < 14 || i >= self.ifaces.len() {
            return;
        }
        let iface = &mut self.ifaces[i];
        iface.rx_packets += 1;
        iface.rx_bytes += f.len() as u64;
        let dst: [u8; 6] = f[0..6].try_into().unwrap();
        if dst != iface.mac && dst != BROADCAST_MAC && dst[0] & 1 == 0 {
            return;
        }
        match be16(f, 12) {
            ETH_ARP => self.arp_input(i, &f[14..]),
            ETH_IP => self.ip_input(i, &f[14..]),
            _ => {}
        }
    }

    fn arp_input(&mut self, i: usize, p: &[u8]) {
        if p.len() < 28 || be16(p, 0) != 1 || be16(p, 2) != ETH_IP || p[4] != 6 || p[5] != 4 {
            return;
        }
        let op = be16(p, 6);
        let sender_mac: [u8; 6] = p[8..14].try_into().unwrap();
        let sender_ip = ip_at(p, 14);
        let target_ip = ip_at(p, 24);
        let now = crate::time::uptime_ms();
        let ours = self.ifaces[i].ip;
        if sender_ip != ANY && (target_ip == ours || self.arp.contains_key(&sender_ip)) {
            self.arp.insert(sender_ip, (sender_mac, now + 600_000));
            self.flush_pending(sender_ip);
        }
        if op == 1 && ours != ANY && target_ip == ours {
            let mut r = vec![0u8; 28];
            put16(&mut r, 0, 1);
            put16(&mut r, 2, ETH_IP);
            r[4] = 6;
            r[5] = 4;
            put16(&mut r, 6, 2);
            r[8..14].copy_from_slice(&self.ifaces[i].mac);
            r[14..18].copy_from_slice(&ours);
            r[18..24].copy_from_slice(&sender_mac);
            r[24..28].copy_from_slice(&sender_ip);
            self.send_frame(i, sender_mac, ETH_ARP, &r);
        }
    }

    fn ip_input(&mut self, i: usize, p: &[u8]) {
        if p.len() < 20 || p[0] >> 4 != 4 {
            return;
        }
        let ihl = (p[0] & 0xF) as usize * 4;
        let total = be16(p, 2) as usize;
        if ihl < 20 || total < ihl || total > p.len() || checksum(&p[..ihl]) != 0 {
            return;
        }
        // Fragments are not reassembled (we never send any, and MTU-sized
        // TCP segments don't need them).
        if be16(p, 6) & 0x3FFF != 0 {
            return;
        }
        let src = ip_at(p, 12);
        let dst = ip_at(p, 16);
        let iface = &self.ifaces[i];
        let for_us = dst == iface.ip
            || dst == BROADCAST
            || (iface.ip != ANY && dst == iface.subnet_broadcast())
            || (iface.ip == ANY && iface.dhcp.is_some())
            || iface.loopback();
        if !for_us {
            return;
        }
        let payload = &p[ihl..total];
        match p[9] {
            PROTO_ICMP => self.icmp_input(src, payload),
            PROTO_UDP => udp::input(self, i, src, dst, payload),
            PROTO_TCP => tcp::input(self, src, dst, payload),
            _ => {}
        }
    }

    fn icmp_input(&mut self, src: Ip, p: &[u8]) {
        if p.len() < 8 || checksum(p) != 0 {
            return;
        }
        match p[0] {
            // Echo request: answer with the same payload.
            8 => {
                let mut r = p.to_vec();
                r[0] = 0;
                put16(&mut r, 2, 0);
                let c = checksum(&r);
                put16(&mut r, 2, c);
                let _ = self.send_ip(src, PROTO_ICMP, &r);
            }
            // Echo reply: to the socket that sent the request.
            0 => {
                if let Some(q) = self.icmp.get_mut(&be16(p, 4)) {
                    if q.len() < 64 {
                        q.push_back((src, p.to_vec()));
                    }
                }
            }
            // Destination unreachable (port, host, …): TCP gives up connecting.
            3 if p.len() >= 8 + 20 + 4 => {
                let inner = &p[8..];
                let ihl = (inner[0] & 0xF) as usize * 4;
                if inner[9] == PROTO_TCP && inner.len() >= ihl + 4 {
                    let dst = ip_at(inner, 16);
                    let sport = be16(inner, ihl);
                    let dport = be16(inner, ihl + 2);
                    self.tcp.unreachable(dst, sport, dport);
                }
            }
            _ => {}
        }
    }

    // --------------------------------------------------------------- output

    /// Which interface reaches `dst`, and the next hop on it.
    pub fn route(&self, dst: Ip) -> Option<(usize, Ip)> {
        if dst[0] == 127 {
            return Some((0, dst));
        }
        if let Some(i) = self.ifaces.iter().position(|f| f.ip == dst) {
            return Some((i, dst));
        }
        if let Some(i) = self.ifaces.iter().position(|f| !f.loopback() && f.in_subnet(dst)) {
            return Some((i, dst));
        }
        self.ifaces
            .iter()
            .position(|f| !f.loopback() && f.ip != ANY && f.gateway != ANY)
            .map(|i| (i, self.ifaces[i].gateway))
    }

    /// Our address on the interface that reaches `dst`.
    pub fn source_for(&self, dst: Ip) -> Option<Ip> {
        let (i, _) = self.route(dst)?;
        Some(if dst[0] == 127 { LOOPBACK } else { self.ifaces[i].ip })
    }

    fn ip_packet(&mut self, src: Ip, dst: Ip, proto: u8, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 20 + payload.len()];
        p[0] = 0x45;
        put16(&mut p, 2, (20 + payload.len()) as u16);
        self.ip_id = self.ip_id.wrapping_add(1);
        put16(&mut p, 4, self.ip_id);
        put16(&mut p, 6, 0x4000); // don't fragment
        p[8] = 64;
        p[9] = proto;
        p[12..16].copy_from_slice(&src);
        p[16..20].copy_from_slice(&dst);
        let c = checksum(&p[..20]);
        put16(&mut p, 10, c);
        p[20..].copy_from_slice(payload);
        p
    }

    /// Sends an IP datagram to `dst` (routing, ARP, loopback).
    pub fn send_ip(&mut self, dst: Ip, proto: u8, payload: &[u8]) -> Result<(), isize> {
        if 20 + payload.len() > MTU {
            return Err(EMSGSIZE);
        }
        let (i, hop) = self.route(dst).ok_or(ENETUNREACH)?;
        let src = if dst[0] == 127 { LOOPBACK } else { self.ifaces[i].ip };
        let packet = self.ip_packet(src, dst, proto, payload);
        if self.ifaces[i].loopback() || self.ifaces.iter().any(|f| f.ip == dst) {
            let lo = &mut self.ifaces[0];
            lo.tx_packets += 1;
            lo.tx_bytes += packet.len() as u64;
            lo.rx_packets += 1;
            lo.rx_bytes += packet.len() as u64;
            RX.lock().push_back(Frame::Local(packet));
            WAKE.wake_all();
            return Ok(());
        }
        let now = crate::time::uptime_ms();
        match self.arp.get(&hop) {
            Some(&(mac, _)) => {
                self.send_frame(i, mac, ETH_IP, &packet);
            }
            None => {
                if self.pending.len() > 256 {
                    self.pending.remove(0);
                }
                let first = !self.pending.iter().any(|p| p.hop == hop);
                self.pending.push(Pending { iface: i, hop, packet, since: now, asked: 1, last_ask: now });
                if first {
                    self.arp_request(i, hop);
                }
            }
        }
        Ok(())
    }

    /// Broadcasts a datagram from `src` on interface `i` (DHCP, before we have an address).
    pub fn send_broadcast(&mut self, i: usize, src: Ip, proto: u8, payload: &[u8]) {
        let packet = self.ip_packet(src, BROADCAST, proto, payload);
        self.send_frame(i, BROADCAST_MAC, ETH_IP, &packet);
    }

    fn arp_request(&mut self, i: usize, target: Ip) {
        let mut r = vec![0u8; 28];
        put16(&mut r, 0, 1);
        put16(&mut r, 2, ETH_IP);
        r[4] = 6;
        r[5] = 4;
        put16(&mut r, 6, 1);
        r[8..14].copy_from_slice(&self.ifaces[i].mac);
        let ip = self.ifaces[i].ip;
        r[14..18].copy_from_slice(&ip);
        r[24..28].copy_from_slice(&target);
        self.send_frame(i, BROADCAST_MAC, ETH_ARP, &r);
    }

    fn flush_pending(&mut self, ip: Ip) {
        let Some(&(mac, _)) = self.arp.get(&ip) else { return };
        let ready: Vec<Pending> = {
            let (ready, rest): (Vec<_>, Vec<_>) = self.pending.drain(..).partition(|p| p.hop == ip);
            self.pending = rest;
            ready
        };
        for p in ready {
            self.send_frame(p.iface, mac, ETH_IP, &p.packet);
        }
    }

    fn send_frame(&mut self, i: usize, dst: [u8; 6], ethertype: u16, payload: &[u8]) {
        let iface = &mut self.ifaces[i];
        let Some(nic) = iface.nic.clone() else { return };
        let mut f = vec![0u8; (14 + payload.len()).max(60)];
        f[0..6].copy_from_slice(&dst);
        f[6..12].copy_from_slice(&iface.mac);
        put16(&mut f, 12, ethertype);
        f[14..14 + payload.len()].copy_from_slice(payload);
        if nic.send(&f) {
            iface.tx_packets += 1;
            iface.tx_bytes += f.len() as u64;
        }
    }

    // --------------------------------------------------------------- timers

    fn timers(&mut self, now: u64) {
        // ARP: ask again each second, give up after three tries.
        let mut ask = Vec::new();
        for p in &mut self.pending {
            if now - p.last_ask >= 1000 && p.asked < 3 {
                p.asked += 1;
                p.last_ask = now;
                ask.push((p.iface, p.hop));
            }
        }
        ask.dedup();
        for (i, hop) in ask {
            self.arp_request(i, hop);
        }
        self.pending.retain(|p| now - p.since < 3500);
        self.arp.retain(|_, (_, until)| *until > now);
        dhcp::tick(self, now);
        tcp::tick(self, now);
    }
}

/// Interfaces for `net_info`.
pub fn interfaces() -> Vec<aurora_abi::NetInterface> {
    use aurora_abi::net::*;
    let now = crate::time::uptime_ms();
    with(|s| {
        s.ifaces
            .iter()
            .map(|f| {
                let mut n = aurora_abi::NetInterface::default();
                let name = f.name.as_bytes();
                n.name[..name.len().min(16)].copy_from_slice(&name[..name.len().min(16)]);
                n.name_len = name.len().min(16) as u32;
                let driver = f.nic.as_ref().map(|d| d.driver()).unwrap_or_else(|| String::from("loopback"));
                let db = driver.as_bytes();
                let dl = db.len().min(32);
                n.driver[..dl].copy_from_slice(&db[..dl]);
                n.driver_len = dl as u32;
                n.flags = IF_UP;
                if f.loopback() {
                    n.flags |= IF_LOOPBACK | IF_LINK;
                } else if f.nic.as_ref().is_some_and(|d| d.link_up()) {
                    n.flags |= IF_LINK;
                }
                if f.dhcp.as_ref().is_some_and(|d| d.bound()) {
                    n.flags |= IF_DHCP;
                    n.lease_secs = f.dhcp.as_ref().map_or(0, |d| d.lease_left(now));
                }
                n.mac = f.mac;
                n.ip = f.ip;
                n.netmask = f.mask;
                n.gateway = f.gateway;
                for (k, d) in f.dns.iter().take(2).enumerate() {
                    n.dns[k] = *d;
                }
                n.rx_packets = f.rx_packets;
                n.tx_packets = f.tx_packets;
                n.rx_bytes = f.rx_bytes;
                n.tx_bytes = f.tx_bytes;
                n
            })
            .collect()
    })
}

/// Whether some interface has an address (and so, probably, the internet).
pub fn online() -> bool {
    STACK.lock().as_ref().is_some_and(|s| s.ifaces.iter().any(|f| !f.loopback() && f.ip != ANY))
}
