//! CanvasRenderingContext2D, on the drawing model in `canvas2d`: paths,
//! fills and strokes, transforms, compositing, clipping, gradients, text,
//! images and pixel access, within fixed memory budgets. Other contexts
//! (WebGL) return null; patterns, shadows and filters aren't drawn.
use super::*;
use crate::canvas2d::{self, Baseline, Cap, Gradient, GradientKind, Join, Op, Paint, Target, TextAlign};
use crate::dom::CanvasBitmap;

const CONTEXT: u32 = NODE + 4;
const GRADIENT: u32 = NODE + 6;
const MAX_PIXELS: u64 = 1024 * 1024;
const PAGE_PIXELS: usize = 2 * 1024 * 1024;
const MAX_CANVASES: usize = 128;
const MAX_GRADIENTS: usize = 4096;
/// Decoded images kept for drawImage, per page.
pub const IMAGE_BYTES: usize = 16 * 1024 * 1024;

/// Glyphs for fillText and sizes for measureText, from the browser's fonts.
pub trait TextHost {
    /// `text` rendered at `px` device pixels: a coverage mask whose top-left
    /// is `ascent` above the baseline at the pen position.
    fn render(&mut self, text: &str, px: f32, bold: bool, italic: bool, mono: bool) -> TextMask;
    /// (advance width, ascent, descent) at `px`.
    fn measure(&mut self, text: &str, px: f32, bold: bool, mono: bool) -> (f32, f32, f32);
}

#[derive(Default)]
pub struct TextMask {
    pub width: u32,
    pub height: u32,
    pub advance: f32,
    pub ascent: f32,
    pub descent: f32,
    pub data: Vec<u8>,
}

/// With no fonts (host tests): sizes from a typical sans-serif, no glyphs.
pub struct NoFonts;

impl TextHost for NoFonts {
    fn render(&mut self, text: &str, px: f32, bold: bool, _italic: bool, mono: bool) -> TextMask {
        let (advance, ascent, descent) = self.measure(text, px, bold, mono);
        TextMask { advance, ascent, descent, ..TextMask::default() }
    }
    fn measure(&mut self, text: &str, px: f32, _bold: bool, mono: bool) -> (f32, f32, f32) {
        let per = if mono { 0.6 } else { 0.55 };
        (text.chars().count() as f32 * px * per, px * 0.8, px * 0.2)
    }
}

fn dimensions(doc: &Document, n: NodeId) -> (u32, u32) {
    let e = doc.element(n).unwrap();
    let size = |name, fallback| e.attr(name).and_then(|s| s.trim().parse::<u32>().ok()).unwrap_or(fallback);
    (size("width", 300), size("height", 150))
}

fn ensure(dom: &mut Dom, n: NodeId) -> bool {
    if dom.doc.canvases.contains_key(&n) {
        return true;
    }
    if dom.doc.canvases.len() >= MAX_CANVASES {
        return false;
    }
    let (w, h) = dimensions(&dom.doc, n);
    let pixels = w as u64 * h as u64;
    let used: usize = dom.doc.canvases.values().map(|c| c.pixels.len()).sum();
    if pixels > MAX_PIXELS || used + pixels as usize > PAGE_PIXELS {
        return false;
    }
    dom.doc.canvases.insert(
        n,
        CanvasBitmap {
            width: w,
            height: h,
            pixels: vec![0; pixels as usize],
            state: canvas2d::State::default(),
            saved: Vec::new(),
            path: canvas2d::Path::default(),
        },
    );
    true
}

fn context_node(rt: &mut Realm, c: &Call) -> Result<NodeId, Value> {
    if let Value::Object(o) = c.this {
        if let Kind::Host(CONTEXT, n) = rt.heap.get(o).kind {
            let n = n as NodeId;
            if with_dom(rt, |_, d| ensure(d, n)) {
                return Ok(n);
            }
            return Err(rt.range_error("Canvas exceeds the pixel budget"));
        }
    }
    Err(rt.type_error("Illegal canvas context invocation"))
}

/// Numeric arguments as f32 (NaN for missing ones).
fn nums(rt: &mut Realm, c: &Call, count: usize) -> Result<Vec<f32>, Value> {
    (0..count).map(|i| rt.to_number(&c.arg(i)).map(|v| v as f32)).collect()
}

/// Runs `f` on a context's canvas (with the page's gradients).
fn with_canvas<R>(rt: &mut Realm, c: &Call, f: impl FnOnce(&mut CanvasBitmap, &[Gradient]) -> R) -> Result<R, Value> {
    let n = context_node(rt, c)?;
    Ok(with_dom(rt, |_, d| {
        let gradients: &[Gradient] = &d.gradients_data;
        f(d.doc.canvases.get_mut(&n).unwrap(), gradients)
    }))
}

fn target(b: &mut CanvasBitmap) -> (Target<'_>, &mut canvas2d::State, &mut canvas2d::Path) {
    (Target { pixels: &mut b.pixels, width: b.width, height: b.height }, &mut b.state, &mut b.path)
}

fn get_context(rt: &mut Realm, c: &Call) -> JsResult {
    let n = this_node(rt, c)?;
    let kind = arg_str(rt, c, 0)?;
    with_dom(rt, |rt, dom| {
        if dom.doc.tag(n) != "canvas" {
            return Err(rt.type_error("Illegal canvas invocation"));
        }
        if kind != "2d" || !ensure(dom, n) {
            return Ok(Value::Null);
        }
        if let Some(o) = dom.canvas_contexts.get(&n) {
            return Ok(Value::Object(*o));
        }
        let o = rt.alloc(Obj::new(dom.protos.canvas_context, Kind::Host(CONTEXT, n as u64)));
        let canvas = wrap(rt, dom, n);
        rt.define(o, "canvas", canvas, 0);
        dom.canvas_contexts.insert(n, o);
        Ok(Value::Object(o))
    })
}

fn to_data_url(rt: &mut Realm, c: &Call) -> JsResult {
    this_node(rt, c)?;
    // No image encoders here: an empty image, as for a zero-sized canvas.
    Ok(Value::str("data:,"))
}

// ---------------------------------------------------------------- paint

fn color_string(color: u32) -> String {
    let a = color >> 24;
    if a == 255 {
        format!("#{:06x}", color & 0xffffff)
    } else {
        let alpha = super::computed::rgb(color);
        let alpha = alpha.rsplit(", ").next().unwrap_or("0)").trim_end_matches(')').to_string();
        format!("rgba({}, {}, {}, {})", (color >> 16) & 255, (color >> 8) & 255, color & 255, alpha)
    }
}

fn paint_get(rt: &mut Realm, c: &Call, stroke: bool) -> JsResult {
    let n = context_node(rt, c)?;
    with_dom(rt, |_, d| {
        let s = &d.doc.canvases[&n].state;
        Ok(match if stroke { s.stroke } else { s.fill } {
            Paint::Color(col) => Value::str(&color_string(col)),
            Paint::Gradient(i) => d.gradients.get(i).map_or(Value::Null, |o| Value::Object(*o)),
        })
    })
}

fn paint_set(rt: &mut Realm, c: &Call, stroke: bool) -> JsResult {
    let n = context_node(rt, c)?;
    let v = c.arg(0);
    let paint = match &v {
        Value::Object(o) => match rt.heap.get(*o).kind {
            Kind::Host(GRADIENT, i) => Some(Paint::Gradient(i as usize)),
            _ => None,
        },
        _ => {
            let s = rt.to_rust_string(&v)?;
            crate::values::color(&s, 0xff000000).map(Paint::Color)
        }
    };
    if let Some(p) = paint {
        with_dom(rt, |_, d| {
            let s = &mut d.doc.canvases.get_mut(&n).unwrap().state;
            if stroke {
                s.stroke = p
            } else {
                s.fill = p
            }
        });
    }
    Ok(Value::Undefined)
}

fn fill_get(rt: &mut Realm, c: &Call) -> JsResult {
    paint_get(rt, c, false)
}
fn fill_set(rt: &mut Realm, c: &Call) -> JsResult {
    paint_set(rt, c, false)
}
fn stroke_get(rt: &mut Realm, c: &Call) -> JsResult {
    paint_get(rt, c, true)
}
fn stroke_set(rt: &mut Realm, c: &Call) -> JsResult {
    paint_set(rt, c, true)
}

fn new_gradient(rt: &mut Realm, kind: GradientKind) -> JsResult {
    with_dom(rt, |rt, d| {
        if d.gradients.len() >= MAX_GRADIENTS {
            return Err(rt.range_error("Too many canvas gradients"));
        }
        let o = rt.alloc(Obj::new(d.protos.gradient, Kind::Host(GRADIENT, d.gradients.len() as u64)));
        d.gradients.push(o);
        d.gradients_data.push(Gradient { kind, stops: Vec::new() });
        Ok(Value::Object(o))
    })
}

fn create_linear_gradient(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    let v = nums(rt, c, 4)?;
    if v.iter().any(|x| !x.is_finite()) {
        return Err(rt.type_error("The provided value is non-finite"));
    }
    new_gradient(rt, GradientKind::Linear(v[0], v[1], v[2], v[3]))
}

fn create_radial_gradient(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    let v = nums(rt, c, 6)?;
    if v.iter().any(|x| !x.is_finite()) {
        return Err(rt.type_error("The provided value is non-finite"));
    }
    if v[2] < 0.0 || v[5] < 0.0 {
        return Err(dom_error(rt, "IndexSizeError", "The radius provided is negative"));
    }
    new_gradient(rt, GradientKind::Radial(v[0], v[1], v[2], v[3], v[4], v[5]))
}

fn create_conic_gradient(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    // Conic gradients aren't drawn; a gradient with no stops paints nothing.
    new_gradient(rt, GradientKind::Linear(0.0, 0.0, 0.0, 0.0))
}

fn add_color_stop(rt: &mut Realm, c: &Call) -> JsResult {
    let i = match &c.this {
        Value::Object(o) => match rt.heap.get(*o).kind {
            Kind::Host(GRADIENT, i) => i as usize,
            _ => return Err(rt.type_error("Illegal invocation")),
        },
        _ => return Err(rt.type_error("Illegal invocation")),
    };
    let offset = rt.to_number(&c.arg(0))? as f32;
    let color = arg_str(rt, c, 1)?;
    if !(0.0..=1.0).contains(&offset) {
        return Err(dom_error(rt, "IndexSizeError", "The provided offset is outside 0 to 1"));
    }
    let Some(color) = crate::values::color(&color, 0xff000000) else {
        return Err(dom_error(rt, "SyntaxError", "The value provided could not be parsed as a color"));
    };
    with_dom(rt, |_, d| d.gradients_data[i].add_stop(offset, color));
    Ok(Value::Undefined)
}

fn create_pattern(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    // Patterns aren't drawn.
    Ok(Value::Null)
}

/// A DOMException from the prelude's constructor.
fn dom_error(rt: &mut Realm, name: &str, msg: &str) -> Value {
    match rt.global_value("DOMException") {
        Ok(ctor) if rt.is_callable(&ctor) => {
            rt.construct(&ctor, &[Value::str(msg), Value::str(name)], None).unwrap_or_else(|e| e)
        }
        _ => rt.type_error(msg),
    }
}

// ---------------------------------------------------------------- state

/// A numeric property: (getter, setter) through the state.
macro_rules! number_prop {
    ($get:ident, $set:ident, $field:ident, $ok:expr) => {
        fn $get(rt: &mut Realm, c: &Call) -> JsResult {
            let v = with_canvas(rt, c, |b, _| b.state.$field)?;
            Ok(Value::Number(v as f64))
        }
        fn $set(rt: &mut Realm, c: &Call) -> JsResult {
            let v = rt.to_number(&c.arg(0))? as f32;
            let ok: fn(f32) -> bool = $ok;
            if v.is_finite() && ok(v) {
                with_canvas(rt, c, |b, _| b.state.$field = v)?;
            }
            Ok(Value::Undefined)
        }
    };
}
number_prop!(line_width_get, line_width_set, line_width, |v| v > 0.0);
number_prop!(miter_get, miter_set, miter_limit, |v| v > 0.0);
number_prop!(dash_offset_get, dash_offset_set, dash_offset, |_| true);
number_prop!(alpha_get, alpha_set, alpha, |v| (0.0..=1.0).contains(&v));

/// A keyword property: names and values in a table.
macro_rules! keyword_prop {
    ($get:ident, $set:ident, $field:ident, $table:expr) => {
        fn $get(rt: &mut Realm, c: &Call) -> JsResult {
            let v = with_canvas(rt, c, |b, _| b.state.$field)?;
            Ok(Value::str($table.iter().find(|(_, k)| *k == v).map_or("", |(n, _)| *n)))
        }
        fn $set(rt: &mut Realm, c: &Call) -> JsResult {
            let s = arg_str(rt, c, 0)?;
            if let Some((_, k)) = $table.iter().find(|(n, _)| *n == s) {
                let k = *k;
                with_canvas(rt, c, |b, _| b.state.$field = k)?;
            }
            Ok(Value::Undefined)
        }
    };
}
const CAPS: [(&str, Cap); 3] = [("butt", Cap::Butt), ("round", Cap::Round), ("square", Cap::Square)];
const JOINS: [(&str, Join); 3] = [("miter", Join::Miter), ("round", Join::Round), ("bevel", Join::Bevel)];
const ALIGNS: [(&str, TextAlign); 5] = [
    ("start", TextAlign::Start),
    ("end", TextAlign::End),
    ("left", TextAlign::Left),
    ("right", TextAlign::Right),
    ("center", TextAlign::Center),
];
const BASELINES: [(&str, Baseline); 6] = [
    ("alphabetic", Baseline::Alphabetic),
    ("top", Baseline::Top),
    ("hanging", Baseline::Hanging),
    ("middle", Baseline::Middle),
    ("ideographic", Baseline::Ideographic),
    ("bottom", Baseline::Bottom),
];
keyword_prop!(cap_get, cap_set, cap, CAPS);
keyword_prop!(join_get, join_set, join, JOINS);
keyword_prop!(align_get, align_set, align, ALIGNS);
keyword_prop!(baseline_get, baseline_set, baseline, BASELINES);
keyword_prop!(op_get, op_set, op, Op::ALL);

fn font_get(rt: &mut Realm, c: &Call) -> JsResult {
    let f = with_canvas(rt, c, |b, _| b.state.font.clone())?;
    Ok(Value::str(&f))
}
fn font_set(rt: &mut Realm, c: &Call) -> JsResult {
    let s = arg_str(rt, c, 0)?;
    with_canvas(rt, c, |b, _| b.state.set_font(&s))?;
    Ok(Value::Undefined)
}
fn smoothing_get(rt: &mut Realm, c: &Call) -> JsResult {
    Ok(Value::Bool(with_canvas(rt, c, |b, _| b.state.smoothing)?))
}
fn smoothing_set(rt: &mut Realm, c: &Call) -> JsResult {
    let v = c.arg(0).truthy();
    with_canvas(rt, c, |b, _| b.state.smoothing = v)?;
    Ok(Value::Undefined)
}

fn save(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    with_dom(rt, |rt, d| {
        let b = d.doc.canvases.get_mut(&n).unwrap();
        if b.saved.len() >= 256 {
            return Err(rt.range_error("Canvas state stack limit reached"));
        }
        b.saved.push(b.state.clone());
        Ok(Value::Undefined)
    })
}
fn restore(rt: &mut Realm, c: &Call) -> JsResult {
    with_canvas(rt, c, |b, _| {
        if let Some(s) = b.saved.pop() {
            b.state = s;
        }
    })?;
    Ok(Value::Undefined)
}
fn reset(rt: &mut Realm, c: &Call) -> JsResult {
    with_canvas(rt, c, |b, _| {
        b.pixels.fill(0);
        b.state = canvas2d::State::default();
        b.saved.clear();
        b.path.clear();
    })?;
    Ok(Value::Undefined)
}

fn set_line_dash(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    let Value::Object(list) = c.arg(0) else { return Err(rt.type_error("setLineDash takes a sequence")) };
    let len = rt.get_str(list, "length")?;
    let len = rt.to_uint32(&len)?.min(1024);
    let mut dash = Vec::new();
    for i in 0..len {
        let v = rt.get_str(list, &i.to_string())?;
        let v = rt.to_number(&v)? as f32;
        if !v.is_finite() || v < 0.0 {
            return Ok(Value::Undefined);
        }
        dash.push(v);
    }
    // An odd list repeats to make it even.
    if dash.len() % 2 == 1 {
        dash.extend(dash.clone());
    }
    with_canvas(rt, c, |b, _| b.state.dash = dash)?;
    Ok(Value::Undefined)
}

fn get_line_dash(rt: &mut Realm, c: &Call) -> JsResult {
    let dash = with_canvas(rt, c, |b, _| b.state.dash.clone())?;
    Ok(rt.array_from(dash.into_iter().map(|v| Value::Number(v as f64)).collect()))
}

// ---------------------------------------------------------------- transforms

fn transform_by(rt: &mut Realm, c: &Call, m: canvas2d::Matrix, replace: bool) -> JsResult {
    if m.iter().all(|v| v.is_finite()) {
        with_canvas(rt, c, |b, _| b.state.matrix = if replace { m } else { canvas2d::multiply(&b.state.matrix, &m) })?;
    } else {
        context_node(rt, c)?;
    }
    Ok(Value::Undefined)
}

fn translate(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 2)?;
    transform_by(rt, c, [1.0, 0.0, 0.0, 1.0, v[0], v[1]], false)
}
fn scale(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 2)?;
    transform_by(rt, c, [v[0], 0.0, 0.0, v[1], 0.0, 0.0], false)
}
fn rotate(rt: &mut Realm, c: &Call) -> JsResult {
    let a = rt.to_number(&c.arg(0))? as f32;
    let (s, co) = (libm::sinf(a), libm::cosf(a));
    transform_by(rt, c, [co, s, -s, co, 0.0, 0.0], false)
}
fn transform(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 6)?;
    transform_by(rt, c, [v[0], v[1], v[2], v[3], v[4], v[5]], false)
}
fn set_transform(rt: &mut Realm, c: &Call) -> JsResult {
    let m = if let Value::Object(o) = c.arg(0) {
        // A DOMMatrix-like object.
        let mut m = canvas2d::IDENTITY;
        for (i, k) in ["a", "b", "c", "d", "e", "f"].iter().enumerate() {
            let v = rt.get_str(o, k)?;
            if !v.is_undefined() {
                m[i] = rt.to_number(&v)? as f32;
            }
        }
        m
    } else if c.args.is_empty() {
        canvas2d::IDENTITY
    } else {
        let v = nums(rt, c, 6)?;
        [v[0], v[1], v[2], v[3], v[4], v[5]]
    };
    transform_by(rt, c, m, true)
}
fn reset_transform(rt: &mut Realm, c: &Call) -> JsResult {
    transform_by(rt, c, canvas2d::IDENTITY, true)
}
fn get_transform(rt: &mut Realm, c: &Call) -> JsResult {
    let m = with_canvas(rt, c, |b, _| b.state.matrix)?;
    let o = rt.new_object();
    for (k, v) in ["a", "b", "c", "d", "e", "f"].iter().zip(m) {
        rt.define(o, *k, Value::Number(v as f64), DEFAULT);
    }
    for (k, v) in ["m11", "m12", "m21", "m22", "m41", "m42"].iter().zip(m) {
        rt.define(o, *k, Value::Number(v as f64), DEFAULT);
    }
    rt.define(o, "is2D", Value::Bool(true), DEFAULT);
    rt.define(o, "isIdentity", Value::Bool(m == canvas2d::IDENTITY), DEFAULT);
    Ok(Value::Object(o))
}

// ---------------------------------------------------------------- paths

fn begin_path(rt: &mut Realm, c: &Call) -> JsResult {
    with_canvas(rt, c, |b, _| b.path.clear())?;
    Ok(Value::Undefined)
}
fn close_path(rt: &mut Realm, c: &Call) -> JsResult {
    with_canvas(rt, c, |b, _| b.path.close())?;
    Ok(Value::Undefined)
}
fn move_to(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 2)?;
    with_canvas(rt, c, |b, _| b.path.move_to(&b.state.matrix, v[0], v[1]))?;
    Ok(Value::Undefined)
}
fn line_to(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 2)?;
    with_canvas(rt, c, |b, _| b.path.line_to(&b.state.matrix, v[0], v[1]))?;
    Ok(Value::Undefined)
}
fn quad_to(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 4)?;
    with_canvas(rt, c, |b, _| b.path.quad_to(&b.state.matrix, v[0], v[1], v[2], v[3]))?;
    Ok(Value::Undefined)
}
fn cubic_to(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 6)?;
    with_canvas(rt, c, |b, _| b.path.cubic_to(&b.state.matrix, v[0], v[1], v[2], v[3], v[4], v[5]))?;
    Ok(Value::Undefined)
}
fn arc(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 5)?;
    let ccw = c.arg(5).truthy();
    if v[2] < 0.0 {
        return Err(dom_error(rt, "IndexSizeError", "The radius provided is negative"));
    }
    with_canvas(rt, c, |b, _| b.path.ellipse(&b.state.matrix, v[0], v[1], v[2], v[2], 0.0, v[3], v[4], ccw))?;
    Ok(Value::Undefined)
}
fn ellipse(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 7)?;
    let ccw = c.arg(7).truthy();
    if v[2] < 0.0 || v[3] < 0.0 {
        return Err(dom_error(rt, "IndexSizeError", "The radius provided is negative"));
    }
    with_canvas(rt, c, |b, _| b.path.ellipse(&b.state.matrix, v[0], v[1], v[2], v[3], v[4], v[5], v[6], ccw))?;
    Ok(Value::Undefined)
}
fn arc_to(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 5)?;
    if v[4] < 0.0 {
        return Err(dom_error(rt, "IndexSizeError", "The radius provided is negative"));
    }
    with_canvas(rt, c, |b, _| b.path.arc_to(&b.state.matrix, v[0], v[1], v[2], v[3], v[4]))?;
    Ok(Value::Undefined)
}
fn rect(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 4)?;
    with_canvas(rt, c, |b, _| b.path.rect(&b.state.matrix, v[0], v[1], v[2], v[3]))?;
    Ok(Value::Undefined)
}
fn round_rect(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 4)?;
    // radii: a number or a list of 1–4 (top-left, top-right, bottom-right, bottom-left).
    let mut radii = Vec::new();
    match c.arg(4) {
        Value::Undefined => radii.push(0.0),
        Value::Object(o) => {
            let len = rt.get_str(o, "length")?;
            for i in 0..rt.to_uint32(&len)?.min(4) {
                let r = rt.get_str(o, &i.to_string())?;
                radii.push(rt.to_number(&r)? as f32);
            }
        }
        r => radii.push(rt.to_number(&r)? as f32),
    }
    if radii.is_empty() || radii.iter().any(|r| *r < 0.0) {
        return Err(dom_error(rt, "IndexSizeError", "The radii provided are invalid"));
    }
    let r = match radii.as_slice() {
        [a] => [*a; 4],
        [a, b] => [*a, *b, *a, *b],
        [a, b, c] => [*a, *b, *c, *b],
        [a, b, c, d] => [*a, *b, *c, *d],
        _ => [0.0; 4],
    };
    with_canvas(rt, c, |b, _| b.path.round_rect(&b.state.matrix, v[0], v[1], v[2], v[3], r))?;
    Ok(Value::Undefined)
}

fn even_odd(rt: &mut Realm, v: &Value) -> Result<bool, Value> {
    Ok(!v.is_undefined() && rt.to_rust_string(v)? == "evenodd")
}

fn fill(rt: &mut Realm, c: &Call) -> JsResult {
    let eo = even_odd(rt, &c.arg(0))?;
    with_canvas(rt, c, |b, g| {
        let (mut t, s, p) = target(b);
        t.fill(p, eo, g, s);
    })?;
    Ok(Value::Undefined)
}
fn stroke(rt: &mut Realm, c: &Call) -> JsResult {
    with_canvas(rt, c, |b, g| {
        let (mut t, s, p) = target(b);
        t.stroke(p, g, s);
    })?;
    Ok(Value::Undefined)
}
fn clip(rt: &mut Realm, c: &Call) -> JsResult {
    let eo = even_odd(rt, &c.arg(0))?;
    with_canvas(rt, c, |b, _| {
        let (t, s, p) = target(b);
        let clip = t.clip(p, eo, s);
        s.clip = Some(clip);
    })?;
    Ok(Value::Undefined)
}
fn is_point_in_path(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 2)?;
    let eo = even_odd(rt, &c.arg(2))?;
    Ok(Value::Bool(with_canvas(rt, c, |b, _| canvas2d::contains(&b.path, v[0], v[1], eo))?))
}
fn is_point_in_stroke(rt: &mut Realm, c: &Call) -> JsResult {
    let v = nums(rt, c, 2)?;
    Ok(Value::Bool(with_canvas(rt, c, |b, _| canvas2d::stroke_contains(&b.path, &b.state, v[0], v[1]))?))
}

fn rect_op(rt: &mut Realm, c: &Call, what: u8) -> JsResult {
    let v = nums(rt, c, 4)?;
    if v.iter().any(|x| !x.is_finite()) {
        context_node(rt, c)?;
        return Ok(Value::Undefined);
    }
    with_canvas(rt, c, |b, g| {
        let (mut t, s, _) = target(b);
        let mut p = canvas2d::Path::default();
        p.rect(&s.matrix, v[0], v[1], v[2], v[3]);
        match what {
            0 => t.fill(&p, false, g, s),
            1 => t.clear(v[0], v[1], v[2], v[3], s),
            _ => t.stroke(&p, g, s),
        }
    })?;
    Ok(Value::Undefined)
}
fn fill_rect(rt: &mut Realm, c: &Call) -> JsResult {
    rect_op(rt, c, 0)
}
fn clear_rect(rt: &mut Realm, c: &Call) -> JsResult {
    rect_op(rt, c, 1)
}
fn stroke_rect(rt: &mut Realm, c: &Call) -> JsResult {
    rect_op(rt, c, 2)
}

// ---------------------------------------------------------------- text

fn draw_text(rt: &mut Realm, c: &Call, stroke: bool) -> JsResult {
    let n = context_node(rt, c)?;
    let text = arg_str(rt, c, 0)?;
    let v = [rt.to_number(&c.arg(1))? as f32, rt.to_number(&c.arg(2))? as f32];
    let max_width = if c.arg(3).is_undefined() { None } else { Some(rt.to_number(&c.arg(3))? as f32) };
    if !v[0].is_finite() || !v[1].is_finite() || max_width.is_some_and(|w| !(w > 0.0)) {
        return Ok(Value::Undefined);
    }
    // Spaces stand in for other whitespace, as the standard says.
    let text: String = text.chars().map(|ch| if ch.is_whitespace() { ' ' } else { ch }).collect();
    with_dom(rt, |_, d| {
        let Dom { doc, text: fonts, gradients_data, .. } = d;
        let b = doc.canvases.get_mut(&n).unwrap();
        let s = &b.state;
        let scale = libm::sqrtf((s.matrix[0] * s.matrix[3] - s.matrix[1] * s.matrix[2]).abs()).max(0.01);
        let mask = fonts.render(&text, s.font_px * scale, s.bold, s.italic, s.mono);
        if mask.width == 0 || mask.height == 0 {
            return;
        }
        let advance = mask.advance / scale;
        let squeeze = max_width.map_or(1.0, |w| if advance > w { w / advance } else { 1.0 });
        let width = advance * squeeze;
        let (ascent, descent) = (mask.ascent / scale, mask.descent / scale);
        let x = match s.align {
            TextAlign::Start | TextAlign::Left => v[0],
            TextAlign::Center => v[0] - width / 2.0,
            TextAlign::End | TextAlign::Right => v[0] - width,
        };
        let baseline = match s.baseline {
            Baseline::Alphabetic => v[1],
            Baseline::Top | Baseline::Hanging => v[1] + ascent,
            Baseline::Middle => v[1] + (ascent - descent) / 2.0,
            Baseline::Ideographic | Baseline::Bottom => v[1] - descent,
        };
        let (dw, dh) = (mask.width as f32 / scale * squeeze, mask.height as f32 / scale);
        let paint = if stroke { s.stroke } else { s.fill };
        let state = s.clone();
        let inverse = canvas2d::invert(&state.matrix);
        let gradients: &[Gradient] = gradients_data;
        let mut t = Target { pixels: &mut b.pixels, width: b.width, height: b.height };
        let (mw, mh) = (mask.width as usize, mask.height as usize);
        t.draw_mapped(x, baseline - ascent, dw, dh, &state, |u, v, px, py| {
            let fx = (u * mw as f32 - 0.5).clamp(0.0, (mw - 1) as f32);
            let fy = (v * mh as f32 - 0.5).clamp(0.0, (mh - 1) as f32);
            let (x0, y0) = (fx as usize, fy as usize);
            let (x1, y1) = ((x0 + 1).min(mw - 1), (y0 + 1).min(mh - 1));
            let (tx, ty) = (fx - x0 as f32, fy - y0 as f32);
            let m = |x: usize, y: usize| mask.data[y * mw + x] as f32;
            let cov =
                (m(x0, y0) * (1.0 - tx) + m(x1, y0) * tx) * (1.0 - ty) + (m(x0, y1) * (1.0 - tx) + m(x1, y1) * tx) * ty;
            let color = match paint {
                Paint::Color(col) => col,
                Paint::Gradient(i) => match (gradients.get(i), inverse) {
                    (Some(g), Some(inv)) => {
                        let (ux, uy) = canvas2d::apply(&inv, px as f32 + 0.5, py as f32 + 0.5);
                        g.color_at_user(ux, uy)
                    }
                    _ => 0,
                },
            };
            let a = ((color >> 24) as f32 * cov / 255.0 + 0.5) as u32;
            (a.min(255) << 24) | (color & 0xffffff)
        });
    });
    Ok(Value::Undefined)
}

fn fill_text(rt: &mut Realm, c: &Call) -> JsResult {
    draw_text(rt, c, false)
}
fn stroke_text(rt: &mut Realm, c: &Call) -> JsResult {
    // Outlined text is drawn filled, with the stroke paint.
    draw_text(rt, c, true)
}

fn measure_text(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    let text = arg_str(rt, c, 0)?;
    let (w, ascent, descent, align) = with_dom(rt, |_, d| {
        let s = &d.doc.canvases[&n].state;
        let (w, a, de) = d.text.measure(&text, s.font_px, s.bold, s.mono);
        (w, a, de, s.align)
    });
    let left = match align {
        TextAlign::Center => w / 2.0,
        TextAlign::End | TextAlign::Right => w,
        _ => 0.0,
    };
    let o = rt.new_object();
    for (k, v) in [
        ("width", w),
        ("actualBoundingBoxLeft", left),
        ("actualBoundingBoxRight", w - left),
        ("actualBoundingBoxAscent", ascent),
        ("actualBoundingBoxDescent", descent),
        ("fontBoundingBoxAscent", ascent),
        ("fontBoundingBoxDescent", descent),
        ("emHeightAscent", ascent),
        ("emHeightDescent", descent),
        ("alphabeticBaseline", 0.0),
    ] {
        rt.define(o, k, Value::Number(v as f64), DEFAULT);
    }
    Ok(Value::Object(o))
}

// ---------------------------------------------------------------- images

/// Pixels of an image source: a canvas, or an <img> the browser decoded.
fn source_pixels(dom: &Dom, n: NodeId) -> Option<(u32, u32, Arc<Vec<u32>>)> {
    match dom.doc.tag(n) {
        "canvas" => dom.doc.canvases.get(&n).map(|b| (b.width, b.height, Arc::new(b.pixels.clone()))),
        "img" => {
            let src = dom.doc.element(n)?.attr("src")?;
            let url = Url::parse(&dom.url).and_then(|u| u.join(src.trim())).map(|u| u.to_string())?;
            dom.images.get(&url).cloned()
        }
        _ => None,
    }
}

fn draw_image(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    let Some(src) = node_of(rt, &c.arg(0)) else {
        return Err(rt.type_error("drawImage: the image argument is not an image or canvas"));
    };
    let count = c.args.len() - 1;
    let v: Vec<f32> =
        (1..=count.min(8)).map(|i| rt.to_number(&c.arg(i)).map(|v| v as f32)).collect::<Result<_, _>>()?;
    with_dom(rt, |_, d| {
        let Some((sw0, sh0, pixels)) = source_pixels(d, src) else { return };
        if sw0 == 0 || sh0 == 0 {
            return;
        }
        let (sw0, sh0) = (sw0 as f32, sh0 as f32);
        let (sx, sy, sw, sh, dx, dy, dw, dh) = match v.len() {
            2 => (0.0, 0.0, sw0, sh0, v[0], v[1], sw0, sh0),
            4 => (0.0, 0.0, sw0, sh0, v[0], v[1], v[2], v[3]),
            8 => (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]),
            _ => return,
        };
        if sw == 0.0 || sh == 0.0 {
            return;
        }
        let b = d.doc.canvases.get_mut(&n).unwrap();
        let state = b.state.clone();
        let mut t = Target { pixels: &mut b.pixels, width: b.width, height: b.height };
        let width = sw0 as u32;
        t.draw_mapped(dx, dy, dw, dh, &state, |u, v, _, _| {
            canvas2d::sample(&pixels, width, sx, sy, sw, sh, u, v, state.smoothing)
        });
    });
    Ok(Value::Undefined)
}

/// `new ImageData(width, height)` through the prelude's class.
fn new_image_data(rt: &mut Realm, w: u32, h: u32) -> JsResult {
    let ctor = rt.global_value("ImageData")?;
    rt.construct(&ctor, &[Value::Number(w as f64), Value::Number(h as f64)], None)
}

/// The Uint8ClampedArray of an ImageData-like object, and its size.
fn image_data_of(rt: &mut Realm, v: &Value) -> Result<(ObjRef, u32, u32), Value> {
    let Value::Object(o) = v else { return Err(rt.type_error("not an ImageData")) };
    let data = rt.get_str(*o, "data")?;
    let w = rt.get_str(*o, "width")?;
    let h = rt.get_str(*o, "height")?;
    let (w, h) = (rt.to_uint32(&w)?, rt.to_uint32(&h)?);
    match data {
        Value::Object(d) if rt.typed_array(d).is_some_and(|t| t.3 >= (w as usize) * (h as usize) * 4) => Ok((d, w, h)),
        _ => Err(rt.type_error("not an ImageData")),
    }
}

fn create_image_data(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    let (w, h) = if let Value::Object(_) = c.arg(0) {
        let (_, w, h) = image_data_of(rt, &c.arg(0))?;
        (w, h)
    } else {
        let v = nums(rt, c, 2)?;
        (v[0].abs() as u32, v[1].abs() as u32)
    };
    if w == 0 || h == 0 {
        return Err(dom_error(rt, "IndexSizeError", "The source width or height is 0"));
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(rt.range_error("ImageData exceeds the pixel budget"));
    }
    new_image_data(rt, w, h)
}

fn get_image_data(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    let mut v = [0i64; 4];
    for (i, v) in v.iter_mut().enumerate() {
        let f = rt.to_number(&c.arg(i))?;
        if !f.is_finite() {
            return Err(rt.type_error("ImageData coordinates must be finite"));
        }
        *v = f.clamp(i32::MIN as f64, i32::MAX as f64) as i64;
    }
    let (w, h) = (v[2].unsigned_abs(), v[3].unsigned_abs());
    if w == 0 || h == 0 {
        return Err(dom_error(rt, "IndexSizeError", "The source width or height is 0"));
    }
    if w * h > MAX_PIXELS {
        return Err(rt.range_error("ImageData exceeds the pixel budget"));
    }
    let (x0, y0) = (v[0] + v[2].min(0), v[1] + v[3].min(0));
    let image = new_image_data(rt, w as u32, h as u32)?;
    let (array, _, _) = image_data_of(rt, &image)?;
    with_dom(rt, |rt, d| {
        let b = &d.doc.canvases[&n];
        for y in 0..h {
            for x in 0..w {
                let (sx, sy) = (x0 + x as i64, y0 + y as i64);
                let px = if sx >= 0 && sy >= 0 && sx < b.width as i64 && sy < b.height as i64 {
                    b.pixels[(sy * b.width as i64 + sx) as usize]
                } else {
                    0
                };
                for (i, shift) in [16, 8, 0, 24].iter().enumerate() {
                    rt.ta_put(array, ((y * w + x) * 4) as usize + i, ((px >> shift) & 255) as f64);
                }
            }
        }
    });
    Ok(image)
}

fn put_image_data(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    let (array, w, h) = image_data_of(rt, &c.arg(0))?;
    let v = nums(rt, c, 7)?;
    let (dx, dy) = (v[1], v[2]);
    if !dx.is_finite() || !dy.is_finite() {
        return Ok(Value::Undefined);
    }
    // The dirty rectangle, within the image.
    let (mut rx, mut ry, mut rw, mut rh) =
        if c.args.len() >= 7 { (v[3], v[4], v[5], v[6]) } else { (0.0, 0.0, w as f32, h as f32) };
    if rw < 0.0 {
        rx += rw;
        rw = -rw;
    }
    if rh < 0.0 {
        ry += rh;
        rh = -rh;
    }
    let x0 = rx.max(0.0) as i64;
    let y0 = ry.max(0.0) as i64;
    let x1 = ((rx + rw) as i64).min(w as i64);
    let y1 = ((ry + rh) as i64).min(h as i64);
    let (dx, dy) = (dx as i64, dy as i64);
    with_dom(rt, |rt, d| {
        let b = d.doc.canvases.get_mut(&n).unwrap();
        for y in y0..y1 {
            for x in x0..x1 {
                let (tx, ty) = (dx + x, dy + y);
                if tx < 0 || ty < 0 || tx >= b.width as i64 || ty >= b.height as i64 {
                    continue;
                }
                let i = ((y * w as i64 + x) * 4) as usize;
                let ch = |k: usize| rt.ta_get(array, i + k) as u32 & 255;
                b.pixels[(ty * b.width as i64 + tx) as usize] = ch(3) << 24 | ch(0) << 16 | ch(1) << 8 | ch(2);
            }
        }
    });
    Ok(Value::Undefined)
}

fn context_attributes(rt: &mut Realm, c: &Call) -> JsResult {
    context_node(rt, c)?;
    let o = rt.new_object();
    rt.define(o, "alpha", Value::Bool(true), DEFAULT);
    rt.define(o, "desynchronized", Value::Bool(false), DEFAULT);
    rt.define(o, "colorSpace", Value::str("srgb"), DEFAULT);
    rt.define(o, "willReadFrequently", Value::Bool(false), DEFAULT);
    Ok(Value::Object(o))
}

pub(super) fn install(rt: &mut Realm, html_element: ObjRef) -> (ObjRef, ObjRef, ObjRef) {
    let canvas = rt.new_object_with(Some(html_element));
    interface(rt, "HTMLCanvasElement", canvas, None);
    for name in ["width", "height"] {
        accessor_with(rt, canvas, name, Value::str(name), dimension_get, Some(dimension_set));
    }
    rt.method(canvas, "getContext", 1, get_context);
    rt.method(canvas, "toDataURL", 0, to_data_url);
    let context = rt.new_object();
    interface(rt, "CanvasRenderingContext2D", context, None);
    for (name, get, set) in [
        ("fillStyle", fill_get as NativeFn, fill_set as NativeFn),
        ("strokeStyle", stroke_get, stroke_set),
        ("lineWidth", line_width_get, line_width_set),
        ("miterLimit", miter_get, miter_set),
        ("lineDashOffset", dash_offset_get, dash_offset_set),
        ("globalAlpha", alpha_get, alpha_set),
        ("lineCap", cap_get, cap_set),
        ("lineJoin", join_get, join_set),
        ("textAlign", align_get, align_set),
        ("textBaseline", baseline_get, baseline_set),
        ("globalCompositeOperation", op_get, op_set),
        ("font", font_get, font_set),
        ("imageSmoothingEnabled", smoothing_get, smoothing_set),
    ] {
        accessor(rt, context, name, get, Some(set));
    }
    // Accepted and remembered, but not drawn.
    for (name, v) in [
        ("shadowBlur", Value::Number(0.0)),
        ("shadowOffsetX", Value::Number(0.0)),
        ("shadowOffsetY", Value::Number(0.0)),
        ("shadowColor", Value::str("rgba(0, 0, 0, 0)")),
        ("filter", Value::str("none")),
        ("direction", Value::str("inherit")),
        ("imageSmoothingQuality", Value::str("low")),
        ("letterSpacing", Value::str("0px")),
    ] {
        rt.define(context, name, v, nebula_script::object::WRITABLE);
    }
    methods(
        rt,
        context,
        &[
            ("fillRect", 4, fill_rect),
            ("clearRect", 4, clear_rect),
            ("strokeRect", 4, stroke_rect),
            ("beginPath", 0, begin_path),
            ("closePath", 0, close_path),
            ("moveTo", 2, move_to),
            ("lineTo", 2, line_to),
            ("quadraticCurveTo", 4, quad_to),
            ("bezierCurveTo", 6, cubic_to),
            ("arc", 5, arc),
            ("arcTo", 5, arc_to),
            ("ellipse", 7, ellipse),
            ("rect", 4, rect),
            ("roundRect", 5, round_rect),
            ("fill", 0, fill),
            ("stroke", 0, stroke),
            ("clip", 0, clip),
            ("isPointInPath", 2, is_point_in_path),
            ("isPointInStroke", 2, is_point_in_stroke),
            ("save", 0, save),
            ("restore", 0, restore),
            ("reset", 0, reset),
            ("translate", 2, translate),
            ("scale", 2, scale),
            ("rotate", 1, rotate),
            ("transform", 6, transform),
            ("setTransform", 0, set_transform),
            ("resetTransform", 0, reset_transform),
            ("getTransform", 0, get_transform),
            ("setLineDash", 1, set_line_dash),
            ("getLineDash", 0, get_line_dash),
            ("fillText", 3, fill_text),
            ("strokeText", 3, stroke_text),
            ("measureText", 1, measure_text),
            ("drawImage", 3, draw_image),
            ("getImageData", 4, get_image_data),
            ("putImageData", 3, put_image_data),
            ("createImageData", 1, create_image_data),
            ("createLinearGradient", 4, create_linear_gradient),
            ("createRadialGradient", 6, create_radial_gradient),
            ("createConicGradient", 3, create_conic_gradient),
            ("createPattern", 2, create_pattern),
            ("getContextAttributes", 0, context_attributes),
        ],
    );
    let gradient = rt.new_object();
    interface(rt, "CanvasGradient", gradient, None);
    rt.method(gradient, "addColorStop", 2, add_color_stop);
    (canvas, context, gradient)
}
