//! Outline → coverage mask, in 16.16 fixed point.
//!
//! Signed-area accumulation (the technique of font-rs and fontdue): every
//! outline edge deposits, into the pixel cells it crosses, the exact area it
//! sweeps to its right. A running sum along each row then yields the
//! coverage of every pixel under the non-zero winding rule — exact
//! anti-aliasing with no supersampling. Quadratic curves are flattened into
//! lines first. Integer-only, so the kernel (soft-float) can use it too.

use super::Outline;
use crate::math::isqrt;
use alloc::vec;
use alloc::vec::Vec;

const ONE: i64 = 1 << 16;

/// A rendered glyph: an 8-bit coverage mask and its placement relative to the
/// pen position (x right, `y_min` = bottom edge above the baseline, y up).
#[derive(Clone, Debug, Default)]
pub struct Mask {
    pub width: u16,
    pub height: u16,
    pub x_min: i16,
    pub y_min: i16,
    pub data: Vec<u8>,
}

struct Acc {
    w: usize,
    h: usize,
    a: Vec<i32>,
}

impl Acc {
    fn add(&mut self, row: usize, col: i64, v: i64) {
        if col >= 0 && (col as usize) < self.w + 2 {
            let i = row * (self.w + 2) + col as usize;
            self.a[i] = self.a[i].wrapping_add(v as i32);
        }
    }

    /// Accumulates the area to the right of the line (x0,y0)→(x1,y1) (16.16 pixel coordinates, y down).
    fn line(&mut self, x0: i64, y0: i64, x1: i64, y1: i64) {
        if y0 == y1 {
            return;
        }
        let (dir, (px0, py0), (px1, py1)) = if y0 < y1 { (1, (x0, y0), (x1, y1)) } else { (-1, (x1, y1), (x0, y0)) };
        let dxdy = ((px1 - px0) << 16) / (py1 - py0);
        let mut x = px0;
        let mut row = py0 >> 16;
        if py0 < 0 {
            x -= py0 * dxdy >> 16;
            row = 0;
        }
        let end_row = ((py1 + ONE - 1) >> 16).min(self.h as i64);
        while row < end_row {
            let top = (row << 16).max(py0);
            let bottom = ((row + 1) << 16).min(py1);
            let dy = bottom - top;
            let xnext = x + (dxdy * dy >> 16);
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let xa_floor = xa.div_euclid(ONE) * ONE;
            let xai = xa_floor >> 16;
            let xb_ceil = (xb + ONE - 1).div_euclid(ONE) * ONE;
            let xbi = xb_ceil >> 16;
            let r = row as usize;
            if xbi <= xai + 1 {
                // The edge stays within one pixel column on this row.
                let xmf = (x + xnext) / 2 - xa_floor;
                self.add(r, xai, d - (d * xmf >> 16));
                self.add(r, xai + 1, d * xmf >> 16);
            } else {
                let s = (1i64 << 32) / (xb - xa); // 1 / width, 16.16
                let xaf = xa - xa_floor;
                let a0 = ((s as i128 * ((ONE - xaf) as i128).pow(2)) >> 33) as i64;
                let xbf = xb - xb_ceil + ONE;
                let am = ((s as i128 * (xbf as i128).pow(2)) >> 33) as i64;
                self.add(r, xai, d * a0 >> 16);
                if xbi == xai + 2 {
                    self.add(r, xai + 1, d * (ONE - a0 - am) >> 16);
                } else {
                    let a1 = s * (3 * ONE / 2 - xaf) >> 16;
                    self.add(r, xai + 1, d * (a1 - a0) >> 16);
                    let step = d * s >> 16;
                    for xi in xai + 2..xbi - 1 {
                        self.add(r, xi, step);
                    }
                    let a2 = a1 + (xbi - xai - 3) * s;
                    self.add(r, xbi - 1, d * (ONE - a2 - am) >> 16);
                }
                self.add(r, xbi, d * am >> 16);
            }
            x = xnext;
            row += 1;
        }
    }

    /// Flattens a quadratic Bézier into enough lines to be smooth at this size.
    fn quad(&mut self, p0: (i64, i64), p1: (i64, i64), p2: (i64, i64)) {
        let dx = p0.0 - 2 * p1.0 + p2.0;
        let dy = p0.1 - 2 * p1.1 + p2.1;
        let dev = isqrt((dx * dx + dy * dy) as u64) as i64; // 16.16 pixels
                                                            // A chord's gap from the curve (sagitta) is about dev / (4 n²): keep it under 1/64 px.
        if dev < ONE / 16 {
            return self.line(p0.0, p0.1, p2.0, p2.1);
        }
        let n = 1 + (isqrt((dev * 16 * ONE) as u64) as i64 >> 16).min(64);
        let mut prev = p0;
        for i in 1..=n {
            let t = (i << 16) / n;
            let mt = ONE - t;
            let a = mt * mt >> 16;
            let b = 2 * mt * t >> 16;
            let c = t * t >> 16;
            let p = ((a * p0.0 + b * p1.0 + c * p2.0) >> 16, (a * p0.1 + b * p1.1 + c * p2.1) >> 16);
            self.line(prev.0, prev.1, p.0, p.1);
            prev = p;
        }
    }
}

/// Renders `outline` at `px_per_em` / `units_per_em`, shifted right by
/// `x_shift` (16.16 pixels, for sub-pixel positioning).
pub fn render(outline: &Outline, units_per_em: u16, px_per_em: u32, x_shift: i32) -> Mask {
    if outline.points.is_empty() {
        return Mask::default();
    }
    let upem = units_per_em as i64;
    let scale = |v: i32| -> i64 { (v as i64 * px_per_em as i64 * ONE) / upem };
    let x_min = (scale(outline.x_min) + x_shift as i64).div_euclid(ONE);
    let x_max = (scale(outline.x_max) + x_shift as i64 + ONE - 1).div_euclid(ONE);
    let y_min = scale(outline.y_min).div_euclid(ONE);
    let y_max = (scale(outline.y_max) + ONE - 1).div_euclid(ONE);
    let (w, h) = ((x_max - x_min).max(1) as usize, (y_max - y_min).max(1) as usize);
    if w > 2048 || h > 2048 {
        return Mask::default();
    }
    let mut acc = Acc { w, h, a: vec![0; (w + 2) * h] };
    // Font units → 16.16 pixels relative to the mask (y down).
    let map = |x: i32, y: i32| -> (i64, i64) { (scale(x) + x_shift as i64 - x_min * ONE, y_max * ONE - scale(y)) };

    let mut start = 0;
    for &end in &outline.contour_ends {
        let pts = &outline.points[start..end.min(outline.points.len())];
        start = end;
        if pts.len() < 2 {
            continue;
        }
        // Begin at an on-curve point (or the midpoint of two off-curve ones).
        let n = pts.len();
        let first_on = pts.iter().position(|p| p.on_curve);
        let (origin, offset) = match first_on {
            Some(i) => (map(pts[i].x, pts[i].y), i),
            None => {
                let (a, b) = (map(pts[0].x, pts[0].y), map(pts[1].x, pts[1].y));
                (((a.0 + b.0) / 2, (a.1 + b.1) / 2), 1)
            }
        };
        let mut cur = origin;
        let mut ctrl: Option<(i64, i64)> = None;
        for k in 1..=n {
            let p = pts[(offset + k) % n];
            let pm = map(p.x, p.y);
            match (p.on_curve, ctrl) {
                (true, None) => {
                    acc.line(cur.0, cur.1, pm.0, pm.1);
                    cur = pm;
                }
                (true, Some(c)) => {
                    acc.quad(cur, c, pm);
                    cur = pm;
                    ctrl = None;
                }
                (false, None) => ctrl = Some(pm),
                (false, Some(c)) => {
                    let mid = ((c.0 + pm.0) / 2, (c.1 + pm.1) / 2);
                    acc.quad(cur, c, mid);
                    cur = mid;
                    ctrl = Some(pm);
                }
            }
        }
        match ctrl {
            Some(c) => acc.quad(cur, c, origin),
            None => acc.line(cur.0, cur.1, origin.0, origin.1),
        }
    }

    let mut data = vec![0u8; w * h];
    for y in 0..h {
        let mut sum: i64 = 0;
        let row = &acc.a[y * (w + 2)..(y + 1) * (w + 2)];
        for x in 0..w {
            sum += row[x] as i64;
            let cov = sum.unsigned_abs().min(ONE as u64);
            data[y * w + x] = ((cov * 255 + ONE as u64 / 2) >> 16) as u8;
        }
    }
    Mask { width: w as u16, height: h as u16, x_min: x_min as i16, y_min: y_min as i16, data }
}
