//! getComputedStyle: the cascade's resolved values as CSS text, read-only.
//! Styles are computed on demand from the live document (so a class change
//! earlier in the same script shows) and cached until the tree changes.

use super::*;
use crate::sheets;
use crate::style::{Align, Display, Float, LineHeight, ListStyle, Position, Style, Transform, WhiteSpace};
use crate::values::Len;
use alloc::rc::Rc;

/// Host tag of a computed declaration; its data is `node << 2 | pseudo`.
pub(super) const COMPUTED: u32 = NODE + 5;

/// Full recomputations allowed per script turn before stale styles are
/// reused: a loop that writes then reads styles mustn't run the cascade
/// thousands of times.
const RESTYLES_PER_TURN: u32 = 16;

/// The properties a computed declaration lists, in CSS (dashed) form.
const PROPS: &[&str] = &[
    "align-items",
    "animation-duration",
    "animation-name",
    "background-color",
    "border-bottom-color",
    "border-bottom-left-radius",
    "border-bottom-right-radius",
    "border-bottom-style",
    "border-bottom-width",
    "border-left-color",
    "border-left-style",
    "border-left-width",
    "border-right-color",
    "border-right-style",
    "border-right-width",
    "border-top-color",
    "border-top-left-radius",
    "border-top-right-radius",
    "border-top-style",
    "border-top-width",
    "bottom",
    "box-sizing",
    "clear",
    "color",
    "column-gap",
    "content",
    "cursor",
    "direction",
    "display",
    "flex-direction",
    "flex-grow",
    "flex-wrap",
    "float",
    "font-family",
    "font-size",
    "font-style",
    "font-weight",
    "grid-column",
    "grid-row",
    "grid-template-columns",
    "height",
    "justify-content",
    "left",
    "line-height",
    "list-style-type",
    "margin-bottom",
    "margin-left",
    "margin-right",
    "margin-top",
    "max-height",
    "max-width",
    "min-height",
    "min-width",
    "opacity",
    "overflow-x",
    "overflow-y",
    "padding-bottom",
    "padding-left",
    "padding-right",
    "padding-top",
    "pointer-events",
    "position",
    "right",
    "row-gap",
    "text-align",
    "text-decoration-line",
    "text-transform",
    "top",
    "transform",
    "transition-delay",
    "transition-duration",
    "transition-property",
    "vertical-align",
    "visibility",
    "white-space",
    "width",
    "z-index",
];

/// Shorthands answered from their longhands.
const SHORTHANDS: &[&str] = &["margin", "padding", "border-width", "border-radius", "overflow", "gap"];

impl Dom {
    /// The page's stylesheets, from the page before each script turn.
    pub fn set_style_sources(&mut self, sources: Arc<sheets::Sources>) {
        if !self.style_sources.as_ref().is_some_and(|s| Arc::ptr_eq(s, &sources)) {
            self.style_sources = Some(sources);
            self.computed = None;
        }
        self.restyles = 0;
    }

    /// Styles for the document as it is now (or as it was at the last
    /// restyle, once a turn's budget is spent).
    fn computed_styles(&mut self) -> Option<&style::Computed> {
        let sources = self.style_sources.clone()?;
        let fresh = self.computed.as_ref().is_some_and(|(v, size, _)| *v == self.doc.version && *size == self.viewport);
        if !fresh && (self.computed.is_none() || self.restyles < RESTYLES_PER_TURN) {
            self.restyles += 1;
            let c = sheets::compute(&self.doc, &sources, &mut self.style_dynamic, self.viewport, true);
            self.computed = Some((self.doc.version, self.viewport, c));
        }
        self.computed.as_ref().map(|(_, _, c)| c)
    }
}

/// A number as CSS prints it: at most three decimals, no trailing zeros.
fn number(v: f32) -> String {
    let s = format!("{v:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    String::from(if s == "-0" { "0" } else { s })
}

fn px(v: f32) -> String {
    format!("{}px", number(v))
}

/// `rgb()` or `rgba()`, as browsers serialize resolved colours.
pub(super) fn rgb(c: u32) -> String {
    let (a, r, g, b) = (c >> 24, (c >> 16) & 255, (c >> 8) & 255, c & 255);
    if a == 255 {
        format!("rgb({r}, {g}, {b})")
    } else {
        format!("rgba({r}, {g}, {b}, {})", number(a as f32 / 255.0))
    }
}

/// A length resolved against `reference` (the containing block's width).
fn len(l: Len, reference: Option<i32>, auto: &str) -> String {
    match l {
        Len::Auto => String::from(auto),
        Len::Calc(pct, v) if pct == 0.0 => px(v),
        Len::Calc(pct, v) => match reference {
            Some(w) => px(pct / 100.0 * w as f32 + v),
            None if v == 0.0 => format!("{}%", number(pct)),
            None => format!("calc({}% + {})", number(pct), px(v)),
        },
    }
}

const SIDES: [&str; 4] = ["top", "right", "bottom", "left"];

fn side(name: &str, prefix: &str, suffix: &str) -> Option<usize> {
    let s = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
    SIDES.iter().position(|x| *x == s)
}

/// One property's resolved value; "" for ones this engine doesn't model.
fn value(s: &Style, name: &str, geometry: Option<(i32, i32, i32, i32)>, parent_width: Option<i32>) -> String {
    if name.starts_with("--") {
        return s.vars.get(name).map(|v| String::from(v.trim())).unwrap_or_default();
    }
    if let Some(i) = side(name, "margin-", "") {
        return len(s.margin[i], parent_width, "0px");
    }
    if let Some(i) = side(name, "padding-", "") {
        return len(s.padding[i], parent_width, "0px");
    }
    if let Some(i) = side(name, "border-", "-width") {
        return px(if s.border_on[i] { s.border_width[i] } else { 0.0 });
    }
    if let Some(i) = side(name, "border-", "-style") {
        return String::from(if s.border_on[i] { "solid" } else { "none" });
    }
    if let Some(i) = side(name, "border-", "-color") {
        return rgb(s.border_color[i].unwrap_or(s.color));
    }
    if let Some(i) = side(name, "", "") {
        return len(s.offsets[i], parent_width, "auto");
    }
    let shown = s.display != Display::None;
    // Used sizes come from layout; inline boxes have none of their own.
    let used = |i: usize| {
        let (_, _, w, h) = geometry.filter(|_| shown && s.display != Display::Inline)?;
        let edges = |a: usize, b: usize| {
            let border = |k: usize| if s.border_on[k] { s.border_width[k] } else { 0.0 };
            let pad = |k: usize| match s.padding[k] {
                Len::Calc(pct, v) => pct / 100.0 * parent_width.unwrap_or(0) as f32 + v,
                Len::Auto => 0.0,
            };
            border(a) + border(b) + pad(a) + pad(b)
        };
        let size = if i == 0 { w as f32 } else { h as f32 };
        let inset = if s.border_box {
            0.0
        } else if i == 0 {
            edges(1, 3)
        } else {
            edges(0, 2)
        };
        Some(px((size - inset).max(0.0)))
    };
    let display = match s.display {
        Display::None => "none",
        Display::Block => "block",
        Display::Inline => "inline",
        Display::InlineBlock => "inline-block",
        Display::ListItem => "list-item",
        Display::Table => "table",
        Display::TableRowGroup => "table-row-group",
        Display::TableRow => "table-row",
        Display::TableCell => "table-cell",
        Display::TableCaption => "table-caption",
        Display::Flex => "flex",
        Display::InlineFlex => "inline-flex",
        Display::Grid => "grid",
    };
    let radius = || px(s.radius);
    String::from(match name {
        "display" => display,
        "position" => match s.position {
            Position::Static => "static",
            Position::Relative => "relative",
            Position::Absolute => "absolute",
            Position::Fixed => "fixed",
            Position::Sticky => "sticky",
        },
        "float" => match s.float {
            Float::None => "none",
            Float::Left => "left",
            Float::Right => "right",
        },
        "clear" => {
            if s.clear {
                "both"
            } else {
                "none"
            }
        }
        "color" => return rgb(s.color),
        "background-color" => return rgb(s.background),
        "opacity" => return number(s.opacity),
        "visibility" => {
            if s.visible {
                "visible"
            } else {
                "hidden"
            }
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            if s.overflow_hidden {
                "hidden"
            } else {
                "visible"
            }
        }
        "font-size" => return px(s.font_size),
        "font-weight" => {
            if s.bold {
                "700"
            } else {
                "400"
            }
        }
        "font-style" => {
            if s.italic {
                "italic"
            } else {
                "normal"
            }
        }
        "font-family" => {
            if s.mono {
                "monospace"
            } else {
                "sans-serif"
            }
        }
        "line-height" => {
            return match s.line_height {
                LineHeight::Normal => String::from("normal"),
                LineHeight::Factor(f) => px(f * s.font_size),
                LineHeight::Px(v) => px(v),
            }
        }
        "text-align" => match s.align {
            Align::Left => "start",
            Align::Center => "center",
            Align::Right => "right",
            Align::Justify => "justify",
        },
        "text-transform" => match s.transform {
            Transform::None => "none",
            Transform::Upper => "uppercase",
            Transform::Lower => "lowercase",
            Transform::Capitalize => "capitalize",
        },
        "text-decoration-line" => match (s.underline, s.strike) {
            (false, false) => "none",
            (true, false) => "underline",
            (false, true) => "line-through",
            (true, true) => "underline line-through",
        },
        "white-space" => match s.white_space {
            WhiteSpace::Normal => "normal",
            WhiteSpace::Pre => "pre",
            WhiteSpace::NoWrap => "nowrap",
            WhiteSpace::PreWrap => "pre-wrap",
            WhiteSpace::PreLine => "pre-line",
        },
        "list-style-type" => match s.list_style {
            ListStyle::None => "none",
            ListStyle::Disc => "disc",
            ListStyle::Circle => "circle",
            ListStyle::Square => "square",
            ListStyle::Decimal => "decimal",
            ListStyle::LowerAlpha => "lower-alpha",
            ListStyle::UpperAlpha => "upper-alpha",
            ListStyle::LowerRoman => "lower-roman",
            ListStyle::UpperRoman => "upper-roman",
        },
        "vertical-align" => {
            if s.vertical_middle {
                "middle"
            } else {
                "baseline"
            }
        }
        "box-sizing" => {
            if s.border_box {
                "border-box"
            } else {
                "content-box"
            }
        }
        "width" => return used(0).unwrap_or_else(|| len(s.width, parent_width, "auto")),
        "height" => return used(1).unwrap_or_else(|| len(s.height, None, "auto")),
        "min-width" => return len(s.min_width, parent_width, "auto"),
        "min-height" => return len(s.min_height, None, "auto"),
        "max-width" => return len(s.max_width, parent_width, "none"),
        "max-height" => return len(s.max_height, None, "none"),
        "border-top-left-radius"
        | "border-top-right-radius"
        | "border-bottom-right-radius"
        | "border-bottom-left-radius"
        | "border-radius" => return radius(),
        "flex-direction" => {
            if s.flex_column {
                "column"
            } else {
                "row"
            }
        }
        "flex-wrap" => {
            if s.flex_wrap {
                "wrap"
            } else {
                "nowrap"
            }
        }
        "flex-grow" => return number(s.flex_grow),
        "justify-content" => match s.justify {
            crate::style::Justify::Start => "normal",
            crate::style::Justify::Center => "center",
            crate::style::Justify::End => "flex-end",
            crate::style::Justify::SpaceBetween => "space-between",
            crate::style::Justify::SpaceAround => "space-around",
        },
        "align-items" => {
            if s.center_items {
                "center"
            } else {
                "normal"
            }
        }
        "column-gap" => return if s.gap == 0.0 { String::from("normal") } else { px(s.gap) },
        "row-gap" => return if s.row_gap == 0.0 { String::from("normal") } else { px(s.row_gap) },
        "grid-template-columns" => return s.grid_columns.clone().unwrap_or_else(|| String::from("none")),
        "grid-column" => return s.grid_column.clone().unwrap_or_else(|| String::from("auto")),
        "grid-row" => return s.grid_row.clone().unwrap_or_else(|| String::from("auto")),
        "content" => return s.content.clone().unwrap_or_else(|| String::from("normal")),
        "z-index" | "cursor" | "pointer-events" => "auto",
        "direction" => "ltr",
        "transform" | "animation-name" => "none",
        "transition-duration" | "transition-delay" | "animation-duration" => "0s",
        "transition-property" => "all",
        _ => "",
    })
}

/// A shorthand: one value if its parts agree, else all four.
fn shorthand(s: &Style, name: &str, g: Option<(i32, i32, i32, i32)>, w: Option<i32>) -> String {
    let parts: Vec<String> = match name {
        "margin" | "padding" => SIDES.iter().map(|x| value(s, &format!("{name}-{x}"), g, w)).collect(),
        "border-width" => SIDES.iter().map(|x| value(s, &format!("border-{x}-width"), g, w)).collect(),
        "border-radius" => alloc::vec![value(s, "border-radius", g, w)],
        "overflow" => alloc::vec![value(s, "overflow", g, w)],
        _ => alloc::vec![value(s, "row-gap", g, w), value(s, "column-gap", g, w)],
    };
    // The shortest form that says the same, as browsers serialize boxes.
    let n = match parts.as_slice() {
        [t, r, b, l] if l == r && t == b && t == r => 1,
        [t, r, b, l] if l == r && t == b => 2,
        [_, r, _, l] if l == r => 3,
        [a, b] if a == b => 1,
        p => p.len(),
    };
    parts[..n].join(" ")
}

fn this_computed(rt: &mut Realm, c: &Call) -> Result<(NodeId, usize), Value> {
    let data = match &c.this {
        Value::Object(o) => match rt.heap.get(*o).kind {
            Kind::Host(t, d) if t == COMPUTED => d,
            _ => return Err(rt.type_error("Illegal invocation")),
        },
        _ => return Err(rt.type_error("Illegal invocation")),
    };
    Ok(((data >> 2) as NodeId, (data & 3) as usize))
}

fn lookup(rt: &mut Realm, c: &Call, name: &str) -> Result<String, Value> {
    let (n, pseudo) = this_computed(rt, c)?;
    let name = if name.starts_with("--") { String::from(name) } else { name.to_ascii_lowercase() };
    Ok(with_dom(rt, |_, dom| {
        // Disconnected elements have no computed style.
        if !dom.doc.is_connected(n) || dom.doc.element(n).is_none() {
            return String::new();
        }
        let geometry = dom.geometry.get(&n).copied();
        let parent_width = dom.doc.parent(n).and_then(|p| dom.geometry.get(&p)).map(|g| g.2);
        let Some(computed) = dom.computed_styles() else { return String::new() };
        let style: Option<Rc<Style>> = if pseudo == 0 {
            computed.styles.get(n).cloned().flatten()
        } else {
            computed.pseudo.get(&n).and_then(|g| g[pseudo - 1].as_ref()).map(|(s, _)| s.clone())
        };
        let Some(style) = style else { return String::new() };
        let geometry = if pseudo == 0 { geometry } else { None };
        if SHORTHANDS.contains(&name.as_str()) {
            shorthand(&style, &name, geometry, parent_width)
        } else {
            value(&style, &name, geometry, parent_width)
        }
    }))
}

fn get_property_value(rt: &mut Realm, c: &Call) -> JsResult {
    let name = arg_str(rt, c, 0)?;
    Ok(Value::str(&lookup(rt, c, &name)?))
}

fn property_get(rt: &mut Realm, c: &Call) -> JsResult {
    let name = rt.native_slots(c.callee)[0].clone();
    let name = rt.to_rust_string(&name)?;
    Ok(Value::str(&lookup(rt, c, &name)?))
}

fn read_only(rt: &mut Realm, _c: &Call) -> JsResult {
    let msg = Value::str("Failed to set a property on 'CSSStyleDeclaration': computed styles are read-only.");
    let err = match rt.global_value("DOMException") {
        Ok(ctor) if rt.is_callable(&ctor) => {
            rt.construct(&ctor, &[msg, Value::str("NoModificationAllowedError")], None)?
        }
        _ => rt.type_error("computed styles are read-only"),
    };
    Err(err)
}

fn length(rt: &mut Realm, c: &Call) -> JsResult {
    this_computed(rt, c)?;
    Ok(Value::Number(PROPS.len() as f64))
}

fn item(rt: &mut Realm, c: &Call) -> JsResult {
    this_computed(rt, c)?;
    let i = rt.to_uint32(&c.arg(0))? as usize;
    Ok(Value::str(PROPS.get(i).copied().unwrap_or("")))
}

fn empty(rt: &mut Realm, c: &Call) -> JsResult {
    this_computed(rt, c)?;
    Ok(Value::str(""))
}

/// `getComputedStyle(element, pseudo)`.
pub(super) fn get_computed_style(rt: &mut Realm, c: &Call) -> JsResult {
    let n = arg_node(rt, c, 0)?;
    let pseudo = if c.arg(1).is_nullish() { String::new() } else { arg_str(rt, c, 1)? };
    let pseudo = match pseudo.trim_start_matches(':').to_ascii_lowercase().as_str() {
        "before" => 1,
        "after" => 2,
        _ => 0,
    };
    let proto = with_dom(rt, |_, dom| dom.protos.computed_style);
    Ok(Value::Object(rt.alloc(Obj::new(proto, Kind::Host(COMPUTED, (n as u64) << 2 | pseudo)))))
}

/// The read-only declaration prototype.
pub(super) fn install(rt: &mut Realm, object_proto: ObjRef) -> ObjRef {
    let proto = rt.new_object_with(Some(object_proto));
    interface(rt, "CSSStyleDeclaration", proto, None);
    methods(
        rt,
        proto,
        &[
            ("getPropertyValue", 1, get_property_value),
            ("setProperty", 2, read_only),
            ("removeProperty", 1, read_only),
            ("item", 1, item),
            ("getPropertyPriority", 1, empty),
        ],
    );
    accessor(rt, proto, "length", length, None);
    accessor(rt, proto, "cssText", empty, Some(read_only));
    for name in PROPS.iter().chain(SHORTHANDS) {
        accessor_with(rt, proto, name, Value::str(name), property_get, Some(read_only));
        let camel = kebab_to_camel(name);
        if camel != *name {
            accessor_with(rt, proto, &camel, Value::str(name), property_get, Some(read_only));
        }
    }
    accessor_with(rt, proto, "cssFloat", Value::str("float"), property_get, Some(read_only));
    proto
}

fn kebab_to_camel(s: &str) -> String {
    let mut out = String::new();
    let mut up = false;
    for ch in s.chars() {
        if ch == '-' {
            up = true;
        } else if up {
            out.push(ch.to_ascii_uppercase());
            up = false;
        } else {
            out.push(ch);
        }
    }
    out
}
