//! The menu-bar battery: charge, charging state, and a popover with the
//! details (from ACPI; hidden on machines without a battery).

use super::Desktop;
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::icons;
use crate::gui::theme::{self, MENUBAR_H};
use alloc::format;
use alloc::string::String;
use aurora_abi::{power, PowerInfo};

const WIDTH: i32 = 64;

fn info() -> Option<PowerInfo> {
    let i = crate::acpi::power_info();
    (i.flags & power::BATTERY != 0).then_some(i)
}

fn duration(minutes: u32) -> String {
    match (minutes / 60, minutes % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// "About 2 h 5 min remaining" style status line.
fn status(i: &PowerInfo) -> String {
    let charging = i.flags & power::CHARGING != 0;
    let on_ac = i.flags & power::AC_ONLINE != 0;
    match (charging, on_ac, i.minutes) {
        (true, _, 0) => String::from("Charging"),
        (true, _, m) => format!("{} until full", duration(m)),
        (false, true, _) if i.percent >= 95 => String::from("Fully charged"),
        (false, true, _) => String::from("Not charging"),
        (false, false, 0) => String::from("Calculating time remaining…"),
        (false, false, m) => format!("About {} remaining", duration(m)),
    }
}

impl Desktop {
    /// Left of the volume control (or of the clock).
    pub(super) fn battery_rect(&self) -> Option<Rect> {
        info()?;
        let right = match self.volume_icon_rect() {
            Some(v) => v.x - 6,
            None => self.w - 18 - theme::ui(13).width(&self.clock) - 12,
        };
        Some(Rect::new(right - WIDTH, 3, WIDTH, MENUBAR_H - 6))
    }

    fn battery_popover_rect(&self) -> Rect {
        let b = self.battery_rect().unwrap_or_default();
        let w = 300;
        Rect::new((b.x + b.w / 2 - w / 2).min(self.w - w - 8), MENUBAR_H + 6, w, 118)
    }

    /// Clicks on the battery item or inside its popover. True if handled.
    pub(super) fn battery_press(&mut self, x: i32, y: i32) -> bool {
        let on_item = self.battery_rect().is_some_and(|r| r.contains(x, y));
        if self.battery_popover {
            let inside = self.battery_popover_rect().contains(x, y);
            if !inside {
                self.battery_popover = false;
                let p = self.battery_popover_rect();
                self.damage(p.inset(-30));
            }
            return inside || on_item || y < MENUBAR_H;
        }
        if on_item && self.menu.is_none() && self.launcher.is_none() {
            self.close_volume_popover();
            if self.center_open() {
                self.toggle_center();
            }
            self.battery_popover = true;
            let p = self.battery_popover_rect();
            self.damage(p.inset(-30));
            return true;
        }
        false
    }

    pub(super) fn close_battery_popover(&mut self) -> bool {
        if core::mem::take(&mut self.battery_popover) {
            let p = self.battery_popover_rect();
            self.damage(p.inset(-30));
            return true;
        }
        false
    }

    pub(super) fn paint_battery_item(&self, cv: &mut Canvas) {
        let (Some(r), Some(i)) = (self.battery_rect(), info()) else { return };
        let t = theme::current();
        if self.battery_popover {
            cv.fill_round_rect(r, 6, t.hover);
        }
        let text = format!("{}%", i.percent);
        let f = theme::ui(12);
        cv.text(r.x + 4, r.y + 15, &text, f, t.text_on_glass);
        let low = i.percent <= 10 && i.flags & power::DISCHARGING != 0;
        let fill = if low { 0xFFFF_453A } else { t.text_on_glass };
        let icon = Rect::new(r.right() - 30, r.y + (r.h - 12) / 2, 26, 12);
        icons::battery(cv, icon, i.percent, i.flags & power::CHARGING != 0, t.text_on_glass, fill);
    }

    pub(super) fn paint_battery_popover(&self, cv: &mut Canvas) {
        if !self.battery_popover {
            return;
        }
        let Some(i) = info() else { return };
        let p = self.battery_popover_rect();
        if !p.inset(-30).intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        cv.shadow(p, 14, 26, 8, t.shadow);
        cv.glass(&self.blurred, p, 14, t.panel_tint);
        cv.stroke_round_rect(p, 14, if t.dark { 0x30FF_FFFF } else { 0x80FF_FFFF });
        cv.text(p.x + 16, p.y + 26, "Battery", theme::ui_bold(13), t.text);
        let pct = format!("{}%", i.percent);
        let big = theme::ui_bold(20);
        cv.text(p.right() - 16 - big.width(&pct), p.y + 30, &pct, big, t.text);
        let f = theme::ui(12);
        cv.text(p.x + 16, p.y + 52, &status(&i), f, t.text_secondary);
        let source = if i.flags & power::AC_ONLINE != 0 { "Power Adapter" } else { "Battery" };
        cv.text(p.x + 16, p.y + 80, "Power Source", f, t.text_secondary);
        cv.text(p.right() - 16 - f.width(source), p.y + 80, source, f, t.text);
        let health = if i.design_mwh > 0 { i.full_mwh as u64 * 100 / i.design_mwh as u64 } else { 100 };
        let model = core::str::from_utf8(&i.model[..i.model_len as usize]).unwrap_or("");
        let line = if model.is_empty() {
            format!("Capacity {}%", health.min(100))
        } else {
            format!("{model} · capacity {}%", health.min(100))
        };
        cv.text(p.x + 16, p.y + 102, &line, f, t.text_secondary);
    }
}
