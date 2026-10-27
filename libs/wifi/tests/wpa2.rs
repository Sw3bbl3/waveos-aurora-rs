//! WPA2-Personal and CCMP against the IEEE 802.11 test vectors and data from
//! an independent implementation (`make_vectors.py`).

#[allow(dead_code)]
#[path = "data/vectors.rs"]
mod v;

use aurora_wifi::crypto::{self, Ptk};
use aurora_wifi::frame::{self, Header, Mac};
use aurora_wifi::rsn::{self, Rsn, Security};
use aurora_wifi::supplicant::{Event, Supplicant};

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

fn mac(b: &[u8]) -> Mac {
    b.try_into().unwrap()
}

fn arr32(b: &[u8]) -> [u8; 32] {
    b.try_into().unwrap()
}

/// IEEE 802.11-2020 J.4: passphrase-to-PSK mapping.
#[test]
fn psk_matches_the_standard() {
    assert_eq!(
        crypto::pmk(b"password", b"IEEE").to_vec(),
        hex("f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e")
    );
    assert_eq!(
        crypto::pmk(b"ThisIsAPassword", b"ThisIsASSID").to_vec(),
        hex("0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af")
    );
    assert_eq!(crypto::pmk(v::PASSPHRASE, v::SSID).to_vec(), v::PMK);
}

/// IEEE 802.11 PRF test case 1 (PRF-512).
#[test]
fn prf_matches_the_standard() {
    let mut out = [0u8; 64];
    crypto::prf(&[0x0B; 20], b"prefix", b"Hi There", &mut out);
    assert_eq!(
        out.to_vec(),
        hex("bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606e17d8da35402ffee75df78c3d31e0f889f012120c0862beb67753e7439ae242edb8373698356cf5a")
    );
}

#[test]
fn ptk_derivation() {
    let p = crypto::ptk(&arr32(v::PMK), &mac(v::AA), &mac(v::SPA), &arr32(v::ANONCE), &arr32(v::SNONCE));
    assert_eq!((p.kck.to_vec(), p.kek.to_vec(), p.tk.to_vec()), (v::KCK.to_vec(), v::KEK.to_vec(), v::TK.to_vec()));
    // The order of addresses and nonces doesn't matter (the AP computes the same).
    let q = crypto::ptk(&arr32(v::PMK), &mac(v::SPA), &mac(v::AA), &arr32(v::SNONCE), &arr32(v::ANONCE));
    assert_eq!(p, q);
}

#[test]
fn passphrase_rules() {
    assert!(crypto::passphrase_valid("12345678"));
    assert!(!crypto::passphrase_valid("1234567"));
    assert!(!crypto::passphrase_valid(&"x".repeat(64)));
    assert!(!crypto::passphrase_valid("tab\there!"));
}

fn supplicant() -> Supplicant {
    Supplicant::new(arr32(v::PMK), mac(v::SPA), mac(v::AA), rsn::station_ie(), v::AP_IE.to_vec(), arr32(v::SNONCE))
}

#[test]
fn four_way_handshake() {
    let mut s = supplicant();
    assert_eq!(s.receive(v::MSG1), Event::Send(v::MSG2.to_vec()));
    match s.receive(v::MSG3) {
        Event::Connected { send, ptk, gtk } => {
            assert_eq!(send, v::MSG4);
            assert_eq!(ptk.tk.to_vec(), v::TK);
            assert_eq!((gtk.key_id, gtk.key.as_slice(), gtk.rsc), (1, v::GTK, 5));
        }
        e => panic!("expected Connected, got {e:?}"),
    }
    // A replayed message 3 is ignored.
    assert_eq!(s.receive(v::MSG3), Event::Ignored);
    // Group rekeying.
    match s.receive(v::GROUP1) {
        Event::GroupRekey { send, gtk } => {
            assert_eq!(send, v::GROUP2);
            assert_eq!((gtk.key_id, gtk.key.as_slice(), gtk.rsc), (2, v::GTK2, 9));
        }
        e => panic!("expected GroupRekey, got {e:?}"),
    }
}

#[test]
fn wrong_password_is_reported() {
    let mut s = Supplicant::new(
        crypto::pmk(b"not the password", v::SSID),
        mac(v::SPA),
        mac(v::AA),
        rsn::station_ie(),
        v::AP_IE.to_vec(),
        arr32(v::SNONCE),
    );
    assert!(matches!(s.receive(v::MSG1), Event::Send(_)));
    assert!(matches!(s.receive(v::MSG3), Event::Failed(_)));
}

#[test]
fn forged_message3_is_rejected() {
    let mut s = supplicant();
    s.receive(v::MSG1);
    assert!(matches!(s.receive(v::MSG3_FORGED), Event::Failed(_)));
}

#[test]
fn downgraded_rsn_element_is_rejected() {
    // The AP's beacon advertised something else than message 3 carries.
    let mut other = v::AP_IE.to_vec();
    *other.last_mut().unwrap() = 0x0C; // different RSN capabilities
    let mut s = Supplicant::new(arr32(v::PMK), mac(v::SPA), mac(v::AA), rsn::station_ie(), other, arr32(v::SNONCE));
    s.receive(v::MSG1);
    assert!(matches!(s.receive(v::MSG3), Event::Failed(_)));
}

#[test]
fn garbage_is_ignored() {
    let mut s = supplicant();
    assert_eq!(s.receive(&[]), Event::Ignored);
    assert_eq!(s.receive(&v::MSG1[..50]), Event::Ignored);
    // Message 3 before message 1.
    assert_eq!(s.receive(v::MSG3), Event::Ignored);
    // Truncated frames of every length never panic.
    for n in 0..v::MSG3.len() {
        let _ = supplicant().receive(&v::MSG3[..n]);
    }
}

#[test]
fn key_wrap_round_trip() {
    let kek: [u8; 16] = v::KEK.try_into().unwrap();
    let data: Vec<u8> = (0..32).collect();
    let wrapped = crypto::wrap_key(&kek, &data).unwrap();
    assert_eq!(crypto::unwrap_key(&kek, &wrapped).unwrap(), data);
    let mut bad = wrapped.clone();
    bad[3] ^= 1;
    assert!(crypto::unwrap_key(&kek, &bad).is_none());
    // RFC 3394 4.1: 128-bit data with a 128-bit KEK.
    let kek: [u8; 16] = hex("000102030405060708090A0B0C0D0E0F").try_into().unwrap();
    assert_eq!(
        crypto::wrap_key(&kek, &hex("00112233445566778899AABBCCDDEEFF")).unwrap(),
        hex("1FA68B0A8112B447AEF34BD8FB5A7B829D3E862371D2CFE5")
    );
}

#[test]
fn ccmp_matches_reference() {
    let tk: [u8; 16] = v::TK.try_into().unwrap();
    let enc = crypto::ccmp_encrypt(&tk, v::CCMP_PLAIN_FRAME, v::CCMP_PN, 0).unwrap();
    assert_eq!(enc, v::CCMP_PROTECTED_FRAME);
    let (dec, pn, key_id) = crypto::ccmp_decrypt(&tk, v::CCMP_PROTECTED_FRAME).unwrap();
    assert_eq!((dec.as_slice(), pn, key_id), (v::CCMP_PLAIN_FRAME, v::CCMP_PN, 0));
    // Any flipped bit fails the MIC.
    for i in [1, 4, 30, 40, v::CCMP_PROTECTED_FRAME.len() - 1] {
        let mut bad = v::CCMP_PROTECTED_FRAME.to_vec();
        bad[i] ^= 0x01;
        assert!(crypto::ccmp_decrypt(&tk, &bad).is_none(), "bit flip at {i} accepted");
    }
    // The mutable header bits (retry) are not authenticated.
    let mut retry = v::CCMP_PROTECTED_FRAME.to_vec();
    retry[1] |= frame::FLAG_RETRY;
    assert!(crypto::ccmp_decrypt(&tk, &retry).is_some());
}

#[test]
fn ptk_debug_hides_keys() {
    let p = Ptk { kck: [1; 16], kek: [2; 16], tk: [3; 16] };
    assert_eq!(format!("{p:?}"), "Ptk(..)");
}

fn beacon(ssid: &[u8], extra: &[u8], caps: u16) -> Vec<u8> {
    let mut f = vec![0x80, 0, 0, 0];
    f.extend_from_slice(&[0xFF; 6]);
    f.extend_from_slice(v::AA);
    f.extend_from_slice(v::AA);
    f.extend_from_slice(&[0x10, 0]);
    f.extend_from_slice(&[0; 8]); // timestamp
    f.extend_from_slice(&100u16.to_le_bytes());
    f.extend_from_slice(&caps.to_le_bytes());
    f.extend_from_slice(&[0, ssid.len() as u8]);
    f.extend_from_slice(ssid);
    f.extend_from_slice(&[3, 1, 6]); // channel 6
    f.extend_from_slice(extra);
    f
}

#[test]
fn beacons() {
    let b = frame::parse_bss(&beacon(b"Cafe", v::AP_IE, 0x0411)).unwrap();
    assert_eq!((b.ssid.as_str(), b.channel, b.security, b.bssid), ("Cafe", 6, Security::Wpa2Psk, mac(v::AA)));
    assert_eq!(b.rsn_ie.as_deref(), Some(v::AP_IE));
    assert_eq!(frame::parse_bss(&beacon(b"Open", &[], 0x0401)).unwrap().security, Security::Open);
    assert_eq!(frame::parse_bss(&beacon(b"Old", &[], 0x0411)).unwrap().security, Security::Wep);
    let hidden = frame::parse_bss(&beacon(&[0; 4], &[], 0x0401)).unwrap();
    assert!(hidden.ssid.is_empty() && hidden.ssid_raw.is_empty());
    // WPA2/WPA3 transition: PSK and SAE.
    let mut mixed = v::AP_IE.to_vec();
    mixed[1] = 24;
    mixed[14] = 2; // two AKMs
    mixed.splice(20..20, [0x00, 0x0F, 0xAC, 0x08]);
    assert_eq!(Rsn::parse(&mixed[2..]).unwrap().security(), Security::Wpa2Wpa3);
    // Truncations never panic.
    let full = beacon(b"Cafe", v::AP_IE, 0x0411);
    for n in 0..full.len() {
        let _ = frame::parse_bss(&full[..n]);
    }
}

#[test]
fn ethernet_round_trip() {
    let mut eth = Vec::new();
    eth.extend_from_slice(&[0x11; 6]); // destination
    eth.extend_from_slice(v::SPA); // source
    eth.extend_from_slice(&[0x08, 0x00]);
    eth.extend_from_slice(b"payload");
    let data = frame::data_from_ethernet(&eth, mac(v::AA), 7).unwrap();
    let h = Header::parse(&data).unwrap();
    assert_eq!((h.addr1, h.addr2, h.addr3), (mac(v::AA), mac(v::SPA), [0x11; 6]));
    assert_eq!(frame::ethernet_from_data(&data).unwrap(), eth);
    // From the AP (FromDS): A1 = destination, A3 = source.
    let mut from_ap = data.clone();
    from_ap[1] = frame::FLAG_FROM_DS;
    from_ap[4..10].copy_from_slice(v::SPA);
    from_ap[10..16].copy_from_slice(v::AA);
    from_ap[16..22].copy_from_slice(&[0x22; 6]);
    let back = frame::ethernet_from_data(&from_ap).unwrap();
    assert_eq!((&back[..6], &back[6..12]), (v::SPA, &[0x22u8; 6][..]));
}

#[test]
fn management_frames() {
    let sta = mac(v::SPA);
    let probe = frame::probe_request(sta, b"", 1);
    let h = Header::parse(&probe).unwrap();
    assert_eq!((h.kind, h.subtype, h.addr1), (frame::TYPE_MGMT, frame::SUBTYPE_PROBE_REQ, frame::BROADCAST));
    let bss = frame::parse_bss(&beacon(b"Cafe", v::AP_IE, 0x0411)).unwrap();
    let assoc = frame::assoc_request(sta, &bss, Some(&rsn::station_ie()), 2);
    assert!(assoc.windows(rsn::station_ie().len()).any(|w| w == rsn::station_ie()));
    // An authentication response (seq 2, success) and an association response.
    let mut auth = frame::auth_request(bss.bssid, sta, 3);
    auth[24 + 2] = 2;
    assert_eq!(frame::auth_response_status(&auth, bss.bssid), Some(0));
    // From the AP (sender = BSSID): capabilities, status 0, AID 1 with the top bits set.
    let mut resp = frame::deauth(bss.bssid, sta, 0, 4);
    resp[0] = frame::SUBTYPE_ASSOC_RESP << 4;
    resp.truncate(24);
    resp.extend_from_slice(&[0x11, 0x04, 0, 0, 0x01, 0xC0]);
    assert_eq!(frame::assoc_response(&resp, bss.bssid), Some((0, 1)));
    assert_eq!(frame::assoc_response(&resp, sta), None);
    let d = frame::deauth(bss.bssid, sta, 15, 5);
    assert_eq!(frame::disconnect_reason(&d, bss.bssid), Some(15));
}
