//! The quick overlay: the home button (or Super+Escape) pulls a panel over
//! whatever is on screen — a game, Wave Play's home or the desktop — with
//! Resume, Home, Close, volume, the battery, how fast the screen is being
//! drawn, and power. What lies underneath dims and blurs: it is the frame
//! that was showing, blurred once when the overlay opens.

use super::glyphs;
use super::pad::Button;
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::theme;
use alloc::string::String;
use alloc::vec::Vec;
use lumen::layer;
use lumen::spring::{self, Spring};

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Item {
    Resume,
    /// Wave Play's home (entering it from the desktop).
    Home,
    /// Close this window (its title).
    Close(u32, String),
    Volume,
    /// Leave Wave Play.
    Desktop,
    Power,
    Sleep,
    Restart,
    ShutDown,
    Cancel,
}

impl Item {
    fn label(&self) -> String {
        String::from(match self {
            Item::Resume => "Resume",
            Item::Home => "Wave Play Home",
            Item::Close(_, name) => return alloc::format!("Close {name}"),
            Item::Volume => "Volume",
            Item::Desktop => "Go to Desktop",
            Item::Power => "Power…",
            Item::Sleep => "Sleep",
            Item::Restart => "Restart",
            Item::ShutDown => "Shut Down",
            Item::Cancel => "Cancel",
        })
    }
}

pub struct Overlay {
    pub open: Spring,
    pub closing: bool,
    pub items: Vec<Item>,
    pub selected: usize,
    /// The power choices replace the list while shown.
    pub power: bool,
    /// What was on screen (device pixels), and the same blurred, small.
    pub sharp: Vec<u32>,
    blur: Vec<u32>,
    bw: i32,
    bh: i32,
    /// The program or place the overlay is over (shown in its header).
    pub title: String,
    pub subtitle: String,
}

/// What to do after a press.
pub enum Action {
    Nothing,
    Moved,
    Close,
    Run(Item),
}

impl Overlay {
    pub fn new(
        sharp: Vec<u32>,
        w_px: i32,
        h_px: i32,
        items: Vec<Item>,
        title: String,
        subtitle: String,
        now: u64,
    ) -> Overlay {
        // Blur a quarter-size copy once; it is filtered back up when drawn.
        let (bw, bh) = ((w_px / 4).max(1), (h_px / 4).max(1));
        let small = if sharp.len() == (w_px * h_px) as usize {
            layer::shrink(&sharp, w_px as u32, h_px as u32, bw as u32, bh as u32)
        } else {
            alloc::vec![0xFF10_1318; (bw * bh) as usize]
        };
        let blur = lumen::wallpaper::blur(&small, bw, bh, (bw / 64).max(3));
        let mut open = Spring::new(0, spring::SMOOTH);
        open.set_target(1000, now);
        Overlay { open, closing: false, items, selected: 0, power: false, sharp, blur, bw, bh, title, subtitle }
    }

    pub fn close(&mut self, now: u64) {
        self.closing = true;
        self.open.set_target(0, now);
    }

    pub fn visible(&self) -> Vec<Item> {
        if self.power {
            alloc::vec![Item::Sleep, Item::Restart, Item::ShutDown, Item::Cancel]
        } else {
            self.items.clone()
        }
    }

    pub fn press(&mut self, b: Button, now: u64) -> Action {
        let items = self.visible();
        match b {
            Button::Up | Button::Down => {
                let n = items.len();
                let next = if b == Button::Down { self.selected + 1 } else { self.selected + n - 1 };
                self.selected = next % n;
                Action::Moved
            }
            Button::Left | Button::Right if items.get(self.selected) == Some(&Item::Volume) => {
                Action::Run(Item::Volume)
            }
            Button::Accept => match items.get(self.selected).cloned() {
                Some(Item::Power) => {
                    self.power = true;
                    self.selected = 0;
                    Action::Moved
                }
                Some(Item::Cancel) => {
                    self.power = false;
                    self.selected = 0;
                    Action::Moved
                }
                Some(Item::Volume) | None => Action::Nothing,
                Some(i) => Action::Run(i),
            },
            Button::Back if self.power => {
                self.power = false;
                self.selected = 0;
                Action::Moved
            }
            Button::Back | Button::Guide | Button::Menu => {
                self.close(now);
                Action::Close
            }
            _ => Action::Nothing,
        }
    }

    /// The panel, in points, at progress `t`.
    pub fn panel(&self, w: i32, h: i32, t: i32) -> Rect {
        let pw = (w * 30 / 100).clamp(320, 440);
        let m = (h * 3 / 100).max(16);
        let x = spring::lerp(-pw - 20, m, t);
        // As tall as what it holds: header, rows, the readout and hints.
        let content = 136 + 54 * self.visible().len() as i32 + 150;
        Rect::new(x, m, pw, content.min(h - 2 * m))
    }

    /// Paints the overlay at its current progress: over the still of the
    /// screen it was opened on (`still`), or over whatever is already drawn.
    pub fn paint(&self, cv: &mut Canvas, w: i32, h: i32, family: u32, status: &Status, still: bool) {
        let t = self.open.value().clamp(0, 1060);
        let screen = Rect::new(0, 0, w, h);
        let k = cv.scale;
        // The frame underneath, blurring and dimming as the panel arrives.
        if still && self.sharp.len() == (w * k * h * k) as usize {
            cv.blit_same(&self.sharp, cv.clip);
        }
        let tb = t.clamp(0, 1000);
        layer::draw_scaled(cv, &self.blur, self.bw as u32, self.bh as u32, screen, (tb * 255 / 1000) as u32, 0);
        cv.fill_rect(cv.clip, ((tb * 110 / 1000) as u32) << 24);
        let p = self.panel(w, h, t);
        if p.right() <= 0 {
            return;
        }
        let th = theme::current();
        cv.shadow(p, 26, 40, 14, th.shadow);
        cv.fill_round_rect(p, 26, 0xF214_181F);
        cv.stroke_round_rect(p, 26, 0x26FF_FFFF);
        let pad = 26;
        let (x, inner_w) = (p.x + pad, p.w - 2 * pad);
        // Header: what this is over, then the clock and the battery.
        let mut y = p.y + 44;
        cv.text_clipped(x, y, &self.title, theme::display(24), 0xFFFF_FFFF, inner_w);
        y += 26;
        cv.text_clipped(x, y, &self.subtitle, theme::ui(14), 0xB3FF_FFFF, inner_w);
        y += 30;
        let info = theme::ui_bold(14);
        let mut ix = x;
        ix += cv.text(ix, y, &status.clock, theme::ui_bold(16), 0xFFFF_FFFF) + 18;
        if let Some((pct, charging)) = status.battery {
            crate::gui::icons::battery(cv, Rect::new(ix, y - 11, 24, 12), pct, charging, 0xFFFF_FFFF, 0xFFFF_FFFF);
            ix += 32;
            ix += cv.text(ix, y, &alloc::format!("{pct}%"), info, 0xCCFF_FFFF) + 18;
        }
        if let Some(name) = &status.controller {
            glyphs::controller(cv, ix + 12, y - 5, 22, 0xCCFF_FFFF);
            cv.text_clipped(ix + 30, y, name, info, 0xCCFF_FFFF, p.right() - pad - ix - 30);
        }
        y += 22;
        cv.fill_rect(Rect::new(x, y, inner_w, 1), 0x1FFF_FFFF);
        y += 14;
        // The list.
        let row_h = 50;
        let f = theme::ui_bold(16);
        for (i, item) in self.visible().iter().enumerate() {
            let r = Rect::new(p.x + 12, y, p.w - 24, row_h);
            let sel = i == self.selected;
            if sel {
                cv.fill_round_rect(r, 14, 0xFFF2_F4F8);
            }
            let fg = if sel { 0xFF10_141A } else { 0xFFFF_FFFF };
            let base = r.y + r.h / 2 + f.ascent as i32 / 2 - 1;
            cv.text(r.x + 16, base, &item.label(), f, fg);
            if *item == Item::Volume {
                let (vol, muted) = status.volume;
                let bar = Rect::new(r.x + 120, r.y + r.h / 2 - 3, r.w - 120 - 64, 6);
                let track = if sel { 0x3010_141A } else { 0x33FF_FFFF };
                cv.fill_round_rect(bar, 3, track);
                let filled = bar.w * vol as i32 / 100;
                if filled > 0 {
                    cv.fill_round_rect(Rect::new(bar.x, bar.y, filled.max(6), bar.h), 3, fg);
                }
                let label = if muted { String::from("Off") } else { alloc::format!("{vol}") };
                cv.text(bar.right() + 14, base, &label, theme::ui_bold(14), fg);
            }
            y += row_h + 4;
        }
        // How smoothly things are being drawn.
        let mut fy = p.bottom() - 84;
        cv.fill_rect(Rect::new(x, fy - 22, inner_w, 1), 0x1FFF_FFFF);
        let small = theme::ui(13);
        let perf = alloc::format!(
            "Screen {} fps · {}.{} ms a frame (slowest {}.{})",
            status.fps,
            status.frame_us / 1000,
            status.frame_us / 100 % 10,
            status.worst_us / 1000,
            status.worst_us / 100 % 10
        );
        cv.text_clipped(x, fy, &perf, small, 0xB3FF_FFFF, inner_w);
        if let Some(gfps) = status.game_fps {
            fy += 20;
            cv.text_clipped(x, fy, &alloc::format!("Game {gfps} fps"), small, 0xB3FF_FFFF, inner_w);
        }
        let hints = [(Button::Accept, "Select"), (Button::Back, if self.power { "Back" } else { "Resume" })];
        super::home::paint_hint_row(cv, p.right() - pad + 4, p.bottom() - 28, family, &hints, 255);
    }
}

/// What the overlay reports, gathered by the desktop each frame.
pub struct Status {
    pub clock: String,
    pub battery: Option<(u32, bool)>,
    pub controller: Option<String>,
    pub volume: (u32, bool),
    pub fps: u32,
    pub frame_us: u64,
    pub worst_us: u64,
    pub game_fps: Option<u32>,
}
