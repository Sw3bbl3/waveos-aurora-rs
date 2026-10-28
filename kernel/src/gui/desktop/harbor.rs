//! Harbor: where apps are moored, floating at the bottom in three bays.
//!
//! The first bay holds Shoal and the pinned apps, the second the apps that
//! are open but not pinned, the third the Downloads and Documents stacks and
//! the Trash. A glowing waterline under an icon means it's open (wider for
//! the focused app); pointing at an icon lifts it and names it, and resting
//! on an open app peeks at its windows. Starting an app sends ripples out
//! from its icon. Harbor sinks away from a focused window that reaches it
//! and rises again when the pointer touches the bottom edge.

use super::Desktop;
use crate::gui::apps::{self, AppKind, CATALOG};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::theme;
use alloc::string::String;
use alloc::vec::Vec;

const GAP: i32 = 8;
const BAY_GAP: i32 = 8;
const PAD: i32 = 10;
const SLIDE_MS: u64 = 240;
const PEEK_DELAY_MS: u64 = 450;
const RIPPLE_MS: u64 = 600;
const THUMB_W: i32 = 150;
const THUMB_H: i32 = 96;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Stack {
    Downloads,
    Documents,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum DockItem {
    Launcher,
    App(AppKind),
    External(u32),
    Stack(Stack),
    Trash,
}

/// The windows of the app the pointer rests on.
pub(super) struct Peek {
    item: usize,
    thumbs: Vec<(u32, String, aurora_image::Image)>,
    hover: Option<usize>,
}

#[derive(Default)]
pub(super) struct HarborState {
    pub hover: Option<usize>,
    /// The pointer is at the bottom edge (or on Harbor): show it even over a window.
    pub revealed: bool,
    hidden: bool,
    changed_at: u64,
    pub peek: Option<Peek>,
    peek_pending: Option<(usize, u64)>,
}

fn ease_out(t: i64) -> i64 {
    let t = t.clamp(0, 1000);
    let inv = 1000 - t;
    1000 - inv * inv / 1000 * inv / 1000
}

impl Desktop {
    fn icon_size(&self) -> i32 {
        theme::dock_icon_size()
    }

    fn bay_h(&self) -> i32 {
        self.icon_size() + 24
    }

    /// The bays' contents, in order.
    fn bays(&self) -> [Vec<DockItem>; 3] {
        let mut pinned = alloc::vec![DockItem::Launcher];
        pinned.extend(CATALOG.iter().filter(|a| a.pinned).map(|a| DockItem::App(a.kind)));
        let mut running: Vec<DockItem> = Vec::new();
        for w in &self.windows {
            let k = w.app.kind();
            let item = if k == AppKind::Other { DockItem::External(w.id) } else { DockItem::App(k) };
            if !apps::info(k).pinned
                && !matches!(k, AppKind::Power | AppKind::Crash | AppKind::DisplayConfirm)
                && !running.contains(&item)
            {
                running.push(item);
            }
        }
        let recent = alloc::vec![DockItem::Stack(Stack::Downloads), DockItem::Stack(Stack::Documents), DockItem::Trash];
        [pinned, running, recent]
    }

    /// How far Harbor has sunk below its place (0 = fully shown).
    fn harbor_offset(&self) -> i32 {
        let depth = self.bay_h() + 30;
        let t = if theme::reduce_motion() {
            1000
        } else {
            (self.now_ms.saturating_sub(self.harbor.changed_at) * 1000 / SLIDE_MS) as i64
        };
        let e = ease_out(t) as i32;
        if self.harbor.hidden {
            depth * e / 1000
        } else {
            depth - depth * e / 1000
        }
    }

    /// Harbor at rest: the bays and every item's icon rectangle.
    pub(super) fn harbor_layout(&self) -> (Rect, Vec<Rect>, Vec<(DockItem, Rect)>) {
        let s = self.icon_size();
        let bays: Vec<Vec<DockItem>> = self.bays().into_iter().filter(|b| !b.is_empty()).collect();
        let widths: Vec<i32> = bays.iter().map(|b| b.len() as i32 * (s + GAP) - GAP + 2 * PAD).collect();
        let total: i32 = widths.iter().sum::<i32>() + BAY_GAP * (bays.len() as i32 - 1);
        let y = self.h - 10 - self.bay_h() + self.harbor_offset();
        let mut x = (self.w - total) / 2;
        let mut bay_rects = Vec::new();
        let mut items = Vec::new();
        for (bay, w) in bays.into_iter().zip(widths) {
            bay_rects.push(Rect::new(x, y, w, self.bay_h()));
            let mut ix = x + PAD;
            for item in bay {
                items.push((item, Rect::new(ix, y + self.bay_h() - 11 - s, s, s)));
                ix += s + GAP;
            }
            x += w + BAY_GAP;
        }
        (Rect::new((self.w - total) / 2, y, total, self.bay_h()), bay_rects, items)
    }

    pub(super) fn dock_layout(&self) -> (Rect, Vec<(DockItem, Rect)>) {
        let (r, _, items) = self.harbor_layout();
        (r, items)
    }

    pub(super) fn dock_rect(&self) -> Rect {
        self.harbor_layout().0
    }

    /// Everything Harbor may paint over: the bays, lifted icons, labels and peeks.
    fn harbor_area(&self) -> Rect {
        let r = self.dock_rect();
        Rect::new(r.x - 120, r.y - THUMB_H - 110, r.w + 240, self.h - (r.y - THUMB_H - 110))
    }

    fn is_running(&self, kind: AppKind) -> bool {
        self.windows.iter().any(|w| w.app.kind() == kind)
    }

    /// Sinks Harbor when the focused window reaches it (or autohide is on), unless revealed.
    pub(super) fn update_harbor(&mut self) {
        let resting = Rect::new(0, self.h - 10 - self.bay_h() - 6, self.w, self.bay_h() + 16);
        let covered = self.focused().is_some_and(|w| w.rect.intersects(&resting));
        let hide = (covered || theme::dock_autohide())
            && !self.harbor.revealed
            && self.launcher.is_none()
            && self.dnd.is_none();
        if hide != self.harbor.hidden {
            // Start from where it is now.
            let elapsed = self.now_ms.saturating_sub(self.harbor.changed_at).min(SLIDE_MS);
            self.harbor.hidden = hide;
            self.harbor.changed_at = self.now_ms - (SLIDE_MS - elapsed);
            self.harbor.peek = None;
            let a = self.harbor_area();
            self.damage(a.union(&Rect::new(0, self.h - self.bay_h() - 60, self.w, self.bay_h() + 60)));
        }
    }

    pub(super) fn harbor_moving(&self) -> bool {
        self.now_ms < self.harbor.changed_at + SLIDE_MS + 16
            || self.launching.iter().any(|&(_, t)| self.now_ms < t + 3000)
    }

    pub(super) fn harbor_deadline(&self) -> u64 {
        self.harbor.peek_pending.map(|(_, at)| at + PEEK_DELAY_MS).unwrap_or(u64::MAX)
    }

    /// The icon of the app owning window `id` (for minimize and restore).
    pub(super) fn dock_icon_of(&self, id: u32) -> Rect {
        let kind = self.index_of(id).map(|i| self.windows[i].app.kind());
        let (dock, items) = self.dock_layout();
        items
            .into_iter()
            .find(|(it, _)| match it {
                DockItem::App(k) => Some(*k) == kind,
                DockItem::External(w) => *w == id,
                _ => false,
            })
            .map(|(_, r)| r)
            .unwrap_or(Rect::new(dock.x + dock.w / 2 - 24, dock.y + 8, 48, 48))
    }

    fn peek_rect(&self) -> Option<Rect> {
        let p = self.harbor.peek.as_ref()?;
        let (_, items) = self.dock_layout();
        let (_, icon) = items.get(p.item)?;
        let n = p.thumbs.len().max(1) as i32;
        let w = n * (THUMB_W + 10) - 10 + 24;
        let h = THUMB_H + 54;
        let x = (icon.x + icon.w / 2 - w / 2).clamp(8, self.w - w - 8);
        Some(Rect::new(x, icon.y - 22 - h, w, h))
    }

    fn peek_thumb_rects(&self) -> Vec<Rect> {
        let Some(p) = self.peek_rect() else { return Vec::new() };
        let n = self.harbor.peek.as_ref().map_or(0, |p| p.thumbs.len());
        (0..n as i32).map(|i| Rect::new(p.x + 12 + i * (THUMB_W + 10), p.y + 32, THUMB_W, THUMB_H)).collect()
    }

    /// Opens the peek for item `index` (its app's windows, as small pictures).
    fn open_peek(&mut self, index: usize) {
        let (_, items) = self.dock_layout();
        let Some((item, _)) = items.get(index).copied() else { return };
        let ids: Vec<u32> = self
            .windows
            .iter()
            .rev()
            .filter(|w| !w.minimized && w.awaiting_frame.is_none())
            .filter(|w| match item {
                DockItem::App(k) => w.app.kind() == k,
                DockItem::External(id) => w.id == id,
                _ => false,
            })
            .map(|w| w.id)
            .take(3)
            .collect();
        if ids.is_empty() {
            return;
        }
        let mut thumbs = Vec::new();
        for id in ids {
            let title = self.index_of(id).map(|i| self.windows[i].app.title()).unwrap_or_default();
            if let Some((px, w, h)) = self.snapshot(id) {
                let img = aurora_image::Image { width: w, height: h, pixels: px };
                thumbs.push((id, title, {
                    let k = crate::gui::scale() as u32;
                    img.thumbnail(THUMB_W as u32 * k, THUMB_H as u32 * k)
                }));
            }
        }
        self.harbor.peek = Some(Peek { item: index, thumbs, hover: None });
        let a = self.harbor_area();
        self.damage(a);
    }

    pub(super) fn step_harbor(&mut self) -> bool {
        if let Some((index, at)) = self.harbor.peek_pending {
            if self.now_ms >= at + PEEK_DELAY_MS {
                self.harbor.peek_pending = None;
                if self.harbor.hover == Some(index) && self.harbor.peek.is_none() {
                    self.open_peek(index);
                }
            }
        }
        let moving = self.harbor_moving();
        if moving {
            let a = self.harbor_area();
            self.damage(a.union(&Rect::new(0, self.h - self.bay_h() - 60, self.w, self.bay_h() + 60)));
        }
        moving
    }

    // ------------------------------------------------------------ input

    pub(super) fn harbor_hover(&mut self, x: i32, y: i32) {
        let (dock, items) = self.dock_layout();
        // The bottom edge (or Harbor itself) brings it up over a window.
        let reveal = y >= self.h - 3
            || (self.harbor.revealed && y >= dock.y - 24)
            || (!self.harbor.hidden && dock.contains(x, y));
        if reveal != self.harbor.revealed {
            self.harbor.revealed = reveal;
            self.update_harbor();
        }
        // Over the peek: keep it and track the thumbnail.
        if let Some(p) = self.peek_rect() {
            if p.inset(-8).contains(x, y) || Rect::new(p.x, p.bottom(), p.w, dock.y - p.bottom() + 8).contains(x, y) {
                let h = self.peek_thumb_rects().iter().position(|r| r.contains(x, y));
                if let Some(peek) = self.harbor.peek.as_mut() {
                    if peek.hover != h {
                        peek.hover = h;
                        self.damage(p.inset(-20));
                    }
                }
                return;
            }
        }
        let hover = if self.harbor.hidden && !self.harbor.revealed {
            None
        } else {
            items.iter().position(|(_, r)| r.inset(-GAP / 2).contains(x, y))
        };
        if hover != self.harbor.hover {
            self.harbor.hover = hover;
            self.harbor.peek_pending = hover
                .filter(|&i| {
                    matches!(items[i].0, DockItem::App(k) if self.is_running(k))
                        || matches!(items[i].0, DockItem::External(_))
                })
                .map(|i| (i, self.now_ms));
            if self.harbor.peek.as_ref().is_some_and(|p| Some(p.item) != hover) {
                self.harbor.peek = None;
            }
            let a = self.harbor_area();
            self.damage(a);
        }
    }

    /// A click on Harbor or its peek. True if handled.
    pub(super) fn harbor_press(&mut self, x: i32, y: i32) -> bool {
        if let Some(i) = self.peek_thumb_rects().iter().position(|r| r.contains(x, y)) {
            let id = self.harbor.peek.as_ref().map(|p| p.thumbs[i].0);
            self.harbor.peek = None;
            if let Some(id) = id {
                self.raise(id);
            }
            return true;
        }
        if self.peek_rect().is_some_and(|p| p.contains(x, y)) {
            return true;
        }
        let (dock, items) = self.dock_layout();
        if self.harbor.hidden && !self.harbor.revealed || !dock.inset(-4).contains(x, y) {
            return false;
        }
        self.harbor.peek = None;
        if let Some((item, _)) = items.into_iter().find(|(_, r)| r.inset(-GAP / 2).contains(x, y)) {
            self.dock_click(item);
        }
        true
    }

    fn dock_click(&mut self, item: DockItem) {
        match item {
            DockItem::External(id) => {
                if self.focused_id() == Some(id) {
                    self.minimize(id);
                } else {
                    self.raise(id);
                }
            }
            DockItem::Launcher => self.toggle_launcher(),
            DockItem::App(kind) => {
                let focused_kind = self.focused().map(|w| w.app.kind());
                let topmost = self.windows.iter().rev().find(|w| w.app.kind() == kind).map(|w| (w.id, w.minimized));
                match topmost {
                    Some((id, false)) if focused_kind == Some(kind) => self.minimize(id),
                    Some((id, _)) => self.raise(id),
                    None => self.open(kind),
                }
            }
            DockItem::Stack(stack) => {
                let folder = match stack {
                    Stack::Downloads => "/Downloads",
                    Stack::Documents => "/Documents",
                };
                self.spawn_program(apps::info(AppKind::Files).path, &[folder]);
            }
            DockItem::Trash => self.spawn_program(apps::info(AppKind::Files).path, &["/Trash"]),
        }
    }

    // --------------------------------------------------------- painting

    pub(super) fn paint_harbor(&self, cv: &mut Canvas) {
        let (dock, bays, items) = self.harbor_layout();
        if dock.y >= self.h || !self.harbor_area().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        let s = self.icon_size();
        for b in &bays {
            cv.shadow(*b, 24, 22, 6, t.shadow / 3);
            cv.frosted(*b, 24, t.glass_tint);
        }
        let focused = self.focused().map(|w| (w.app.kind(), w.id));
        for (idx, (item, r)) in items.iter().enumerate() {
            let hovered = self.harbor.hover == Some(idx);
            // Pointing lifts and enlarges the icon.
            let r = if hovered {
                let g = s * 5 / 4;
                Rect::new(r.x + (s - g) / 2, r.bottom() - g - 6, g, g)
            } else {
                *r
            };
            let base = items[idx].1;
            let launching = matches!(item, DockItem::App(k) if self.launching.iter().any(|&(lk, at)| lk == *k && self.now_ms < at + 3000));
            if launching {
                // Two rings spread from the icon while the app starts.
                let (cx, cy) = base.center();
                for k in 0..2u64 {
                    let at = self
                        .launching
                        .iter()
                        .find(|&&(lk, _)| matches!(item, DockItem::App(kk) if *kk == lk))
                        .map(|&(_, at)| at)
                        .unwrap_or(0);
                    let phase = (self.now_ms.saturating_sub(at) + k * RIPPLE_MS / 2) % RIPPLE_MS;
                    let rr = s / 2 + (phase as i32 * 22 / RIPPLE_MS as i32);
                    let a = 220 - phase as i32 * 220 / RIPPLE_MS as i32;
                    cv.stroke_arc(
                        cx * 16,
                        cy * 16,
                        rr * 16,
                        28,
                        0,
                        1024,
                        with_alpha(theme::accent_glow(), a.max(0) as u8),
                    );
                }
            }
            match item {
                DockItem::Launcher => icons::draw(cv, Icon::Launcher, r),
                DockItem::App(kind) => icons::draw(cv, apps::info(*kind).icon, r),
                DockItem::External(_) => icons::draw(cv, Icon::Aurora, r),
                DockItem::Trash => icons::draw(cv, if self.trash_full { Icon::TrashFull } else { Icon::Trash }, r),
                DockItem::Stack(stack) => paint_stack(cv, r, *stack),
            }
            // The waterline: open apps glow beneath; the focused one widest.
            let open = match item {
                DockItem::App(k) => self.is_running(*k),
                DockItem::External(_) => true,
                DockItem::Launcher => self.launcher.is_some(),
                _ => false,
            };
            if open {
                let is_focused = match item {
                    DockItem::App(k) => focused.is_some_and(|(fk, _)| fk == *k),
                    DockItem::External(id) => focused.is_some_and(|(_, fid)| fid == *id),
                    DockItem::Launcher => self.launcher.is_some(),
                    _ => false,
                };
                let w = if is_focused { 28 } else { 16 };
                let line = Rect::new(base.x + base.w / 2 - w / 2, base.bottom() + 5, w, 3);
                let c = theme::waterline();
                if t.dark {
                    cv.glow(line, 2, 4, c, if is_focused { 90 } else { 50 });
                }
                cv.fill_round_rect(line, 2, with_alpha(c, if is_focused { 255 } else { 190 }));
            }
        }
        // The name of the pointed-at item.
        if let Some((item, r)) = self.harbor.hover.and_then(|i| items.get(i)) {
            if self.harbor.peek.is_none() {
                let external;
                let label = match item {
                    DockItem::External(id) => {
                        external = self.windows.iter().find(|w| w.id == *id).map(|w| w.app.title()).unwrap_or_default();
                        external.as_str()
                    }
                    DockItem::Launcher => "Shoal",
                    DockItem::App(k) => apps::info(*k).name,
                    DockItem::Stack(Stack::Downloads) => "Downloads",
                    DockItem::Stack(Stack::Documents) => "Documents",
                    DockItem::Trash => "Trash",
                };
                let f = theme::ui_bold(12);
                let w = f.width(label) + 22;
                let tip = Rect::new(r.x + r.w / 2 - w / 2, r.y - 46, w, 26);
                cv.shadow(tip, 13, 12, 3, t.shadow / 3);
                cv.frosted(tip, 13, t.panel_tint);
                cv.text_centered(tip, label, f, t.text);
            }
        }
        self.paint_peek(cv);
    }

    fn paint_peek(&self, cv: &mut Canvas) {
        let (Some(p), Some(peek)) = (self.peek_rect(), self.harbor.peek.as_ref()) else { return };
        let t = theme::current();
        cv.shadow(p, 18, 26, 8, t.shadow * 2 / 3);
        cv.frosted(p, 18, t.panel_tint);
        let (_, items) = self.dock_layout();
        let name = match items.get(peek.item).map(|(i, _)| *i) {
            Some(DockItem::App(k)) => String::from(apps::info(k).name),
            _ => String::from("Window"),
        };
        cv.text(p.x + 14, p.y + 22, &name, theme::ui_bold(13), t.text);
        let count = alloc::format!("{} window{}", peek.thumbs.len(), if peek.thumbs.len() == 1 { "" } else { "s" });
        let f = theme::ui(12);
        cv.text(p.right() - 14 - f.width(&count), p.y + 22, &count, f, t.text_secondary);
        for (i, (r, (_, title, img))) in self.peek_thumb_rects().into_iter().zip(&peek.thumbs).enumerate() {
            // The thumbnail is in device pixels.
            let (iw, ih) = (img.width as i32 / cv.scale, img.height as i32 / cv.scale);
            let pic = Rect::new(r.x + (r.w - iw) / 2, r.y + (r.h - 18 - ih) / 2, iw, ih);
            cv.blit(&img.pixels, img.width, img.height, pic, 8);
            if peek.hover == Some(i) {
                cv.stroke_round_rect(pic.inset(-2), 10, theme::accent());
                cv.stroke_round_rect(pic.inset(-3), 11, theme::accent());
            }
            cv.text_clipped(r.x + 2, r.bottom() - 2, title, theme::ui(11), t.text_secondary, r.w - 4);
        }
    }
}

/// The Downloads (fanned cards) and Documents (a folder) stacks.
fn paint_stack(cv: &mut Canvas, r: Rect, stack: Stack) {
    let s = r.w;
    let u = |v: i32| v * s / 50;
    match stack {
        Stack::Downloads => {
            // A tray the arrow drops into.
            let body = Rect::new(r.x + u(2), r.y + u(2), u(46), u(46));
            cv.fill_round_rect_vgradient(body, u(12), 0xFF4F_E0D2, 0xFF1F_8BE0);
            let (cx, cy) = body.center();
            cv.line(cx, cy - u(13), cx, cy + u(4), 3, 0xFFFF_FFFF);
            cv.line(cx - u(8), cy - u(4), cx, cy + u(5), 3, 0xFFFF_FFFF);
            cv.line(cx + u(8), cy - u(4), cx, cy + u(5), 3, 0xFFFF_FFFF);
            cv.line(cx - u(12), cy + u(12), cx + u(12), cy + u(12), 3, 0xFFFF_FFFF);
        }
        Stack::Documents => {
            let tab = Rect::new(r.x + u(2), r.y + u(6), u(22), u(10));
            cv.fill_round_rect(tab, u(4), 0xFF7A_B8FF);
            let body = Rect::new(r.x + u(2), r.y + u(10), u(46), u(38));
            cv.fill_round_rect_vgradient(body, u(10), 0xFF7A_B8FF, 0xFF2F_62F5);
            for (i, w) in [20, 20, 13].iter().enumerate() {
                let y = body.y + u(12) + i as i32 * u(7);
                cv.line(r.x + u(15), y, r.x + u(15) + u(*w), y, 2, 0xFFFF_FFFF);
            }
        }
    }
}
