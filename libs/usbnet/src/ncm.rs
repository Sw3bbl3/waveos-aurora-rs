//! CDC-NCM transfer blocks (NTBs).
//!
//! An NTB is one USB transfer carrying many Ethernet frames ("datagrams"):
//! a transfer header (NTH) with the block's length and the offset of the
//! first datagram pointer table (NDP); each NDP lists (offset, length)
//! pairs, ends with a zero pair, and may point to another NDP. NTB16 uses
//! 16-bit fields (blocks up to 64 KiB), NTB32 32-bit ones.
//!
//! The device states its limits in the 28-byte NTB parameter structure
//! ([`Params`]): the largest blocks each way, and where datagrams must
//! start in an outgoing block (`offset % divisor == remainder`).

use crate::{align_up, le16, le32, put16, put32, Error, MIN_FRAME};
use alloc::vec::Vec;
use core::ops::Range;

const NTH16_SIGNATURE: u32 = 0x484D_434E; // "NCMH"
const NTH32_SIGNATURE: u32 = 0x686D_636E; // "ncmh"
const NDP16_NOCRC: u32 = 0x304D_434E; // "NCM0"
const NDP16_CRC: u32 = 0x314D_434E; // "NCM1"
const NDP32_NOCRC: u32 = 0x306D_636E; // "ncm0"
const NDP32_CRC: u32 = 0x316D_636E; // "ncm1"

/// NDPs a block may chain before we stop following (a loop, or abuse).
const MAX_NDPS: usize = 32;
/// Datagrams one block may carry before we stop reading (memory bound).
pub const MAX_DATAGRAMS: usize = 512;

/// GET_NTB_PARAMETERS' answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Params {
    /// Bit 0: NTB16, bit 1: NTB32.
    pub formats: u16,
    pub in_max: u32,
    pub in_divisor: u16,
    pub in_remainder: u16,
    pub in_alignment: u16,
    pub out_max: u32,
    pub out_divisor: u16,
    pub out_remainder: u16,
    pub out_alignment: u16,
    /// Datagrams per outgoing block (0: no limit).
    pub out_max_datagrams: u16,
}

impl Params {
    pub const LEN: usize = 28;

    /// Reads the structure; out-of-range values are clamped to something
    /// safe to build blocks with rather than rejected (devices get these wrong).
    pub fn parse(b: &[u8]) -> Result<Params, Error> {
        if b.len() < Params::LEN {
            return Err(Error::Truncated);
        }
        let g16 = |at| le16(b, at).unwrap_or(0);
        let g32 = |at| le32(b, at).unwrap_or(0);
        let p = Params {
            formats: g16(2),
            in_max: g32(4),
            in_divisor: g16(8),
            in_remainder: g16(10),
            in_alignment: g16(12),
            out_max: g32(16),
            out_divisor: g16(20),
            out_remainder: g16(22),
            out_alignment: g16(24),
            out_max_datagrams: g16(26),
        };
        if p.formats & 1 == 0 {
            // NTB16 support is mandatory.
            return Err(Error::Malformed);
        }
        Ok(p)
    }

    /// Whether 32-bit blocks are on offer.
    pub fn ntb32(&self) -> bool {
        self.formats & 2 != 0
    }

    /// The outgoing layout we build with, sanitised.
    pub fn out_layout(&self, ntb32: bool, max_bytes: usize) -> Layout {
        let divisor = (self.out_divisor as usize).clamp(1, 512);
        // A divisor that is not a power of two, or a remainder past it, is
        // ignored: 4-byte alignment works with every device we know.
        let (divisor, remainder) = if divisor.is_power_of_two() && (self.out_remainder as usize) < divisor {
            (divisor, self.out_remainder as usize)
        } else {
            (4, 0)
        };
        let ndp_align = (self.out_alignment as usize).clamp(4, 64);
        let ndp_align = if ndp_align.is_power_of_two() { ndp_align } else { 4 };
        let limit = if ntb32 { usize::MAX } else { 0xFFFF };
        let max = (self.out_max as usize).clamp(2048, limit).min(max_bytes.max(2048));
        Layout {
            ntb32,
            max,
            divisor,
            remainder,
            ndp_align,
            max_datagrams: if self.out_max_datagrams == 0 {
                MAX_DATAGRAMS
            } else {
                (self.out_max_datagrams as usize).min(MAX_DATAGRAMS)
            },
        }
    }
}

/// Where things go in the blocks we send.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub ntb32: bool,
    /// Largest block.
    pub max: usize,
    pub divisor: usize,
    pub remainder: usize,
    pub ndp_align: usize,
    pub max_datagrams: usize,
}

impl Layout {
    fn nth_len(&self) -> usize {
        if self.ntb32 {
            16
        } else {
            12
        }
    }

    fn ndp_len(&self, datagrams: usize) -> usize {
        // Header, the entries and the terminating zero entry.
        if self.ntb32 {
            16 + 8 * (datagrams + 1)
        } else {
            8 + 4 * (datagrams + 1)
        }
    }

    /// Where a datagram after `end` may start.
    fn datagram_start(&self, end: usize) -> usize {
        let base = align_up(end, self.divisor);
        let at = base - self.divisor + self.remainder;
        if at >= end {
            at
        } else {
            base + self.remainder
        }
    }
}

/// Builds outgoing blocks: datagrams first, the NDP after them.
pub struct Builder {
    layout: Layout,
    /// Offsets and lengths of the datagrams placed so far.
    entries: Vec<(usize, usize)>,
    end: usize,
}

impl Builder {
    pub fn new(layout: Layout) -> Builder {
        Builder { layout, entries: Vec::new(), end: layout.nth_len() }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn count(&self) -> usize {
        self.entries.len()
    }

    /// Whether a `len`-byte frame still fits.
    pub fn fits(&self, len: usize) -> bool {
        if self.entries.len() >= self.layout.max_datagrams || len == 0 {
            return false;
        }
        let start = self.layout.datagram_start(self.end);
        let ndp = align_up(start + len, self.layout.ndp_align);
        ndp + self.layout.ndp_len(self.entries.len() + 1) <= self.layout.max
    }

    /// Copies `frame` into `buf` (the block being built, at least
    /// `layout.max` bytes). False when it does not fit: send the block first.
    pub fn push(&mut self, buf: &mut [u8], frame: &[u8]) -> bool {
        if !self.fits(frame.len()) || buf.len() < self.layout.max {
            return false;
        }
        let start = self.layout.datagram_start(self.end);
        buf[self.end..start].fill(0);
        buf[start..start + frame.len()].copy_from_slice(frame);
        self.entries.push((start, frame.len()));
        self.end = start + frame.len();
        true
    }

    /// Writes the headers; returns the block's length. The builder starts over.
    pub fn finish(&mut self, buf: &mut [u8], sequence: u16) -> usize {
        let l = self.layout;
        let ndp = align_up(self.end, l.ndp_align);
        buf[self.end..ndp].fill(0);
        let total = ndp + l.ndp_len(self.entries.len());
        if l.ntb32 {
            put32(buf, 0, NTH32_SIGNATURE);
            put16(buf, 4, 16);
            put16(buf, 6, sequence);
            put32(buf, 8, total as u32);
            put32(buf, 12, ndp as u32);
            put32(buf, ndp, NDP32_NOCRC);
            put16(buf, ndp + 4, l.ndp_len(self.entries.len()) as u16);
            put16(buf, ndp + 6, 0);
            put32(buf, ndp + 8, 0);
            put32(buf, ndp + 12, 0);
            for (k, &(at, len)) in self.entries.iter().chain(&[(0, 0)]).enumerate() {
                put32(buf, ndp + 16 + 8 * k, at as u32);
                put32(buf, ndp + 20 + 8 * k, len as u32);
            }
        } else {
            put32(buf, 0, NTH16_SIGNATURE);
            put16(buf, 4, 12);
            put16(buf, 6, sequence);
            put16(buf, 8, total as u16);
            put16(buf, 10, ndp as u16);
            put32(buf, ndp, NDP16_NOCRC);
            put16(buf, ndp + 4, l.ndp_len(self.entries.len()) as u16);
            put16(buf, ndp + 6, 0);
            for (k, &(at, len)) in self.entries.iter().chain(&[(0, 0)]).enumerate() {
                put16(buf, ndp + 8 + 4 * k, at as u16);
                put16(buf, ndp + 10 + 4 * k, len as u16);
            }
        }
        self.entries.clear();
        self.end = l.nth_len();
        total
    }
}

/// The datagrams of a received block, as ranges of `b`. Every range lies
/// inside the block and is a plausible Ethernet frame ([`MIN_FRAME`] to
/// `max_frame` bytes); entries that are not are skipped, while a header or
/// table that is broken rejects the block.
pub fn parse(b: &[u8], max_frame: usize) -> Result<Vec<Range<usize>>, Error> {
    let max_frame = max_frame.clamp(MIN_FRAME, 16 * 1024);
    let sig = le32(b, 0).ok_or(Error::Truncated)?;
    let ntb32 = match sig {
        NTH16_SIGNATURE => false,
        NTH32_SIGNATURE => true,
        _ => return Err(Error::Malformed),
    };
    let header_len = le16(b, 4).ok_or(Error::Truncated)? as usize;
    let (block_len, mut ndp) = if ntb32 {
        if header_len != 16 {
            return Err(Error::Malformed);
        }
        (le32(b, 8).ok_or(Error::Truncated)? as usize, le32(b, 12).ok_or(Error::Truncated)? as usize)
    } else {
        if header_len != 12 {
            return Err(Error::Malformed);
        }
        (le16(b, 8).ok_or(Error::Truncated)? as usize, le16(b, 10).ok_or(Error::Truncated)? as usize)
    };
    // Some devices pad the transfer past the block, a few report 0 for
    // "the whole transfer": either way nothing outside `b` is read.
    let block = if block_len == 0 || block_len > b.len() { b.len() } else { block_len };
    let b = &b[..block];
    let mut out = Vec::new();
    let mut tables = 0;
    while ndp != 0 {
        tables += 1;
        if tables > MAX_NDPS {
            return Err(Error::TooMany);
        }
        if ndp < header_len || ndp % 4 != 0 {
            return Err(Error::Malformed);
        }
        let sig = le32(b, ndp).ok_or(Error::Truncated)?;
        let table_len = le16(b, ndp + 4).ok_or(Error::Truncated)? as usize;
        let (head, entry, next) = match (ntb32, sig) {
            (false, NDP16_NOCRC | NDP16_CRC) => (8, 4, le16(b, ndp + 6).ok_or(Error::Truncated)? as usize),
            (true, NDP32_NOCRC | NDP32_CRC) => (16, 8, le32(b, ndp + 8).ok_or(Error::Truncated)? as usize),
            _ => return Err(Error::Malformed),
        };
        if table_len < head + 2 * entry || ndp + table_len > b.len() {
            return Err(Error::Truncated);
        }
        let entries = (table_len - head) / entry;
        for k in 0..entries {
            let at = ndp + head + k * entry;
            let (index, len) = if ntb32 {
                (le32(b, at).unwrap_or(0) as usize, le32(b, at + 4).unwrap_or(0) as usize)
            } else {
                (le16(b, at).unwrap_or(0) as usize, le16(b, at + 2).unwrap_or(0) as usize)
            };
            if index == 0 || len == 0 {
                break;
            }
            if out.len() >= MAX_DATAGRAMS {
                return Err(Error::TooMany);
            }
            // In bounds, past the header and a plausible frame.
            let fits = index >= header_len && index.checked_add(len).is_some_and(|end| end <= b.len());
            if fits && (MIN_FRAME..=max_frame).contains(&len) {
                out.push(index..index + len);
            }
        }
        ndp = next;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MAX_FRAME;
    use alloc::vec;

    fn params(out_max: u32, divisor: u16, remainder: u16, align: u16, max_dg: u16, formats: u16) -> Params {
        Params {
            formats,
            in_max: 16384,
            in_divisor: 4,
            in_remainder: 0,
            in_alignment: 4,
            out_max,
            out_divisor: divisor,
            out_remainder: remainder,
            out_alignment: align,
            out_max_datagrams: max_dg,
        }
    }

    fn frame(n: usize, seed: u8) -> Vec<u8> {
        (0..n).map(|i| (i as u8).wrapping_mul(7).wrapping_add(seed)).collect()
    }

    fn round_trip(p: Params, ntb32: bool, sizes: &[usize]) {
        let layout = p.out_layout(ntb32, 1 << 20);
        let mut buf = vec![0xEEu8; layout.max];
        let mut b = Builder::new(layout);
        let frames: Vec<Vec<u8>> = sizes.iter().enumerate().map(|(k, &n)| frame(n, k as u8)).collect();
        let mut sent = 0;
        for f in &frames {
            if !b.push(&mut buf, f) {
                break;
            }
            sent += 1;
        }
        let n = b.finish(&mut buf, 9);
        assert!(n <= layout.max);
        let got = parse(&buf[..n], MAX_FRAME).unwrap();
        assert_eq!(got.len(), sent);
        for (r, f) in got.iter().zip(&frames) {
            assert_eq!(&buf[r.clone()], &f[..]);
            assert_eq!(r.start % layout.divisor, layout.remainder);
        }
    }

    #[test]
    fn ntb16_round_trip() {
        round_trip(params(16384, 4, 0, 4, 0, 1), false, &[60, 1514, 100, 64, 1000]);
        // A device that wants datagrams at offset ≡ 2 (mod 4) for IP alignment.
        round_trip(params(8192, 4, 2, 8, 0, 1), false, &[60, 61, 62, 63, 1514]);
    }

    #[test]
    fn ntb32_round_trip() {
        round_trip(params(65536, 8, 0, 8, 0, 3), true, &[1514; 40]);
    }

    #[test]
    fn limits_respected() {
        // Two datagrams at most per block.
        let layout = params(16384, 4, 0, 4, 2, 1).out_layout(false, 1 << 20);
        let mut buf = vec![0u8; layout.max];
        let mut b = Builder::new(layout);
        assert!(b.push(&mut buf, &frame(100, 1)));
        assert!(b.push(&mut buf, &frame(100, 2)));
        assert!(!b.push(&mut buf, &frame(100, 3)));
        // Size: a 2048-byte block takes one full frame, not two.
        let layout = params(2048, 4, 0, 4, 0, 1).out_layout(false, 1 << 20);
        let mut buf = vec![0u8; layout.max];
        let mut b = Builder::new(layout);
        assert!(b.push(&mut buf, &frame(1514, 1)));
        assert!(!b.push(&mut buf, &frame(1514, 2)));
        assert!(b.finish(&mut buf, 0) <= 2048);
    }

    #[test]
    fn insane_params_are_tamed() {
        let l = params(0, 0, 9, 3, 0, 1).out_layout(false, 1 << 20);
        assert_eq!((l.max, l.divisor, l.remainder, l.ndp_align), (2048, 4, 0, 4));
        let l = params(u32::MAX, 6, 0, 1024, 0, 1).out_layout(false, 1 << 20);
        assert_eq!((l.max, l.divisor, l.ndp_align), (0xFFFF, 4, 64));
        assert_eq!(Params::parse(&[0; 27]), Err(Error::Truncated));
        let mut raw = [0u8; 28];
        raw[2] = 2; // NTB32 only
        assert_eq!(Params::parse(&raw), Err(Error::Malformed));
    }

    #[test]
    fn hostile_blocks() {
        // Datagram past the end, a frame too short, an NDP pointing at itself.
        let mut b = vec![0u8; 64];
        put32(&mut b, 0, NTH16_SIGNATURE);
        put16(&mut b, 4, 12);
        put16(&mut b, 8, 64);
        put16(&mut b, 10, 12);
        put32(&mut b, 12, NDP16_NOCRC);
        put16(&mut b, 16, 20);
        put16(&mut b, 18, 12); // next NDP: itself
        put16(&mut b, 20, 40);
        put16(&mut b, 22, 1000); // runs off the end
        put16(&mut b, 24, 40);
        put16(&mut b, 26, 4); // too short
        assert_eq!(parse(&b, MAX_FRAME), Err(Error::TooMany));
        put16(&mut b, 18, 0);
        assert_eq!(parse(&b, MAX_FRAME), Ok(vec![]));
        // NDP at an unaligned or out-of-range offset.
        put16(&mut b, 10, 13);
        assert_eq!(parse(&b, MAX_FRAME), Err(Error::Malformed));
        put16(&mut b, 10, 4);
        assert_eq!(parse(&b, MAX_FRAME), Err(Error::Malformed));
        put16(&mut b, 10, 60);
        assert_eq!(parse(&b, MAX_FRAME), Err(Error::Truncated));
    }
}
