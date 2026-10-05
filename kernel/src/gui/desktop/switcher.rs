use super::*;
use crate::gui::icons;
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
        self.quick_open = false;
        self.close_volume_popover();
        self.close_battery_popover();
        self.close_network_popover();
        self.super_alone = false;
        self.switcher = Some(Switcher::new(ids, reverse));
        self.damage_all();
    }
    fn switcher_rect(&self) -> Rect {
        let count = self.switcher.as_ref().map_or(0, |s| s.ids.len()).min(7) as i32;
        let w = 520.min(self.w - 32);
        let h = 64 + count * 56;
        Rect::new((self.w - w) / 2, (self.h - h) / 2, w, h)
    }
    fn switcher_first(&self) -> usize {
        self.switcher.as_ref().map_or(0, |s| s.selected.saturating_sub(6))
    }
    fn finish_switcher(&mut self, activate: bool) {
        let selected = self.switcher.take().and_then(|s| s.current());
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
        let r = self.switcher_rect();
        if r.contains(x, y) && y >= r.y + 48 {
            let i = self.switcher_first() + ((y - r.y - 48) / 56) as usize;
            if let Some(s) = self.switcher.as_mut() {
                if i < s.ids.len() {
                    s.selected = i;
                    self.finish_switcher(true);
                }
            }
        } else {
            self.finish_switcher(false);
        }
        true
    }
    pub(super) fn paint_switcher(&self, cv: &mut Canvas) {
        let Some(s) = &self.switcher else { return };
        let r = self.switcher_rect();
        let t = theme::current();
        cv.shadow(r, 20, 28, 10, t.shadow);
        cv.frosted(r, 20, t.panel_tint);
        cv.text(r.x + 20, r.y + 30, "Switch windows", theme::ui_bold(16), t.text);
        let first = self.switcher_first();
        for (n, id) in s.ids.iter().enumerate().skip(first).take(7) {
            let Some(i) = self.index_of(*id) else { continue };
            let w = &self.windows[i];
            let row = Rect::new(r.x + 10, r.y + 48 + (n - first) as i32 * 56, r.w - 20, 50);
            if n == s.selected {
                cv.fill_round_rect(row, 10, t.window_bg_alt);
                cv.stroke_round_rect(row, 10, theme::accent());
            }
            icons::draw(cv, apps::info(w.app.kind()).icon, Rect::new(row.x + 10, row.y + 8, 32, 32));
            cv.text_clipped(row.x + 54, row.y + 22, &w.app.title(), theme::ui_bold(14), t.text, row.w - 68);
            cv.text(
                row.x + 54,
                row.y + 41,
                if w.minimized { "Minimized · release Alt to restore" } else { "Release Alt to switch" },
                theme::ui(11),
                t.text_secondary,
            );
        }
    }
}
