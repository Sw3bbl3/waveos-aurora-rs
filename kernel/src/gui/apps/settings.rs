use super::{App, AppKind, Env, Request};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme::{self, ACCENT};
use crate::gui::wallpaper;
use alloc::format;
use alloc::vec::Vec;

const THUMB_W: i32 = 150;
const THUMB_H: i32 = 94;

pub struct Settings {
    thumbs: Vec<Vec<u32>>,
    hovered: Option<Target>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Light,
    Dark,
    Wallpaper(u8),
}

impl Settings {
    pub fn new() -> Self {
        let thumbs = (0..wallpaper::NAMES.len() as u8).map(|i| wallpaper::generate(i, THUMB_W, THUMB_H)).collect();
        Self { thumbs, hovered: None }
    }

    fn targets(area: Rect) -> Vec<(Target, Rect)> {
        let mut v = Vec::new();
        let x = area.x + 28;
        v.push((Target::Light, Rect::new(x, area.y + 62, THUMB_W, THUMB_H)));
        v.push((Target::Dark, Rect::new(x + THUMB_W + 22, area.y + 62, THUMB_W, THUMB_H)));
        for i in 0..wallpaper::NAMES.len() as i32 {
            v.push((Target::Wallpaper(i as u8), Rect::new(x + i * (THUMB_W + 22), area.y + 238, THUMB_W, THUMB_H)));
        }
        v
    }

    fn target_at(area: Rect, x: i32, y: i32) -> Option<Target> {
        Self::targets(area).into_iter().find(|(_, r)| r.contains(x, y)).map(|(t, _)| t)
    }
}

fn mode_preview(cv: &mut Canvas, r: Rect, dark: bool) {
    let t = if dark { &theme::DARK } else { &theme::LIGHT };
    let bg = if dark { 0xFF10_1426 } else { 0xFFBF_D6F5 };
    cv.fill_round_rect(r, 10, bg);
    let win = Rect::new(r.x + 22, r.y + 16, r.w - 44, r.h - 30);
    cv.fill_round_rect(win, 6, t.window_bg);
    cv.fill_rect(Rect::new(win.x + 6, win.y + 16, win.w - 12, 1), t.separator);
    for (i, c) in [theme::CLOSE, theme::MINIMIZE, theme::ZOOM].iter().enumerate() {
        cv.fill_circle(win.x + 9 + i as i32 * 9, win.y + 8, 3, *c);
    }
    for i in 0..3 {
        cv.fill_round_rect(
            Rect::new(win.x + 10, win.y + 26 + i * 10, win.w - 30 - i * 14, 5),
            2,
            with_alpha(t.text, 0x50),
        );
    }
}

impl App for Settings {
    fn kind(&self) -> AppKind {
        AppKind::Settings
    }
    fn size(&self) -> (i32, i32) {
        (560, 470)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let x = area.x + 28;
        cv.text(x, area.y + 30, "Appearance", theme::ui_bold(16), t.text);
        cv.text(x, area.y + 206, "Wallpaper", theme::ui_bold(16), t.text);

        for (target, r) in Self::targets(area) {
            let (selected, label) = match target {
                Target::Light => (!t.dark, "Light"),
                Target::Dark => (t.dark, "Dark"),
                Target::Wallpaper(i) => (theme::wallpaper() == i, wallpaper::NAMES[i as usize]),
            };
            match target {
                Target::Wallpaper(i) => {
                    let img = &self.thumbs[i as usize];
                    cv.fill_round_rect_with(r, 10, |px, py| img[((py - r.y) * THUMB_W + (px - r.x)) as usize]);
                }
                Target::Light => mode_preview(cv, r, false),
                Target::Dark => mode_preview(cv, r, true),
            }
            if selected {
                cv.stroke_round_rect(r.inset(-3), 13, ACCENT);
                cv.stroke_round_rect(r.inset(-2), 12, ACCENT);
            } else if self.hovered == Some(target) {
                cv.stroke_round_rect(r.inset(-2), 12, with_alpha(ACCENT, 0x80));
            }
            let f = if selected { theme::ui_bold(13) } else { theme::ui(13) };
            let lw = f.width(label);
            cv.text(r.x + (r.w - lw) / 2, r.bottom() + 22, label, f, if selected { t.text } else { t.text_secondary });
        }

        let info = format!(
            "WaveOS Aurora {} · {}×{} · {} MiB RAM",
            crate::VERSION,
            env.screen.0,
            env.screen.1,
            crate::mm::stats().total_bytes >> 20
        );
        cv.fill_rect(Rect::new(x, area.bottom() - 48, area.w - 56, 1), t.separator);
        cv.text(x, area.bottom() - 22, &info, theme::ui(12), t.text_secondary);
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        match Self::target_at(area, x, y) {
            Some(Target::Light) => env.requests.push(Request::SetDark(false)),
            Some(Target::Dark) => env.requests.push(Request::SetDark(true)),
            Some(Target::Wallpaper(i)) => env.requests.push(Request::SetWallpaper(i)),
            None => return false,
        }
        true
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = Self::target_at(area, x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
}
