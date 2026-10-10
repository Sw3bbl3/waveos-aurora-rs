//! The 2D canvas's drawing model, without script bindings: paths kept in
//! device space, filled and stroked with exact-area coverage (Lyra's
//! rasterizer), Porter–Duff compositing on straight-alpha pixels,
//! gradients, clipping, and transformed images and text masks.
//!
//! Strokes are outlined in device space with the line width scaled by the
//! transform's average scale, so a skewed or unevenly scaled transform
//! strokes with an even width.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use libm::{acosf, ceilf, cosf, floorf, fmodf, sinf, sqrtf};
use lumen::ttf::raster::Coverage;

/// Points a path may hold, to bound memory.
pub const MAX_POINTS: usize = 1 << 17;
/// How far (device px) a flattened curve may stray from the true one.
const TOLERANCE: f32 = 0.2;
const TAU: f32 = core::f32::consts::TAU;
const PI: f32 = core::f32::consts::PI;
/// A 2D affine transform [a, b, c, d, e, f], as in `setTransform`.
pub type Matrix = [f32; 6];
pub const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cap {
    Butt,
    Round,
    Square,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Join {
    Miter,
    Round,
    Bevel,
}

/// The compositing operators drawn exactly; others are ignored when set.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Op {
    SourceOver,
    DestinationOver,
    DestinationOut,
    SourceAtop,
    Lighter,
    Xor,
    Copy,
}

impl Op {
    pub const ALL: [(&'static str, Op); 7] = [
        ("source-over", Op::SourceOver),
        ("destination-over", Op::DestinationOver),
        ("destination-out", Op::DestinationOut),
        ("source-atop", Op::SourceAtop),
        ("lighter", Op::Lighter),
        ("xor", Op::Xor),
        ("copy", Op::Copy),
    ];
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    Start,
    End,
    Left,
    Right,
    Center,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Baseline {
    Alphabetic,
    Top,
    Hanging,
    Middle,
    Ideographic,
    Bottom,
}

/// What a fill or stroke paints with: a colour (0xAARRGGBB, straight
/// alpha) or a gradient, by index into the page's gradients.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Paint {
    Color(u32),
    Gradient(usize),
}

#[derive(Clone, Debug)]
pub enum GradientKind {
    Linear(f32, f32, f32, f32),
    Radial(f32, f32, f32, f32, f32, f32),
}

/// A gradient in the user space it's drawn in, with sorted colour stops.
#[derive(Clone, Debug)]
pub struct Gradient {
    pub kind: GradientKind,
    pub stops: Vec<(f32, u32)>,
}

impl Gradient {
    pub fn add_stop(&mut self, offset: f32, color: u32) {
        // Equal offsets keep insertion order (a hard edge).
        let at = self.stops.iter().position(|(o, _)| *o > offset).unwrap_or(self.stops.len());
        self.stops.insert(at, (offset, color));
    }

    /// The gradient's position at a user-space point (None: nothing drawn).
    fn position(&self, x: f32, y: f32) -> Option<f32> {
        match self.kind {
            GradientKind::Linear(x0, y0, x1, y1) => {
                let (dx, dy) = (x1 - x0, y1 - y0);
                let len = dx * dx + dy * dy;
                if len == 0.0 {
                    return None;
                }
                Some(((x - x0) * dx + (y - y0) * dy) / len)
            }
            GradientKind::Radial(x0, y0, r0, x1, y1, r1) => {
                // The largest t with |p − c(t)| = r(t) ≥ 0 (two-point conical).
                let (cdx, cdy, dr) = (x1 - x0, y1 - y0, r1 - r0);
                let (pdx, pdy) = (x - x0, y - y0);
                let a = cdx * cdx + cdy * cdy - dr * dr;
                let b = pdx * cdx + pdy * cdy + r0 * dr;
                let c = pdx * pdx + pdy * pdy - r0 * r0;
                if a.abs() < 1e-6 {
                    if b == 0.0 {
                        return None;
                    }
                    let t = c / (2.0 * b);
                    return (r0 + t * dr >= 0.0).then_some(t);
                }
                let disc = b * b - a * c;
                if disc < 0.0 {
                    return None;
                }
                let s = sqrtf(disc);
                let (t1, t2) = ((b + s) / a, (b - s) / a);
                let (hi, lo) = if t1 > t2 { (t1, t2) } else { (t2, t1) };
                [hi, lo].into_iter().find(|t| r0 + t * dr >= 0.0)
            }
        }
    }

    /// The colour at a user-space point, interpolated in premultiplied space.
    pub fn color_at_user(&self, x: f32, y: f32) -> u32 {
        let (Some(t), Some(first), Some(last)) = (self.position(x, y), self.stops.first(), self.stops.last()) else {
            return 0;
        };
        let t = t.clamp(0.0, 1.0);
        if t <= first.0 {
            return first.1;
        }
        if t >= last.0 {
            return last.1;
        }
        let i = self.stops.iter().position(|(o, _)| *o > t).unwrap_or(self.stops.len() - 1);
        let ((o0, c0), (o1, c1)) = (self.stops[i - 1], self.stops[i]);
        let f = if o1 > o0 { (t - o0) / (o1 - o0) } else { 1.0 };
        let p0 = premultiply(c0);
        let p1 = premultiply(c1);
        let mix: [f32; 4] = core::array::from_fn(|k| p0[k] + (p1[k] - p0[k]) * f);
        unpremultiply(mix)
    }
}

fn premultiply(c: u32) -> [f32; 4] {
    let a = (c >> 24) as f32 / 255.0;
    [((c >> 16) & 255) as f32 / 255.0 * a, ((c >> 8) & 255) as f32 / 255.0 * a, (c & 255) as f32 / 255.0 * a, a]
}

fn unpremultiply(p: [f32; 4]) -> u32 {
    let a = p[3].clamp(0.0, 1.0);
    if a <= 0.0 {
        return 0;
    }
    let ch = |v: f32| ((v / a).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    ((a * 255.0 + 0.5) as u32) << 24 | ch(p[0]) << 16 | ch(p[1]) << 8 | ch(p[2])
}

/// Everything `save()` keeps.
#[derive(Clone, Debug)]
pub struct State {
    pub fill: Paint,
    pub stroke: Paint,
    pub line_width: f32,
    pub cap: Cap,
    pub join: Join,
    pub miter_limit: f32,
    pub dash: Vec<f32>,
    pub dash_offset: f32,
    pub alpha: f32,
    pub op: Op,
    pub matrix: Matrix,
    /// The `font` value as set, and what was understood from it.
    pub font: String,
    pub font_px: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub align: TextAlign,
    pub baseline: Baseline,
    pub smoothing: bool,
    /// Coverage (0–255) per canvas pixel; None = no clip.
    pub clip: Option<Arc<Vec<u8>>>,
}

impl Default for State {
    fn default() -> Self {
        State {
            fill: Paint::Color(0xFF00_0000),
            stroke: Paint::Color(0xFF00_0000),
            line_width: 1.0,
            cap: Cap::Butt,
            join: Join::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            alpha: 1.0,
            op: Op::SourceOver,
            matrix: IDENTITY,
            font: String::from("10px sans-serif"),
            font_px: 10.0,
            bold: false,
            italic: false,
            mono: false,
            align: TextAlign::Start,
            baseline: Baseline::Alphabetic,
            smoothing: true,
            clip: None,
        }
    }
}

impl State {
    /// Applies a CSS `font` shorthand ("italic bold 16px monospace"); false
    /// if it has no size (and so is ignored, as in browsers).
    pub fn set_font(&mut self, value: &str) -> bool {
        let mut size = None;
        let (mut bold, mut italic) = (false, false);
        let mut family = String::new();
        for (i, word) in value.split_ascii_whitespace().enumerate() {
            let lower = word.to_ascii_lowercase();
            if size.is_some() {
                family.push_str(&lower);
                family.push(' ');
                continue;
            }
            let size_part = lower.split('/').next().unwrap_or("");
            let px = |n: &str, unit: f32| n.parse::<f32>().ok().map(|v| v * unit);
            let parsed = if let Some(n) = size_part.strip_suffix("px") {
                px(n, 1.0)
            } else if let Some(n) = size_part.strip_suffix("pt") {
                px(n, 4.0 / 3.0)
            } else if let Some(n) = size_part.strip_suffix("rem").or_else(|| size_part.strip_suffix("em")) {
                px(n, 16.0)
            } else {
                None
            };
            match parsed {
                Some(v) if v > 0.0 && v.is_finite() => size = Some(v),
                _ => match lower.as_str() {
                    "bold" | "bolder" | "600" | "700" | "800" | "900" => bold = true,
                    "italic" | "oblique" => italic = true,
                    _ if i > 4 => return false,
                    _ => {}
                },
            }
        }
        let Some(px) = size else { return false };
        self.font = String::from(value.trim());
        self.font_px = px;
        self.bold = bold;
        self.italic = italic;
        self.mono = family.contains("mono") || family.contains("courier") || family.contains("consol");
        true
    }
}

/// One subpath: device-space points, and whether it was closed.
#[derive(Clone, Debug, Default)]
pub struct Subpath {
    pub points: Vec<(f32, f32)>,
    pub closed: bool,
}

/// The current path, already transformed to device space.
#[derive(Clone, Debug, Default)]
pub struct Path {
    pub subpaths: Vec<Subpath>,
    count: usize,
}

pub fn apply(m: &Matrix, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

pub fn invert(m: &Matrix) -> Option<Matrix> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det == 0.0 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(a * m[4] + c * m[5]), -(b * m[4] + d * m[5])])
}

pub fn multiply(m: &Matrix, n: &Matrix) -> Matrix {
    [
        m[0] * n[0] + m[2] * n[1],
        m[1] * n[0] + m[3] * n[1],
        m[0] * n[2] + m[2] * n[3],
        m[1] * n[2] + m[3] * n[3],
        m[0] * n[4] + m[2] * n[5] + m[4],
        m[1] * n[4] + m[3] * n[5] + m[5],
    ]
}

/// How much the transform enlarges lengths (on average).
fn scale_of(m: &Matrix) -> f32 {
    sqrtf((m[0] * m[3] - m[1] * m[2]).abs())
}

/// The longest a unit vector can become.
fn max_scale(m: &Matrix) -> f32 {
    sqrtf((m[0] * m[0] + m[1] * m[1]).max(m[2] * m[2] + m[3] * m[3]))
}

fn finite(v: &[f32]) -> bool {
    v.iter().all(|x| x.is_finite())
}

impl Path {
    pub fn clear(&mut self) {
        self.subpaths.clear();
        self.count = 0;
    }

    fn push(&mut self, p: (f32, f32)) {
        if self.count >= MAX_POINTS {
            return;
        }
        self.count += 1;
        if let Some(s) = self.subpaths.last_mut() {
            s.points.push(p);
        }
    }

    fn last(&self) -> Option<(f32, f32)> {
        self.subpaths.last().and_then(|s| s.points.last().copied())
    }

    pub fn move_to(&mut self, m: &Matrix, x: f32, y: f32) {
        if !finite(&[x, y]) || self.count >= MAX_POINTS {
            return;
        }
        if self.subpaths.last().is_some_and(|s| s.points.len() == 1 && !s.closed) {
            self.subpaths.pop();
            self.count -= 1;
        }
        self.subpaths.push(Subpath::default());
        self.push(apply(m, x, y));
    }

    /// Starts a subpath at (x, y) if there's none (as lineTo does).
    fn ensure(&mut self, m: &Matrix, x: f32, y: f32) -> bool {
        if self.last().is_none() {
            self.move_to(m, x, y);
            return false;
        }
        true
    }

    pub fn line_to(&mut self, m: &Matrix, x: f32, y: f32) {
        if finite(&[x, y]) && self.ensure(m, x, y) {
            self.push(apply(m, x, y));
        }
    }

    pub fn close(&mut self) {
        if let Some(s) = self.subpaths.last_mut() {
            if !s.closed && !s.points.is_empty() {
                s.closed = true;
                let start = s.points[0];
                self.subpaths.push(Subpath { points: vec![start], closed: false });
                self.count += 1;
            }
        }
    }

    pub fn quad_to(&mut self, m: &Matrix, cx: f32, cy: f32, x: f32, y: f32) {
        if !finite(&[cx, cy, x, y]) {
            return;
        }
        self.ensure(m, cx, cy);
        let p0 = self.last().unwrap();
        let (p1, p2) = (apply(m, cx, cy), apply(m, x, y));
        let dd = hypot(p0.0 - 2.0 * p1.0 + p2.0, p0.1 - 2.0 * p1.1 + p2.1);
        let n = (ceilf(sqrtf(dd / (4.0 * TOLERANCE))) as usize).clamp(1, 64);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let (a, b, c) = (mt * mt, 2.0 * mt * t, t * t);
            self.push((a * p0.0 + b * p1.0 + c * p2.0, a * p0.1 + b * p1.1 + c * p2.1));
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn cubic_to(&mut self, m: &Matrix, c1x: f32, c1y: f32, c2x: f32, c2y: f32, x: f32, y: f32) {
        if !finite(&[c1x, c1y, c2x, c2y, x, y]) {
            return;
        }
        self.ensure(m, c1x, c1y);
        let p0 = self.last().unwrap();
        let (p1, p2, p3) = (apply(m, c1x, c1y), apply(m, c2x, c2y), apply(m, x, y));
        let dd = hypot(p0.0 - 2.0 * p1.0 + p2.0, p0.1 - 2.0 * p1.1 + p2.1)
            .max(hypot(p1.0 - 2.0 * p2.0 + p3.0, p1.1 - 2.0 * p2.1 + p3.1));
        let n = (ceilf(sqrtf(0.75 * dd / TOLERANCE)) as usize).clamp(1, 100);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let mt = 1.0 - t;
            let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            self.push((a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0, a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1));
        }
    }

    /// `ellipse()` (and `arc()`, with equal radii and no rotation). The
    /// radii must already be checked to be non-negative.
    #[allow(clippy::too_many_arguments)]
    pub fn ellipse(
        &mut self,
        m: &Matrix,
        cx: f32,
        cy: f32,
        rx: f32,
        ry: f32,
        rotation: f32,
        start: f32,
        end: f32,
        ccw: bool,
    ) {
        if !finite(&[cx, cy, rx, ry, rotation, start, end]) {
            return;
        }
        let sweep = sweep(start, end, ccw);
        let (cr, sr) = (cosf(rotation), sinf(rotation));
        let at = |t: f32| {
            let (ct, st) = (cosf(t), sinf(t));
            apply(m, cx + rx * ct * cr - ry * st * sr, cy + rx * ct * sr + ry * st * cr)
        };
        let first = at(start);
        if self.last().is_some() {
            self.push(first);
        } else {
            self.subpaths.push(Subpath::default());
            self.push(first);
        }
        let r = rx.max(ry) * max_scale(m);
        let step = if r > TOLERANCE { 2.0 * acosf(1.0 - TOLERANCE / r) } else { PI / 2.0 };
        let n = (ceilf(sweep.abs() / step.max(0.01)) as usize).clamp(1, 1024);
        for i in 1..=n {
            self.push(at(start + sweep * i as f32 / n as f32));
        }
    }

    pub fn rect(&mut self, m: &Matrix, x: f32, y: f32, w: f32, h: f32) {
        if !finite(&[x, y, w, h]) {
            return;
        }
        self.move_to(m, x, y);
        self.line_to(m, x + w, y);
        self.line_to(m, x + w, y + h);
        self.line_to(m, x, y + h);
        self.close();
    }

    /// `roundRect()` with one radius per corner (top-left, top-right,
    /// bottom-right, bottom-left), already validated.
    pub fn round_rect(&mut self, m: &Matrix, x: f32, y: f32, w: f32, h: f32, radii: [f32; 4]) {
        if !finite(&[x, y, w, h]) {
            return;
        }
        // Normalize to a positive box, then shrink radii that don't fit.
        let (x, w) = if w < 0.0 { (x + w, -w) } else { (x, w) };
        let (y, h) = if h < 0.0 { (y + h, -h) } else { (y, h) };
        let [tl, tr, br, bl] = radii;
        let f = [w / (tl + tr), w / (bl + br), h / (tl + bl), h / (tr + br)]
            .into_iter()
            .filter(|v| v.is_finite())
            .fold(1.0f32, f32::min);
        let [tl, tr, br, bl] = [tl * f, tr * f, br * f, bl * f];
        self.move_to(m, x + tl, y);
        self.line_to(m, x + w - tr, y);
        self.ellipse(m, x + w - tr, y + tr, tr, tr, 0.0, -PI / 2.0, 0.0, false);
        self.line_to(m, x + w, y + h - br);
        self.ellipse(m, x + w - br, y + h - br, br, br, 0.0, 0.0, PI / 2.0, false);
        self.line_to(m, x + bl, y + h);
        self.ellipse(m, x + bl, y + h - bl, bl, bl, 0.0, PI / 2.0, PI, false);
        self.line_to(m, x, y + tl);
        self.ellipse(m, x + tl, y + tl, tl, tl, 0.0, PI, 1.5 * PI, false);
        self.close();
    }

    /// `arcTo()`: a line toward (x1, y1) rounded into the line to (x2, y2).
    pub fn arc_to(&mut self, m: &Matrix, x1: f32, y1: f32, x2: f32, y2: f32, r: f32) {
        if !finite(&[x1, y1, x2, y2, r]) {
            return;
        }
        if !self.ensure(m, x1, y1) {
            return;
        }
        // The current point, back in user space.
        let Some((x0, y0)) = invert(m).zip(self.last()).map(|(inv, p)| apply(&inv, p.0, p.1)) else {
            return;
        };
        let (v1x, v1y) = (x0 - x1, y0 - y1);
        let (v2x, v2y) = (x2 - x1, y2 - y1);
        let (l1, l2) = (hypot(v1x, v1y), hypot(v2x, v2y));
        let cross = v1x * v2y - v1y * v2x;
        if r == 0.0 || l1 == 0.0 || l2 == 0.0 || cross.abs() < 1e-6 * l1 * l2 {
            self.line_to(m, x1, y1);
            return;
        }
        let (u1x, u1y, u2x, u2y) = (v1x / l1, v1y / l1, v2x / l2, v2y / l2);
        let angle = acosf((u1x * u2x + u1y * u2y).clamp(-1.0, 1.0));
        let dist = r / libm::tanf(angle / 2.0);
        let (t1x, t1y) = (x1 + u1x * dist, y1 + u1y * dist);
        let (t2x, t2y) = (x1 + u2x * dist, y1 + u2y * dist);
        let (bx, by) = (u1x + u2x, u1y + u2y);
        let bl = hypot(bx, by);
        let d = r / sinf(angle / 2.0);
        let (cx, cy) = (x1 + bx / bl * d, y1 + by / bl * d);
        let a0 = libm::atan2f(t1y - cy, t1x - cx);
        let a1 = libm::atan2f(t2y - cy, t2x - cx);
        // The short way round.
        let mut delta = a1 - a0;
        while delta > PI {
            delta -= TAU;
        }
        while delta < -PI {
            delta += TAU;
        }
        self.line_to(m, t1x, t1y);
        self.ellipse(m, cx, cy, r, r, 0.0, a0, a0 + delta, delta < 0.0);
    }

    /// Each subpath as a polygon (open ones closed implicitly), for filling.
    pub fn polygons(&self) -> Vec<&[(f32, f32)]> {
        self.subpaths.iter().filter(|s| s.points.len() > 2).map(|s| s.points.as_slice()).collect()
    }
}

fn hypot(x: f32, y: f32) -> f32 {
    sqrtf(x * x + y * y)
}

/// The signed sweep of an arc, as the canvas standard defines it.
fn sweep(start: f32, end: f32, ccw: bool) -> f32 {
    let wrap = |v: f32| {
        let r = fmodf(v, TAU);
        if r < 0.0 {
            r + TAU
        } else {
            r
        }
    };
    if !ccw {
        if end - start >= TAU {
            TAU
        } else {
            wrap(end - start)
        }
    } else if start - end >= TAU {
        -TAU
    } else {
        -wrap(start - end)
    }
}

/// A coverage mask over part of the canvas.
pub struct Mask {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

/// Rasterizes polygons (device space) clipped to a `width × height` canvas.
pub fn rasterize(polygons: &[&[(f32, f32)]], even_odd: bool, width: u32, height: u32) -> Option<Mask> {
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in polygons.iter().flat_map(|p| p.iter()) {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    let bx0 = floorf(x0).max(0.0) as usize;
    let by0 = floorf(y0).max(0.0) as usize;
    let bx1 = (ceilf(x1).min(width as f32)).max(0.0) as usize;
    let by1 = (ceilf(y1).min(height as f32)).max(0.0) as usize;
    if bx1 <= bx0 || by1 <= by0 {
        return None;
    }
    let (w, h) = (bx1 - bx0, by1 - by0);
    let mut cov = Coverage::new(w, h);
    let fix = |v: f32, o: usize| ((v - o as f32).clamp(-1e6, 1e6) * 65536.0) as i64;
    for poly in polygons {
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            cov.line(fix(a.0, bx0), fix(a.1, by0), fix(b.0, bx0), fix(b.1, by0));
        }
    }
    Some(Mask { x: bx0, y: by0, w, h, data: cov.finish(even_odd) })
}

/// The outline of a stroke, as polygons to fill with the non-zero rule.
pub fn stroke_polygons(path: &Path, s: &State) -> Vec<Vec<(f32, f32)>> {
    let scale = scale_of(&s.matrix);
    let hw = s.line_width * scale / 2.0;
    let mut out = Vec::new();
    if !(hw > 0.0) || !hw.is_finite() {
        return out;
    }
    for sub in &path.subpaths {
        let mut pts: Vec<(f32, f32)> = Vec::with_capacity(sub.points.len());
        for &p in &sub.points {
            if pts.last().is_none_or(|q: &(f32, f32)| hypot(q.0 - p.0, q.1 - p.1) > 1e-4) {
                pts.push(p);
            }
        }
        let mut closed = sub.closed;
        if closed && pts.len() > 2 && hypot(pts[0].0 - pts[pts.len() - 1].0, pts[0].1 - pts[pts.len() - 1].1) <= 1e-4 {
            pts.pop();
        }
        if pts.len() < 2 {
            closed = false;
        }
        if s.dash.iter().any(|d| *d > 0.0) {
            for piece in dashes(&pts, closed, &s.dash, s.dash_offset, scale) {
                outline(&piece, false, hw, s, &mut out);
            }
        } else {
            outline(&pts, closed, hw, s, &mut out);
        }
    }
    out
}

/// Splits a polyline into its dashes.
fn dashes(pts: &[(f32, f32)], closed: bool, pattern: &[f32], offset: f32, scale: f32) -> Vec<Vec<(f32, f32)>> {
    let pattern: Vec<f32> = pattern.iter().map(|d| d * scale).collect();
    let total: f32 = pattern.iter().sum();
    let mut out = Vec::new();
    if pts.len() < 2 || total <= 0.0 {
        return out;
    }
    // Where in the pattern the line starts.
    let mut pos = fmodf(offset * scale, total);
    if pos < 0.0 {
        pos += total;
    }
    let mut i = 0;
    while pos >= pattern[i] {
        pos -= pattern[i];
        i = (i + 1) % pattern.len();
    }
    let mut left = pattern[i] - pos;
    let mut on = i % 2 == 0;
    let mut cur: Vec<(f32, f32)> = if on { vec![pts[0]] } else { Vec::new() };
    let n = if closed { pts.len() + 1 } else { pts.len() };
    for k in 1..n {
        let (a, b) = (pts[k - 1], pts[k % pts.len()]);
        let len = hypot(b.0 - a.0, b.1 - a.1);
        let mut done = 0.0;
        while len - done > left {
            done += left;
            let t = done / len;
            let p = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
            if on {
                cur.push(p);
                out.push(core::mem::take(&mut cur));
            } else {
                cur = vec![p];
            }
            on = !on;
            i = (i + 1) % pattern.len();
            left = pattern[i];
            if out.len() > 100_000 {
                return out;
            }
        }
        left -= len - done;
        if on {
            cur.push(b);
        }
    }
    if on && cur.len() > 1 {
        out.push(cur);
    }
    out
}

fn push_polygon(out: &mut Vec<Vec<(f32, f32)>>, mut poly: Vec<(f32, f32)>) {
    // Every piece winds the same way, so overlaps add instead of cancelling.
    let area: f32 = (0..poly.len())
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            a.0 * b.1 - b.0 * a.1
        })
        .sum();
    if area < 0.0 {
        poly.reverse();
    }
    out.push(poly);
}

fn circle(c: (f32, f32), r: f32) -> Vec<(f32, f32)> {
    let step = if r > TOLERANCE { 2.0 * acosf(1.0 - TOLERANCE / r) } else { PI / 2.0 };
    let n = (ceilf(TAU / step.max(0.01)) as usize).clamp(8, 256);
    (0..n)
        .map(|i| {
            let t = TAU * i as f32 / n as f32;
            (c.0 + r * cosf(t), c.1 + r * sinf(t))
        })
        .collect()
}

/// A polyline's stroke: a quad per segment, joins and caps.
fn outline(pts: &[(f32, f32)], closed: bool, hw: f32, s: &State, out: &mut Vec<Vec<(f32, f32)>>) {
    if pts.is_empty() {
        return;
    }
    if pts.len() == 1 {
        // A zero-length subpath shows only with round or square caps.
        let p = pts[0];
        match s.cap {
            Cap::Round => push_polygon(out, circle(p, hw)),
            Cap::Square => push_polygon(
                out,
                vec![(p.0 - hw, p.1 - hw), (p.0 + hw, p.1 - hw), (p.0 + hw, p.1 + hw), (p.0 - hw, p.1 + hw)],
            ),
            Cap::Butt => {}
        }
        return;
    }
    let seg_count = if closed { pts.len() } else { pts.len() - 1 };
    let dir = |i: usize| {
        let (a, b) = (pts[i % pts.len()], pts[(i + 1) % pts.len()]);
        let l = hypot(b.0 - a.0, b.1 - a.1);
        ((b.0 - a.0) / l, (b.1 - a.1) / l)
    };
    for i in 0..seg_count {
        let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
        let d = dir(i);
        let n = (-d.1 * hw, d.0 * hw);
        push_polygon(
            out,
            vec![(a.0 + n.0, a.1 + n.1), (b.0 + n.0, b.1 + n.1), (b.0 - n.0, b.1 - n.1), (a.0 - n.0, a.1 - n.1)],
        );
    }
    // Joins at interior vertices (every vertex, when closed).
    let joins: Vec<usize> = if closed { (0..pts.len()).collect() } else { (1..pts.len() - 1).collect() };
    for v in joins {
        let d0 = dir((v + seg_count - 1) % seg_count);
        let d1 = dir(v % seg_count);
        let p = pts[v];
        let cross = d0.0 * d1.1 - d0.1 * d1.0;
        let dot = d0.0 * d1.0 + d0.1 * d1.1;
        if cross.abs() < 1e-6 && dot > 0.0 {
            continue;
        }
        if s.join == Join::Round {
            push_polygon(out, circle(p, hw));
            continue;
        }
        let side = if cross > 0.0 { -1.0 } else { 1.0 };
        let n0 = (-d0.1 * hw * side, d0.0 * hw * side);
        let n1 = (-d1.1 * hw * side, d1.0 * hw * side);
        let o0 = (p.0 + n0.0, p.1 + n0.1);
        let o1 = (p.0 + n1.0, p.1 + n1.1);
        let half = sqrtf(((1.0 + dot) / 2.0).max(0.0));
        let ratio = if half > 1e-6 { 1.0 / half } else { f32::INFINITY };
        if s.join == Join::Miter && ratio <= s.miter_limit {
            let (bx, by) = (n0.0 + n1.0, n0.1 + n1.1);
            let bl = hypot(bx, by);
            if bl > 1e-6 {
                let tip = (p.0 + bx / bl * hw * ratio, p.1 + by / bl * hw * ratio);
                push_polygon(out, vec![p, o0, tip, o1]);
                continue;
            }
        }
        push_polygon(out, vec![p, o0, o1]);
    }
    if closed {
        return;
    }
    // Caps.
    for (p, d) in [
        (pts[0], {
            let d = dir(0);
            (-d.0, -d.1)
        }),
        (pts[pts.len() - 1], dir(pts.len() - 2)),
    ] {
        match s.cap {
            Cap::Butt => {}
            Cap::Round => push_polygon(out, circle(p, hw)),
            Cap::Square => {
                let n = (-d.1 * hw, d.0 * hw);
                let e = (d.0 * hw, d.1 * hw);
                push_polygon(
                    out,
                    vec![
                        (p.0 + n.0, p.1 + n.1),
                        (p.0 + n.0 + e.0, p.1 + n.1 + e.1),
                        (p.0 - n.0 + e.0, p.1 - n.1 + e.1),
                        (p.0 - n.0, p.1 - n.1),
                    ],
                );
            }
        }
    }
}

/// Whether a device-space point is inside the path.
pub fn contains(path: &Path, x: f32, y: f32, even_odd: bool) -> bool {
    let mut winding = 0i32;
    for poly in path.polygons() {
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            if (a.1 <= y) != (b.1 <= y) {
                let t = (y - a.1) / (b.1 - a.1);
                if a.0 + t * (b.0 - a.0) > x {
                    winding += if b.1 > a.1 { 1 } else { -1 };
                }
            }
        }
    }
    if even_odd {
        winding % 2 != 0
    } else {
        winding != 0
    }
}

/// Whether a device-space point is within the stroke's half-width of the path.
pub fn stroke_contains(path: &Path, s: &State, x: f32, y: f32) -> bool {
    let hw = s.line_width * scale_of(&s.matrix) / 2.0;
    path.subpaths.iter().any(|sub| {
        let n = sub.points.len();
        let segs = if sub.closed { n } else { n.saturating_sub(1) };
        (0..segs).any(|i| {
            let (a, b) = (sub.points[i], sub.points[(i + 1) % n]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = dx * dx + dy * dy;
            let t = if len > 0.0 { (((x - a.0) * dx + (y - a.1) * dy) / len).clamp(0.0, 1.0) } else { 0.0 };
            hypot(a.0 + t * dx - x, a.1 + t * dy - y) <= hw
        })
    })
}

/// Composites `src` (straight alpha) onto `dst` at strength `k` (coverage ×
/// globalAlpha × clip).
fn blend(dst: &mut u32, src: u32, k: f32, op: Op) {
    let s = premultiply(src);
    let s = [s[0] * k, s[1] * k, s[2] * k, s[3] * k];
    let d = premultiply(*dst);
    let (fa, fb) = match op {
        Op::SourceOver => (1.0, 1.0 - s[3]),
        Op::DestinationOver => (1.0 - d[3], 1.0),
        Op::DestinationOut => (0.0, 1.0 - s[3]),
        Op::SourceAtop => (d[3], 1.0 - s[3]),
        Op::Xor => (1.0 - d[3], 1.0 - s[3]),
        Op::Lighter => (1.0, 1.0),
        Op::Copy => (1.0, 1.0 - k),
    };
    let out: [f32; 4] = core::array::from_fn(|i| (s[i] * fa + d[i] * fb).min(1.0));
    *dst = unpremultiply(out);
}

/// A canvas's pixels and size, for drawing into.
pub struct Target<'a> {
    pub pixels: &'a mut [u32],
    pub width: u32,
    pub height: u32,
}

impl Target<'_> {
    fn clip_at(&self, s: &State, x: usize, y: usize) -> f32 {
        s.clip.as_ref().map_or(1.0, |c| c[y * self.width as usize + x] as f32 / 255.0)
    }

    /// Paints a coverage mask with a fill or stroke paint.
    pub fn paint(&mut self, mask: &Mask, paint: Paint, gradients: &[Gradient], s: &State) {
        let inverse = invert(&s.matrix);
        let alpha = s.alpha.clamp(0.0, 1.0);
        for row in 0..mask.h {
            let y = mask.y + row;
            for col in 0..mask.w {
                let x = mask.x + col;
                let cov = mask.data[row * mask.w + col] as f32 / 255.0 * self.clip_at(s, x, y);
                if cov <= 0.0 && s.op != Op::Copy {
                    continue;
                }
                let color = match paint {
                    Paint::Color(c) => c,
                    Paint::Gradient(i) => match (gradients.get(i), inverse) {
                        (Some(g), Some(inv)) => {
                            let (ux, uy) = apply(&inv, x as f32 + 0.5, y as f32 + 0.5);
                            g.color_at_user(ux, uy)
                        }
                        _ => 0,
                    },
                };
                blend(&mut self.pixels[y * self.width as usize + x], color, cov * alpha, s.op);
            }
        }
    }

    pub fn fill(&mut self, path: &Path, even_odd: bool, gradients: &[Gradient], s: &State) {
        if let Some(mask) = rasterize(&path.polygons(), even_odd, self.width, self.height) {
            self.paint(&mask, s.fill, gradients, s);
        }
    }

    pub fn stroke(&mut self, path: &Path, gradients: &[Gradient], s: &State) {
        let polys = stroke_polygons(path, s);
        let refs: Vec<&[(f32, f32)]> = polys.iter().map(Vec::as_slice).collect();
        if let Some(mask) = rasterize(&refs, false, self.width, self.height) {
            self.paint(&mask, s.stroke, gradients, s);
        }
    }

    /// `clearRect`: transparent black under the (transformed) rectangle.
    pub fn clear(&mut self, x: f32, y: f32, w: f32, h: f32, s: &State) {
        let mut p = Path::default();
        p.rect(&s.matrix, x, y, w, h);
        if let Some(mask) = rasterize(&p.polygons(), false, self.width, self.height) {
            for row in 0..mask.h {
                for col in 0..mask.w {
                    let (px, py) = (mask.x + col, mask.y + row);
                    let k = mask.data[row * mask.w + col] as f32 / 255.0 * self.clip_at(s, px, py);
                    let dst = &mut self.pixels[py * self.width as usize + px];
                    if k >= 1.0 {
                        *dst = 0;
                    } else if k > 0.0 {
                        let d = premultiply(*dst);
                        *dst = unpremultiply(core::array::from_fn(|i| d[i] * (1.0 - k)));
                    }
                }
            }
        }
    }

    /// Draws the user-space rectangle (dx, dy, dw, dh), each pixel's colour
    /// coming from `sample(u, v)` with u, v in 0..1 across it.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_mapped(
        &mut self,
        dx: f32,
        dy: f32,
        dw: f32,
        dh: f32,
        s: &State,
        mut sample: impl FnMut(f32, f32, usize, usize) -> u32,
    ) {
        if dw == 0.0 || dh == 0.0 || !finite(&[dx, dy, dw, dh]) {
            return;
        }
        let Some(inv) = invert(&s.matrix) else { return };
        let mut p = Path::default();
        p.rect(&s.matrix, dx, dy, dw, dh);
        let Some(mask) = rasterize(&p.polygons(), false, self.width, self.height) else { return };
        let alpha = s.alpha.clamp(0.0, 1.0);
        for row in 0..mask.h {
            let y = mask.y + row;
            for col in 0..mask.w {
                let x = mask.x + col;
                let cov = mask.data[row * mask.w + col] as f32 / 255.0 * self.clip_at(s, x, y);
                if cov <= 0.0 {
                    continue;
                }
                let (ux, uy) = apply(&inv, x as f32 + 0.5, y as f32 + 0.5);
                let color = sample(((ux - dx) / dw).clamp(0.0, 1.0), ((uy - dy) / dh).clamp(0.0, 1.0), x, y);
                blend(&mut self.pixels[y * self.width as usize + x], color, cov * alpha, s.op);
            }
        }
    }

    /// Intersects the clip with a path: the new clip for `s`.
    pub fn clip(&self, path: &Path, even_odd: bool, s: &State) -> Arc<Vec<u8>> {
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![0u8; w * h];
        if let Some(mask) = rasterize(&path.polygons(), even_odd, self.width, self.height) {
            for row in 0..mask.h {
                let start = (mask.y + row) * w + mask.x;
                out[start..start + mask.w].copy_from_slice(&mask.data[row * mask.w..(row + 1) * mask.w]);
            }
        }
        if let Some(old) = &s.clip {
            for (o, c) in out.iter_mut().zip(old.iter()) {
                *o = ((*o as u32 * *c as u32 + 127) / 255) as u8;
            }
        }
        Arc::new(out)
    }
}

/// Samples straight-alpha pixels at (u, v) in 0..1 over a source rectangle,
/// bilinearly when `smooth`.
#[allow(clippy::too_many_arguments)]
pub fn sample(pixels: &[u32], width: u32, sx: f32, sy: f32, sw: f32, sh: f32, u: f32, v: f32, smooth: bool) -> u32 {
    let height = (pixels.len() / width.max(1) as usize) as f32;
    let fx = sx + u * sw - 0.5;
    let fy = sy + v * sh - 0.5;
    let clamp_x = |x: f32| x.clamp(sx.max(0.0), (sx + sw).min(width as f32) - 1.0).max(0.0) as usize;
    let clamp_y = |y: f32| y.clamp(sy.max(0.0), (sy + sh).min(height) - 1.0).max(0.0) as usize;
    let at = |x: usize, y: usize| pixels.get(y * width as usize + x).copied().unwrap_or(0);
    if !smooth {
        return at(clamp_x(floorf(fx + 0.5)), clamp_y(floorf(fy + 0.5)));
    }
    let (x0, y0) = (floorf(fx), floorf(fy));
    let (tx, ty) = (fx - x0, fy - y0);
    let corners = [
        (premultiply(at(clamp_x(x0), clamp_y(y0))), (1.0 - tx) * (1.0 - ty)),
        (premultiply(at(clamp_x(x0 + 1.0), clamp_y(y0))), tx * (1.0 - ty)),
        (premultiply(at(clamp_x(x0), clamp_y(y0 + 1.0))), (1.0 - tx) * ty),
        (premultiply(at(clamp_x(x0 + 1.0), clamp_y(y0 + 1.0))), tx * ty),
    ];
    unpremultiply(core::array::from_fn(|k| corners.iter().map(|(c, w)| c[k] * w).sum()))
}
