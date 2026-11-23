//! The two big movements of Wave Play, drawn from layers cached once:
//!
//! - **The morph** between the desktop and Wave Play. When it starts, the
//!   desktop is rendered once into layers — the wallpaper with its items,
//!   each window, the bar's bubbles and Harbor's bays. Every frame then
//!   moves and fades those layers along one spring-driven timeline (windows
//!   recede and fade, staggered from the front; the bar and Harbor slide off;
//!   the wallpaper blurs, then gives way to the lit backdrop; tiles rise in).
//!   Reversing it — at any moment — runs the same timeline back from where
//!   it is, with its velocity.
//! - **Launching**: the tile grows into the screen while the program starts,
//!   then hands over to its window; going home shrinks it back into its tile.
//!
//! Nothing here blurs per frame: the blurred wallpaper is the desktop's own
//! (made once with the wallpaper), and the backdrop is small and filtered up.

use super::home::Geo;
use crate::gui::canvas::Canvas;
use crate::gui::desktop::{Desktop, SHADOW_BLUR, SHADOW_OFFSET};
use crate::gui::geom::Rect;
use crate::gui::theme::{self, MENUBAR_H, WINDOW_RADIUS};
use alloc::vec::Vec;
use lumen::layer;
use lumen::spring::{self, stage, Spring};

/// A piece of the desktop, as pixels (device pixels, `w`×`h`).
pub struct Piece {
    pub rect: Rect,
    pub pixels: Vec<u32>,
    pub w: u32,
    pub h: u32,
    pub radius: i32,
}

pub struct Layers {
    /// The wallpaper with the desktop's items (full screen, device pixels).
    pub base: Vec<u32>,
    /// Windows from the bottom of the stack up.
    pub windows: Vec<Piece>,
    pub bar: Vec<Piece>,
    pub dock: Vec<Piece>,
}

/// Cuts `r` (points) out of a full-screen buffer.
fn crop(buf: &[u32], width_px: i32, k: i32, r: Rect, radius: i32) -> Piece {
    let (x, y, w, h) = (r.x * k, r.y * k, r.w * k, r.h * k);
    let mut pixels = Vec::with_capacity((w * h).max(0) as usize);
    for row in y..y + h {
        let at = (row * width_px + x) as usize;
        pixels.extend_from_slice(&buf[at..at + w as usize]);
    }
    Piece { rect: r, pixels, w: w as u32, h: h as u32, radius }
}

/// `r` scaled by `permille` about its centre.
fn scaled(r: Rect, permille: i32) -> Rect {
    let (w, h) = (r.w * permille / 1000, r.h * permille / 1000);
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w.max(1), h.max(1))
}

pub fn lerp_rect(a: Rect, b: Rect, t: i32) -> Rect {
    Rect::new(
        spring::lerp(a.x, b.x, t),
        spring::lerp(a.y, b.y, t),
        spring::lerp(a.w, b.w, t).max(1),
        spring::lerp(a.h, b.h, t).max(1),
    )
}

impl Desktop {
    /// Renders the desktop as it is now into layers (once per morph).
    pub(super) fn play_capture_layers(&mut self) -> Layers {
        let t0 = crate::time::now_ns();
        let k = crate::gui::scale();
        let (pw, ph) = (self.w * k, self.h * k);
        let mut base = alloc::vec![0u32; (pw * ph) as usize];
        {
            let mut cv = Canvas::new(&mut base, pw, ph).with_scale(k);
            self.paint_below(&mut cv, 0);
        }
        // The bar and Harbor over the wallpaper, to cut their shapes from.
        let mut chrome = base.clone();
        {
            let mut cv = Canvas::new(&mut chrome, pw, ph).with_scale(k);
            self.paint_harbor(&mut cv);
            self.paint_bar(&mut cv);
        }
        let screen = self.screen();
        let fits = |r: &Rect| !r.is_empty() && screen.intersect(r) == *r;
        let mut bar = Vec::new();
        let left = self.left_bubble().0;
        let right = self.right_bubble().0;
        for r in [left, right] {
            if fits(&r) && !self.locked() {
                bar.push(crop(&chrome, pw, k, r, r.h / 2));
            }
        }
        if super::super::bar::crest_on() {
            let c = self.crest_rect();
            if fits(&c) {
                bar.push(crop(&chrome, pw, k, c, c.h.min(52) / 2));
            }
        }
        let dock: Vec<Piece> =
            self.harbor_layout().1.into_iter().filter(fits).map(|r| crop(&chrome, pw, k, r, 24)).collect();
        drop(chrome);
        let ids: Vec<u32> = self
            .windows
            .iter()
            .filter(|w| !w.minimized && !w.animating && w.awaiting_frame.is_none())
            .map(|w| w.id)
            .collect();
        let mut windows = Vec::new();
        for id in ids {
            let Some(i) = self.index_of(id) else { continue };
            let rect = self.windows[i].rect;
            if let Some((pixels, w, h)) = self.snapshot(id) {
                windows.push(Piece { rect, pixels, w, h, radius: WINDOW_RADIUS });
            }
        }
        log!(
            "play",
            "desktop layers: {} windows, {} bar and {} dock pieces in {} us",
            windows.len(),
            bar.len(),
            dock.len(),
            (crate::time::now_ns() - t0) / 1000
        );
        Layers { base, windows, bar, dock }
    }

    /// One frame of the morph at progress `p` (0 desktop … 1000 Wave Play).
    pub(super) fn play_paint_morph(&self, cv: &mut Canvas, layers: &Layers, g: &Geo, p: i32) {
        use crate::gui::stages::{time as staged, Stage};
        let screen = self.screen();
        let blur = stage(p, 0, 420).clamp(0, 1000);
        let lit = stage(p, 220, 760).clamp(0, 1000);
        staged(Stage::Wallpaper, || {
            if lit >= 1000 {
                self.play.home.backdrop.paint(cv, screen, 255);
                return;
            }
            if blur <= 0 {
                cv.blit_same(&layers.base, cv.clip);
            } else if blur >= 1000 {
                cv.blit_same(&self.blurred, cv.clip);
            } else {
                layer::crossfade(cv, &layers.base, &self.blurred, blur * 256 / 1000);
            }
            if lit > 0 {
                self.play.home.backdrop.paint(cv, screen, (lit * 255 / 1000) as u32);
            }
            // The wallpaper dims as it recedes.
            cv.fill_rect(cv.clip, ((blur * 70 / 1000 * (1000 - lit) / 1000) as u32) << 24);
        });
        // Windows recede and fade, the frontmost first.
        let t = theme::current();
        staged(Stage::Body, || {
            let n = layers.windows.len() as i32;
            for (i, w) in layers.windows.iter().enumerate() {
                let rank = (n - 1 - i as i32).min(5);
                let local = stage(p, rank * 45, rank * 45 + 480);
                let gone = local.clamp(0, 1000);
                let alpha = (255 * (1000 - gone) / 1000) as u32;
                if alpha == 0 {
                    continue;
                }
                let r = scaled(w.rect, 1000 - 90 * local / 1000).offset(0, 26 * local / 1000);
                if !r.inset(-SHADOW_BLUR).intersects(&cv.clip) {
                    continue;
                }
                cv.shadow(r, WINDOW_RADIUS, SHADOW_BLUR, SHADOW_OFFSET, t.shadow * alpha / 255);
                layer::draw_scaled(cv, &w.pixels, w.w, w.h, r, alpha, WINDOW_RADIUS * r.w / w.rect.w.max(1));
            }
        });
        // The bar rises away; Harbor sinks.
        staged(Stage::Chrome, || {
            let up = stage(p, 0, 380);
            let down = stage(p, 40, 440);
            for (pieces, local, dy) in
                [(&layers.bar, up, -(MENUBAR_H + 18)), (&layers.dock, down, theme::dock_height() + 44)]
            {
                let gone = local.clamp(0, 1000);
                let alpha = (255 * (1000 - gone) / 1000) as u32;
                if alpha == 0 {
                    continue;
                }
                for piece in pieces {
                    let r = piece.rect.offset(0, dy * local / 1000);
                    if !r.inset(-20).intersects(&cv.clip) {
                        continue;
                    }
                    cv.shadow(r, piece.radius, 14, 4, t.shadow / 4 * alpha / 255);
                    layer::draw_scaled(cv, &piece.pixels, piece.w, piece.h, r, alpha, piece.radius);
                }
            }
        });
        // Wave Play arrives.
        staged(Stage::Overlays, || self.play.home.paint(cv, g, self.play.nav.family, Some(p), false));
    }

    /// The launch card: the tile `from` grown towards the whole screen by
    /// `z` (0..1000), showing the entry's artwork, and over it — once the
    /// program has drawn — its window by `reveal`.
    pub(super) fn play_paint_card(&self, cv: &mut Canvas, g: &Geo, entry: usize, from: Rect, z: i32, opacity: u32) {
        let screen = self.screen();
        let r = lerp_rect(from, screen, z.clamp(0, 1060));
        let radius = spring::lerp(g.radius, 0, z.clamp(0, 1000));
        let home = &self.play.home;
        cv.shadow(r, radius, 30, 12, 140 * opacity / 255 * (1000 - z.clamp(0, 1000)) as u32 / 1000);
        match home.pics.get(entry).cloned().flatten() {
            Some(p) => {
                layer::draw_scaled(cv, &p.field, 2, 2, r, opacity, radius);
                cv.with_clip(r, |cv| layer::scrim(cv, r, 0, 0, 80 * opacity / 255));
                let s = spring::lerp(from.w * 54 / 100, g.h * 20 / 100, z.clamp(0, 1000));
                let icon = Rect::new(r.x + (r.w - s) / 2, r.y + (r.h - s) / 2, s, s);
                cv.shadow(icon.inset(s / 10), s / 5, s / 8, s / 16, 60 * opacity / 255);
                cv.draw_image(&p.pixels, p.w, p.h, icon, opacity);
            }
            None => cv.fill_round_rect(r, radius, (opacity << 24) | 0x2A3342),
        }
    }

    /// A picture of the screen (a game's last frame) shrinking into `to`.
    pub(super) fn play_paint_shrink(&self, cv: &mut Canvas, g: &Geo, picture: &[u32], to: Rect, z: i32) {
        let k = crate::gui::scale();
        let r = lerp_rect(to, self.screen(), z.clamp(0, 1060));
        let radius = spring::lerp(g.radius, 0, z.clamp(0, 1000));
        // As it lands, the picture dissolves into the tile's own artwork.
        let opacity = (z.clamp(0, 220) * 255 / 220) as u32;
        cv.shadow(r, radius, 30, 12, 140 * (1000 - z.clamp(0, 1000)) as u32 / 1000 * opacity / 255);
        layer::draw_scaled(cv, picture, (self.w * k) as u32, (self.h * k) as u32, r, opacity, radius);
    }
}

/// The tile → screen zoom of a launch (and back).
pub struct Launch {
    pub entry: usize,
    pub from: Rect,
    pub zoom: Spring,
    /// The window taking over from the card (0 card … 1000 window).
    pub reveal: Spring,
    pub started: u64,
    pub window: Option<u32>,
}

impl Launch {
    pub fn new(entry: usize, from: Rect, now: u64) -> Launch {
        let mut zoom = Spring::new(0, spring::SCENE);
        zoom.set_target(1000, now);
        Launch { entry, from, zoom, reveal: Spring::new(0, spring::SMOOTH), started: now, window: None }
    }
}

/// Going home from a program: its last frame shrinking into its tile.
pub struct Shrink {
    pub entry: Option<usize>,
    pub picture: Vec<u32>,
    pub zoom: Spring,
}
