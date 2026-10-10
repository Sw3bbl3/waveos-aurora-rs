//! Bounded software 2D canvas: CSS color probes and rectangle painting.
//! Other contexts (including WebGL) return null; paths, text and transforms
//! are not exposed as no-op drawing methods.
use super::*;
use crate::dom::CanvasBitmap;

const CONTEXT: u32 = NODE + 4;
const MAX_PIXELS: u64 = 1024 * 1024;
const PAGE_PIXELS: usize = 2 * 1024 * 1024;
const MAX_CANVASES: usize = 128;

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
        CanvasBitmap { width: w, height: h, pixels: vec![0; pixels as usize], fill: 0xff000000, saved: Vec::new() },
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

fn fill_get(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    let color = with_dom(rt, |_, d| d.doc.canvases[&n].fill);
    let a = color >> 24;
    Ok(Value::str(&if a == 255 {
        format!("#{:06x}", color & 0xffffff)
    } else {
        format!("rgba({}, {}, {}, {})", (color >> 16) & 255, (color >> 8) & 255, color & 255, a as f64 / 255.0)
    }))
}

fn fill_set(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    let value = arg_str(rt, c, 0)?;
    if let Some(color) = crate::values::color(&value, 0xff000000) {
        with_dom(rt, |_, d| d.doc.canvases.get_mut(&n).unwrap().fill = color);
    }
    Ok(Value::Undefined)
}

fn rectangle(rt: &mut Realm, c: &Call, clear: bool) -> JsResult {
    let n = context_node(rt, c)?;
    let mut v = [0i64; 4];
    for (i, v) in v.iter_mut().enumerate() {
        let f = rt.to_number(&c.arg(i))?;
        if !f.is_finite() {
            return Ok(Value::Undefined);
        }
        *v = f.clamp(i32::MIN as f64, i32::MAX as f64) as i64;
    }
    with_dom(rt, |_, d| {
        let b = d.doc.canvases.get_mut(&n).unwrap();
        let (x0, x1) = (v[0].min(v[0] + v[2]).max(0), v[0].max(v[0] + v[2]).min(b.width as i64));
        let (y0, y1) = (v[1].min(v[1] + v[3]).max(0), v[1].max(v[1] + v[3]).min(b.height as i64));
        let src = b.fill;
        let sa = src >> 24;
        for y in y0..y1 {
            for x in x0..x1 {
                let dst = &mut b.pixels[(y * b.width as i64 + x) as usize];
                if clear {
                    *dst = 0;
                    continue;
                }
                let da = *dst >> 24;
                let alpha = sa * 255 + da * (255 - sa);
                if alpha == 0 {
                    *dst = 0;
                    continue;
                }
                let mut out = ((alpha + 127) / 255) << 24;
                for shift in [0, 8, 16] {
                    let channel =
                        (((src >> shift) & 255) * sa * 255 + ((*dst >> shift) & 255) * da * (255 - sa) + alpha / 2)
                            / alpha;
                    out |= channel << shift;
                }
                *dst = out;
            }
        }
    });
    Ok(Value::Undefined)
}

fn fill_rect(rt: &mut Realm, c: &Call) -> JsResult {
    rectangle(rt, c, false)
}
fn clear_rect(rt: &mut Realm, c: &Call) -> JsResult {
    rectangle(rt, c, true)
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
        return Err(rt.error(nebula_script::ErrorKind::Error, "IndexSizeError: Empty ImageData"));
    }
    if w * h > MAX_PIXELS {
        return Err(rt.range_error("ImageData exceeds the pixel budget"));
    }
    let (x0, y0) = (v[0] + v[2].min(0), v[1] + v[3].min(0));
    let constructor = rt.get_str(rt.global, "Uint8ClampedArray")?;
    let data = rt.construct(&constructor, &[Value::Number((w * h * 4) as f64)], None)?;
    let Value::Object(array) = data else { unreachable!() };
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
    let o = rt.new_object();
    rt.define(o, "data", data, HIDDEN);
    rt.define(o, "width", Value::Number(w as f64), HIDDEN);
    rt.define(o, "height", Value::Number(h as f64), HIDDEN);
    Ok(Value::Object(o))
}

fn save(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    with_dom(rt, |rt, d| {
        let b = d.doc.canvases.get_mut(&n).unwrap();
        if b.saved.len() >= 256 {
            return Err(rt.range_error("Canvas state stack limit reached"));
        }
        b.saved.push(b.fill);
        Ok(Value::Undefined)
    })
}
fn restore(rt: &mut Realm, c: &Call) -> JsResult {
    let n = context_node(rt, c)?;
    with_dom(rt, |_, d| {
        let b = d.doc.canvases.get_mut(&n).unwrap();
        if let Some(fill) = b.saved.pop() {
            b.fill = fill;
        }
    });
    Ok(Value::Undefined)
}

pub(super) fn install(rt: &mut Realm, html_element: ObjRef) -> (ObjRef, ObjRef) {
    let canvas = rt.new_object_with(Some(html_element));
    interface(rt, "HTMLCanvasElement", canvas, None);
    for name in ["width", "height"] {
        accessor_with(rt, canvas, name, Value::str(name), dimension_get, Some(dimension_set));
    }
    rt.method(canvas, "getContext", 1, get_context);
    let context = rt.new_object();
    interface(rt, "CanvasRenderingContext2D", context, None);
    accessor(rt, context, "fillStyle", fill_get, Some(fill_set));
    methods(
        rt,
        context,
        &[
            ("fillRect", 4, fill_rect),
            ("clearRect", 4, clear_rect),
            ("getImageData", 4, get_image_data),
            ("save", 0, save),
            ("restore", 0, restore),
        ],
    );
    (canvas, context)
}
