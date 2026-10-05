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
                    let p = 0xff000000 | (red / n) << 16 | (green / n) << 8 | blue / n;
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
                for y in visible.y..visible.bottom() {
                    for x in visible.x..visible.right() {
                        let fx = ((x - bounds.x) * (256 / step)).max(0);
                        let fy = ((y - bounds.y) * (256 / step)).max(0);
                        let (ix, iy) = ((fx / 256).min(w - 1), (fy / 256).min(h - 1));
                        let at = |x: i32, y: i32| layer.blurred[(y.min(h - 1) * w + x.min(w - 1)) as usize];
                        let p = mix(
                            mix(at(ix, iy), at(ix + 1, iy), fx % 256),
                            mix(at(ix, iy + 1), at(ix + 1, iy + 1), fx % 256),
                            fy % 256,
                        );
                        let base = blend(p, tint, opacity);
                        // A soft vertical sheen gives the material depth without obscuring content.
                        let shine = (1 - (y - rect.y) * 10 / rect.h.max(1)).max(0) as u32;
                        layer.rendered[((y - visible.y) * visible.w + x - visible.x) as usize] =
                            blend(base, 0xffffffff, if dark { shine * 9 } else { shine * 22 });
                    }
                }
                layer.render_key = Some(key);
            }
            cv.fill_round_rect_with(rect, radius, |x, y| {
                layer.rendered[((y - visible.y) * visible.w + x - visible.x) as usize]
            });
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
