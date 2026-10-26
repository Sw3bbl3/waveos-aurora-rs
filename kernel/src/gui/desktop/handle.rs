//! Windows without title bars: the handle, the waterline and Moorings.
//!
//! Apps get the whole window. Bring the pointer to a window's top edge and a
//! small bar fades in there; touch it and it rises into a pill above the
//! window with the title and three buttons (shrink, expand, close). Drag the
//! bar or the pill — or hold Super and drag anywhere — to move the window.
//! While a window is dragged, Moorings drops six layouts from the top of the
//! screen; slide onto a zone (or push into a screen edge) and let go to moor
//! the window there. The focused window glows along its lower edge: the
//! waterline.

use super::{Desktop, Drag, DragKind};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme::{self, MENUBAR_H};
use lumen::windowing::{self, Placement, MOORINGS};

/// How far above the top edge the pointer summons the handle.
const REACH: i32 = 28;
const PILL_H: i32 = 32;
const BUTTON: i32 = 24;
const TILE_W: i32 = 58;
const TILE_H: i32 = 40;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stage {
    /// The thin bar on the top edge.
    Bar,
    /// Risen into the pill with the title and buttons.
    Pill,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Handle {
    pub window: u32,
    pub stage: Stage,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PillHit {
    Shrink,
    Expand,
    Close,
    Grab,
}

impl Desktop {
    /// The bar on window `i`'s top edge.
    fn handle_bar(&self, i: usize) -> Rect {
        let r = self.windows[i].rect;
        Rect::new(r.x + r.w / 2 - 23, r.y + 7, 46, 5)
    }

    /// The pill for window `i`: above the window when there's room, else just inside its top.
    fn handle_pill(&self, i: usize) -> Rect {
        let r = self.windows[i].rect;
        let title = self.windows[i].app.title();
        let tw = theme::ui_bold(13).width(&title).min(r.w - 140).max(40);
        let w = 14 + tw + 10 + 3 * (BUTTON + 4) + 2;
        let y = if r.y - PILL_H - 8 >= MENUBAR_H { r.y - PILL_H - 8 } else { r.y + 6 };
        Rect::new(r.x + r.w / 2 - w / 2, y, w, PILL_H)
    }

    fn pill_buttons(pill: Rect) -> [(PillHit, Rect); 3] {
        let y = pill.y + (PILL_H - BUTTON) / 2;
        let x = pill.right() - 5 - 3 * BUTTON - 2 * 4;
        [
            (PillHit::Shrink, Rect::new(x, y, BUTTON, BUTTON)),
            (PillHit::Expand, Rect::new(x + BUTTON + 4, y, BUTTON, BUTTON)),
            (PillHit::Close, Rect::new(x + 2 * (BUTTON + 4), y, BUTTON, BUTTON)),
        ]
    }

    /// Everything the handle of window `i` may paint.
    fn handle_area(&self, i: usize) -> Rect {
        self.handle_bar(i).union(&self.handle_pill(i)).inset(-12)
    }

    pub(super) fn damage_handle(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            let a = self.handle_area(i);
            self.damage(a);
        }
    }

    /// The window whose top edge the pointer is near (only one not covered there).
    fn handle_target(&self, x: i32, y: i32) -> Option<(usize, Stage)> {
        let under = self.window_at(x, y);
        for i in (0..self.windows.len()).rev() {
            let w = &self.windows[i];
            if w.minimized || w.animating || w.awaiting_frame.is_some() {
                continue;
            }
            let r = w.rect;
            let showing = self.handle.filter(|h| h.window == w.id);
            let pill = self.handle_pill(i);
            if showing.is_some_and(|h| h.stage == Stage::Pill) && pill.inset(-6).contains(x, y) {
                return Some((i, Stage::Pill));
            }
            let zone = Rect::new(r.x, r.y - REACH, r.w, REACH + 26);
            if zone.contains(x, y) && (under.is_none() || under == Some(i) || y < r.y) {
                let near_bar = self.handle_bar(i).inset_xy(-30, -6).contains(x, y);
                let stage =
                    if near_bar || showing.is_some_and(|h| h.stage == Stage::Pill) { Stage::Pill } else { Stage::Bar };
                return Some((i, stage));
            }
            // A higher window covers what's below at this point.
            if under == Some(i) {
                return None;
            }
        }
        None
    }

    /// Pointer motion: shows, raises or hides the handle.
    pub(super) fn handle_hover(&mut self, x: i32, y: i32) {
        let next = if self.locked() || self.menu.is_some() || self.launcher.is_some() {
            None
        } else {
            self.handle_target(x, y).map(|(i, stage)| Handle { window: self.windows[i].id, stage })
        };
        let hover_button = next
            .filter(|h| h.stage == Stage::Pill)
            .and_then(|h| self.index_of(h.window))
            .and_then(|i| Self::pill_buttons(self.handle_pill(i)).into_iter().position(|(_, r)| r.contains(x, y)));
        if next != self.handle || hover_button != self.handle_button {
            for id in [self.handle.map(|h| h.window), next.map(|h| h.window)].into_iter().flatten() {
                self.damage_handle(id);
            }
            self.handle = next;
            self.handle_button = hover_button;
        }
    }

    /// A press on the handle: its buttons, or a drag to move. True if handled.
    pub(super) fn handle_press(&mut self, x: i32, y: i32, double: bool) -> bool {
        let Some(h) = self.handle else { return false };
        let Some(i) = self.index_of(h.window) else { return false };
        let pill = self.handle_pill(i);
        let hit = if h.stage == Stage::Pill && pill.contains(x, y) {
            Some(Self::pill_buttons(pill).into_iter().find(|(_, r)| r.contains(x, y)).map_or(PillHit::Grab, |(b, _)| b))
        } else if self.handle_bar(i).inset_xy(-30, -8).contains(x, y) {
            Some(PillHit::Grab)
        } else {
            None
        };
        let Some(hit) = hit else { return false };
        let id = h.window;
        if self.focused_id() != Some(id) {
            self.raise(id);
        }
        match hit {
            PillHit::Shrink => self.minimize(id),
            PillHit::Expand => self.toggle_zoom(id),
            PillHit::Close => self.request_close(id),
            PillHit::Grab if double => self.toggle_zoom(id),
            PillHit::Grab => self.begin_move(id, x, y),
        }
        self.handle = None;
        self.damage_all();
        true
    }

    /// Starts moving window `id` (restoring it first if it was moored or expanded).
    pub(super) fn begin_move(&mut self, id: u32, x: i32, y: i32) {
        let Some(i) = self.index_of(id) else { return };
        if let Some(saved) = self.windows[i].saved.take() {
            let mut restored = saved;
            restored.x = (x - saved.w / 2).clamp(0, (self.w - saved.w).max(0));
            restored.y = y.max(MENUBAR_H + 8);
            self.windows[i].rect = restored;
            self.windows[i].placement = Placement::Floating;
        }
        let start_rect = self.windows[i].rect;
        self.drag = Some(Drag { window: id, kind: DragKind::Move, start_x: x, start_y: y, start_rect });
        self.damage_all();
    }

    // -------------------------------------------------------------- Moorings

    /// The layouts row that drops from the top while a window is dragged.
    pub(super) fn moorings_rect(&self) -> Rect {
        let w = MOORINGS.len() as i32 * (TILE_W + 8) - 8 + 20;
        Rect::new(self.w / 2 - w / 2, MENUBAR_H + 6, w, TILE_H + 20)
    }

    fn tile(&self, n: usize) -> Rect {
        let m = self.moorings_rect();
        Rect::new(m.x + 10 + n as i32 * (TILE_W + 8), m.y + 10, TILE_W, TILE_H)
    }

    /// The zones of a layout tile, inside the tile.
    fn tile_zones(tile: Rect, layout: &[(u8, u8, u8, u8)]) -> alloc::vec::Vec<((u8, u8, u8, u8), Rect)> {
        let inner = tile.inset(4);
        layout
            .iter()
            .map(|&z| {
                let (x, y, w, h) = (z.0 as i32, z.1 as i32, z.2 as i32, z.3 as i32);
                let l = inner.x + inner.w * x / 12 + if x > 0 { 2 } else { 0 };
                let r = inner.x + inner.w * (x + w) / 12 - if x + w < 12 { 1 } else { 0 };
                let t = inner.y + inner.h * y / 12 + if y > 0 { 2 } else { 0 };
                let b = inner.y + inner.h * (y + h) / 12 - if y + h < 12 { 1 } else { 0 };
                (z, Rect::new(l, t, r - l, b - t))
            })
            .collect()
    }

    /// Dragging window `i` to (`x`, `y`): Moorings and the screen edges pick a target.
    pub(super) fn moorings_drag(&mut self, i: usize, x: i32, y: i32) {
        let area = self.work_area();
        let (mw, mh) = self.windows[i].app.min_size();
        let near_top = y < MENUBAR_H + 40 || (self.moorings_open && self.moorings_rect().inset(-24).contains(x, y));
        let mut zone = None;
        if self.windows[i].app.resizable() && near_top {
            for (n, layout) in MOORINGS.iter().enumerate() {
                if let Some((z, _)) =
                    Self::tile_zones(self.tile(n), layout).into_iter().find(|(_, r)| r.inset(-2).contains(x, y))
                {
                    zone = Some(z);
                }
            }
        }
        let placement = if !self.windows[i].app.resizable() {
            None
        } else if let Some(z) = zone {
            Some(windowing::zone_placement(z))
        } else if x < 18 {
            Some(Placement::Left)
        } else if x > self.w - 18 {
            Some(Placement::Right)
        } else {
            None
        };
        let preview = placement.and_then(|p| windowing::target(p, area, (mw, mh))).zip(placement);
        let open = near_top && self.windows[i].app.resizable();
        if preview != self.snap_preview || open != self.moorings_open || zone != self.mooring_zone {
            self.snap_preview = preview;
            self.moorings_open = open;
            self.mooring_zone = zone;
            self.damage_all();
        }
    }

    /// The ghost of the target and, while open, the layouts row.
    pub(super) fn paint_moorings(&self, cv: &mut Canvas) {
        let t = theme::current();
        let glow = theme::accent_glow();
        if let Some((r, _)) = self.snap_preview {
            cv.glow(r, 18, 14, glow, if t.dark { 60 } else { 80 });
            cv.fill_round_rect(r, 18, with_alpha(glow, 0x26));
            cv.stroke_round_rect(r, 18, with_alpha(glow, 0xC0));
            cv.stroke_round_rect(r.inset(1), 17, with_alpha(glow, 0x80));
        }
        if !self.moorings_open {
            return;
        }
        let m = self.moorings_rect();
        cv.shadow(m, 20, 26, 8, t.shadow * 2 / 3);
        cv.frosted(m, 20, t.panel_tint);
        for (n, layout) in MOORINGS.iter().enumerate() {
            let tile = self.tile(n);
            let hot = Self::tile_zones(tile, layout).iter().any(|(z, _)| Some(*z) == self.mooring_zone);
            cv.fill_round_rect(tile, 9, with_alpha(t.text, if hot { 0x26 } else { 0x10 }));
            if hot {
                cv.stroke_round_rect(tile, 9, with_alpha(glow, 0xD0));
            }
            for (z, r) in Self::tile_zones(tile, layout) {
                let on = Some(z) == self.mooring_zone;
                cv.fill_round_rect(r, 4, if on { glow } else { with_alpha(t.text, 0x30) });
            }
        }
        if let Some(z) = self.mooring_zone {
            let label = alloc::format!("{} · drop to moor", zone_name(z));
            let f = theme::ui_bold(12);
            let tag = Rect::new(self.w / 2 - (f.width(&label) + 20) / 2, m.bottom() + 8, f.width(&label) + 20, 26);
            cv.frosted(tag, 13, t.glass_tint);
            cv.text(tag.x + 10, tag.y + 18, &label, f, t.text_on_glass);
        }
    }

    // ---------------------------------------------------------- painting

    /// The handle of the window the pointer is near (above all windows).
    pub(super) fn paint_handle(&self, cv: &mut Canvas) {
        let Some(h) = self.handle else { return };
        let Some(i) = self.index_of(h.window) else { return };
        if !self.handle_area(i).intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        match h.stage {
            Stage::Bar => {
                let bar = self.handle_bar(i);
                cv.fill_round_rect(bar, 3, with_alpha(t.text, 0xA8));
            }
            Stage::Pill => {
                let pill = self.handle_pill(i);
                cv.shadow(pill, PILL_H / 2, 16, 4, t.shadow / 3);
                cv.frosted(pill, PILL_H / 2, t.glass_tint);
                let title = self.windows[i].app.title();
                let f = theme::ui_bold(13);
                let buttons = Self::pill_buttons(pill);
                cv.text_clipped(pill.x + 14, pill.y + 21, &title, f, t.text_on_glass, buttons[0].1.x - pill.x - 22);
                for (n, (b, r)) in buttons.into_iter().enumerate() {
                    let hovered = self.handle_button == Some(n);
                    let (cx, cy) = r.center();
                    let ink = if b == PillHit::Close { theme::CLOSE } else { t.text_on_glass };
                    if b == PillHit::Close {
                        cv.fill_circle(cx, cy, BUTTON / 2, with_alpha(theme::CLOSE, if hovered { 0x58 } else { 0x2E }));
                    } else if hovered {
                        cv.fill_circle(cx, cy, BUTTON / 2, t.hover);
                    }
                    match b {
                        PillHit::Shrink => cv.line(cx - 4, cy, cx + 4, cy, 2, ink),
                        PillHit::Expand => {
                            cv.line(cx + 1, cy - 4, cx + 4, cy - 4, 2, ink);
                            cv.line(cx + 4, cy - 4, cx + 4, cy - 1, 2, ink);
                            cv.line(cx - 1, cy + 4, cx - 4, cy + 4, 2, ink);
                            cv.line(cx - 4, cy + 4, cx - 4, cy + 1, 2, ink);
                        }
                        PillHit::Close => {
                            cv.line(cx - 3, cy - 3, cx + 3, cy + 3, 2, ink);
                            cv.line(cx + 3, cy - 3, cx - 3, cy + 3, 2, ink);
                        }
                        PillHit::Grab => {}
                    }
                }
            }
        }
    }
}

/// "Left half", "Top right quarter", … for the Moorings caption.
fn zone_name(z: (u8, u8, u8, u8)) -> &'static str {
    match z {
        (0, 0, 6, 12) => "Left half",
        (6, 0, 6, 12) => "Right half",
        (0, 0, 8, 12) => "Left two thirds",
        (8, 0, 4, 12) => "Right third",
        (0, 0, 4, 12) => "Left third",
        (4, 0, 8, 12) => "Right two thirds",
        (4, 0, 4, 12) => "Middle third",
        (0, 0, 6, 6) => "Top left quarter",
        (6, 0, 6, 6) => "Top right quarter",
        (0, 6, 6, 6) => "Bottom left quarter",
        (6, 6, 6, 6) => "Bottom right quarter",
        _ => "Whole screen",
    }
}

/// The waterline: a glowing line along the focused window's lower edge.
pub(super) fn paint_waterline(cv: &mut Canvas, r: Rect, radius: i32) {
    let c = theme::waterline();
    let dark = theme::current().dark;
    // The glow beneath, strongest in the middle third, fading at the ends.
    let (x0, x1) = (r.x + r.w * 12 / 100, r.right() - r.w * 12 / 100);
    for dy in 1..=14 {
        let a = (if dark { 70 } else { 46 }) * (15 - dy) * (15 - dy) / 225;
        let row = Rect::new(x0, r.bottom() + dy - 2, x1 - x0, 1);
        cv.fill_rect_with(row, |x, _| {
            let edge = ((x - x0).min(x1 - x) * 4 * 256 / (x1 - x0).max(1)).min(256);
            with_alpha(c, (a as i32 * edge / 256) as u8)
        });
    }
    // The line itself, on the window's inside edge.
    cv.with_clip(Rect::new(r.x, r.bottom() - 2, r.w, 2).intersect(&cv.clip), |cv| {
        cv.fill_round_rect_with(Rect::new(r.x, r.bottom() - radius * 2, r.w, radius * 2), radius, |x, _| {
            let edge = ((x - r.x).min(r.right() - x) * 6 * 256 / r.w.max(1)).min(256);
            with_alpha(c, (230 * edge / 256) as u8)
        });
    });
}
