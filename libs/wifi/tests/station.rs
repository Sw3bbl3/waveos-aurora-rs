//! The station against the simulated access point (`aurora_wifi::sim`):
//! scanning, joining, the handshake over the air, encrypted traffic both
//! ways, broadcast, replay protection, rekeying, wrong passwords and being
//! kicked off.

use aurora_wifi::crypto;
use aurora_wifi::frame::{self, Header, Mac, TYPE_DATA};
use aurora_wifi::rsn::Security;
use aurora_wifi::sim::AccessPoint;
use aurora_wifi::station::{Output, State, Station};

const AP: Mac = [0x02, 0, 0, 0xA0, 0xB0, 0xC0];
const STA: Mac = [0x02, 0, 0x5E, 0x10, 0xAB, 0x01];
const SSID: &[u8] = b"Harbor Cafe";
const PASS: &str = "tide pools at dawn";

fn wpa2_ap() -> AccessPoint {
    AccessPoint::new(AP, SSID, 11, Some(PASS), [7; 32])
}

/// Passes frames between the station and the AP until both are quiet;
/// returns what the station handed to the network stack.
fn air(sta: &mut Station, ap: &mut AccessPoint, out: Vec<Output>, now: u64) -> Vec<Output> {
    let mut pending = out;
    let mut delivered = Vec::new();
    for _ in 0..20 {
        let mut next = Vec::new();
        for o in pending {
            match o {
                Output::Transmit(f) => {
                    for reply in ap.receive(&f) {
                        next.extend(sta.receive(&reply, now));
                    }
                }
                other => delivered.push(other),
            }
        }
        if next.is_empty() {
            break;
        }
        pending = next;
    }
    delivered
}

fn received(out: &[Output]) -> Vec<Vec<u8>> {
    out.iter().filter_map(|o| if let Output::Receive(f) = o { Some(f.clone()) } else { None }).collect()
}

fn transmitted(out: &[Output]) -> Vec<Vec<u8>> {
    out.iter().filter_map(|o| if let Output::Transmit(f) = o { Some(f.clone()) } else { None }).collect()
}

fn join(sta: &mut Station, ap: &mut AccessPoint, pass: &str) -> Vec<Output> {
    sta.receive(&ap.beacon(), 0);
    let bss = sta.networks(0).into_iter().next().expect("the network is listed");
    let out = sta.connect(bss, Some(pass), [9; 32], 0);
    air(sta, ap, out, 10)
}

fn ethernet(dst: Mac, payload: &[u8]) -> Vec<u8> {
    let mut eth = dst.to_vec();
    eth.extend_from_slice(&STA);
    eth.extend_from_slice(&[0x08, 0x00]);
    eth.extend_from_slice(payload);
    eth
}

#[test]
fn joins_a_wpa2_network_and_carries_traffic() {
    let mut ap = wpa2_ap();
    let mut sta = Station::new(STA);
    sta.receive(&ap.beacon(), 0);
    let nets = sta.networks(0);
    assert_eq!((nets.len(), nets[0].ssid.as_str(), nets[0].security), (1, "Harbor Cafe", Security::Wpa2Psk));

    let out = join(&mut sta, &mut ap, PASS);
    assert_eq!(*sta.state(), State::Connected);
    assert!(out.contains(&Output::Link(true)));
    assert_eq!(ap.connected, Some(STA));

    // Station → AP: encrypted, with rising packet numbers.
    let eth = ethernet([0x11; 6], b"ping");
    let tx1 = sta.send(&eth).unwrap();
    let tx2 = sta.send(&eth).unwrap();
    assert_ne!(tx1[1] & frame::FLAG_PROTECTED, 0);
    let tk = ap.ptk.as_ref().unwrap().tk;
    let (_, pn1, _) = crypto::ccmp_decrypt(&tk, &tx1).unwrap();
    let (_, pn2, _) = crypto::ccmp_decrypt(&tk, &tx2).unwrap();
    assert!(pn2 > pn1);
    ap.receive(&tx1);
    assert_eq!(ap.received, vec![eth.clone()]);

    // AP → station: decrypted and delivered as Ethernet.
    let rx = ap.send_to_station(0x0800, b"pong").unwrap();
    let got = received(&sta.receive(&rx, 50));
    assert_eq!(got.len(), 1);
    assert_eq!((&got[0][0..6], &got[0][14..]), (&STA[..], &b"pong"[..]));
    // The same frame again is a replay: dropped.
    assert!(received(&sta.receive(&rx, 51)).is_empty());
    // Unprotected data is dropped on a protected link.
    let plain = ap.data_to(STA, 0x0800, b"sneaky");
    assert!(received(&sta.receive(&plain, 52)).is_empty());

    // Broadcast with the group key, once.
    let b = ap.broadcast(0x0806, b"who-has");
    assert_eq!(received(&sta.receive(&b, 60)).len(), 1);
    assert!(received(&sta.receive(&b, 61)).is_empty());

    // Group rekeying, then broadcast with the new key.
    let rekey = ap.rekey().unwrap();
    let out = sta.receive(&rekey, 70);
    assert_eq!(transmitted(&out).len(), 1);
    let b = ap.broadcast(0x0806, b"new-key");
    assert_eq!(received(&sta.receive(&b, 80)).len(), 1);

    // Kicked off by the AP.
    let mut deauth = frame::deauth(AP, STA, 7, 9);
    deauth[4..10].copy_from_slice(&STA);
    deauth[10..16].copy_from_slice(&AP);
    let out = sta.receive(&deauth, 90);
    assert!(out.contains(&Output::Link(false)));
    assert!(matches!(sta.state(), State::Failed(r) if r.contains("reason 7")));
    assert!(sta.send(&eth).is_none());
}

#[test]
fn wrong_password_fails_the_handshake() {
    let mut ap = wpa2_ap();
    let mut sta = Station::new(STA);
    join(&mut sta, &mut ap, "not the password");
    // The AP can't verify message 2 and stays silent: the station times out.
    assert_eq!(*sta.state(), State::Handshake);
    assert_eq!(ap.connected, None);
    let out = sta.tick(6000);
    assert!(matches!(sta.state(), State::Failed(r) if r.contains("password")));
    assert!(transmitted(&out).iter().any(|f| Header::parse(f).unwrap().subtype == frame::SUBTYPE_DEAUTH));
}

#[test]
fn joins_an_open_network() {
    let mut ap = AccessPoint::new(AP, b"Lighthouse Guest", 1, None, [7; 32]);
    let mut sta = Station::new(STA);
    sta.receive(&ap.beacon(), 0);
    let bss = sta.networks(0).remove(0);
    assert_eq!(bss.security, Security::Open);
    let out = sta.connect(bss, None, [9; 32], 0);
    let out = air(&mut sta, &mut ap, out, 10);
    assert_eq!(*sta.state(), State::Connected);
    assert!(out.contains(&Output::Link(true)));
    let rx = ap.send_to_station(0x0800, b"hello").unwrap();
    assert_eq!(received(&sta.receive(&rx, 50)).len(), 1);
    let tx = sta.send(&ethernet([0x11; 6], &[1, 2])).unwrap();
    let h = Header::parse(&tx).unwrap();
    assert_eq!((h.kind, h.flags & frame::FLAG_PROTECTED), (TYPE_DATA, 0));
    // Leaving sends a deauthentication and drops the link.
    let out = sta.disconnect();
    assert!(out.contains(&Output::Link(false)));
    assert_eq!(*sta.state(), State::Idle);
    air(&mut sta, &mut ap, out, 60);
    assert_eq!(ap.connected, None);
}

#[test]
fn probe_requests_are_answered() {
    let mut ap = wpa2_ap();
    let mut sta = Station::new(STA);
    let replies = ap.receive(&sta.probe());
    assert_eq!(replies.len(), 1);
    sta.receive(&replies[0], 0);
    assert_eq!(sta.networks(0)[0].ssid, "Harbor Cafe");
}

#[test]
fn retries_then_gives_up() {
    let ap = wpa2_ap();
    let mut sta = Station::new(STA);
    sta.receive(&ap.beacon(), 0);
    let bss = sta.networks(0).remove(0);
    sta.connect(bss, Some(PASS), [9; 32], 0);
    assert_eq!(transmitted(&sta.tick(600)).len(), 1); // second try
    assert_eq!(transmitted(&sta.tick(1200)).len(), 1); // third try
    sta.tick(1800);
    assert!(matches!(sta.state(), State::Failed(r) if r.contains("did not answer")));
}

#[test]
fn refuses_what_it_cannot_do() {
    let ap = wpa2_ap();
    let mut sta = Station::new(STA);
    sta.receive(&ap.beacon(), 0);
    let bss = sta.networks(0).remove(0);
    sta.connect(bss.clone(), Some("short"), [9; 32], 0);
    assert!(matches!(sta.state(), State::Failed(r) if r.contains("8 to 63")));
    let mut wep = bss;
    wep.security = Security::Wep;
    sta.connect(wep, Some(PASS), [9; 32], 0);
    assert!(matches!(sta.state(), State::Failed(r) if r.contains("WEP")));
    // Old networks drop out of the list.
    assert!(sta.networks(120_000).is_empty());
}
