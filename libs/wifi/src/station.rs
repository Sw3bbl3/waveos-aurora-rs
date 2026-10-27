//! The station: joining a network and carrying traffic, independent of the
//! radio. A driver feeds it received 802.11 frames and the time; it answers
//! with frames to transmit and Ethernet frames to hand to the network
//! stack. Encryption (CCMP) happens here, in software.
//!
//!   Idle → Authenticating → Associating → Handshake (protected networks)
//!        → Connected, or Failed with a reason the user can read.

use crate::crypto::{self, Ptk};
use crate::frame::{self, Bss, Header, Mac, FLAG_FROM_DS, TYPE_DATA, TYPE_MGMT};
use crate::rsn::{self, Security};
use crate::supplicant::{Event, Gtk, Supplicant, ETHERTYPE_EAPOL};
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

const AUTH_TIMEOUT_MS: u64 = 500;
const ASSOC_TIMEOUT_MS: u64 = 500;
const HANDSHAKE_TIMEOUT_MS: u64 = 5000;
const TRIES: u8 = 3;
/// Networks not heard from for this long leave the scan list.
const STALE_MS: u64 = 60_000;
/// Reason code for "leaving" in deauthentication.
const REASON_LEAVING: u16 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Idle,
    Authenticating,
    Associating,
    /// The 4-way handshake (protected networks).
    Handshake,
    Connected,
    Failed(String),
}

/// Something the radio or the network stack must do.
#[derive(Debug, PartialEq, Eq)]
pub enum Output {
    /// Transmit this 802.11 frame.
    Transmit(Vec<u8>),
    /// An Ethernet frame for the network stack.
    Receive(Vec<u8>),
    /// The link came up (the network stack should start DHCP) or went down.
    Link(bool),
}

/// A network in the scan list, with when it was last heard.
#[derive(Clone, Debug)]
pub struct Seen {
    pub bss: Bss,
    pub last_ms: u64,
}

struct Keys {
    tk: [u8; 16],
    tx_pn: u64,
    /// Highest packet number received, per traffic identifier (replay protection).
    rx_pn: [Option<u64>; 17],
    gtk: Option<Gtk>,
    gtk_rx_pn: Option<u64>,
}

pub struct Station {
    mac: Mac,
    state: State,
    target: Option<Bss>,
    pmk: Option<[u8; 32]>,
    supplicant: Option<Supplicant>,
    keys: Option<Keys>,
    deadline: u64,
    tries: u8,
    seq: u16,
    seen: BTreeMap<Mac, Seen>,
}

impl Station {
    pub fn new(mac: Mac) -> Station {
        Station {
            mac,
            state: State::Idle,
            target: None,
            pmk: None,
            supplicant: None,
            keys: None,
            deadline: 0,
            tries: 0,
            seq: 0,
            seen: BTreeMap::new(),
        }
    }

    pub fn mac(&self) -> Mac {
        self.mac
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    /// The network we are joining or joined.
    pub fn network(&self) -> Option<&Bss> {
        self.target.as_ref()
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0xFFF;
        self.seq
    }

    /// Networks heard recently, strongest name first (one entry per SSID).
    pub fn networks(&self, now_ms: u64) -> Vec<Bss> {
        let mut out: Vec<Bss> = Vec::new();
        for s in self.seen.values() {
            if now_ms.saturating_sub(s.last_ms) > STALE_MS || s.bss.ssid.is_empty() {
                continue;
            }
            if !out.iter().any(|b| b.ssid == s.bss.ssid) {
                out.push(s.bss.clone());
            }
        }
        out.sort_by_key(|b| b.ssid.to_lowercase());
        out
    }

    /// A probe request asking every network on the current channel to answer.
    pub fn probe(&mut self) -> Vec<u8> {
        let seq = self.next_seq();
        frame::probe_request(self.mac, b"", seq)
    }

    /// Starts joining `bss`. `passphrase` is required for WPA2 networks;
    /// `snonce` must be fresh random bytes.
    pub fn connect(&mut self, bss: Bss, passphrase: Option<&str>, snonce: [u8; 32], now_ms: u64) -> Vec<Output> {
        let mut out = self.disconnect();
        if !bss.security.joinable() {
            self.state = State::Failed(alloc::format!("{} networks are not supported", bss.security.label()));
            return out;
        }
        if bss.security != Security::Open {
            let Some(p) = passphrase.filter(|p| crypto::passphrase_valid(p)) else {
                self.state = State::Failed(String::from("The password must be 8 to 63 characters"));
                return out;
            };
            let pmk = crypto::pmk(p.as_bytes(), &bss.ssid_raw);
            let ap_ie = bss.rsn_ie.clone().unwrap_or_default();
            self.supplicant = Some(Supplicant::new(pmk, self.mac, bss.bssid, rsn::station_ie(), ap_ie, snonce));
            self.pmk = Some(pmk);
        }
        let seq = self.next_seq();
        out.push(Output::Transmit(frame::auth_request(self.mac, bss.bssid, seq)));
        self.target = Some(bss);
        self.state = State::Authenticating;
        self.tries = 1;
        self.deadline = now_ms + AUTH_TIMEOUT_MS;
        out
    }

    /// Leaves the network (if any).
    pub fn disconnect(&mut self) -> Vec<Output> {
        let mut out = Vec::new();
        let was_up = self.state == State::Connected;
        if let Some(bss) = &self.target {
            if !matches!(self.state, State::Idle | State::Failed(_)) {
                let bssid = bss.bssid;
                let seq = self.next_seq();
                out.push(Output::Transmit(frame::deauth(self.mac, bssid, REASON_LEAVING, seq)));
            }
        }
        if was_up {
            out.push(Output::Link(false));
        }
        self.reset(State::Idle);
        out
    }

    fn reset(&mut self, state: State) {
        self.state = state;
        self.supplicant = None;
        self.keys = None;
        self.pmk = None;
        if self.state == State::Idle {
            self.target = None;
        }
    }

    fn fail(&mut self, reason: &str) -> Vec<Output> {
        let mut out = Vec::new();
        if let Some(bss) = &self.target {
            let bssid = bss.bssid;
            let seq = self.next_seq();
            out.push(Output::Transmit(frame::deauth(self.mac, bssid, REASON_LEAVING, seq)));
        }
        if self.state == State::Connected {
            out.push(Output::Link(false));
        }
        self.reset(State::Failed(String::from(reason)));
        out
    }

    /// Retries and timeouts; call every few hundred milliseconds.
    pub fn tick(&mut self, now_ms: u64) -> Vec<Output> {
        if now_ms < self.deadline {
            return Vec::new();
        }
        let Some(bss) = self.target.clone() else { return Vec::new() };
        match self.state {
            State::Authenticating | State::Associating if self.tries < TRIES => {
                self.tries += 1;
                let seq = self.next_seq();
                let f = if self.state == State::Authenticating {
                    self.deadline = now_ms + AUTH_TIMEOUT_MS;
                    frame::auth_request(self.mac, bss.bssid, seq)
                } else {
                    self.deadline = now_ms + ASSOC_TIMEOUT_MS;
                    self.assoc_frame(&bss, seq)
                };
                alloc::vec![Output::Transmit(f)]
            }
            State::Authenticating | State::Associating => self.fail("The network did not answer"),
            State::Handshake => self.fail("Could not join: check the password"),
            _ => Vec::new(),
        }
    }

    fn assoc_frame(&self, bss: &Bss, seq: u16) -> Vec<u8> {
        let ie = (bss.security != Security::Open).then(rsn::station_ie);
        frame::assoc_request(self.mac, bss, ie.as_deref(), seq)
    }

    /// Handles a received 802.11 frame.
    pub fn receive(&mut self, f: &[u8], now_ms: u64) -> Vec<Output> {
        let Some(h) = Header::parse(f) else { return Vec::new() };
        match h.kind {
            TYPE_MGMT => self.management(f, &h, now_ms),
            TYPE_DATA => self.data(f, &h),
            _ => Vec::new(),
        }
    }

    fn management(&mut self, f: &[u8], h: &Header, now_ms: u64) -> Vec<Output> {
        if let Some(bss) = frame::parse_bss(f) {
            self.seen.insert(bss.bssid, Seen { bss, last_ms: now_ms });
            return Vec::new();
        }
        if h.addr1 != self.mac {
            return Vec::new();
        }
        let Some(target) = self.target.clone() else { return Vec::new() };
        if let Some(reason) = frame::disconnect_reason(f, target.bssid) {
            if !matches!(self.state, State::Idle | State::Failed(_)) {
                let was_up = self.state == State::Connected;
                self.reset(State::Failed(alloc::format!("Disconnected by the network (reason {reason})")));
                return if was_up { alloc::vec![Output::Link(false)] } else { Vec::new() };
            }
            return Vec::new();
        }
        match self.state {
            State::Authenticating => match frame::auth_response_status(f, target.bssid) {
                Some(0) => {
                    let seq = self.next_seq();
                    self.state = State::Associating;
                    self.tries = 1;
                    self.deadline = now_ms + ASSOC_TIMEOUT_MS;
                    alloc::vec![Output::Transmit(self.assoc_frame(&target, seq))]
                }
                Some(status) => self.fail(&alloc::format!("The network refused to authenticate (status {status})")),
                None => Vec::new(),
            },
            State::Associating => match frame::assoc_response(f, target.bssid) {
                Some((0, _)) if target.security == Security::Open => {
                    self.keys = None;
                    self.state = State::Connected;
                    alloc::vec![Output::Link(true)]
                }
                Some((0, _)) => {
                    self.state = State::Handshake;
                    self.deadline = now_ms + HANDSHAKE_TIMEOUT_MS;
                    Vec::new()
                }
                Some((status, _)) => self.fail(&alloc::format!("The network refused to associate (status {status})")),
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }

    fn data(&mut self, f: &[u8], h: &Header) -> Vec<Output> {
        let Some(target) = &self.target else { return Vec::new() };
        // Only frames from our access point (FromDS, transmitter = BSSID).
        if h.flags & FLAG_FROM_DS == 0 || h.addr2 != target.bssid {
            return Vec::new();
        }
        let multicast = h.addr1[0] & 1 != 0;
        if !multicast && h.addr1 != self.mac {
            return Vec::new();
        }
        let plain = if f[1] & frame::FLAG_PROTECTED != 0 {
            let Some(p) = self.decrypt(f, h, multicast) else { return Vec::new() };
            p
        } else if self.keys.is_some() && !self.is_eapol(f, h) {
            return Vec::new(); // unprotected data on a protected link
        } else {
            f.to_vec()
        };
        let Some(eth) = frame::ethernet_from_data(&plain) else { return Vec::new() };
        if u16::from_be_bytes([eth[12], eth[13]]) == ETHERTYPE_EAPOL {
            return self.eapol(&eth[14..]);
        }
        if self.state != State::Connected {
            return Vec::new();
        }
        alloc::vec![Output::Receive(eth)]
    }

    fn is_eapol(&self, f: &[u8], h: &Header) -> bool {
        f.get(h.len + 6..h.len + 8) == Some(&ETHERTYPE_EAPOL.to_be_bytes())
    }

    fn decrypt(&mut self, f: &[u8], h: &Header, multicast: bool) -> Option<Vec<u8>> {
        let keys = self.keys.as_mut()?;
        if multicast {
            let gtk = keys.gtk.as_ref()?;
            let key: [u8; 16] = gtk.key.as_slice().try_into().ok()?;
            let (plain, pn, key_id) = crypto::ccmp_decrypt(&key, f)?;
            // The RSC from the handshake is the last packet number already sent.
            let last = keys.gtk_rx_pn.or((gtk.rsc > 0).then_some(gtk.rsc));
            if key_id != gtk.key_id || last.is_some_and(|l| pn <= l) {
                return None;
            }
            keys.gtk_rx_pn = Some(pn);
            Some(plain)
        } else {
            let (plain, pn, _) = crypto::ccmp_decrypt(&keys.tk, f)?;
            let slot = if h.qos.is_some() { h.tid() as usize } else { 16 };
            if keys.rx_pn[slot].is_some_and(|last| pn <= last) {
                return None; // replayed
            }
            keys.rx_pn[slot] = Some(pn);
            Some(plain)
        }
    }

    fn eapol(&mut self, eapol: &[u8]) -> Vec<Output> {
        let Some(s) = self.supplicant.as_mut() else { return Vec::new() };
        match s.receive(eapol) {
            Event::Send(reply) => self.send_eapol(&reply),
            Event::Connected { send, ptk, gtk } => {
                // Message 4 goes out unencrypted; everything after it is protected.
                let mut out = self.send_eapol(&send);
                self.install(&ptk, gtk);
                self.state = State::Connected;
                out.push(Output::Link(true));
                out
            }
            Event::GroupRekey { send, gtk } => {
                let out = self.send_eapol(&send);
                if let Some(k) = self.keys.as_mut() {
                    k.gtk = Some(gtk);
                    k.gtk_rx_pn = None;
                }
                out
            }
            Event::Failed(reason) => self.fail(reason),
            Event::Ignored => Vec::new(),
        }
    }

    fn install(&mut self, ptk: &Ptk, gtk: Gtk) {
        self.keys = Some(Keys { tk: ptk.tk, tx_pn: 0, rx_pn: [None; 17], gtk: Some(gtk), gtk_rx_pn: None });
    }

    fn send_eapol(&mut self, payload: &[u8]) -> Vec<Output> {
        let Some(bss) = &self.target else { return Vec::new() };
        let mut eth = Vec::with_capacity(14 + payload.len());
        eth.extend_from_slice(&bss.bssid);
        eth.extend_from_slice(&self.mac);
        eth.extend_from_slice(&ETHERTYPE_EAPOL.to_be_bytes());
        eth.extend_from_slice(payload);
        self.send(&eth).map(Output::Transmit).into_iter().collect()
    }

    /// An Ethernet frame from the network stack, as an 802.11 frame to
    /// transmit (encrypted once keys are installed). `None` when not connected.
    pub fn send(&mut self, eth: &[u8]) -> Option<Vec<u8>> {
        let bssid = self.target.as_ref()?.bssid;
        let eapol = eth.get(12..14) == Some(&ETHERTYPE_EAPOL.to_be_bytes());
        if self.state != State::Connected && !(eapol && self.state == State::Handshake) {
            return None;
        }
        let seq = self.next_seq();
        let f = frame::data_from_ethernet(eth, bssid, seq)?;
        match self.keys.as_mut() {
            Some(k) => {
                k.tx_pn += 1;
                crypto::ccmp_encrypt(&k.tk, &f, k.tx_pn, 0)
            }
            None => Some(f),
        }
    }
}
