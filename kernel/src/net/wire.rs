//! Byte-level helpers for packet headers: big-endian fields, the internet
//! checksum, and address formatting.

use super::Ip;
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

/// One's-complement sum of 16-bit words (not yet folded or inverted).
pub fn sum(data: &[u8], mut acc: u32) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        acc = acc.wrapping_add(u16::from_be_bytes([c[0], c[1]]) as u32);
    }
    if let [last] = chunks.remainder() {
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

/// TCP/UDP checksum over the IPv4 pseudo-header and `segment`.
pub fn transport_checksum(src: Ip, dst: Ip, proto: u8, segment: &[u8]) -> u16 {
    let mut acc = sum(&src, 0);
    acc = sum(&dst, acc);
    acc = acc.wrapping_add(proto as u32);
    acc = acc.wrapping_add(segment.len() as u32);
    fold(sum(segment, acc))
}

pub fn ip_str(ip: Ip) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

pub fn mac_str(m: [u8; 6]) -> String {
    format!("{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}", m[0], m[1], m[2], m[3], m[4], m[5])
}
