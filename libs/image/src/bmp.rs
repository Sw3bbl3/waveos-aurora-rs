//! Windows bitmaps: uncompressed 1/4/8-bit palettes, 24-bit and 32-bit
//! (with BI_BITFIELDS masks), bottom-up or top-down.

use crate::{Error, Image, MAX_PIXELS};
use alloc::vec;
use alloc::vec::Vec;

fn u16le(d: &[u8], o: usize) -> Result<u16, Error> {
    d.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]])).ok_or(Error::Corrupt)
}

fn u32le(d: &[u8], o: usize) -> Result<u32, Error> {
    d.get(o..o + 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]])).ok_or(Error::Corrupt)
}

/// Extracts the field selected by `mask` from `v`, scaled to 8 bits.
fn field(v: u32, mask: u32) -> u32 {
    if mask == 0 {
        return 0xFF;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let x = (v & mask) >> shift;
    if bits >= 8 {
        x >> (bits - 8)
    } else {
        x * 255 / ((1 << bits) - 1)
    }
}

pub fn decode(d: &[u8]) -> Result<Image, Error> {
    if !d.starts_with(b"BM") {
        return Err(Error::Unsupported);
    }
    let data_off = u32le(d, 10)? as usize;
    let hdr = u32le(d, 14)? as usize;
    if hdr < 40 {
        return Err(Error::Unsupported); // OS/2 core headers
    }
    let width = u32le(d, 18)? as i32;
    let raw_h = u32le(d, 22)? as i32;
    let bpp = u16le(d, 28)?;
    let compression = u32le(d, 30)?;
    if width <= 0 || raw_h == 0 {
        return Err(Error::Corrupt);
    }
    let (w, h) = (width as u32, raw_h.unsigned_abs());
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(Error::TooLarge);
    }
    let top_down = raw_h < 0;
    let (mut rm, mut gm, mut bm, mut am) = (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0);
    match (compression, bpp) {
        (0, 1 | 4 | 8 | 24) => {}
        (0, 32) => am = 0xFF00_0000,
        (3, 16 | 32) => {
            rm = u32le(d, 54)?;
            gm = u32le(d, 58)?;
            bm = u32le(d, 62)?;
            am = if hdr >= 56 { u32le(d, 66)? } else { 0 };
        }
        (0, 16) => (rm, gm, bm) = (0x7C00, 0x03E0, 0x001F),
        _ => return Err(Error::Unsupported),
    }
    let mut palette: Vec<u32> = Vec::new();
    if bpp <= 8 {
        let colors = match u32le(d, 46)? {
            0 => 1 << bpp,
            n => n.min(256),
        } as usize;
        let at = 14 + hdr;
        for i in 0..colors {
            let c = u32le(d, at + 4 * i)?;
            palette.push(0xFF00_0000 | c & 0x00FF_FFFF);
        }
    }
    let stride = (w as usize * bpp as usize).div_ceil(32) * 4;
    let mut img = Image { width: w, height: h, pixels: vec![0; w as usize * h as usize] };
    let mut all_clear = am != 0;
    for row in 0..h as usize {
        let src = d.get(data_off + row * stride..data_off + (row + 1) * stride).ok_or(Error::Corrupt)?;
        let y = if top_down { row } else { h as usize - 1 - row };
        for x in 0..w as usize {
            let px = match bpp {
                1 | 4 | 8 => {
                    let per = 8 / bpp as usize;
                    let shift = 8 - bpp as usize * (x % per + 1);
                    let idx = (src[x / per] >> shift) as usize & ((1 << bpp) - 1);
                    palette.get(idx).copied().unwrap_or(0xFF00_0000)
                }
                24 => 0xFF00_0000 | (src[3 * x + 2] as u32) << 16 | (src[3 * x + 1] as u32) << 8 | src[3 * x] as u32,
                _ => {
                    let v = if bpp == 16 {
                        u16::from_le_bytes([src[2 * x], src[2 * x + 1]]) as u32
                    } else {
                        u32::from_le_bytes([src[4 * x], src[4 * x + 1], src[4 * x + 2], src[4 * x + 3]])
                    };
                    let a = if am != 0 { field(v, am) } else { 0xFF };
                    if a != 0 {
                        all_clear = false;
                    }
                    a << 24 | field(v, rm) << 16 | field(v, gm) << 8 | field(v, bm)
                }
            };
            img.pixels[y * w as usize + x] = px;
        }
    }
    // Many writers leave the alpha byte of 32-bit bitmaps zero: treat as opaque.
    if all_clear {
        for p in &mut img.pixels {
            *p |= 0xFF00_0000;
        }
    }
    Ok(img)
}
