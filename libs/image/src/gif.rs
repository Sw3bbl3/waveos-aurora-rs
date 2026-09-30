//! GIF decoding: the first frame (GIF87a/89a), with its transparency and
//! interlacing, composed onto the logical screen.

use crate::{Error, Image, MAX_PIXELS};
use alloc::vec;
use alloc::vec::Vec;

fn u16le(d: &[u8], i: usize) -> Result<usize, Error> {
    Ok(*d.get(i).ok_or(Error::Corrupt)? as usize | (*d.get(i + 1).ok_or(Error::Corrupt)? as usize) << 8)
}

fn palette(d: &[u8], at: usize, size: usize) -> Result<Vec<u32>, Error> {
    let bytes = d.get(at..at + size * 3).ok_or(Error::Corrupt)?;
    Ok(bytes.chunks(3).map(|c| 0xFF00_0000 | (c[0] as u32) << 16 | (c[1] as u32) << 8 | c[2] as u32).collect())
}

pub fn decode(d: &[u8]) -> Result<Image, Error> {
    if !(d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a")) {
        return Err(Error::Unsupported);
    }
    let (sw, sh) = (u16le(d, 6)?, u16le(d, 8)?);
    let flags = *d.get(10).ok_or(Error::Corrupt)?;
    let mut i = 13;
    let mut global = Vec::new();
    if flags & 0x80 != 0 {
        let size = 2 << (flags & 7);
        global = palette(d, i, size)?;
        i += size * 3;
    }
    let mut transparent: Option<u8> = None;
    loop {
        match *d.get(i).ok_or(Error::Corrupt)? {
            0x21 => {
                // Extension: graphic control carries transparency.
                let label = *d.get(i + 1).ok_or(Error::Corrupt)?;
                i += 2;
                if label == 0xF9 && d.get(i) == Some(&4) {
                    let packed = *d.get(i + 1).ok_or(Error::Corrupt)?;
                    if packed & 1 != 0 {
                        transparent = Some(*d.get(i + 4).ok_or(Error::Corrupt)?);
                    }
                }
                // Skip the sub-blocks.
                loop {
                    let n = *d.get(i).ok_or(Error::Corrupt)? as usize;
                    i += 1 + n;
                    if n == 0 {
                        break;
                    }
                }
            }
            0x2C => break,
            0x3B => return Err(Error::Corrupt),
            _ => return Err(Error::Corrupt),
        }
    }
    // Image descriptor.
    let (fx, fy, fw, fh) = (u16le(d, i + 1)?, u16le(d, i + 3)?, u16le(d, i + 5)?, u16le(d, i + 7)?);
    let iflags = *d.get(i + 9).ok_or(Error::Corrupt)?;
    i += 10;
    let colors = if iflags & 0x80 != 0 {
        let size = 2 << (iflags & 7);
        let p = palette(d, i, size)?;
        i += size * 3;
        p
    } else {
        global
    };
    let (sw, sh) = if sw == 0 || sh == 0 { (fx + fw, fy + fh) } else { (sw, sh) };
    if sw == 0 || sh == 0 || fw == 0 || fh == 0 {
        return Err(Error::Corrupt);
    }
    if (sw * sh) as u64 > MAX_PIXELS || (fw * fh) as u64 > MAX_PIXELS {
        return Err(Error::TooLarge);
    }
    let min_code = *d.get(i).ok_or(Error::Corrupt)? as u32;
    i += 1;
    if !(1..=11).contains(&min_code) {
        return Err(Error::Corrupt);
    }
    // Gather the data sub-blocks.
    let mut data = Vec::new();
    loop {
        let n = *d.get(i).unwrap_or(&0) as usize;
        i += 1;
        if n == 0 {
            break;
        }
        data.extend_from_slice(d.get(i..i + n).ok_or(Error::Corrupt)?);
        i += n;
    }
    let indices = lzw(&data, min_code, fw * fh);
    // Row order: interlaced frames store rows in four passes.
    let rows: Vec<usize> = if iflags & 0x40 != 0 {
        (0..fh).step_by(8).chain((4..fh).step_by(8)).chain((2..fh).step_by(4)).chain((1..fh).step_by(2)).collect()
    } else {
        (0..fh).collect()
    };
    let mut pixels = vec![0u32; sw * sh];
    for (n, &row) in rows.iter().enumerate() {
        let y = fy + row;
        if y >= sh {
            continue;
        }
        for col in 0..fw {
            let x = fx + col;
            let Some(&idx) = indices.get(n * fw + col) else { break };
            if x >= sw || Some(idx) == transparent {
                continue;
            }
            pixels[y * sw + x] = *colors.get(idx as usize).unwrap_or(&0xFF00_0000);
        }
    }
    Ok(Image { width: sw as u32, height: sh as u32, pixels })
}

/// Variable-length-code LZW, least significant bit first, up to `limit` outputs.
fn lzw(data: &[u8], min_code: u32, limit: usize) -> Vec<u8> {
    let clear = 1u32 << min_code;
    let eoi = clear + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    for c in 0..clear {
        suffix[c as usize] = c as u8;
        first[c as usize] = c as u8;
    }
    let mut out = Vec::with_capacity(limit);
    let mut size = min_code + 1;
    let mut next = eoi + 1;
    let mut old: Option<u32> = None;
    let (mut acc, mut nbits, mut pos) = (0u32, 0u32, 0usize);
    let mut stack = Vec::with_capacity(4096);
    while out.len() < limit {
        while nbits < size {
            let Some(&b) = data.get(pos) else { return out };
            acc |= (b as u32) << nbits;
            nbits += 8;
            pos += 1;
        }
        let code = acc & ((1 << size) - 1);
        acc >>= size;
        nbits -= size;
        if code == clear {
            size = min_code + 1;
            next = eoi + 1;
            old = None;
            continue;
        }
        if code == eoi {
            break;
        }
        let Some(prev) = old else {
            if code >= clear {
                break;
            }
            out.push(code as u8);
            old = Some(code);
            continue;
        };
        let (emit, first_char) = if code < next {
            (code, first[code as usize])
        } else if code == next {
            (prev, first[prev as usize])
        } else {
            break;
        };
        // Unwind the string for `emit`.
        stack.clear();
        let mut c = emit;
        loop {
            stack.push(suffix[c as usize]);
            if c < clear {
                break;
            }
            c = prefix[c as usize] as u32;
        }
        out.extend(stack.iter().rev());
        if code == next {
            out.push(first_char);
        }
        if next < 4096 {
            prefix[next as usize] = prev as u16;
            suffix[next as usize] = first_char;
            first[next as usize] = first[prev as usize];
            next += 1;
            if next == 1 << size && size < 12 {
                size += 1;
            }
        }
        old = Some(code);
    }
    out.truncate(limit);
    out
}
