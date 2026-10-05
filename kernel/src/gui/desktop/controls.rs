//! Quick settings share exactly the same preference paths as Settings.
use super::Desktop;
use crate::gui::{canvas::Canvas, geom::Rect, prefs, theme};
use aurora_abi::{
    input::{KeyCode, KeyEvent},
    pref,
};
use lumen::ui::{Action, Key, View};
impl Desktop {
    fn controls_icon(&self) -> Rect {
        let right = self
            .network_rect()
            .or_else(|| self.battery_rect())
            .or_else(|| self.volume_icon_rect())
            .map(|r| r.x - 8)
            .unwrap_or(self.w - 250);
        Rect::new(right - 28, 3, 28, 24)
    }
    fn controls_rect(&self) -> Rect {
        Rect::new(self.w - 356, theme::MENUBAR_H + 10, 344, 366)
    }
    pub(super) fn paint_controls_icon(&self, cv: &mut Canvas) {
        let r = self.controls_icon();
        let color = theme::current().text_on_glass;
        for (y, knob) in [(r.y + 8, r.x + 10), (r.y + 16, r.x + 19)] {
            cv.line(r.x + 5, y, r.right() - 5, y, 2, color);
            cv.fill_circle(knob, y, 3, color);
        }
    }
    fn control_action(&mut self, action: Option<Action>) -> bool {
        let Some(action) = action else { return false };
        let result = match action {
            Action::Toggle(1, on) => prefs::set(pref::DARK, if on { "1" } else { "0" }),
            Action::Toggle(2, on) => prefs::set(pref::DND, if on { "1" } else { "0" }),
            Action::Change(3, value) => prefs::set(pref::VOLUME, &alloc::format!("{}", value / 10)),
            Action::Activate(4) => {
                self.quick_open = false;
                self.spawn_program("/System/Apps/Settings.elf", &[]);
                Ok(())
            }
            _ => return false,
        };
        if let Err(e) = result {
            crate::gui::notify::system("Couldn't save setting", aurora_abi::err::name(e));
        }
        self.damage_all();
        true
    }
    pub(super) fn controls_press(&mut self, x: i32, y: i32) -> bool {
        if self.controls_icon().contains(x, y) {
            self.quick_open = !self.quick_open;
            self.close_menu();
            self.launcher = None;
            self.close_volume_popover();
            self.close_battery_popover();
            self.close_network_popover();
            if self.center_open() {
                self.toggle_center();
            }
            self.damage_all();
            return true;
        }
        if self.quick_open {
            if self.controls_rect().contains(x, y) {
                let a = self.quick_ui.click(x, y);
                self.control_action(a);
                return true;
            }
            self.quick_open = false;
            self.damage_all();
        }
        false
    }
    pub(super) fn controls_move(&mut self, x: i32, y: i32) -> bool {
        if !self.quick_open {
            return false;
        }
        if self.quick_ui.hover(x, y) {
            self.damage_all();
        }
        if self.buttons & 1 != 0 {
            let a = self.quick_ui.drag(x);
            return self.control_action(a);
        }
        false
    }
    pub(super) fn controls_key(&mut self, k: &KeyEvent) -> bool {
        if !self.quick_open || !k.pressed {
            return false;
        }
        let key = match k.code {
            KeyCode::Escape => {
                self.quick_open = false;
                self.damage_all();
                return true;
            }
            KeyCode::Tab => {
                if k.mods.shift {
                    Key::Previous
                } else {
                    Key::Next
                }
            }
            KeyCode::Enter => Key::Activate,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            _ => return true,
        };
        let a = self.quick_ui.key(key);
        self.control_action(a);
        self.damage_all();
        true
    }
    pub(super) fn paint_controls(&mut self, cv: &mut Canvas) {
        if !self.quick_open {
            return;
        }
        let r = self.controls_rect();
        let t = theme::current();
        cv.shadow(r, 20, 28, 10, t.shadow);
        cv.frosted(r, 20, t.panel_tint);
        let content = View::column(alloc::vec![
            View::heading("Control Center"),
            View::toggle(1, "Dark appearance", t.dark),
            View::toggle(2, "Do Not Disturb", prefs::get_bool(pref::DND)),
            View::slider(3, "Volume", prefs::get(pref::VOLUME).and_then(|v| v.parse::<i32>().ok()).unwrap_or(70) * 10),
            View::button(4, "Open Settings")
        ])
        .padding(22)
        .gap(15);
        self.quick_ui.draw(cv, &content, r);
    }
}
