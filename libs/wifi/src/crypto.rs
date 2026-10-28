//! WPA2-Personal key derivation (IEEE 802.11-2020 §12.7) and CCMP
//! (§12.5.3): passphrase → PMK, PMK + nonces → PTK, EAPOL-Key MICs, AES key
//! unwrapping, and frame encryption.

use crate::frame::{Header, Mac, FLAG_MORE_DATA, FLAG_PROTECTED, FLAG_PWR_MGMT, FLAG_RETRY, TYPE_DATA};
use aes::Aes128;
use alloc::vec::Vec;
use ccm::aead::AeadInPlace;
use ccm::consts::{U13, U8};
use hmac::{Hmac, Mac as _};
use sha1::Sha1;

type HmacSha1 = Hmac<Sha1>;

/// The pairwise master key of a WPA2-Personal network: PBKDF2-HMAC-SHA1 of
/// the passphrase, salted with the SSID, 4096 rounds.
pub fn pmk(passphrase: &[u8], ssid: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha1>(passphrase, ssid, 4096, &mut out);
    out
}

/// A passphrase must be 8–63 printable ASCII characters (64 hex digits is a raw PMK).
pub fn passphrase_valid(p: &str) -> bool {
    (8..=63).contains(&p.len()) && p.bytes().all(|c| (0x20..0x7F).contains(&c))
}

/// PRF-n (§12.7.1.2): HMAC-SHA1(key, label || 0 || data || counter), repeated.
pub fn prf(key: &[u8], label: &[u8], data: &[u8], out: &mut [u8]) {
    for (i, chunk) in out.chunks_mut(20).enumerate() {
        let mut mac = HmacSha1::new_from_slice(key).expect("HMAC takes any key length");
        mac.update(label);
        mac.update(&[0]);
        mac.update(data);
        mac.update(&[i as u8]);
        let digest = mac.finalize().into_bytes();
        chunk.copy_from_slice(&digest[..chunk.len()]);
    }
}

/// The pairwise transient key for CCMP: confirmation key (MICs), encryption
/// key (wrapped group keys) and temporal key (frames).
#[derive(Clone, PartialEq, Eq)]
pub struct Ptk {
    pub kck: [u8; 16],
    pub kek: [u8; 16],
    pub tk: [u8; 16],
}

impl core::fmt::Debug for Ptk {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str("Ptk(..)") // keys stay out of logs
    }
}

/// PTK = PRF-384(PMK, "Pairwise key expansion", min(AA,SPA) || max(AA,SPA) ||
/// min(ANonce,SNonce) || max(ANonce,SNonce)).
pub fn ptk(pmk: &[u8; 32], aa: &Mac, spa: &Mac, anonce: &[u8; 32], snonce: &[u8; 32]) -> Ptk {
    let mut data = [0u8; 76];
    let (a1, a2) = if aa < spa { (aa, spa) } else { (spa, aa) };
    let (n1, n2) = if anonce < snonce { (anonce, snonce) } else { (snonce, anonce) };
    data[..6].copy_from_slice(a1);
    data[6..12].copy_from_slice(a2);
    data[12..44].copy_from_slice(n1);
    data[44..76].copy_from_slice(n2);
    let mut key = [0u8; 48];
    prf(pmk, b"Pairwise key expansion", &data, &mut key);
    let mut p = Ptk { kck: [0; 16], kek: [0; 16], tk: [0; 16] };
    p.kck.copy_from_slice(&key[..16]);
    p.kek.copy_from_slice(&key[16..32]);
    p.tk.copy_from_slice(&key[32..48]);
    p
}

/// The EAPOL-Key MIC for key descriptor version 2: HMAC-SHA1, truncated to
/// 16 bytes, over the frame with its MIC field zeroed.
pub fn eapol_mic(kck: &[u8; 16], frame: &[u8]) -> [u8; 16] {
    let mut mac = HmacSha1::new_from_slice(kck).expect("HMAC takes any key length");
    mac.update(frame);
    let digest = mac.finalize().into_bytes();
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Constant-time comparison (MICs).
pub fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// AES key unwrap (RFC 3394) of EAPOL-Key key data with the KEK.
pub fn unwrap_key(kek: &[u8; 16], wrapped: &[u8]) -> Option<Vec<u8>> {
    if wrapped.len() < 24 || !wrapped.len().is_multiple_of(8) {
        return None;
    }
    let kw = aes_kw::KekAes128::from(*kek);
    let mut out = alloc::vec![0u8; wrapped.len() - 8];
    kw.unwrap(wrapped, &mut out).ok()?;
    Some(out)
}

/// AES key wrap (RFC 3394), for tests and an access point's side.
pub fn wrap_key(kek: &[u8; 16], data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 16 || !data.len().is_multiple_of(8) {
        return None;
    }
    let kw = aes_kw::KekAes128::from(*kek);
    let mut out = alloc::vec![0u8; data.len() + 8];
    kw.wrap(data, &mut out).ok()?;
    Some(out)
}

// ------------------------------------------------------------------ CCMP

type Ccm = ccm::Ccm<Aes128, U8, U13>;

/// CCMP header length, and the MIC appended to the payload.
pub const CCMP_HEADER: usize = 8;
pub const CCMP_MIC: usize = 8;

/// The 48-bit packet number from a CCMP header.
pub fn ccmp_pn(h: &[u8]) -> u64 {
    u64::from(h[0])
        | u64::from(h[1]) << 8
        | u64::from(h[4]) << 16
        | u64::from(h[5]) << 24
        | u64::from(h[6]) << 32
        | u64::from(h[7]) << 40
}

fn ccmp_header(pn: u64, key_id: u8) -> [u8; 8] {
    [
        pn as u8,
        (pn >> 8) as u8,
        0,
        0x20 | (key_id & 3) << 6,
        (pn >> 16) as u8,
        (pn >> 24) as u8,
        (pn >> 32) as u8,
        (pn >> 40) as u8,
    ]
}

/// Nonce: priority || A2 || PN (most significant byte first).
fn nonce(h: &Header, pn: u64) -> [u8; 13] {
    let mut n = [0u8; 13];
    n[0] = h.tid();
    n[1..7].copy_from_slice(&h.addr2);
    for i in 0..6 {
        n[7 + i] = (pn >> (40 - 8 * i)) as u8;
    }
    n
}

/// Additional authenticated data: the header with the mutable bits masked.
fn aad(frame: &[u8], h: &Header) -> Vec<u8> {
    let mut a = Vec::with_capacity(30);
    let mut fc0 = frame[0];
    if h.kind == TYPE_DATA {
        fc0 &= !0x70; // subtype bits 4-6
    }
    a.push(fc0);
    a.push((frame[1] & !(FLAG_RETRY | FLAG_PWR_MGMT | FLAG_MORE_DATA)) | FLAG_PROTECTED);
    a.extend_from_slice(&frame[4..22]); // A1, A2, A3
    a.extend_from_slice(&(h.seq_ctrl & 0xF).to_le_bytes()); // fragment number only
    if let Some(a4) = h.addr4 {
        a.extend_from_slice(&a4);
    }
    if h.qos.is_some() {
        a.extend_from_slice(&[h.tid(), 0]);
    }
    a
}

/// Encrypts an unprotected frame: inserts the CCMP header after the MAC
/// header, encrypts the body and appends the MIC; sets the Protected flag.
pub fn ccmp_encrypt(tk: &[u8; 16], frame: &[u8], pn: u64, key_id: u8) -> Option<Vec<u8>> {
    let mut f = frame.to_vec();
    f[1] |= FLAG_PROTECTED;
    let h = Header::parse(&f)?;
    let mut body = f.split_off(h.len);
    let tag = <Ccm as ccm::aead::KeyInit>::new(tk.into())
        .encrypt_in_place_detached(&nonce(&h, pn).into(), &aad(&f, &h), &mut body)
        .ok()?;
    f.extend_from_slice(&ccmp_header(pn, key_id));
    f.extend_from_slice(&body);
    f.extend_from_slice(&tag);
    Some(f)
}

/// Decrypts a protected frame: returns (the frame without CCMP header and
/// MIC and with Protected cleared, its packet number, key id), or `None` if
/// the MIC fails. Replay checking (PN must increase) is the caller's.
pub fn ccmp_decrypt(tk: &[u8; 16], frame: &[u8]) -> Option<(Vec<u8>, u64, u8)> {
    let h = Header::parse(frame)?;
    if frame[1] & FLAG_PROTECTED == 0 || frame.len() < h.len + CCMP_HEADER + CCMP_MIC {
        return None;
    }
    let ch = &frame[h.len..h.len + CCMP_HEADER];
    if ch[3] & 0x20 == 0 {
        return None; // ExtIV must be set
    }
    let pn = ccmp_pn(ch);
    let key_id = ch[3] >> 6;
    let mut body = frame[h.len + CCMP_HEADER..frame.len() - CCMP_MIC].to_vec();
    let tag = &frame[frame.len() - CCMP_MIC..];
    <Ccm as ccm::aead::KeyInit>::new(tk.into())
        .decrypt_in_place_detached(&nonce(&h, pn).into(), &aad(frame, &h), &mut body, tag.into())
        .ok()?;
    let mut out = frame[..h.len].to_vec();
    out[1] &= !FLAG_PROTECTED;
    out.extend_from_slice(&body);
    Some((out, pn, key_id))
}
