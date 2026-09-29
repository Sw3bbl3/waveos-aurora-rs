//! Preview — views PNG and BMP images.
//!
//! Fit to window or zoom (wheel, +/−, double-click for actual size), drag to
//! pan, rotate, step through the pictures in the same folder with ← and →,
//! and set a picture as the desktop wallpaper.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora::fs;
use aurora_image::Image;
use ripple::canvas::{with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::icons::{self, Icon};
use ripple::theme;
use ripple::widgets::{button, ButtonStyle};
use ripple::{App, Env, KeyCode, KeyEvent, Request};

aurora::entry!(main);

const TOOLBAR_H: i32 = 46;
const MIN_ZOOM: i32 = 5; // percent
const MAX_ZOOM: i32 = 3200;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Prev,
    Next,
    ZoomOut,
    ZoomIn,
    Fit,
    Rotate,
    Wallpaper,
}

struct Preview {
    path: Option<String>,
    image: Option<Image>,
    error: Option<String>,
    /// Percent; `None` = fit to window.
    zoom: Option<i32>,
    /// Pan offset of the image centre from the viewport centre, in screen pixels.
    pan: (i32, i32),
    drag_from: Option<(i32, i32, (i32, i32))>,
    hover: Option<Tool>,
    /// A reduced copy for drawing at small zoom levels (quality and speed).
    reduced: Option<Image>,
    status: Option<(String, u64)>,
    /// Content area of the last draw (keyboard zoom uses it).
    area: Rect,
}

impl Preview {
    fn new(path: Option<String>) -> Self {
        let mut p = Preview {
            path: None,
            image: None,
            error: None,
            zoom: None,
            pan: (0, 0),
            drag_from: None,
            hover: None,
            reduced: None,
            status: None,
            area: Rect::new(0, 0, 640, 460),
        };
        if let Some(path) = path {
            p.load(path);
        }
        p
    }

    fn load(&mut self, path: String) {
        self.image = None;
        self.reduced = None;
        self.zoom = None;
        self.pan = (0, 0);
        self.error = match fs::read(&path) {
            Ok(data) => match aurora_image::decode(&data) {
                Ok(img) => {
                    self.image = Some(img);
                    None
                }
                Err(aurora_image::Error::Unsupported) => Some(String::from("This file isn't a PNG or BMP image.")),
                Err(aurora_image::Error::TooLarge) => Some(String::from("This image is too large to open.")),
                Err(aurora_image::Error::Corrupt) => Some(String::from("This image is damaged.")),
            },
            Err(e) => Some(format!("Couldn't open the file: {e}")),
        };
        self.path = Some(path);
    }

    /// Other images in the same folder, sorted by name.
    fn siblings(&self) -> Vec<String> {
        let Some(path) = &self.path else { return Vec::new() };
        let dir = fs::parent(path);
        let mut v: Vec<String> = fs::read_dir(dir)
            .map(|es| {
                es.into_iter()
                    .filter(|e| !e.is_dir && aurora_image::is_image_name(&e.name))
                    .map(|e| fs::join(dir, &e.name))
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by_key(|a| a.to_lowercase());
        v
    }

    fn step(&mut self, delta: i32) {
        let list = self.siblings();
        let Some(cur) = self.path.as_ref().and_then(|p| list.iter().position(|s| s == p)) else { return };
        let next = (cur as i32 + delta).rem_euclid(list.len() as i32) as usize;
        if next != cur {
            self.load(list[next].clone());
        }
    }

    fn viewport(area: Rect) -> Rect {
        Rect::new(area.x, area.y + TOOLBAR_H, area.w, area.h - TOOLBAR_H)
    }

    /// Current zoom in percent (fit computes it from the viewport).
    fn effective_zoom(&self, area: Rect) -> i32 {
        let Some(img) = &self.image else { return 100 };
        match self.zoom {
            Some(z) => z,
            None => {
                let v = Self::viewport(area).inset(24);
                let zx = v.w * 100 / img.width.max(1) as i32;
                let zy = v.h * 100 / img.height.max(1) as i32;
                zx.min(zy).clamp(MIN_ZOOM, 100)
            }
        }
    }

    fn image_rect(&self, area: Rect) -> Option<Rect> {
        let img = self.image.as_ref()?;
        let z = self.effective_zoom(area);
        let (w, h) =
            ((img.width as i64 * z as i64 / 100).max(1) as i32, (img.height as i64 * z as i64 / 100).max(1) as i32);
        let v = Self::viewport(area);
        let (cx, cy) = v.center();
        Some(Rect::new(cx - w / 2 + self.pan.0, cy - h / 2 + self.pan.1, w, h))
    }

    fn set_zoom(&mut self, area: Rect, z: i32) {
        let old = self.effective_zoom(area);
        let z = z.clamp(MIN_ZOOM, MAX_ZOOM);
        // Keep the viewport centre on the same image point.
        self.pan = (self.pan.0 * z / old.max(1), self.pan.1 * z / old.max(1));
        self.zoom = Some(z);
    }

    fn zoom_by(&mut self, area: Rect, steps: i32) {
        let z = self.effective_zoom(area);
        let next = if steps > 0 { z * 5 / 4 + 1 } else { z * 4 / 5 };
        self.set_zoom(area, next);
    }

    fn rotate(&mut self) {
        let Some(img) = &self.image else { return };
        let (w, h) = (img.width, img.height);
        let mut px = alloc::vec![0u32; img.pixels.len()];
        for y in 0..h {
            for x in 0..w {
                // 90° clockwise: (x, y) → (h - 1 - y, x) in a h×w image.
                px[(x * h + (h - 1 - y)) as usize] = img.pixels[(y * w + x) as usize];
            }
        }
        self.image = Some(Image { width: h, height: w, pixels: px });
        self.reduced = None;
        self.pan = (0, 0);
    }

    fn tools(area: Rect) -> Vec<(Tool, Rect)> {
        let y = area.y + 8;
        let r = area.right();
        alloc::vec![
            (Tool::Prev, Rect::new(area.x + 12, y, 30, 30)),
            (Tool::Next, Rect::new(area.x + 46, y, 30, 30)),
            (Tool::Wallpaper, Rect::new(r - 150, y, 138, 30)),
            (Tool::Rotate, Rect::new(r - 190, y, 30, 30)),
            (Tool::Fit, Rect::new(r - 250, y, 52, 30)),
            (Tool::ZoomIn, Rect::new(r - 290, y, 30, 30)),
            (Tool::ZoomOut, Rect::new(r - 380, y, 30, 30)),
        ]
    }

    fn tool_at(area: Rect, x: i32, y: i32) -> Option<Tool> {
        Self::tools(area).into_iter().find(|(_, r)| r.contains(x, y)).map(|(t, _)| t)
    }

    fn run_tool(&mut self, t: Tool, area: Rect, env: &mut Env) {
        match t {
            Tool::Prev => self.step(-1),
            Tool::Next => self.step(1),
            Tool::ZoomOut => self.zoom_by(area, -1),
            Tool::ZoomIn => self.zoom_by(area, 1),
            Tool::Fit => {
                self.zoom = None;
                self.pan = (0, 0);
            }
            Tool::Rotate => self.rotate(),
            Tool::Wallpaper => {
                if let (Some(p), Some(_)) = (&self.path, &self.image) {
                    env.requests.push(Request::SetWallpaperImage(p.clone()));
                    self.status = Some((String::from("Set as desktop wallpaper"), env.now_ms));
                }
            }
        }
    }
}

fn glyph_button(cv: &mut Canvas, r: Rect, hovered: bool, draw: impl FnOnce(&mut Canvas, i32, i32, u32)) {
    let t = theme::current();
    if hovered {
        cv.fill_round_rect(r, 7, t.hover);
    }
    let (cx, cy) = r.center();
    draw(cv, cx, cy, t.text);
}

impl App for Preview {
    fn title(&self) -> String {
        match &self.path {
            Some(p) => String::from(fs::file_name(p)),
            None => String::from("Preview"),
        }
    }

    fn size(&self) -> (i32, i32) {
        match &self.image {
            Some(img) => {
                let (w, h) = aurora_image::fit(img.width, img.height, 960, 600);
                ((w as i32).max(560), h as i32 + TOOLBAR_H)
            }
            None => (640, 460),
        }
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        let t = theme::current();
        let view = Self::viewport(area);
        cv.fill_rect(view, if t.dark { 0xFF18_181E } else { 0xFFE9_E9EE });

        match (&self.image, &self.error) {
            (Some(img), _) => {
                let r = self.image_rect(area).unwrap();
                cv.with_clip(view, |cv| {
                    if !img.is_opaque() {
                        cv.checkerboard(r, 8, 0xFFFF_FFFF, 0xFFDD_DDE2);
                    }
                    // Draw from a reduced copy when shrinking a lot (quality and speed).
                    if r.w * 2 < img.width as i32 {
                        let stale = self.reduced.as_ref().is_none_or(|t| t.width != r.w as u32);
                        if stale {
                            self.reduced = Some(img.thumbnail(r.w as u32, r.h as u32));
                        }
                        let small = self.reduced.as_ref().unwrap();
                        cv.draw_image(&small.pixels, small.width, small.height, r, 255);
                    } else {
                        cv.draw_image(&img.pixels, img.width, img.height, r, 255);
                    }
                });
            }
            (None, Some(err)) => {
                let (cx, cy) = view.center();
                icons::draw(cv, Icon::Preview, Rect::new(cx - 32, cy - 70, 64, 64));
                cv.text_centered(Rect::new(view.x, cy + 6, view.w, 24), err, theme::ui(14), t.text_secondary);
            }
            (None, None) => {
                let (cx, cy) = view.center();
                icons::draw(cv, Icon::Preview, Rect::new(cx - 32, cy - 70, 64, 64));
                cv.text_centered(Rect::new(view.x, cy + 6, view.w, 24), "No image open", theme::ui_bold(16), t.text);
                cv.text_centered(
                    Rect::new(view.x, cy + 32, view.w, 20),
                    "Double-click a picture in Files to open it here.",
                    theme::ui(13),
                    t.text_secondary,
                );
            }
        }

        // Toolbar.
        let bar = Rect::new(area.x, area.y, area.w, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(bar.x, bar.bottom() - 1, bar.w, 1), t.separator);
        let has = self.image.is_some();
        for (tool, r) in Self::tools(area) {
            let hovered = self.hover == Some(tool);
            match tool {
                Tool::Prev | Tool::Next => glyph_button(cv, r, hovered, |cv, cx, cy, c| {
                    let d = if tool == Tool::Prev { 1 } else { -1 };
                    cv.line(cx + 3 * d, cy - 6, cx - 3 * d, cy, 2, c);
                    cv.line(cx - 3 * d, cy, cx + 3 * d, cy + 6, 2, c);
                }),
                Tool::ZoomOut | Tool::ZoomIn => glyph_button(cv, r, hovered, |cv, cx, cy, c| {
                    cv.line(cx - 6, cy, cx + 6, cy, 2, c);
                    if tool == Tool::ZoomIn {
                        cv.line(cx, cy - 6, cx, cy + 6, 2, c);
                    }
                }),
                Tool::Rotate => glyph_button(cv, r, hovered, |cv, cx, cy, c| {
                    let pts: Vec<(i32, i32)> = (0..=18)
                        .map(|i| {
                            let a = 180 + i * 38; // most of a circle, 1024 units per turn
                            (cx + 7 * ripple::math::sin(a + 256) / 16384, cy + 7 * ripple::math::sin(a) / 16384)
                        })
                        .collect();
                    cv.polyline(&pts, 2, c);
                    let (ex, ey) = *pts.last().unwrap();
                    cv.line(ex, ey, ex + 4, ey - 3, 2, c);
                    cv.line(ex, ey, ex + 4, ey + 3, 2, c);
                }),
                Tool::Fit => {
                    button(cv, r, "Fit", ButtonStyle::Secondary, hovered);
                    if self.zoom.is_none() && has {
                        cv.stroke_round_rect(r, 8, with_alpha(theme::accent(), 0xC0));
                    }
                }
                Tool::Wallpaper => button(cv, r, "Set as Wallpaper", ButtonStyle::Secondary, hovered && has),
            }
        }
        // Zoom level between the zoom buttons.
        let zr = Rect::new(area.right() - 350, area.y + 8, 60, 30);
        let zoom_text = if has { format!("{}%", self.effective_zoom(area)) } else { String::from("—") };
        cv.text_centered(zr, &zoom_text, theme::ui(13), t.text_secondary);
        // Name and dimensions.
        if let Some(img) = &self.image {
            let info = format!("{} × {}", img.width, img.height);
            cv.text(area.x + 88, area.y + 29, &info, theme::ui(13), t.text_secondary);
        }
        if let Some((msg, at)) = &self.status {
            if env.now_ms < at + 2500 {
                let f = theme::ui_bold(13);
                let w = f.width(msg) + 32;
                let r = Rect::new(view.x + (view.w - w) / 2, view.bottom() - 52, w, 34);
                cv.fill_round_rect(r, 17, 0xE025_2530);
                cv.text_centered(r, msg, f, 0xFFFF_FFFF);
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(t) = Self::tool_at(area, x, y) {
            self.run_tool(t, area, env);
            return true;
        }
        if Self::viewport(area).contains(x, y) && self.image.is_some() {
            self.drag_from = Some((x, y, self.pan));
        }
        false
    }

    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if Self::tool_at(area, x, y).is_some() {
            return self.click(x, y, area, env);
        }
        if self.image.is_some() && Self::viewport(area).contains(x, y) {
            if self.zoom.is_none() {
                self.set_zoom(area, 100);
            } else {
                self.zoom = None;
                self.pan = (0, 0);
            }
            return true;
        }
        false
    }

    fn drag(&mut self, x: i32, y: i32, _area: Rect, _env: &mut Env) -> bool {
        if let Some((sx, sy, start)) = self.drag_from {
            self.pan = (start.0 + x - sx, start.1 + y - sy);
            return true;
        }
        false
    }

    fn release(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        self.drag_from = None;
        false
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = Self::tool_at(area, x, y);
        let changed = h != self.hover;
        self.hover = h;
        changed
    }

    fn scroll(&mut self, delta: i32, area: Rect) -> bool {
        if self.image.is_none() {
            return false;
        }
        self.zoom_by(area, -delta.signum());
        true
    }

    fn key(&mut self, ev: &KeyEvent, _env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        let area = self.area;
        match (ev.code, ev.ch) {
            (KeyCode::Left, _) => self.step(-1),
            (KeyCode::Right, _) => self.step(1),
            (_, Some('+')) | (_, Some('=')) => self.zoom_by(area, 1),
            (_, Some('-')) => self.zoom_by(area, -1),
            (_, Some('0')) => {
                self.zoom = None;
                self.pan = (0, 0);
            }
            (_, Some('1')) => self.set_zoom(area, 100),
            (_, Some('r')) | (_, Some('R')) => self.rotate(),
            _ => return false,
        }
        true
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        match &self.status {
            Some((_, at)) if env.now_ms >= at + 2500 => {
                self.status = None;
                true
            }
            _ => false,
        }
    }
}

fn main(args: aurora::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    ripple::run(Preview::new(path))
}
