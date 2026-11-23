//! Short surface crossfades reuse the last composed scene, without its cursor.
//! Targets become interactive immediately. Snapshots are bounded to the changed
//! surface and released after 180 ms; no animation timer runs while idle.
use super::Desktop;
use crate::gui::canvas::{blend, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use alloc::vec::Vec;

pub(super) struct Reveal {
    rect: Rect,
    pixels: Vec<u32>,
    scale: i32,
    start: u64,
}

impl Desktop {
    pub(super) fn begin_reveal(&mut self, rect: Rect) {
        if theme::reduce_motion() || self.shore.is_some() {
            return;
        }
        let scale = crate::gui::scale();
        if self.scene.len() != (self.w * self.h * scale * scale) as usize {
            return;
        }
        let r = rect.intersect(&self.screen());
        if r.is_empty() {
            return;
        }
        let pixel = Rect::new(r.x * scale, r.y * scale, r.w * scale, r.h * scale);
        let mut pixels = Vec::with_capacity(pixel.area() as usize);
        for y in pixel.y..pixel.bottom() {
            let at = (y * self.w * scale + pixel.x) as usize;
            pixels.extend_from_slice(&self.scene[at..at + pixel.w as usize]);
        }
        if let Some(old) = self.reveal.take() {
            self.damage(old.rect);
        }
        self.reveal = Some(Reveal { rect: r, pixels, scale, start: self.now_ms });
        self.damage(r);
    }

    pub(super) fn reveal_moving(&self) -> bool {
        self.reveal.is_some()
    }

    pub(super) fn step_reveal(&mut self) -> bool {
        if let Some(r) = &self.reveal {
            let (rect, finished) = (
                r.rect,
                theme::reduce_motion()
                    || r.scale != crate::gui::scale()
                    || self.now_ms.saturating_sub(r.start) >= lumen::motion::REVEAL,
            );
            if finished {
                self.reveal = None;
            }
            self.damage(rect);
        }
        self.reveal.is_some()
    }

    pub(super) fn paint_reveal(&self, cv: &mut Canvas) {
        let Some(r) = &self.reveal else {
            return;
        };
        if r.scale != cv.scale || theme::reduce_motion() {
            return;
        }
        let pixel = cv.to_px(r.rect);
        let clip = pixel.intersect(&cv.pclip());
        let amount =
            lumen::motion::ease(self.now_ms.saturating_sub(r.start), lumen::motion::REVEAL) as u32 * 255 / 1000;
        for y in clip.y..clip.bottom() {
            for x in clip.x..clip.right() {
                let before = r.pixels[((y - pixel.y) * pixel.w + x - pixel.x) as usize];
                let at = (y * cv.width + x) as usize;
                cv.buf[at] = blend(before, cv.buf[at], amount);
            }
        }
    }
}
