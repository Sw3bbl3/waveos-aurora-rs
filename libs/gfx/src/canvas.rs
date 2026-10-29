//! Software rasterizer: clipped, alpha-blended drawing into a 32-bit buffer.
//!
//! Colors are `0xAARRGGBB`. All drawing is clipped to `clip`, which the
//! compositor sets to the damaged region being repainted. Anti-aliasing uses
//! 8-bit coverage computed from integer distance fields.

use crate::font::Font;
use crate::geom::Rect;
use crate::math::{atan2, cos, isqrt, sin};

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
    let da = alpha(dst);
    if da < 255 {
        let oa = a + da * inv / 255;
        if oa == 0 {
            return 0;
        }
        let channel = |shift: u32| {
            ((((src >> shift) & 255) * a * 255 + ((dst >> shift) & 255) * da * inv) / (oa * 255)).min(255) << shift
        };
        return oa << 24 | channel(16) | channel(8) | channel(0);
    }
    let rb = ((src & 0x00FF_00FF) * a + (dst & 0x00FF_00FF) * inv + 0x0080_0080) >> 8 & 0x00FF_00FF;
    let g = ((src & 0x0000_FF00) * a + (dst & 0x0000_FF00) * inv + 0x0000_8000) >> 8 & 0x0000_FF00;
    0xFF00_0000 | rb | g
}

/// Linear interpolation between two colors, t in 0..=256.
#[inline]
pub fn mix(a: u32, b: u32, t: i32) -> u32 {
    // Two channels per multiply: each 16-bit lane holds one channel's
    // weighted sum (at most 255 × 256), so lanes never carry into each other.
    let t = t.clamp(0, 256) as u32;
    let u = 256 - t;
    let rb = (((a & 0x00FF_00FF) * u + (b & 0x00FF_00FF) * t) >> 8) & 0x00FF_00FF;
    let ag = ((a >> 8) & 0x00FF_00FF) * u + ((b >> 8) & 0x00FF_00FF) * t;
    rb | (ag & 0xFF00_FF00)
}

/// Blends the solid colour `c` (0xRRGGBB) at opacity `a` over every pixel
/// of `row`. Same result as [`blend`] per pixel, with the source terms
/// computed once for the row.
#[inline]
pub fn blend_row(row: &mut [u32], c: u32, a: u32) {
    if a == 0 {
        return;
    }
    if a >= 255 {
        row.fill(c | 0xFF00_0000);
        return;
    }
    #[cfg(target_arch = "x86_64")]
    if row.len() >= 8 && crate::simd::enabled() {
        let mut i = 0;
        loop {
            // SAFETY: `enabled` said SSE2 may be used here.
            i += unsafe { crate::simd::blend_row(&mut row[i..], c, a) };
            if row.len() - i < 4 {
                break;
            }
            // The group it stopped at has transparency: blend it exactly.
            for p in &mut row[i..i + 4] {
                *p = blend(*p, c, a);
            }
            i += 4;
        }
        for p in &mut row[i..] {
            *p = blend(*p, c, a);
        }
        return;
    }
    let inv = 255 - a;
    let rb = (c & 0x00FF_00FF) * a + 0x0080_0080;
    let g = (c & 0x0000_FF00) * a + 0x0000_8000;
    for p in row {
        let d = *p;
        *p = if d >> 24 == 255 {
            0xFF00_0000
                | (((d & 0x00FF_00FF) * inv + rb) >> 8 & 0x00FF_00FF)
                | (((d & 0x0000_FF00) * inv + g) >> 8 & 0x0000_FF00)
        } else {
            blend(d, c, a)
        };
    }
}

/// Blends black at a per-pixel opacity (`shade[i]`, 0..=255) over `row`.
#[inline]
pub fn shade_row(row: &mut [u32], shade: &[u32]) {
    #[allow(unused_mut)] // Only the x86 SIMD path advances this cursor.
    let mut i = 0;
    #[cfg(target_arch = "x86_64")]
    if row.len() >= 8 && crate::simd::enabled() {
        loop {
            // SAFETY: `enabled` said SSE2 may be used here; `shade` matches `row`.
            i += unsafe { crate::simd::shade_row(&mut row[i..], &shade[i..]) };
            if row.len() - i < 4 {
                break;
            }
            for k in i..i + 4 {
                if shade[k] > 0 {
                    row[k] = blend(row[k], 0, shade[k]);
                }
            }
            i += 4;
        }
    }
    for k in i..row.len() {
        if shade[k] > 0 {
            row[k] = blend(row[k], 0, shade[k]);
        }
    }
}

/// A pixel buffer to draw into.
///
/// Drawing is in points: on a HiDPI screen (`scale` 2) a point is 2×2 device
/// pixels, and shapes, text and images are drawn at the full device
/// resolution. `buf`, `width` and `height` are in device pixels; `clip` and
/// every drawing call's coordinates are in points.
pub struct Canvas<'a> {
    pub materials: Option<&'a mut crate::material::MaterialCache>,
    pub buf: &'a mut [u32],
    /// Width of `buf` in device pixels (the row stride).
    pub width: i32,
    #[allow(dead_code)]
    pub height: i32,
    /// Where drawing lands, in points.
    pub clip: Rect,
    /// Device pixels per point.
    pub scale: i32,
    /// A clip in device pixels that overrides `clip` (internal, see `with_pclip`).
    px_clip: Option<Rect>,
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
        Self { buf, width, height, clip: Rect::new(0, 0, width, height), materials: None, scale: 1, px_clip: None }
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

    /// Draws at `scale` device pixels per point.
    pub fn with_scale(mut self, scale: i32) -> Self {
        self.scale = scale.max(1);
        self.clip = Rect::new(0, 0, self.width / self.scale, self.height / self.scale);
        self
    }

    /// The canvas size in points.
    pub fn size(&self) -> (i32, i32) {
        (self.width / self.scale, self.height / self.scale)
    }

    /// `r` (in points) in device pixels.
    #[inline]
    pub fn to_px(&self, r: Rect) -> Rect {
        let s = self.scale;
        if s == 1 {
            r
        } else {
            Rect::new(r.x * s, r.y * s, r.w * s, r.h * s)
        }
    }

    /// The colour already drawn at point (x, y) (its top-left device pixel).
    pub fn pixel(&self, x: i32, y: i32) -> u32 {
        self.buf[(y * self.scale * self.width + x * self.scale) as usize]
    }

    /// The clip in device pixels.
    #[inline]
    pub fn pclip(&self) -> Rect {
        match self.px_clip {
            Some(r) => r,
            None => self.to_px(self.clip),
        }
    }

    /// Runs `f` with the device-pixel clip narrowed to `r`.
    fn with_pclip(&mut self, r: Rect, f: impl FnOnce(&mut Self)) {
        let saved = self.px_clip;
        let clip = self.pclip().intersect(&r);
        if !clip.is_empty() {
            self.px_clip = Some(clip);
            f(self);
        }
        self.px_clip = saved;
    }

    // ------------------------------------------------- drawing, in points

    pub fn fill_rect(&mut self, r: Rect, c: u32) {
        self.fill_rect_px(self.to_px(r), c)
    }

    /// Fills `r` with a colour (and alpha) computed per point.
    pub fn fill_rect_with(&mut self, r: Rect, color: impl Fn(i32, i32) -> u32) {
        let s = self.scale;
        self.fill_rect_with_px(self.to_px(r), |x, y| color(x.div_euclid(s), y.div_euclid(s)))
    }

    pub fn fill_round_rect(&mut self, r: Rect, radius: i32, c: u32) {
        self.fill_round_rect_px(self.to_px(r), radius * self.scale, c)
    }

    pub fn fill_round_rect_with(&mut self, r: Rect, radius: i32, color: impl Fn(i32, i32) -> u32) {
        let s = self.scale;
        self.fill_round_rect_with_px(self.to_px(r), radius * s, |x, y| color(x.div_euclid(s), y.div_euclid(s)))
    }

    /// An anti-aliased outline of a rounded rectangle, one point wide.
    pub fn stroke_round_rect(&mut self, r: Rect, radius: i32, c: u32) {
        let (s, px) = (self.scale, self.to_px(r));
        for i in 0..s {
            self.stroke_round_rect_px(px.inset(i), (radius * s - i).max(0), c);
        }
    }

    /// A soft drop shadow around `r` (drawn outside the shape only).
    pub fn shadow(&mut self, r: Rect, radius: i32, blur: i32, offset_y: i32, strength: u32) {
        let s = self.scale;
        self.shadow_px(self.to_px(r), radius * s, blur * s, offset_y * s, strength)
    }

    /// Copies pixels from a full-screen source buffer (same dimensions as the
    /// canvas, in device pixels) within `r`.
    pub fn blit_same(&mut self, src: &[u32], r: Rect) {
        self.blit_same_px(src, self.to_px(r))
    }

    /// Copies an opaque `src_w`×`src_h` image (device pixels, row stride =
    /// `src_w`) 1:1 to `dst`'s top-left corner, clipped to `dst` and the
    /// canvas clip. With `radius > 0` all four corners of `dst` are rounded
    /// (anti-aliased) — window contents.
    pub fn blit(&mut self, src: &[u32], src_w: u32, src_h: u32, dst: Rect, radius: i32) {
        self.blit_px(src, src_w, src_h, self.to_px(dst), radius * self.scale)
    }

    /// Copies opaque pixels covering `part` (row stride `part.w`, in device
    /// pixels) into the canvas, inside `shape` with its corners rounded.
    pub fn blit_part_rounded(&mut self, src: &[u32], part: Rect, shape: Rect, radius: i32) {
        self.blit_part_rounded_px(src, self.to_px(part), self.to_px(shape), radius * self.scale)
    }

    /// Like [`blit`](Self::blit) for a surface with transparency (glass regions).
    pub fn blit_alpha(&mut self, src: &[u32], sw: u32, sh: u32, dst: Rect, radius: i32) {
        self.blit_alpha_px(src, sw, sh, self.to_px(dst), radius * self.scale)
    }

    /// Draws a `src_w`×`src_h` image with straight alpha scaled into `dst`
    /// (bilinear filtering, premultiplied so transparent edges don't fringe),
    /// at `opacity` 0..=255. For large reductions, draw a thumbnail instead.
    pub fn draw_image(&mut self, src: &[u32], src_w: u32, src_h: u32, dst: Rect, opacity: u32) {
        self.draw_image_px(src, src_w, src_h, self.to_px(dst), opacity)
    }

    /// Draws an image scaled into `dst` (as [`draw_image`](Self::draw_image)),
    /// showing only what falls inside `shape` with its corners rounded at
    /// `radius` (anti-aliased): thumbnails and photo cards.
    pub fn draw_image_rounded(&mut self, src: &[u32], sw: u32, sh: u32, dst: Rect, shape: Rect, radius: i32) {
        let (dst, shape) = (self.to_px(dst), self.to_px(shape));
        let radius = (radius * self.scale).min(shape.w / 2).min(shape.h / 2).max(0);
        let clip = self.pclip().intersect(&shape);
        if clip.is_empty() {
            return;
        }
        // What lies under the four corners, to blend back outside the curve.
        let corners = [
            Rect::new(shape.x, shape.y, radius, radius),
            Rect::new(shape.right() - radius, shape.y, radius, radius),
            Rect::new(shape.x, shape.bottom() - radius, radius, radius),
            Rect::new(shape.right() - radius, shape.bottom() - radius, radius, radius),
        ]
        .map(|c| c.intersect(&clip));
        let saved: alloc::vec::Vec<alloc::vec::Vec<u32>> = corners
            .iter()
            .map(|c| {
                let mut v = alloc::vec::Vec::with_capacity((c.w * c.h).max(0) as usize);
                for y in c.y..c.bottom() {
                    let row = (y * self.width) as usize;
                    v.extend_from_slice(&self.buf[row + c.x as usize..row + c.right() as usize]);
                }
                v
            })
            .collect();
        self.with_pclip(clip, |cv| cv.draw_image_px(src, sw, sh, dst, 255));
        for (c, old) in corners.iter().zip(&saved) {
            for (k, y) in (c.y..c.bottom()).enumerate() {
                for (j, x) in (c.x..c.right()).enumerate() {
                    let cov = round_rect_coverage(&shape, radius, x, y);
                    let i = (y * self.width + x) as usize;
                    self.buf[i] = mix(old[k * c.w as usize + j], self.buf[i], cov as i32);
                }
            }
        }
    }

    /// Copies opaque pixels at (x, y), a pixel per point, clipped (video frames).
    pub fn blit_opaque(&mut self, src: &[u32], src_w: u32, src_h: u32, x: i32, y: i32) {
        let s = self.scale;
        if s == 1 {
            return self.blit_opaque_px(src, src_w, src_h, x, y);
        }
        let dst = self.to_px(Rect::new(x, y, src_w as i32, src_h as i32));
        let vis = dst.intersect(&self.pclip());
        if vis.is_empty() || src.len() < (src_w * src_h) as usize {
            return;
        }
        for row in vis.y..vis.bottom() {
            let s0 = ((row - dst.y) / s) as usize * src_w as usize;
            let d0 = (row * self.width) as usize;
            for col in vis.x..vis.right() {
                self.buf[d0 + col as usize] = src[s0 + ((col - dst.x) / s) as usize];
            }
        }
    }

    /// The grey checkerboard shown behind transparent images.
    pub fn checkerboard(&mut self, r: Rect, cell: i32, light: u32, dark: u32) {
        self.checkerboard_px(self.to_px(r), cell * self.scale, light, dark)
    }

    /// The font that draws `font` at this canvas's scale.
    fn device_font(&self, font: Font) -> Font {
        if self.scale == 1 {
            font
        } else {
            crate::font::get(font.face, font.size.saturating_mul(self.scale as u16))
        }
    }

    /// Draws `s` with its baseline at `y`. Returns the advance in points.
    pub fn text(&mut self, x: i32, y: i32, s: &str, font: Font, c: u32) -> i32 {
        let k = self.scale;
        let f = self.device_font(font);
        (self.text_px(x * k, y * k, s, f, c) + k / 2) / k
    }

    /// Draws `s` slanted about 12° (a synthesized italic). Returns the advance in points.
    pub fn text_slanted(&mut self, x: i32, y: i32, s: &str, font: Font, c: u32) -> i32 {
        let k = self.scale;
        let f = self.device_font(font);
        (self.text_slanted_px(x * k, y * k, s, f, c) + k / 2) / k
    }

    /// Anti-aliased thick polyline, blended once per pixel (no darkened joints).
    pub fn polyline(&mut self, pts: &[(i32, i32)], width: i32, c: u32) {
        let s = self.scale;
        if s == 1 {
            return self.polyline_px(pts, width, c);
        }
        let pts: alloc::vec::Vec<(i32, i32)> = pts.iter().map(|&(x, y)| (x * s, y * s)).collect();
        self.polyline_px(&pts, width * s, c)
    }

    /// Anti-aliased arc with round caps. Centre and radius are in 1/16 point
    /// so small rings sit exactly; angles use 1024 units per turn (0 at 3
    /// o'clock, growing clockwise) and the arc runs from `start` for `sweep`.
    #[allow(clippy::too_many_arguments)] // a circle, a pen and an angle range
    pub fn stroke_arc(&mut self, cx: i32, cy: i32, r: i32, width: i32, start: i32, sweep: i32, c: u32) {
        let s = self.scale;
        self.stroke_arc_px(cx * s, cy * s, r * s, width * s, start, sweep, c)
    }

    /// Anti-aliased thick line segment (round caps).
    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, c: u32) {
        let s = self.scale;
        self.line_px(x0 * s, y0 * s, x1 * s, y1 * s, width * s, c)
    }

    // ---------------------------------------- drawing, in device pixels

    #[inline]
    fn put(&mut self, x: i32, y: i32, c: u32, a: u32) {
        let i = (y * self.width + x) as usize;
        self.buf[i] = blend(self.buf[i], c, a);
    }

    fn fill_rect_px(&mut self, r: Rect, c: u32) {
        let r = r.intersect(&self.pclip());
        if r.is_empty() {
            return;
        }
        let a = alpha(c);
        for y in r.y..r.bottom() {
            let row = &mut self.buf[(y * self.width + r.x) as usize..(y * self.width + r.right()) as usize];
            if a == 255 {
                row.fill(c);
            } else {
                blend_row(row, c, a);
            }
        }
    }

    /// Fills `r` with a colour (and alpha) computed per pixel.
    fn fill_rect_with_px(&mut self, r: Rect, color: impl Fn(i32, i32) -> u32) {
        let area = r.intersect(&self.pclip());
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let c = color(x, y);
                self.put(x, y, c, alpha(c));
            }
        }
    }

    fn fill_round_rect_px(&mut self, r: Rect, radius: i32, c: u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2);
        let area = r.intersect(&self.pclip());
        if area.is_empty() {
            return;
        }
        let a = alpha(c);
        // Rows clear of the corners are one solid span; corner rows blend
        // their anti-aliased ends pixel by pixel and the middle as a span.
        let inner_l = (r.x + radius).clamp(area.x, area.right());
        let inner_r = (r.right() - radius).clamp(inner_l, area.right());
        for y in area.y..area.bottom() {
            let base = (y * self.width) as usize;
            if y >= r.y + radius && y < r.bottom() - radius {
                blend_row(&mut self.buf[base + area.x as usize..base + area.right() as usize], c, a);
                continue;
            }
            for x in (area.x..inner_l).chain(inner_r..area.right()) {
                self.put(x, y, c, (a * round_rect_coverage(&r, radius, x, y)) >> 8);
            }
            if inner_l < inner_r {
                blend_row(&mut self.buf[base + inner_l as usize..base + inner_r as usize], c, a);
            }
        }
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

    fn fill_round_rect_with_px(&mut self, r: Rect, radius: i32, color: impl Fn(i32, i32) -> u32) {
        let radius = radius.min(r.w / 2).min(r.h / 2);
        let area = r.intersect(&self.pclip());
        for y in area.y..area.bottom() {
            let in_band = y < r.y + radius || y >= r.bottom() - radius;
            for x in area.x..area.right() {
                let c = color(x, y);
                let mut a = alpha(c);
                if in_band && (x < r.x + radius || x >= r.right() - radius) {
                    a = (a * round_rect_coverage(&r, radius, x, y)) >> 8;
                }
                self.put(x, y, c, a);
            }
        }
    }

    /// A 1px anti-aliased outline of a rounded rectangle.
    fn stroke_round_rect_px(&mut self, r: Rect, radius: i32, c: u32) {
        if radius < 0 || 2 * radius > r.w || 2 * radius > r.h {
            return self.stroke_round_rect_slow(r, radius, c);
        }
        let inner = r.inset(1);
        let area = r.intersect(&self.pclip());
        let base = alpha(c);
        let ri = (radius - 1).max(0);
        // Only the corner columns need coverage: between them the top and
        // bottom rows are solid and every other row is empty inside.
        let corner_l = (r.x + radius).clamp(area.x, area.right());
        let corner_r = (r.right() - radius).clamp(corner_l, area.right());
        for y in area.y..area.bottom() {
            let edge_row = y == r.y || y == r.bottom() - 1;
            let in_band = y < r.y + radius || y >= r.bottom() - radius;
            if !edge_row && !in_band {
                self.put_clipped(r.x, y, c, base);
                self.put_clipped(r.right() - 1, y, c, base);
                continue;
            }
            for x in (area.x..corner_l).chain(corner_r..area.right()) {
                let outer = round_rect_coverage(&r, radius, x, y);
                let inn = if inner.contains(x, y) { round_rect_coverage(&inner, ri, x, y) } else { 0 };
                let cov = outer.saturating_sub(inn);
                if cov > 0 {
                    self.put(x, y, c, (base * cov) >> 8);
                }
            }
            if edge_row {
                let row = (y * self.width) as usize;
                blend_row(&mut self.buf[row + corner_l as usize..row + corner_r as usize], c, base);
            }
        }
    }

    fn stroke_round_rect_slow(&mut self, r: Rect, radius: i32, c: u32) {
        let inner = r.inset(1);
        let area = r.intersect(&self.pclip());
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
                    self.put(x, y, c, (base * cov) >> 8);
                }
            }
        }
    }

    fn put_clipped(&mut self, x: i32, y: i32, c: u32, a: u32) {
        if self.pclip().contains(x, y) {
            self.put(x, y, c, a);
        }
    }

    /// Anti-aliased filled circle; centre and radius in pixels (centre on a pixel corner).
    pub fn fill_circle(&mut self, cx: i32, cy: i32, radius: i32, c: u32) {
        let r = Rect::new(cx - radius, cy - radius, radius * 2, radius * 2);
        self.fill_round_rect(r, radius, c);
    }

    /// A soft drop shadow around `r` (drawn outside the shape only).
    ///
    /// The shade only depends on how far a pixel lies past the rounded
    /// rectangle's straight sides (`u`, `v`, in whole pixels from where the
    /// corner arcs begin), so it is tabulated once per call: a profile for
    /// the edges and a small table for the corners, instead of a square root
    /// per pixel. The interior, which the window covers, is skipped by spans.
    fn shadow_px(&mut self, r: Rect, radius: i32, blur: i32, offset_y: i32, strength: u32) {
        // Blending treats any opacity from 255 up as solid.
        let strength = strength.min(255);
        let s = r.offset(0, offset_y);
        let bounds = Rect::new(s.x - blur, s.y - blur, s.w + 2 * blur, s.h + 2 * blur);
        let area = bounds.intersect(&self.pclip());
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
            (strength * t * t) >> 16
        };
        let rad = radius.max(1);
        // Past the straight part by u pixels: qx = u * 256 + 128 (and the same for v).
        let n = (rad + blur + 1).max(1) as usize;
        let edge: alloc::vec::Vec<u32> = (0..n).map(|u| falloff((u as i64 * 256 + 128) - rad as i64 * 256)).collect();
        let mut corner = alloc::vec![0u32; n * n];
        for v in 0..n {
            let oy = v as i64 * 256 + 128;
            for u in 0..n {
                let ox = u as i64 * 256 + 128;
                corner[v * n + u] = falloff(crate::math::isqrt((ox * ox + oy * oy) as u64) as i64 - rad as i64 * 256);
            }
        }
        // Columns (rows) mirror about the centre line x = cx (y = cy).
        let (cx, cy) = (s.x + s.w / 2, s.y + s.h / 2);
        let (ex, ey) = (s.w / 2 - rad, s.h / 2 - rad);
        let past = |p: i32, c: i32, e: i32| -> i32 { (if p >= c { p - c } else { c - 1 - p }) - e };
        // The interior of the window itself is painted on top; skip it.
        let hole = r.inset(radius / 2 + 1);
        let mut shade = alloc::vec::Vec::with_capacity(area.w as usize);
        for y in area.y..area.bottom() {
            let v = past(y, cy, ey);
            let row_hole = y >= hole.y && y < hole.bottom();
            let (skip_l, skip_r) = if row_hole {
                let l = hole.x.clamp(area.x, area.right());
                (l, hole.right().clamp(l, area.right()))
            } else {
                (area.x, area.x)
            };
            let base = (y * self.width) as usize;
            // Along the straight middle (u < 0) a row's shade is constant.
            let mid = if v >= 0 { edge.get(v as usize).copied().unwrap_or(0) } else { strength };
            let (mid_l, mid_r) = (cx - ex, cx + ex);
            for (x0, x1) in [(area.x, skip_l), (skip_r, area.right())] {
                if x0 >= x1 {
                    continue;
                }
                let spans = if mid_l < mid_r {
                    let (a, b) = (mid_l.clamp(x0, x1), mid_r.clamp(x0, x1));
                    [(x0, a, false), (a, b, true), (b, x1, false)]
                } else {
                    [(x0, x1, false), (x1, x1, false), (x1, x1, false)]
                };
                for (s0, s1, middle) in spans {
                    if s0 >= s1 {
                        continue;
                    }
                    let row = &mut self.buf[base + s0 as usize..base + s1 as usize];
                    if middle {
                        blend_row(row, 0, mid);
                        continue;
                    }
                    shade.clear();
                    shade.extend((s0..s1).map(|x| {
                        let u = past(x, cx, ex);
                        match (u >= 0, v >= 0) {
                            (true, true) => {
                                if u as usize >= n || v as usize >= n {
                                    0
                                } else {
                                    corner[v as usize * n + u as usize]
                                }
                            }
                            (true, false) => edge.get(u as usize).copied().unwrap_or(0),
                            (false, true) => edge.get(v as usize).copied().unwrap_or(0),
                            (false, false) => strength,
                        }
                    }));
                    shade_row(row, &shade);
                }
            }
        }
    }

    /// Copies pixels from a full-screen source buffer (same dimensions) within `r`.
    fn blit_same_px(&mut self, src: &[u32], r: Rect) {
        let r = r.intersect(&self.pclip());
        for y in r.y..r.bottom() {
            let a = (y * self.width + r.x) as usize;
            let b = (y * self.width + r.right()) as usize;
            self.buf[a..b].copy_from_slice(&src[a..b]);
        }
    }

    /// Copies an opaque `src_w`×`src_h` image (row stride = `src_w`) to `dst`'s
    /// top-left corner, clipped to `dst` and the canvas clip. With `radius > 0`
    /// all four corners of `dst` are rounded (anti-aliased) — window contents.
    fn blit_px(&mut self, src: &[u32], src_w: u32, src_h: u32, dst: Rect, radius: i32) {
        let img = Rect::new(dst.x, dst.y, (src_w as i32).min(dst.w), (src_h as i32).min(dst.h));
        let radius = radius.min(dst.w / 2).min(dst.h / 2).max(0);
        let straight = Rect::new(img.x, dst.y + radius, img.w, dst.h - 2 * radius);
        let area = img.intersect(&straight).intersect(&self.pclip());
        for y in area.y..area.bottom() {
            let s0 = ((y - dst.y) as u32 * src_w + (area.x - dst.x) as u32) as usize;
            let d0 = (y * self.width + area.x) as usize;
            let n = area.w as usize;
            self.buf[d0..d0 + n].copy_from_slice(&src[s0..s0 + n]);
        }
        if radius > 0 {
            for band in [Rect::new(dst.x, dst.y, dst.w, radius), Rect::new(dst.x, dst.bottom() - radius, dst.w, radius)]
            {
                self.with_pclip(band.intersect(&img), |cv| {
                    cv.fill_round_rect_with_px(dst, radius, |x, y| {
                        0xFF00_0000 | src[((y - dst.y) as u32 * src_w + (x - dst.x) as u32) as usize]
                    })
                });
            }
        }
    }

    /// Copies opaque pixels covering `part` (row stride `part.w`) into the
    /// canvas, inside `shape` with its corners rounded at `radius`.
    pub(crate) fn blit_part_rounded_px(&mut self, src: &[u32], part: Rect, shape: Rect, radius: i32) {
        let radius = radius.min(shape.w / 2).min(shape.h / 2);
        let area = part.intersect(&shape).intersect(&self.pclip());
        for y in area.y..area.bottom() {
            let s0 = ((y - part.y) * part.w + (area.x - part.x)) as usize;
            let d0 = (y * self.width + area.x) as usize;
            if y >= shape.y + radius && y < shape.bottom() - radius {
                self.buf[d0..d0 + area.w as usize].copy_from_slice(&src[s0..s0 + area.w as usize]);
                continue;
            }
            for x in area.x..area.right() {
                let c = src[s0 + (x - area.x) as usize];
                let mut a = alpha(c);
                if x < shape.x + radius || x >= shape.right() - radius {
                    a = (a * round_rect_coverage(&shape, radius, x, y)) >> 8;
                }
                self.put(x, y, c, a);
            }
        }
    }

    /// Like [`blit`](Self::blit) for a surface with transparency (glass regions).
    fn blit_alpha_px(&mut self, src: &[u32], sw: u32, sh: u32, dst: Rect, radius: i32) {
        let img = Rect::new(dst.x, dst.y, (sw as i32).min(dst.w), (sh as i32).min(dst.h));
        self.with_pclip(img, |cv| {
            cv.fill_round_rect_with_px(dst, radius, |x, y| src[((y - dst.y) as u32 * sw + (x - dst.x) as u32) as usize])
        });
    }

    /// Draws a `src_w`×`src_h` image with straight alpha scaled into `dst`
    /// (bilinear filtering, premultiplied so transparent edges don't fringe),
    /// at `opacity` 0..=255. For large reductions, draw a thumbnail instead.
    fn draw_image_px(&mut self, src: &[u32], src_w: u32, src_h: u32, dst: Rect, opacity: u32) {
        if dst.w <= 0 || dst.h <= 0 || src_w == 0 || src_h == 0 {
            return;
        }
        let vis = dst.intersect(&self.pclip());
        if vis.is_empty() {
            return;
        }
        let (sw, sh) = (src_w as i64, src_h as i64);
        // Source position of a destination pixel centre, 16.16, minus half a texel.
        let step_x = (sw << 16) / dst.w as i64;
        let step_y = (sh << 16) / dst.h as i64;
        let exact = dst.w as u32 == src_w && dst.h as u32 == src_h;
        if exact {
            // 1:1: blend each row straight from the source.
            for y in vis.y..vis.bottom() {
                let s0 = ((y - dst.y) as u32 * src_w + (vis.x - dst.x) as u32) as usize;
                let d0 = (y * self.width + vis.x) as usize;
                let n = vis.w as usize;
                for (d, &p) in self.buf[d0..d0 + n].iter_mut().zip(&src[s0..s0 + n]) {
                    let pa = (p >> 24) * opacity / 255;
                    if pa != 0 {
                        *d = blend(*d, p, pa);
                    }
                }
            }
            return;
        }
        let texel = |x: i64, y: i64| -> u32 { src[(y.clamp(0, sh - 1) * sw + x.clamp(0, sw - 1)) as usize] };
        for y in vis.y..vis.bottom() {
            let fy = (y - dst.y) as i64 * step_y + step_y / 2 - 32768;
            let (y0, ty) = (fy >> 16, (fy & 0xFFFF) >> 8);
            for x in vis.x..vis.right() {
                let p = if exact {
                    src[((y - dst.y) as u32 * src_w + (x - dst.x) as u32) as usize]
                } else {
                    let fx = (x - dst.x) as i64 * step_x + step_x / 2 - 32768;
                    let (x0, tx) = (fx >> 16, (fx & 0xFFFF) >> 8);
                    let w = [(256 - tx) * (256 - ty), tx * (256 - ty), (256 - tx) * ty, tx * ty];
                    let t = [texel(x0, y0), texel(x0 + 1, y0), texel(x0, y0 + 1), texel(x0 + 1, y0 + 1)];
                    let (mut a, mut r, mut g, mut b) = (0i64, 0i64, 0i64, 0i64);
                    for k in 0..4 {
                        let pa = (t[k] >> 24) as i64 * w[k];
                        a += pa;
                        r += (t[k] >> 16 & 0xFF) as i64 * pa;
                        g += (t[k] >> 8 & 0xFF) as i64 * pa;
                        b += (t[k] & 0xFF) as i64 * pa;
                    }
                    if a == 0 {
                        continue;
                    }
                    ((a >> 16) as u32) << 24 | ((r / a) as u32) << 16 | ((g / a) as u32) << 8 | (b / a) as u32
                };
                let pa = (p >> 24) * opacity / 255;
                if pa != 0 {
                    self.put(x, y, p, pa);
                }
            }
        }
    }

    /// Copies opaque pixels 1:1 at (x, y), clipped (video frames).
    fn blit_opaque_px(&mut self, src: &[u32], src_w: u32, src_h: u32, x: i32, y: i32) {
        let vis = Rect::new(x, y, src_w as i32, src_h as i32).intersect(&self.pclip());
        if vis.is_empty() || src.len() < (src_w * src_h) as usize {
            return;
        }
        for row in vis.y..vis.bottom() {
            let s = (row - y) as usize * src_w as usize + (vis.x - x) as usize;
            let d = (row * self.width + vis.x) as usize;
            self.buf[d..d + vis.w as usize].copy_from_slice(&src[s..s + vis.w as usize]);
        }
    }

    /// The grey checkerboard shown behind transparent images.
    fn checkerboard_px(&mut self, r: Rect, cell: i32, light: u32, dark: u32) {
        let vis = r.intersect(&self.pclip());
        for y in vis.y..vis.bottom() {
            for x in vis.x..vis.right() {
                let odd = ((x - r.x) / cell + (y - r.y) / cell) & 1 == 1;
                self.buf[(y * self.width + x) as usize] = if odd { dark } else { light };
            }
        }
    }

    /// Frosted glass: the pre-blurred wallpaper under a rounded rect, tinted.
    pub fn glass(&mut self, _blurred: &[u32], r: Rect, radius: i32, tint: u32) {
        self.frosted(r, radius, tint);
    }

    pub fn frosted(&mut self, r: Rect, radius: i32, tint: u32) {
        if let Some(cache) = self.materials.take() {
            cache.paint(self, r, radius, tint);
            self.materials = Some(cache);
        } else {
            crate::material::MaterialCache::default().paint(self, r, radius, tint);
        }
    }

    /// Draws `s` with its baseline at `y`. Returns the advance in pixels.
    fn text_px(&mut self, x: i32, y: i32, s: &str, font: Font, c: u32) -> i32 {
        let base_a = alpha(c);
        let clip = self.pclip();
        let adv = font.layout(s, |g| {
            let gr = Rect::new(x + g.x, y + g.y, g.w as i32, g.h as i32);
            let vis = gr.intersect(&clip);
            for py in vis.y..vis.bottom() {
                let row = ((py - gr.y) * g.w as i32) as usize;
                for px in vis.x..vis.right() {
                    let cov = g.coverage[row + (px - gr.x) as usize] as u32;
                    if cov != 0 {
                        self.put(px, py, c, cov * base_a / 255);
                    }
                }
            }
        });
        (adv + 32) >> 6
    }

    /// Draws `s` slanted about 12° (a synthesized italic). Returns the advance in pixels.
    fn text_slanted_px(&mut self, x: i32, y: i32, s: &str, font: Font, c: u32) -> i32 {
        let base_a = alpha(c);
        let clip = self.pclip();
        let adv = font.layout(s, |g| {
            for row in 0..g.h as i32 {
                let py = y + g.y + row;
                if py < clip.y || py >= clip.bottom() {
                    continue;
                }
                // Rows above the baseline lean right.
                let shift = (y - py) * 13 / 64;
                for col in 0..g.w as i32 {
                    let px = x + g.x + col + shift;
                    if px < clip.x || px >= clip.right() {
                        continue;
                    }
                    let cov = g.coverage[(row * g.w as i32 + col) as usize] as u32;
                    if cov != 0 {
                        self.put(px, py, c, cov * base_a / 255);
                    }
                }
            }
        });
        (adv + 32) >> 6
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
    fn polyline_px(&mut self, pts: &[(i32, i32)], width: i32, c: u32) {
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
        let area = Rect::new(x0 - pad, y0 - pad, x1 - x0 + 2 * pad, y1 - y0 + 2 * pad).intersect(&self.pclip());
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
                    self.put(x, y, c, (base * cov) >> 8);
                }
            }
        }
    }

    /// Anti-aliased arc with round caps. Centre and radius are in 1/16 px so
    /// small rings sit exactly; angles use 1024 units per turn (0 at 3
    /// o'clock, growing clockwise) and the arc runs from `start` for `sweep`.
    #[allow(clippy::too_many_arguments)] // a circle, a pen and an angle range
    fn stroke_arc_px(&mut self, cx: i32, cy: i32, r: i32, width: i32, start: i32, sweep: i32, c: u32) {
        if sweep <= 0 || r <= 0 {
            return;
        }
        let hw = width as i64 / 2;
        let reach = (r + width) / 16 + 2;
        let bounds = Rect::new(cx / 16 - reach, cy / 16 - reach, 2 * reach + 1, 2 * reach + 1);
        let area = bounds.intersect(&self.pclip());
        let full = sweep >= 1024;
        let end = start + sweep;
        let point =
            |a: i32| (cx as i64 + r as i64 * cos(a) as i64 / 16384, cy as i64 + r as i64 * sin(a) as i64 / 16384);
        let (s, e) = (point(start), point(end));
        let base = alpha(c);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let (px, py) = ((x * 16 + 8) as i64, (y * 16 + 8) as i64);
                let (dx, dy) = (px - cx as i64, py - cy as i64);
                let rel = (atan2(dy, dx) - start).rem_euclid(1024);
                let d = if full || rel <= sweep {
                    (isqrt((dx * dx + dy * dy) as u64) as i64 - r as i64).abs()
                } else {
                    let ds = (px - s.0) * (px - s.0) + (py - s.1) * (py - s.1);
                    let de = (px - e.0) * (px - e.0) + (py - e.1) * (py - e.1);
                    isqrt(ds.min(de) as u64) as i64
                };
                let cov = ((hw + 8 - d) * 16).clamp(0, 256) as u32;
                if cov > 0 {
                    self.put(x, y, c, (base * cov) >> 8);
                }
            }
        }
    }

    /// A soft coloured halo around a rounded rectangle (Crest's activity
    /// glow, the waterline). `strength` is the alpha at the edge.
    pub fn glow(&mut self, r: Rect, radius: i32, size: i32, c: u32, strength: u32) {
        for i in 1..=size {
            let falloff = (size - i + 1) as u32;
            let a = strength * falloff * falloff / (size * size).max(1) as u32;
            if a > 0 {
                self.stroke_round_rect(r.inset(-i), radius + i, (c & 0x00FF_FFFF) | a.min(255) << 24);
            }
        }
    }

    /// Anti-aliased thick line segment (round caps).
    fn line_px(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, c: u32) {
        // Work in 1/16 px.
        let (ax, ay, bx, by) = (x0 * 16, y0 * 16, x1 * 16, y1 * 16);
        let hw = width * 8; // half width in 1/16 px
        let pad = width / 2 + 2;
        let bounds =
            Rect::new(x0.min(x1) - pad, y0.min(y1) - pad, (x0 - x1).abs() + 2 * pad, (y0 - y1).abs() + 2 * pad);
        let area = bounds.intersect(&self.pclip());
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
                    self.put(x, y, c, (base * cov) >> 8);
                }
            }
        }
    }
}

/// Tight damage for an app surface. First frames and resizes are always complete.
pub fn changed_bounds(old: &[u32], new: &[u32], w: i32, h: i32, stride: i32, force: bool) -> Option<Rect> {
    if w <= 0 || h <= 0 {
        return None;
    }
    if force || old.len() != new.len() {
        return Some(Rect::new(0, 0, w, h));
    }
    let (mut left, mut top, mut right, mut bottom) = (w, h, 0, 0);
    for y in 0..h {
        let start = (y * stride) as usize;
        let a = &old[start..start + w as usize];
        let b = &new[start..start + w as usize];
        if a == b {
            continue;
        }
        top = top.min(y);
        bottom = y + 1;
        let l = a.iter().zip(b).position(|(a, b)| a != b).unwrap() as i32;
        let r = a.iter().zip(b).rposition(|(a, b)| a != b).unwrap() as i32 + 1;
        left = left.min(l);
        right = right.max(r);
    }
    (bottom > top).then_some(Rect::new(left, top, right - left, bottom - top))
}
#[cfg(test)]
mod damage_tests {
    use super::*;
    #[test]
    fn tight_damage_includes_all_changes_and_ignores_stride_padding() {
        let a = alloc::vec![0;120];
        let mut b = a.clone();
        assert_eq!(changed_bounds(&a, &b, 8, 10, 12, false), None);
        b[12 * 3 + 2] = 1;
        b[12 * 6 + 5] = 1;
        b[11] = 9;
        assert_eq!(changed_bounds(&a, &b, 8, 10, 12, false), Some(Rect::new(2, 3, 4, 4)));
        assert_eq!(changed_bounds(&[], &b, 8, 10, 12, true), Some(Rect::new(0, 0, 8, 10)));
    }

    /// The per-pixel shadow before it was tabulated: the reference.
    fn shadow_reference(cv: &mut Canvas, r: Rect, radius: i32, blur: i32, offset_y: i32, strength: u32) {
        let s = r.offset(0, offset_y);
        let bounds = Rect::new(s.x - blur, s.y - blur, s.w + 2 * blur, s.h + 2 * blur);
        let area = bounds.intersect(&cv.clip);
        let blur256 = (blur * 256) as i64;
        let falloff = |d256: i64| -> u32 {
            if d256 <= 0 {
                return strength;
            }
            if d256 >= blur256 {
                return 0;
            }
            let t = ((blur256 - d256) * 256 / blur256) as u32;
            (strength * t * t) >> 16
        };
        let rad = radius.max(1);
        let hole = r.inset(radius / 2 + 1);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if hole.contains(x, y) {
                    continue;
                }
                let px = x * 256 + 128;
                let py = y * 256 + 128;
                let qx = (px - (s.x + s.w / 2) * 256).abs() - (s.w / 2 - rad) * 256;
                let qy = (py - (s.y + s.h / 2) * 256).abs() - (s.h / 2 - rad) * 256;
                let (ox, oy) = (qx.max(0) as i64, qy.max(0) as i64);
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
                    cv.put(x, y, 0, a);
                }
            }
        }
    }

    #[test]
    fn fast_shadows_and_fills_match_the_reference() {
        let (w, h) = (96, 80);
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |n: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as i32
        };
        for case in 0..400 {
            let r = Rect::new(next(90) - 20, next(70) - 20, next(80) + 1, next(70) + 1);
            let clip = Rect::new(next(60), next(50), next(96) + 1, next(80) + 1);
            let radius = next(30);
            let start: alloc::vec::Vec<u32> = (0..w * h)
                .map(|i| if case % 3 == 0 { 0x8040_2010 + i as u32 } else { 0xFF10_2030 + i as u32 * 77 })
                .collect();
            let (mut a, mut b) = (start.clone(), start.clone());
            {
                let (mut ca, mut cb) = (Canvas::new(&mut a, w, h), Canvas::new(&mut b, w, h));
                ca.clip = clip.intersect(&Rect::new(0, 0, w, h));
                cb.clip = ca.clip;
                let (blur, off, strength) = (next(40) + 1, next(16), next(255) as u32 + 1);
                ca.shadow(r, radius, blur, off, strength);
                shadow_reference(&mut cb, r, radius, blur, off, strength);
                let c = (next(256) as u32) << 24 | 0x0033_99CC;
                ca.fill_round_rect(r, radius, c);
                cb.fill_round_rect_with(r, radius, |_, _| c);
                ca.fill_rect(r, c);
                let ar = alpha(c);
                let area = r.intersect(&cb.clip);
                for y in area.y..area.bottom() {
                    for x in area.x..area.right() {
                        if ar == 255 {
                            cb.buf[(y * w + x) as usize] = c;
                        } else {
                            cb.put(x, y, c, ar);
                        }
                    }
                }
            }
            assert!(a == b, "case {case}: {r:?} radius {radius} clip {clip:?}");
        }
    }

    #[test]
    fn fast_strokes_match_the_reference() {
        let (w, h) = (90, 70);
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: i32| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as i32
        };
        for case in 0..400 {
            let r = Rect::new(next(80) - 10, next(60) - 10, next(80) + 1, next(60) + 1);
            let clip = Rect::new(next(50), next(40), next(90) + 1, next(70) + 1).intersect(&Rect::new(0, 0, w, h));
            let radius = next(40);
            let c = (next(256) as u32) << 24 | 0x00AA_5522;
            let start: alloc::vec::Vec<u32> =
                (0..w * h).map(|i| 0xFF00_0000 | (i as u32).wrapping_mul(2654435761)).collect();
            let (mut a, mut b) = (start.clone(), start);
            let (mut ca, mut cb) = (Canvas::new(&mut a, w, h), Canvas::new(&mut b, w, h));
            ca.clip = clip;
            cb.clip = clip;
            ca.stroke_round_rect(r, radius, c);
            cb.stroke_round_rect_slow(r, radius, c);
            assert!(a == b, "case {case}: {r:?} radius {radius} clip {clip:?}");
        }
    }
}

#[cfg(test)]
mod scale_tests {
    use super::*;

    #[test]
    fn points_cover_scale_squared_device_pixels() {
        let mut buf = alloc::vec![0u32; 8 * 6];
        let mut cv = Canvas::new(&mut buf, 8, 6).with_scale(2);
        assert_eq!(cv.size(), (4, 3));
        assert_eq!(cv.clip, Rect::new(0, 0, 4, 3));
        cv.fill_rect(Rect::new(1, 1, 2, 1), 0xFF00_00FF);
        assert_eq!(cv.pixel(1, 1), 0xFF00_00FF);
        let filled: alloc::vec::Vec<usize> = (0..48).filter(|&i| buf[i] != 0).collect();
        // Columns 2..6 of rows 2..4.
        assert_eq!(filled, [18, 19, 20, 21, 26, 27, 28, 29]);
    }

    #[test]
    fn the_clip_is_in_points() {
        let mut buf = alloc::vec![0u32; 8 * 8];
        let mut cv = Canvas::new(&mut buf, 8, 8).with_scale(2);
        cv.clip = Rect::new(0, 0, 1, 1);
        cv.fill_rect(Rect::new(0, 0, 4, 4), 0xFFFF_FFFF);
        assert_eq!(buf.iter().filter(|&&p| p != 0).count(), 4);
    }

    #[test]
    fn text_is_drawn_at_device_size_and_advances_in_points() {
        let data = include_bytes!("../../../assets/fonts/Manrope-Medium.ttf");
        crate::font::install(crate::font::Face::Regular, data);
        let f = crate::font::get(crate::font::Face::Regular, 12);
        let mut one = alloc::vec![0u32; 200 * 40];
        let a1 = Canvas::new(&mut one, 200, 40).text(0, 20, "Wave", f, 0xFFFF_FFFF);
        let mut two = alloc::vec![0u32; 400 * 80];
        let a2 = Canvas::new(&mut two, 400, 80).with_scale(2).text(0, 20, "Wave", f, 0xFFFF_FFFF);
        assert!((a1 - a2).abs() <= 2, "{a1} vs {a2}");
        assert!(two.iter().any(|&p| p != 0));
    }
}
