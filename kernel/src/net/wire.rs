//! Byte-level helpers for packet headers: big-endian fields, the internet
//! checksum, and address formatting.

use super::{as_v4, Addr, Ip};
use alloc::format;
use alloc::string::String;

pub fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}

pub fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

pub fn put16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_be_bytes());
}

pub fn put32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_be_bytes());
}

pub fn ip_at(b: &[u8], o: usize) -> Ip {
    [b[o], b[o + 1], b[o + 2], b[o + 3]]
}

/// Local ports handed out to sockets that don't bind one (TCP and UDP).
pub const EPHEMERAL: core::ops::Range<u16> = 49152..65535;

/// The ephemeral port after `p`, wrapping around the range.
pub fn next_ephemeral(p: u16) -> u16 {
    match p.checked_add(1) {
        Some(n) if EPHEMERAL.contains(&n) => n,
        _ => EPHEMERAL.start,
    }
}

/// One's-complement sum of 16-bit words (not yet folded or inverted).
pub fn sum(data: &[u8], mut acc: u32) -> u32 {
    let (words, rest) = data.as_chunks::<2>();
    for &w in words {
        acc = acc.wrapping_add(u16::from_be_bytes(w) as u32);
    }
    if let [last] = rest {
        acc = acc.wrapping_add((*last as u32) << 8);
    }
    acc
}

pub fn fold(mut acc: u32) -> u16 {
    while acc > 0xFFFF {
        acc = (acc & 0xFFFF) + (acc >> 16);
    }
    !(acc as u16)
}

/// The internet checksum of `data` (RFC 1071).
pub fn checksum(data: &[u8]) -> u16 {
    fold(sum(data, 0))
}

/// TCP/UDP (and ICMPv6) checksum over the pseudo-header and `segment`:
/// IPv4's when both addresses are IPv4-mapped, else IPv6's.
pub fn transport_checksum(src: Addr, dst: Addr, proto: u8, segment: &[u8]) -> u16 {
    let mut acc = match (as_v4(&src), as_v4(&dst)) {
        (Some(s), Some(d)) => sum(&d, sum(&s, 0)),
        _ => sum(&dst, sum(&src, 0)),
    };
    acc = acc.wrapping_add(proto as u32);
    acc = acc.wrapping_add((segment.len() as u32) >> 16).wrapping_add(segment.len() as u32 & 0xFFFF);
    fold(sum(segment, acc))
}

/// An address of either family as text.
#[allow(dead_code)] // used by the kernel tests
pub fn addr_str(a: &Addr) -> String {
    match as_v4(a) {
        Some(ip) => ip_str(ip),
        None => ip6_str(a),
    }
}

/// IPv6 text form, with the longest run of zero groups as "::".
pub fn ip6_str(a: &[u8; 16]) -> String {
    let g: [u16; 8] = core::array::from_fn(|i| u16::from_be_bytes([a[2 * i], a[2 * i + 1]]));
    let (mut best, mut best_len, mut i) = (8, 0, 0);
    while i < 8 {
        if g[i] == 0 {
            let start = i;
            while i < 8 && g[i] == 0 {
                i += 1;
            }
            if i - start > best_len && i - start > 1 {
                best = start;
                best_len = i - start;
            }
        } else {
            i += 1;
        }
    }
    let part =
        |r: core::ops::Range<usize>| g[r].iter().map(|x| format!("{x:x}")).collect::<alloc::vec::Vec<_>>().join(":");
    if best_len == 0 {
        part(0..8)
    } else {
        format!("{}::{}", part(0..best), part(best + best_len..8))
    }
}

pub fn ip_str(ip: Ip) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

pub fn mac_str(m: [u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}
