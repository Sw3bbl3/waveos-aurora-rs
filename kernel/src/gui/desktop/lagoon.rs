//! Lagoon: every window of the Cove in front, spread out side by side.
//!
//! Super+Tab, F3 or three fingers swept up on a touchpad calm the water:
//! the wallpaper blurs, and each window glides on a spring from where it
//! is to a place of its own, as a live picture with its name and its app's
//! icon (minimised windows rise from Harbor into a row of their own). Each
//! display shows its own windows. Point at one, or move with the arrow
//! keys or Tab; click it or press Enter to bring it forward, and every
//! window glides back. Typing narrows the pictures to the windows whose
//! name or app matches; Escape clears that, then returns. The × on a
//! picture (or Ctrl+W, Delete) closes that window.
//!
//! Along the top is the row of Coves (see `coves`): click one to look into
//! it, + for another, × to remove one (its windows go to its neighbour);
//! drag a picture onto a Cove to move the window there (onto + for a new
//! Cove of its own). Ctrl+Super+Left/Right change Cove here too, with Shift
//! taking the chosen window along.
//!
//! Everything moves on interruptible springs: opening again while it
//! closes turns it around mid-way. With Reduce Motion it appears and goes
//! at once. While it rests, only what changes is painted again: a picture
//! that the pointer moves over, or one whose window drew something new
//! (refreshed at most every 66 ms).

use super::*;
use crate::gui::canvas::{blend, with_alpha};
use crate::gui::icons;
use aurora_image::Image;
use lumen::lagoon as layout;
use lumen::spring::Spring;

/// Opening and closing: brisk, landing with the faintest overshoot.
const OPENING: (u32, u32) = (440, 930);
/// Pictures making room for one another.
const SETTLING: (u32, u32) = (340, 1000);
const GAP: i32 = 28;
/// Room under a picture for its name.
const LABEL: i32 = 38;
/// The row of minimised windows, above Harbor.
const SHELF_H: i32 = 104;
/// The Coves' pictures: at most this wide, and the room under them.
const CARD_W: i32 = 132;
const CARD_LABEL: i32 = 26;
const CARD_GAP: i32 = 16;
const STRIP_TOP: i32 = MENUBAR_H + 10;
/// Window pictures for the Coves' row are made at 1/MINI of their size.
const MINI: u32 = 6;
/// A live picture is made again at most this often.
const LIVE_MS: u64 = 66;
const DRAG_START: i32 = 6;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Hit {
    Tile(u32),
    Close(u32),
    Cove(usize),
    RemoveCove(usize),
    AddCove,
}

struct Tile {
    id: u32,
    /// Where it comes from and goes back to: the window, or its Harbor icon.
    home: Rect,
    minimized: bool,
    /// It matches what was typed.
    shown: bool,
    /// The whole window (device pixels), and a copy the size of its place.
    picture: Image,
    thumb: Option<Image>,
    slot: Rect,
    x: Spring,
    y: Spring,
    w: Spring,
    h: Spring,
    /// Thousandths.
    alpha: Spring,
    /// The window drew something since its picture was made (when).
    stale: Option<u64>,
    /// Not placed yet.
    fresh: bool,
}

impl Tile {
    fn place(&self) -> Rect {
        Rect::new(self.x.value(), self.y.value(), self.w.value().max(1), self.h.value().max(1))
    }

    fn moving(&mut self, now: u64) -> bool {
        // (All four, not stopping at the first that moves.)
        [&mut self.x, &mut self.y, &mut self.w, &mut self.h, &mut self.alpha]
            .into_iter()
            .fold(false, |m, s| s.advance(now) | m)
    }

    fn aim(&mut self, r: Rect, alpha: i32, now: u64, snap: bool) {
        for (s, v) in
            [(&mut self.x, r.x), (&mut self.y, r.y), (&mut self.w, r.w), (&mut self.h, r.h), (&mut self.alpha, alpha)]
        {
            if snap {
                s.snap(v, now);
            } else {
                s.set_target(v, now);
            }
        }
    }
}

struct Press {
    at: (i32, i32),
    hit: Hit,
    dragging: bool,
}

pub(super) struct Lagoon {
    open: Spring,
    closing: bool,
    tiles: Vec<Tile>,
    selected: Option<u32>,
    hover: Option<Hit>,
    query: String,
    press: Option<Press>,
    /// Small pictures of every window, for the Coves' row.
    minis: Vec<(u32, Image)>,
    /// The main display's wallpaper, the size of a Cove's picture.
    wallpaper: Option<Image>,
    last_live: u64,
}

fn matches(title: &str, app: &str, query: &str) -> bool {
    let q = query.to_lowercase();
    title.to_lowercase().contains(&q) || app.to_lowercase().contains(&q)
}

/// `r` scaled by `permille` about its centre.
fn shrunk(r: Rect, permille: i32) -> Rect {
    let (w, h) = (r.w * permille / 1000, r.h * permille / 1000);
    Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w.max(1), h.max(1))
}

impl Desktop {
    pub(super) fn lagoon_open(&self) -> bool {
        self.lagoon.as_ref().is_some_and(|l| !l.closing)
    }

    /// Lagoon is on screen (opening, open or closing): it paints instead of the windows.
    pub(super) fn lagoon_shows(&self) -> bool {
        self.lagoon.is_some()
    }

    pub(super) fn lagoon_moving(&self) -> bool {
        self.lagoon.as_ref().is_some_and(|l| {
            // (Closing, it is moving until it is gone.)
            l.closing
                || !l.open.settled()
                || l.tiles
                    .iter()
                    .any(|t| !(t.x.settled() && t.y.settled() && t.w.settled() && t.h.settled() && t.alpha.settled()))
        })
    }

    /// When a live picture is next due.
    pub(super) fn lagoon_deadline(&self) -> u64 {
        match &self.lagoon {
            Some(l) if l.tiles.iter().any(|t| t.stale.is_some()) => l.last_live + LIVE_MS,
            _ => u64::MAX,
        }
    }

    /// Super+Tab, F3: opens Lagoon, or closes it (back to the window in front).
    pub(super) fn toggle_lagoon(&mut self) {
        if self.lagoon_open() {
            self.close_lagoon(None);
        } else {
            self.open_lagoon();
        }
    }

    pub(super) fn open_lagoon(&mut self) {
        if self.shore.is_some() || self.play_covers() || self.power.is_some() {
            return;
        }
        let now = self.now_ms;
        if let Some(l) = self.lagoon.as_mut() {
            // Closing: turn around.
            l.closing = false;
            l.open.set_target(1000, now);
            if theme::reduce_motion() {
                l.open.snap(1000, now);
            }
            self.damage_all();
            return;
        }
        // Nothing else stays open over it.
        self.launcher = None;
        self.close_menu();
        self.center = None;
        self.spotlight = None;
        self.current = None;
        if self.switcher.is_some() {
            self.finish_switcher(false);
        }
        self.harbor.peek = None;
        self.drag = None;
        self.snap_preview = None;
        self.moorings_open = false;
        self.grab = None;
        self.super_alone = false;
        self.reveal = None;
        // Running animations end where they were going.
        self.ghosts.clear();
        for w in &mut self.windows {
            w.animating = w.awaiting_frame.is_some();
        }
        self.coves.end_slide();
        let selected = self.focused_id();
        self.lagoon = Some(Lagoon {
            open: Spring::new(0, OPENING),
            closing: false,
            tiles: Vec::new(),
            selected,
            hover: None,
            query: String::new(),
            press: None,
            minis: Vec::new(),
            wallpaper: None,
            last_live: now,
        });
        self.lagoon_tiles(false);
        self.lagoon_minis();
        let k = crate::gui::scale() as u32;
        let (cw, ch) = self.card_size();
        let wp = Image {
            width: (self.w * k as i32) as u32,
            height: (self.h * k as i32) as u32,
            pixels: self.wallpaper.clone(),
        };
        let mini =
            (wp.pixels.len() == (wp.width * wp.height) as usize).then(|| wp.thumbnail(cw as u32 * k, ch as u32 * k));
        let l = self.lagoon.as_mut().unwrap();
        l.wallpaper = mini;
        if l.selected.is_none_or(|id| !l.tiles.iter().any(|t| t.id == id)) {
            l.selected = l.tiles.first().map(|t| t.id);
        }
        l.open.set_target(1000, now);
        if theme::reduce_motion() {
            l.open.snap(1000, now);
        }
        self.update_harbor();
        self.damage_all();
        log!(
            "gui",
            "lagoon: {} windows, cove {} of {}",
            self.lagoon.as_ref().map_or(0, |l| l.tiles.len()),
            self.cove_index() + 1,
            self.coves.ids.len()
        );
    }

    /// Closes Lagoon: the windows glide back (with `activate`, that one comes forward first).
    pub(super) fn close_lagoon(&mut self, activate: Option<u32>) {
        if !self.lagoon_open() {
            return;
        }
        if let Some(id) = activate {
            self.raise(id);
        }
        // Back to where each window is now, in their order on screen.
        let homes: Vec<(u32, Rect, bool)> = self
            .windows
            .iter()
            .map(|w| (w.id, if w.minimized { self.dock_icon_of(w.id) } else { w.rect }, w.minimized))
            .collect();
        let now = self.now_ms;
        let l = self.lagoon.as_mut().unwrap();
        l.tiles.retain(|t| homes.iter().any(|h| h.0 == t.id));
        l.tiles.sort_by_key(|t| homes.iter().position(|h| h.0 == t.id));
        for t in &mut l.tiles {
            if let Some(&(_, home, min)) = homes.iter().find(|h| h.0 == t.id) {
                t.home = home;
                t.minimized = min;
            }
            // Pictures hidden by a search come back as they go.
            if !t.shown {
                t.shown = true;
                let slot = t.slot;
                t.aim(slot, 1000, now, false);
            }
        }
        l.closing = true;
        l.press = None;
        l.open.set_target(0, now);
        if theme::reduce_motion() {
            self.lagoon = None;
        }
        self.update_harbor();
        self.damage_all();
    }

    /// Makes the pictures of the Cove in front (`fade`: new ones grow in
    /// where they go, as when looking into another Cove).
    fn lagoon_tiles(&mut self, fade: bool) {
        let Some(l) = self.lagoon.as_ref() else { return };
        let keep: Vec<u32> = l.tiles.iter().map(|t| t.id).collect();
        let ids: Vec<(u32, bool, Rect)> = self
            .windows
            .iter()
            .filter(|w| !w.elsewhere && w.awaiting_frame.is_none())
            .map(|w| (w.id, w.minimized, w.rect))
            .collect();
        let mut tiles = Vec::new();
        let mut old = core::mem::take(&mut self.lagoon.as_mut().unwrap().tiles);
        let now = self.now_ms;
        for (id, minimized, rect) in ids {
            if let Some(at) = old.iter().position(|t| t.id == id).filter(|_| keep.contains(&id)) {
                tiles.push(old.swap_remove(at));
                continue;
            }
            let Some((px, w, h)) = self.snapshot(id) else { continue };
            let home = if minimized { self.dock_icon_of(id) } else { rect };
            let mut t = Tile {
                id,
                home,
                minimized,
                shown: true,
                picture: Image { width: w, height: h, pixels: px },
                thumb: None,
                slot: home,
                x: Spring::new(home.x, SETTLING),
                y: Spring::new(home.y, SETTLING),
                w: Spring::new(home.w, SETTLING),
                h: Spring::new(home.h, SETTLING),
                alpha: Spring::new(1000, SETTLING),
                stale: None,
                fresh: true,
            };
            t.alpha.snap(if fade { 0 } else { 1000 }, now);
            tiles.push(t);
        }
        self.lagoon.as_mut().unwrap().tiles = tiles;
        self.lagoon_layout(true, fade);
    }

    /// Small pictures of every window for the Coves' row.
    fn lagoon_minis(&mut self) {
        let ids: Vec<u32> =
            self.windows.iter().filter(|w| !w.minimized && w.awaiting_frame.is_none()).map(|w| w.id).collect();
        let mut minis = Vec::new();
        for id in ids {
            let tile =
                self.lagoon.as_ref().and_then(|l| l.tiles.iter().find(|t| t.id == id)).map(|t| t.picture.clone());
            let picture = match tile {
                Some(p) => p,
                None => match self.snapshot(id) {
                    Some((px, w, h)) => Image { width: w, height: h, pixels: px },
                    None => continue,
                },
            };
            minis.push((id, picture.thumbnail((picture.width / MINI).max(1), (picture.height / MINI).max(1))));
        }
        if let Some(l) = self.lagoon.as_mut() {
            l.minis = minis;
        }
    }

    /// Where every picture goes: per display, the windows in reading order
    /// spread over it (on the main display between the Coves' row and
    /// Harbor), minimised ones on a shelf above Harbor. `snap` puts new
    /// pictures there at once; `fade` grows them in.
    fn lagoon_layout(&mut self, snap_new: bool, fade: bool) {
        let rects = self.display_rects();
        let main = self.main_index().min(rects.len() - 1);
        let strip_bottom = STRIP_TOP + self.card_size().1 + CARD_LABEL;
        let harbor_top = self.harbor_top();
        let now = self.now_ms;
        let reduce = theme::reduce_motion();
        let Some(l) = self.lagoon.as_ref() else { return };
        let query = l.query.clone();
        let shown: Vec<bool> = l
            .tiles
            .iter()
            .map(|t| {
                query.is_empty()
                    || self.index_of(t.id).is_some_and(|i| {
                        let w = &self.windows[i];
                        matches(&w.app.title(), apps::info(w.app.kind()).name, &query)
                    })
            })
            .collect();
        let shelf = l.tiles.iter().zip(&shown).any(|(t, s)| t.minimized && *s);
        let mut slots: Vec<Option<Rect>> = alloc::vec![None; l.tiles.len()];
        for (d, display) in rects.iter().enumerate() {
            let area = if d == main {
                let bottom = harbor_top - 18 - if shelf { SHELF_H + 14 } else { 0 };
                Rect::new(display.x + 48, strip_bottom + 22, display.w - 96, bottom - strip_bottom - 22)
            } else {
                display.inset(48)
            };
            let members: Vec<usize> = (0..l.tiles.len())
                .filter(|&i| shown[i] && !l.tiles[i].minimized && lumen::displays::home(&rects, l.tiles[i].home) == d)
                .collect();
            let homes: Vec<Rect> = members.iter().map(|&i| l.tiles[i].home).collect();
            let order = layout::reading_order(&homes, (area.h / 3).max(1));
            let sizes: Vec<(i32, i32)> = order.iter().map(|&o| (homes[o].w, homes[o].h)).collect();
            for (n, r) in layout::spread(&sizes, area, GAP, LABEL).into_iter().enumerate() {
                slots[members[order[n]]] = Some(r);
            }
        }
        // The shelf: minimised windows, small, in one row.
        let display = rects[main];
        let shelf_area = Rect::new(display.x + 96, harbor_top - 18 - SHELF_H, display.w - 192, SHELF_H);
        let mins: Vec<usize> = (0..l.tiles.len()).filter(|&i| shown[i] && l.tiles[i].minimized).collect();
        let sizes: Vec<(i32, i32)> = mins
            .iter()
            .map(|&i| {
                let p = &l.tiles[i].picture;
                let k = crate::gui::scale() as u32;
                ((p.width / k) as i32, (p.height / k) as i32)
            })
            .collect();
        for (n, r) in layout::spread(&sizes, shelf_area, GAP / 2, LABEL - 6).into_iter().enumerate() {
            slots[mins[n]] = Some(r);
        }
        let k = crate::gui::scale() as u32;
        let l = self.lagoon.as_mut().unwrap();
        for (i, t) in l.tiles.iter_mut().enumerate() {
            let new = core::mem::take(&mut t.fresh);
            t.shown = shown[i];
            match slots[i] {
                Some(slot) => {
                    t.slot = slot;
                    if fade && new {
                        t.aim(shrunk(slot, 920), 0, now, true);
                    }
                    t.aim(slot, 1000, now, reduce || (snap_new && new && !fade));
                    // A copy the size of its place, made once it has one.
                    let (tw, th) = (slot.w as u32 * k, slot.h as u32 * k);
                    if t.thumb.as_ref().is_none_or(|th0| th0.width.abs_diff(tw) > 1 || th0.height.abs_diff(th) > 1) {
                        t.thumb = Some(t.picture.thumbnail(tw.max(1), th.max(1)));
                    }
                }
                // Not what was typed: it sinks away where it was.
                None => t.aim(shrunk(t.slot, 880), 0, now, reduce),
            }
        }
        if l.selected.is_none_or(|s| !l.tiles.iter().any(|t| t.id == s && t.shown)) {
            l.selected = layout_first(l);
        }
        self.damage_all();
    }

    /// Size of a Cove's picture (the main display's shape), fitted to how many there are.
    fn card_size(&self) -> (i32, i32) {
        let n = self.coves.ids.len() as i32 + 1;
        let w = CARD_W.min((self.w - 96) / n - CARD_GAP).max(48);
        (w, w * self.h / self.w.max(1))
    }

    /// Each Cove's picture, then the + (all on the main display, along the top).
    fn cove_cards(&self) -> Vec<Rect> {
        let (w, h) = self.card_size();
        let n = self.coves.ids.len() as i32 + 1;
        let total = n * (w + CARD_GAP) - CARD_GAP;
        let x0 = (self.w - total) / 2;
        // It slides down from under the bar as Lagoon opens.
        let p = self.lagoon.as_ref().map_or(1000, |l| l.open.value().clamp(0, 1000));
        let y = STRIP_TOP - (1000 - p) * (h + CARD_LABEL + STRIP_TOP) / 1000;
        (0..n).map(|i| Rect::new(x0 + i * (w + CARD_GAP), y, w, h)).collect()
    }

    fn card_remove_button(card: Rect) -> Rect {
        Rect::new(card.right() - 12, card.y - 8, 20, 20)
    }

    fn close_button(r: Rect) -> Rect {
        Rect::new(r.right() - 30, r.y + 6, 24, 24)
    }

    /// The picture rectangle of a tile right now (between its window and its place).
    fn tile_rect(&self, t: &Tile) -> Rect {
        let p = self.lagoon.as_ref().map_or(1000, |l| l.open.value());
        layout::lerp_rect(t.home, t.place(), p)
    }

    /// What is under (`x`, `y`).
    fn lagoon_hit(&self, x: i32, y: i32) -> Option<Hit> {
        let l = self.lagoon.as_ref()?;
        if self.on_main(x, y) {
            let cards = self.cove_cards();
            let n = self.coves.ids.len();
            for (i, c) in cards.iter().enumerate() {
                // The × shows on the Cove pointed at (while there is more than one).
                let armed = i < n && n > 1 && matches!(l.hover, Some(Hit::Cove(h) | Hit::RemoveCove(h)) if h == i);
                if armed && Self::card_remove_button(*c).inset(-3).contains(x, y) {
                    return Some(Hit::RemoveCove(i));
                }
                if c.inset(-4).union(&Rect::new(c.x, c.bottom(), c.w, CARD_LABEL)).contains(x, y) {
                    return Some(if i == n { Hit::AddCove } else { Hit::Cove(i) });
                }
            }
        }
        let dragging = l.press.as_ref().is_some_and(|p| p.dragging);
        for t in l.tiles.iter().rev() {
            if !t.shown || dragging && matches!(l.press.as_ref().map(|p| p.hit), Some(Hit::Tile(id)) if id == t.id) {
                continue;
            }
            let r = self.tile_rect(t);
            if r.contains(x, y) {
                if Self::close_button(r).contains(x, y) && !dragging {
                    return Some(Hit::Close(t.id));
                }
                return Some(Hit::Tile(t.id));
            }
            // The name under the picture counts too.
            if Rect::new(r.x, r.bottom(), r.w, LABEL - 6).contains(x, y) {
                return Some(Hit::Tile(t.id));
            }
        }
        None
    }

    /// Everything a tile paints (picture, ring, name, shadow).
    fn tile_area(&self, id: u32) -> Rect {
        let Some(t) = self.lagoon.as_ref().and_then(|l| l.tiles.iter().find(|t| t.id == id)) else {
            return Rect::default();
        };
        let r = self.tile_rect(t);
        let label = self.tile_label(r, id);
        r.inset(-14).union(&label.inset(-6)).union(&Rect::new(r.x - 20, r.y - 10, r.w + 40, r.h + 40))
    }

    fn damage_hit(&mut self, h: Option<Hit>) {
        match h {
            Some(Hit::Tile(id) | Hit::Close(id)) => {
                let a = self.tile_area(id);
                self.damage(a);
            }
            Some(Hit::Cove(i) | Hit::RemoveCove(i)) => {
                if let Some(c) = self.cove_cards().get(i) {
                    self.damage(c.inset(-14).union(&Rect::new(c.x - 14, c.bottom(), c.w + 28, CARD_LABEL + 8)));
                }
            }
            Some(Hit::AddCove) => {
                if let Some(c) = self.cove_cards().last() {
                    self.damage(c.inset(-14).union(&Rect::new(c.x - 14, c.bottom(), c.w + 28, CARD_LABEL + 8)));
                }
            }
            None => {}
        }
    }

    fn set_lagoon_selected(&mut self, id: Option<u32>) {
        let Some(l) = self.lagoon.as_mut() else { return };
        if l.selected == id || id.is_none() {
            return;
        }
        let old = core::mem::replace(&mut l.selected, id);
        if let Some(o) = old {
            self.damage_hit(Some(Hit::Tile(o)));
        }
        self.damage_hit(id.map(Hit::Tile));
    }

    // ------------------------------------------------------------- input

    /// Pointer input while Lagoon is open: true if it took it.
    pub(super) fn lagoon_pointer(&mut self, x: i32, y: i32, moved: bool, pressed: bool, released: bool) -> bool {
        if !self.lagoon_open() {
            return false;
        }
        if moved {
            self.bar_hover(x, y);
            self.harbor_hover(x, y);
            self.lagoon_hover(x, y);
        }
        if pressed {
            let hit = self.lagoon_hit(x, y);
            match hit {
                Some(Hit::Tile(_)) => {
                    if let Some(l) = self.lagoon.as_mut() {
                        l.press = Some(Press { at: (x, y), hit: hit.unwrap(), dragging: false });
                    }
                }
                Some(Hit::Close(id)) => self.request_close(id),
                Some(Hit::Cove(i)) => {
                    // Pressing starts a drag of nothing; looking into a Cove happens here.
                    self.switch_cove(i, None);
                }
                Some(Hit::RemoveCove(i)) => self.remove_cove(i),
                Some(Hit::AddCove) => {
                    self.add_cove();
                }
                None => {
                    // The bar and Harbor work as ever (Lagoon gives way).
                    let (dock, _) = self.dock_layout();
                    let through = self.on_main(x, y) && (y < MENUBAR_H || dock.inset(-4).contains(x, y));
                    self.close_lagoon(None);
                    return !through;
                }
            }
        }
        if released {
            let press = self.lagoon.as_mut().and_then(|l| l.press.take());
            if let Some(p) = press {
                match (p.hit, p.dragging) {
                    (Hit::Tile(id), true) => self.lagoon_drop(id, x, y),
                    (Hit::Tile(id), false) => self.close_lagoon(Some(id)),
                    _ => {}
                }
            }
        }
        true
    }

    fn lagoon_hover(&mut self, x: i32, y: i32) {
        let Some(l) = self.lagoon.as_mut() else { return };
        if let Some(p) = l.press.as_mut() {
            if !p.dragging && (x - p.at.0).abs().max((y - p.at.1).abs()) >= DRAG_START {
                p.dragging = true;
            }
            if p.dragging {
                // The picture follows the pointer; the Coves light up under it.
                self.damage_all();
                let hover = self.lagoon_hit(x, y).filter(|h| matches!(h, Hit::Cove(_) | Hit::AddCove));
                if let Some(l) = self.lagoon.as_mut() {
                    l.hover = hover;
                }
                return;
            }
        }
        let hit = self.lagoon_hit(x, y);
        let Some(l) = self.lagoon.as_mut() else { return };
        if hit != l.hover {
            let old = core::mem::replace(&mut l.hover, hit);
            self.damage_hit(old);
            self.damage_hit(hit);
            if let Some(Hit::Tile(id) | Hit::Close(id)) = hit {
                self.set_lagoon_selected(Some(id));
            }
        }
    }

    /// What the pointer is on once things stop moving under it (without
    /// changing which window is chosen).
    fn lagoon_rehover(&mut self) {
        if self.lagoon_dragging() {
            return;
        }
        let (x, y) = self.cursor;
        let hit = self.lagoon_hit(x, y);
        let Some(l) = self.lagoon.as_mut() else { return };
        if hit != l.hover {
            let old = core::mem::replace(&mut l.hover, hit);
            self.damage_hit(old);
            self.damage_hit(hit);
        }
    }

    /// A picture dragged and let go at (`x`, `y`): onto a Cove, that window moves there.
    fn lagoon_drop(&mut self, id: u32, x: i32, y: i32) {
        let target = match self.lagoon_hit(x, y) {
            Some(Hit::Cove(i)) => Some(i),
            Some(Hit::AddCove) => self.add_cove(),
            _ => None,
        };
        if let Some(l) = self.lagoon.as_mut() {
            l.hover = None;
        }
        if let Some(i) = target.filter(|&i| i != self.cove_index()) {
            self.move_to_cove(id, i);
            if let Some(l) = self.lagoon.as_mut() {
                l.tiles.retain(|t| t.id != id);
            }
            self.lagoon_layout(false, false);
        }
        self.damage_all();
    }

    /// Keys while Lagoon is open: true if it took the key.
    pub(super) fn lagoon_key(&mut self, k: &KeyEvent) -> bool {
        if !self.lagoon_open() || k.code == KeyCode::Super {
            return false;
        }
        if !k.pressed {
            return true;
        }
        let ctrl_super = k.mods.ctrl && k.mods.super_key;
        match k.code {
            KeyCode::Left | KeyCode::Right if ctrl_super => {
                let step = if k.code == KeyCode::Left { -1 } else { 1 };
                let carry = if k.mods.shift { self.lagoon.as_ref().and_then(|l| l.selected) } else { None };
                let at = self.cove_index() as i32 + step;
                if at >= 0 && (at as usize) < self.coves.ids.len() {
                    match carry {
                        // The chosen window goes alone; Lagoon stays here.
                        Some(id) => {
                            self.move_to_cove(id, at as usize);
                            if let Some(l) = self.lagoon.as_mut() {
                                l.tiles.retain(|t| t.id != id);
                            }
                            self.lagoon_layout(false, false);
                            self.switch_cove(at as usize, None);
                            self.set_lagoon_selected(Some(id));
                        }
                        None => self.switch_cove(at as usize, None),
                    }
                }
            }
            KeyCode::Tab if k.mods.super_key => self.close_lagoon(None),
            KeyCode::F3 => self.close_lagoon(None),
            KeyCode::Escape => {
                if self.lagoon.as_ref().is_some_and(|l| !l.query.is_empty()) {
                    self.lagoon.as_mut().unwrap().query.clear();
                    self.lagoon_layout(false, false);
                } else {
                    self.close_lagoon(None);
                }
            }
            KeyCode::Enter => {
                let id = self
                    .lagoon
                    .as_ref()
                    .and_then(|l| l.selected.filter(|s| l.tiles.iter().any(|t| t.id == *s && t.shown)));
                if id.is_some() {
                    self.close_lagoon(id);
                }
            }
            KeyCode::Tab => self.lagoon_step(if k.mods.shift { -1 } else { 1 }),
            KeyCode::Left => self.lagoon_arrow((-1, 0)),
            KeyCode::Right => self.lagoon_arrow((1, 0)),
            KeyCode::Up => self.lagoon_arrow((0, -1)),
            KeyCode::Down => self.lagoon_arrow((0, 1)),
            KeyCode::Delete => self.lagoon_close_selected(),
            _ if k.mods.ctrl && matches!(k.ch, Some('w' | 'W')) => self.lagoon_close_selected(),
            KeyCode::Backspace => {
                if let Some(l) = self.lagoon.as_mut() {
                    if l.query.pop().is_some() {
                        self.lagoon_layout(false, false);
                    }
                }
            }
            _ => {
                if let Some(c) = k.ch.filter(|c| !c.is_control() && !k.mods.ctrl && !k.mods.super_key && !k.mods.alt) {
                    if let Some(l) = self.lagoon.as_mut() {
                        if l.query.chars().count() < 32 && (c != ' ' || !l.query.is_empty()) {
                            l.query.push(c);
                            self.lagoon_layout(false, false);
                        }
                    }
                }
            }
        }
        true
    }

    fn lagoon_close_selected(&mut self) {
        if let Some(id) = self.lagoon.as_ref().and_then(|l| l.selected) {
            self.request_close(id);
        }
    }

    /// Shown pictures in reading order (window ids), as they sit in their places.
    fn lagoon_order(&self) -> Vec<(u32, Rect)> {
        let Some(l) = self.lagoon.as_ref() else { return Vec::new() };
        let shown: Vec<(u32, Rect)> = l.tiles.iter().filter(|t| t.shown).map(|t| (t.id, t.slot)).collect();
        let rects: Vec<Rect> = shown.iter().map(|s| s.1).collect();
        layout::reading_order(&rects, 24).into_iter().map(|i| shown[i]).collect()
    }

    fn lagoon_step(&mut self, step: i32) {
        let order = self.lagoon_order();
        if order.is_empty() {
            return;
        }
        let at = self.lagoon.as_ref().and_then(|l| l.selected).and_then(|s| order.iter().position(|o| o.0 == s));
        let n = order.len() as i32;
        let next = match at {
            Some(i) => (i as i32 + step).rem_euclid(n),
            None => 0,
        };
        self.set_lagoon_selected(Some(order[next as usize].0));
    }

    fn lagoon_arrow(&mut self, dir: (i32, i32)) {
        let order = self.lagoon_order();
        let rects: Vec<Rect> = order.iter().map(|o| o.1).collect();
        let at = self.lagoon.as_ref().and_then(|l| l.selected).and_then(|s| order.iter().position(|o| o.0 == s));
        let next = match at {
            Some(i) => layout::neighbour(&rects, i, dir),
            None => (!order.is_empty()).then_some(0),
        };
        if let Some(n) = next {
            self.set_lagoon_selected(Some(order[n].0));
        }
    }

    pub(super) fn lagoon_dragging(&self) -> bool {
        self.lagoon.as_ref().is_some_and(|l| l.press.as_ref().is_some_and(|p| p.dragging))
    }

    /// The pointer's shape over Lagoon.
    pub(super) fn lagoon_cursor(&self, x: i32, y: i32) -> Option<u32> {
        use aurora_abi::cursor as c;
        if !self.lagoon_open() {
            return None;
        }
        if self.lagoon_dragging() {
            return Some(c::GRABBING);
        }
        Some(match self.lagoon_hit(x, y) {
            Some(_) => c::POINTER,
            None => c::ARROW,
        })
    }

    /// Three fingers swept up or down on a touchpad: Lagoon opens or closes.
    pub(super) fn swipe(&mut self, up: bool) {
        if self.shore.is_some() || self.play_covers() {
            return;
        }
        if up {
            self.open_lagoon();
        } else {
            self.close_lagoon(None);
        }
    }

    // ------------------------------------------------------- the rest of the desktop

    /// Window `id` drew something: its picture is made again soon.
    pub(super) fn lagoon_presented(&mut self, id: u32) -> bool {
        let now = self.now_ms;
        let Some(t) = self.lagoon.as_mut().and_then(|l| l.tiles.iter_mut().find(|t| t.id == id)) else {
            return self.lagoon.is_some();
        };
        t.stale.get_or_insert(now);
        true
    }

    /// Window `id` is going away.
    pub(super) fn lagoon_forget(&mut self, id: u32) {
        let Some(l) = self.lagoon.as_mut() else { return };
        l.minis.retain(|m| m.0 != id);
        if l.tiles.iter().any(|t| t.id == id) {
            l.tiles.retain(|t| t.id != id);
            if l.closing {
                self.damage_all();
            } else {
                self.lagoon_layout(false, false);
            }
        }
    }

    /// Another Cove came forward while Lagoon is open: its windows grow in.
    pub(super) fn lagoon_cove_changed(&mut self) {
        let Some(l) = self.lagoon.as_mut() else { return };
        if l.closing {
            // (Raising a window of another Cove while closing: back to the start.)
            self.lagoon = None;
            return;
        }
        l.tiles.clear();
        l.query.clear();
        l.selected = None;
        l.hover = None;
        self.lagoon_tiles(true);
        let focused = self.focused_id();
        if let Some(l) = self.lagoon.as_mut() {
            if focused.is_some_and(|f| l.tiles.iter().any(|t| t.id == f)) {
                l.selected = focused;
            }
        }
    }

    /// The number of Coves changed: the row is laid out again.
    pub(super) fn lagoon_coves_changed(&mut self) {
        if self.lagoon.is_some() {
            self.damage_all();
        }
    }

    /// Advances Lagoon's springs and refreshes a live picture; true while moving.
    pub(super) fn step_lagoon(&mut self) -> bool {
        if self.lagoon.is_none() {
            return false;
        }
        if self.shore.is_some() || self.play_covers() || self.power.is_some() {
            self.lagoon = None;
            self.damage_all();
            return false;
        }
        let now = self.now_ms;
        let Some(l) = self.lagoon.as_mut() else { return false };
        let mut moving = l.open.advance(now);
        for t in &mut l.tiles {
            moving |= t.moving(now);
        }
        if l.closing && l.open.settled() {
            self.lagoon = None;
            self.update_harbor();
            self.damage_all();
            return false;
        }
        // One live picture per step, the one waiting longest.
        let due = now >= l.last_live + LIVE_MS;
        let next = l.tiles.iter().filter(|t| t.stale.is_some()).min_by_key(|t| t.stale).map(|t| t.id);
        if due && next.is_some() && !moving {
            l.last_live = now;
        }
        if moving {
            self.damage_all();
        } else {
            self.lagoon_rehover();
        }
        if let (true, Some(id), false) = (due, next, moving) {
            if let Some((px, w, h)) = self.snapshot(id) {
                let area = self.tile_area(id);
                if let Some(t) = self.lagoon.as_mut().and_then(|l| l.tiles.iter_mut().find(|t| t.id == id)) {
                    t.stale = None;
                    t.picture = Image { width: w, height: h, pixels: px };
                    let th = t.thumb.as_ref().map(|i| (i.width, i.height));
                    t.thumb = th.map(|(tw, th)| t.picture.thumbnail(tw, th));
                }
                self.damage(area);
            }
        }
        moving
    }

    // ------------------------------------------------------------ painting

    /// The water under Lagoon: the wallpaper (and desktop items) blurring and dimming as it opens.
    pub(super) fn paint_lagoon_backdrop(&mut self, cv: &mut Canvas) {
        let p = self.lagoon.as_ref().map_or(0, |l| l.open.value().clamp(0, 1000)) as u32;
        cv.blit_same(&self.wallpaper, cv.clip);
        if p < 1000 {
            self.paint_items(cv);
        }
        if self.blurred.len() == cv.buf.len() {
            let a = p * 255 / 1000;
            let clip = cv.pclip();
            for y in clip.y..clip.bottom() {
                let row = (y * cv.width) as usize;
                for i in row + clip.x as usize..row + clip.right() as usize {
                    cv.buf[i] = blend(cv.buf[i], self.blurred[i], a);
                }
            }
        }
        let t = theme::current();
        let veil = if t.dark { 0x000A_0E1E } else { 0x00E8_ECF4 };
        let most = if t.dark { 0x88 } else { 0x60 };
        cv.fill_rect(cv.clip, with_alpha(veil, (most * p / 1000) as u8));
    }

    /// The name under a picture: (its pill).
    fn tile_label(&self, r: Rect, id: u32) -> Rect {
        let title = self.index_of(id).map(|i| self.windows[i].app.title()).unwrap_or_default();
        let f = theme::ui_bold(12);
        let w = (f.width(&title) + 20 + 26).min(r.w.max(120) + 40).max(60);
        Rect::new(r.x + r.w / 2 - w / 2, r.bottom() + 10, w, 24)
    }

    /// The pictures, the Coves' row and what was typed.
    pub(super) fn paint_lagoon(&self, cv: &mut Canvas) {
        let Some(l) = &self.lagoon else { return };
        let t = theme::current();
        let p = l.open.value();
        let settled = !self.lagoon_moving();
        let k = cv.scale;
        let dragged = l.press.as_ref().filter(|p| p.dragging).map(|p| p.hit);
        // Minimised windows sit on a shelf of their own.
        let shelf = l
            .tiles
            .iter()
            .filter(|t| t.minimized && t.shown && t.alpha.value() > 0)
            .map(|t| {
                let r = self.tile_rect(t);
                r.union(&self.tile_label(r, t.id))
            })
            .reduce(|a, b| a.union(&b));
        if let Some(r) = shelf.filter(|r| r.inset(-16).intersects(&cv.clip)) {
            let a = (p.clamp(0, 1000) as u32 * if t.dark { 0x14 } else { 0x2A } / 1000) as u8;
            cv.fill_round_rect(r.inset(-14), 22, with_alpha(0xFF_FFFF, a));
        }
        for tile in &l.tiles {
            if dragged == Some(Hit::Tile(tile.id)) {
                continue;
            }
            let a = (tile.alpha.value().clamp(0, 1000) * 255 / 1000) as u32;
            // Minimised windows rise from Harbor, faint at first.
            let a = if tile.minimized { a * (300 + 700 * p.clamp(0, 1000) as u32 / 1000) / 1000 } else { a };
            if a == 0 {
                continue;
            }
            let r = self.tile_rect(tile);
            let area = self.tile_area(tile.id);
            if !area.intersects(&cv.clip) {
                continue;
            }
            let selected = l.selected == Some(tile.id) && !l.closing;
            let shown_p = if l.closing { p } else { p.clamp(0, 1000) };
            // The ring around the chosen one fades in as Lagoon settles.
            if selected && a > 200 {
                let ring = with_alpha(theme::accent(), (shown_p.clamp(0, 1000) * 255 / 1000) as u8);
                cv.fill_round_rect(
                    r.inset(-6),
                    14,
                    with_alpha(theme::accent(), (shown_p.clamp(0, 1000) * 0x30 / 1000) as u8),
                );
                cv.stroke_round_rect(r.inset(-6), 14, ring);
                cv.stroke_round_rect(r.inset(-5), 13, ring);
            }
            let strength = t.shadow * a / 255 * 2 / 3;
            cv.shadow(r, 10, 18, 6, strength);
            let thumb = tile.thumb.as_ref().filter(|th| {
                settled && (th.width as i32 - r.w * k).abs() <= 1 && (th.height as i32 - r.h * k).abs() <= 1
            });
            match thumb {
                Some(th) => cv.draw_image(&th.pixels, th.width, th.height, r, a),
                None => {
                    // While moving: the copy its place size when it is no larger, else the whole picture.
                    let src = tile.thumb.as_ref().filter(|th| r.w * k <= th.width as i32).unwrap_or(&tile.picture);
                    cv.draw_image(&src.pixels, src.width, src.height, r, a);
                }
            }
            // The name and the app, fading in over the last part of the way.
            let la = ((shown_p - 550).clamp(0, 450) * 255 / 450) as u32 * a / 255;
            if la > 0 {
                let label = self.tile_label(r, tile.id);
                let Some(w) = self.index_of(tile.id).map(|i| &self.windows[i]) else { continue };
                let (bg, ink) = if selected {
                    (with_alpha(theme::accent(), la as u8), with_alpha(0xFF_FFFF, la as u8))
                } else {
                    let bg = if t.dark { 0x1D_2632 } else { 0xFF_FFFF };
                    (with_alpha(bg, (la * 0xD8 / 255) as u8), with_alpha(t.text, la as u8))
                };
                cv.fill_round_rect(label, 12, bg);
                if la > 200 {
                    icons::draw(cv, apps::info(w.app.kind()).icon, Rect::new(label.x + 6, label.y + 3, 18, 18));
                }
                cv.text_clipped(label.x + 30, label.y + 17, &w.app.title(), theme::ui_bold(12), ink, label.w - 40);
            }
            // Close, on the picture the pointer is on.
            let hovered = matches!(l.hover, Some(Hit::Tile(id) | Hit::Close(id)) if id == tile.id);
            if hovered && p >= 950 && !l.closing {
                let b = Self::close_button(r);
                let hot = l.hover == Some(Hit::Close(tile.id));
                let (cx, cy) = b.center();
                cv.shadow(b, 12, 8, 2, t.shadow / 3);
                cv.fill_circle(cx, cy, 12, if hot { theme::CLOSE } else { with_alpha(t.window_bg, 0xF0) });
                let ink = if hot { 0xFFFF_FFFF } else { t.text };
                cv.line(cx - 4, cy - 4, cx + 4, cy + 4, 2, ink);
                cv.line(cx + 4, cy - 4, cx - 4, cy + 4, 2, ink);
            }
        }
        self.paint_cove_strip(cv);
        self.paint_lagoon_query(cv);
        // The dragged picture, over everything, smaller, under the pointer.
        if let Some(Hit::Tile(id)) = dragged {
            if let Some(tile) = l.tiles.iter().find(|t| t.id == id) {
                let place = tile.place();
                let scale = 600;
                let (w, h) = (place.w * scale / 1000, place.h * scale / 1000);
                let (x, y) = self.cursor;
                let r = Rect::new(x - w / 2, y - h / 3, w, h);
                cv.shadow(r, 10, 24, 10, t.shadow);
                let src = tile.thumb.as_ref().unwrap_or(&tile.picture);
                cv.draw_image(&src.pixels, src.width, src.height, r, 235);
            }
        }
    }

    /// The Coves' row along the top of the main display.
    fn paint_cove_strip(&self, cv: &mut Canvas) {
        let Some(l) = &self.lagoon else { return };
        let cards = self.cove_cards();
        let Some(first) = cards.first() else { return };
        let band = Rect::new(0, first.y - 14, self.w, first.h + CARD_LABEL + 28);
        if !band.intersects(&cv.clip) || first.bottom() + CARD_LABEL < 0 {
            return;
        }
        let t = theme::current();
        let n = self.coves.ids.len();
        let current = self.cove_index();
        let dragging = l.press.as_ref().is_some_and(|p| p.dragging);
        let rects = self.display_rects();
        let main = rects[self.main_index().min(rects.len() - 1)];
        let k = crate::gui::scale();
        for (i, card) in cards.iter().enumerate() {
            let hovered = l.hover == Some(Hit::Cove(i)) || l.hover == Some(Hit::RemoveCove(i));
            let drop = dragging && hovered;
            let label_y = card.bottom() + 18;
            if i == n {
                // + : a new Cove.
                let add_hover = l.hover == Some(Hit::AddCove);
                let fill = if add_hover { with_alpha(theme::accent(), 0x30) } else { with_alpha(t.text, 0x12) };
                cv.fill_round_rect(*card, 10, fill);
                cv.stroke_round_rect(*card, 10, if add_hover { theme::accent() } else { with_alpha(t.text, 0x38) });
                let (cx, cy) = card.center();
                let ink = if add_hover { theme::accent() } else { with_alpha(t.text, 0xB0) };
                cv.line(cx - 9, cy, cx + 9, cy, 2, ink);
                cv.line(cx, cy - 9, cx, cy + 9, 2, ink);
                let f = theme::ui_bold(12);
                let name = "New Cove";
                cv.text(card.x + (card.w - f.width(name)) / 2, label_y, name, f, t.text_secondary);
                continue;
            }
            cv.shadow(*card, 10, 14, 4, t.shadow / 2);
            // The wallpaper, and the Cove's windows in miniature.
            match &l.wallpaper {
                Some(wp) => cv.draw_image_rounded(&wp.pixels, wp.width, wp.height, *card, *card, 10),
                None => cv.fill_round_rect(*card, 10, with_alpha(t.text, 0x20)),
            }
            let cove = self.coves.ids[i];
            cv.with_clip(card.intersect(&cv.clip), |cv| {
                for w in self.windows.iter().filter(|w| w.cove == cove && !w.minimized) {
                    let Some((_, mini)) = l.minis.iter().find(|m| m.0 == w.id) else { continue };
                    if !main.intersects(&w.rect) {
                        continue;
                    }
                    let r = w.rect;
                    let mr = Rect::new(
                        card.x + (r.x - main.x) * card.w / main.w,
                        card.y + (r.y - main.y) * card.h / main.h,
                        (r.w * card.w / main.w).max(2),
                        (r.h * card.h / main.h).max(2),
                    );
                    cv.shadow(mr, 3, 4, 1, t.shadow / 3);
                    cv.draw_image(&mini.pixels, mini.width, mini.height, mr, 255);
                }
            });
            let _ = k;
            if i == current {
                cv.stroke_round_rect(card.inset(-3), 13, theme::accent());
                cv.stroke_round_rect(card.inset(-2), 12, theme::accent());
            } else if drop {
                cv.fill_round_rect(*card, 10, with_alpha(theme::accent(), 0x40));
                cv.stroke_round_rect(card.inset(-3), 13, theme::accent());
            } else if hovered {
                cv.fill_round_rect(*card, 10, with_alpha(0xFF_FFFF, 0x26));
                cv.stroke_round_rect(card.inset(-2), 12, with_alpha(t.text, 0x60));
            }
            let f = theme::ui_bold(12);
            let name = self.cove_name(i);
            let ink = if i == current { t.text } else { t.text_secondary };
            cv.text(card.x + (card.w - f.width(&name)) / 2, label_y, &name, f, ink);
            if i == current {
                let line = Rect::new(card.x + card.w / 2 - 12, label_y + 6, 24, 3);
                cv.fill_round_rect(line, 2, theme::waterline());
            }
            // × removes a Cove (while there is more than one).
            if hovered && n > 1 && !dragging {
                let b = Self::card_remove_button(*card);
                let hot = l.hover == Some(Hit::RemoveCove(i));
                let (cx, cy) = b.center();
                cv.shadow(b, 10, 6, 2, t.shadow / 3);
                cv.fill_circle(cx, cy, 10, if hot { theme::CLOSE } else { with_alpha(t.window_bg, 0xF4) });
                let ink = if hot { 0xFFFF_FFFF } else { t.text };
                cv.line(cx - 3, cy - 3, cx + 3, cy + 3, 2, ink);
                cv.line(cx + 3, cy - 3, cx - 3, cy + 3, 2, ink);
            }
        }
    }

    /// What was typed (or that nothing is here), under the Coves' row.
    fn paint_lagoon_query(&self, cv: &mut Canvas) {
        let Some(l) = &self.lagoon else { return };
        let t = theme::current();
        let any = l.tiles.iter().any(|t| t.shown);
        let (cw, ch) = self.card_size();
        let _ = cw;
        let top = STRIP_TOP + ch + CARD_LABEL + 6;
        if !l.query.is_empty() {
            let f = theme::ui(14);
            let text = l.query.as_str();
            let w = (f.width(text) + 58).max(160);
            let pill = Rect::new(self.w / 2 - w / 2, top, w, 30);
            if pill.inset(-8).intersects(&cv.clip) {
                cv.fill_round_rect(pill, 15, if t.dark { 0xF01A_2036 } else { 0xF8FF_FFFF });
                cv.stroke_round_rect(pill, 15, with_alpha(theme::accent(), 0x90));
                super::bar::paint_seek(cv, pill.x + 18, pill.y + 15, t.text_secondary);
                let x = pill.x + 34;
                let tw = cv.text_clipped(x, pill.y + 20, text, f, t.text, pill.w - 46);
                cv.fill_rect(Rect::new(x + tw + 1, pill.y + 7, 2, 16), theme::accent());
            }
        }
        if !any && !l.closing && l.open.value() > 600 {
            let msg = if l.query.is_empty() {
                alloc::format!("No windows in {}", self.cove_name(self.cove_index()))
            } else {
                String::from("No windows match")
            };
            let f = theme::ui_bold(16);
            let hint = if l.query.is_empty() {
                "Open an app from Harbor or Shoal, or drag a window here from another Cove"
            } else {
                "Escape clears the search"
            };
            let hf = theme::ui(13);
            let cy = self.h / 2;
            cv.text((self.w - f.width(&msg)) / 2, cy, &msg, f, t.text);
            cv.text((self.w - hf.width(hint)) / 2, cy + 24, hint, hf, t.text_secondary);
        }
    }
}

/// The first shown picture in reading order.
fn layout_first(l: &Lagoon) -> Option<u32> {
    let shown: Vec<&Tile> = l.tiles.iter().filter(|t| t.shown).collect();
    let rects: Vec<Rect> = shown.iter().map(|t| t.slot).collect();
    layout::reading_order(&rects, 24).first().map(|&i| shown[i].id)
}

#[cfg(feature = "ktest")]
impl Desktop {
    /// Lagoon's state for the self-tests: (open, windows shown with their places).
    pub fn lagoon_state(&self) -> Option<(bool, Vec<(u32, Rect)>)> {
        self.lagoon.as_ref().map(|l| (!l.closing, l.tiles.iter().filter(|t| t.shown).map(|t| (t.id, t.slot)).collect()))
    }
}
