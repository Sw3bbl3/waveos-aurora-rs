//! A TrueType font reader, written for Aurora.
//!
//! Supports what UI text needs: metrics (`head`, `hhea`, `OS/2`, `maxp`,
//! `hmtx`), character mapping (`cmap` formats 4 and 12), quadratic outlines
//! (`loca`/`glyf`, simple and composite glyphs) and pair kerning (the `kern`
//! table and GPOS pair adjustment, formats 1 and 2). Outlines are turned into
//! coverage masks by [`raster`], in fixed point, so the soft-float kernel can
//! render text at any size too.

pub mod raster;

use alloc::vec::Vec;

/// Reads big-endian values with bounds checks (malformed fonts yield zeros).
#[derive(Clone, Copy)]
struct Bytes<'a>(&'a [u8]);

impl<'a> Bytes<'a> {
    fn u8(&self, off: usize) -> u8 {
        self.0.get(off).copied().unwrap_or(0)
    }
    fn u16(&self, off: usize) -> u16 {
        match self.0.get(off..off + 2) {
            Some(b) => u16::from_be_bytes([b[0], b[1]]),
            None => 0,
        }
    }
    fn i16(&self, off: usize) -> i16 {
        self.u16(off) as i16
    }
    fn u32(&self, off: usize) -> u32 {
        match self.0.get(off..off + 4) {
            Some(b) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
            None => 0,
        }
    }
    fn sub(&self, off: usize, len: usize) -> Bytes<'a> {
        let end = off.saturating_add(len).min(self.0.len());
        Bytes(self.0.get(off.min(end)..end).unwrap_or(&[]))
    }
    fn from(&self, off: usize) -> Bytes<'a> {
        Bytes(self.0.get(off..).unwrap_or(&[]))
    }
}

/// A point of a glyph outline in font units (y up).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
    pub on_curve: bool,
}

/// A glyph outline: points and the index one past the end of each contour.
#[derive(Default, Debug)]
pub struct Outline {
    pub points: Vec<Point>,
    pub contour_ends: Vec<usize>,
    pub x_min: i32,
    pub y_min: i32,
    pub x_max: i32,
    pub y_max: i32,
}

pub struct TrueType<'a> {
    loca: Bytes<'a>,
    glyf: Bytes<'a>,
    hmtx: Bytes<'a>,
    cmap: Bytes<'a>,
    kern: Option<Bytes<'a>>,
    /// GPOS pair-adjustment subtables.
    gpos_pairs: Vec<Bytes<'a>>,
    long_loca: bool,
    num_hmetrics: u16,
    pub num_glyphs: u16,
    pub units_per_em: u16,
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
}

fn tag(t: &[u8; 4]) -> u32 {
    u32::from_be_bytes(*t)
}

impl<'a> TrueType<'a> {
    pub fn parse(data: &'a [u8]) -> Option<TrueType<'a>> {
        let d = Bytes(data);
        let version = d.u32(0);
        if version != 0x0001_0000 && version != tag(b"true") {
            return None; // not TrueType outlines (e.g. CFF-based OpenType)
        }
        let n = d.u16(4) as usize;
        let find = |t: &[u8; 4]| -> Option<Bytes<'a>> {
            (0..n)
                .map(|i| 12 + 16 * i)
                .find(|&r| d.u32(r) == tag(t))
                .map(|r| d.sub(d.u32(r + 8) as usize, d.u32(r + 12) as usize))
        };
        let head = find(b"head")?;
        let hhea = find(b"hhea")?;
        let maxp = find(b"maxp")?;
        let mut tt = TrueType {
            loca: find(b"loca")?,
            glyf: find(b"glyf")?,
            hmtx: find(b"hmtx")?,
            cmap: find(b"cmap")?,
            kern: find(b"kern"),
            gpos_pairs: Vec::new(),
            long_loca: head.i16(50) != 0,
            num_hmetrics: hhea.u16(34),
            num_glyphs: maxp.u16(4),
            units_per_em: head.u16(18).max(16),
            ascender: hhea.i16(4),
            descender: hhea.i16(6),
            line_gap: hhea.i16(8),
        };
        // Like most layout engines: typographic metrics when the font asks for them.
        if let Some(os2) = find(b"OS/2") {
            if os2.u16(62) & (1 << 7) != 0 && os2.0.len() >= 74 {
                tt.ascender = os2.i16(68);
                tt.descender = os2.i16(70);
                tt.line_gap = os2.i16(72);
            }
        }
        if let Some(gpos) = find(b"GPOS") {
            tt.gpos_pairs = gpos_pair_subtables(gpos);
        }
        tt.cmap = select_cmap(tt.cmap)?;
        Some(tt)
    }

    /// The glyph for a character (0 = missing).
    pub fn glyph_index(&self, c: char) -> u16 {
        let c = c as u32;
        let t = self.cmap;
        match t.u16(0) {
            4 => {
                if c > 0xFFFF {
                    return 0;
                }
                let segs = (t.u16(6) / 2) as usize;
                let ends = 14;
                let starts = ends + 2 * segs + 2;
                let deltas = starts + 2 * segs;
                let ranges = deltas + 2 * segs;
                // Binary search for the first segment whose end >= c.
                let (mut lo, mut hi) = (0, segs);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    if (t.u16(ends + 2 * mid) as u32) < c {
                        lo = mid + 1;
                    } else {
                        hi = mid;
                    }
                }
                if lo >= segs {
                    return 0;
                }
                let start = t.u16(starts + 2 * lo) as u32;
                if c < start {
                    return 0;
                }
                let delta = t.u16(deltas + 2 * lo);
                let range = t.u16(ranges + 2 * lo) as usize;
                if range == 0 {
                    return (c as u16).wrapping_add(delta);
                }
                let at = ranges + 2 * lo + range + 2 * (c - start) as usize;
                match t.u16(at) {
                    0 => 0,
                    g => g.wrapping_add(delta),
                }
            }
            12 => {
                let groups = t.u32(12) as usize;
                let (mut lo, mut hi) = (0, groups);
                while lo < hi {
                    let mid = (lo + hi) / 2;
                    let g = 16 + 12 * mid;
                    if t.u32(g + 4) < c {
                        lo = mid + 1;
                    } else if t.u32(g) > c {
                        hi = mid;
                    } else {
                        return (t.u32(g + 8) + (c - t.u32(g))) as u16;
                    }
                }
                0
            }
            _ => 0,
        }
    }

    /// Advance width in font units.
    pub fn advance(&self, glyph: u16) -> u16 {
        let i = glyph.min(self.num_hmetrics.saturating_sub(1)) as usize;
        self.hmtx.u16(4 * i)
    }

    /// Pair kerning between two glyphs in font units (negative = closer).
    pub fn kerning(&self, left: u16, right: u16) -> i32 {
        for st in &self.gpos_pairs {
            if let Some(v) = gpos_pair_value(*st, left, right) {
                return v;
            }
        }
        if let Some(k) = self.kern {
            return kern_table_value(k, left, right);
        }
        0
    }

    fn glyph_data(&self, glyph: u16) -> Option<Bytes<'a>> {
        if glyph >= self.num_glyphs {
            return None;
        }
        let g = glyph as usize;
        let (start, end) = if self.long_loca {
            (self.loca.u32(4 * g) as usize, self.loca.u32(4 * g + 4) as usize)
        } else {
            (self.loca.u16(2 * g) as usize * 2, self.loca.u16(2 * g + 2) as usize * 2)
        };
        (end > start).then(|| self.glyf.sub(start, end - start))
    }

    /// The outline of `glyph` in font units (empty for spaces).
    pub fn outline(&self, glyph: u16) -> Outline {
        let mut o = Outline::default();
        self.append_outline(glyph, [65536, 0, 0, 65536, 0, 0], &mut o, 0);
        if let Some(first) = o.points.first() {
            let (mut x0, mut y0, mut x1, mut y1) = (first.x, first.y, first.x, first.y);
            for p in &o.points {
                x0 = x0.min(p.x);
                y0 = y0.min(p.y);
                x1 = x1.max(p.x);
                y1 = y1.max(p.y);
            }
            (o.x_min, o.y_min, o.x_max, o.y_max) = (x0, y0, x1, y1);
        }
        o
    }

    /// Appends `glyph` transformed by `m` = [a, b, c, d, dx, dy] (a–d in 16.16).
    fn append_outline(&self, glyph: u16, m: [i32; 6], out: &mut Outline, depth: u32) {
        let Some(g) = self.glyph_data(glyph) else { return };
        let contours = g.i16(0);
        let xf = |x: i32, y: i32| -> Point {
            let px = ((x as i64 * m[0] as i64 + y as i64 * m[2] as i64) >> 16) as i32 + m[4];
            let py = ((x as i64 * m[1] as i64 + y as i64 * m[3] as i64) >> 16) as i32 + m[5];
            Point { x: px, y: py, on_curve: true }
        };
        if contours >= 0 {
            let nc = contours as usize;
            let mut ends = Vec::with_capacity(nc);
            for i in 0..nc {
                ends.push(g.u16(10 + 2 * i) as usize);
            }
            let npts = ends.last().map(|&e| e + 1).unwrap_or(0);
            if npts == 0 || npts > 20_000 {
                return;
            }
            let ins_len = g.u16(10 + 2 * nc) as usize;
            let mut p = 12 + 2 * nc + ins_len;
            let mut flags = Vec::with_capacity(npts);
            while flags.len() < npts {
                let f = g.u8(p);
                p += 1;
                flags.push(f);
                if f & 8 != 0 {
                    let rep = g.u8(p);
                    p += 1;
                    for _ in 0..rep {
                        if flags.len() < npts {
                            flags.push(f);
                        }
                    }
                }
            }
            let mut xs = Vec::with_capacity(npts);
            let mut v = 0i32;
            for &f in &flags {
                if f & 2 != 0 {
                    let dx = g.u8(p) as i32;
                    p += 1;
                    v += if f & 16 != 0 { dx } else { -dx };
                } else if f & 16 == 0 {
                    v += g.i16(p) as i32;
                    p += 2;
                }
                xs.push(v);
            }
            v = 0;
            let base = out.points.len();
            for (i, &f) in flags.iter().enumerate() {
                if f & 4 != 0 {
                    let dy = g.u8(p) as i32;
                    p += 1;
                    v += if f & 32 != 0 { dy } else { -dy };
                } else if f & 32 == 0 {
                    v += g.i16(p) as i32;
                    p += 2;
                }
                let mut pt = xf(xs[i], v);
                pt.on_curve = f & 1 != 0;
                out.points.push(pt);
            }
            for e in ends {
                out.contour_ends.push(base + e.min(npts - 1) + 1);
            }
        } else if depth < 8 {
            let mut p = 10;
            loop {
                let flags = g.u16(p);
                let child = g.u16(p + 2);
                p += 4;
                let (dx, dy) = if flags & 1 != 0 {
                    let r = (g.i16(p) as i32, g.i16(p + 2) as i32);
                    p += 4;
                    r
                } else {
                    let r = (g.u8(p) as i8 as i32, g.u8(p + 1) as i8 as i32);
                    p += 2;
                    r
                };
                let f2 = |v: i16| (v as i32) << 2; // 2.14 → 16.16
                let (mut a, mut b, mut c, mut d) = (65536, 0, 0, 65536);
                if flags & 8 != 0 {
                    a = f2(g.i16(p));
                    d = a;
                    p += 2;
                } else if flags & 0x40 != 0 {
                    a = f2(g.i16(p));
                    d = f2(g.i16(p + 2));
                    p += 4;
                } else if flags & 0x80 != 0 {
                    a = f2(g.i16(p));
                    b = f2(g.i16(p + 2));
                    c = f2(g.i16(p + 4));
                    d = f2(g.i16(p + 6));
                    p += 8;
                }
                // Only offsets given as x/y values are supported (point matching is rare).
                let (ox, oy) = if flags & 2 != 0 { (dx, dy) } else { (0, 0) };
                let mul = |x: i32, y: i32| ((x as i64 * y as i64) >> 16) as i32;
                let child_m = [
                    mul(a, m[0]) + mul(b, m[2]),
                    mul(a, m[1]) + mul(b, m[3]),
                    mul(c, m[0]) + mul(d, m[2]),
                    mul(c, m[1]) + mul(d, m[3]),
                    xf(ox, oy).x,
                    xf(ox, oy).y,
                ];
                self.append_outline(child, child_m, out, depth + 1);
                if flags & 0x20 == 0 {
                    break;
                }
            }
        }
    }
}

/// Picks the best Unicode subtable: full repertoire (format 12) over BMP (format 4).
fn select_cmap(cmap: Bytes) -> Option<Bytes> {
    let n = cmap.u16(2) as usize;
    let mut best: Option<(u32, Bytes)> = None;
    for i in 0..n {
        let r = 4 + 8 * i;
        let (platform, encoding) = (cmap.u16(r), cmap.u16(r + 2));
        let sub = cmap.from(cmap.u32(r + 4) as usize);
        let format = sub.u16(0);
        let score = match (platform, encoding, format) {
            (3, 10, 12) | (0, 4, 12) | (0, 6, 12) => 3,
            (3, 1, 4) | (0, 3, 4) | (0, 1, 4) | (0, 0, 4) => 2,
            _ => 0,
        };
        if score > 0 && best.as_ref().is_none_or(|(s, _)| score > *s) {
            best = Some((score, sub));
        }
    }
    best.map(|(_, b)| b)
}

fn kern_table_value(k: Bytes, left: u16, right: u16) -> i32 {
    if k.u16(0) != 0 {
        return 0; // only the Microsoft version-0 layout
    }
    let n = k.u16(2) as usize;
    let mut off = 4;
    for _ in 0..n {
        let len = k.u16(off + 2) as usize;
        let coverage = k.u16(off + 4);
        if coverage >> 8 == 0 && coverage & 1 != 0 {
            let npairs = k.u16(off + 6) as usize;
            let key = (left as u32) << 16 | right as u32;
            let (mut lo, mut hi) = (0, npairs);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let e = off + 14 + 6 * mid;
                let k2 = k.u32(e);
                if k2 < key {
                    lo = mid + 1;
                } else if k2 > key {
                    hi = mid;
                } else {
                    return k.i16(e + 4) as i32;
                }
            }
        }
        off += len.max(6);
    }
    0
}

/// Pair adjustment subtables (lookup type 2, also through extension lookups) of the
/// default `kern` feature of any script.
fn gpos_pair_subtables(gpos: Bytes) -> Vec<Bytes> {
    let mut out = Vec::new();
    let features = gpos.from(gpos.u16(6) as usize);
    let lookups = gpos.from(gpos.u16(8) as usize);
    let mut wanted: Vec<u16> = Vec::new();
    for i in 0..features.u16(0) as usize {
        let r = 2 + 6 * i;
        if features.u32(r) == tag(b"kern") {
            let f = features.from(features.u16(r + 4) as usize);
            for j in 0..f.u16(2) as usize {
                let l = f.u16(4 + 2 * j);
                if !wanted.contains(&l) {
                    wanted.push(l);
                }
            }
        }
    }
    wanted.sort_unstable();
    for l in wanted {
        let lookup = lookups.from(lookups.u16(2 + 2 * l as usize) as usize);
        let kind = lookup.u16(0);
        for s in 0..lookup.u16(4) as usize {
            let mut st = lookup.from(lookup.u16(6 + 2 * s) as usize);
            let mut k = kind;
            if k == 9 {
                k = st.u16(2);
                st = st.from(st.u32(4) as usize);
            }
            if k == 2 {
                out.push(st);
            }
        }
    }
    out
}

/// Index of `glyph` in a coverage table.
fn coverage_index(cov: Bytes, glyph: u16) -> Option<usize> {
    match cov.u16(0) {
        1 => {
            let n = cov.u16(2) as usize;
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let g = cov.u16(4 + 2 * mid);
                if g < glyph {
                    lo = mid + 1;
                } else if g > glyph {
                    hi = mid;
                } else {
                    return Some(mid);
                }
            }
            None
        }
        2 => {
            let n = cov.u16(2) as usize;
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = 4 + 6 * mid;
                if cov.u16(r + 2) < glyph {
                    lo = mid + 1;
                } else if cov.u16(r) > glyph {
                    hi = mid;
                } else {
                    return Some(cov.u16(r + 4) as usize + (glyph - cov.u16(r)) as usize);
                }
            }
            None
        }
        _ => None,
    }
}

fn class_of(cd: Bytes, glyph: u16) -> u16 {
    match cd.u16(0) {
        1 => {
            let start = cd.u16(2);
            let n = cd.u16(4);
            if glyph >= start && glyph - start < n {
                cd.u16(6 + 2 * (glyph - start) as usize)
            } else {
                0
            }
        }
        2 => {
            let n = cd.u16(2) as usize;
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = 4 + 6 * mid;
                if cd.u16(r + 2) < glyph {
                    lo = mid + 1;
                } else if cd.u16(r) > glyph {
                    hi = mid;
                } else {
                    return cd.u16(r + 4);
                }
            }
            0
        }
        _ => 0,
    }
}

/// Size in bytes of a ValueRecord with the given format bits.
fn value_size(format: u16) -> usize {
    2 * format.count_ones() as usize
}

/// The XAdvance of the first glyph's ValueRecord (the part kerning uses).
fn x_advance(v: Bytes, format: u16) -> i32 {
    if format & 4 == 0 {
        return 0;
    }
    let before = (format & 3).count_ones() as usize;
    v.i16(2 * before) as i32
}

fn gpos_pair_value(st: Bytes, left: u16, right: u16) -> Option<i32> {
    let format = st.u16(0);
    let cov = st.from(st.u16(2) as usize);
    let idx = coverage_index(cov, left)?;
    let (vf1, vf2) = (st.u16(4), st.u16(6));
    let (s1, s2) = (value_size(vf1), value_size(vf2));
    match format {
        1 => {
            let set = st.from(st.u16(10 + 2 * idx) as usize);
            let n = set.u16(0) as usize;
            let rec = 2 + s1 + s2;
            let (mut lo, mut hi) = (0, n);
            while lo < hi {
                let mid = (lo + hi) / 2;
                let r = 2 + rec * mid;
                let g = set.u16(r);
                if g < right {
                    lo = mid + 1;
                } else if g > right {
                    hi = mid;
                } else {
                    return Some(x_advance(set.from(r + 2), vf1));
                }
            }
            None
        }
        2 => {
            let c1 = class_of(st.from(st.u16(8) as usize), left) as usize;
            let c2 = class_of(st.from(st.u16(10) as usize), right) as usize;
            let (n1, n2) = (st.u16(12) as usize, st.u16(14) as usize);
            if c1 >= n1 || c2 >= n2 {
                return None;
            }
            let r = 16 + (c1 * n2 + c2) * (s1 + s2);
            Some(x_advance(st.from(r), vf1))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests;
