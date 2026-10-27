//! 802.11 frames: the MAC header, the management frames a station needs
//! (beacons and probe responses in; probe requests, authentication,
//! association and deauthentication out) and data frames, which carry
//! Ethernet payloads behind an LLC/SNAP header.

use crate::rsn::{Rsn, Security};
use alloc::string::String;
use alloc::vec::Vec;

pub type Mac = [u8; 6];
pub const BROADCAST: Mac = [0xFF; 6];

// Frame control: type and subtype.
pub const TYPE_MGMT: u8 = 0;
pub const TYPE_DATA: u8 = 2;
pub const SUBTYPE_ASSOC_REQ: u8 = 0;
pub const SUBTYPE_ASSOC_RESP: u8 = 1;
pub const SUBTYPE_PROBE_REQ: u8 = 4;
pub const SUBTYPE_PROBE_RESP: u8 = 5;
pub const SUBTYPE_BEACON: u8 = 8;
pub const SUBTYPE_DISASSOC: u8 = 10;
pub const SUBTYPE_AUTH: u8 = 11;
pub const SUBTYPE_DEAUTH: u8 = 12;
/// Data subtypes with this bit carry a QoS control field.
pub const SUBTYPE_QOS: u8 = 0x8;

// Frame control flags (second byte).
pub const FLAG_TO_DS: u8 = 0x01;
pub const FLAG_FROM_DS: u8 = 0x02;
pub const FLAG_MORE_FRAGS: u8 = 0x04;
pub const FLAG_RETRY: u8 = 0x08;
pub const FLAG_PWR_MGMT: u8 = 0x10;
pub const FLAG_MORE_DATA: u8 = 0x20;
pub const FLAG_PROTECTED: u8 = 0x40;
pub const FLAG_ORDER: u8 = 0x80;

// Information elements.
pub const IE_SSID: u8 = 0;
pub const IE_RATES: u8 = 1;
pub const IE_DS_PARAMS: u8 = 3;
pub const IE_RSN: u8 = 48;
pub const IE_EXT_RATES: u8 = 50;
pub const IE_VENDOR: u8 = 221;

/// The parsed MAC header of any frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub subtype: u8,
    pub flags: u8,
    pub duration: u16,
    pub addr1: Mac,
    pub addr2: Mac,
    pub addr3: Mac,
    pub seq_ctrl: u16,
    /// Present only when both ToDS and FromDS are set (mesh/WDS).
    pub addr4: Option<Mac>,
    /// QoS control, for QoS data frames.
    pub qos: Option<u16>,
    /// Header length in bytes (including QoS and HT control).
    pub len: usize,
}

fn mac_at(b: &[u8], at: usize) -> Mac {
    let mut m = [0; 6];
    m.copy_from_slice(&b[at..at + 6]);
    m
}

impl Header {
    pub fn parse(b: &[u8]) -> Option<Header> {
        if b.len() < 24 {
            return None;
        }
        let fc0 = b[0];
        if fc0 & 3 != 0 {
            return None; // protocol version 0 only
        }
        let kind = (fc0 >> 2) & 3;
        let subtype = fc0 >> 4;
        let flags = b[1];
        let mut len = 24;
        let addr4 = if kind == TYPE_DATA && flags & (FLAG_TO_DS | FLAG_FROM_DS) == FLAG_TO_DS | FLAG_FROM_DS {
            len += 6;
            Some(mac_at(b.get(..30)?, 24))
        } else {
            None
        };
        let qos = if kind == TYPE_DATA && subtype & SUBTYPE_QOS != 0 {
            let q = b.get(len..len + 2)?;
            len += 2;
            Some(u16::from_le_bytes([q[0], q[1]]))
        } else {
            None
        };
        // HT control follows in QoS data and management frames with Order set.
        if flags & FLAG_ORDER != 0 && (qos.is_some() || kind == TYPE_MGMT) {
            len += 4;
        }
        if b.len() < len {
            return None;
        }
        Some(Header {
            kind,
            subtype,
            flags,
            duration: u16::from_le_bytes([b[2], b[3]]),
            addr1: mac_at(b, 4),
            addr2: mac_at(b, 10),
            addr3: mac_at(b, 16),
            seq_ctrl: u16::from_le_bytes([b[22], b[23]]),
            addr4,
            qos,
            len,
        })
    }

    /// The traffic identifier (QoS priority) of a QoS data frame, else 0.
    pub fn tid(&self) -> u8 {
        self.qos.map_or(0, |q| (q & 0xF) as u8)
    }
}

/// Writes a 24-byte header (no QoS, no address 4) with addresses 1, 2 and 3.
fn header(out: &mut Vec<u8>, kind: u8, subtype: u8, flags: u8, addrs: [Mac; 3], seq: u16) {
    out.push(kind << 2 | subtype << 4);
    out.push(flags);
    out.extend_from_slice(&[0, 0]); // duration: the hardware fills it in
    for a in addrs {
        out.extend_from_slice(&a);
    }
    out.extend_from_slice(&(seq << 4).to_le_bytes());
}

/// Iterates the information elements in `b`: (id, body). Stops at the first
/// element that runs past the end.
pub fn elements(b: &[u8]) -> impl Iterator<Item = (u8, &[u8])> {
    let mut at = 0;
    core::iter::from_fn(move || {
        let id = *b.get(at)?;
        let len = *b.get(at + 1)? as usize;
        let body = b.get(at + 2..at + 2 + len)?;
        at += 2 + len;
        Some((id, body))
    })
}

/// A network seen in a beacon or probe response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bss {
    pub bssid: Mac,
    /// The name; empty for hidden networks.
    pub ssid: String,
    /// Raw SSID bytes (not always UTF-8).
    pub ssid_raw: Vec<u8>,
    pub channel: u8,
    pub beacon_interval: u16,
    pub capabilities: u16,
    pub security: Security,
    /// The RSN element exactly as the AP sent it (the 4-way handshake checks it).
    pub rsn_ie: Option<Vec<u8>>,
}

/// Capability bit: the network requires privacy (WEP or better).
pub const CAP_PRIVACY: u16 = 0x0010;

/// Parses a beacon or probe response (the whole frame, header included).
pub fn parse_bss(frame: &[u8]) -> Option<Bss> {
    let h = Header::parse(frame)?;
    if h.kind != TYPE_MGMT || !matches!(h.subtype, SUBTYPE_BEACON | SUBTYPE_PROBE_RESP) {
        return None;
    }
    // Timestamp (8), beacon interval (2), capabilities (2), then elements.
    let body = frame.get(h.len..)?;
    if body.len() < 12 {
        return None;
    }
    let beacon_interval = u16::from_le_bytes([body[8], body[9]]);
    let capabilities = u16::from_le_bytes([body[10], body[11]]);
    let (mut ssid_raw, mut channel, mut rsn_ie, mut wpa1) = (Vec::new(), 0, None, false);
    for (id, data) in elements(&body[12..]) {
        match id {
            IE_SSID if data.len() <= 32 => ssid_raw = data.to_vec(),
            IE_DS_PARAMS if data.len() == 1 => channel = data[0],
            IE_RSN => {
                let mut ie = Vec::with_capacity(data.len() + 2);
                ie.extend_from_slice(&[IE_RSN, data.len() as u8]);
                ie.extend_from_slice(data);
                rsn_ie = Some(ie);
            }
            // The Microsoft WPA (version 1) element.
            IE_VENDOR if data.len() >= 4 && data[..4] == [0x00, 0x50, 0xF2, 0x01] => wpa1 = true,
            _ => {}
        }
    }
    // Hidden networks send an empty or zeroed SSID.
    let hidden = ssid_raw.iter().all(|&c| c == 0);
    let ssid = if hidden { String::new() } else { String::from_utf8_lossy(&ssid_raw).into_owned() };
    let security = match rsn_ie.as_deref().and_then(|ie| Rsn::parse(&ie[2..])) {
        Some(rsn) => rsn.security(),
        None if wpa1 => Security::Wpa1,
        None if capabilities & CAP_PRIVACY != 0 => Security::Wep,
        None => Security::Open,
    };
    Some(Bss {
        bssid: h.addr3,
        ssid,
        ssid_raw: if hidden { Vec::new() } else { ssid_raw },
        channel,
        beacon_interval,
        capabilities,
        security,
        rsn_ie,
    })
}

/// Rates offered in probe and association requests (500 kb/s units; the
/// basic 802.11b rates are marked with the top bit).
const RATES: [u8; 8] = [0x82, 0x84, 0x8B, 0x96, 0x0C, 0x12, 0x18, 0x24];
const EXT_RATES: [u8; 4] = [0x30, 0x48, 0x60, 0x6C];

fn push_ie(out: &mut Vec<u8>, id: u8, data: &[u8]) {
    out.push(id);
    out.push(data.len() as u8);
    out.extend_from_slice(data);
}

/// A broadcast probe request, for `ssid` (empty: any network).
pub fn probe_request(sta: Mac, ssid: &[u8], seq: u16) -> Vec<u8> {
    let mut f = Vec::with_capacity(64);
    header(&mut f, TYPE_MGMT, SUBTYPE_PROBE_REQ, 0, [BROADCAST, sta, BROADCAST], seq);
    push_ie(&mut f, IE_SSID, ssid);
    push_ie(&mut f, IE_RATES, &RATES);
    push_ie(&mut f, IE_EXT_RATES, &EXT_RATES);
    f
}

/// Open System authentication, first frame.
pub fn auth_request(sta: Mac, bssid: Mac, seq: u16) -> Vec<u8> {
    let mut f = Vec::with_capacity(30);
    header(&mut f, TYPE_MGMT, SUBTYPE_AUTH, 0, [bssid, sta, bssid], seq);
    f.extend_from_slice(&0u16.to_le_bytes()); // algorithm: open system
    f.extend_from_slice(&1u16.to_le_bytes()); // transaction sequence
    f.extend_from_slice(&0u16.to_le_bytes()); // status
    f
}

/// The status code of an authentication response from `bssid` (0 = success).
pub fn auth_response_status(frame: &[u8], bssid: Mac) -> Option<u16> {
    let h = Header::parse(frame)?;
    let b = frame.get(h.len..h.len + 6)?;
    if h.kind != TYPE_MGMT || h.subtype != SUBTYPE_AUTH || h.addr2 != bssid || u16::from_le_bytes([b[2], b[3]]) != 2 {
        return None;
    }
    Some(u16::from_le_bytes([b[4], b[5]]))
}

/// An association request carrying our RSN element (if the network is protected).
pub fn assoc_request(sta: Mac, bss: &Bss, rsn_ie: Option<&[u8]>, seq: u16) -> Vec<u8> {
    let mut f = Vec::with_capacity(96);
    header(&mut f, TYPE_MGMT, SUBTYPE_ASSOC_REQ, 0, [bss.bssid, sta, bss.bssid], seq);
    // Capabilities: ESS, plus privacy when protected; listen interval 10 beacons.
    let caps: u16 = 0x0001 | if rsn_ie.is_some() { CAP_PRIVACY } else { 0 };
    f.extend_from_slice(&caps.to_le_bytes());
    f.extend_from_slice(&10u16.to_le_bytes());
    push_ie(&mut f, IE_SSID, &bss.ssid_raw);
    push_ie(&mut f, IE_RATES, &RATES);
    push_ie(&mut f, IE_EXT_RATES, &EXT_RATES);
    if let Some(ie) = rsn_ie {
        f.extend_from_slice(ie);
    }
    f
}

/// (status code, association ID) of an association response from `bssid`.
pub fn assoc_response(frame: &[u8], bssid: Mac) -> Option<(u16, u16)> {
    let h = Header::parse(frame)?;
    let b = frame.get(h.len..h.len + 6)?;
    if h.kind != TYPE_MGMT || h.subtype != SUBTYPE_ASSOC_RESP || h.addr2 != bssid {
        return None;
    }
    Some((u16::from_le_bytes([b[2], b[3]]), u16::from_le_bytes([b[4], b[5]]) & 0x3FFF))
}

/// Deauthentication (leaving the network) with a reason code.
pub fn deauth(sta: Mac, bssid: Mac, reason: u16, seq: u16) -> Vec<u8> {
    let mut f = Vec::with_capacity(26);
    header(&mut f, TYPE_MGMT, SUBTYPE_DEAUTH, 0, [bssid, sta, bssid], seq);
    f.extend_from_slice(&reason.to_le_bytes());
    f
}

/// The reason code, if `frame` is a deauthentication or disassociation from `bssid`.
pub fn disconnect_reason(frame: &[u8], bssid: Mac) -> Option<u16> {
    let h = Header::parse(frame)?;
    let b = frame.get(h.len..h.len + 2)?;
    (h.kind == TYPE_MGMT && matches!(h.subtype, SUBTYPE_DEAUTH | SUBTYPE_DISASSOC) && h.addr2 == bssid)
        .then(|| u16::from_le_bytes([b[0], b[1]]))
}

const LLC_SNAP: [u8; 6] = [0xAA, 0xAA, 0x03, 0x00, 0x00, 0x00];

/// Wraps an Ethernet frame (destination, source, type, payload) as a data
/// frame from a station to its access point (ToDS). Unprotected: CCMP is
/// applied afterwards when keys are installed.
pub fn data_from_ethernet(eth: &[u8], bssid: Mac, seq: u16) -> Option<Vec<u8>> {
    if eth.len() < 14 {
        return None;
    }
    let (dst, src) = (mac_at(eth, 0), mac_at(eth, 6));
    let mut f = Vec::with_capacity(eth.len() + 24 + 8 - 6);
    header(&mut f, TYPE_DATA, 0, FLAG_TO_DS, [bssid, src, dst], seq);
    f.extend_from_slice(&LLC_SNAP);
    f.extend_from_slice(&eth[12..]); // EtherType and payload
    Some(f)
}

/// Unwraps a (decrypted) data frame from the access point into an Ethernet
/// frame, or `None` for null/non-SNAP frames.
pub fn ethernet_from_data(frame: &[u8]) -> Option<Vec<u8>> {
    let h = Header::parse(frame)?;
    if h.kind != TYPE_DATA || h.subtype & 0x4 != 0 {
        return None; // not data, or a null-data subtype
    }
    let (dst, src) = match h.flags & (FLAG_TO_DS | FLAG_FROM_DS) {
        0 => (h.addr1, h.addr2),
        FLAG_FROM_DS => (h.addr1, h.addr3),
        FLAG_TO_DS => (h.addr3, h.addr2),
        _ => (h.addr3, h.addr4?),
    };
    let body = frame.get(h.len..)?;
    if body.len() < 8 || body[..6] != LLC_SNAP {
        return None;
    }
    let mut eth = Vec::with_capacity(body.len() + 6);
    eth.extend_from_slice(&dst);
    eth.extend_from_slice(&src);
    eth.extend_from_slice(&body[6..]);
    Some(eth)
}
