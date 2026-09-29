//! TCP (RFC 9293): reliable byte streams.
//!
//! Each connection keeps a send buffer (bytes from `snd_una`: sent but not
//! yet acknowledged, then not yet sent) and a receive buffer. Segments go out
//! as the peer's window and our congestion window allow; unacknowledged data
//! is resent when the retransmission timer (RFC 6298) fires or after three
//! duplicate ACKs. Received segments that arrive early wait in an
//! out-of-order map. Options: MSS and window scaling.
//!
//! Connection methods only build segments; the stack sends them afterwards,
//! which keeps the borrow of the connection table short.

use super::wire::*;
use super::{Ip, Stack, ANY, PROTO_TCP};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub type ConnId = u32;

const FIN: u8 = 0x01;
const SYN: u8 = 0x02;
const RST: u8 = 0x04;
const PSH: u8 = 0x08;
const ACK: u8 = 0x10;

/// Our maximum segment size (Ethernet MTU minus IP and TCP headers).
const MSS: u16 = 1460;
const RECV_CAP: usize = 256 * 1024;
pub const SEND_CAP: usize = 256 * 1024;
/// Window scale we offer (256 KiB >> 3 fits the 16-bit field).
const RCV_SHIFT: u8 = 3;
const RTO_INITIAL: u64 = 1000;
const RTO_MIN: u64 = 200;
const RTO_MAX: u64 = 60_000;
const DELAYED_ACK: u64 = 40;
/// TIME-WAIT is kept short: 2 × MSL would be minutes.
const TIME_WAIT: u64 = 5000;
const EPHEMERAL: core::ops::Range<u16> = 49152..65535;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum State {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
}

fn lt(a: u32, b: u32) -> bool {
    (a.wrapping_sub(b) as i32) < 0
}

fn le(a: u32, b: u32) -> bool {
    a == b || lt(a, b)
}

pub struct Conn {
    pub state: State,
    pub local: (Ip, u16),
    pub remote: (Ip, u16),
    iss: u32,
    snd_una: u32,
    snd_nxt: u32,
    snd_wnd: u32,
    snd_wl1: u32,
    snd_wl2: u32,
    send_buf: VecDeque<u8>,
    /// The program shut down sending: a FIN follows the data.
    fin_queued: bool,
    fin_sent: bool,
    fin_seq: u32,
    fin_acked: bool,
    rcv_nxt: u32,
    recv_buf: VecDeque<u8>,
    ooo: BTreeMap<u32, Vec<u8>>,
    pub fin_received: bool,
    /// The peer's MSS, and window scale shifts (theirs, ours).
    mss: u16,
    snd_shift: u8,
    rcv_shift: u8,
    rto: u64,
    srtt: u64,
    rttvar: u64,
    rtt_probe: Option<(u32, u64)>,
    rexmit_at: Option<u64>,
    retries: u32,
    cwnd: u32,
    ssthresh: u32,
    dup_acks: u32,
    syn_pending: bool,
    ack_now: bool,
    ack_at: Option<u64>,
    segments_unacked: u32,
    last_adv: usize,
    timewait_until: u64,
    backlog: usize,
    pub accept_queue: VecDeque<ConnId>,
    parent: Option<ConnId>,
    pub error: Option<isize>,
    /// The program closed its socket: the connection goes away once finished.
    pub orphan: bool,
}

impl Conn {
    fn new(local: (Ip, u16), remote: (Ip, u16), state: State) -> Conn {
        let iss = crate::random::u32();
        Conn {
            state,
            local,
            remote,
            iss,
            snd_una: iss,
            snd_nxt: iss.wrapping_add(1),
            snd_wnd: 0,
            snd_wl1: 0,
            snd_wl2: 0,
            send_buf: VecDeque::new(),
            fin_queued: false,
            fin_sent: false,
            fin_seq: 0,
            fin_acked: false,
            rcv_nxt: 0,
            recv_buf: VecDeque::new(),
            ooo: BTreeMap::new(),
            fin_received: false,
            mss: 536,
            snd_shift: 0,
            rcv_shift: 0,
            rto: RTO_INITIAL,
            srtt: 0,
            rttvar: 0,
            rtt_probe: None,
            rexmit_at: None,
            retries: 0,
            cwnd: 0,
            ssthresh: 64 * 1024,
            dup_acks: 0,
            syn_pending: state == State::SynSent || state == State::SynReceived,
            ack_now: false,
            ack_at: None,
            segments_unacked: 0,
            last_adv: RECV_CAP,
            timewait_until: 0,
            backlog: 0,
            accept_queue: VecDeque::new(),
            parent: None,
            error: None,
            orphan: false,
        }
    }

    fn recv_space(&self) -> usize {
        let held: usize = self.ooo.values().map(|v| v.len()).sum();
        RECV_CAP.saturating_sub(self.recv_buf.len() + held)
    }

    fn window_field(&self, syn: bool) -> u16 {
        let space = self.recv_space();
        if syn {
            space.min(0xFFFF) as u16
        } else {
            (space >> self.rcv_shift).min(0xFFFF) as u16
        }
    }

    fn segment(&mut self, seq: u32, flags: u8, data: &[u8]) -> Vec<u8> {
        let syn = flags & SYN != 0;
        let opts: &[u8] = if syn { &[2, 4, (MSS >> 8) as u8, MSS as u8, 1, 3, 3, RCV_SHIFT] } else { &[] };
        let hlen = 20 + opts.len();
        let mut s = vec![0u8; hlen + data.len()];
        put16(&mut s, 0, self.local.1);
        put16(&mut s, 2, self.remote.1);
        put32(&mut s, 4, seq);
        let ack = flags & ACK != 0;
        put32(&mut s, 8, if ack { self.rcv_nxt } else { 0 });
        s[12] = ((hlen / 4) as u8) << 4;
        s[13] = flags;
        let w = self.window_field(syn);
        put16(&mut s, 14, w);
        s[20..hlen].copy_from_slice(opts);
        s[hlen..].copy_from_slice(data);
        let c = transport_checksum(self.local.0, self.remote.0, PROTO_TCP, &s);
        put16(&mut s, 16, c);
        if ack {
            // This segment carries the acknowledgement.
            self.ack_now = false;
            self.ack_at = None;
            self.segments_unacked = 0;
            self.last_adv = self.recv_space();
        }
        s
    }

    fn in_flight(&self) -> u32 {
        self.snd_nxt.wrapping_sub(self.snd_una)
    }

    fn unsent(&self) -> usize {
        let sent_data = self.in_flight() as usize - (self.fin_sent && !self.fin_acked) as usize;
        self.send_buf.len().saturating_sub(sent_data)
    }

    /// Segments to send now: SYN, data, FIN, or a bare ACK.
    fn output(&mut self, now: u64) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        match self.state {
            State::SynSent if self.syn_pending => {
                self.syn_pending = false;
                out.push(self.segment(self.iss, SYN, &[]));
                self.rexmit_at.get_or_insert(now + self.rto);
                return out;
            }
            State::SynReceived if self.syn_pending => {
                self.syn_pending = false;
                out.push(self.segment(self.iss, SYN | ACK, &[]));
                self.rexmit_at.get_or_insert(now + self.rto);
                return out;
            }
            State::Established | State::CloseWait | State::FinWait1 | State::Closing | State::LastAck => {}
            _ => {
                if self.ack_now && self.state != State::Closed && self.state != State::Listen {
                    out.push(self.segment(self.snd_nxt, ACK, &[]));
                }
                return out;
            }
        }
        let mss = self.mss.min(MSS) as u32;
        let window = self.snd_wnd.min(self.cwnd.max(mss));
        // Probe a closed window with one byte when nothing is in flight.
        let window = if self.snd_wnd == 0 && self.in_flight() == 0 && self.unsent() > 0 { 1 } else { window };
        while self.unsent() > 0 && self.in_flight() < window {
            let offset = self.snd_nxt.wrapping_sub(self.snd_una) as usize;
            let n = (self.unsent() as u32).min(mss).min(window - self.in_flight()) as usize;
            if n == 0 {
                break;
            }
            let data: Vec<u8> = self.send_buf.range(offset..offset + n).copied().collect();
            let flags = if n == self.unsent() { ACK | PSH } else { ACK };
            let seq = self.snd_nxt;
            out.push(self.segment(seq, flags, &data));
            self.snd_nxt = self.snd_nxt.wrapping_add(n as u32);
            if self.rtt_probe.is_none() {
                self.rtt_probe = Some((self.snd_nxt, now));
            }
            self.rexmit_at.get_or_insert(now + self.rto);
        }
        if self.fin_queued && !self.fin_sent && self.unsent() == 0 {
            self.fin_seq = self.snd_nxt;
            let seq = self.snd_nxt;
            out.push(self.segment(seq, FIN | ACK, &[]));
            self.snd_nxt = self.snd_nxt.wrapping_add(1);
            self.fin_sent = true;
            self.state = match self.state {
                State::Established => State::FinWait1,
                State::CloseWait => State::LastAck,
                s => s,
            };
            self.rexmit_at.get_or_insert(now + self.rto);
        }
        if self.ack_now {
            let seq = self.snd_nxt;
            out.push(self.segment(seq, ACK, &[]));
        }
        out
    }

    /// Our part of the handshake is done: set up the send side.
    fn established(&mut self, window: u16, now: u64) {
        self.snd_una = self.iss.wrapping_add(1);
        self.snd_nxt = self.snd_una;
        self.snd_wnd = window as u32; // never scaled in SYN segments
        self.cwnd = 10 * self.mss.min(MSS) as u32;
        self.rexmit_at = None;
        self.retries = 0;
        self.state = State::Established;
        if let Some((_, sent)) = self.rtt_probe.take() {
            self.rtt_sample(now - sent);
        }
    }

    fn rtt_sample(&mut self, r: u64) {
        if self.srtt == 0 {
            self.srtt = r;
            self.rttvar = r / 2;
        } else {
            let diff = self.srtt.abs_diff(r);
            self.rttvar = (3 * self.rttvar + diff) / 4;
            self.srtt = (7 * self.srtt + r) / 8;
        }
        self.rto = (self.srtt + (4 * self.rttvar).max(10)).clamp(RTO_MIN, RTO_MAX);
    }

    fn parse_options(&mut self, opts: &[u8]) {
        let mut i = 0;
        let mut scale = None;
        while i < opts.len() {
            match opts[i] {
                0 => break,
                1 => i += 1,
                kind => {
                    let Some(&len) = opts.get(i + 1) else { break };
                    if len < 2 || i + len as usize > opts.len() {
                        break;
                    }
                    match (kind, len) {
                        (2, 4) => self.mss = be16(opts, i + 2).max(64),
                        (3, 3) => scale = Some(opts[i + 2].min(14)),
                        _ => {}
                    }
                    i += len as usize;
                }
            }
        }
        // Window scaling needs both sides' agreement (we always offer it).
        if let Some(s) = scale {
            self.snd_shift = s;
            self.rcv_shift = RCV_SHIFT;
        }
    }

    /// The peer acknowledged up to `ack`.
    fn acked(&mut self, ack: u32, now: u64) {
        let acked = ack.wrapping_sub(self.snd_una);
        let mut data = acked as usize;
        if self.fin_sent && lt(self.fin_seq, ack) {
            self.fin_acked = true;
            data -= 1;
        }
        let data = data.min(self.send_buf.len());
        self.send_buf.drain(..data);
        self.snd_una = ack;
        if let Some((seq, sent)) = self.rtt_probe {
            if le(seq, ack) {
                self.rtt_sample(now - sent);
                self.rtt_probe = None;
            }
        }
        self.retries = 0;
        self.dup_acks = 0;
        self.rexmit_at = if self.snd_una == self.snd_nxt { None } else { Some(now + self.rto) };
        let mss = self.mss.min(MSS) as u32;
        if self.cwnd < self.ssthresh {
            self.cwnd += acked.min(mss);
        } else {
            self.cwnd += (mss * mss / self.cwnd.max(1)).max(1);
        }
        self.cwnd = self.cwnd.min(4 * 1024 * 1024);
    }

    /// Takes in-order data (and whatever it unblocks from the out-of-order map).
    fn deliver(&mut self, mut seq: u32, mut data: &[u8]) {
        if lt(seq, self.rcv_nxt) {
            let skip = self.rcv_nxt.wrapping_sub(seq) as usize;
            if skip >= data.len() {
                return;
            }
            data = &data[skip..];
            seq = self.rcv_nxt;
        }
        if seq != self.rcv_nxt {
            if lt(self.rcv_nxt, seq) && data.len() <= self.recv_space() {
                self.ooo.entry(seq).or_insert_with(|| data.to_vec());
            }
            self.ack_now = true; // duplicate ACK tells the sender what's missing
            return;
        }
        let n = data.len().min(RECV_CAP - self.recv_buf.len());
        self.recv_buf.extend(&data[..n]);
        self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
        while let Some((&s, _)) = self.ooo.first_key_value() {
            if lt(self.rcv_nxt, s) {
                break;
            }
            let (s, seg) = self.ooo.pop_first().unwrap();
            let skip = self.rcv_nxt.wrapping_sub(s) as usize;
            if skip < seg.len() {
                let rest = &seg[skip..];
                let n = rest.len().min(RECV_CAP - self.recv_buf.len());
                self.recv_buf.extend(&rest[..n]);
                self.rcv_nxt = self.rcv_nxt.wrapping_add(n as u32);
            }
        }
    }
}

#[derive(Default)]
pub struct Table {
    pub conns: BTreeMap<ConnId, Conn>,
    next_id: ConnId,
    next_port: u16,
}

impl Table {
    fn insert(&mut self, c: Conn) -> ConnId {
        self.next_id = self.next_id.wrapping_add(1).max(1);
        while self.conns.contains_key(&self.next_id) {
            self.next_id = self.next_id.wrapping_add(1).max(1);
        }
        self.conns.insert(self.next_id, c);
        self.next_id
    }

    fn port_used(&self, port: u16) -> bool {
        self.conns.values().any(|c| c.local.1 == port && c.state != State::Closed)
    }

    fn ephemeral(&mut self) -> Option<u16> {
        if self.next_port == 0 {
            self.next_port = EPHEMERAL.start + (crate::random::u32() % (EPHEMERAL.end - EPHEMERAL.start) as u32) as u16;
        }
        for _ in 0..(EPHEMERAL.end - EPHEMERAL.start) {
            let p = self.next_port;
            self.next_port = if p + 1 >= EPHEMERAL.end { EPHEMERAL.start } else { p + 1 };
            if !self.port_used(p) {
                return Some(p);
            }
        }
        None
    }

    /// An ICMP "unreachable" about one of our connections.
    pub fn unreachable(&mut self, remote: Ip, local_port: u16, remote_port: u16) {
        for c in self.conns.values_mut() {
            if c.state == State::SynSent && c.local.1 == local_port && c.remote == (remote, remote_port) {
                c.error = Some(EHOSTUNREACH);
                c.state = State::Closed;
            }
        }
    }
}

fn flush(s: &mut Stack, id: ConnId, now: u64) {
    let (dst, segs) = match s.tcp.conns.get_mut(&id) {
        Some(c) => (c.remote.0, c.output(now)),
        None => return,
    };
    for seg in segs {
        let _ = s.send_ip(dst, PROTO_TCP, &seg);
    }
}

// ------------------------------------------------------------- socket calls

/// Starts connecting to `remote`; the socket waits for `State::Established`.
pub fn connect(s: &mut Stack, remote: (Ip, u16)) -> Result<ConnId, isize> {
    let src = s.source_for(remote.0).ok_or(ENETUNREACH)?;
    if src == ANY {
        return Err(ENETUNREACH);
    }
    let port = s.tcp.ephemeral().ok_or(EADDRINUSE)?;
    let mut c = Conn::new((src, port), remote, State::SynSent);
    c.rtt_probe = Some((c.iss.wrapping_add(1), crate::time::uptime_ms()));
    let id = s.tcp.insert(c);
    flush(s, id, crate::time::uptime_ms());
    Ok(id)
}

pub fn listen(s: &mut Stack, port: u16, backlog: usize) -> Result<ConnId, isize> {
    if port == 0 || s.tcp.conns.values().any(|c| c.local.1 == port && c.state == State::Listen) {
        return Err(EADDRINUSE);
    }
    let mut c = Conn::new((ANY, port), (ANY, 0), State::Listen);
    c.backlog = backlog.clamp(1, 128);
    Ok(s.tcp.insert(c))
}

/// The next fully established connection on a listener.
pub fn accept(s: &mut Stack, listener: ConnId) -> Option<ConnId> {
    s.tcp.conns.get_mut(&listener)?.accept_queue.pop_front()
}

/// Queues bytes to send; returns how many fit.
pub fn send(s: &mut Stack, id: ConnId, data: &[u8]) -> Result<usize, isize> {
    let now = crate::time::uptime_ms();
    let c = s.tcp.conns.get_mut(&id).ok_or(ENOTCONN)?;
    if let Some(e) = c.error {
        return Err(e);
    }
    match c.state {
        State::Established | State::CloseWait => {}
        State::SynSent | State::SynReceived => return Ok(0),
        _ => return Err(EPIPE),
    }
    if c.fin_queued {
        return Err(EPIPE);
    }
    let n = data.len().min(SEND_CAP - c.send_buf.len());
    c.send_buf.extend(&data[..n]);
    flush(s, id, now);
    Ok(n)
}

/// Takes received bytes: `Ok(0)` means the peer finished sending (end of file).
pub fn recv(s: &mut Stack, id: ConnId, out: &mut [u8]) -> Result<usize, isize> {
    let now = crate::time::uptime_ms();
    let c = s.tcp.conns.get_mut(&id).ok_or(ENOTCONN)?;
    let n = out.len().min(c.recv_buf.len());
    for (o, b) in out.iter_mut().zip(c.recv_buf.drain(..n)) {
        *o = b;
    }
    if n == 0 {
        // Everything the peer sent was delivered: end of file, even if the
        // connection was reset afterwards.
        if c.fin_received {
            return Ok(0);
        }
        if let Some(e) = c.error {
            return Err(e);
        }
        return if c.state == State::Closed { Ok(0) } else { Err(EAGAIN) };
    }
    // Tell the peer when the window has opened up noticeably (pointless once
    // it has finished sending).
    let space = c.recv_space();
    let opened = space >= c.last_adv + (2 * MSS as usize).min(RECV_CAP / 2) || c.last_adv < MSS as usize;
    if opened && !c.fin_received {
        c.ack_now = true;
        flush(s, id, now);
    }
    Ok(n)
}

pub fn readable(s: &Stack, id: ConnId) -> usize {
    s.tcp.conns.get(&id).map_or(0, |c| c.recv_buf.len())
}

/// Something to report to a reader: data, end of file, or an error.
pub fn read_ready(s: &Stack, id: ConnId) -> bool {
    s.tcp
        .conns
        .get(&id)
        .is_none_or(|c| !c.recv_buf.is_empty() || c.fin_received || c.error.is_some() || c.state == State::Closed)
}

pub fn write_ready(s: &Stack, id: ConnId) -> bool {
    s.tcp.conns.get(&id).is_none_or(|c| {
        c.error.is_some()
            || !matches!(c.state, State::Established | State::CloseWait | State::SynSent | State::SynReceived)
            || (c.state != State::SynSent && c.state != State::SynReceived && c.send_buf.len() < SEND_CAP)
    })
}

/// No more data from us: a FIN follows what is queued.
pub fn shutdown(s: &mut Stack, id: ConnId) {
    let now = crate::time::uptime_ms();
    if let Some(c) = s.tcp.conns.get_mut(&id) {
        match c.state {
            State::Established | State::CloseWait => c.fin_queued = true,
            State::SynSent | State::Listen => c.state = State::Closed,
            _ => {}
        }
    }
    flush(s, id, now);
}

/// The socket is closed: finish politely, or reset if data was left unread.
pub fn close(s: &mut Stack, id: ConnId) {
    let Some(c) = s.tcp.conns.get_mut(&id) else { return };
    c.orphan = true;
    match c.state {
        State::Listen | State::SynSent | State::Closed | State::TimeWait => {
            // Connections that were never accepted are reset.
            let pending: Vec<ConnId> = c.accept_queue.drain(..).collect();
            s.tcp.conns.remove(&id);
            for p in pending {
                abort(s, p);
            }
        }
        _ if !c.recv_buf.is_empty() => abort(s, id),
        _ => shutdown(s, id),
    }
}

/// Resets the connection.
pub fn abort(s: &mut Stack, id: ConnId) {
    let Some(c) = s.tcp.conns.get_mut(&id) else { return };
    if !matches!(c.state, State::Closed | State::Listen | State::TimeWait | State::SynSent) {
        let seq = c.snd_nxt;
        let seg = c.segment(seq, RST | ACK, &[]);
        let dst = c.remote.0;
        let _ = s.send_ip(dst, PROTO_TCP, &seg);
    }
    s.tcp.conns.remove(&id);
}

pub fn state(s: &Stack, id: ConnId) -> Option<(State, (Ip, u16), (Ip, u16), Option<isize>)> {
    s.tcp.conns.get(&id).map(|c| (c.state, c.local, c.remote, c.error))
}

// ---------------------------------------------------------------- segments

/// Answers a segment that matches no connection with a reset.
fn reset_reply(s: &mut Stack, src: Ip, dst: Ip, p: &[u8], len: u32) {
    let flags = p[13];
    if flags & RST != 0 {
        return;
    }
    let mut r = vec![0u8; 20];
    put16(&mut r, 0, be16(p, 2));
    put16(&mut r, 2, be16(p, 0));
    if flags & ACK != 0 {
        put32(&mut r, 4, be32(p, 8));
        r[13] = RST;
    } else {
        put32(&mut r, 8, be32(p, 4).wrapping_add(len));
        r[13] = RST | ACK;
    }
    r[12] = 5 << 4;
    let c = transport_checksum(dst, src, PROTO_TCP, &r);
    put16(&mut r, 16, c);
    let _ = s.send_ip(src, PROTO_TCP, &r);
}

pub fn input(s: &mut Stack, src: Ip, dst: Ip, p: &[u8]) {
    if p.len() < 20 || transport_checksum(src, dst, PROTO_TCP, p) != 0 {
        return;
    }
    let sport = be16(p, 0);
    let dport = be16(p, 2);
    let seq = be32(p, 4);
    let ack = be32(p, 8);
    let off = (p[12] >> 4) as usize * 4;
    if off < 20 || off > p.len() {
        return;
    }
    let flags = p[13];
    let window = be16(p, 14);
    let opts = &p[20..off];
    let data = &p[off..];
    let seg_len = data.len() as u32 + (flags & SYN != 0) as u32 + (flags & FIN != 0) as u32;
    let now = crate::time::uptime_ms();

    let id = s
        .tcp
        .conns
        .iter()
        .find(|(_, c)| c.state != State::Listen && c.local.1 == dport && c.remote == (src, sport))
        .or_else(|| s.tcp.conns.iter().find(|(_, c)| c.state == State::Listen && c.local.1 == dport))
        .map(|(&id, _)| id);
    let Some(id) = id else {
        reset_reply(s, src, dst, p, seg_len);
        return;
    };

    // A listener spawns a half-open connection for each SYN.
    if s.tcp.conns[&id].state == State::Listen {
        if flags & RST != 0 {
            return;
        }
        if flags & ACK != 0 || flags & SYN == 0 {
            reset_reply(s, src, dst, p, seg_len);
            return;
        }
        let waiting = s.tcp.conns.values().filter(|c| c.parent == Some(id) && c.state == State::SynReceived).count();
        let l = &s.tcp.conns[&id];
        if l.accept_queue.len() + waiting >= l.backlog {
            return; // full: the client will retry
        }
        let mut c = Conn::new((dst, dport), (src, sport), State::SynReceived);
        c.parse_options(opts);
        c.rcv_nxt = seq.wrapping_add(1);
        c.parent = Some(id);
        c.rtt_probe = Some((c.iss.wrapping_add(1), now));
        let child = s.tcp.insert(c);
        flush(s, child, now);
        return;
    }

    let c = s.tcp.conns.get_mut(&id).unwrap();
    if c.state == State::SynSent {
        if flags & ACK != 0 && ack != c.iss.wrapping_add(1) {
            reset_reply(s, src, dst, p, seg_len);
            return;
        }
        if flags & RST != 0 {
            if flags & ACK != 0 {
                c.error = Some(ECONNREFUSED);
                c.state = State::Closed;
            }
            return;
        }
        if flags & SYN != 0 {
            c.parse_options(opts);
            c.rcv_nxt = seq.wrapping_add(1);
            if flags & ACK != 0 {
                c.established(window, now);
                c.snd_wl1 = seq;
                c.snd_wl2 = ack;
                c.ack_now = true;
            } else {
                c.state = State::SynReceived;
                c.syn_pending = true;
            }
            flush(s, id, now);
        }
        return;
    }

    // Is the segment within what we can receive?
    let wnd = c.recv_space().max(1) as u32;
    let acceptable = if seg_len == 0 {
        le(c.rcv_nxt, seq) && lt(seq, c.rcv_nxt.wrapping_add(wnd))
    } else {
        lt(seq, c.rcv_nxt.wrapping_add(wnd)) && lt(c.rcv_nxt, seq.wrapping_add(seg_len))
    };
    if !acceptable {
        if flags & RST == 0 {
            c.ack_now = true;
            flush(s, id, now);
        }
        return;
    }
    if flags & RST != 0 {
        // RFC 1337: a reset can't cut TIME-WAIT short.
        if c.state == State::TimeWait {
            return;
        }
        if c.state == State::SynReceived && c.parent.is_some() {
            s.tcp.conns.remove(&id);
        } else {
            c.error = Some(ECONNRESET);
            c.state = State::Closed;
            c.rexmit_at = None;
        }
        return;
    }
    if flags & SYN != 0 {
        // A SYN inside a synchronized connection: remind the peer where we are.
        c.ack_now = true;
        flush(s, id, now);
        return;
    }
    if flags & ACK == 0 {
        return;
    }

    if c.state == State::SynReceived {
        if lt(c.iss, ack) && le(ack, c.snd_nxt) {
            c.established(window, now);
            // Unlike the SYN's, this segment's window is scaled.
            c.snd_wnd = (window as u32) << c.snd_shift;
            c.snd_wl1 = seq;
            c.snd_wl2 = ack;
            if let Some(parent) = c.parent {
                if let Some(l) = s.tcp.conns.get_mut(&parent) {
                    l.accept_queue.push_back(id);
                }
            }
        } else {
            reset_reply(s, src, dst, p, seg_len);
            return;
        }
    }

    let c = s.tcp.conns.get_mut(&id).unwrap();
    if lt(c.snd_una, ack) && le(ack, c.snd_nxt) {
        c.acked(ack, now);
    } else if ack == c.snd_una && data.is_empty() && flags & FIN == 0 && c.snd_una != c.snd_nxt {
        c.dup_acks += 1;
        if c.dup_acks == 3 {
            // Fast retransmit: resend from the first unacknowledged byte.
            let mss = c.mss.min(MSS) as u32;
            c.ssthresh = (c.in_flight() / 2).max(2 * mss);
            c.cwnd = c.ssthresh + 3 * mss;
            c.snd_nxt = c.snd_una;
            c.fin_sent = c.fin_sent && c.fin_acked;
            c.rtt_probe = None;
        }
    } else if lt(c.snd_nxt, ack) {
        c.ack_now = true;
        flush(s, id, now);
        return;
    }
    if lt(c.snd_wl1, seq) || (c.snd_wl1 == seq && le(c.snd_wl2, ack)) {
        c.snd_wnd = (window as u32) << c.snd_shift;
        c.snd_wl1 = seq;
        c.snd_wl2 = ack;
    }
    if c.fin_acked {
        match c.state {
            State::FinWait1 => c.state = State::FinWait2,
            State::Closing => {
                c.state = State::TimeWait;
                c.timewait_until = now + TIME_WAIT;
            }
            State::LastAck => {
                c.state = State::Closed;
            }
            _ => {}
        }
    }

    if !data.is_empty() && matches!(c.state, State::Established | State::FinWait1 | State::FinWait2) {
        c.deliver(seq, data);
        c.segments_unacked += 1;
        if c.segments_unacked >= 2 || !c.ooo.is_empty() {
            c.ack_now = true;
        } else {
            c.ack_at.get_or_insert(now + DELAYED_ACK);
        }
    }
    if flags & FIN != 0 && seq.wrapping_add(data.len() as u32) == c.rcv_nxt && !c.fin_received {
        c.rcv_nxt = c.rcv_nxt.wrapping_add(1);
        c.fin_received = true;
        c.ack_now = true;
        c.state = match c.state {
            State::SynReceived | State::Established => State::CloseWait,
            State::FinWait1 if c.fin_acked => {
                c.timewait_until = now + TIME_WAIT;
                State::TimeWait
            }
            State::FinWait1 => State::Closing,
            State::FinWait2 => {
                c.timewait_until = now + TIME_WAIT;
                State::TimeWait
            }
            s => s,
        };
    }
    flush(s, id, now);
}

/// Retransmissions, delayed ACKs, TIME-WAIT, and cleaning up.
pub fn tick(s: &mut Stack, now: u64) {
    let ids: Vec<ConnId> = s.tcp.conns.keys().copied().collect();
    for id in ids {
        let Some(c) = s.tcp.conns.get_mut(&id) else { continue };
        if c.rexmit_at.is_some_and(|t| now >= t) {
            c.retries += 1;
            let limit = if matches!(c.state, State::SynSent | State::SynReceived) { 6 } else { 12 };
            if c.retries > limit {
                c.error = Some(ETIMEDOUT);
                c.state = State::Closed;
                c.rexmit_at = None;
            } else {
                // Go back to the first unacknowledged byte, slowly.
                let mss = c.mss.min(MSS) as u32;
                c.ssthresh = (c.in_flight() / 2).max(2 * mss);
                c.cwnd = mss;
                c.rto = (c.rto * 2).min(RTO_MAX);
                if matches!(c.state, State::SynSent | State::SynReceived) {
                    c.syn_pending = true;
                } else {
                    c.snd_nxt = c.snd_una;
                    if c.fin_sent && !c.fin_acked {
                        c.fin_sent = false;
                    }
                }
                c.rtt_probe = None;
                c.rexmit_at = Some(now + c.rto);
            }
        }
        if c.ack_at.is_some_and(|t| now >= t) {
            c.ack_now = true;
        }
        if c.state == State::TimeWait && now >= c.timewait_until {
            c.state = State::Closed;
        }
        // Half-open connections whose listener went away.
        let orphaned_child = c.parent.is_some_and(|p| !s.tcp.conns.contains_key(&p));
        let c = s.tcp.conns.get_mut(&id).unwrap();
        if (c.state == State::Closed && c.orphan) || (orphaned_child && c.state == State::SynReceived) {
            s.tcp.conns.remove(&id);
            continue;
        }
        flush(s, id, now);
    }
}
