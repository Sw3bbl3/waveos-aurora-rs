//! The WPA2-Personal supplicant: EAPOL-Key frames and the 4-way and group
//! key handshakes (IEEE 802.11-2020 §12.7.6–12.7.7), for CCMP with key
//! descriptor version 2 (HMAC-SHA1 MICs, AES key wrap).
//!
//! The station side only: it answers what the access point sends, checks
//! every MIC and replay counter, and hands out keys to install.

use crate::crypto::{self, Ptk};
use crate::frame::Mac;
use alloc::vec::Vec;

/// EtherType of EAPOL frames (they travel as ordinary data frames).
pub const ETHERTYPE_EAPOL: u16 = 0x888E;

// Key information bits.
pub const KI_VERSION_MASK: u16 = 0x0007;
pub const KI_VERSION_AES_HMAC_SHA1: u16 = 2;
pub const KI_PAIRWISE: u16 = 0x0008;
pub const KI_INSTALL: u16 = 0x0040;
pub const KI_ACK: u16 = 0x0080;
pub const KI_MIC: u16 = 0x0100;
pub const KI_SECURE: u16 = 0x0200;
pub const KI_ERROR: u16 = 0x0400;
pub const KI_REQUEST: u16 = 0x0800;
pub const KI_ENCRYPTED: u16 = 0x1000;

const DESC_RSN: u8 = 2;
/// Offsets inside an EAPOL-Key frame (EAPOL header included).
const OFF_KEY_INFO: usize = 5;
const OFF_REPLAY: usize = 9;
const OFF_NONCE: usize = 17;
const OFF_RSC: usize = 65;
const OFF_MIC: usize = 81;
const OFF_DATA_LEN: usize = 97;
const HEADER_LEN: usize = 99;

/// A parsed EAPOL-Key frame.
#[derive(Clone, Debug)]
pub struct KeyFrame<'a> {
    pub raw: &'a [u8],
    pub key_info: u16,
    pub replay: u64,
    pub nonce: [u8; 32],
    pub rsc: [u8; 8],
    pub mic: [u8; 16],
    pub data: &'a [u8],
}

impl<'a> KeyFrame<'a> {
    /// Parses an EAPOL frame (the payload after the EtherType).
    pub fn parse(raw: &'a [u8]) -> Option<KeyFrame<'a>> {
        if raw.len() < HEADER_LEN || raw[1] != 3 || raw[4] != DESC_RSN {
            return None; // not an EAPOL-Key frame with the RSN descriptor
        }
        let body_len = u16::from_be_bytes([raw[2], raw[3]]) as usize;
        let raw = raw.get(..4 + body_len)?;
        let data_len = u16::from_be_bytes([raw[OFF_DATA_LEN], raw[OFF_DATA_LEN + 1]]) as usize;
        let data = raw.get(HEADER_LEN..HEADER_LEN + data_len)?;
        let mut f = KeyFrame {
            raw,
            key_info: u16::from_be_bytes([raw[OFF_KEY_INFO], raw[OFF_KEY_INFO + 1]]),
            replay: u64::from_be_bytes(raw[OFF_REPLAY..OFF_REPLAY + 8].try_into().ok()?),
            nonce: [0; 32],
            rsc: [0; 8],
            mic: [0; 16],
            data,
        };
        f.nonce.copy_from_slice(&raw[OFF_NONCE..OFF_NONCE + 32]);
        f.rsc.copy_from_slice(&raw[OFF_RSC..OFF_RSC + 8]);
        f.mic.copy_from_slice(&raw[OFF_MIC..OFF_MIC + 16]);
        Some(f)
    }

    /// Whether the MIC is right for `kck`.
    pub fn mic_valid(&self, kck: &[u8; 16]) -> bool {
        let mut copy = self.raw.to_vec();
        copy[OFF_MIC..OFF_MIC + 16].fill(0);
        crypto::same(&crypto::eapol_mic(kck, &copy), &self.mic)
    }
}

/// Builds an EAPOL-Key frame from the station; `kck` signs it.
fn key_frame(key_info: u16, replay: u64, nonce: &[u8; 32], data: &[u8], kck: Option<&[u8; 16]>) -> Vec<u8> {
    let body = HEADER_LEN - 4 + data.len();
    let mut f = alloc::vec![0u8; HEADER_LEN + data.len()];
    f[0] = 1; // 802.1X-2001: what every access point accepts
    f[1] = 3; // EAPOL-Key
    f[2..4].copy_from_slice(&(body as u16).to_be_bytes());
    f[4] = DESC_RSN;
    f[OFF_KEY_INFO..OFF_KEY_INFO + 2].copy_from_slice(&key_info.to_be_bytes());
    // Key length: 0 in station frames.
    f[OFF_REPLAY..OFF_REPLAY + 8].copy_from_slice(&replay.to_be_bytes());
    f[OFF_NONCE..OFF_NONCE + 32].copy_from_slice(nonce);
    f[OFF_DATA_LEN..OFF_DATA_LEN + 2].copy_from_slice(&(data.len() as u16).to_be_bytes());
    f[HEADER_LEN..].copy_from_slice(data);
    if let Some(kck) = kck {
        let mic = crypto::eapol_mic(kck, &f);
        f[OFF_MIC..OFF_MIC + 16].copy_from_slice(&mic);
    }
    f
}

/// A group key, from a GTK KDE.
#[derive(Clone, PartialEq, Eq)]
pub struct Gtk {
    pub key_id: u8,
    pub key: Vec<u8>,
    /// Receive sequence counter: the last packet number seen (little-endian).
    pub rsc: u64,
}

impl core::fmt::Debug for Gtk {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "Gtk(id {}, {} bytes)", self.key_id, self.key.len())
    }
}

/// What the station should do after a frame from the access point.
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    /// Send this EAPOL frame (payload after the EtherType) to the access point.
    Send(Vec<u8>),
    /// The 4-way handshake finished: send the frame (message 4), then
    /// install the pairwise key and the group key.
    Connected { send: Vec<u8>, ptk: Ptk, gtk: Gtk },
    /// A new group key (rekeying): send the reply, then install the key.
    GroupRekey { send: Vec<u8>, gtk: Gtk },
    /// The handshake cannot succeed: most likely a wrong password.
    Failed(&'static str),
    /// Nothing to do (duplicate, stale or unexpected frame: dropped).
    Ignored,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    /// Waiting for message 1.
    Start,
    /// Message 2 sent; waiting for message 3.
    SentMsg2,
    Done,
}

pub struct Supplicant {
    pmk: [u8; 32],
    spa: Mac,
    aa: Mac,
    /// Our RSN element (sent in message 2) and the AP's (checked in message 3).
    own_ie: Vec<u8>,
    ap_ie: Vec<u8>,
    snonce: [u8; 32],
    /// The PTK derived for the current ANonce.
    tptk: Option<Ptk>,
    ptk: Option<Ptk>,
    replay: Option<u64>,
    state: State,
}

impl Supplicant {
    /// `snonce` must be fresh random bytes for every connection.
    pub fn new(pmk: [u8; 32], spa: Mac, aa: Mac, own_ie: Vec<u8>, ap_ie: Vec<u8>, snonce: [u8; 32]) -> Supplicant {
        Supplicant { pmk, spa, aa, own_ie, ap_ie, snonce, tptk: None, ptk: None, replay: None, state: State::Start }
    }

    /// The access point's replay counter must increase with every frame.
    fn fresh(&self, replay: u64) -> bool {
        self.replay.is_none_or(|r| replay > r)
    }

    /// Handles one EAPOL frame from the access point.
    pub fn receive(&mut self, eapol: &[u8]) -> Event {
        let Some(k) = KeyFrame::parse(eapol) else { return Event::Ignored };
        if k.key_info & KI_VERSION_MASK != KI_VERSION_AES_HMAC_SHA1 || k.key_info & KI_ACK == 0 {
            return Event::Ignored;
        }
        if !self.fresh(k.replay) {
            return Event::Ignored;
        }
        match (k.key_info & KI_PAIRWISE != 0, k.key_info & KI_MIC != 0) {
            (true, false) => self.message1(&k),
            (true, true) => self.message3(&k),
            (false, true) => self.group1(&k),
            (false, false) => Event::Ignored,
        }
    }

    /// Message 1: the ANonce. Derive a PTK and answer with our SNonce.
    fn message1(&mut self, k: &KeyFrame) -> Event {
        if self.state == State::Done {
            return Event::Ignored; // a new handshake needs a new Supplicant
        }
        let tptk = crypto::ptk(&self.pmk, &self.aa, &self.spa, &k.nonce, &self.snonce);
        let info = KI_VERSION_AES_HMAC_SHA1 | KI_PAIRWISE | KI_MIC;
        let frame = key_frame(info, k.replay, &self.snonce, &self.own_ie, Some(&tptk.kck));
        self.replay = Some(k.replay);
        self.tptk = Some(tptk);
        self.state = State::SentMsg2;
        Event::Send(frame)
    }

    /// Message 3: install. Check the MIC (a wrong password fails here), that
    /// the AP's RSN element matches its beacon, and unwrap the group key.
    fn message3(&mut self, k: &KeyFrame) -> Event {
        let Some(tptk) = self.tptk.clone() else { return Event::Ignored };
        if self.state != State::SentMsg2 {
            return Event::Ignored;
        }
        if !k.mic_valid(&tptk.kck) {
            return Event::Failed("the network rejected the password");
        }
        if k.key_info & (KI_INSTALL | KI_SECURE | KI_ENCRYPTED) != KI_INSTALL | KI_SECURE | KI_ENCRYPTED {
            return Event::Failed("message 3 without an encrypted group key");
        }
        let Some(data) = crypto::unwrap_key(&tptk.kek, k.data) else {
            return Event::Failed("the group key could not be unwrapped");
        };
        let rsn = kdes(&data).find(|(id, _)| *id == 48).map(|(_, b)| b);
        if rsn.is_none_or(|ie| ie != self.ap_ie.as_slice()) {
            return Event::Failed("the access point's security settings changed (possible attack)");
        }
        let Some(mut gtk) = gtk_from(&data) else { return Event::Failed("no group key in message 3") };
        gtk.rsc = u64::from_le_bytes(k.rsc);
        self.replay = Some(k.replay);
        let info = KI_VERSION_AES_HMAC_SHA1 | KI_PAIRWISE | KI_MIC | KI_SECURE;
        let frame = key_frame(info, k.replay, &[0; 32], &[], Some(&tptk.kck));
        self.ptk = Some(tptk.clone());
        self.state = State::Done;
        Event::Connected { send: frame, ptk: tptk, gtk }
    }

    /// Group key handshake, message 1 (after the 4-way handshake): a new GTK.
    fn group1(&mut self, k: &KeyFrame) -> Event {
        let Some(ptk) = self.ptk.clone() else { return Event::Ignored };
        if !k.mic_valid(&ptk.kck) {
            return Event::Ignored;
        }
        if k.key_info & (KI_SECURE | KI_ENCRYPTED) != KI_SECURE | KI_ENCRYPTED {
            return Event::Ignored;
        }
        let Some(data) = crypto::unwrap_key(&ptk.kek, k.data) else { return Event::Ignored };
        let Some(mut gtk) = gtk_from(&data) else { return Event::Ignored };
        gtk.rsc = u64::from_le_bytes(k.rsc);
        self.replay = Some(k.replay);
        let info = KI_VERSION_AES_HMAC_SHA1 | KI_MIC | KI_SECURE;
        let frame = key_frame(info, k.replay, &[0; 32], &[], Some(&ptk.kck));
        Event::GroupRekey { send: frame, gtk }
    }
}

/// Iterates the elements and KDEs in decrypted key data: (id, whole element).
/// KDEs are vendor elements (221) with the 00-0F-AC OUI; padding (0xDD 0x00) ends it.
fn kdes(data: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0;
    core::iter::from_fn(move || {
        let id = *data.get(at)?;
        let len = *data.get(at + 1)? as usize;
        if id == 0xDD && len == 0 {
            return None;
        }
        let whole = data.get(at..at + 2 + len)?;
        at += 2 + len;
        Some((id, whole))
    })
}

/// The GTK KDE: DD len 00-0F-AC 01, then key id/Tx byte, reserved, key.
fn gtk_from(data: &[u8]) -> Option<Gtk> {
    let (_, kde) = kdes(data).find(|(id, e)| *id == 0xDD && e.len() >= 8 && e[2..6] == [0x00, 0x0F, 0xAC, 0x01])?;
    let key = kde[8..].to_vec();
    if key.len() != 16 {
        return None; // CCMP group keys only
    }
    Some(Gtk { key_id: kde[6] & 3, key, rsc: 0 })
}

