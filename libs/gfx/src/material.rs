//! Lumen backdrop materials. Reusable quarter-resolution buffers sample the
//! scene below a surface, never its own previously composited pixels.
use crate::{
    canvas::{blend, mix, with_alpha, Canvas},
    geom::Rect,
    theme,
};
use alloc::vec::Vec;

#[derive(Default)]
struct Layer {
    source: Vec<u32>,
    blurred: Vec<u32>,
    temporary: Vec<u32>,
    width: i32,
    step: i32,
    rendered: Vec<u32>,
    render_key: Option<(Rect, Rect, u32, bool)>,
}

#[derive(Default)]
pub struct MaterialCache {
    layers: Vec<Layer>,
    next: usize,
    moving: bool,
}

impl MaterialCache {
    pub fn set_moving(&mut self, moving: bool) {
        self.moving = moving;
    }
    pub fn begin_frame(&mut self) {
        self.next = 0;
    }
    pub fn clear(&mut self) {
        self.layers.clear();
        self.next = 0;
    }

    pub fn paint(&mut self, cv: &mut Canvas, rect: Rect, radius: i32, tint: u32) {
        let visible = rect.intersect(&cv.clip);
        if visible.is_empty() {
            return;
        }
        let dark = theme::current().dark;
        if theme::reduce_transparency() || theme::high_contrast() {
            cv.fill_round_rect(rect, radius, tint | 0xff000000);
        } else {
            // Moving surfaces use a coarser sample grid; release restores full
            // quality. Four samples per cell avoid scanning every source pixel.
            let step = if self.moving { 8 } else { 4 };
            let blur_radius = if self.moving { 2 } else { 3 };
            let padding = step * (blur_radius * 2 + 1);
            let bounds = visible.inset(-padding).intersect(&Rect::new(0, 0, cv.width, cv.height));
            let (w, h) = ((bounds.w + step - 1) / step, (bounds.h + step - 1) / step);
            if self.next == self.layers.len() {
                self.layers.push(Layer::default());
            }
            let layer = &mut self.layers[self.next];
            self.next += 1;
            let len = (w * h) as usize;
            let mut changed = layer.width != w || layer.source.len() != len || layer.step != step;
            layer.step = step;
            layer.width = w;
            layer.source.resize(len, 0);
            for y in 0..h {
                for x in 0..w {
                    let mut red = 0;
                    let mut green = 0;
                    let mut blue = 0;
                    let mut n = 0;
                    for dy in [step / 4, step * 3 / 4] {
                        for dx in [step / 4, step * 3 / 4] {
                            let sx = (bounds.x + x * step + dx).min(bounds.right() - 1);
                            let sy = (bounds.y + y * step + dy).min(bounds.bottom() - 1);
                            let p = cv.buf[(sy * cv.width + sx) as usize];
                            red += (p >> 16) & 255;
                            green += (p >> 8) & 255;
                            blue += p & 255;
                            n += 1;
                        }
                    }
                    let p = 0xff000000 | (red / n) << 16 | (green / n) << 8 | (blue / n);
                    let i = (y * w + x) as usize;
                    changed |= layer.source[i] != p;
                    layer.source[i] = p;
                }
            }
            if changed || layer.blurred.len() != len {
                layer.blurred.resize(len, 0);
                layer.temporary.resize(len, 0);
                layer.blurred.copy_from_slice(&layer.source);
                for _ in 0..2 {
                    crate::wallpaper::box_pass(&layer.blurred, &mut layer.temporary, w, h, blur_radius, true);
                    crate::wallpaper::box_pass(&layer.temporary, &mut layer.blurred, w, h, blur_radius, false);
                }
            }
            let opacity = (tint >> 24) as i32;
            let opacity = (opacity + (70 - theme::glass_intensity() as i32) * 2).clamp(64, 245) as u32;
            let key = (rect, visible, (tint & 0xffffff) | opacity << 24, dark);
            if changed || layer.render_key != Some(key) {
                layer.rendered.resize((visible.w * visible.h) as usize, 0);
                let at = |x: usize, y: i32| layer.blurred[(y.min(h - 1) * w) as usize + x.min(w as usize - 1)];
                // A soft sheen fading down the top of the material gives it depth
                // without obscuring content (over at most 40 px, never a hard edge).
                let band = (rect.h / 4).clamp(1, 40);
                let sheen = if dark { 10 } else { 24 };
                let mut column = alloc::vec![0u32; w as usize + 1];
                for y in visible.y..visible.bottom() {
                    // Interpolate the blurred grid down to this row once, then across it.
                    let fy = ((y - bounds.y) * (256 / step)).max(0);
                    let iy = (fy / 256).min(h - 1);
                    for (ix, c) in column.iter_mut().enumerate() {
                        *c = mix(at(ix, iy), at(ix, iy + 1), fy % 256);
                    }
                    let shine = ((band - (y - rect.y)).max(0) * 256 / band) as u32 * sheen / 256;
                    let out = &mut layer.rendered[((y - visible.y) * visible.w) as usize..][..visible.w as usize];
                    for (x, o) in (visible.x..visible.right()).zip(out) {
                        let fx = ((x - bounds.x) * (256 / step)).max(0);
                        let ix = ((fx / 256).min(w - 1)) as usize;
                        let base = blend(mix(column[ix], column[ix + 1], fx % 256), tint, opacity);
                        *o = if shine == 0 { base } else { blend(base, 0xffffffff, shine) };
                    }
                }
                layer.render_key = Some(key);
            }
            cv.blit_part_rounded(&layer.rendered, visible, rect, radius);
        }
        cv.stroke_round_rect(
            rect,
            radius,
            if theme::high_contrast() {
                theme::current().text
            } else if dark {
                0x45ffffff
            } else {
                0x95ffffff
            },
        );
        if radius > 0 {
            let top = Rect::new(rect.x + radius, rect.y + 1, (rect.w - radius * 2).max(0), 1);
            cv.fill_rect(top, with_alpha(0xffffff, if dark { 45 } else { 130 }));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn backdrop_changes_are_seen_without_feedback() {
        let mut cache = MaterialCache::default();
        let mut pixels = alloc::vec![0xffff0000; 64*64];
        for color in [0xffff0000, 0xff0000ff] {
            pixels.fill(color);
            cache.begin_frame();
            let mut cv = Canvas::new(&mut pixels, 64, 64);
            cache.paint(&mut cv, Rect::new(8, 8, 48, 48), 8, 0x80000000);
            let p = pixels[32 * 64 + 32];
            if color == 0xffff0000 {
                assert!((p >> 16) & 255 > p & 255);
            } else {
                assert!(p & 255 > (p >> 16) & 255);
            }
        }
    }
}
