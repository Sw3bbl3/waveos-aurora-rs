//! LZ4 block compression (the block format of the LZ4 specification),
//! without allocation, for the kernel's compressed memory (`mm::swap`).
//!
//! A block is a series of sequences: a token (four bits of literal length,
//! four bits of match length less 4), longer lengths continued in bytes of
//! 255, the literals, then a two-byte little-endian offset back into what
//! is already decoded. The last sequence has literals only; the last five
//! bytes are always literals and the last match starts at least twelve
//! bytes before the end, as the specification requires, so other LZ4
//! decoders read what this writes.
//!
//! [`compress`] is the fast greedy compressor (one hash probe per
//! position, skipping faster through data that doesn't match). It works on
//! inputs up to 64 KiB, which is all the kernel needs (a page).
//! [`decompress`] checks every length and offset against both buffers: any
//! input, however corrupt, gives an error or some output, never a read or
//! write out of bounds (the host tests mutate valid blocks to make sure).

#![no_std]

/// The largest input [`compress`] takes (offsets are 16 bits).
pub const MAX_INPUT: usize = 65535;
/// Entries in the compressor's hash table (2^12; 8 KiB of `u16`).
pub const HASH_LOG: u32 = 12;
pub const TABLE: usize = 1 << HASH_LOG;

const MIN_MATCH: usize = 4;
/// The last five bytes are literals.
const LAST_LITERALS: usize = 5;
/// The last match starts at least this far from the end.
const MF_LIMIT: usize = 12;

/// The most a block of `len` bytes can take compressed (incompressible
/// data grows by a byte per 255, plus a token).
pub const fn max_compressed(len: usize) -> usize {
    len + len / 255 + 16
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The block ends in the middle of a sequence.
    Truncated,
    /// An offset of zero, or one reaching before the start of the output.
    BadOffset,
    /// The output would not fit in the buffer given.
    OutputFull,
}

fn read32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn hash(v: u32) -> usize {
    (v.wrapping_mul(2_654_435_761) >> (32 - HASH_LOG)) as usize
}

/// Writes into `dst` from `at`, failing (None) when it is full.
struct Out<'a> {
    dst: &'a mut [u8],
    at: usize,
}

impl Out<'_> {
    fn byte(&mut self, b: u8) -> Option<()> {
        *self.dst.get_mut(self.at)? = b;
        self.at += 1;
        Some(())
    }

    fn bytes(&mut self, b: &[u8]) -> Option<()> {
        self.dst.get_mut(self.at..self.at + b.len())?.copy_from_slice(b);
        self.at += b.len();
        Some(())
    }

    /// The rest of a length past the token's 15: bytes of 255, then the remainder.
    fn length(&mut self, mut n: usize) -> Option<()> {
        while n >= 255 {
            self.byte(255)?;
            n -= 255;
        }
        self.byte(n as u8)
    }

    /// One sequence: `literals`, then (unless it is the last) a match of
    /// `len` bytes `offset` back.
    fn sequence(&mut self, literals: &[u8], m: Option<(usize, usize)>) -> Option<()> {
        let lit = literals.len();
        let ml = m.map_or(0, |(_, len)| len - MIN_MATCH);
        self.byte(((lit.min(15) as u8) << 4) | ml.min(15) as u8)?;
        if lit >= 15 {
            self.length(lit - 15)?;
        }
        self.bytes(literals)?;
        if let Some((offset, _)) = m {
            self.bytes(&(offset as u16).to_le_bytes())?;
            if ml >= 15 {
                self.length(ml - 15)?;
            }
        }
        Some(())
    }
}

/// Compresses `src` (at most [`MAX_INPUT`] bytes) into `dst`, using
/// `table` as scratch. Returns the compressed length, or None if it does
/// not fit in `dst` (a `dst` of [`max_compressed`] bytes always fits; a
/// smaller one is how callers say "only if it shrinks this much").
pub fn compress(src: &[u8], dst: &mut [u8], table: &mut [u16; TABLE]) -> Option<usize> {
    if src.len() > MAX_INPUT {
        return None;
    }
    let mut out = Out { dst, at: 0 };
    let len = src.len();
    let mut anchor = 0;
    if len > MF_LIMIT {
        table.fill(0);
        let match_limit = len - MF_LIMIT;
        let extend_limit = len - LAST_LITERALS;
        let mut ip = 1;
        'outer: while ip < match_limit {
            // Find a match: one probe per position, stepping further the
            // longer nothing matches (incompressible data goes fast).
            let mut misses = 0usize;
            let cand = loop {
                if ip >= match_limit {
                    break 'outer;
                }
                let v = read32(src, ip);
                let h = hash(v);
                let cand = table[h] as usize;
                table[h] = ip as u16;
                if cand < ip && ip - cand <= MAX_INPUT && read32(src, cand) == v {
                    break cand;
                }
                misses += 1;
                ip += 1 + (misses >> 5);
            };
            // Extend backwards over literals that match too, then forwards.
            let (mut ip0, mut c0) = (ip, cand);
            while ip0 > anchor && c0 > 0 && src[ip0 - 1] == src[c0 - 1] {
                ip0 -= 1;
                c0 -= 1;
            }
            let mut mlen = MIN_MATCH + (ip - ip0);
            while ip0 + mlen < extend_limit && src[c0 + mlen] == src[ip0 + mlen] {
                mlen += 1;
            }
            out.sequence(&src[anchor..ip0], Some((ip0 - c0, mlen)))?;
            ip = ip0 + mlen;
            anchor = ip;
            // Positions inside the match are worth remembering for the next.
            if ip < match_limit {
                table[hash(read32(src, ip - 2))] = (ip - 2) as u16;
            }
        }
    }
    out.sequence(&src[anchor..], None)?;
    Some(out.at)
}

/// Decompresses the block `src` into `dst`; returns the decompressed
/// length. Corrupt input gives an error (or wrong bytes), never a panic.
pub fn decompress(src: &[u8], dst: &mut [u8]) -> Result<usize, Error> {
    let (mut ip, mut op) = (0usize, 0usize);
    let next = |ip: &mut usize| -> Result<u8, Error> {
        let b = *src.get(*ip).ok_or(Error::Truncated)?;
        *ip += 1;
        Ok(b)
    };
    let length = |ip: &mut usize, mut n: usize| -> Result<usize, Error> {
        loop {
            let b = next(ip)?;
            n = n.checked_add(b as usize).ok_or(Error::OutputFull)?;
            if b != 255 {
                return Ok(n);
            }
        }
    };
    loop {
        let token = next(&mut ip)?;
        let mut lit = (token >> 4) as usize;
        if lit == 15 {
            lit = length(&mut ip, lit)?;
        }
        let from = src.get(ip..ip.checked_add(lit).ok_or(Error::Truncated)?).ok_or(Error::Truncated)?;
        let to = dst.get_mut(op..op.checked_add(lit).ok_or(Error::OutputFull)?).ok_or(Error::OutputFull)?;
        to.copy_from_slice(from);
        ip += lit;
        op += lit;
        if ip == src.len() {
            // The last sequence: literals only.
            return Ok(op);
        }
        let offset = u16::from_le_bytes([next(&mut ip)?, next(&mut ip)?]) as usize;
        if offset == 0 || offset > op {
            return Err(Error::BadOffset);
        }
        let mut mlen = (token & 15) as usize;
        if mlen == 15 {
            mlen = length(&mut ip, mlen)?;
        }
        mlen += MIN_MATCH;
        let end = op.checked_add(mlen).filter(|&e| e <= dst.len()).ok_or(Error::OutputFull)?;
        let start = op - offset;
        if offset >= mlen {
            dst.copy_within(start..start + mlen, op);
        } else {
            // Overlapping: the match repeats what it is copying.
            for i in 0..mlen {
                dst[op + i] = dst[start + i];
            }
        }
        op = end;
    }
}

#[cfg(test)]
mod tests;
