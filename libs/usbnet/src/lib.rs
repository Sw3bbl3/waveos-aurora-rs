//! Network over USB for WaveOS: the wire formats of USB Ethernet adapters,
//! docks and phone tethering, without any hardware access.
//!
//! - [`cdc`]: the Communications Device Class — which interfaces carry a
//!   network function, their functional descriptors (MAC address string,
//!   union), the interrupt endpoint's notifications (link, speed), and the
//!   class requests;
//! - [`ncm`]: CDC-NCM transfer blocks (NTB16 and NTB32), which carry many
//!   Ethernet frames per USB transfer;
//! - [`eem`]: CDC-EEM, frames behind a 2-byte header on a byte stream;
//! - [`rndis`]: Microsoft's RNDIS (Android tethering, many adapters): its
//!   control messages, OID queries and data packets;
//! - [`asix`]: the ASIX AX88772 and AX88179 adapters' vendor framing;
//! - [`select`]: which configuration of a device to use, and which driver
//!   takes which vendor chip.
//!
//! The kernel's drivers (kernel/src/drivers/usb/net) move the bytes; all of
//! the parsing happens here, so it is tested — and fuzzed — on the host. A
//! USB device is untrusted input: every length and offset it sends is
//! checked before use, and nothing here panics on any input.

#![no_std]

extern crate alloc;

pub mod asix;
pub mod cdc;
pub mod eem;
pub mod ncm;
pub mod rndis;
pub mod select;

/// The largest Ethernet frame the drivers pass on (1500-byte payload,
/// header and an 802.1Q tag; no FCS).
pub const MAX_FRAME: usize = 1518;
/// The smallest: an Ethernet header.
pub const MIN_FRAME: usize = 14;

/// Why a transfer was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// Shorter than its header says, or a field points outside it.
    Truncated,
    /// A signature, type or length field that is not allowed.
    Malformed,
    /// More entries than the format allows (or a loop of tables).
    TooMany,
}

pub(crate) fn le16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}

pub(crate) fn le32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([*b.get(at)?, *b.get(at + 1)?, *b.get(at + 2)?, *b.get(at + 3)?]))
}

pub(crate) fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

pub(crate) fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

/// `v` rounded up to a multiple of `align` (any non-zero value; 0 and 1 leave it).
pub(crate) fn align_up(v: usize, align: usize) -> usize {
    if align <= 1 {
        v
    } else {
        v.div_ceil(align) * align
    }
}

/// The IEEE 802.3 CRC-32 of `data` (Ethernet's frame check sequence, and
/// what CDC-EEM puts after a frame when it computes one).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// Whether `mac` can be a station's address (not all zeros, not multicast
/// or broadcast).
pub fn valid_mac(mac: &[u8; 6]) -> bool {
    *mac != [0; 6] && mac[0] & 1 == 0
}

/// A locally administered address (bit 1 of the first byte), as phones and
/// USB gadgets make up; adapters with a vendor's address are "real" NICs.
pub fn locally_administered(mac: &[u8; 6]) -> bool {
    mac[0] & 2 != 0
}

#[cfg(test)]
mod tests {
    #[test]
    fn crc_known_value() {
        assert_eq!(super::crc32(b"123456789"), 0xCBF4_3926);
    }

    #[test]
    fn align() {
        assert_eq!(super::align_up(13, 4), 16);
        assert_eq!(super::align_up(16, 4), 16);
        assert_eq!(super::align_up(5, 0), 5);
        assert_eq!(super::align_up(5, 3), 6);
    }
}
