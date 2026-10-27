//! The RSN element: which ciphers and key management a network offers.

use alloc::vec::Vec;

const OUI: [u8; 3] = [0x00, 0x0F, 0xAC];
pub const CIPHER_TKIP: u8 = 2;
pub const CIPHER_CCMP: u8 = 4;
pub const CIPHER_GCMP: u8 = 8;
pub const AKM_8021X: u8 = 1;
pub const AKM_PSK: u8 = 2;
pub const AKM_PSK_SHA256: u8 = 6;
pub const AKM_SAE: u8 = 8;

/// How a network is protected, as far as joining it is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    Open,
    /// Obsolete and broken: not offered.
    Wep,
    /// WPA version 1 (TKIP): obsolete, not offered.
    Wpa1,
    /// WPA2-Personal with CCMP: supported.
    Wpa2Psk,
    /// WPA3-Personal (SAE) only.
    Wpa3Sae,
    /// WPA2/WPA3 transition mode: joinable with WPA2-PSK.
    Wpa2Wpa3,
    /// 802.1X (enterprise).
    Enterprise,
    /// Something else (e.g. TKIP-only pairwise cipher).
    Unsupported,
}

impl Security {
    /// Whether WaveOS can join a network with this security.
    pub fn joinable(self) -> bool {
        matches!(self, Security::Open | Security::Wpa2Psk | Security::Wpa2Wpa3)
    }

    pub fn label(self) -> &'static str {
        match self {
            Security::Open => "Open",
            Security::Wep => "WEP",
            Security::Wpa1 => "WPA",
            Security::Wpa2Psk => "WPA2 Personal",
            Security::Wpa3Sae => "WPA3 Personal",
            Security::Wpa2Wpa3 => "WPA2/WPA3 Personal",
            Security::Enterprise => "WPA2 Enterprise",
            Security::Unsupported => "Unsupported",
        }
    }
}

/// A parsed RSN element body (without the id and length bytes).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rsn {
    pub group: u8,
    pub pairwise: Vec<u8>,
    pub akm: Vec<u8>,
    pub capabilities: u16,
}

/// Reads a list of 00-0F-AC suites: (count, then 4 bytes each). Suites from
/// other vendors are kept as 0 (never matches).
fn suites(b: &[u8], at: &mut usize) -> Option<Vec<u8>> {
    let n = u16::from_le_bytes([*b.get(*at)?, *b.get(*at + 1)?]) as usize;
    *at += 2;
    let list = b.get(*at..*at + 4 * n)?;
    *at += 4 * n;
    Some(list.chunks(4).map(|s| if s[..3] == OUI { s[3] } else { 0 }).collect())
}

impl Rsn {
    /// Parses the body; fields missing at the end take their defaults (CCMP, 802.1X).
    pub fn parse(b: &[u8]) -> Option<Rsn> {
        if b.len() < 2 || u16::from_le_bytes([b[0], b[1]]) != 1 {
            return None;
        }
        let mut rsn = Rsn { group: CIPHER_CCMP, pairwise: alloc::vec![CIPHER_CCMP], akm: alloc::vec![AKM_8021X], capabilities: 0 };
        if b.len() < 6 {
            return Some(rsn);
        }
        rsn.group = if b[2..5] == OUI { b[5] } else { 0 };
        let mut at = 6;
        if at < b.len() {
            rsn.pairwise = suites(b, &mut at)?;
        }
        if at < b.len() {
            rsn.akm = suites(b, &mut at)?;
        }
        if at + 2 <= b.len() {
            rsn.capabilities = u16::from_le_bytes([b[at], b[at + 1]]);
        }
        Some(rsn)
    }

    pub fn security(&self) -> Security {
        let psk = self.akm.contains(&AKM_PSK) || self.akm.contains(&AKM_PSK_SHA256);
        let sae = self.akm.contains(&AKM_SAE);
        // CCMP for both unicast and broadcast (TKIP is not implemented).
        if (psk || sae) && (!self.pairwise.contains(&CIPHER_CCMP) || self.group != CIPHER_CCMP) {
            return Security::Unsupported;
        }
        match (psk, sae) {
            (true, true) => Security::Wpa2Wpa3,
            (true, false) if self.akm.contains(&AKM_PSK) => Security::Wpa2Psk,
            (true, false) => Security::Unsupported, // PSK-SHA256 only
            (false, true) => Security::Wpa3Sae,
            _ if self.akm.contains(&AKM_8021X) => Security::Enterprise,
            _ => Security::Unsupported,
        }
    }
}

/// Our RSN element: WPA2-PSK with CCMP.
pub fn station_ie() -> Vec<u8> {
    let mut ie = alloc::vec![48, 20, 1, 0];
    ie.extend_from_slice(&OUI);
    ie.push(CIPHER_CCMP);
    ie.extend_from_slice(&[1, 0]);
    ie.extend_from_slice(&OUI);
    ie.push(CIPHER_CCMP);
    ie.extend_from_slice(&[1, 0]);
    ie.extend_from_slice(&OUI);
    ie.push(AKM_PSK);
    ie.extend_from_slice(&[0, 0]); // capabilities
    ie
}
