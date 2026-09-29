//! Sound in the shell: the menu-bar volume control (a speaker icon that
//! opens a slider), the volume keys, and the level display they show.

use super::Desktop;
use crate::drivers::audio;
use crate::drivers::input::KeyCode;
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons;
use crate::gui::theme::{self, DOCK_H, DOCK_MARGIN, MENUBAR_H};
use crate::gui::{prefs, widgets};

/// How long the level display stays after a volume key.
const HUD_MS: u64 = 1500;
const HUD_SIZE: i32 = 180;
/// Volume keys move in sixteenths.
const STEP: u32 = 100 / 16;

#[derive(Default)]
pub struct VolumeUi {
    popover: bool,
    /// The slider knob is being dragged.
    dragging: bool,
    /// When the level display appeared (volume keys).
    hud_at: Option<u64>,
}

fn level() -> (u32, bool) {
    let s = audio::state();
    ((s & 0xFF) as u32, s & aurora_abi::audio::MUTED != 0)
}

impl Desktop {
    /// The speaker in the menu bar, left of the clock (only with a sound device).
    pub(super) fn volume_icon_rect(&self) -> Option<Rect> {
        if !audio::present() {
            return None;
        }
        let clock_w = theme::ui(13).width(&self.clock);
        Some(Rect::new(self.w - 18 - clock_w - 42, 3, 30, MENUBAR_H - 6))
    }

    fn volume_popover_rect(&self) -> Rect {
        let icon = self.volume_icon_rect().unwrap_or_default();
        let w = 280;
        let x = (icon.x + icon.w / 2 - w / 2).min(self.w - w - 8);
        Rect::new(x, MENUBAR_H + 6, w, 92)
    }

    fn volume_mute_rect(&self) -> Rect {
        let p = self.volume_popover_rect();
        Rect::new(p.x + 12, p.y + 42, 34, 34)
    }

    fn volume_slider_rect(&self) -> Rect {
        let p = self.volume_popover_rect();
        Rect::new(p.x + 60, p.y + 50, p.w - 80, 18)
    }

    fn damage_volume(&mut self) {
        if let Some(i) = self.volume_icon_rect() {
            self.damage(i);
        }
        let p = self.volume_popover_rect();
        self.damage(p.inset(-30));
        let h = self.volume_hud_rect();
        self.damage(h.inset(-30));
    }

    /// Applies a new level; `tick` plays the feedback sound.
    fn set_level(&mut self, volume: u32, muted: bool, tick: bool) {
        prefs::set_volume(volume, muted);
        if tick && !muted {
            audio::play_sound("volume");
        }
        self.damage_volume();
    }

    /// Mouse press: the menu-bar icon toggles the popover; inside it, the
    /// mute button and slider work; outside, it closes. True if handled.
    pub(super) fn volume_press(&mut self, x: i32, y: i32) -> bool {
        let on_icon = self.volume_icon_rect().is_some_and(|r| r.contains(x, y));
        if self.volume.popover {
            if self.volume_popover_rect().contains(x, y) {
                let (v, muted) = level();
                if self.volume_mute_rect().contains(x, y) {
                    self.set_level(v, !muted, false);
                } else if self.volume_slider_rect().inset(-10).contains(x, y) {
                    self.volume.dragging = true;
                    let nv = widgets::slider_value(self.volume_slider_rect(), x) as u32 / 10;
                    self.set_level(nv, false, false);
                }
                return true;
            }
            self.volume.popover = false;
            self.damage_volume();
            return on_icon || y < MENUBAR_H;
        }
        if on_icon && self.menu.is_none() && self.launcher.is_none() {
            if self.center_open() {
                self.toggle_center();
            }
            self.volume.popover = true;
            self.volume.hud_at = None;
            self.damage_volume();
            return true;
        }
        false
    }

    /// Pointer motion while the slider is held. True if handled.
    pub(super) fn volume_drag(&mut self, x: i32) -> bool {
        if !self.volume.dragging {
            return false;
        }
        let nv = widgets::slider_value(self.volume_slider_rect(), x) as u32 / 10;
        if nv != level().0 {
            self.set_level(nv, false, false);
        }
        true
    }

    /// Button released: a slider drag ends with the feedback sound.
    pub(super) fn volume_release(&mut self) -> bool {
        if core::mem::take(&mut self.volume.dragging) {
            let (v, muted) = level();
            self.set_level(v, muted, true);
            return true;
        }
        false
    }

    pub(super) fn volume_popover_open(&self) -> bool {
        self.volume.popover
    }

    pub(super) fn close_volume_popover(&mut self) {
        if core::mem::take(&mut self.volume.popover) {
            self.damage_volume();
        }
    }

    /// Mute / Volume Down / Volume Up keys. True if `code` was one.
    pub(super) fn volume_key(&mut self, code: KeyCode) -> bool {
        let (v, muted) = level();
        match code {
            KeyCode::Mute => self.set_level(v, !muted, false),
            KeyCode::VolumeUp => self.set_level((v + STEP).min(100), false, true),
            KeyCode::VolumeDown => self.set_level(v.saturating_sub(STEP), false, true),
            _ => return false,
        }
        if !self.volume.popover {
            self.volume.hud_at = Some(self.now_ms);
        }
        self.damage_volume();
        true
    }

    fn volume_hud_rect(&self) -> Rect {
        Rect::new((self.w - HUD_SIZE) / 2, self.h - DOCK_H - DOCK_MARGIN - HUD_SIZE - 60, HUD_SIZE, HUD_SIZE)
    }

    /// Hides the level display when its time is up. True while it shows.
    pub(super) fn step_volume(&mut self) -> bool {
        match self.volume.hud_at {
            Some(t) if self.now_ms >= t + HUD_MS => {
                self.volume.hud_at = None;
                let r = self.volume_hud_rect();
                self.damage(r.inset(-30));
                false
            }
            Some(_) => true,
            None => false,
        }
    }

    /// The menu-bar icon (painted with the menu bar).
    pub(super) fn paint_volume_icon(&self, cv: &mut Canvas) {
        let Some(r) = self.volume_icon_rect() else { return };
        let t = theme::current();
        if self.volume.popover {
            cv.fill_round_rect(r, 6, t.hover);
        }
        let (v, muted) = level();
        icons::speaker(cv, r.inset(4).offset(3, 0), (!muted).then_some(v), t.text_on_glass);
    }

    /// The popover and the level display (painted above windows).
    pub(super) fn paint_volume(&self, cv: &mut Canvas) {
        let t = theme::current();
        let (v, muted) = level();
        if self.volume.popover {
            let p = self.volume_popover_rect();
            if p.inset(-30).intersects(&cv.clip) {
                cv.shadow(p, 14, 26, 8, t.shadow);
                cv.glass(&self.blurred, p, 14, t.panel_tint);
                cv.stroke_round_rect(p, 14, if t.dark { 0x30FF_FFFF } else { 0x80FF_FFFF });
                cv.text(p.x + 16, p.y + 26, "Sound", theme::ui_bold(13), t.text);
                let device = if audio::state() & aurora_abi::audio::HEADPHONES != 0 { "Headphones" } else { "Output" };
                let f = theme::ui(12);
                cv.text(p.right() - 16 - f.width(device), p.y + 26, device, f, t.text_secondary);
                let m = self.volume_mute_rect();
                cv.fill_round_rect(m, 17, if muted { theme::accent() } else { t.hover });
                let c = if muted { 0xFFFF_FFFF } else { t.text };
                icons::speaker(cv, m.inset(8).offset(1, 0), (!muted).then_some(v), c);
                let value = if muted { 0 } else { v as i32 * 10 };
                widgets::slider(cv, self.volume_slider_rect(), value, self.volume.dragging);
            }
        }
        if self.volume.hud_at.is_some() {
            let h = self.volume_hud_rect();
            if h.inset(-30).intersects(&cv.clip) {
                cv.shadow(h, 28, 30, 10, t.shadow);
                cv.glass(&self.blurred, h, 28, t.panel_tint);
                icons::speaker(cv, Rect::new(h.x + 50, h.y + 36, 80, 80), (!muted).then_some(v), t.text);
                // The level, in sixteenths like the keys' steps.
                let bar = Rect::new(h.x + 24, h.bottom() - 34, h.w - 48, 6);
                cv.fill_round_rect(bar, 3, with_alpha(t.text, 0x30));
                let lit = if muted { 0 } else { (v * 16).div_ceil(100) as i32 };
                if lit > 0 {
                    cv.fill_round_rect(Rect::new(bar.x, bar.y, bar.w * lit / 16, bar.h), 3, t.text);
                }
            }
        }
    }
}
