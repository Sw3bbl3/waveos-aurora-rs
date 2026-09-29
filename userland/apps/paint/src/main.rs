//! Paint — draw with brushes and shapes, fill areas, pick colours, and save
//! PNG pictures.
//!
//! Brush strokes are anti-aliased and composited through a per-stroke
//! coverage mask, so overlapping dabs never darken the edge. Shapes preview
//! while you drag. Undo keeps the last dozen steps.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use aurora::fs;
use aurora_image::Image;
use ripple::canvas::{blend, rgb, with_alpha, Canvas};
use ripple::geom::Rect;
use ripple::text::TextField;
use ripple::theme;
use ripple::widgets::{self, button, ButtonStyle};
use ripple::{App, Env, KeyCode, KeyEvent};

use libm::{ceilf, fabsf, sqrtf};

const TOOLBAR_H: i32 = 56;
const STATUS_H: i32 = 26;
const UNDO: usize = 12;
const WHITE: u32 = 0xFFFF_FFFF;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Brush,
    Eraser,
    Line,
    Rect,
    Ellipse,
    Fill,
    Picker,
}

const TOOLS: [(Tool, &str); 7] = [
    (Tool::Brush, "Brush (B)"),
    (Tool::Eraser, "Eraser (E)"),
    (Tool::Line, "Line (L)"),
    (Tool::Rect, "Rectangle (R)"),
    (Tool::Ellipse, "Ellipse (O)"),
    (Tool::Fill, "Fill (F)"),
    (Tool::Picker, "Colour picker (I)"),
];

const PALETTE: [u32; 14] = [
    rgb(0x1D, 0x1D, 0x24),
    rgb(0x8E, 0x8E, 0x93),
    WHITE,
    rgb(0xFF, 0x3B, 0x30),
    rgb(0xFF, 0x95, 0x00),
    rgb(0xFF, 0xCC, 0x00),
    rgb(0x34, 0xC7, 0x59),
    rgb(0x00, 0xC7, 0xBE),
    rgb(0x30, 0xB0, 0xC7),
    rgb(0x00, 0x7A, 0xFF),
    rgb(0x58, 0x56, 0xD6),
    rgb(0xAF, 0x52, 0xDE),
    rgb(0xFF, 0x2D, 0x55),
    rgb(0xA2, 0x84, 0x5E),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    Tool(usize),
    Size,
    FillShapes,
    Swatch(usize),
    Current,
    Undo,
    Redo,
    Save,
    HueBar,
    SvSquare,
    SheetSave,
    SheetCancel,
}

struct Stroke {
    /// The image before this operation (also the undo step).
    before: Vec<u32>,
    /// Per-pixel brush coverage for this stroke.
    mask: Vec<u8>,
    last: (f32, f32),
    start: (f32, f32),
    /// Area the shape preview touched last time (restored before redrawing).
    dirty: Option<Rect>,
}

pub struct Paint {
    img: Image,
    path: Option<String>,
    dirty: bool,
    tool: Tool,
    color: u32,
    size: i32,
    fill_shapes: bool,
    /// Percent; `None` fits the window.
    zoom: Option<i32>,
    undo: Vec<Vec<u32>>,
    redo: Vec<Vec<u32>>,
    stroke: Option<Stroke>,
    hover: Option<Hit>,
    picker: Option<(f32, f32, f32)>,
    sheet: Option<TextField>,
    cursor: Option<(i32, i32)>,
    status: Option<(String, u64)>,
    area: Rect,
    sliding: Option<Hit>,
}

fn rgb_to_hsv(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = (((c >> 16) & 0xFF) as f32 / 255.0, ((c >> 8) & 0xFF) as f32 / 255.0, (c & 0xFF) as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d) % 6.0)
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h };
    (h, if max == 0.0 { 0.0 } else { d / max }, max)
}

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> u32 {
    let c = v * s;
    let hp = (h / 60.0) % 6.0;
    let x = c * (1.0 - fabsf((hp % 2.0) - 1.0));
    let (r, g, b) = match hp as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let to = |f: f32| ((f + m) * 255.0 + 0.5) as u8;
    rgb(to(r), to(g), to(b))
}

impl Paint {
    fn new(path: Option<String>) -> Self {
        let mut p = Paint {
            img: Image::new(1200, 800, WHITE),
            path: None,
            dirty: false,
            tool: Tool::Brush,
            color: PALETTE[9],
            size: 8,
            fill_shapes: false,
            zoom: None,
            undo: Vec::new(),
            redo: Vec::new(),
            stroke: None,
            hover: None,
            picker: None,
            sheet: None,
            cursor: None,
            status: None,
            area: Rect::new(0, 0, 1000, 700),
            sliding: None,
        };
        if let Some(path) = path {
            p.open(&path);
        }
        p
    }

    fn open(&mut self, path: &str) {
        match fs::read(path).ok().and_then(|d| aurora_image::decode(&d).ok()) {
            Some(mut img) => {
                // Flatten transparency onto white: Paint works on opaque pictures.
                for px in &mut img.pixels {
                    *px = blend(WHITE, *px, *px >> 24);
                }
                self.img = img;
                self.path = Some(String::from(path));
                self.dirty = false;
                self.undo.clear();
                self.redo.clear();
                self.zoom = None;
            }
            None => self.flash(format!("Couldn't open {}", fs::file_name(path))),
        }
    }

    fn flash(&mut self, msg: String) {
        self.status = Some((msg, aurora::time::uptime_ms()));
    }

    fn name(&self) -> String {
        self.path.as_deref().map(|p| String::from(fs::file_name(p))).unwrap_or_else(|| String::from("Untitled"))
    }

    fn save_to(&mut self, path: String) {
        let png = aurora_image::png::encode(&self.img);
        match fs::write(&path, &png) {
            Ok(()) => {
                self.flash(format!("Saved {}", fs::file_name(&path)));
                self.path = Some(path);
                self.dirty = false;
            }
            Err(e) => self.flash(format!("Couldn't save: {e}")),
        }
    }

    fn save(&mut self) {
        match self.path.clone() {
            Some(p) if p.to_lowercase().ends_with(".png") => self.save_to(p),
            _ => {
                let mut f = TextField::new("Untitled.png", "Name");
                f.edit.select(0, 8);
                self.sheet = Some(f);
            }
        }
    }

    fn push_undo(&mut self, snapshot: Vec<u32>) {
        self.undo.push(snapshot);
        if self.undo.len() > UNDO {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            let cur = core::mem::replace(&mut self.img.pixels, prev);
            self.redo.push(cur);
            self.dirty = true;
        }
    }

    fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            let cur = core::mem::replace(&mut self.img.pixels, next);
            self.undo.push(cur);
        }
    }

    // ------------------------------------------------------------ layout

    fn view(area: Rect) -> Rect {
        Rect::new(area.x, area.y + TOOLBAR_H, area.w, area.h - TOOLBAR_H - STATUS_H)
    }

    fn zoom_pct(&self, area: Rect) -> i32 {
        self.zoom.unwrap_or_else(|| {
            let v = Self::view(area).inset(20);
            (v.w * 100 / self.img.width as i32).min(v.h * 100 / self.img.height as i32).clamp(5, 100)
        })
    }

    fn image_rect(&self, area: Rect) -> Rect {
        let z = self.zoom_pct(area);
        let (w, h) = (self.img.width as i32 * z / 100, self.img.height as i32 * z / 100);
        let v = Self::view(area);
        Rect::new(v.x + (v.w - w) / 2, v.y + (v.h - h) / 2, w, h)
    }

    /// Image coordinates (fractional) of a point in the window.
    fn to_image(&self, x: i32, y: i32) -> (f32, f32) {
        let r = self.image_rect(self.area);
        (
            (x - r.x) as f32 * self.img.width as f32 / r.w.max(1) as f32,
            (y - r.y) as f32 * self.img.height as f32 / r.h.max(1) as f32,
        )
    }

    fn hits(&self, area: Rect) -> Vec<(Hit, Rect)> {
        let mut v = Vec::new();
        let y = area.y + 11;
        let mut x = area.x + 12;
        for i in 0..TOOLS.len() {
            v.push((Hit::Tool(i), Rect::new(x, y, 34, 34)));
            x += 38;
        }
        x += 14;
        v.push((Hit::Size, Rect::new(x, y + 7, 110, 20)));
        x += 150;
        v.push((Hit::FillShapes, Rect::new(x, y + 5, 40, 24)));
        x += 58;
        v.push((Hit::Current, Rect::new(x, y + 1, 32, 32)));
        x += 42;
        for i in 0..PALETTE.len() {
            let (col, row) = (i as i32 % 7, i as i32 / 7);
            v.push((Hit::Swatch(i), Rect::new(x + col * 18, y + row * 18, 15, 15)));
        }
        let r = area.right();
        v.push((Hit::Save, Rect::new(r - 84, y + 2, 72, 30)));
        v.push((Hit::Redo, Rect::new(r - 126, y + 2, 34, 30)));
        v.push((Hit::Undo, Rect::new(r - 164, y + 2, 34, 30)));
        if self.picker.is_some() {
            let p = Self::picker_rect(area);
            v.push((Hit::SvSquare, Rect::new(p.x + 12, p.y + 12, p.w - 24, 140)));
            v.push((Hit::HueBar, Rect::new(p.x + 12, p.y + 162, p.w - 24, 16)));
        }
        if self.sheet.is_some() {
            let s = Self::sheet_rect(area);
            v.push((Hit::SheetSave, Rect::new(s.right() - 104, s.bottom() - 46, 88, 30)));
            v.push((Hit::SheetCancel, Rect::new(s.right() - 200, s.bottom() - 46, 88, 30)));
        }
        v
    }

    fn picker_rect(area: Rect) -> Rect {
        let cur = Rect::new(area.x + 12 + 7 * 38 + 14 + 150 + 58, area.y + 12, 32, 32);
        Rect::new(cur.x - 40, area.y + TOOLBAR_H + 4, 220, 190)
    }

    fn sheet_rect(area: Rect) -> Rect {
        Rect::new(area.x + (area.w - 380) / 2, area.y + TOOLBAR_H + 20, 380, 150)
    }

    fn hit_at(&self, x: i32, y: i32) -> Option<Hit> {
        // Overlays first.
        let hits = self.hits(self.area);
        if self.sheet.is_some() {
            return hits
                .into_iter()
                .filter(|(h, _)| matches!(h, Hit::SheetSave | Hit::SheetCancel))
                .find(|(_, r)| r.contains(x, y))
                .map(|(h, _)| h);
        }
        if self.picker.is_some() {
            if let Some((h, _)) =
                hits.iter().find(|(h, r)| matches!(h, Hit::HueBar | Hit::SvSquare) && r.contains(x, y))
            {
                return Some(*h);
            }
        }
        hits.into_iter().find(|(_, r)| r.contains(x, y)).map(|(h, _)| h)
    }

    // ----------------------------------------------------------- painting

    fn stamp(&mut self, cx: f32, cy: f32) {
        let Some(s) = &mut self.stroke else { return };
        let (w, h) = (self.img.width as i32, self.img.height as i32);
        let r = (self.size as f32 / 2.0).max(0.6);
        let color = if self.tool == Tool::Eraser { WHITE } else { self.color };
        let (x0, x1) = (((cx - r - 1.0) as i32).max(0), ((cx + r + 1.0) as i32 + 1).min(w));
        let (y0, y1) = (((cy - r - 1.0) as i32).max(0), ((cy + r + 1.0) as i32 + 1).min(h));
        for y in y0..y1 {
            for x in x0..x1 {
                let dx = x as f32 + 0.5 - cx;
                let dy = y as f32 + 0.5 - cy;
                let d = sqrtf(dx * dx + dy * dy);
                let cov = (r + 0.5 - d).clamp(0.0, 1.0);
                if cov <= 0.0 {
                    continue;
                }
                let i = (y * w + x) as usize;
                let c = (cov * 255.0) as u8;
                if c > s.mask[i] {
                    s.mask[i] = c;
                    self.img.pixels[i] = blend(s.before[i], color, c as u32);
                }
            }
        }
    }

    fn stroke_to(&mut self, p: (f32, f32)) {
        let Some(s) = &self.stroke else { return };
        let (lx, ly) = s.last;
        let dist = sqrtf((p.0 - lx) * (p.0 - lx) + (p.1 - ly) * (p.1 - ly));
        let step = (self.size as f32 / 4.0).max(0.5);
        let n = ceilf(dist / step).max(1.0) as i32;
        for k in 1..=n {
            let t = k as f32 / n as f32;
            self.stamp(lx + (p.0 - lx) * t, ly + (p.1 - ly) * t);
        }
        if let Some(s) = &mut self.stroke {
            s.last = p;
        }
    }

    /// Redraws the shape being dragged from `start` to `end`.
    fn shape_preview(&mut self, end: (f32, f32)) {
        let Some(s) = &mut self.stroke else { return };
        let (w, h) = (self.img.width as i32, self.img.height as i32);
        // Restore what the previous preview covered.
        if let Some(d) = s.dirty.take() {
            for y in d.y.max(0)..d.bottom().min(h) {
                let a = (y * w + d.x.max(0)) as usize;
                let b = (y * w + d.right().min(w)) as usize;
                self.img.pixels[a..b].copy_from_slice(&s.before[a..b]);
            }
        }
        let (sx, sy) = (s.start.0 as i32, s.start.1 as i32);
        let (ex, ey) = (end.0 as i32, end.1 as i32);
        let pad = self.size + 4;
        let bbox = Rect::new(sx.min(ex) - pad, sy.min(ey) - pad, (ex - sx).abs() + 2 * pad, (ey - sy).abs() + 2 * pad);
        s.dirty = Some(bbox);
        let (tool, color, size, fill) = (self.tool, self.color, self.size, self.fill_shapes);
        let mut cv = Canvas::new(&mut self.img.pixels, w, h);
        cv.clip = bbox.intersect(&Rect::new(0, 0, w, h));
        match tool {
            Tool::Line => cv.line(sx, sy, ex, ey, size.max(1), color),
            Tool::Rect => {
                let r = Rect::new(sx.min(ex), sy.min(ey), (ex - sx).abs().max(1), (ey - sy).abs().max(1));
                if fill {
                    cv.fill_rect(r, color);
                } else {
                    let t = size.max(1);
                    cv.fill_rect(Rect::new(r.x - t / 2, r.y - t / 2, r.w + t, t), color);
                    cv.fill_rect(Rect::new(r.x - t / 2, r.bottom() - t / 2, r.w + t, t), color);
                    cv.fill_rect(Rect::new(r.x - t / 2, r.y - t / 2, t, r.h + t), color);
                    cv.fill_rect(Rect::new(r.right() - t / 2, r.y - t / 2, t, r.h + t), color);
                }
            }
            Tool::Ellipse => {
                let (cx, cy) = ((sx + ex) as f32 / 2.0, (sy + ey) as f32 / 2.0);
                let (rx, ry) = (((ex - sx).abs() as f32 / 2.0).max(1.0), ((ey - sy).abs() as f32 / 2.0).max(1.0));
                let half = size as f32 / 2.0;
                let clip = cv.clip;
                for y in clip.y..clip.bottom() {
                    for x in clip.x..clip.right() {
                        let (dx, dy) = (x as f32 + 0.5 - cx, y as f32 + 0.5 - cy);
                        // Approximate signed distance to the ellipse, in pixels.
                        let (ax, ay) = (dx / rx, dy / ry);
                        let k = sqrtf(ax * ax + ay * ay);
                        let (gx, gy) = (dx / (rx * rx), dy / (ry * ry));
                        let grad = sqrtf(gx * gx + gy * gy).max(1e-6);
                        let dist = (k - 1.0) * k / (grad * k.max(1e-6)).max(1e-6);
                        let cov = if fill {
                            (0.5 - dist).clamp(0.0, 1.0)
                        } else {
                            (half + 0.5 - fabsf(dist)).clamp(0.0, 1.0)
                        };
                        if cov > 0.0 {
                            let i = (y * w + x) as usize;
                            cv.buf[i] = blend(cv.buf[i], color, (cov * 255.0) as u32);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn flood_fill(&mut self, x: i32, y: i32) {
        let (w, h) = (self.img.width as i32, self.img.height as i32);
        if x < 0 || y < 0 || x >= w || y >= h {
            return;
        }
        let target = self.img.pixels[(y * w + x) as usize];
        if target == self.color {
            return;
        }
        let close = |c: u32| {
            let d = |s: u32| (((c >> s) & 0xFF) as i32 - ((target >> s) & 0xFF) as i32).abs();
            d(16) <= 24 && d(8) <= 24 && d(0) <= 24
        };
        self.push_undo(self.img.pixels.clone());
        let mut seen = vec![false; (w * h) as usize];
        let mut stack = vec![(x, y)];
        while let Some((sx, sy)) = stack.pop() {
            let row = (sy * w) as usize;
            let mut l = sx;
            while l > 0 && !seen[row + (l - 1) as usize] && close(self.img.pixels[row + (l - 1) as usize]) {
                l -= 1;
            }
            let mut r = sx;
            while r + 1 < w && !seen[row + (r + 1) as usize] && close(self.img.pixels[row + (r + 1) as usize]) {
                r += 1;
            }
            for xx in l..=r {
                let i = row + xx as usize;
                if seen[i] {
                    continue;
                }
                seen[i] = true;
                self.img.pixels[i] = self.color;
                for ny in [sy - 1, sy + 1] {
                    if ny >= 0 && ny < h {
                        let j = (ny * w + xx) as usize;
                        if !seen[j] && close(self.img.pixels[j]) {
                            stack.push((xx, ny));
                        }
                    }
                }
            }
        }
    }

    fn begin(&mut self, x: i32, y: i32) {
        let p = self.to_image(x, y);
        match self.tool {
            Tool::Fill => self.flood_fill(p.0 as i32, p.1 as i32),
            Tool::Picker => {
                let (ix, iy) = (p.0 as i32, p.1 as i32);
                if ix >= 0 && iy >= 0 && (ix as u32) < self.img.width && (iy as u32) < self.img.height {
                    self.color = self.img.pixels[(iy as u32 * self.img.width + ix as u32) as usize];
                    self.tool = Tool::Brush;
                }
            }
            _ => {
                let before = self.img.pixels.clone();
                self.push_undo(before.clone());
                let mask = if matches!(self.tool, Tool::Brush | Tool::Eraser) {
                    vec![0u8; self.img.pixels.len()]
                } else {
                    Vec::new()
                };
                self.stroke = Some(Stroke { before, mask, last: p, start: p, dirty: None });
                if matches!(self.tool, Tool::Brush | Tool::Eraser) {
                    self.stamp(p.0, p.1);
                }
            }
        }
    }

    // ------------------------------------------------------------ drawing

    fn draw_tool_icon(cv: &mut Canvas, tool: Tool, r: Rect, c: u32) {
        let (cx, cy) = r.center();
        match tool {
            Tool::Brush => {
                cv.line(cx - 6, cy + 6, cx + 6, cy - 6, 3, c);
                cv.fill_circle(cx - 7, cy + 7, 3, c);
            }
            Tool::Eraser => {
                cv.fill_round_rect(Rect::new(cx - 8, cy - 4, 16, 9), 2, with_alpha(c, 0x60));
                cv.stroke_round_rect(Rect::new(cx - 8, cy - 4, 16, 9), 2, c);
            }
            Tool::Line => cv.line(cx - 8, cy + 7, cx + 8, cy - 7, 2, c),
            Tool::Rect => cv.stroke_round_rect(Rect::new(cx - 8, cy - 6, 16, 12), 1, c),
            Tool::Ellipse => {
                cv.fill_circle(cx, cy, 8, c);
                cv.fill_circle(cx, cy, 6, theme::current().titlebar);
            }
            Tool::Fill => {
                cv.fill_round_rect(Rect::new(cx - 7, cy - 5, 11, 11), 2, c);
                cv.fill_circle(cx + 7, cy + 5, 2, c);
            }
            Tool::Picker => {
                cv.line(cx - 6, cy + 6, cx + 3, cy - 3, 2, c);
                cv.fill_circle(cx + 5, cy - 5, 4, c);
            }
        }
    }

    fn draw_toolbar(&mut self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let bar = Rect::new(area.x, area.y, area.w, TOOLBAR_H);
        cv.fill_rect(bar, t.titlebar);
        cv.fill_rect(Rect::new(area.x, bar.bottom() - 1, area.w, 1), t.separator);
        for (h, r) in self.hits(area) {
            let hovered = self.hover == Some(h);
            match h {
                Hit::Tool(i) => {
                    let active = TOOLS[i].0 == self.tool;
                    if active {
                        cv.fill_round_rect(r, 8, with_alpha(theme::accent(), 0x30));
                    } else if hovered {
                        cv.fill_round_rect(r, 8, t.hover);
                    }
                    Self::draw_tool_icon(cv, TOOLS[i].0, r, if active { theme::accent() } else { t.text });
                }
                Hit::Size => {
                    widgets::slider(cv, r, (self.size - 1) * 1000 / 63, hovered);
                    cv.text(r.right() + 12, r.y + 15, &format!("{} px", self.size), theme::ui(12), t.text_secondary);
                }
                Hit::FillShapes => {
                    widgets::toggle(cv, r, self.fill_shapes);
                    cv.text(r.x - 2, r.bottom() + 12, "Fill", theme::ui(10), t.text_secondary);
                }
                Hit::Current => {
                    cv.fill_round_rect(r, 8, self.color);
                    cv.stroke_round_rect(r, 8, t.control_border);
                }
                Hit::Swatch(i) => {
                    cv.fill_round_rect(r, 4, PALETTE[i]);
                    cv.stroke_round_rect(
                        r,
                        4,
                        if PALETTE[i] == self.color { theme::accent() } else { t.control_border },
                    );
                }
                Hit::Undo | Hit::Redo => {
                    let enabled = if h == Hit::Undo { !self.undo.is_empty() } else { !self.redo.is_empty() };
                    if hovered && enabled {
                        cv.fill_round_rect(r, 7, t.hover);
                    }
                    let c = if enabled { t.text } else { with_alpha(t.text_secondary, 0x70) };
                    curved_arrow(cv, r.center(), h == Hit::Undo, c);
                }
                Hit::Save => button(cv, r, "Save", ButtonStyle::Primary, hovered),
                _ => {}
            }
        }
    }

    fn draw_picker(&self, cv: &mut Canvas, area: Rect) {
        let Some((hue, s, v)) = self.picker else { return };
        let t = theme::current();
        let p = Self::picker_rect(area);
        cv.shadow(p, 12, 18, 6, t.shadow);
        cv.fill_round_rect(p, 12, t.window_bg);
        cv.stroke_round_rect(p, 12, t.window_border);
        let sq = Rect::new(p.x + 12, p.y + 12, p.w - 24, 140);
        for y in sq.y..sq.bottom() {
            for x in sq.x..sq.right() {
                let ss = (x - sq.x) as f32 / (sq.w - 1) as f32;
                let vv = 1.0 - (y - sq.y) as f32 / (sq.h - 1) as f32;
                if cv.clip.contains(x, y) {
                    cv.buf[(y * cv.width + x) as usize] = hsv_to_rgb(hue, ss, vv);
                }
            }
        }
        let (mx, my) = (sq.x + (s * (sq.w - 1) as f32) as i32, sq.y + ((1.0 - v) * (sq.h - 1) as f32) as i32);
        cv.fill_circle(mx, my, 6, 0xFFFF_FFFF);
        cv.fill_circle(mx, my, 4, hsv_to_rgb(hue, s, v));
        let bar = Rect::new(p.x + 12, p.y + 162, p.w - 24, 16);
        for x in bar.x..bar.right() {
            let hh = (x - bar.x) as f32 * 360.0 / bar.w as f32;
            cv.fill_rect(Rect::new(x, bar.y, 1, bar.h), hsv_to_rgb(hh, 1.0, 1.0));
        }
        let hx = bar.x + (hue / 360.0 * bar.w as f32) as i32;
        cv.fill_round_rect(Rect::new(hx - 3, bar.y - 2, 6, bar.h + 4), 3, 0xFFFF_FFFF);
        cv.stroke_round_rect(Rect::new(hx - 3, bar.y - 2, 6, bar.h + 4), 3, 0x60000000);
    }

    fn pick_at(&mut self, h: Hit, x: i32, y: i32) {
        let Some((hue, s, v)) = self.picker else { return };
        let Some(r) = self.hits(self.area).into_iter().find(|(k, _)| *k == h).map(|(_, r)| r) else { return };
        let (nh, ns, nv) = match h {
            Hit::HueBar => (((x - r.x) as f32 / r.w as f32 * 360.0).clamp(0.0, 359.9), s, v),
            _ => (
                hue,
                ((x - r.x) as f32 / (r.w - 1) as f32).clamp(0.0, 1.0),
                (1.0 - (y - r.y) as f32 / (r.h - 1) as f32).clamp(0.0, 1.0),
            ),
        };
        self.picker = Some((nh, ns, nv));
        self.color = hsv_to_rgb(nh, ns, nv);
        self.sliding = Some(h);
    }
}

/// ↶ / ↷: a half circle with an arrowhead at its start.
fn curved_arrow(cv: &mut Canvas, (cx, cy): (i32, i32), left: bool, c: u32) {
    let d = if left { 1 } else { -1 };
    // Upper half circle, radius 6, from the far side over the top to the arrow side.
    let pts: Vec<(i32, i32)> = (0..=12)
        .map(|k| {
            let a = k as f32 * core::f32::consts::PI / 12.0;
            let (x, y) = (libm::cosf(a), libm::sinf(a));
            (cx + d * (x * 6.0) as i32, cy + 3 - (y * 6.0) as i32)
        })
        .collect();
    cv.polyline(&pts, 2, c);
    let (ex, ey) = *pts.last().unwrap();
    cv.line(ex, ey + 1, ex - 3 * d, ey - 3, 2, c);
    cv.line(ex, ey + 1, ex + 3 * d, ey - 3, 2, c);
}

impl App for Paint {
    fn title(&self) -> String {
        format!("{}{} — Paint", self.name(), if self.dirty { " •" } else { "" })
    }

    fn size(&self) -> (i32, i32) {
        (1040, 700)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        self.area = area;
        let t = theme::current();
        let view = Self::view(area);
        cv.fill_rect(view, if t.dark { 0xFF1A_1A20 } else { 0xFFDD_DDE3 });
        let r = self.image_rect(area);
        cv.shadow(r, 2, 16, 4, t.shadow / 2);
        cv.with_clip(view, |cv| {
            if r.w as u32 >= self.img.width {
                // Nearest neighbour at 100% and above: crisp pixels.
                let clip = cv.clip.intersect(&r);
                let (iw, ih) = (self.img.width as i32, self.img.height as i32);
                for y in clip.y..clip.bottom() {
                    let iy = ((y - r.y) * ih / r.h).min(ih - 1);
                    for x in clip.x..clip.right() {
                        let ix = ((x - r.x) * iw / r.w).min(iw - 1);
                        cv.buf[(y * cv.width + x) as usize] = self.img.pixels[(iy * iw + ix) as usize];
                    }
                }
            } else {
                cv.draw_image(&self.img.pixels, self.img.width, self.img.height, r, 255);
            }
            // Brush outline under the pointer.
            if let (Some((x, y)), true) = (self.cursor, matches!(self.tool, Tool::Brush | Tool::Eraser)) {
                let rad = (self.size * r.w / self.img.width as i32 / 2).max(2);
                cv.fill_circle(x, y, rad + 1, 0x60FF_FFFF);
                cv.fill_circle(x, y, rad, with_alpha(if self.tool == Tool::Eraser { WHITE } else { self.color }, 0x90));
            }
        });
        self.draw_toolbar(cv, area);
        // Status bar.
        let sb = Rect::new(area.x, area.bottom() - STATUS_H, area.w, STATUS_H);
        cv.fill_rect(sb, t.titlebar);
        cv.fill_rect(Rect::new(sb.x, sb.y, sb.w, 1), t.separator);
        let pos = self
            .cursor
            .map(|(x, y)| self.to_image(x, y))
            .filter(|p| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < self.img.width as f32 && p.1 < self.img.height as f32);
        let mut info = format!("{} × {} px   ·   {}%", self.img.width, self.img.height, self.zoom_pct(area));
        if let Some(p) = pos {
            info.push_str(&format!("   ·   {}, {}", p.0 as i32, p.1 as i32));
        }
        cv.text(sb.x + 14, sb.y + 18, &info, theme::ui(12), t.text_secondary);
        let tip = TOOLS.iter().find(|(k, _)| *k == self.tool).map(|(_, s)| *s).unwrap_or("");
        let f = theme::ui(12);
        let msg = match &self.status {
            Some((m, at)) if env.now_ms < at + 3000 => m.clone(),
            _ => String::from(tip),
        };
        cv.text(sb.right() - 14 - f.width(&msg), sb.y + 18, &msg, f, t.text_secondary);
        self.draw_picker(cv, area);
        if let Some(field) = &mut self.sheet {
            cv.fill_rect(area, with_alpha(0x000000, 0x30));
            let s = Self::sheet_rect(area);
            cv.shadow(s, 12, 20, 6, t.shadow);
            cv.fill_round_rect(s, 12, t.window_bg);
            cv.stroke_round_rect(s, 12, t.window_border);
            cv.text(s.x + 18, s.y + 30, "Save to Pictures as", theme::ui_bold(15), t.text);
            field.draw(cv, Rect::new(s.x + 18, s.y + 44, s.w - 36, 34), env.focused);
        }
        if self.sheet.is_some() {
            for (h, r) in self.hits(area) {
                match h {
                    Hit::SheetSave => button(cv, r, "Save", ButtonStyle::Primary, self.hover == Some(h)),
                    Hit::SheetCancel => button(cv, r, "Cancel", ButtonStyle::Secondary, self.hover == Some(h)),
                    _ => {}
                }
            }
        }
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        if let Some(f) = &mut self.sheet {
            if f.click(x, y, env.mods.shift, env.now_ms) {
                return true;
            }
        }
        match self.hit_at(x, y) {
            Some(Hit::Tool(i)) => self.tool = TOOLS[i].0,
            Some(Hit::Size) => {
                self.sliding = Some(Hit::Size);
                self.drag(x, y, area, env);
            }
            Some(Hit::FillShapes) => self.fill_shapes = !self.fill_shapes,
            Some(Hit::Swatch(i)) => {
                self.color = PALETTE[i];
                self.picker = None;
            }
            Some(Hit::Current) => {
                self.picker = if self.picker.is_some() { None } else { Some(rgb_to_hsv(self.color)) };
            }
            Some(Hit::Undo) => self.undo(),
            Some(Hit::Redo) => self.redo(),
            Some(Hit::Save) => self.save(),
            Some(h @ (Hit::HueBar | Hit::SvSquare)) => self.pick_at(h, x, y),
            Some(Hit::SheetSave) => {
                if let Some(f) = self.sheet.take() {
                    let mut name = String::from(f.text().trim());
                    if name.is_empty() {
                        name = String::from("Untitled.png");
                    }
                    if !name.to_lowercase().ends_with(".png") {
                        name.push_str(".png");
                    }
                    let _ = fs::mkdir("/Pictures");
                    self.save_to(fs::join("/Pictures", &name));
                }
            }
            Some(Hit::SheetCancel) => self.sheet = None,
            None => {
                if self.sheet.is_some() {
                    return false;
                }
                if self.picker.take().is_some() {
                    return true;
                }
                if Self::view(area).contains(x, y) {
                    self.begin(x, y);
                }
            }
        }
        true
    }

    fn drag(&mut self, x: i32, y: i32, _area: Rect, _env: &mut Env) -> bool {
        self.cursor = Some((x, y));
        match self.sliding {
            Some(Hit::Size) => {
                if let Some(r) = self.hits(self.area).into_iter().find(|(h, _)| *h == Hit::Size).map(|(_, r)| r) {
                    self.size = 1 + widgets::slider_value(r, x) * 63 / 1000;
                }
                return true;
            }
            Some(h @ (Hit::HueBar | Hit::SvSquare)) => {
                self.pick_at(h, x, y);
                return true;
            }
            _ => {}
        }
        if let Some(sheet) = &mut self.sheet {
            return sheet.drag(x);
        }
        if self.stroke.is_none() {
            return false;
        }
        let p = self.to_image(x, y);
        match self.tool {
            Tool::Brush | Tool::Eraser => self.stroke_to(p),
            Tool::Line | Tool::Rect | Tool::Ellipse => self.shape_preview(p),
            _ => {}
        }
        true
    }

    fn release(&mut self, _x: i32, _y: i32, _area: Rect, _env: &mut Env) -> bool {
        self.sliding = None;
        if let Some(f) = &mut self.sheet {
            f.release();
        }
        self.stroke.take().is_some()
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = if x < 0 { None } else { self.hit_at(x, y) };
        let c = (x >= 0 && Self::view(area).contains(x, y)).then_some((x, y));
        let changed = h != self.hover || c != self.cursor;
        self.hover = h;
        self.cursor = c;
        changed
    }

    fn scroll(&mut self, delta: i32, area: Rect) -> bool {
        let z = self.zoom_pct(area);
        let next = if delta < 0 { z * 5 / 4 + 1 } else { z * 4 / 5 };
        self.zoom = Some(next.clamp(5, 800));
        true
    }

    fn drop(&mut self, _x: i32, _y: i32, kind: u32, _area: Rect, _env: &mut Env) -> bool {
        if let Some(aurora::dnd::Dropped::Files(paths)) = aurora::dnd::dropped(kind) {
            if let Some(p) = paths.iter().find(|p| aurora_image::is_image_name(p)) {
                let p = p.clone();
                self.open(&p);
                return true;
            }
        }
        false
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        if let Some(f) = &mut self.sheet {
            match ev.code {
                KeyCode::Enter => {
                    let r =
                        self.hits(self.area).into_iter().find(|(h, _)| *h == Hit::SheetSave).map(|(_, r)| r).unwrap();
                    let (cx, cy) = r.center();
                    let area = self.area;
                    return self.click(cx, cy, area, env);
                }
                KeyCode::Escape => self.sheet = None,
                _ => {
                    f.key(ev, env.now_ms);
                }
            }
            return true;
        }
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        match (ctrl, ev.ch.map(|c| c.to_ascii_lowercase())) {
            (true, Some('z')) if ev.mods.shift => self.redo(),
            (true, Some('z')) => self.undo(),
            (true, Some('y')) => self.redo(),
            (true, Some('s')) => self.save(),
            (true, Some('0')) => self.zoom = None,
            (true, Some('1')) => self.zoom = Some(100),
            (false, Some('b')) => self.tool = Tool::Brush,
            (false, Some('e')) => self.tool = Tool::Eraser,
            (false, Some('l')) => self.tool = Tool::Line,
            (false, Some('r')) => self.tool = Tool::Rect,
            (false, Some('o')) => self.tool = Tool::Ellipse,
            (false, Some('f')) => self.tool = Tool::Fill,
            (false, Some('i')) => self.tool = Tool::Picker,
            (false, Some('[')) => self.size = (self.size - 1).max(1),
            (false, Some(']')) => self.size = (self.size + 1).min(64),
            _ => return false,
        }
        true
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        if let Some(f) = &mut self.sheet {
            return f.tick(env.now_ms, env.focused);
        }
        matches!(&self.status, Some((_, at)) if env.now_ms >= at + 3000 && env.now_ms < at + 3200)
    }
}

aurora::entry!(main);

fn main(args: aurora::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    ripple::run(Paint::new(path))
}
