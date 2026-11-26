//! A dragged window turns about the point held, against the way it is
//! moving and more the faster it goes (a true rotation; its size never
//! changes), and comes back level smoothly, without overshooting, when the
//! drag slows or ends. Only the
//! drawing changes: the window's rectangle, hit-testing and where it lands
//! stay exactly as without it, and once level the window is drawn as ever.
//! Off with Reduce Motion.

use super::*;
use crate::gui::canvas::blend;
use lumen::spring::Spring;

/// The turn asked for at most, in 1/10000 radian (about 8 degrees).
const MOST: i32 = 1400;
/// The furthest it is ever drawn turned.
const SWING: i32 = 1400;
/// Follows the speed smoothly and comes back level without overshooting.
const LOOSE: (u32, u32) = (260, 1000);

#[derive(Default)]
pub(super) struct Lean {
    window: Option<u32>,
    turn: Option<Spring>,
    /// Where and when the pointer last moved the window.
    last: (i32, u64),
    /// The point held, from the window's top left corner (points).
    held: (i32, i32),
}

impl Desktop {
    /// The dragged window moved; the pointer is at (`x`, `y`).
    pub(super) fn lean_moved(&mut self, id: u32, x: i32, y: i32) {
        let now = self.now_ms;
        if theme::reduce_motion() {
            self.lean = Lean::default();
            return;
        }
        let held = match self.index_of(id) {
            Some(i) => {
                let r = self.windows[i].rect;
                ((x - r.x).clamp(0, r.w), (y - r.y).clamp(0, r.h))
            }
            None => return,
        };
        let l = &mut self.lean;
        if l.window != Some(id) {
            *l = Lean { window: Some(id), turn: Some(Spring::new(0, LOOSE)), last: (x, now), held };
            return;
        }
        l.held = held;
        let dt = now.saturating_sub(l.last.1).max(8) as i32;
        let speed = (x - l.last.0) * 1000 / dt; // points a second
        l.last = (x, now);
        if let Some(s) = l.turn.as_mut() {
            // It turns against the way it is going, more the faster it goes.
            s.set_target((-speed * 3 / 5).clamp(-MOST, MOST), now);
        }
    }

    /// The drag ended: back to level, swinging.
    pub(super) fn lean_release(&mut self) {
        let now = self.now_ms;
        if let Some(s) = self.lean.turn.as_mut() {
            s.set_target(0, now);
        }
    }

    /// Window `id`'s turn now, in 1/10000 radian.
    fn lean_turn(&self, id: u32) -> i32 {
        match (&self.lean.turn, self.lean.window) {
            (Some(s), Some(w)) if w == id && !theme::reduce_motion() => s.value().clamp(-SWING, SWING),
            _ => 0,
        }
    }

    /// The area a swinging window may cover beyond its own bounds.
    pub(super) fn lean_area(&self, r: Rect) -> Rect {
        let m = (r.w + r.h) * SWING / 10_000 + 8;
        r.inset_xy(-m, -m)
    }

    /// Advances the swing; true while it moves.
    pub(super) fn step_lean(&mut self) -> bool {
        let now = self.now_ms;
        let (Some(id), Some(s)) = (self.lean.window, self.lean.turn.as_mut()) else { return false };
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
        self.lean.turn.is_some_and(|s| !s.settled())
    }

    /// Draws window `i` turned about the point held, if it swings; false to
    /// draw it as usual.
    pub(super) fn paint_leaning(&mut self, cv: &mut Canvas, i: usize) -> bool {
        let (id, r) = (self.windows[i].id, self.windows[i].rect);
        let a = self.lean_turn(id) as i64;
        if a == 0 {
            return false;
        }
        let held = self.lean.held;
        let area = self.lean_area(r);
        let Some((px, pw, ph)) = self.snapshot(id) else { return false };
        // No shadow while it swings: a level one would show past the turned edges.
        let k = cv.scale;
        let dst = cv.to_px(r);
        let clip = cv.to_px(area).intersect(&cv.pclip());
        let (pw, ph) = (pw as i64, ph as i64);
        // sin and cos of a small angle, 16.16 fixed point.
        let sin = (a * 65536 / 10_000) - (a * a * a * 65536 / 6 / 1_000_000_000_000);
        let cos = 65536 - a * a * 65536 / 200_000_000;
        // The pivot, in the picture's own pixels.
        let (hx, hy) = ((held.0 * k) as i64, (held.1 * k) as i64);
        let (wf, hf) = (pw << 16, ph << 16);
        for y in clip.y..clip.bottom() {
            let dy = (y - dst.y) as i64 - hy;
            let dx = (clip.x - dst.x) as i64 - hx;
            // Each screen pixel, turned back, says where in the picture it comes from.
            let mut sx = dx * cos + dy * sin + (hx << 16) + 32768;
            let mut sy = dy * cos - dx * sin + (hy << 16) + 32768;
            let row = (y * cv.width) as usize;
            for x in clip.x..clip.right() {
                let edge = sx.min(wf - sx).min(sy).min(hf - sy);
                if edge > 0 {
                    let p = px[((sy >> 16) * pw + (sx >> 16)) as usize];
                    let mut alpha = p >> 24;
                    if edge < 65536 {
                        // The outermost pixel fades, so the turned edge isn't a staircase.
                        alpha = alpha * (edge >> 8) as u32 / 256;
                    }
                    let at = row + x as usize;
                    cv.buf[at] = blend(cv.buf[at], p, alpha);
                }
                sx += cos;
                sy -= sin;
            }
        }
        true
    }
}
