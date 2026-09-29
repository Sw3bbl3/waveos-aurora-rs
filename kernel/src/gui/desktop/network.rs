//! The menu-bar network item: whether we're connected, and a popover with
//! the interface, its addresses and traffic.

use super::Desktop;
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme::{self, MENUBAR_H};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::net::*;

fn ip(a: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", a[0], a[1], a[2], a[3])
}

fn size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.1} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{} KB", n >> 10),
        n => format!("{n} B"),
    }
}

impl Desktop {
    /// Left of the battery (or volume, or clock); only with a network card.
    pub(super) fn network_rect(&self) -> Option<Rect> {
        let cards = crate::net::interfaces().iter().any(|i| i.flags & IF_LOOPBACK == 0);
        if !cards {
            return None;
        }
        let right = match (self.battery_rect(), self.volume_icon_rect()) {
            (Some(b), _) => b.x - 6,
            (None, Some(v)) => v.x - 6,
            _ => self.w - 18 - theme::ui(13).width(&self.clock) - 12,
        };
        Some(Rect::new(right - 30, 3, 30, MENUBAR_H - 6))
    }

    fn network_popover_rect(&self) -> Rect {
        let r = self.network_rect().unwrap_or_default();
        let w = 300;
        Rect::new((r.x + r.w / 2 - w / 2).min(self.w - w - 8), MENUBAR_H + 6, w, 176)
    }

    pub(super) fn network_press(&mut self, x: i32, y: i32) -> bool {
        let on_item = self.network_rect().is_some_and(|r| r.contains(x, y));
        if self.network_popover {
            let inside = self.network_popover_rect().contains(x, y);
            if !inside {
                self.network_popover = false;
                let p = self.network_popover_rect();
                self.damage(p.inset(-30));
            }
            return inside || on_item || y < MENUBAR_H;
        }
        if on_item && self.menu.is_none() && self.launcher.is_none() {
            self.close_volume_popover();
            self.close_battery_popover();
            if self.center_open() {
                self.toggle_center();
            }
            self.network_popover = true;
            let p = self.network_popover_rect();
            self.damage(p.inset(-30));
            return true;
        }
        false
    }

    pub(super) fn close_network_popover(&mut self) -> bool {
        if core::mem::take(&mut self.network_popover) {
            let p = self.network_popover_rect();
            self.damage(p.inset(-30));
            return true;
        }
        false
    }

    /// "<···>": the wired-network glyph; faded and struck through when offline.
    pub(super) fn paint_network_item(&self, cv: &mut Canvas) {
        let Some(r) = self.network_rect() else { return };
        let t = theme::current();
        if self.network_popover {
            cv.fill_round_rect(r, 6, t.hover);
        }
        let online = crate::net::online();
        let c = if online { t.text_on_glass } else { with_alpha(t.text_on_glass, 0x70) };
        let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
        cv.polyline(&[(cx - 6, cy - 5), (cx - 11, cy), (cx - 6, cy + 5)], 2, c);
        cv.polyline(&[(cx + 6, cy - 5), (cx + 11, cy), (cx + 6, cy + 5)], 2, c);
        for dx in [-3, 0, 3] {
            cv.fill_circle(cx + dx, cy, 1, c);
        }
        if !online {
            cv.line(cx - 8, cy + 7, cx + 8, cy - 7, 2, c);
        }
    }

    pub(super) fn paint_network_popover(&self, cv: &mut Canvas) {
        if !self.network_popover {
            return;
        }
        let p = self.network_popover_rect();
        if !p.inset(-30).intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        cv.shadow(p, 14, 26, 8, t.shadow);
        cv.glass(&self.blurred, p, 14, t.panel_tint);
        cv.stroke_round_rect(p, 14, if t.dark { 0x30FF_FFFF } else { 0x80FF_FFFF });
        let cards: Vec<_> = crate::net::interfaces().into_iter().filter(|i| i.flags & IF_LOOPBACK == 0).collect();
        let Some(i) = cards.iter().find(|i| i.ip != [0; 4]).or(cards.first()) else { return };
        let f = theme::ui(12);
        cv.text(p.x + 16, p.y + 26, "Network", theme::ui_bold(13), t.text);
        let (status, color) = match (i.flags & IF_LINK != 0, i.ip != [0; 4]) {
            (true, true) => ("Connected", 0xFF30_D158),
            (true, false) => ("Getting an address…", 0xFFFF_9F0A),
            (false, _) => ("Cable unplugged", 0xFFFF_453A),
        };
        cv.fill_circle(p.right() - 16 - f.width(status) - 10, p.y + 22, 4, color);
        cv.text(p.right() - 16 - f.width(status), p.y + 26, status, f, t.text_secondary);
        let driver = core::str::from_utf8(&i.driver[..i.driver_len as usize]).unwrap_or("");
        let name = core::str::from_utf8(&i.name[..i.name_len as usize]).unwrap_or("");
        let dns: Vec<String> = i.dns.iter().filter(|d| **d != [0; 4]).map(|d| ip(*d)).collect();
        let rows = [
            ("Ethernet", format!("{name} · {driver}")),
            ("IP address", if i.ip == [0; 4] { String::from("—") } else { ip(i.ip) }),
            ("Router", if i.gateway == [0; 4] { String::from("—") } else { ip(i.gateway) }),
            ("DNS", if dns.is_empty() { String::from("—") } else { dns.join(", ") }),
            ("Traffic", format!("↓ {}   ↑ {}", size(i.rx_bytes), size(i.tx_bytes))),
        ];
        for (k, (label, value)) in rows.iter().enumerate() {
            let y = p.y + 54 + k as i32 * 24;
            cv.text(p.x + 16, y, label, f, t.text_secondary);
            cv.text_clipped(p.x + 110, y, value, f, t.text, p.w - 126);
        }
    }
}
