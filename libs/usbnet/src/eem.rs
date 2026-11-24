//! CDC-EEM: Ethernet frames on a byte stream, each behind a 2-byte header.
//!
//! A header's top bit says what follows. A *data* packet (bit 15 clear) is
//! a frame and its 4-byte CRC — or, when bit 14 is clear, the sentinel
//! 0xDEADBEEF in its place — with the length (frame + 4) in bits 0–13. A
//! *command* (bit 15 set) carries its code in bits 11–13; the only one
//! with a payload is Echo, which the host must send back as Echo Response.
//! A header of zero is a zero-length packet, used for padding.

use crate::{crc32, Error, MAX_FRAME, MIN_FRAME};
use alloc::vec::Vec;

const SENTINEL: [u8; 4] = [0xDE, 0xAD, 0xBE, 0xEF];
const CMD_ECHO: u16 = 0;
const CMD_ECHO_RESPONSE: u16 = 1;

/// Bytes a frame takes on the wire.
pub fn packet_len(frame_len: usize) -> usize {
    2 + frame_len + 4
}

/// Writes a data packet for `frame` (sentinel, no CRC) at the start of
/// `buf`; returns its length.
pub fn write_packet(buf: &mut [u8], frame: &[u8]) -> usize {
    let len = frame.len() + 4;
    buf[..2].copy_from_slice(&((len as u16) & 0x3FFF).to_le_bytes());
    buf[2..2 + frame.len()].copy_from_slice(frame);
    buf[2 + frame.len()..2 + len].copy_from_slice(&SENTINEL);
    2 + len
}

/// A zero-length packet (to end a transfer that fills whole USB packets).
pub const ZLP: [u8; 2] = [0, 0];

/// What came in, besides frames.
#[derive(Debug, Default)]
pub struct Received {
    /// Echo commands' answers, to send back (whole EEM packets).
    pub replies: Vec<Vec<u8>>,
    pub bad_crc: u64,
    pub errors: u64,
}

/// Splits a received stream, keeping an incomplete packet for the next transfer.
#[derive(Default)]
pub struct Receiver {
    carry: Vec<u8>,
}

impl Receiver {
    /// Hands each good frame in `data` to `out`.
    pub fn feed(&mut self, data: &[u8], r: &mut Received, out: &mut dyn FnMut(&[u8])) {
        let mut buf = core::mem::take(&mut self.carry);
        let used = if buf.is_empty() {
            let used = packets(data, r, out);
            buf.extend_from_slice(&data[used..]);
            buf.len()
        } else {
            buf.extend_from_slice(data);
            let used = packets(&buf, r, out);
            buf.drain(..used);
            buf.len()
        };
        // An incomplete packet is at most a frame long.
        if used > 2 + 0x3FFF {
            r.errors += 1;
            buf.clear();
        }
        self.carry = buf;
    }
}

fn packets(b: &[u8], r: &mut Received, out: &mut dyn FnMut(&[u8])) -> usize {
    let mut at = 0;
    while b.len() - at >= 2 {
        let h = u16::from_le_bytes([b[at], b[at + 1]]);
        let body = at + 2;
        if h & 0x8000 == 0 {
            let len = (h & 0x3FFF) as usize;
            if len == 0 {
                at = body;
                continue;
            }
            if b.len() < body + len {
                return at;
            }
            if len < 4 {
                r.errors += 1;
            } else {
                let (frame, tail) = b[body..body + len].split_at(len - 4);
                let ok = if h & 0x4000 != 0 { crc32(frame).to_le_bytes() == *tail } else { *tail == SENTINEL };
                if !ok {
                    r.bad_crc += 1;
                } else if (MIN_FRAME..=MAX_FRAME).contains(&frame.len()) {
                    out(frame);
                } else {
                    r.errors += 1;
                }
            }
            at = body + len;
        } else {
            let cmd = (h >> 11) & 7;
            if cmd == CMD_ECHO {
                let len = (h & 0x7FF) as usize;
                if b.len() < body + len {
                    return at;
                }
                if r.replies.len() < 8 {
                    let mut reply = Vec::with_capacity(2 + len);
                    reply.extend_from_slice(&(0x8000 | CMD_ECHO_RESPONSE << 11 | len as u16).to_le_bytes());
                    reply.extend_from_slice(&b[body..body + len]);
                    r.replies.push(reply);
                }
                at = body + len;
            } else {
                // Echo responses, suspend and response hints, tickles: nothing to do.
                at = body;
            }
        }
    }
    at
}

/// Checks a stream without keeping anything: for tests and fuzzing.
pub fn parse_all(b: &[u8]) -> Result<Vec<Vec<u8>>, Error> {
    let mut frames = Vec::new();
    let mut r = Received::default();
    let used = packets(b, &mut r, &mut |f| frames.push(f.to_vec()));
    if used != b.len() {
        return Err(Error::Truncated);
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn round_trip() {
        let frames: Vec<Vec<u8>> = (0..4).map(|k| vec![k as u8 + 1; 60 + 400 * k]).collect();
        let mut stream = Vec::new();
        for f in &frames {
            let mut b = vec![0u8; packet_len(f.len())];
            write_packet(&mut b, f);
            stream.extend_from_slice(&b);
            stream.extend_from_slice(&ZLP);
        }
        assert_eq!(parse_all(&stream).unwrap(), frames);
        for cut in 0..=stream.len() {
            let mut rx = Receiver::default();
            let mut r = Received::default();
            let mut got = Vec::new();
            rx.feed(&stream[..cut], &mut r, &mut |f| got.push(f.to_vec()));
            rx.feed(&stream[cut..], &mut r, &mut |f| got.push(f.to_vec()));
            assert_eq!(got, frames, "split at {cut}");
        }
    }

    #[test]
    fn crc_and_echo() {
        let frame = vec![0x42u8; 64];
        let mut p = vec![];
        p.extend_from_slice(&(0x4000u16 | 68).to_le_bytes());
        p.extend_from_slice(&frame);
        p.extend_from_slice(&crc32(&frame).to_le_bytes());
        // Echo with three bytes, then the same frame with a broken CRC.
        p.extend_from_slice(&[3, 0x80, 7, 8, 9]);
        p.extend_from_slice(&(0x4000u16 | 68).to_le_bytes());
        p.extend_from_slice(&frame);
        p.extend_from_slice(&[0, 0, 0, 0]);
        let mut r = Received::default();
        let mut got = 0;
        Receiver::default().feed(&p, &mut r, &mut |f| {
            assert_eq!(f, &frame[..]);
            got += 1
        });
        assert_eq!((got, r.bad_crc), (1, 1));
        assert_eq!(r.replies, vec![vec![3, 0x88, 7, 8, 9]]);
    }
}
