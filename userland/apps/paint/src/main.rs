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
use aurora_image::Image as Picture;
use corekit::fs;
use waveui::lumen::canvas::{blend, rgb, with_alpha, Canvas as Surface};
use waveui::lumen::geom::Rect;
use waveui::lumen::theme;
use waveui::prelude::*;

use libm::{ceilf, fabsf, sqrtf};

const UNDO: usize = 12;
const WHITE: u32 = 0xFFFF_FFFF;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
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
    img: Picture,
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
    picker: Option<(f32, f32, f32)>,
    /// The Save sheet's file name, while it is open.
    sheet: Option<String>,
    /// The pointer over the picture area (relative to it).
    cursor: Option<(i32, i32)>,
    status: Option<String>,
    /// The size of the picture area.
    view: (i32, i32),
}

#[derive(Clone)]
pub enum Msg {
    Tool(usize),
    Size(f32),
    FillShapes(bool),
    Swatch(usize),
    TogglePicker,
    Hue(i32, i32),
    Shade(i32, i32),
    Undo,
    Redo,
    Save,
    SheetName(String),
    SheetSave,
    SheetCancel,
    Begin(i32, i32),
    Move(i32, i32),
    End,
    Cursor(Option<(i32, i32)>),
    Zoom(i32),
    ZoomTo(Option<i32>),
    Resize(i32, i32),
    Open(String),
    ClearStatus,
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
            img: Picture::new(1200, 800, WHITE),
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
            picker: None,
            sheet: None,
            cursor: None,
            status: None,
            view: (1000, 600),
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
        self.status = Some(msg);
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
            _ => self.sheet = Some(String::from("Untitled.png")),
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

    fn zoom_pct(&self) -> i32 {
        self.zoom.unwrap_or_else(|| {
            let v = Rect::new(0, 0, self.view.0, self.view.1).inset(20);
            (v.w * 100 / self.img.width as i32).min(v.h * 100 / self.img.height as i32).clamp(5, 100)
        })
    }

    /// Where the picture sits in the picture area.
    fn image_rect(&self) -> Rect {
        let z = self.zoom_pct();
        let (w, h) = (self.img.width as i32 * z / 100, self.img.height as i32 * z / 100);
        Rect::new((self.view.0 - w) / 2, (self.view.1 - h) / 2, w, h)
    }

    /// Image coordinates (fractional) of a point in the picture area.
    fn to_image(&self, x: i32, y: i32) -> (f32, f32) {
        let r = self.image_rect();
        (
            (x - r.x) as f32 * self.img.width as f32 / r.w.max(1) as f32,
            (y - r.y) as f32 * self.img.height as f32 / r.h.max(1) as f32,
        )
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
        let mut cv = Surface::new(&mut self.img.pixels, w, h);
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

    fn draw_tool_icon(cv: &mut Surface, tool: Tool, r: Rect, c: u32) {
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
                cv.stroke_round_rect(Rect::new(cx - 8, cy - 7, 16, 14), 7, c);
                cv.stroke_round_rect(Rect::new(cx - 7, cy - 6, 14, 12), 6, c);
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

    fn paint_picture(&self, cv: &mut Surface, view: Rect, dark: bool) {
        cv.fill_rect(view, if dark { 0xFF1A_1A20 } else { 0xFFDD_DDE3 });
        let r = self.image_rect().offset(view.x, view.y);
        cv.shadow(r, 2, 16, 4, theme::current().shadow / 2);
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
            let (x, y) = (view.x + x, view.y + y);
            cv.fill_circle(x, y, rad + 1, 0x60FF_FFFF);
            cv.fill_circle(x, y, rad, with_alpha(if self.tool == Tool::Eraser { WHITE } else { self.color }, 0x90));
        }
    }

    fn picker_view(&self) -> Option<View<'_, Msg>> {
        let (hue, s, v) = self.picker?;
        let square = Canvas::new(move |cv, sq| {
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
        })
        .on_input(|e| match e {
            CanvasEvent::Press { x, y, .. } | CanvasEvent::Drag { x, y } => Some(Msg::Shade(x, y)),
            _ => None,
        })
        .size(196, 140)
        .corner_radius(8)
        .clipped();
        let bar = Canvas::new(move |cv, bar| {
            for x in bar.x..bar.right() {
                let hh = (x - bar.x) as f32 * 360.0 / bar.w as f32;
                cv.fill_rect(Rect::new(x, bar.y, 1, bar.h), hsv_to_rgb(hh, 1.0, 1.0));
            }
            let hx = bar.x + (hue / 360.0 * bar.w as f32) as i32;
            cv.fill_round_rect(Rect::new(hx - 3, bar.y - 2, 6, bar.h + 4), 3, 0xFFFF_FFFF);
            cv.stroke_round_rect(Rect::new(hx - 3, bar.y - 2, 6, bar.h + 4), 3, 0x6000_0000);
        })
        .on_input(|e| match e {
            CanvasEvent::Press { x, y, .. } | CanvasEvent::Drag { x, y } => Some(Msg::Hue(x, y)),
            _ => None,
        })
        .size(196, 20);
        Some(
            VStack::new()
                .push(square)
                .push(bar)
                .spacing(10)
                .padding(12)
                .background_rounded(Color::Background, 12)
                .border(Color::Separator, 12)
                .shadow(),
        )
    }

    fn toolbar(&self) -> View<'_, Msg> {
        let accent = theme::accent();
        let mut tools = HStack::new().spacing(4);
        for (i, (tool, tip)) in TOOLS.iter().enumerate() {
            let (tool, active) = (*tool, *tool == self.tool);
            let icon = Canvas::new(move |cv, r| {
                let c = if active { accent } else { theme::current().text };
                Self::draw_tool_icon(cv, tool, r, c)
            })
            .size(20, 20);
            let mut b = Button::with(icon).action(Msg::Tool(i)).button_style(ButtonStyle::Toolbar).help(*tip);
            if active {
                b = b.background_rounded(Color::Custom(with_alpha(accent, 0x30)), 8);
            }
            tools = tools.push(b);
        }
        let color = self.color;
        let current = Canvas::new(move |cv, r| {
            cv.fill_round_rect(r, 8, color);
            cv.stroke_round_rect(r, 8, theme::current().control_border);
        })
        .size(30, 30)
        .on_tap(Msg::TogglePicker)
        .help("More colours");
        let mut swatches = Grid::new(15).spacing(3).width(7 * 18);
        for (i, c) in PALETTE.iter().enumerate() {
            let (c, on) = (*c, *c == self.color);
            swatches = swatches.push(
                Canvas::new(move |cv, r| {
                    cv.fill_round_rect(r, 4, c);
                    cv.stroke_round_rect(r, 4, if on { theme::accent() } else { theme::current().control_border });
                })
                .size(15, 15)
                .on_tap(Msg::Swatch(i)),
            );
        }
        toolbar(vec![
            tools,
            Divider::new().height(28),
            Slider::new(self.size as f32, 1.0..=64.0).step(1.0).on_change(Msg::Size).width(120).help("Brush size"),
            Text::new(format!("{} px", self.size)).style(TextStyle::Callout).foreground(Color::SecondaryLabel).width(44),
            Toggle::checkbox("Fill shapes", self.fill_shapes).on_change(Msg::FillShapes),
            Divider::new().height(28),
            current,
            swatches,
            Spacer::new(),
            Button::symbol(Symbol::Undo).action(Msg::Undo).disabled(self.undo.is_empty()).help("Undo (Ctrl+Z)"),
            Button::symbol(Symbol::Redo).action(Msg::Redo).disabled(self.redo.is_empty()).help("Redo (Ctrl+Shift+Z)"),
            Button::new("Save").action(Msg::Save).button_style(ButtonStyle::Prominent).shortcut(Shortcut::ctrl('s')),
        ])
    }

    fn pick(&mut self, hue: Option<f32>, sv: Option<(f32, f32)>) {
        let Some((h, s, v)) = self.picker else { return };
        let h = hue.unwrap_or(h);
        let (s, v) = sv.unwrap_or((s, v));
        self.picker = Some((h, s, v));
        self.color = hsv_to_rgb(h, s, v);
    }
}

impl App for Paint {
    type Message = Msg;

    fn title(&self) -> String {
        format!("{}{} — Paint", self.name(), if self.dirty { " •" } else { "" })
    }

    fn size(&self) -> (i32, i32) {
        (1040, 700)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Tool(i) => self.tool = TOOLS[i].0,
            Msg::Size(v) => self.size = (v as i32).clamp(1, 64),
            Msg::FillShapes(on) => self.fill_shapes = on,
            Msg::Swatch(i) => {
                self.color = PALETTE[i];
                self.picker = None;
            }
            Msg::TogglePicker => {
                self.picker = if self.picker.is_some() { None } else { Some(rgb_to_hsv(self.color)) };
            }
            Msg::Hue(x, _) => self.pick(Some((x as f32 / 196.0 * 360.0).clamp(0.0, 359.9)), None),
            Msg::Shade(x, y) => {
                self.pick(None, Some(((x as f32 / 195.0).clamp(0.0, 1.0), (1.0 - y as f32 / 139.0).clamp(0.0, 1.0))))
            }
            Msg::Undo => self.undo(),
            Msg::Redo => self.redo(),
            Msg::Save => {
                self.save();
                if self.sheet.is_some() {
                    cx.focus("save-name");
                }
            }
            Msg::SheetName(n) => self.sheet = Some(n),
            Msg::SheetSave => {
                if let Some(name) = self.sheet.take() {
                    let mut name = String::from(name.trim());
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
            Msg::SheetCancel => self.sheet = None,
            Msg::Begin(x, y) => {
                if self.picker.take().is_none() {
                    self.begin(x, y);
                }
            }
            Msg::Move(x, y) => {
                self.cursor = Some((x, y));
                if self.stroke.is_some() {
                    let p = self.to_image(x, y);
                    match self.tool {
                        Tool::Brush | Tool::Eraser => self.stroke_to(p),
                        Tool::Line | Tool::Rect | Tool::Ellipse => self.shape_preview(p),
                        _ => {}
                    }
                }
            }
            Msg::End => self.stroke = None,
            Msg::Cursor(c) => self.cursor = c,
            Msg::Zoom(d) => {
                let z = self.zoom_pct();
                let next = if d < 0 { z * 5 / 4 + 1 } else { z * 4 / 5 };
                self.zoom = Some(next.clamp(5, 800));
            }
            Msg::ZoomTo(z) => self.zoom = z,
            Msg::Resize(w, h) => self.view = (w, h),
            Msg::Open(p) => self.open(&p),
            Msg::ClearStatus => self.status = None,
        }
        if self.status.is_some() {
            cx.cancel(|m| matches!(m, Msg::ClearStatus));
            cx.after(3000, Msg::ClearStatus);
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        let tool = |t: Tool| TOOLS.iter().position(|x| x.0 == t).map(Msg::Tool);
        match (ctrl, ev.ch.map(|c| c.to_ascii_lowercase())) {
            (true, Some('z')) if ev.mods.shift => Some(Msg::Redo),
            (true, Some('z')) => Some(Msg::Undo),
            (true, Some('y')) => Some(Msg::Redo),
            (true, Some('0')) => Some(Msg::ZoomTo(None)),
            (true, Some('1')) => Some(Msg::ZoomTo(Some(100))),
            (false, Some('b')) => tool(Tool::Brush),
            (false, Some('e')) => tool(Tool::Eraser),
            (false, Some('l')) => tool(Tool::Line),
            (false, Some('r')) => tool(Tool::Rect),
            (false, Some('o')) => tool(Tool::Ellipse),
            (false, Some('f')) => tool(Tool::Fill),
            (false, Some('i')) => tool(Tool::Picker),
            (false, Some('[')) => Some(Msg::Size((self.size - 1) as f32)),
            (false, Some(']')) => Some(Msg::Size((self.size + 1) as f32)),
            _ => None,
        }
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        let name = self.sheet.as_ref()?;
        Some(
            VStack::new()
                .push(Text::new("Save to Pictures as").style(TextStyle::Title2))
                .push(
                    TextField::new("Name", name.clone())
                        .on_change(Msg::SheetName)
                        .on_submit(Msg::SheetSave)
                        .id("save-name")
                        .focused(),
                )
                .push(
                    HStack::new()
                        .push(Spacer::new())
                        .push(Button::new("Cancel").action(Msg::SheetCancel).shortcut(Shortcut::cancel()).min_width(84))
                        .push(
                            Button::new("Save")
                                .action(Msg::SheetSave)
                                .button_style(ButtonStyle::Prominent)
                                .shortcut(Shortcut::default_action())
                                .min_width(84),
                        )
                        .spacing(8),
                )
                .spacing(14)
                .alignment(Align::Stretch)
                .width(360),
        )
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let dark = env.dark;
        let picture = Canvas::new(move |cv, r| self.paint_picture(cv, r, dark))
            .on_input(|e| match e {
                CanvasEvent::Press { x, y, button: 1, .. } => Some(Msg::Begin(x, y)),
                CanvasEvent::Drag { x, y } => Some(Msg::Move(x, y)),
                CanvasEvent::Release { .. } => Some(Msg::End),
                CanvasEvent::Hover { x, y } => Some(Msg::Cursor(Some((x, y)))),
                CanvasEvent::Leave => Some(Msg::Cursor(None)),
                CanvasEvent::Scroll { dy, .. } if dy != 0 => Some(Msg::Zoom(dy)),
                CanvasEvent::Resize { w, h } => Some(Msg::Resize(w, h)),
                _ => None,
            })
            .on_drop(|d| match d.items {
                corekit::dnd::Dropped::Files(paths) => {
                    paths.into_iter().find(|p| aurora_image::is_image_name(p)).map(Msg::Open)
                }
                _ => None,
            })
            .fill();
        let pos = self
            .cursor
            .map(|(x, y)| self.to_image(x, y))
            .filter(|p| p.0 >= 0.0 && p.1 >= 0.0 && p.0 < self.img.width as f32 && p.1 < self.img.height as f32);
        let mut info = format!("{} × {} px   ·   {}%", self.img.width, self.img.height, self.zoom_pct());
        if let Some(p) = pos {
            info.push_str(&format!("   ·   {}, {}", p.0 as i32, p.1 as i32));
        }
        let tip = TOOLS.iter().find(|(k, _)| *k == self.tool).map(|(_, s)| *s).unwrap_or("");
        let status = HStack::new()
            .push(Text::new(info).style(TextStyle::Footnote).foreground(Color::SecondaryLabel).grow(1))
            .push(
                Text::new(self.status.clone().unwrap_or_else(|| String::from(tip)))
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel),
            )
            .padding_xy(14, 0)
            .height(26)
            .fill_width();
        let overlay = self.picker_view().map(|p| {
            VStack::new()
                .push(HStack::new().push(p).padding_insets(Insets { top: 4, leading: 370, ..Default::default() }))
                .push(Spacer::new())
                .alignment(Align::Start)
                .fill()
        });
        VStack::new()
            .push(self.toolbar())
            .push(Divider::new())
            .push(ZStack::with(vec![picture]).push_if(overlay).fill())
            .push(Divider::new())
            .push(status)
            .spacing(0)
            .fill()
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    waveui::run(Paint::new(path))
}
