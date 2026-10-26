//! The boot screen: the WayV logo, a progress bar that advances with each
//! start-up stage, and the version at the bottom. It follows the appearance
//! (dark or light, remembered in boot.conf) and is replaced by Shore or the
//! desktop when Lumen Server paints its first frame.

use super::canvas::{mix, rgb, Canvas};
use super::geom::Rect;
use super::theme;
use alloc::vec::Vec;
use bootinfo::Framebuffer;

/// Stages reported after the boot screen appears, in order (see `telemetry::stage`).
const STAGES: [&str; 9] = ["scheduler", "smp", "storage", "network", "usb", "sound", "acpi", "gui", "desktop"];

struct Screen {
    fb: Framebuffer,
    back: Vec<u32>,
    bar: Rect,
    dark: bool,
    done: usize,
}

static SCREEN: spin::Mutex<Option<Screen>> = spin::Mutex::new(None);

fn colors(dark: bool) -> (u32, u32, u32) {
    // (background, track, version text)
    if dark {
        (rgb(0x04, 0x06, 0x1A), rgb(0x23, 0x25, 0x37), rgb(0x84, 0x88, 0x9A))
    } else {
        (rgb(0xF7, 0xF9, 0xFF), rgb(0xDE, 0xE0, 0xE8), rgb(0x6A, 0x70, 0x84))
    }
}

/// The WayV logo, scaled to `width` (area-averaged).
fn logo(width: u32) -> Option<aurora_image::Image> {
    let data = crate::fs::read_all("/System/Brand/wayv-logo.png").ok()?;
    let img = aurora_image::decode(&data).ok()?;
    Some(img.thumbnail(width, width))
}

/// Paints the boot screen (called once the system image and fonts are available).
pub fn show(fb: Framebuffer, dark: bool) {
    let (w, h) = (fb.width as i32, fb.height as i32);
    let mut back = alloc::vec![0u32; (w * h) as usize];
    let (bg, _, ink) = colors(dark);
    {
        let mut cv = Canvas::new(&mut back, w, h);
        cv.clip = Rect::new(0, 0, w, h);
        cv.fill_rect(cv.clip, bg);
        // A soft violet-blue glow behind the logo, strongest at the centre.
        let glow = rgb(0x4A, 0x63, 0xFF);
        let strength = if dark { 56 } else { 24 };
        let (cx, cy, rx, ry) = (w / 2, h / 2 - h / 40, w * 5 / 16, h * 5 / 16);
        for y in (cy - ry).max(0)..(cy + ry).min(h) {
            let dy = (y - cy) * 1024 / ry;
            for x in (cx - rx).max(0)..(cx + rx).min(w) {
                let dx = (x - cx) * 1024 / rx;
                let d2 = (dx * dx + dy * dy) as i64;
                if d2 < 1024 * 1024 {
                    let t = 1024 - (d2 * 1024 / (1024 * 1024)) as i32; // 1 at centre, 0 at the edge
                    let a = strength * t * t / (1024 * 1024);
                    let i = (y * w + x) as usize;
                    cv.buf[i] = mix(cv.buf[i], glow, a);
                }
            }
        }
        let logo_w = (w / 4).clamp(240, 480);
        let logo_h = match logo(logo_w as u32) {
            Some(img) => {
                let lh = img.height as i32;
                let r = Rect::new((w - img.width as i32) / 2, h / 2 - lh / 2 - 26, img.width as i32, lh);
                cv.draw_image(&img.pixels, img.width, img.height, r, 255);
                lh
            }
            None => {
                let f = theme::display(40);
                let r = Rect::new(0, h / 2 - 50, w, 50);
                cv.text_centered(r, "WayV", f, theme::accent());
                50
            }
        };
        let version = alloc::format!("WaveOS Aurora {}", short_version());
        let f = theme::ui_bold(13);
        cv.text(w / 2 - f.width(&version) / 2, h - 42, &version, f, ink);
        let _ = logo_h;
    }
    let bar_w = (w / 6).clamp(180, 280);
    let bar = Rect::new((w - bar_w) / 2, h / 2 + h / 16 + 18, bar_w, 4);
    let mut screen = Screen { fb, back, bar, dark, done: 0 };
    paint_bar(&mut screen);
    super::flush(super::front_buffer(&fb), &screen.back, &fb, Rect::new(0, 0, w, h));
    *SCREEN.lock() = Some(screen);
}

/// "0.8" for 0.8.0, "0.8.1" otherwise.
pub fn short_version() -> &'static str {
    crate::VERSION.strip_suffix(".0").unwrap_or(crate::VERSION)
}

fn paint_bar(s: &mut Screen) {
    let (w, h) = (s.fb.width as i32, s.fb.height as i32);
    let (_, track, _) = colors(s.dark);
    let mut cv = Canvas::new(&mut s.back, w, h);
    cv.clip = s.bar;
    cv.fill_round_rect(s.bar, 2, track);
    let filled = s.bar.w * (s.done as i32 + 1) / (STAGES.len() as i32 + 1);
    let fill = Rect::new(s.bar.x, s.bar.y, filled.max(4), s.bar.h);
    // The WayV gradient, blue to violet.
    let (from, to) = (rgb(0x4A, 0x63, 0xFF), rgb(0x8D, 0x2C, 0xF0));
    let bar = s.bar;
    cv.fill_round_rect_with(fill, 2, |x, _| mix(from, to, (x - bar.x) * 256 / bar.w.max(1)));
}

/// A start-up stage began: advance the bar (stages before the screen appeared are ignored).
pub fn stage(id: &str) {
    let Some(mut guard) = SCREEN.try_lock() else { return };
    let Some(s) = guard.as_mut() else { return };
    let Some(n) = STAGES.iter().position(|&st| st == id) else { return };
    if n < s.done {
        return;
    }
    s.done = n + 1;
    paint_bar(s);
    let r = s.bar;
    super::flush(super::front_buffer(&s.fb), &s.back, &s.fb, r);
}

/// The desktop took over the screen: free the boot screen's buffer.
pub fn finish() {
    SCREEN.lock().take();
}
