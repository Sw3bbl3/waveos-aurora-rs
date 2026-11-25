//! A dragged window leans with the pointer: its picture shears a little
//! (at most 6 % of its height across, about three degrees) the way it is
//! moving, in proportion to how fast, and swings back level on a spring
//! when the drag slows or ends. Only the drawing changes: the window's
//! rectangle, hit-testing and where it lands stay exactly as without it,
//! and once level the window is drawn as ever. Off with Reduce Motion.

use super::*;
use crate::gui::canvas::blend;
use lumen::spring::Spring;

/// Thousandths of the height the top edge may move across.
const MOST: i32 = 60;

#[derive(Default)]
pub(super) struct Lean {
    window: Option<u32>,
    shear: Option<Spring>,
    /// Where and when the pointer last moved the window.
    last: (i32, u64),
}

impl Desktop {
    /// The dragged window moved to pointer x `x`.
    pub(super) fn lean_moved(&mut self, id: u32, x: i32) {
        let now = self.now_ms;
        if theme::reduce_motion() {
            self.lean = Lean::default();
            return;
        }
        let l = &mut self.lean;
        if l.window != Some(id) {
            *l = Lean { window: Some(id), shear: Some(Spring::new(0, lumen::spring::SNAPPY)), last: (x, now) };
            return;
        }
        let dt = now.saturating_sub(l.last.1).max(8) as i32;
        let speed = (x - l.last.0) * 1000 / dt; // points a second
        l.last = (x, now);
        if let Some(s) = l.shear.as_mut() {
            // Moving right, the top trails behind: it leans into the motion.
            s.set_target((-speed / 25).clamp(-MOST, MOST), now);
        }
    }

    /// The drag ended: back to level.
    pub(super) fn lean_release(&mut self) {
        let now = self.now_ms;
        if let Some(s) = self.lean.shear.as_mut() {
            s.set_target(0, now);
        }
    }

    /// How far the top edge is moved across now (points), for window `id`.
    fn lean_offset(&self, id: u32, h: i32) -> i32 {
        match (&self.lean.shear, self.lean.window) {
            (Some(s), Some(w)) if w == id && !theme::reduce_motion() => s.value() * h / 2 / 1000,
            _ => 0,
        }
    }

    /// The area a leaning window may cover beyond its own bounds.
    pub(super) fn lean_area(&self, r: Rect) -> Rect {
        r.inset_xy(-(MOST * r.h / 2000 + 4), 0)
    }

    /// Advances the swing; true while it moves.
    pub(super) fn step_lean(&mut self) -> bool {
        let now = self.now_ms;
        let (Some(id), Some(s)) = (self.lean.window, self.lean.shear.as_mut()) else { return false };
        // The pointer stopped: level again.
        if now.saturating_sub(self.lean.last.1) > 60 {
            s.set_target(0, now);
        }
        let moving = s.advance(now) && !theme::reduce_motion();
        let dragged = self.drag.is_some_and(|d| d.window == id);
        if let Some(i) = self.index_of(id) {
            let a = self.lean_area(self.windows[i].bounds());
            self.drag_damage = dragged;
            self.damage(a);
            self.drag_damage = false;
        }
        if !moving && !dragged {
            self.lean = Lean::default();
        }
        moving
    }

    pub(super) fn lean_moving(&self) -> bool {
        self.lean.shear.is_some_and(|s| !s.settled())
    }

    /// Draws window `i` sheared, if it leans; false to draw it as usual.
    pub(super) fn paint_leaning(&mut self, cv: &mut Canvas, i: usize) -> bool {
        let (id, r) = (self.windows[i].id, self.windows[i].rect);
        let top = self.lean_offset(id, r.h);
        if top == 0 {
            return false;
        }
        let Some((px, pw, ph)) = self.snapshot(id) else { return false };
        let t = theme::current();
        cv.shadow(r, WINDOW_RADIUS, SHADOW_BLUR, SHADOW_OFFSET, t.shadow);
        let k = cv.scale;
        let dst = cv.to_px(r);
        let clip = cv.to_px(self.lean_area(r)).intersect(&cv.pclip());
        let (pw, ph) = (pw as i32, ph as i32);
        for y in clip.y.max(dst.y)..clip.bottom().min(dst.y + ph) {
            let sy = y - dst.y;
            // Rows above the middle move by up to `top`, below it the other way.
            let shift = top * k * (ph / 2 - sy) / (ph / 2).max(1);
            let row = (y * cv.width) as usize;
            for x in clip.x..clip.right() {
                let sx = x - dst.x - shift;
                if sx < 0 || sx >= pw {
                    continue;
                }
                let p = px[(sy * pw + sx) as usize];
                let at = row + x as usize;
                cv.buf[at] = blend(cv.buf[at], p, p >> 24);
            }
        }
        true
    }
}
