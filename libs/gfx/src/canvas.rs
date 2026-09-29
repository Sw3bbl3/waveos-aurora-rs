//! Software rasterizer: clipped, alpha-blended drawing into a 32-bit buffer.
//!
//! Colors are `0xAARRGGBB`. All drawing is clipped to `clip`, which the
//! compositor sets to the damaged region being repainted. Anti-aliasing uses
//! 8-bit coverage computed from integer distance fields.

use crate::font::Font;
use crate::geom::Rect;
use crate::math::isqrt;

pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    0xFF00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

pub const fn with_alpha(c: u32, a: u8) -> u32 {
    (c & 0x00FF_FFFF) | (a as u32) << 24
}

#[inline]
pub fn alpha(c: u32) -> u32 {
    c >> 24
}

/// Blends `src` (0xRRGGBB) over `dst` with opacity `a` (0..=255).
#[inline]
pub fn blend(dst: u32, src: u32, a: u32) -> u32 {
    if a == 0 {
        return dst;
    }
    if a >= 255 {
        return src | 0xFF00_0000;
    }
    let inv = 255 - a;
    let rb = ((src & 0x00FF_00FF) * a + (dst & 0x00FF_00FF) * inv + 0x0080_0080) >> 8 & 0x00FF_00FF;
    let g = ((src & 0x0000_FF00) * a + (dst & 0x0000_FF00) * inv + 0x0000_8000) >> 8 & 0x0000_FF00;
    0xFF00_0000 | rb | g
}

/// Linear interpolation between two colors, t in 0..=256.
pub fn mix(a: u32, b: u32, t: i32) -> u32 {
    let t = t.clamp(0, 256) as u32;
    let ch = |s: u32| {
        let x = (a >> s) & 0xFF;
        let y = (b >> s) & 0xFF;
        ((x * (256 - t) + y * t) >> 8) << s
    };
    ch(24) | ch(16) | ch(8) | ch(0)
}

pub struct Canvas<'a> {
    pub buf: &'a mut [u32],
    pub width: i32,
    #[allow(dead_code)]
    pub height: i32,
    pub clip: Rect,
}

/// Anti-aliased coverage (0..=256) of pixel (x, y) for a rounded rectangle.
#[inline]
fn round_rect_coverage(r: &Rect, radius: i32, x: i32, y: i32) -> u32 {
    if radius <= 0 {
        return 256;
    }
    // Distance from the pixel centre to the nearest corner-circle centre, in 1/256 px.
    let cx = if x < r.x + radius {
        r.x + radius
    } else if x >= r.right() - radius {
        r.right() - radius
    } else {
        return 256;
    };
    let cy = if y < r.y + radius {
        r.y + radius
    } else if y >= r.bottom() - radius {
        r.bottom() - radius
    } else {
        return 256;
    };
    let dx = (x * 256 + 128 - cx * 256) as i64;
    let dy = (y * 256 + 128 - cy * 256) as i64;
    let d = isqrt((dx * dx + dy * dy) as u64) as i64;
    (radius as i64 * 256 + 128 - d).clamp(0, 256) as u32
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u32], width: i32, height: i32) -> Self {
        Self { buf, width, height, clip: Rect::new(0, 0, width, height) }
    }

    /// Runs `f` with the clip narrowed to `r`, restoring it afterwards.
    pub fn with_clip(&mut self, r: Rect, f: impl FnOnce(&mut Self)) {
        let saved = self.clip;
        self.clip = self.clip.intersect(&r);
        if !self.clip.is_empty() {
            f(self);
        }
        self.clip = saved;
    }

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: u32, a: u32) {
        let i = (y * self.width + x) as usize;
        self.buf[i] = blend(self.buf[i], c, a);
    }

    pub fn fill_rect(&mut self, r: Rect, c: u32) {
        let r = r.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let a = alpha(c);
        for y in r.y..r.bottom() {
            let row = &mut self.buf[(y * self.width + r.x) as usize..(y * self.width + r.right()) as usize];
            if a == 255 {
                row.fill(c);
            } else {
                for p in row {
                    *p = blend(*p, c, a);
                }
            }
        }
    }

    pub fn fill_round_rect(&mut self, r: Rect, radius: i32, c: u32) {
        self.fill_round_rect_with(r, radius, |_, _| c);
    }

    /// Vertical gradient inside a rounded rectangle.
    pub fn fill_round_rect_vgradient(&mut self, r: Rect, radius: i32, top: u32, bottom: u32) {
        let h = r.h.max(1);
        self.fill_round_rect_with(r, radius, |_, y| mix(top, bottom, (y - r.y) * 256 / h));
    }

    /// Diagonal gradient (top-left → bottom-right).
    pub fn fill_round_rect_dgradient(&mut self, r: Rect, radius: i32, from: u32, to: u32) {
        let span = (r.w + r.h).max(1);
        self.fill_round_rect_with(r, radius, |x, y| mix(from, to, ((x - r.x) + (y - r.y)) * 256 / span));
    }

    pub fn fill_round_rect_with(&mut self, r: Rect, radius: i32, color: impl Fn(i32, i32) -> u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2);
        let area = r.intersect(&self.clip);
        for y in area.y..area.bottom() {
            let in_band = y < r.y + radius || y >= r.bottom() - radius;
            for x in area.x..area.right() {
                let c = color(x, y);
                let mut a = alpha(c);
                if in_band && (x < r.x + radius || x >= r.right() - radius) {
                    a = a * round_rect_coverage(&r, radius, x, y) >> 8;
                }
                self.put(x, y, c, a);
            }
        }
    }

    /// Fills only the bottom corners rounded (for window content under a title bar).
    pub fn fill_rect_round_bottom(&mut self, r: Rect, radius: i32, c: u32) {
        let top = Rect::new(r.x, r.y, r.w, (r.h - radius).max(0));
        self.fill_rect(top, c);
        let bottom = Rect::new(r.x, r.bottom() - 2 * radius, r.w, 2 * radius);
        let band = Rect::new(r.x, r.bottom() - radius, r.w, radius);
        self.with_clip(band, |cv| cv.fill_round_rect(bottom, radius, c));
    }

    /// A 1px anti-aliased outline of a rounded rectangle.
    pub fn stroke_round_rect(&mut self, r: Rect, radius: i32, c: u32) {
        let inner = r.inset(1);
        let area = r.intersect(&self.clip);
        let base = alpha(c);
        let ri = (radius - 1).max(0);
        for y in area.y..area.bottom() {
            let edge_row = y == r.y || y == r.bottom() - 1;
            let in_band = y < r.y + radius || y >= r.bottom() - radius;
            if !edge_row && !in_band {
                // Middle rows: only the two side pixels.
                self.put_clipped(r.x, y, c, base);
                self.put_clipped(r.right() - 1, y, c, base);
                continue;
            }
            for x in area.x..area.right() {
                let outer = round_rect_coverage(&r, radius, x, y);
                let inn = if inner.contains(x, y) { round_rect_coverage(&inner, ri, x, y) } else { 0 };
                let cov = outer.saturating_sub(inn);
                if cov > 0 {
                    self.put(x, y, c, base * cov >> 8);
                }
            }
        }
    }

    fn put_clipped(&mut self, x: i32, y: i32, c: u32, a: u32) {
        if self.clip.contains(x, y) {
            self.put(x, y, c, a);
        }
    }

    /// Anti-aliased filled circle; centre and radius in pixels (centre on a pixel corner).
    pub fn fill_circle(&mut self, cx: i32, cy: i32, radius: i32, c: u32) {
        let r = Rect::new(cx - radius, cy - radius, radius * 2, radius * 2);
        self.fill_round_rect(r, radius, c);
    }

    /// A soft drop shadow around `r` (drawn outside the shape only).
    pub fn shadow(&mut self, r: Rect, radius: i32, blur: i32, offset_y: i32, strength: u32) {
        let s = r.offset(0, offset_y);
        let bounds = Rect::new(s.x - blur, s.y - blur, s.w + 2 * blur, s.h + 2 * blur);
        let area = bounds.intersect(&self.clip);
        if area.is_empty() {
            return;
        }
        let blur256 = (blur * 256) as i64;
        let falloff = |d256: i64| -> u32 {
            // d256: distance outside the shape in 1/256 px. Quadratic falloff.
            if d256 <= 0 {
                return strength;
            }
            if d256 >= blur256 {
                return 0;
            }
            let t = ((blur256 - d256) * 256 / blur256) as u32; // 256 → 0
            strength * t * t >> 16
        };
        let rad = radius.max(1);
        // The interior of the window itself is painted on top; skip it.
        let hole = r.inset(radius / 2 + 1);
        for y in area.y..area.bottom() {
            let row_hole = y >= hole.y && y < hole.bottom();
            for x in area.x..area.right() {
                if row_hole && x >= hole.x && x < hole.right() {
                    continue;
                }
                // Signed distance to the rounded rectangle s.
                let px = x * 256 + 128;
                let py = y * 256 + 128;
                let qx = (px - (s.x + s.w / 2) * 256).abs() - (s.w / 2 - rad) * 256;
                let qy = (py - (s.y + s.h / 2) * 256).abs() - (s.h / 2 - rad) * 256;
                let ox = qx.max(0) as i64;
                let oy = qy.max(0) as i64;
                let outside = if ox == 0 {
                    oy
                } else if oy == 0 {
                    ox
                } else {
                    isqrt((ox * ox + oy * oy) as u64) as i64
                };
                let d = outside + (qx.max(qy).min(0)) as i64 - rad as i64 * 256;
                let a = falloff(d);
                if a > 0 {
                    self.put(x, y, 0, a);
                }
            }
        }
    }

    /// Copies pixels from a full-screen source buffer (same dimensions) within `r`.
    pub fn blit_same(&mut self, src: &[u32], r: Rect) {
        let r = r.intersect(&self.clip);
        for y in r.y..r.bottom() {
            let a = (y * self.width + r.x) as usize;
            let b = (y * self.width + r.right()) as usize;
            self.buf[a..b].copy_from_slice(&src[a..b]);
        }
    }

    /// Copies an opaque `src_w`×`src_h` image (row stride = `src_w`) to `dst`'s
    /// top-left corner, clipped to `dst` and the canvas clip. With `radius > 0`
    /// the bottom corners are rounded (anti-aliased) — used for window contents.
    pub fn blit(&mut self, src: &[u32], src_w: u32, src_h: u32, dst: Rect, radius: i32) {
        let img = Rect::new(dst.x, dst.y, (src_w as i32).min(dst.w), (src_h as i32).min(dst.h));
        let straight = if radius > 0 { Rect::new(img.x, img.y, img.w, dst.h - radius) } else { img };
        let area = img.intersect(&straight).intersect(&self.clip);
        for y in area.y..area.bottom() {
            let s0 = ((y - dst.y) as u32 * src_w + (area.x - dst.x) as u32) as usize;
            let d0 = (y * self.width + area.x) as usize;
            let n = area.w as usize;
            self.buf[d0..d0 + n].copy_from_slice(&src[s0..s0 + n]);
        }
        if radius > 0 {
            let band = Rect::new(dst.x, dst.bottom() - radius, dst.w, radius);
            let shape = Rect::new(dst.x, dst.bottom() - 2 * radius, dst.w, 2 * radius);
            self.with_clip(band.intersect(&img), |cv| {
                cv.fill_round_rect_with(shape, radius, |x, y| {
                    0xFF00_0000 | src[((y - dst.y) as u32 * src_w + (x - dst.x) as u32) as usize]
                })
            });
        }
    }

    /// Frosted glass: the pre-blurred wallpaper under a rounded rect, tinted.
    pub fn glass(&mut self, blurred: &[u32], r: Rect, radius: i32, tint: u32) {
        let w = self.width;
        let ta = alpha(tint);
        self.fill_round_rect_with(r, radius, |x, y| blend(blurred[(y * w + x) as usize], tint, ta));
    }

    /// Draws `s` with its baseline at `y`. Returns the advance in pixels.
    pub fn text(&mut self, x: i32, y: i32, s: &str, font: Font, c: u32) -> i32 {
        let base_a = alpha(c);
        let mut pen = x << 6;
        for ch in s.chars() {
            let g = font.glyph(ch);
            let gx = (pen + 32 >> 6) + g.xmin as i32;
            let gy = y - g.ymin as i32 - g.h as i32;
            let gr = Rect::new(gx, gy, g.w as i32, g.h as i32);
            let vis = gr.intersect(&self.clip);
            if !vis.is_empty() {
                let bmp = font.bitmap_of(g);
                for py in vis.y..vis.bottom() {
                    for px in vis.x..vis.right() {
                        let cov = bmp[((py - gy) * g.w as i32 + (px - gx)) as usize] as u32;
                        if cov != 0 {
                            self.put(px, py, c, cov * base_a / 255);
                        }
                    }
                }
            }
            pen += g.adv64 as i32;
        }
        ((pen + 32) >> 6) - x
    }

    /// Draws text centred horizontally in `r`, vertically centred on its cap height.
    pub fn text_centered(&mut self, r: Rect, s: &str, font: Font, c: u32) {
        let w = font.width(s);
        let base = r.y + (r.h + font.ascent as i32 + font.descent as i32) / 2;
        self.text(r.x + (r.w - w) / 2, base, s, font, c);
    }

    /// Text truncated with an ellipsis to fit `max_w`.
    pub fn text_clipped(&mut self, x: i32, y: i32, s: &str, font: Font, c: u32, max_w: i32) -> i32 {
        if font.width(s) <= max_w {
            return self.text(x, y, s, font, c);
        }
        let ell = font.width("…");
        let mut end = 0;
        let mut w = 0;
        for (i, ch) in s.char_indices() {
            let a = font.advance(ch);
            if w + a + ell > max_w {
                break;
            }
            w += a;
            end = i + ch.len_utf8();
        }
        let adv = self.text(x, y, &s[..end], font, c);
        adv + self.text(x + adv, y, "…", font, c)
    }

    /// Anti-aliased thick polyline, blended once per pixel (no darkened joints).
    pub fn polyline(&mut self, pts: &[(i32, i32)], width: i32, c: u32) {
        if pts.len() < 2 {
            return;
        }
        let pad = width / 2 + 2;
        let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        let area = Rect::new(x0 - pad, y0 - pad, x1 - x0 + 2 * pad, y1 - y0 + 2 * pad).intersect(&self.clip);
        let hw = (width * 8) as i64; // half width, 1/16 px
        let base = alpha(c);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let px = (x * 16 + 8) as i64;
                let py = (y * 16 + 8) as i64;
                let mut best = i64::MAX;
                for seg in pts.windows(2) {
                    let (ax, ay) = (seg[0].0 as i64 * 16, seg[0].1 as i64 * 16);
                    let (bx, by) = (seg[1].0 as i64 * 16, seg[1].1 as i64 * 16);
                    let lim = hw + 32;
                    if px < ax.min(bx) - lim || px > ax.max(bx) + lim || py < ay.min(by) - lim || py > ay.max(by) + lim
                    {
                        continue;
                    }
                    let (dx, dy) = (bx - ax, by - ay);
                    let len2 = (dx * dx + dy * dy).max(1);
                    let t = (((px - ax) * dx + (py - ay) * dy) * 1024 / len2).clamp(0, 1024);
                    let qx = px - ax - dx * t / 1024;
                    let qy = py - ay - dy * t / 1024;
                    best = best.min(qx * qx + qy * qy);
                }
                if best == i64::MAX {
                    continue;
                }
                let d = isqrt(best as u64) as i64;
                let cov = ((hw + 8 - d) * 16).clamp(0, 256) as u32;
                if cov > 0 {
                    self.put(x, y, c, base * cov >> 8);
                }
            }
        }
    }

    /// Anti-aliased thick line segment (round caps).
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, c: u32) {
        // Work in 1/16 px.
        let (ax, ay, bx, by) = (x0 * 16, y0 * 16, x1 * 16, y1 * 16);
        let hw = width * 8; // half width in 1/16 px
        let pad = width / 2 + 2;
        let bounds =
            Rect::new(x0.min(x1) - pad, y0.min(y1) - pad, (x0 - x1).abs() + 2 * pad, (y0 - y1).abs() + 2 * pad);
        let area = bounds.intersect(&self.clip);
        let (dx, dy) = ((bx - ax) as i64, (by - ay) as i64);
        let len2 = (dx * dx + dy * dy).max(1);
        let base = alpha(c);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let px = (x * 16 + 8 - ax) as i64;
                let py = (y * 16 + 8 - ay) as i64;
                let t = ((px * dx + py * dy) * 1024 / len2).clamp(0, 1024);
                let qx = px - dx * t / 1024;
                let qy = py - dy * t / 1024;
                let d = isqrt((qx * qx + qy * qy) as u64) as i64; // 1/16 px
                let cov = ((hw as i64 + 8 - d) * 16).clamp(0, 256) as u32;
                if cov > 0 {
                    self.put(x, y, c, base * cov >> 8);
                }
            }
        }
    }
}
