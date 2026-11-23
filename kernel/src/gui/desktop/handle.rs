//! Reserved window chrome, the waterline and Moorings.
//!
//! Apps draw below a stable strip containing the title and hover controls.
//! Drag the strip — or hold Super and drag anywhere — to move the window.
//! While a window is dragged, Moorings drops six layouts from the top of the
//! screen; slide onto a zone (or push into a screen edge) and let go to moor
//! the window there. The focused window glows along its lower edge: the
//! waterline.

use super::{Desktop, Drag, DragKind};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme::{self, MENUBAR_H};
use lumen::windowing::{self, Placement, MOORINGS};

const BUTTON: i32 = 26;
const TILE_W: i32 = 58;
const TILE_H: i32 = 40;

fn strip(r: Rect) -> Rect {
    Rect::new(r.x, r.y, r.w, windowing::CHROME_H)
}
/// Only this opaque portion changes during hover. Keeping damage away from
/// app sidebars avoids rebuilding their backdrop while the controls fade.
fn control_area(r: Rect) -> Rect {
    Rect::new(r.right() - 112, r.y, 112, windowing::CHROME_H)
}

fn buttons(r: Rect) -> [Rect; 3] {
    core::array::from_fn(|n| Rect::new(r.right() - 104 + n as i32 * 30, r.y + 4, BUTTON, BUTTON))
}

impl Desktop {
    pub(super) fn damage_handle(&mut self, id: u32) {
        if let Some(i) = self.index_of(id) {
            self.damage(strip(self.windows[i].rect));
        }
    }

    pub(super) fn resize_press(&mut self, x: i32, y: i32) -> bool {
        let Some(i) = self.window_at(x, y) else { return false };
        let w = &self.windows[i];
        let edges = if w.app.resizable() { windowing::resize_edges(w.rect, x, y) } else { 0 };
        if edges == 0 {
            return false;
        }
        let d = Drag {
            window: w.id,
            kind: DragKind::Resize(edges),
            start_x: x,
            start_y: y,
            start_rect: w.rect,
            started: true,
        };
        self.raise(d.window);
        self.drag = Some(d);
        self.damage_cursor();
        true
    }

    pub(super) fn handle_hover(&mut self, x: i32, y: i32) {
        let target = if self.locked()
            || self.menu.is_some()
            || self.launcher.is_some()
            || self.spotlight_open()
            || self.current_open()
        {
            None
        } else {
            self.window_at(x, y).filter(|&i| strip(self.windows[i].rect).contains(x, y))
        };
        let mut damage = alloc::vec::Vec::new();
        for (i, w) in self.windows.iter_mut().enumerate() {
            let visible = target == Some(i) || w.chrome_focus.is_some() || self.drag.is_some_and(|d| d.window == w.id);
            let hover = (target == Some(i)).then(|| buttons(w.rect).iter().position(|r| r.contains(x, y))).flatten();
            let before = w.chrome.value(self.now_ms, theme::reduce_motion());
            w.chrome.set(visible, self.now_ms, theme::reduce_motion());
            if hover != w.chrome_hover
                || before != w.chrome.value(self.now_ms, theme::reduce_motion())
                || w.chrome.moving(self.now_ms, theme::reduce_motion())
            {
                damage.push((w.id, control_area(w.rect)));
            }
            w.chrome_hover = hover;
        }
        for (id, r) in damage {
            self.damage_chrome(id, r);
        }
    }

    pub(super) fn step_handle(&mut self) -> bool {
        let (x, y) = self.cursor;
        self.handle_hover(x, y);
        let damage: alloc::vec::Vec<(u32, Rect)> = self
            .windows
            .iter()
            .filter(|w| w.chrome.moving(self.now_ms, theme::reduce_motion()))
            .map(|w| (w.id, control_area(w.rect)))
            .collect();
        let busy = !damage.is_empty();
        for (id, r) in damage {
            self.damage_chrome(id, r);
        }
        busy
    }

    /// Damage from window `id`'s own controls. On a window being moved it is
    /// drawn over the backdrop, so the scene below stays valid.
    fn damage_chrome(&mut self, id: u32, r: Rect) {
        self.drag_damage = self.drag.is_some_and(|d| d.window == id);
        self.damage(r);
        self.drag_damage = false;
    }

    fn control_action(&mut self, id: u32, button: usize) {
        match button {
            0 => self.minimize(id),
            1 => self.toggle_zoom(id),
            _ => self.request_close(id),
        }
    }

    pub(super) fn handle_key(&mut self, k: &crate::drivers::input::KeyEvent) -> bool {
        use crate::drivers::input::KeyCode;
        if !k.pressed {
            return false;
        }
        let Some(i) = self.focused_id().and_then(|id| self.index_of(id)) else {
            return false;
        };
        if k.mods.alt && k.ch == Some(' ') {
            self.windows[i].chrome_focus = Some(0);
            self.damage_handle(self.windows[i].id);
            return true;
        }
        let Some(n) = self.windows[i].chrome_focus else {
            return false;
        };
        let id = self.windows[i].id;
        match k.code {
            KeyCode::Tab | KeyCode::Right | KeyCode::Left => {
                let backwards = k.mods.shift || k.code == KeyCode::Left;
                self.windows[i].chrome_focus = Some((n + if backwards { 2 } else { 1 }) % 3);
            }
            KeyCode::Enter => self.control_action(id, n),
            KeyCode::Escape => self.windows[i].chrome_focus = None,
            _ if k.ch == Some(' ') => self.control_action(id, n),
            _ => return false,
        }
        self.damage_handle(id);
        true
    }

    pub(super) fn handle_press(&mut self, x: i32, y: i32, double: bool) -> bool {
        let Some(i) = self.window_at(x, y) else {
            return false;
        };
        let r = self.windows[i].rect;
        if !strip(r).contains(x, y) {
            return false;
        }
        let id = self.windows[i].id;
        let action = buttons(r).iter().position(|r| r.contains(x, y));
        self.raise(id);
        if let Some(n) = action {
            self.control_action(id, n);
        } else if double {
            self.toggle_zoom(id);
        } else {
            self.begin_move(id, x, y);
        }
        true
    }

    /// Arms a move. Moored windows restore only after actual pointer movement.
    pub(super) fn begin_move(&mut self, id: u32, x: i32, y: i32) {
        let Some(i) = self.index_of(id) else { return };
        let start_rect = self.windows[i].rect;
        self.drag = Some(Drag { window: id, kind: DragKind::Move, start_x: x, start_y: y, start_rect, started: false });
        self.damage_cursor();
    }

    // -------------------------------------------------------------- Moorings

    /// The layouts row that drops from the top of the display a window is
    /// dragged on (below the bar on the main display).
    pub(super) fn moorings_rect(&self) -> Rect {
        let w = MOORINGS.len() as i32 * (TILE_W + 8) - 8 + 20;
        let on = self.moorings_on.min(self.display_rects().len() - 1);
        let d = self.display_rects()[on];
        let top = if on == self.main_index() { MENUBAR_H + 6 } else { d.y + 6 };
        Rect::new(d.x + d.w / 2 - w / 2, top, w, TILE_H + 20)
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
        // Moorings and the edges belong to the display under the pointer.
        let on = self.display_at(x, y);
        if on != self.moorings_on {
            self.moorings_on = on;
            self.moorings_open = false;
        }
        let rects = self.display_rects();
        let d = rects[on];
        let area = self.work_area_on(on);
        let (mw, mh) = self.windows[i].minimum();
        // An edge another display continues is for crossing, not for mooring.
        let outer = |ex: i32| lumen::displays::at(&rects, ex, y).is_none();
        let near_top = (y < area.y + 32 && y >= d.y)
            || (self.moorings_open && self.moorings_rect().inset(-24).contains(x, y));
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
        } else if x < d.x + 18 && outer(d.x - 1) {
            Some(Placement::Left)
        } else if x > d.right() - 18 && outer(d.right()) {
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
            let tag = Rect::new(m.x + m.w / 2 - (f.width(&label) + 20) / 2, m.bottom() + 8, f.width(&label) + 20, 26);
            cv.frosted(tag, 13, t.glass_tint);
            cv.text(tag.x + 10, tag.y + 18, &label, f, t.text_on_glass);
        }
    }

    // ---------------------------------------------------------- painting
}

/// Painted with each window so higher windows occlude both chrome and controls.
pub(super) fn paint_chrome(cv: &mut Canvas, w: &super::Window, focused: bool, now: u64) {
    let t = theme::current();
    let r = strip(w.rect);
    let reveal = w.chrome.value(now, theme::reduce_motion());
    cv.with_clip(r, |cv| cv.fill_round_rect(w.rect, theme::WINDOW_RADIUS, t.titlebar));
    cv.fill_rect(Rect::new(r.x, r.bottom() - 1, r.w, 1), t.separator);
    let f = theme::ui(12);
    cv.text_clipped(
        r.x + 16,
        r.y + 21,
        &w.app.title(),
        f,
        if focused { t.text } else { t.text_secondary },
        (r.w - 140).max(0),
    );
    let alpha = (255 * reveal / 1000) as u8;
    if reveal < 1000 {
        cv.fill_round_rect(
            Rect::new(r.right() - 58, r.y + 15, 26, 3),
            2,
            with_alpha(t.text_secondary, (110 * (1000 - reveal) / 1000) as u8),
        );
    }
    if reveal == 0 {
        return;
    }
    for (n, b) in buttons(w.rect).into_iter().enumerate() {
        let b = b.offset(0, (1000 - reveal) * -3 / 1000);
        let (cx, cy) = b.center();
        let hot = w.chrome_hover == Some(n) || w.chrome_focus == Some(n);
        let ink = if n == 2 && hot { theme::CLOSE } else { t.text };
        if hot {
            cv.fill_round_rect(
                b,
                8,
                with_alpha(if n == 2 { theme::CLOSE } else { t.text }, if n == 2 { 35 } else { 22 }),
            );
        }
        if w.chrome_focus == Some(n) {
            cv.stroke_round_rect(b, 8, theme::accent());
        }
        let ink = with_alpha(ink, alpha);
        match n {
            0 => cv.line(cx - 4, cy + 1, cx + 4, cy + 1, 1, ink),
            1 if w.placement == Placement::Maximized => {
                cv.stroke_round_rect(Rect::new(cx - 2, cy - 5, 8, 8), 2, ink);
                cv.fill_round_rect(Rect::new(cx - 5, cy - 2, 8, 8), 2, t.titlebar);
                cv.stroke_round_rect(Rect::new(cx - 5, cy - 2, 8, 8), 2, ink);
            }
            1 => cv.stroke_round_rect(
                Rect::new(cx - 4, cy - 4, 9, 9),
                2,
                with_alpha(ink, if w.app.resizable() { alpha } else { alpha / 3 }),
            ),
            _ => {
                cv.line(cx - 3, cy - 3, cx + 3, cy + 3, 1, ink);
                cv.line(cx + 3, cy - 3, cx - 3, cy + 3, 1, ink);
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
