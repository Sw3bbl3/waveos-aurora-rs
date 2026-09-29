//! Window animations: open (scale up and fade in), close (fade out), minimize
//! into the dock and restore from it, and zoom.
//!
//! An animation draws a *snapshot* of the window — rendered offscreen once,
//! with its title bar and content — scaled and faded between two rectangles.
//! While it runs the real window stays hidden. The compositor ticks at 60 Hz
//! only while something is animating.

use super::{paint_window, Desktop, SHADOW_BLUR, SHADOW_OFFSET};
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::theme::{self, WINDOW_RADIUS};
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Open,
    Close,
    Minimize,
    Restore,
    Zoom,
}

pub struct Ghost {
    kind: Kind,
    start: u64,
    duration: u64,
    from: Rect,
    to: Rect,
    from_alpha: i32,
    to_alpha: i32,
    pixels: Vec<u32>,
    w: u32,
    h: u32,
    /// The window hidden until this animation finishes (none for close/minimize).
    window: Option<u32>,
    last: Rect,
}

/// Progress 0..=1000 → eased 0..=1000.
fn ease(kind: Kind, t: i64) -> i64 {
    let t = t.clamp(0, 1000);
    match kind {
        // Accelerate away.
        Kind::Close | Kind::Minimize => t * t / 1000,
        // Decelerate into place.
        _ => {
            let inv = 1000 - t;
            1000 - inv * inv / 1000 * inv / 1000
        }
    }
}

fn lerp(a: i32, b: i32, t: i64) -> i32 {
    a + ((b - a) as i64 * t / 1000) as i32
}

fn lerp_rect(a: Rect, b: Rect, t: i64) -> Rect {
    Rect::new(lerp(a.x, b.x, t), lerp(a.y, b.y, t), lerp(a.w, b.w, t).max(1), lerp(a.h, b.h, t).max(1))
}

/// `r` scaled by `permille` about its centre.
fn scaled(r: Rect, permille: i32) -> Rect {
    let (w, h) = (r.w * permille / 1000, r.h * permille / 1000);
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
}

fn halo(r: Rect) -> Rect {
    Rect::new(r.x - SHADOW_BLUR, r.y - SHADOW_BLUR, r.w + 2 * SHADOW_BLUR, r.h + 2 * SHADOW_BLUR + SHADOW_OFFSET)
}

impl Desktop {
    /// Renders window `id` (chrome and content, no shadow) into a buffer.
    fn snapshot(&mut self, id: u32) -> Option<(Vec<u32>, u32, u32)> {
        let i = self.index_of(id)?;
        let focused = self.focused_id() == Some(id);
        let env = self.env(focused);
        let w = &mut self.windows[i];
        let saved = w.rect;
        let (rw, rh) = (saved.w.max(1), saved.h.max(1));
        let mut buf = alloc::vec![0u32; (rw * rh) as usize];
        w.rect = Rect::new(0, 0, rw, rh);
        {
            let mut cv = Canvas::new(&mut buf, rw, rh);
            paint_window(&mut cv, w, focused, false, &env, false);
        }
        w.rect = saved;
        Some((buf, rw as u32, rh as u32))
    }

    fn add_ghost(&mut self, kind: Kind, id: u32, from: Rect, to: Rect, alpha: (i32, i32), duration: u64, hide: bool) {
        let Some((pixels, w, h)) = self.snapshot(id) else { return };
        if hide {
            if let Some(i) = self.index_of(id) {
                self.windows[i].animating = true;
            }
        }
        // Replace an animation already running for this window.
        self.ghosts.retain(|g| g.window != Some(id));
        self.ghosts.push(Ghost {
            kind,
            start: self.now_ms,
            duration,
            from,
            to,
            from_alpha: alpha.0,
            to_alpha: alpha.1,
            pixels,
            w,
            h,
            window: hide.then_some(id),
            last: from,
        });
        self.damage(halo(from));
        self.damage(halo(to));
    }

    pub(super) fn animate_open(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        let r = self.windows[i].rect;
        self.add_ghost(Kind::Open, id, scaled(r, 920), r, (0, 255), 210, true);
    }

    /// Call before removing the window.
    pub(super) fn animate_close(&mut self, id: u32) {
        let Some(i) = self.index_of(id) else { return };
        let r = self.windows[i].rect;
        if self.windows[i].minimized || self.windows[i].animating {
            return;
        }
        self.add_ghost(Kind::Close, id, r, scaled(r, 930), (255, 0), 170, false);
    }

    /// Call before hiding the window; `target` is its dock icon.
    pub(super) fn animate_minimize(&mut self, id: u32, target: Rect) {
        let Some(i) = self.index_of(id) else { return };
        let r = self.windows[i].rect;
        self.add_ghost(Kind::Minimize, id, r, target, (255, 40), 300, false);
    }

    pub(super) fn animate_restore(&mut self, id: u32, from: Rect) {
        let Some(i) = self.index_of(id) else { return };
        let r = self.windows[i].rect;
        self.add_ghost(Kind::Restore, id, from, r, (60, 255), 280, true);
    }

    /// Call after the window got its new rectangle; `old` is where it was.
    pub(super) fn animate_zoom(&mut self, id: u32, old: Rect) {
        let Some(i) = self.index_of(id) else { return };
        let new = self.windows[i].rect;
        // Snapshot at the old size (content hasn't been resized yet).
        self.windows[i].rect = old;
        let snap = self.snapshot(id);
        self.windows[i].rect = new;
        let Some((pixels, w, h)) = snap else { return };
        self.windows[i].animating = true;
        self.ghosts.retain(|g| g.window != Some(id));
        self.ghosts.push(Ghost {
            kind: Kind::Zoom,
            start: self.now_ms,
            duration: 220,
            from: old,
            to: new,
            from_alpha: 255,
            to_alpha: 255,
            pixels,
            w,
            h,
            window: Some(id),
            last: old,
        });
        self.damage(halo(old.union(&new)));
    }

    /// Advances animations; true while any is running.
    pub(super) fn step_animations(&mut self) -> bool {
        let now = self.now_ms;
        let mut finished = Vec::new();
        let mut damage = Vec::new();
        for (k, g) in self.ghosts.iter_mut().enumerate() {
            let t = ((now.saturating_sub(g.start)) * 1000 / g.duration.max(1)) as i64;
            let r = lerp_rect(g.from, g.to, ease(g.kind, t));
            damage.push(halo(g.last.union(&r)));
            g.last = r;
            if t >= 1000 {
                finished.push(k);
            }
        }
        for r in damage {
            self.damage(r);
        }
        for k in finished.into_iter().rev() {
            let g = self.ghosts.remove(k);
            self.damage(halo(g.last));
            if let Some(id) = g.window {
                if let Some(i) = self.index_of(id) {
                    self.windows[i].animating = false;
                    let b = self.windows[i].bounds();
                    self.damage(b);
                }
            }
        }
        !self.ghosts.is_empty()
    }

    pub(super) fn paint_ghosts(&self, cv: &mut Canvas) {
        let t = theme::current();
        let now = self.now_ms;
        for g in &self.ghosts {
            if !halo(g.last).intersects(&cv.clip) {
                continue;
            }
            let p = ((now.saturating_sub(g.start)) * 1000 / g.duration.max(1)) as i64;
            let e = ease(g.kind, p);
            let r = lerp_rect(g.from, g.to, e);
            let alpha = lerp(g.from_alpha, g.to_alpha, e).clamp(0, 255);
            if alpha == 0 {
                continue;
            }
            let strength = t.shadow * alpha as u32 / 255 * (r.w as u32).min(g.w) / g.w.max(1);
            cv.shadow(r, WINDOW_RADIUS * r.w / g.w.max(1) as i32, SHADOW_BLUR, SHADOW_OFFSET, strength);
            cv.draw_image(&g.pixels, g.w, g.h, r, alpha as u32);
        }
    }

    pub(super) fn animating(&self) -> bool {
        !self.ghosts.is_empty()
    }
}
