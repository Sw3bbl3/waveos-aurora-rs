//! Remote NDIS: control messages, OIDs and data packets.
//!
//! The host sends control messages with SEND_ENCAPSULATED_COMMAND on the
//! control endpoint and collects each answer with GET_ENCAPSULATED_RESPONSE
//! (the interrupt endpoint says when one is ready). Bring-up is INITIALIZE,
//! then queries (the MAC address, the link), then setting a packet filter —
//! only then does the device pass traffic. Ethernet frames travel on the
//! bulk endpoints inside PACKET messages (a 44-byte header, then the frame),
//! several to a transfer when the device allows.
//!
//! All numbers are little-endian 32-bit words; offsets inside a message
//! count from its third word (byte 8).

use crate::{align_up, le32, put32, Error, MAX_FRAME, MIN_FRAME};
use alloc::vec;
use alloc::vec::Vec;
use core::ops::Range;

pub const PACKET_MSG: u32 = 0x0000_0001;
pub const INITIALIZE_MSG: u32 = 0x0000_0002;
pub const HALT_MSG: u32 = 0x0000_0003;
pub const QUERY_MSG: u32 = 0x0000_0004;
pub const SET_MSG: u32 = 0x0000_0005;
pub const RESET_MSG: u32 = 0x0000_0006;
pub const INDICATE_STATUS_MSG: u32 = 0x0000_0007;
pub const KEEPALIVE_MSG: u32 = 0x0000_0008;
const COMPLETION: u32 = 0x8000_0000;

pub const STATUS_SUCCESS: u32 = 0;
pub const STATUS_MEDIA_CONNECT: u32 = 0x4001_000B;
pub const STATUS_MEDIA_DISCONNECT: u32 = 0x4001_000C;
pub const STATUS_LINK_SPEED_CHANGE: u32 = 0x4001_0013;

pub const OID_GEN_SUPPORTED_LIST: u32 = 0x0001_0101;
pub const OID_GEN_MAXIMUM_FRAME_SIZE: u32 = 0x0001_0106;
/// Link speed, in units of 100 bit/s.
pub const OID_GEN_LINK_SPEED: u32 = 0x0001_0107;
pub const OID_GEN_CURRENT_PACKET_FILTER: u32 = 0x0001_010E;
/// 0 connected, 1 disconnected.
pub const OID_GEN_MEDIA_CONNECT_STATUS: u32 = 0x0001_0114;
pub const OID_802_3_PERMANENT_ADDRESS: u32 = 0x0101_0101;
pub const OID_802_3_CURRENT_ADDRESS: u32 = 0x0101_0102;
pub const OID_802_3_MULTICAST_LIST: u32 = 0x0101_0103;

// OID_GEN_CURRENT_PACKET_FILTER bits.
pub const FILTER_DIRECTED: u32 = 0x01;
pub const FILTER_MULTICAST: u32 = 0x02;
pub const FILTER_ALL_MULTICAST: u32 = 0x04;
pub const FILTER_BROADCAST: u32 = 0x08;
pub const FILTER_PROMISCUOUS: u32 = 0x20;

/// A PACKET message's header.
pub const PACKET_HEADER: usize = 44;
/// The largest control message we accept (a supported-OID list, a vendor string).
pub const MAX_CONTROL: usize = 4096;
/// The largest data message: a device that claims more is lying or broken.
const MAX_PACKET_MSG: usize = 64 * 1024;

fn message(kind: u32, words: &[u32]) -> Vec<u8> {
    let mut m = vec![0u8; 8 + 4 * words.len()];
    put32(&mut m, 0, kind);
    let n = m.len() as u32;
    put32(&mut m, 4, n);
    for (k, &w) in words.iter().enumerate() {
        put32(&mut m, 8 + 4 * k, w);
    }
    m
}

/// INITIALIZE: version 1.0, and the largest transfer we take from the device.
pub fn initialize(request: u32, max_transfer: u32) -> Vec<u8> {
    message(INITIALIZE_MSG, &[request, 1, 0, max_transfer])
}

/// QUERY for `oid` (with no input buffer).
pub fn query(request: u32, oid: u32) -> Vec<u8> {
    message(QUERY_MSG, &[request, oid, 0, 0, 0])
}

/// SET `oid` to `value`.
pub fn set(request: u32, oid: u32, value: &[u8]) -> Vec<u8> {
    let mut m = message(SET_MSG, &[request, oid, value.len() as u32, 20, 0]);
    m.extend_from_slice(value);
    let n = m.len() as u32;
    put32(&mut m, 4, n);
    m
}

pub fn keepalive(request: u32) -> Vec<u8> {
    message(KEEPALIVE_MSG, &[request])
}

/// Our answer to the device's own KEEPALIVE.
pub fn keepalive_complete(request: u32) -> Vec<u8> {
    message(KEEPALIVE_MSG | COMPLETION, &[request, STATUS_SUCCESS])
}

pub fn halt(request: u32) -> Vec<u8> {
    message(HALT_MSG, &[request])
}

pub fn reset() -> Vec<u8> {
    message(RESET_MSG, &[0])
}

/// What INITIALIZE's completion says about the data channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Init {
    /// Medium 0 is 802.3 (Ethernet); anything else we can't drive.
    pub medium: u32,
    /// PACKET messages the device takes in one transfer (at least 1).
    pub max_packets: u32,
    /// Bytes the device takes in one transfer.
    pub max_transfer: u32,
    /// Each message after the first starts at a multiple of 2^this.
    pub alignment_shift: u32,
}

/// A control message from the device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    InitComplete {
        request: u32,
        status: u32,
        init: Init,
    },
    /// `data` is where the answer lies in the message.
    QueryComplete {
        request: u32,
        status: u32,
        data: Range<usize>,
    },
    SetComplete {
        request: u32,
        status: u32,
    },
    ResetComplete {
        status: u32,
    },
    KeepaliveComplete {
        request: u32,
        status: u32,
    },
    /// The link came up or went down, or another event (`status`).
    Indicate {
        status: u32,
    },
    /// The device asks whether we are still there: answer [`keepalive_complete`].
    Keepalive {
        request: u32,
    },
}

/// Reads one control message (the first `MessageLength` bytes of `b`).
pub fn parse_response(b: &[u8]) -> Result<Response, Error> {
    let kind = le32(b, 0).ok_or(Error::Truncated)?;
    let len = le32(b, 4).ok_or(Error::Truncated)? as usize;
    if len < 12 || len > b.len().min(MAX_CONTROL) {
        return Err(Error::Truncated);
    }
    let b = &b[..len];
    let w = |k: usize| le32(b, 8 + 4 * k).ok_or(Error::Truncated);
    Ok(match kind {
        k if k == INITIALIZE_MSG | COMPLETION => Response::InitComplete {
            request: w(0)?,
            status: w(1)?,
            init: Init {
                medium: w(5)?,
                max_packets: w(6)?.max(1),
                max_transfer: w(7)?,
                alignment_shift: w(8)?.min(12),
            },
        },
        k if k == QUERY_MSG | COMPLETION => {
            let (request, status, length, offset) = (w(0)?, w(1)?, w(2)? as usize, w(3)? as usize);
            let data = if length == 0 {
                0..0
            } else {
                let start = offset.checked_add(8).ok_or(Error::Malformed)?;
                let end = start.checked_add(length).ok_or(Error::Malformed)?;
                if start < 24 || end > len {
                    return Err(Error::Truncated);
                }
                start..end
            };
            Response::QueryComplete { request, status, data }
        }
        k if k == SET_MSG | COMPLETION => Response::SetComplete { request: w(0)?, status: w(1)? },
        k if k == RESET_MSG | COMPLETION => Response::ResetComplete { status: w(0)? },
        k if k == KEEPALIVE_MSG | COMPLETION => Response::KeepaliveComplete { request: w(0)?, status: w(1)? },
        INDICATE_STATUS_MSG => Response::Indicate { status: w(0)? },
        KEEPALIVE_MSG => Response::Keepalive { request: w(0)? },
        _ => return Err(Error::Malformed),
    })
}

/// A query answer as a 32-bit value.
pub fn query_u32(b: &[u8], data: &Range<usize>) -> Option<u32> {
    (data.len() >= 4).then(|| le32(b, data.start)).flatten()
}

/// Bytes a PACKET message for `frame_len` bytes takes, padded for `shift`.
pub fn packet_len(frame_len: usize, shift: u32) -> usize {
    align_up(PACKET_HEADER + frame_len, 1 << shift.min(12))
}

/// Writes a PACKET message for `frame` at the start of `buf` (which must
/// hold [`packet_len`] bytes); returns its length.
pub fn write_packet(buf: &mut [u8], frame: &[u8], shift: u32) -> usize {
    let total = packet_len(frame.len(), shift);
    buf[..PACKET_HEADER].fill(0);
    put32(buf, 0, PACKET_MSG);
    put32(buf, 4, total as u32);
    put32(buf, 8, (PACKET_HEADER - 8) as u32);
    put32(buf, 12, frame.len() as u32);
    buf[PACKET_HEADER..PACKET_HEADER + frame.len()].copy_from_slice(frame);
    buf[PACKET_HEADER + frame.len()..total].fill(0);
    total
}

/// Takes the data channel apart: PACKET messages, several per transfer,
/// possibly (on emulated devices) one split across two transfers.
#[derive(Default)]
pub struct Receiver {
    carry: Vec<u8>,
    /// Messages that made no sense (the rest of their transfer was dropped).
    pub errors: u64,
}

impl Receiver {
    /// Hands each Ethernet frame in `data` to `out`.
    pub fn feed(&mut self, data: &[u8], out: &mut dyn FnMut(&[u8])) {
        if self.carry.is_empty() {
            let used = self.messages(data, out);
            if used < data.len() {
                self.carry.extend_from_slice(&data[used..]);
            }
            return;
        }
        let mut buf = core::mem::take(&mut self.carry);
        buf.extend_from_slice(data);
        let used = self.messages(&buf, out);
        buf.drain(..used.min(buf.len()));
        self.carry = buf;
    }

    /// Frames from whole messages at the start of `b`; returns the bytes
    /// consumed (the rest is an incomplete message, or was dropped).
    fn messages(&mut self, b: &[u8], out: &mut dyn FnMut(&[u8])) -> usize {
        let mut at = 0;
        while b.len() - at >= 8 {
            let m = &b[at..];
            let kind = le32(m, 0).unwrap_or(0);
            let len = le32(m, 4).unwrap_or(0) as usize;
            if kind == 0 && len == 0 {
                // Zero padding after the last message.
                return b.len();
            }
            if kind != PACKET_MSG || !(PACKET_HEADER..=MAX_PACKET_MSG).contains(&len) {
                self.errors += 1;
                return b.len();
            }
            if m.len() < len {
                return at;
            }
            let offset = le32(m, 8).unwrap_or(0) as usize;
            let size = le32(m, 12).unwrap_or(0) as usize;
            let start = offset.saturating_add(8);
            match start.checked_add(size) {
                Some(end) if start >= PACKET_HEADER && end <= len && (MIN_FRAME..=MAX_FRAME).contains(&size) => {
                    out(&m[start..end]);
                }
                _ => self.errors += 1,
            }
            at += len;
        }
        at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests() {
        let m = initialize(1, 16384);
        assert_eq!(m.len(), 24);
        assert_eq!(le32(&m, 0), Some(2));
        assert_eq!(le32(&m, 20), Some(16384));
        let m = set(3, OID_GEN_CURRENT_PACKET_FILTER, &0x0Fu32.to_le_bytes());
        assert_eq!(m.len(), 32);
        assert_eq!((le32(&m, 4), le32(&m, 16), le32(&m, 20), le32(&m, 28)), (Some(32), Some(4), Some(20), Some(15)));
        assert_eq!(query(2, OID_802_3_PERMANENT_ADDRESS).len(), 28);
    }

    #[test]
    fn qemu_query_answer() {
        // QUERY_CMPLT with the MAC address, as QEMU builds it.
        let mut m = vec![0u8; 24 + 6];
        put32(&mut m, 0, QUERY_MSG | COMPLETION);
        put32(&mut m, 4, 30);
        put32(&mut m, 8, 7);
        put32(&mut m, 16, 6);
        put32(&mut m, 20, 16);
        m[24..].copy_from_slice(&[0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        let Ok(Response::QueryComplete { request: 7, status: 0, data }) = parse_response(&m) else { panic!() };
        assert_eq!(&m[data], &[0x52, 0x54, 0, 0x12, 0x34, 0x56]);
        // The answer may not point outside the message, nor over its header.
        put32(&mut m, 20, 17);
        assert_eq!(parse_response(&m), Err(Error::Truncated));
        put32(&mut m, 20, 0);
        assert_eq!(parse_response(&m), Err(Error::Truncated));
        put32(&mut m, 20, 16);
        put32(&mut m, 16, u32::MAX);
        assert!(parse_response(&m).is_err());
    }

    #[test]
    fn packets_round_trip_across_transfers() {
        let frames: Vec<Vec<u8>> = (0..5).map(|k| vec![k as u8; 60 + k * 300]).collect();
        let mut stream = Vec::new();
        for f in &frames {
            let mut b = vec![0u8; packet_len(f.len(), 2)];
            write_packet(&mut b, f, 2);
            assert_eq!(b.len() % 4, 0);
            stream.extend_from_slice(&b);
        }
        // Split anywhere: every frame comes out once, whole.
        for cut in 0..=stream.len() {
            let mut r = Receiver::default();
            let mut got = Vec::new();
            r.feed(&stream[..cut], &mut |f| got.push(f.to_vec()));
            r.feed(&stream[cut..], &mut |f| got.push(f.to_vec()));
            assert_eq!(got, frames, "split at {cut}");
            assert_eq!(r.errors, 0);
        }
    }

    #[test]
    fn hostile_packets() {
        let mut r = Receiver::default();
        let mut n = 0;
        // Data offset pointing back into the header; a length beyond the message.
        let mut b = vec![0u8; 100];
        write_packet(&mut b, &[1; 56], 0);
        put32(&mut b, 8, 0);
        r.feed(&b, &mut |_| n += 1);
        write_packet(&mut b, &[1; 56], 0);
        put32(&mut b, 12, 0xFFFF_FFF0);
        r.feed(&b, &mut |_| n += 1);
        assert_eq!((n, r.errors), (0, 2));
        // A message claiming 4 GiB is not waited for.
        put32(&mut b, 4, u32::MAX);
        r.feed(&b, &mut |_| n += 1);
        assert!(r.carry.is_empty());
    }
}
