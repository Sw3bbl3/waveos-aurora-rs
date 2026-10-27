//! A software radio with simulated networks around it, for the kernel tests
//! and for trying Wi-Fi in a virtual machine (boot option `wifi_sim=1`,
//! `cargo xtask run --wifi-sim`). Behind the WPA2 network sits a small DHCP
//! server, so a connection goes all the way to an IP address.
//!
//!   "Harbor Cafe"       WPA2-Personal, password "tide pools at dawn"
//!   "Lighthouse Guest"  open

use super::Radio;
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_wifi::sim::AccessPoint;
use aurora_wifi::Bss;

pub const PASSWORD: &str = "tide pools at dawn";
/// The address the DHCP server hands out, and its own.
pub const CLIENT_IP: [u8; 4] = [192, 168, 77, 20];
const SERVER_IP: [u8; 4] = [192, 168, 77, 1];

struct SimRadio {
    mac: [u8; 6],
    aps: IrqMutex<Vec<AccessPoint>>,
}

pub fn start() {
    let mut anonce = [0u8; 32];
    crate::random::fill(&mut anonce);
    let aps = alloc::vec![
        AccessPoint::new([0x02, 0x57, 0x41, 0x56, 0x45, 0x01], b"Harbor Cafe", 6, Some(PASSWORD), anonce),
        AccessPoint::new([0x02, 0x57, 0x41, 0x56, 0x45, 0x02], b"Lighthouse Guest", 11, None, anonce),
    ];
    let radio = SimRadio { mac: [0x02, 0x57, 0x4F, 0x53, 0x00, 0x01], aps: IrqMutex::new(aps) };
    super::register(Arc::new(radio));
}

impl Radio for SimRadio {
    fn name(&self) -> String {
        String::from("Simulated Wi-Fi")
    }
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn unavailable(&self) -> Option<String> {
        None
    }
    fn scan(&self, probe: &[u8]) {
        let replies: Vec<Vec<u8>> = self.aps.lock().iter_mut().flat_map(|ap| ap.receive(probe)).collect();
        for r in replies {
            super::received(r);
        }
    }
    fn join(&self, _bss: &Bss) -> bool {
        true
    }
    fn leave(&self) {}
    fn transmit(&self, frame: &[u8]) -> bool {
        let mut replies = Vec::new();
        for ap in self.aps.lock().iter_mut() {
            replies.extend(ap.receive(frame));
            // Requests the station sent through the AP, answered by the "network".
            for eth in core::mem::take(&mut ap.received) {
                if let Some(ip) = dhcp_reply(&eth) {
                    replies.extend(ap.send_to_station(0x0800, &ip));
                }
            }
        }
        for r in replies {
            super::received(r);
        }
        true
    }
}

fn checksum(b: &[u8]) -> u16 {
    let mut sum: u32 = b.chunks(2).map(|c| u32::from(c[0]) << 8 | u32::from(*c.get(1).unwrap_or(&0))).sum();
    while sum > 0xFFFF {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Answers a DHCP discover with an offer and a request with an
/// acknowledgement; returns the IPv4 packet.
fn dhcp_reply(eth: &[u8]) -> Option<Vec<u8>> {
    let ip = eth.get(14..)?;
    if eth[12..14] != [0x08, 0x00] || ip.len() < 28 || ip[9] != 17 {
        return None;
    }
    let ihl = (ip[0] & 0xF) as usize * 4;
    let udp = ip.get(ihl..)?;
    if udp.len() < 8 + 240 || u16::from_be_bytes([udp[2], udp[3]]) != 67 {
        return None;
    }
    let req = &udp[8..];
    // Option 53: the message type.
    let mut kind = 0;
    let mut at = 240;
    while at + 1 < req.len() && req[at] != 255 {
        let (code, len) = (req[at], *req.get(at + 1)? as usize);
        if code == 53 && len == 1 {
            kind = *req.get(at + 2)?;
        }
        at += 2 + len;
    }
    let reply_kind = match kind {
        1 => 2, // discover → offer
        3 => 5, // request → ack
        _ => return None,
    };
    let mut m = alloc::vec![0u8; 240];
    m[0] = 2; // reply
    m[1] = 1;
    m[2] = 6;
    m[4..8].copy_from_slice(&req[4..8]); // transaction id
    m[10..12].copy_from_slice(&req[10..12]); // flags
    m[16..20].copy_from_slice(&CLIENT_IP);
    m[20..24].copy_from_slice(&SERVER_IP);
    m[28..44].copy_from_slice(&req[28..44]); // client hardware address
    m[236..240].copy_from_slice(&[99, 130, 83, 99]);
    m.extend_from_slice(&[53, 1, reply_kind]);
    m.extend_from_slice(&[54, 4]);
    m.extend_from_slice(&SERVER_IP);
    m.extend_from_slice(&[51, 4, 0, 0, 0x0E, 0x10]); // one hour
    m.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
    m.extend_from_slice(&[3, 4]);
    m.extend_from_slice(&SERVER_IP);
    m.extend_from_slice(&[6, 4]);
    m.extend_from_slice(&SERVER_IP);
    m.push(255);

    let udp_len = 8 + m.len();
    let mut p = alloc::vec![0u8; 20];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&((20 + udp_len) as u16).to_be_bytes());
    p[8] = 64;
    p[9] = 17;
    p[12..16].copy_from_slice(&SERVER_IP);
    p[16..20].copy_from_slice(&[255; 4]);
    let sum = checksum(&p);
    p[10..12].copy_from_slice(&sum.to_be_bytes());
    p.extend_from_slice(&67u16.to_be_bytes());
    p.extend_from_slice(&68u16.to_be_bytes());
    p.extend_from_slice(&(udp_len as u16).to_be_bytes());
    p.extend_from_slice(&[0, 0]); // no UDP checksum
    p.extend_from_slice(&m);
    Some(p)
}
