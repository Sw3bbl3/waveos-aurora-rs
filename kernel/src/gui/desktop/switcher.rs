//! Glide: Alt+Tab shows the open windows as pictures in a row, most recent
//! first. Tab moves along, releasing Alt brings the chosen one forward.

use super::*;
use crate::gui::canvas::with_alpha;
use crate::gui::icons;

const CARD_W: i32 = 200;
const CARD_H: i32 = 164;
const THUMB_H: i32 = 120;
const VISIBLE: usize = 5;

impl Desktop {
    pub(super) fn touch_mru(&mut self, id: u32) {
        self.mru.retain(|v| *v != id);
        self.mru.insert(0, id);
    }

    pub(super) fn begin_switcher(&mut self, reverse: bool) {
        if let Some(id) = self.focused_id() {
            self.touch_mru(id);
        }
        let ids = self
            .mru
            .iter()
            .copied()
            .filter(|id| self.index_of(*id).is_some_and(|i| self.windows[i].awaiting_frame.is_none()))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return;
        }
        self.launcher = None;
        self.menu = None;
        self.center = None;
        self.spotlight = None;
        self.current = None;
        self.super_alone = false;
        self.glide = ids
            .iter()
            .filter_map(|&id| {
                let (px, w, h) = self.snapshot(id)?;
                let img = aurora_image::Image { width: w, height: h, pixels: px };
                Some((id, img.thumbnail((CARD_W - 20) as u32, THUMB_H as u32)))
            })
            .collect();
        self.switcher = Some(Switcher::new(ids, reverse));
        self.damage_all();
    }

    fn switcher_rect(&self) -> Rect {
        let count = self.switcher.as_ref().map_or(0, |s| s.ids.len()).min(VISIBLE) as i32;
        let w = count * (CARD_W + 12) - 12 + 40;
        let h = CARD_H + 40;
        Rect::new((self.w - w) / 2, (self.h - h) / 2, w, h)
    }

    fn switcher_first(&self) -> usize {
        self.switcher.as_ref().map_or(0, |s| s.selected.saturating_sub(VISIBLE - 1))
    }

    fn switcher_cards(&self) -> Vec<(usize, Rect)> {
        let r = self.switcher_rect();
        let first = self.switcher_first();
        let n = self.switcher.as_ref().map_or(0, |s| s.ids.len());
        (first..n.min(first + VISIBLE))
            .map(|i| (i, Rect::new(r.x + 20 + (i - first) as i32 * (CARD_W + 12), r.y + 20, CARD_W, CARD_H)))
            .collect()
    }

    fn finish_switcher(&mut self, activate: bool) {
        let selected = self.switcher.take().and_then(|s| s.current());
        self.glide.clear();
        if activate {
            if let Some(id) = selected {
                self.raise(id);
            }
        }
        self.damage_all();
    }

    pub(super) fn switcher_key(&mut self, k: &KeyEvent) -> bool {
        if self.switcher.is_none() {
            return false;
        }
        if !k.pressed && k.code == KeyCode::Alt {
            self.finish_switcher(true);
            return true;
        }
        if k.pressed {
            match k.code {
                KeyCode::Escape => self.finish_switcher(false),
                KeyCode::Enter => self.finish_switcher(true),
                KeyCode::Tab | KeyCode::Right | KeyCode::Down => {
                    self.switcher.as_mut().unwrap().step(k.mods.shift);
                    self.damage_all();
                }
                KeyCode::Left | KeyCode::Up => {
                    self.switcher.as_mut().unwrap().step(true);
                    self.damage_all();
                }
                _ => {}
            }
        }
        true
    }

    pub(super) fn switcher_press(&mut self, x: i32, y: i32) -> bool {
        if self.switcher.is_none() {
            return false;
        }
        match self.switcher_cards().into_iter().find(|(_, r)| r.contains(x, y)) {
            Some((i, _)) => {
                if let Some(s) = self.switcher.as_mut() {
                    s.selected = i;
                }
                self.finish_switcher(true);
            }
            None => self.finish_switcher(false),
        }
        true
    }

    pub(super) fn paint_switcher(&self, cv: &mut Canvas) {
        let Some(s) = &self.switcher else { return };
        let r = self.switcher_rect();
        let t = theme::current();
        cv.shadow(r, 28, 34, 12, t.shadow);
        cv.frosted(r, 28, t.panel_tint);
        cv.stroke_round_rect(r, 28, if t.dark { 0x24FF_FFFF } else { 0x80FF_FFFF });
        for (n, card) in self.switcher_cards() {
            let Some(i) = self.index_of(s.ids[n]) else { continue };
            let w = &self.windows[i];
            let selected = n == s.selected;
            if selected {
                cv.fill_round_rect(card, 18, with_alpha(theme::accent(), 0x2A));
                cv.stroke_round_rect(card, 18, theme::accent());
                cv.stroke_round_rect(card.inset(1), 17, theme::accent());
            }
            // The window's picture, letterboxed in the card.
            let well = Rect::new(card.x + 10, card.y + 10, card.w - 20, THUMB_H);
            match self.glide.iter().find(|(id, _)| *id == w.id) {
                Some((_, img)) => {
                    let pic = Rect::new(
                        well.x + (well.w - img.width as i32) / 2,
                        well.y + (well.h - img.height as i32) / 2,
                        img.width as i32,
                        img.height as i32,
                    );
                    cv.shadow(pic, 8, 10, 3, t.shadow / 3);
                    cv.fill_round_rect_with(pic, 8, |x, y| {
                        img.pixels[((y - pic.y) as u32 * img.width + (x - pic.x) as u32) as usize] | 0xFF00_0000
                    });
                    if w.minimized {
                        cv.fill_round_rect(pic, 8, with_alpha(t.window_bg, 0x80));
                    }
                }
                None => cv.fill_round_rect(well, 8, with_alpha(t.text, 0x10)),
            }
            let icon = Rect::new(card.x + 12, card.bottom() - 30, 22, 22);
            icons::draw(cv, apps::info(w.app.kind()).icon, icon);
            let title = w.app.title();
            cv.text_clipped(icon.right() + 8, card.bottom() - 14, &title, theme::ui_bold(13), t.text, card.w - 54);
            if selected {
                let line = Rect::new(card.x + card.w / 2 - 14, card.bottom() + 6, 28, 3);
                cv.fill_round_rect(line, 2, theme::waterline());
            }
        }
    }
}
