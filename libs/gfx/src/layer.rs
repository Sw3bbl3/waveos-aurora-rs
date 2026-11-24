//! Cached layers moved, scaled and cross-faded every frame: the work of a
//! full-screen transition, done with as few operations per pixel as possible.
//!
//! The general [`Canvas::draw_image`](crate::canvas::Canvas::draw_image)
//! filters straight alpha in 64-bit arithmetic for any picture. Snapshots of
//! windows and pre-blurred backdrops are opaque, so these helpers filter two
//! colour channels per 32-bit multiply ([`mix`]) with the source columns
//! looked up once per call. Coordinates are in points, like the canvas.

use crate::canvas::{mix, Canvas};
use crate::geom::Rect;
use crate::math::isqrt;
use alloc::vec::Vec;

/// Anti-aliased coverage (0..=256) of device pixel (x, y) in `r` with corners of `radius`.
#[inline]
fn corner_coverage(r: &Rect, radius: i32, x: i32, y: i32) -> u32 {
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

/// `dst = mix(a, b, t)` for whole rows (t 0..=256), alpha set; SIMD in the
/// compositor and wherever the target has it.
#[inline]
fn mix_rows(dst: &mut [u32], a: &[u32], b: &[u32], t: i32) {
    let n = dst.len().min(a.len()).min(b.len());
    let mut done = 0;
    if n >= 8 && crate::simd::enabled() {
        // SAFETY: SIMD is usable; all three hold n pixels.
        done = unsafe { crate::simd::mix_rows(dst.as_mut_ptr(), a.as_ptr(), b.as_ptr(), n, t.clamp(0, 256) as u32) };
    }
    for ((o, &x), &y) in dst[done..n].iter_mut().zip(&a[done..n]).zip(&b[done..n]) {
        *o = mix(x, y, t) | 0xFF00_0000;
    }
}

/// `dst = mix(dst, b, t)` in place.
#[inline]
fn mix_over(dst: &mut [u32], b: &[u32], t: i32) {
    let n = dst.len().min(b.len());
    let mut done = 0;
    if n >= 8 && crate::simd::enabled() {
        // SAFETY: as above; the routine reads each group before writing it.
        done = unsafe { crate::simd::mix_rows(dst.as_mut_ptr(), dst.as_ptr(), b.as_ptr(), n, t.clamp(0, 256) as u32) };
    }
    for (o, &y) in dst[done..n].iter_mut().zip(&b[done..n]) {
        *o = mix(*o, y, t) | 0xFF00_0000;
    }
}

/// Draws the part `part` of an opaque `sw`×`sh` picture (device pixels;
/// `part` in 1/256 source pixels) scaled into `dst`, at `opacity` (0..=255),
/// with its corners rounded at `radius` points. Bilinear filtering; fine for
/// reductions down to about half size.
#[allow(clippy::too_many_arguments)] // a picture, the part of it, where, how
pub fn draw_part(
    cv: &mut Canvas,
    src: &[u32],
    sw: u32,
    sh: u32,
    part: (i32, i32, i32, i32),
    dst: Rect,
    opacity: u32,
    radius: i32,
) {
    let d = cv.to_px(dst);
    if d.w <= 0 || d.h <= 0 || sw == 0 || sh == 0 || opacity == 0 || src.len() < (sw * sh) as usize {
        return;
    }
    let vis = d.intersect(&cv.pclip());
    if vis.is_empty() {
        return;
    }
    let radius = (radius * cv.scale).min(d.w / 2).min(d.h / 2).max(0);
    let (px, py, pw, ph) = part;
    // Source position of each destination pixel centre, 24.8, minus half a texel.
    let step_x = (pw as i64) / d.w as i64;
    let step_y = (ph as i64) / d.h as i64;
    let (max_x, max_y) = (sw as i64 - 1, sh as i64 - 1);
    let columns: Vec<(u32, u32, u32)> = (vis.x..vis.right())
        .map(|x| {
            let f = (px as i64 + (x - d.x) as i64 * step_x + step_x / 2 - 128).max(0);
            let x0 = (f >> 8).min(max_x);
            (x0 as u32, (x0 + 1).min(max_x) as u32, (f & 255) as u32)
        })
        .collect();
    let width = cv.width;
    let alpha = opacity.min(255) * 256 / 255;
    // Separable: each source row needed is filtered across once into a row
    // cache, then output rows blend two cached rows (one lerp per pixel).
    let n = columns.len();
    let mut rows: [(i64, Vec<u32>); 2] = [(-1, alloc::vec![0; n]), (-1, alloc::vec![0; n])];
    let mut blended = if alpha < 256 { alloc::vec![0u32; n] } else { Vec::new() };
    let expand = |sy: i64, out: &mut Vec<u32>| {
        let r = &src[(sy * sw as i64) as usize..][..sw as usize];
        for (o, &(x0, x1, tx)) in out.iter_mut().zip(&columns) {
            *o = mix(r[x0 as usize], r[x1 as usize], tx as i32);
        }
    };
    for y in vis.y..vis.bottom() {
        let f = (py as i64 + (y - d.y) as i64 * step_y + step_y / 2 - 128).max(0);
        let y0 = (f >> 8).min(max_y);
        let y1 = (y0 + 1).min(max_y);
        let ty = (f & 255) as i32;
        // Reuse what the previous output row expanded.
        if rows[0].0 != y0 {
            if rows[1].0 == y0 {
                rows.swap(0, 1);
            } else {
                expand(y0, &mut rows[0].1);
                rows[0].0 = y0;
            }
        }
        if rows[1].0 != y1 {
            expand(y1, &mut rows[1].1);
            rows[1].0 = y1;
        }
        let (top, bottom) = (&rows[0].1, &rows[1].1);
        let row = &mut cv.buf[(y * width + vis.x) as usize..(y * width + vis.right()) as usize];
        let in_band = y < d.y + radius || y >= d.bottom() - radius;
        if !in_band {
            if alpha >= 256 {
                mix_rows(row, top, bottom, ty);
            } else {
                mix_rows(&mut blended, top, bottom, ty);
                mix_over(row, &blended, alpha as i32);
            }
            continue;
        }
        for (i, p) in row.iter_mut().enumerate() {
            let c = mix(top[i], bottom[i], ty);
            let mut a = alpha;
            if in_band {
                let x = vis.x + i as i32;
                if x < d.x + radius || x >= d.right() - radius {
                    a = (a * corner_coverage(&d, radius, x, y)) >> 8;
                }
            }
            if a >= 256 {
                *p = c | 0xFF00_0000;
            } else if a > 0 {
                *p = mix(*p, c, a as i32) | 0xFF00_0000;
            }
        }
    }
}

/// Draws a whole opaque picture scaled into `dst` (see [`draw_part`]).
pub fn draw_scaled(cv: &mut Canvas, src: &[u32], sw: u32, sh: u32, dst: Rect, opacity: u32, radius: i32) {
    draw_part(cv, src, sw, sh, (0, 0, sw as i32 * 256, sh as i32 * 256), dst, opacity, radius);
}

/// Writes `a` cross-faded towards `b` by `t` (0..=256) under the clip. Both
/// are full-canvas buffers in device pixels.
pub fn crossfade(cv: &mut Canvas, a: &[u32], b: &[u32], t: i32) {
    let clip = cv.pclip();
    let w = cv.width;
    for y in clip.y..clip.bottom() {
        let at = (y * w + clip.x) as usize;
        let n = clip.w as usize;
        let (ra, rb) = (&a[at..at + n], &b[at..at + n]);
        let out = &mut cv.buf[at..at + n];
        if t <= 0 {
            out.copy_from_slice(ra);
        } else if t >= 256 {
            out.copy_from_slice(rb);
        } else {
            mix_rows(out, ra, rb, t);
        }
    }
}

/// Darkens (or tints) the clip with `color` at an opacity that runs from
/// `top` to `bottom` (0..=255) down `r`: legibility scrims, vignettes.
pub fn scrim(cv: &mut Canvas, r: Rect, color: u32, top: u32, bottom: u32) {
    let d = cv.to_px(r);
    let vis = d.intersect(&cv.pclip());
    let w = cv.width;
    let h = d.h.max(1) as i64;
    for y in vis.y..vis.bottom() {
        let a = (top as i64 + (bottom as i64 - top as i64) * (y - d.y) as i64 / h).clamp(0, 255) as u32;
        let at = (y * w + vis.x) as usize;
        crate::canvas::blend_row(&mut cv.buf[at..at + vis.w as usize], color, a);
    }
}

/// Box-averages an opaque picture down to `w`×`h` (device pixels).
pub fn shrink(src: &[u32], sw: u32, sh: u32, w: u32, h: u32) -> Vec<u32> {
    let (w, h) = (w.max(1), h.max(1));
    let mut out = alloc::vec![0u32; (w * h) as usize];
    for oy in 0..h {
        let (y0, y1) = (oy * sh / h, ((oy + 1) * sh / h).max(oy * sh / h + 1).min(sh));
        for ox in 0..w {
            let (x0, x1) = (ox * sw / w, ((ox + 1) * sw / w).max(ox * sw / w + 1).min(sw));
            let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
            for y in y0..y1 {
                for &c in &src[(y * sw + x0) as usize..(y * sw + x1) as usize] {
                    r += c >> 16 & 0xFF;
                    g += c >> 8 & 0xFF;
                    b += c & 0xFF;
                    n += 1;
                }
            }
            let n = n.max(1);
            out[(oy * w + ox) as usize] = 0xFF00_0000 | (r / n) << 16 | (g / n) << 8 | (b / n);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_size_is_a_copy_and_opacity_blends() {
        let src: Vec<u32> = (0..16).map(|i| 0xFF00_0000 | (i * 0x0A0A0A)).collect();
        let mut buf = alloc::vec![0xFF00_0000u32; 16];
        let mut cv = Canvas::new(&mut buf, 4, 4);
        draw_scaled(&mut cv, &src, 4, 4, Rect::new(0, 0, 4, 4), 255, 0);
        assert_eq!(buf, src);
        let mut buf = alloc::vec![0xFF00_0000u32; 16];
        let mut cv = Canvas::new(&mut buf, 4, 4);
        draw_scaled(&mut cv, &[0xFFFF_FFFF; 4], 2, 2, Rect::new(0, 0, 4, 4), 128, 0);
        assert!(buf.iter().all(|&p| (p & 0xFF) >= 0x7E && (p & 0xFF) <= 0x81), "{:x?}", buf);
    }

    #[test]
    fn corners_are_rounded_and_clip_respected() {
        let mut buf = alloc::vec![0xFF00_0000u32; 400];
        let mut cv = Canvas::new(&mut buf, 20, 20);
        cv.clip = Rect::new(0, 0, 10, 20);
        draw_scaled(&mut cv, &[0xFFFF_FFFF; 4], 2, 2, Rect::new(0, 0, 20, 20), 255, 6);
        assert_eq!(buf[0], 0xFF00_0000, "corner outside the curve");
        assert_eq!(buf[10 * 20 + 5], 0xFFFF_FFFF, "inside");
        assert_eq!(buf[10 * 20 + 15], 0xFF00_0000, "outside the clip");
    }

    #[test]
    fn crossfade_and_shrink() {
        let a = alloc::vec![0xFF00_0000u32; 4];
        let b = alloc::vec![0xFFFF_FFFFu32; 4];
        let mut buf = alloc::vec![0u32; 4];
        let mut cv = Canvas::new(&mut buf, 2, 2);
        crossfade(&mut cv, &a, &b, 128);
        assert!(buf.iter().all(|&p| p & 0xFF == 0x7F || p & 0xFF == 0x80));
        let s = shrink(&[0xFFFF_0000, 0xFF00_00FF, 0xFFFF_0000, 0xFF00_00FF], 2, 2, 1, 1);
        assert_eq!(s, alloc::vec![0xFF7F_007F]);
    }
}
