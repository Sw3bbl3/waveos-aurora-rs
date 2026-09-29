//! PNG: every colour type and bit depth, palettes with transparency,
//! colour-key transparency, all five filters and Adam7 interlacing.

use crate::{Error, Image, MAX_PIXELS};
use alloc::vec;
use alloc::vec::Vec;

pub const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

const GRAY: u8 = 0;
const RGB: u8 = 2;
const PALETTE: u8 = 3;
const GRAY_ALPHA: u8 = 4;
const RGBA: u8 = 6;

fn crc_table() -> [u32; 256] {
    let mut t = [0u32; 256];
    for (n, e) in t.iter_mut().enumerate() {
        let mut c = n as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *e = c;
    }
    t
}

fn crc(table: &[u32; 256], parts: &[&[u8]]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for p in parts {
        for &b in *p {
            c = table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8);
        }
    }
    c ^ 0xFFFF_FFFF
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

struct Header {
    width: u32,
    height: u32,
    depth: u8,
    color: u8,
    interlaced: bool,
}

impl Header {
    fn channels(&self) -> usize {
        match self.color {
            GRAY | PALETTE => 1,
            GRAY_ALPHA => 2,
            RGB => 3,
            _ => 4,
        }
    }

    fn bits_per_pixel(&self) -> usize {
        self.channels() * self.depth as usize
    }

    /// Bytes in one filtered row of a `w`-pixel-wide image (without the filter byte).
    fn row_bytes(&self, w: u32) -> usize {
        (w as usize * self.bits_per_pixel()).div_ceil(8)
    }
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if !data.starts_with(&SIGNATURE) {
        return Err(Error::Unsupported);
    }
    let table = crc_table();
    let mut pos = 8;
    let mut header: Option<Header> = None;
    let mut palette: Vec<u32> = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    let mut idat: Vec<u8> = Vec::new();
    loop {
        if pos + 12 > data.len() {
            return Err(Error::Corrupt);
        }
        let len = be32(&data[pos..]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let body = data.get(pos + 8..pos + 8 + len).ok_or(Error::Corrupt)?;
        let stored = be32(data.get(pos + 8 + len..pos + 12 + len).ok_or(Error::Corrupt)?);
        if crc(&table, &[kind, body]) != stored {
            return Err(Error::Corrupt);
        }
        pos += 12 + len;
        match kind {
            b"IHDR" => {
                if body.len() < 13 {
                    return Err(Error::Corrupt);
                }
                let h = Header {
                    width: be32(body),
                    height: be32(&body[4..]),
                    depth: body[8],
                    color: body[9],
                    interlaced: body[12] == 1,
                };
                let valid_depth = match h.color {
                    GRAY => matches!(h.depth, 1 | 2 | 4 | 8 | 16),
                    PALETTE => matches!(h.depth, 1 | 2 | 4 | 8),
                    RGB | GRAY_ALPHA | RGBA => matches!(h.depth, 8 | 16),
                    _ => false,
                };
                if !valid_depth || body[10] != 0 || body[11] != 0 || body[12] > 1 || h.width == 0 || h.height == 0 {
                    return Err(Error::Unsupported);
                }
                if h.width as u64 * h.height as u64 > MAX_PIXELS {
                    return Err(Error::TooLarge);
                }
                header = Some(h);
            }
            b"PLTE" => {
                palette = body
                    .chunks_exact(3)
                    .map(|c| 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32)
                    .collect();
            }
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {
                // Unknown critical chunks (uppercase first letter) can't be ignored.
                if kind[0] & 0x20 == 0 {
                    return Err(Error::Unsupported);
                }
            }
        }
    }
    let h = header.ok_or(Error::Corrupt)?;
    if h.color == PALETTE {
        if palette.is_empty() {
            return Err(Error::Corrupt);
        }
        for (i, &a) in trns.iter().enumerate().take(palette.len()) {
            palette[i] = (palette[i] & 0x00FF_FFFF) | (a as u32) << 24;
        }
    }

    // Size of the decompressed stream: one filter byte per row of every pass.
    let expected: usize =
        passes(&h).iter().filter(|p| p.w > 0 && p.h > 0).map(|p| (h.row_bytes(p.w) + 1) * p.h as usize).sum();
    let raw = miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&idat, expected).map_err(|_| Error::Corrupt)?;
    if raw.len() < expected {
        return Err(Error::Corrupt);
    }

    let mut out = Image { width: h.width, height: h.height, pixels: vec![0; h.width as usize * h.height as usize] };
    let key = colour_key(&h, &trns);
    let mut off = 0;
    for p in passes(&h) {
        if p.w == 0 || p.h == 0 {
            continue;
        }
        let rb = h.row_bytes(p.w);
        let bpp = h.bits_per_pixel().div_ceil(8).max(1);
        let mut prev = vec![0u8; rb];
        let mut cur = vec![0u8; rb];
        for row in 0..p.h {
            let filter = raw[off];
            cur.copy_from_slice(&raw[off + 1..off + 1 + rb]);
            off += rb + 1;
            unfilter(filter, &mut cur, &prev, bpp)?;
            let y = p.y0 + row * p.dy;
            for i in 0..p.w {
                let x = p.x0 + i * p.dx;
                out.pixels[(y * h.width + x) as usize] = pixel(&h, &cur, i as usize, &palette, key);
            }
            core::mem::swap(&mut prev, &mut cur);
        }
    }
    Ok(out)
}

/// One Adam7 pass (or the whole image): its size and where its pixels go.
struct Pass {
    w: u32,
    h: u32,
    x0: u32,
    y0: u32,
    dx: u32,
    dy: u32,
}

fn passes(h: &Header) -> Vec<Pass> {
    if !h.interlaced {
        return vec![Pass { w: h.width, h: h.height, x0: 0, y0: 0, dx: 1, dy: 1 }];
    }
    const ADAM7: [(u32, u32, u32, u32); 7] =
        [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)];
    ADAM7
        .iter()
        .map(|&(x0, y0, dx, dy)| Pass {
            w: (h.width + dx - x0 - 1) / dx,
            h: (h.height + dy - y0 - 1) / dy,
            x0,
            y0,
            dx,
            dy,
        })
        .collect()
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = ((p - a as i16).abs(), (p - b as i16).abs(), (p - c as i16).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

fn unfilter(filter: u8, cur: &mut [u8], prev: &[u8], bpp: usize) -> Result<(), Error> {
    match filter {
        0 => {}
        1 => {
            for i in bpp..cur.len() {
                cur[i] = cur[i].wrapping_add(cur[i - bpp]);
            }
        }
        2 => {
            for i in 0..cur.len() {
                cur[i] = cur[i].wrapping_add(prev[i]);
            }
        }
        3 => {
            for i in 0..cur.len() {
                let left = if i >= bpp { cur[i - bpp] as u16 } else { 0 };
                cur[i] = cur[i].wrapping_add(((left + prev[i] as u16) / 2) as u8);
            }
        }
        4 => {
            for i in 0..cur.len() {
                let (a, c) = if i >= bpp { (cur[i - bpp], prev[i - bpp]) } else { (0, 0) };
                cur[i] = cur[i].wrapping_add(paeth(a, prev[i], c));
            }
        }
        _ => return Err(Error::Corrupt),
    }
    Ok(())
}

/// The tRNS colour key of grey or RGB images, as full-depth samples.
#[derive(Clone, Copy)]
enum Key {
    None,
    Gray(u16),
    Rgb(u16, u16, u16),
}

fn colour_key(h: &Header, trns: &[u8]) -> Key {
    let s = |i: usize| u16::from_be_bytes([trns[i], trns[i + 1]]);
    match h.color {
        GRAY if trns.len() >= 2 => Key::Gray(s(0)),
        RGB if trns.len() >= 6 => Key::Rgb(s(0), s(2), s(4)),
        _ => Key::None,
    }
}

/// Sample `i` (in channel units) of a row at the image's bit depth.
fn sample(row: &[u8], depth: u8, i: usize) -> u16 {
    match depth {
        16 => u16::from_be_bytes([row[2 * i], row[2 * i + 1]]),
        8 => row[i] as u16,
        d => {
            let per = 8 / d as usize;
            let byte = row[i / per];
            let shift = 8 - d as usize * (i % per + 1);
            (byte >> shift) as u16 & ((1 << d) - 1)
        }
    }
}

/// A sample scaled to 8 bits.
fn to8(v: u16, depth: u8) -> u32 {
    match depth {
        16 => (v >> 8) as u32,
        8 => v as u32,
        d => (v as u32 * 255) / ((1 << d) - 1),
    }
}

fn pixel(h: &Header, row: &[u8], i: usize, palette: &[u32], key: Key) -> u32 {
    let d = h.depth;
    match h.color {
        GRAY => {
            let v = sample(row, d, i);
            let g = to8(v, d);
            let a = if matches!(key, Key::Gray(k) if k == v) { 0 } else { 0xFF };
            a << 24 | g << 16 | g << 8 | g
        }
        GRAY_ALPHA => {
            let g = to8(sample(row, d, 2 * i), d);
            let a = to8(sample(row, d, 2 * i + 1), d);
            a << 24 | g << 16 | g << 8 | g
        }
        RGB => {
            let (r, g, b) = (sample(row, d, 3 * i), sample(row, d, 3 * i + 1), sample(row, d, 3 * i + 2));
            let a = if matches!(key, Key::Rgb(kr, kg, kb) if (kr, kg, kb) == (r, g, b)) { 0 } else { 0xFF };
            a << 24 | to8(r, d) << 16 | to8(g, d) << 8 | to8(b, d)
        }
        PALETTE => palette.get(sample(row, d, i) as usize).copied().unwrap_or(0xFF00_0000),
        _ => {
            let c = |k: usize| to8(sample(row, d, 4 * i + k), d);
            c(3) << 24 | c(0) << 16 | c(1) << 8 | c(2)
        }
    }
}

// ---------------------------------------------------------------- encoder

fn chunk(out: &mut Vec<u8>, table: &[u32; 256], kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
    out.extend_from_slice(&crc(table, &[kind, body]).to_be_bytes());
}

/// Encodes 8-bit RGB (opaque images) or RGBA. Each row gets the filter that
/// minimises the sum of absolute residuals (the libpng heuristic).
pub fn encode(img: &Image) -> Vec<u8> {
    let opaque = img.is_opaque();
    let channels = if opaque { 3 } else { 4 };
    let (w, h) = (img.width as usize, img.height as usize);
    let rb = w * channels;
    let mut raw = Vec::with_capacity((rb + 1) * h);
    let mut prev = vec![0u8; rb];
    let mut cur = vec![0u8; rb];
    let mut candidate = vec![0u8; rb];
    let mut best = vec![0u8; rb];
    for y in 0..h {
        for x in 0..w {
            let p = img.pixels[y * w + x];
            let o = x * channels;
            cur[o] = (p >> 16) as u8;
            cur[o + 1] = (p >> 8) as u8;
            cur[o + 2] = p as u8;
            if !opaque {
                cur[o + 3] = (p >> 24) as u8;
            }
        }
        let mut best_score = u64::MAX;
        let mut best_filter = 0;
        for f in 0..5u8 {
            for i in 0..rb {
                let (a, b, c) =
                    if i >= channels { (cur[i - channels], prev[i], prev[i - channels]) } else { (0, prev[i], 0) };
                let pred = match f {
                    0 => 0,
                    1 => a,
                    2 => b,
                    3 => ((a as u16 + b as u16) / 2) as u8,
                    _ => paeth(a, b, c),
                };
                candidate[i] = cur[i].wrapping_sub(pred);
            }
            let score: u64 = candidate.iter().map(|&v| (v as i8).unsigned_abs() as u64).sum();
            if score < best_score {
                best_score = score;
                best_filter = f;
                best.copy_from_slice(&candidate);
            }
        }
        raw.push(best_filter);
        raw.extend_from_slice(&best);
        core::mem::swap(&mut prev, &mut cur);
    }
    let table = crc_table();
    let mut out = Vec::from(SIGNATURE);
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&img.width.to_be_bytes());
    ihdr.extend_from_slice(&img.height.to_be_bytes());
    ihdr.extend_from_slice(&[8, if opaque { RGB } else { RGBA }, 0, 0, 0]);
    chunk(&mut out, &table, b"IHDR", &ihdr);
    chunk(&mut out, &table, b"IDAT", &miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6));
    chunk(&mut out, &table, b"IEND", &[]);
    out
}
