//! JPEG decoding: baseline and progressive Huffman-coded JPEGs (SOF0/1/2),
//! 8-bit, greyscale, YCbCr, RGB, CMYK and YCCK, any sampling factors,
//! restart intervals. Integer IDCT (the classic 12-bit fixed-point one).
//!
//! Coefficients for the whole image are collected first (which progressive
//! images need anyway), then each block is dequantised, transformed and
//! colour-converted.

use crate::{Error, Image, MAX_PIXELS};
use alloc::vec;
use alloc::vec::Vec;

/// Zig-zag order: the natural index of the k-th coefficient in a scan.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21,
    28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54,
    47, 55, 62, 63,
];

#[derive(Clone, Default)]
struct Huffman {
    /// Fast lookup on the next 9 bits: (length, symbol); length 0 = slow path.
    fast: Vec<(u8, u8)>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    symbols: Vec<u8>,
}

const FAST_BITS: u32 = 9;

/// The standard tables (ITU T.81 Annex K), for streams without DHT (Motion JPEG).
const STD_DC_LUMA: ([u8; 16], &[u8]) =
    ([0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
const STD_DC_CHROMA: ([u8; 16], &[u8]) =
    ([0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0], &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
const STD_AC_LUMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d],
    &[
        0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71,
        0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0, 0x24, 0x33, 0x62, 0x72,
        0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x34, 0x35, 0x36, 0x37,
        0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59,
        0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83,
        0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3,
        0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
        0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
        0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);
const STD_AC_CHROMA: ([u8; 16], &[u8]) = (
    [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77],
    &[
        0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22,
        0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0, 0x15, 0x62, 0x72, 0xd1,
        0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26, 0x27, 0x28, 0x29, 0x2a, 0x35, 0x36,
        0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58,
        0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a,
        0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a,
        0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba,
        0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
        0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8, 0xf9, 0xfa,
    ],
);

impl Huffman {
    fn new(counts: &[u8; 16], symbols: &[u8]) -> Result<Huffman, Error> {
        let mut h = Huffman { fast: vec![(0, 0); 1 << FAST_BITS], symbols: symbols.to_vec(), ..Default::default() };
        let mut code = 0i32;
        let mut k = 0i32;
        for len in 1..=16 {
            let n = counts[len - 1] as i32;
            h.valptr[len] = k;
            h.mincode[len] = code;
            if n > 0 {
                h.maxcode[len] = code + n - 1;
                // Fill the fast table.
                if len as u32 <= FAST_BITS {
                    for i in 0..n {
                        let c = (code + i) as u32;
                        let shift = FAST_BITS - len as u32;
                        let sym = *symbols.get((k + i) as usize).ok_or(Error::Corrupt)?;
                        for fill in 0..(1u32 << shift) {
                            h.fast[((c << shift) | fill) as usize] = (len as u8, sym);
                        }
                    }
                }
            } else {
                h.maxcode[len] = -1;
            }
            code += n;
            k += n;
            if code > (1 << len) {
                return Err(Error::Corrupt);
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        if k as usize > symbols.len() {
            return Err(Error::Corrupt);
        }
        Ok(h)
    }
}

/// Reads entropy-coded bits, undoing 0xFF00 stuffing and stopping at markers.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    count: u32,
    /// A marker was reached: pad with zeros.
    marker: bool,
}

impl<'a> Bits<'a> {
    fn fill(&mut self) {
        while self.count <= 24 {
            let mut byte = 0;
            if !self.marker && self.pos < self.data.len() {
                byte = self.data[self.pos];
                if byte == 0xFF {
                    let next = *self.data.get(self.pos + 1).unwrap_or(&0);
                    if next == 0x00 {
                        self.pos += 2;
                    } else {
                        self.marker = true;
                        byte = 0;
                    }
                } else {
                    self.pos += 1;
                }
            }
            self.acc |= (byte as u32) << (24 - self.count);
            self.count += 8;
        }
    }

    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        self.fill();
        let v = self.acc >> (32 - n);
        self.acc <<= n;
        self.count -= n;
        v
    }

    fn bit(&mut self) -> bool {
        self.bits(1) != 0
    }

    fn decode(&mut self, h: &Huffman) -> Result<u8, Error> {
        if h.fast.is_empty() {
            return Err(Error::Corrupt);
        }
        self.fill();
        let (len, sym) = h.fast[(self.acc >> (32 - FAST_BITS)) as usize];
        if len > 0 {
            self.acc <<= len;
            self.count -= len as u32;
            return Ok(sym);
        }
        let mut code = 0i32;
        for len in 1..=16 {
            code = (code << 1) | self.bits(1) as i32;
            if code <= h.maxcode[len] {
                let i = h.valptr[len] + code - h.mincode[len];
                return h.symbols.get(i as usize).copied().ok_or(Error::Corrupt);
            }
        }
        Err(Error::Corrupt)
    }

    /// A value of `n` bits, sign-extended the JPEG way.
    fn receive_extend(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        let v = self.bits(n) as i32;
        if v < 1 << (n - 1) {
            v - (1 << n) + 1
        } else {
            v
        }
    }

    /// Skips to the byte after the next RSTn marker.
    fn restart(&mut self) {
        self.acc = 0;
        self.count = 0;
        self.marker = false;
        while self.pos + 1 < self.data.len() {
            if self.data[self.pos] == 0xFF && (0xD0..=0xD7).contains(&self.data[self.pos + 1]) {
                self.pos += 2;
                return;
            }
            self.pos += 1;
        }
    }
}

#[derive(Clone, Default)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    /// Blocks per line / column, padded to whole MCUs.
    bw: usize,
    bh: usize,
    coefs: Vec<i16>,
    dc_pred: i32,
    dc_table: usize,
    ac_table: usize,
}

struct Decoder<'a> {
    data: &'a [u8],
    qt: [[u16; 64]; 4],
    dc: [Huffman; 4],
    ac: [Huffman; 4],
    comps: Vec<Component>,
    width: usize,
    height: usize,
    progressive: bool,
    hmax: usize,
    vmax: usize,
    mcux: usize,
    mcuy: usize,
    restart_interval: usize,
    eobrun: u32,
    adobe: Option<u8>,
    frame: bool,
}

fn u16_at(d: &[u8], i: usize) -> Result<usize, Error> {
    Ok(((*d.get(i).ok_or(Error::Corrupt)? as usize) << 8) | *d.get(i + 1).ok_or(Error::Corrupt)? as usize)
}

pub fn decode(data: &[u8]) -> Result<Image, Error> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return Err(Error::Unsupported);
    }
    let mut d = Decoder {
        data,
        qt: [[1; 64]; 4],
        dc: Default::default(),
        ac: Default::default(),
        comps: Vec::new(),
        width: 0,
        height: 0,
        progressive: false,
        hmax: 1,
        vmax: 1,
        mcux: 0,
        mcuy: 0,
        restart_interval: 0,
        eobrun: 0,
        adobe: None,
        frame: false,
    };
    let std = |t: ([u8; 16], &[u8])| Huffman::new(&t.0, t.1);
    d.dc = [std(STD_DC_LUMA)?, std(STD_DC_CHROMA)?, Huffman::default(), Huffman::default()];
    d.ac = [std(STD_AC_LUMA)?, std(STD_AC_CHROMA)?, Huffman::default(), Huffman::default()];
    let mut i = 2;
    loop {
        // Find the next marker.
        while i < data.len() && data[i] != 0xFF {
            i += 1;
        }
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        let Some(&marker) = data.get(i) else { break };
        i += 1;
        match marker {
            0xD8 | 0x01 | 0xD0..=0xD7 => continue,
            0xD9 => break,
            _ => {}
        }
        let len = u16_at(data, i)?;
        let seg = data.get(i + 2..i + len).ok_or(Error::Corrupt)?;
        match marker {
            0xC0 | 0xC1 | 0xC2 => {
                d.progressive = marker == 0xC2;
                d.frame(seg)?;
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Err(Error::Unsupported),
            0xC4 => d.huffman(seg)?,
            0xDB => d.quant(seg)?,
            0xDD => d.restart_interval = u16_at(seg, 0)?,
            0xEE if seg.starts_with(b"Adobe") && seg.len() >= 12 => d.adobe = Some(seg[11]),
            0xDA => {
                let end = d.scan(seg, i + len)?;
                i = end;
                continue;
            }
            _ => {}
        }
        i += len;
    }
    if !d.frame {
        return Err(Error::Corrupt);
    }
    d.output()
}

impl<'a> Decoder<'a> {
    fn frame(&mut self, s: &[u8]) -> Result<(), Error> {
        if *s.first().ok_or(Error::Corrupt)? != 8 {
            return Err(Error::Unsupported);
        }
        self.height = u16_at(s, 1)?;
        self.width = u16_at(s, 3)?;
        let n = *s.get(5).ok_or(Error::Corrupt)? as usize;
        if self.width == 0 || self.height == 0 || !matches!(n, 1 | 3 | 4) {
            return Err(Error::Unsupported);
        }
        if (self.width * self.height) as u64 > MAX_PIXELS {
            return Err(Error::TooLarge);
        }
        self.comps.clear();
        for k in 0..n {
            let c = s.get(6 + k * 3..9 + k * 3).ok_or(Error::Corrupt)?;
            let (h, v) = ((c[1] >> 4) as usize, (c[1] & 15) as usize);
            if !(1..=4).contains(&h) || !(1..=4).contains(&v) || c[2] > 3 {
                return Err(Error::Corrupt);
            }
            self.comps.push(Component { id: c[0], h, v, tq: c[2] as usize, ..Default::default() });
        }
        self.hmax = self.comps.iter().map(|c| c.h).max().unwrap();
        self.vmax = self.comps.iter().map(|c| c.v).max().unwrap();
        self.mcux = self.width.div_ceil(8 * self.hmax);
        self.mcuy = self.height.div_ceil(8 * self.vmax);
        for c in &mut self.comps {
            c.bw = self.mcux * c.h;
            c.bh = self.mcuy * c.v;
            c.coefs = vec![0; c.bw * c.bh * 64];
        }
        self.frame = true;
        Ok(())
    }

    fn huffman(&mut self, mut s: &[u8]) -> Result<(), Error> {
        while !s.is_empty() {
            let tc = s[0] >> 4;
            let th = (s[0] & 15) as usize;
            if th > 3 || tc > 1 {
                return Err(Error::Corrupt);
            }
            let counts: [u8; 16] = s.get(1..17).ok_or(Error::Corrupt)?.try_into().unwrap();
            let total: usize = counts.iter().map(|c| *c as usize).sum();
            let symbols = s.get(17..17 + total).ok_or(Error::Corrupt)?;
            let h = Huffman::new(&counts, symbols)?;
            if tc == 0 {
                self.dc[th] = h;
            } else {
                self.ac[th] = h;
            }
            s = &s[17 + total..];
        }
        Ok(())
    }

    fn quant(&mut self, mut s: &[u8]) -> Result<(), Error> {
        while !s.is_empty() {
            let p = s[0] >> 4;
            let t = (s[0] & 15) as usize;
            if t > 3 {
                return Err(Error::Corrupt);
            }
            let size = if p == 0 { 64 } else { 128 };
            let v = s.get(1..1 + size).ok_or(Error::Corrupt)?;
            for k in 0..64 {
                self.qt[t][ZIGZAG[k]] = if p == 0 { v[k] as u16 } else { (v[2 * k] as u16) << 8 | v[2 * k + 1] as u16 };
            }
            s = &s[1 + size..];
        }
        Ok(())
    }

    /// Decodes one scan; returns where its entropy-coded data ends.
    fn scan(&mut self, s: &[u8], data_start: usize) -> Result<usize, Error> {
        if !self.frame {
            return Err(Error::Corrupt);
        }
        let n = *s.first().ok_or(Error::Corrupt)? as usize;
        let mut members = Vec::with_capacity(n);
        for k in 0..n {
            let c = s.get(1 + k * 2..3 + k * 2).ok_or(Error::Corrupt)?;
            let idx = self.comps.iter().position(|x| x.id == c[0]).ok_or(Error::Corrupt)?;
            self.comps[idx].dc_table = (c[1] >> 4) as usize & 3;
            self.comps[idx].ac_table = (c[1] & 15) as usize & 3;
            members.push(idx);
        }
        let p = 1 + n * 2;
        let (ss, se, a) = (
            *s.get(p).ok_or(Error::Corrupt)? as usize,
            *s.get(p + 1).ok_or(Error::Corrupt)? as usize,
            *s.get(p + 2).ok_or(Error::Corrupt)?,
        );
        let (ah, al) = ((a >> 4) as u32, (a & 15) as u32);
        if !self.progressive {
            // Baseline: the whole spectrum at once.
        } else if ss > se || se > 63 || (ss == 0 && se != 0) || (ss > 0 && n != 1) {
            return Err(Error::Corrupt);
        }
        for c in &mut self.comps {
            c.dc_pred = 0;
        }
        self.eobrun = 0;
        let mut bits = Bits { data: self.data, pos: data_start, acc: 0, count: 0, marker: false };
        let mut until_restart = self.restart_interval;
        // One component alone is coded block by block over its own extent.
        let (units_x, units_y) = if n == 1 {
            let c = &self.comps[members[0]];
            let cw = (self.width * c.h).div_ceil(self.hmax);
            let ch = (self.height * c.v).div_ceil(self.vmax);
            (cw.div_ceil(8), ch.div_ceil(8))
        } else {
            (self.mcux, self.mcuy)
        };
        for uy in 0..units_y {
            for ux in 0..units_x {
                if self.restart_interval > 0 {
                    if until_restart == 0 {
                        bits.restart();
                        for c in &mut self.comps {
                            c.dc_pred = 0;
                        }
                        self.eobrun = 0;
                        until_restart = self.restart_interval;
                    }
                    until_restart -= 1;
                }
                if n == 1 {
                    self.block(&mut bits, members[0], uy * self.comps[members[0]].bw + ux, ss, se, ah, al)?;
                } else {
                    for &ci in &members {
                        let (h, v, bw) = (self.comps[ci].h, self.comps[ci].v, self.comps[ci].bw);
                        for by in 0..v {
                            for bx in 0..h {
                                let b = (uy * v + by) * bw + ux * h + bx;
                                self.block(&mut bits, ci, b, ss, se, ah, al)?;
                            }
                        }
                    }
                }
            }
        }
        // Continue after the entropy-coded data (at the next marker that isn't RSTn).
        let mut end = bits.pos;
        while end + 1 < self.data.len() {
            if self.data[end] == 0xFF && self.data[end + 1] != 0 && !(0xD0..=0xD7).contains(&self.data[end + 1]) {
                break;
            }
            end += 1;
        }
        Ok(end)
    }

    #[allow(clippy::too_many_arguments)]
    fn block(
        &mut self,
        bits: &mut Bits,
        ci: usize,
        b: usize,
        ss: usize,
        se: usize,
        ah: u32,
        al: u32,
    ) -> Result<(), Error> {
        let dc_table = self.comps[ci].dc_table;
        let ac_table = self.comps[ci].ac_table;
        let base = b * 64;
        if base + 64 > self.comps[ci].coefs.len() {
            return Ok(());
        }
        if !self.progressive {
            let t = bits.decode(&self.dc[dc_table])? as u32;
            let diff = bits.receive_extend(t.min(16));
            let c = &mut self.comps[ci];
            c.dc_pred += diff;
            c.coefs[base] = c.dc_pred as i16;
            let mut k = 1;
            while k < 64 {
                let rs = bits.decode(&self.ac[ac_table])?;
                let (r, s) = ((rs >> 4) as usize, (rs & 15) as u32);
                if s == 0 {
                    if r == 15 {
                        k += 16;
                        continue;
                    }
                    break;
                }
                k += r;
                if k > 63 {
                    break;
                }
                self.comps[ci].coefs[base + ZIGZAG[k]] = bits.receive_extend(s) as i16;
                k += 1;
            }
            return Ok(());
        }
        if ss == 0 {
            // DC scans.
            if ah == 0 {
                let t = bits.decode(&self.dc[dc_table])? as u32;
                let diff = bits.receive_extend(t.min(16));
                let c = &mut self.comps[ci];
                c.dc_pred += diff;
                c.coefs[base] = (c.dc_pred << al) as i16;
            } else if bits.bit() {
                self.comps[ci].coefs[base] |= 1 << al;
            }
            return Ok(());
        }
        if ah == 0 {
            // AC first pass.
            if self.eobrun > 0 {
                self.eobrun -= 1;
                return Ok(());
            }
            let mut k = ss;
            while k <= se {
                let rs = bits.decode(&self.ac[ac_table])?;
                let (r, s) = ((rs >> 4) as u32, (rs & 15) as u32);
                if s == 0 {
                    if r < 15 {
                        self.eobrun = (1 << r) - 1;
                        if r > 0 {
                            self.eobrun += bits.bits(r);
                        }
                        break;
                    }
                    k += 16;
                    continue;
                }
                k += r as usize;
                if k > 63 {
                    break;
                }
                self.comps[ci].coefs[base + ZIGZAG[k]] = (bits.receive_extend(s) * (1 << al)) as i16;
                k += 1;
            }
            return Ok(());
        }
        // AC refinement (libjpeg's decode_mcu_AC_refine).
        let p1: i16 = 1 << al;
        let m1: i16 = -1 << al;
        let coefs = &mut self.comps[ci].coefs[base..base + 64];
        let mut k = ss;
        if self.eobrun == 0 {
            while k <= se {
                let rs = bits.decode(&self.ac[ac_table])?;
                let (mut r, s) = ((rs >> 4) as i32, (rs & 15) as u32);
                let mut value = 0i16;
                if s != 0 {
                    value = if bits.bit() { p1 } else { m1 };
                } else if r != 15 {
                    self.eobrun = 1 << r;
                    if r > 0 {
                        self.eobrun += bits.bits(r as u32);
                    }
                    break;
                }
                while k <= se {
                    let z = ZIGZAG[k];
                    if coefs[z] != 0 {
                        if bits.bit() && coefs[z] & p1 == 0 {
                            coefs[z] += if coefs[z] >= 0 { p1 } else { m1 };
                        }
                    } else {
                        r -= 1;
                        if r < 0 {
                            break;
                        }
                    }
                    k += 1;
                }
                if value != 0 && k <= se {
                    coefs[ZIGZAG[k]] = value;
                }
                k += 1;
            }
        }
        if self.eobrun > 0 {
            while k <= se {
                let z = ZIGZAG[k];
                if coefs[z] != 0 && bits.bit() && coefs[z] & p1 == 0 {
                    coefs[z] += if coefs[z] >= 0 { p1 } else { m1 };
                }
                k += 1;
            }
            self.eobrun -= 1;
        }
        Ok(())
    }

    /// Dequantises, transforms and converts to pixels.
    fn output(&self) -> Result<Image, Error> {
        // Each component as an 8-bit plane.
        let planes: Vec<(Vec<u8>, usize)> = self
            .comps
            .iter()
            .map(|c| {
                let stride = c.bw * 8;
                let mut plane = vec![0u8; stride * c.bh * 8];
                let q = &self.qt[c.tq];
                let mut block = [0i32; 64];
                for by in 0..c.bh {
                    for bx in 0..c.bw {
                        let src = &c.coefs[(by * c.bw + bx) * 64..][..64];
                        for i in 0..64 {
                            block[i] = src[i] as i32 * q[i] as i32;
                        }
                        idct(&block, &mut plane[by * 8 * stride + bx * 8..], stride);
                    }
                }
                (plane, stride)
            })
            .collect();
        let (w, h) = (self.width, self.height);
        let mut pixels = Vec::with_capacity(w * h);
        // Subsampled planes are interpolated bilinearly at each pixel's centre.
        let sample = |ci: usize, x: usize, y: usize| -> i32 {
            let c = &self.comps[ci];
            let (plane, stride) = &planes[ci];
            if c.h == self.hmax && c.v == self.vmax {
                return plane[y * stride + x] as i32;
            }
            let rows = plane.len() / stride;
            // Position in the plane, 1/256 units: (x + 0.5) * h / hmax - 0.5.
            let fx = (((2 * x + 1) * c.h * 128) / self.hmax) as i32 - 128;
            let fy = (((2 * y + 1) * c.v * 128) / self.vmax) as i32 - 128;
            let (x0, y0) = (fx.max(0) >> 8, fy.max(0) >> 8);
            let (ax, ay) = (if fx < 0 { 0 } else { fx & 255 }, if fy < 0 { 0 } else { fy & 255 });
            let (x0, y0) = (x0 as usize, y0 as usize);
            let x1 = (x0 + 1).min(stride - 1);
            let y1 = (y0 + 1).min(rows - 1);
            let p = |xx: usize, yy: usize| plane[yy * stride + xx] as i32;
            let top = p(x0, y0) * (256 - ax) + p(x1, y0) * ax;
            let bottom = p(x0, y1) * (256 - ax) + p(x1, y1) * ax;
            (top * (256 - ay) + bottom * ay + 32768) >> 16
        };
        let clamp = |v: i32| v.clamp(0, 255) as u32;
        let ycc = |y: i32, cb: i32, cr: i32| {
            let (cb, cr) = (cb - 128, cr - 128);
            let r = y + ((91881 * cr) >> 16);
            let g = y - ((22554 * cb + 46802 * cr) >> 16);
            let b = y + ((116130 * cb) >> 16);
            (r, g, b)
        };
        let rgb_ids =
            self.comps.len() == 3 && self.comps[0].id == b'R' && self.comps[1].id == b'G' && self.comps[2].id == b'B';
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = match self.comps.len() {
                    1 => {
                        let v = sample(0, x, y);
                        (v, v, v)
                    }
                    3 if self.adobe == Some(0) || rgb_ids => (sample(0, x, y), sample(1, x, y), sample(2, x, y)),
                    3 => ycc(sample(0, x, y), sample(1, x, y), sample(2, x, y)),
                    _ => {
                        let (c, m, ye) = if self.adobe == Some(2) {
                            let (r, g, b) = ycc(sample(0, x, y), sample(1, x, y), sample(2, x, y));
                            // YCC encodes the complement of the (Adobe-inverted) CMY.
                            (255 - r.clamp(0, 255), 255 - g.clamp(0, 255), 255 - b.clamp(0, 255))
                        } else {
                            (sample(0, x, y), sample(1, x, y), sample(2, x, y))
                        };
                        let k = sample(3, x, y);
                        // Adobe stores CMYK inverted.
                        (c * k / 255, m * k / 255, ye * k / 255)
                    }
                };
                pixels.push(0xFF00_0000 | clamp(r) << 16 | clamp(g) << 8 | clamp(b));
            }
        }
        Ok(Image { width: w as u32, height: h as u32, pixels })
    }
}

/// 8×8 inverse DCT (stb_image's integer version), writing clamped samples.
fn idct(input: &[i32; 64], out: &mut [u8], stride: usize) {
    const fn f2f(x: f32) -> i32 {
        (x * 4096.0 + 0.5) as i32
    }
    const C0: i32 = f2f(0.541_196_1);
    const C1: i32 = f2f(-1.847_759_1);
    const C2: i32 = f2f(0.765_366_85);
    const C3: i32 = f2f(1.175_875_6);
    const C4: i32 = f2f(0.298_631_34);
    const C5: i32 = f2f(2.053_119_9);
    const C6: i32 = f2f(3.072_711);
    const C7: i32 = f2f(1.501_321_1);
    const C8: i32 = f2f(-0.899_976_2);
    const C9: i32 = f2f(-2.562_915_3);
    const C10: i32 = f2f(-1.961_570_6);
    const C11: i32 = f2f(-0.390_180_64);
    #[inline(always)]
    fn idct_1d(s: [i32; 8]) -> [i32; 8] {
        let (p2, p3) = (s[2], s[6]);
        let p1 = (p2 + p3) * C0;
        let t2 = p1 + p3 * C1;
        let t3 = p1 + p2 * C2;
        let (p2, p3) = (s[0], s[4]);
        let t0 = (p2 + p3) * 4096;
        let t1 = (p2 - p3) * 4096;
        let (x0, x3, x1, x2) = (t0 + t3, t0 - t3, t1 + t2, t1 - t2);
        let (mut t0, mut t1, mut t2, mut t3) = (s[7], s[5], s[3], s[1]);
        let p3 = t0 + t2;
        let p4 = t1 + t3;
        let p1 = t0 + t3;
        let p2 = t1 + t2;
        let p5 = (p3 + p4) * C3;
        t0 *= C4;
        t1 *= C5;
        t2 *= C6;
        t3 *= C7;
        let p1 = p5 + p1 * C8;
        let p2 = p5 + p2 * C9;
        let p3 = p3 * C10;
        let p4 = p4 * C11;
        t3 += p1 + p4;
        t2 += p2 + p3;
        t1 += p2 + p4;
        t0 += p1 + p3;
        [x0, x1, x2, x3, t0, t1, t2, t3]
    }
    let mut v = [0i32; 64];
    for i in 0..8 {
        let col = [
            input[i],
            input[8 + i],
            input[16 + i],
            input[24 + i],
            input[32 + i],
            input[40 + i],
            input[48 + i],
            input[56 + i],
        ];
        if col[1..].iter().all(|c| *c == 0) {
            let dc = col[0] * 4;
            for r in 0..8 {
                v[r * 8 + i] = dc;
            }
            continue;
        }
        let [x0, x1, x2, x3, t0, t1, t2, t3] = idct_1d(col);
        let (x0, x1, x2, x3) = (x0 + 512, x1 + 512, x2 + 512, x3 + 512);
        v[i] = (x0 + t3) >> 10;
        v[56 + i] = (x0 - t3) >> 10;
        v[8 + i] = (x1 + t2) >> 10;
        v[48 + i] = (x1 - t2) >> 10;
        v[16 + i] = (x2 + t1) >> 10;
        v[40 + i] = (x2 - t1) >> 10;
        v[24 + i] = (x3 + t0) >> 10;
        v[32 + i] = (x3 - t0) >> 10;
    }
    for r in 0..8 {
        let row: [i32; 8] = v[r * 8..r * 8 + 8].try_into().unwrap();
        let [x0, x1, x2, x3, t0, t1, t2, t3] = idct_1d(row);
        let bias = 65536 + (128 << 17);
        let (x0, x1, x2, x3) = (x0 + bias, x1 + bias, x2 + bias, x3 + bias);
        let o = &mut out[r * stride..r * stride + 8];
        let c = |v: i32| (v >> 17).clamp(0, 255) as u8;
        o[0] = c(x0 + t3);
        o[7] = c(x0 - t3);
        o[1] = c(x1 + t2);
        o[6] = c(x1 - t2);
        o[2] = c(x2 + t1);
        o[5] = c(x2 - t1);
        o[3] = c(x3 + t0);
        o[4] = c(x3 - t0);
    }
}
