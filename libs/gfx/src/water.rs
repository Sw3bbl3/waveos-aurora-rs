//! Bounded integer height-field water. Two fixed grids, no allocation per impulse/frame.
use crate::{canvas::Canvas, geom::Rect};
use alloc::vec::Vec;
const GW: usize = 192;
const GH: usize = 120;
const STEP: u64 = 16;
pub struct Water {
    height: Vec<i32>,
    previous: Vec<i32>,
    normals: Vec<(i32, i32)>,
    active: bool,
    last: u64,
    expires: u64,
    accumulator: u64,
    bounds: Rect,
}
impl Default for Water {
    fn default() -> Self {
        Self::new()
    }
}
impl Water {
    pub fn new() -> Self {
        Self {
            height: alloc::vec![0;GW*GH],
            previous: alloc::vec![0;GW*GH],
            normals: alloc::vec![(0,0);GW*GH],
            active: false,
            last: 0,
            expires: 0,
            accumulator: 0,
            bounds: Rect::new(0, 0, 0, 0),
        }
    }
    pub fn active(&self) -> bool {
        self.active
    }
    pub fn clear(&mut self) {
        self.height.fill(0);
        self.previous.fill(0);
        self.normals.fill((0, 0));
        self.active = false;
        self.accumulator = 0;
        self.bounds = Rect::new(0, 0, 0, 0);
    }
    pub fn impulse(&mut self, x: i32, y: i32, w: i32, h: i32, now: u64) {
        if w <= 0 || h <= 0 {
            return;
        }
        let gx = (x.clamp(0, w - 1) as usize * (GW - 2) / w as usize + 1).min(GW - 2);
        let gy = (y.clamp(0, h - 1) as usize * (GH - 2) / h as usize + 1).min(GH - 2);
        // Smooth compact depression produces an expanding rim, not a painted ring.
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let r = dx * dx + dy * dy;
                if r > 16 {
                    continue;
                }
                let xx = gx as i32 + dx;
                let yy = gy as i32 + dy;
                if xx > 0 && xx < (GW - 1) as i32 && yy > 0 && yy < (GH - 1) as i32 {
                    let i = yy as usize * GW + xx as usize;
                    self.height[i] = (self.height[i] - (16 - r) * 150).clamp(-8192, 8192);
                }
            }
        }
        if !self.active {
            self.last = now;
            self.accumulator = 0;
        }
        self.active = true;
        self.expires = now + 2200;
    }
    /// Returns whether the surface changed, including its final return to still water.
    pub fn advance(&mut self, now: u64, w: i32, h: i32) -> bool {
        if !self.active {
            return false;
        }
        if now >= self.expires {
            self.clear();
            return true;
        }
        self.accumulator += now.saturating_sub(self.last).min(64);
        self.last = now;
        if self.accumulator < STEP {
            return false;
        }
        while self.accumulator >= STEP {
            self.accumulator -= STEP;
            for y in 1..GH - 1 {
                for x in 1..GW - 1 {
                    let i = y * GW + x;
                    let v = (self.height[i - 1] + self.height[i + 1] + self.height[i - GW] + self.height[i + GW]) / 2
                        - self.previous[i];
                    self.previous[i] = (v * 950 / 1000).clamp(-8192, 8192);
                }
            }
            core::mem::swap(&mut self.height, &mut self.previous);
        }
        let (mut left, mut top, mut right, mut bottom) = (GW, GH, 0, 0);
        for y in 1..GH - 1 {
            for x in 1..GW - 1 {
                let i = y * GW + x;
                self.normals[i] = (self.height[i - 1] - self.height[i + 1], self.height[i - GW] - self.height[i + GW]);
                if self.height[i].abs() > 3 || self.previous[i].abs() > 3 {
                    left = left.min(x);
                    right = right.max(x);
                    top = top.min(y);
                    bottom = bottom.max(y);
                }
            }
        }
        if right == 0 {
            self.clear();
        } else {
            self.bounds = Rect::new(
                (left.saturating_sub(2) as i32 * w / GW as i32).max(0),
                (top.saturating_sub(2) as i32 * h / GH as i32).max(0),
                ((right - left + 5) as i32 * w / GW as i32 + 2).min(w),
                ((bottom - top + 5) as i32 * h / GH as i32 + 2).min(h),
            );
        }
        true
    }
    pub fn bounds(&self) -> Rect {
        self.bounds
    }
    /// Refracts only wallpaper pixels. The compositor draws icons and windows afterwards.
    pub fn paint(&self, cv: &mut Canvas, wallpaper: &[u32], coarse: bool) {
        if !self.active || wallpaper.len() != cv.buf.len() {
            return;
        }
        let clip = cv.to_px(self.bounds).intersect(&cv.pclip());
        if clip.w <= 0 || clip.h <= 0 {
            return;
        }
        let (w, h) = (cv.width, cv.height);
        let stride = if coarse { 2 } else { 1 };
        let xstep = ((GW as i64 - 3) << 16) / w.max(1) as i64;
        let ystep = ((GH as i64 - 3) << 16) / h.max(1) as i64;
        for y in (clip.y..clip.bottom()).step_by(stride) {
            let fy = (y as i64 * ystep) >> 8;
            let gy = 1 + (fy >> 8) as usize;
            let ty = fy as i32 & 255;
            for x in (clip.x..clip.right()).step_by(stride) {
                let fx = (x as i64 * xstep) >> 8;
                let gx = 1 + (fx >> 8) as usize;
                let tx = fx as i32 & 255;
                let i = gy * GW + gx;
                let a = self.normals[i];
                let b = self.normals[i + 1];
                let c = self.normals[i + GW];
                let d = self.normals[i + GW + 1];
                let lerp = |a: i32, b: i32, t: i32| a + (b - a) * t / 256;
                let nx = lerp(lerp(a.0, b.0, tx), lerp(c.0, d.0, tx), ty);
                let ny = lerp(lerp(a.1, b.1, tx), lerp(c.1, d.1, tx), ty);
                let dx = (nx / 75).clamp(-24, 24) * cv.scale;
                let dy = (ny / 75).clamp(-24, 24) * cv.scale;
                let sx = (x + dx).clamp(0, w - 1);
                let sy = (y + dy).clamp(0, h - 1);
                let pixel = wallpaper[(sy * w + sx) as usize];
                let light = ((nx - ny) / 28).clamp(-38, 58);
                let channel = |shift: u32| (((pixel >> shift) & 255) as i32 + light).clamp(0, 255) as u32;
                let color = 0xFF000000 | channel(16) << 16 | channel(8) << 8 | channel(0);
                for oy in 0..stride as i32 {
                    for ox in 0..stride as i32 {
                        if x + ox < clip.right() && y + oy < clip.bottom() {
                            cv.buf[((y + oy) * w + x + ox) as usize] = color;
                        }
                    }
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_overlapping_impulses_and_idle() {
        let mut a = Water::new();
        let cap = (a.height.capacity(), a.previous.capacity());
        for t in 0..100 {
            a.impulse(640, 400, 1280, 800, t);
            a.advance(t, 1280, 800);
        }
        assert!(a.height.iter().all(|v| v.abs() <= 8192));
        assert_eq!(cap, (a.height.capacity(), a.previous.capacity()));
        assert!(a.advance(2400, 1280, 800));
        assert!(!a.active());
        assert!(!a.advance(3000, 1280, 800));
    }
    #[test]
    fn waves_interact_and_clear() {
        let mut a = Water::new();
        a.impulse(500, 400, 1280, 800, 0);
        a.advance(160, 1280, 800);
        let b = a.height.clone();
        a.impulse(550, 400, 1280, 800, 160);
        a.advance(320, 1280, 800);
        assert_ne!(a.height, b);
        a.clear();
        assert!(a.height.iter().all(|&v| v == 0));
    }
    #[test]
    fn paint_preserves_outside_damage() {
        let mut a = Water::new();
        a.impulse(64, 40, 128, 80, 0);
        a.advance(16, 128, 80);
        let bg = alloc::vec![0xFF445566;128*80];
        let mut out = bg.clone();
        let mut cv = Canvas::new(&mut out, 128, 80);
        cv.clip = Rect::new(0, 0, 10, 10);
        a.paint(&mut cv, &bg, false);
        assert_eq!(&out[128 * 10..], &bg[128 * 10..]);
    }
}
