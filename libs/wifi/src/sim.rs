//! A simulated access point, for testing stations without a radio: it
//! beacons, answers authentication and association, runs the WPA2 4-way
//! handshake as the authenticator, and exchanges CCMP-protected data.
//! Used by the host tests and the kernel's self-test.

use crate::crypto::{self, Ptk};
use crate::frame::{self, Header, Mac, FLAG_FROM_DS, TYPE_DATA, TYPE_MGMT};
use crate::supplicant::ETHERTYPE_EAPOL;
use alloc::vec;
use alloc::vec::Vec;

/// RSN element: group CCMP, pairwise CCMP, AKM PSK.
pub fn wpa2_ie() -> Vec<u8> {
    let mut ie = vec![48, 20, 1, 0];
    ie.extend_from_slice(&[0x00, 0x0F, 0xAC, 4]);
    ie.extend_from_slice(&[1, 0, 0x00, 0x0F, 0xAC, 4]);
    ie.extend_from_slice(&[1, 0, 0x00, 0x0F, 0xAC, 2]);
    ie.extend_from_slice(&[0, 0]);
    ie
}

const KI_V2: u16 = 0x0002;
const KI_PAIRWISE: u16 = 0x0008;
const KI_INSTALL: u16 = 0x0040;
const KI_ACK: u16 = 0x0080;
const KI_MIC: u16 = 0x0100;
const KI_SECURE: u16 = 0x0200;
const KI_ENCRYPTED: u16 = 0x1000;

pub struct AccessPoint {
    pub bssid: Mac,
    pub ssid: Vec<u8>,
    pub channel: u8,
    /// `None`: an open network.
    pmk: Option<[u8; 32]>,
    anonce: [u8; 32],
    replay: u64,
    pub ptk: Option<Ptk>,
    /// The PTK between messages 2 and 4.
    pending: Option<Ptk>,
    pub gtk: [u8; 16],
    pub gtk_id: u8,
    pn: u64,
    gtk_pn: u64,
    seq: u16,
    /// The associated station, once the handshake (if any) completed.
    pub connected: Option<Mac>,
    station: Option<Mac>,
    /// Ethernet frames the station sent us (decrypted).
    pub received: Vec<Vec<u8>>,
}

fn header(fc: [u8; 2], a1: Mac, a2: Mac, a3: Mac, seq: u16) -> Vec<u8> {
    let mut f = fc.to_vec();
    f.extend_from_slice(&[0, 0]);
    f.extend_from_slice(&a1);
    f.extend_from_slice(&a2);
    f.extend_from_slice(&a3);
    f.extend_from_slice(&(seq << 4).to_le_bytes());
    f
}

fn key_frame(info: u16, replay: u64, nonce: &[u8; 32], rsc: u64, data: &[u8], kck: Option<&[u8; 16]>) -> Vec<u8> {
    let mut f = vec![1, 3];
    f.extend_from_slice(&((95 + data.len()) as u16).to_be_bytes());
    f.push(2);
    f.extend_from_slice(&info.to_be_bytes());
    f.extend_from_slice(&16u16.to_be_bytes());
    f.extend_from_slice(&replay.to_be_bytes());
    f.extend_from_slice(nonce);
    f.extend_from_slice(&[0; 16]);
    f.extend_from_slice(&rsc.to_le_bytes());
    f.extend_from_slice(&[0; 8]);
    f.extend_from_slice(&[0; 16]);
    f.extend_from_slice(&(data.len() as u16).to_be_bytes());
    f.extend_from_slice(data);
    if let Some(kck) = kck {
        let mic = crypto::eapol_mic(kck, &f);
        f[81..97].copy_from_slice(&mic);
    }
    f
}

/// Key data padding: 0xDD then zeros, to a multiple of 8 bytes.
fn pad(mut d: Vec<u8>) -> Vec<u8> {
    if !d.len().is_multiple_of(8) {
        d.push(0xDD);
        while !d.len().is_multiple_of(8) {
            d.push(0);
        }
    }
    d
}

fn mic_ok(kck: &[u8; 16], eapol: &[u8]) -> bool {
    if eapol.len() < 97 {
        return false;
    }
    let mut zeroed = eapol.to_vec();
    zeroed[81..97].fill(0);
    crypto::same(&crypto::eapol_mic(kck, &zeroed), &eapol[81..97])
}

impl AccessPoint {
    /// A network; `passphrase` makes it WPA2-Personal.
    pub fn new(bssid: Mac, ssid: &[u8], channel: u8, passphrase: Option<&str>, anonce: [u8; 32]) -> AccessPoint {
        AccessPoint {
            bssid,
            ssid: ssid.to_vec(),
            channel,
            pmk: passphrase.map(|p| crypto::pmk(p.as_bytes(), ssid)),
            anonce,
            replay: 0,
            ptk: None,
            pending: None,
            gtk: [0x3C; 16],
            gtk_id: 1,
            pn: 0,
            gtk_pn: 100,
            seq: 0,
            connected: None,
            station: None,
            received: Vec::new(),
        }
    }

    fn next_seq(&mut self) -> u16 {
        self.seq = (self.seq + 1) & 0xFFF;
        self.seq
    }

    pub fn beacon(&self) -> Vec<u8> {
        let mut f = header([0x80, 0], frame::BROADCAST, self.bssid, self.bssid, 0);
        f.extend_from_slice(&[0; 8]);
        f.extend_from_slice(&100u16.to_le_bytes());
        f.extend_from_slice(&(if self.pmk.is_some() { 0x0411u16 } else { 0x0401 }).to_le_bytes());
        f.extend_from_slice(&[0, self.ssid.len() as u8]);
        f.extend_from_slice(&self.ssid);
        f.extend_from_slice(&[3, 1, self.channel]);
        if self.pmk.is_some() {
            f.extend_from_slice(&wpa2_ie());
        }
        f
    }

    /// An unprotected data frame to `dst`.
    pub fn data_to(&mut self, dst: Mac, ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let seq = self.next_seq();
        let mut f = header([0x08, FLAG_FROM_DS], dst, self.bssid, self.bssid, seq);
        f.extend_from_slice(&[0xAA, 0xAA, 0x03, 0, 0, 0]);
        f.extend_from_slice(&ethertype.to_be_bytes());
        f.extend_from_slice(payload);
        f
    }

    /// A data frame to the station, encrypted once keys are in place.
    pub fn send_to_station(&mut self, ethertype: u16, payload: &[u8]) -> Option<Vec<u8>> {
        let sta = self.station?;
        let f = self.data_to(sta, ethertype, payload);
        match &self.ptk {
            Some(p) => {
                self.pn += 1;
                crypto::ccmp_encrypt(&p.tk, &f, self.pn, 0)
            }
            None => Some(f),
        }
    }

    /// A broadcast frame, encrypted with the group key on protected networks.
    pub fn broadcast(&mut self, ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let f = self.data_to(frame::BROADCAST, ethertype, payload);
        if self.pmk.is_none() {
            return f;
        }
        self.gtk_pn += 1;
        crypto::ccmp_encrypt(&self.gtk, &f, self.gtk_pn, self.gtk_id).unwrap_or(f)
    }

    /// A new group key, sent to the station (group key handshake message 1).
    pub fn rekey(&mut self) -> Option<Vec<u8>> {
        let ptk = self.ptk.clone()?;
        self.gtk = [0x5A; 16];
        self.gtk_id = if self.gtk_id == 1 { 2 } else { 1 };
        self.gtk_pn = 0;
        let mut kd = vec![0xDD, 22, 0x00, 0x0F, 0xAC, 0x01, self.gtk_id, 0];
        kd.extend_from_slice(&self.gtk);
        let wrapped = crypto::wrap_key(&ptk.kek, &pad(kd))?;
        self.replay += 1;
        let k = key_frame(
            KI_V2 | KI_ACK | KI_MIC | KI_SECURE | KI_ENCRYPTED,
            self.replay,
            &[0; 32],
            0,
            &wrapped,
            Some(&ptk.kck),
        );
        self.send_to_station(ETHERTYPE_EAPOL, &k)
    }

    /// Handles a frame from a station; returns the frames the AP sends back.
    pub fn receive(&mut self, f: &[u8]) -> Vec<Vec<u8>> {
        let Some(h) = Header::parse(f) else { return Vec::new() };
        if h.addr1 != self.bssid && h.addr1 != frame::BROADCAST {
            return Vec::new();
        }
        match (h.kind, h.subtype) {
            (TYPE_MGMT, frame::SUBTYPE_PROBE_REQ) => {
                let mut resp = self.beacon();
                resp[0] = frame::SUBTYPE_PROBE_RESP << 4;
                resp[4..10].copy_from_slice(&h.addr2);
                vec![resp]
            }
            (TYPE_MGMT, frame::SUBTYPE_AUTH) => {
                let mut r = header([0xB0, 0], h.addr2, self.bssid, self.bssid, 1);
                r.extend_from_slice(&[0, 0, 2, 0, 0, 0]);
                vec![r]
            }
            (TYPE_MGMT, frame::SUBTYPE_ASSOC_REQ) => {
                let mut r = header([0x10, 0], h.addr2, self.bssid, self.bssid, 2);
                r.extend_from_slice(&[0x11, 0x04, 0, 0, 0x01, 0xC0]);
                self.station = Some(h.addr2);
                self.ptk = None;
                if self.pmk.is_none() {
                    self.connected = Some(h.addr2);
                    return vec![r];
                }
                // Then message 1 of the 4-way handshake.
                self.replay += 1;
                let k = key_frame(KI_V2 | KI_PAIRWISE | KI_ACK, self.replay, &self.anonce, 0, &[], None);
                let m1 = self.data_to(h.addr2, ETHERTYPE_EAPOL, &k);
                vec![r, m1]
            }
            (TYPE_MGMT, frame::SUBTYPE_DEAUTH) => {
                self.connected = None;
                self.station = None;
                Vec::new()
            }
            (TYPE_DATA, _) if Some(h.addr2) == self.station => self.data(f),
            _ => Vec::new(),
        }
    }

    fn data(&mut self, f: &[u8]) -> Vec<Vec<u8>> {
        let plain = match &self.ptk {
            Some(p) if f[1] & frame::FLAG_PROTECTED != 0 => match crypto::ccmp_decrypt(&p.tk, f) {
                Some((plain, _, _)) => plain,
                None => return Vec::new(),
            },
            _ => f.to_vec(),
        };
        let Some(eth) = frame::ethernet_from_data(&plain) else { return Vec::new() };
        if u16::from_be_bytes([eth[12], eth[13]]) != ETHERTYPE_EAPOL {
            if self.connected.is_some() {
                self.received.push(eth);
            }
            return Vec::new();
        }
        let eapol = &eth[14..];
        let Some(pmk) = self.pmk else { return Vec::new() };
        let Some(sta) = self.station else { return Vec::new() };
        if eapol.len() < 97 {
            return Vec::new();
        }
        let info = u16::from_be_bytes([eapol[5], eapol[6]]);
        if info & KI_PAIRWISE != 0 && info & KI_SECURE == 0 {
            // Message 2: derive the PTK from the SNonce and check the MIC.
            let snonce: [u8; 32] = eapol[17..49].try_into().unwrap_or([0; 32]);
            let ptk = crypto::ptk(&pmk, &self.bssid, &sta, &self.anonce, &snonce);
            if !mic_ok(&ptk.kck, eapol) {
                return Vec::new(); // wrong password: stay silent, as real APs do
            }
            let mut kd = wpa2_ie();
            kd.extend_from_slice(&[0xDD, 22, 0x00, 0x0F, 0xAC, 0x01, self.gtk_id, 0]);
            kd.extend_from_slice(&self.gtk);
            let Some(wrapped) = crypto::wrap_key(&ptk.kek, &pad(kd)) else { return Vec::new() };
            self.replay += 1;
            let info = KI_V2 | KI_PAIRWISE | KI_INSTALL | KI_ACK | KI_MIC | KI_SECURE | KI_ENCRYPTED;
            let k = key_frame(info, self.replay, &self.anonce, self.gtk_pn, &wrapped, Some(&ptk.kck));
            let m3 = self.data_to(sta, ETHERTYPE_EAPOL, &k);
            // Keys go in after message 4.
            self.pending = Some(ptk);
            return vec![m3];
        }
        if info & KI_PAIRWISE != 0 {
            // Message 4.
            if let Some(ptk) = self.pending.take() {
                if mic_ok(&ptk.kck, eapol) {
                    self.ptk = Some(ptk);
                    self.connected = Some(sta);
                }
            }
        }
        Vec::new()
    }
}
