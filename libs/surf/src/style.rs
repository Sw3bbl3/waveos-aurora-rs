//! Computed styles: the cascade (user agent sheet, presentational
//! attributes, author sheets, style attributes), inheritance and custom
//! properties.

use crate::css::{self, Declaration, Stylesheet};
use crate::dom::{Document, NodeData, NodeId};
use crate::values::{self, Len, Units};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Table,
    TableRowGroup,
    TableRow,
    TableCell,
    TableCaption,
    Flex,
    InlineFlex,
    Grid,
}

impl Display {
    pub fn is_inline(self) -> bool {
        matches!(self, Display::Inline | Display::InlineBlock | Display::InlineFlex)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WhiteSpace {
    Normal,
    Pre,
    NoWrap,
    PreWrap,
    PreLine,
}

impl WhiteSpace {
    pub fn keeps_spaces(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap)
    }
    pub fn keeps_newlines(self) -> bool {
        matches!(self, WhiteSpace::Pre | WhiteSpace::PreWrap | WhiteSpace::PreLine)
    }
    pub fn wraps(self) -> bool {
        !matches!(self, WhiteSpace::Pre | WhiteSpace::NoWrap)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListStyle {
    None,
    Disc,
    Circle,
    Square,
    Decimal,
    LowerAlpha,
    UpperAlpha,
    LowerRoman,
    UpperRoman,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Float {
    None,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transform {
    None,
    Upper,
    Lower,
    Capitalize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    Normal,
    Factor(f32),
    Px(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    SpaceBetween,
    SpaceAround,
}

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub color: u32,
    pub background: u32,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
    pub underline: bool,
    pub strike: bool,
    pub align: Align,
    /// `text-align: -webkit-center` (<center>): also centres child blocks.
    pub center_blocks: bool,
    pub line_height: LineHeight,
    pub white_space: WhiteSpace,
    pub list_style: ListStyle,
    pub transform: Transform,
    pub visible: bool,
    /// top, right, bottom, left
    pub margin: [Len; 4],
    pub padding: [Len; 4],
    pub border_width: [f32; 4],
    pub border_on: [bool; 4],
    pub border_color: [Option<u32>; 4],
    pub radius: f32,
    pub width: Len,
    pub min_width: Len,
    pub max_width: Len,
    pub height: Len,
    pub min_height: Len,
    pub max_height: Len,
    pub border_box: bool,
    pub position: Position,
    pub float: Float,
    pub clear: bool,
    pub overflow_hidden: bool,
    pub offsets: [Len; 4],
    pub clipped: bool,
    pub opacity: f32,
    pub flex_column: bool,
    pub flex_wrap: bool,
    pub justify: Justify,
    pub center_items: bool,
    pub gap: f32,
    pub flex_grow: f32,
    pub vertical_middle: bool,
    pub background_image: bool,
    /// A mask-image (icons drawn by masking a colour): we can't mask, so skip the fill.
    pub masked: bool,
    /// Custom properties (inherited), shared until changed.
    pub vars: Rc<BTreeMap<String, String>>,
}

pub const DEFAULT_LINK: u32 = 0xFF1A_0DAB;

impl Style {
    pub fn root() -> Style {
        Style {
            display: Display::Block,
            color: 0xFF00_0000,
            background: 0,
            font_size: 16.0,
            bold: false,
            italic: false,
            mono: false,
            underline: false,
            strike: false,
            align: Align::Left,
            center_blocks: false,
            line_height: LineHeight::Normal,
            white_space: WhiteSpace::Normal,
            list_style: ListStyle::Disc,
            transform: Transform::None,
            visible: true,
            margin: [Len::ZERO; 4],
            padding: [Len::ZERO; 4],
            border_width: [3.0; 4],
            border_on: [false; 4],
            border_color: [None; 4],
            radius: 0.0,
            width: Len::Auto,
            min_width: Len::Auto,
            max_width: Len::Auto,
            height: Len::Auto,
            min_height: Len::Auto,
            max_height: Len::Auto,
            border_box: false,
            position: Position::Static,
            float: Float::None,
            clear: false,
            overflow_hidden: false,
            offsets: [Len::Auto; 4],
            clipped: false,
            opacity: 1.0,
            flex_column: false,
            flex_wrap: false,
            justify: Justify::Start,
            center_items: false,
            gap: 0.0,
            flex_grow: 0.0,
            vertical_middle: false,
            background_image: false,
            masked: false,
            vars: Rc::new(BTreeMap::new()),
        }
    }

    /// A child's starting point: inherited properties kept, the rest reset.
    pub fn inherit(&self) -> Style {
        let mut s = Style::root();
        s.color = self.color;
        s.font_size = self.font_size;
        s.bold = self.bold;
        s.italic = self.italic;
        s.mono = self.mono;
        s.align = self.align;
        s.center_blocks = self.center_blocks;
        s.line_height = self.line_height;
        s.white_space = self.white_space;
        s.list_style = self.list_style;
        s.transform = self.transform;
        s.visible = self.visible;
        // Decorations are drawn across descendants; inheriting is close enough.
        s.underline = self.underline;
        s.strike = self.strike;
        s.vars = self.vars.clone();
        s.display = Display::Inline;
        s
    }

    /// Border widths that are actually drawn.
    pub fn borders(&self) -> [f32; 4] {
        let mut b = [0.0; 4];
        for i in 0..4 {
            if self.border_on[i] {
                b[i] = self.border_width[i];
            }
        }
        b
    }

    /// The used line height in px.
    pub fn line_px(&self) -> f32 {
        match self.line_height {
            LineHeight::Normal => self.font_size * 1.25,
            LineHeight::Factor(f) => self.font_size * f,
            LineHeight::Px(p) => p,
        }
    }

    /// Takes the element out of the page (display: none, or visually hidden).
    pub fn hidden(&self) -> bool {
        self.display == Display::None || self.clipped
    }
}

/// The user agent stylesheet.
pub const UA_CSS: &str = r#"
head, script, style, title, meta, link, template, base, noembed, param, source, track, datalist, area, map,
svg, canvas, iframe, video, audio, object, embed, dialog:not([open]), [hidden], input[type=hidden], rp { display: none }
html, body, div, section, article, aside, nav, header, footer, main, p, h1, h2, h3, h4, h5, h6, ul, ol, dl, dt, dd,
blockquote, pre, figure, figcaption, form, fieldset, legend, address, hr, details, summary, center, menu, dir, hgroup,
noscript, search, optgroup, picture > source { display: block }
li { display: list-item }
table { display: table; border-collapse: separate }
thead, tbody, tfoot { display: table-row-group }
tr { display: table-row }
td, th { display: table-cell; padding: 1px; vertical-align: middle }
caption { display: table-caption; text-align: center }
body { margin: 8px; line-height: normal }
p, dl, ul, ol, menu, dir, pre { margin-top: 1em; margin-bottom: 1em }
blockquote, figure { margin: 1em 40px }
h1 { font-size: 2em; margin: .67em 0; font-weight: bold }
h2 { font-size: 1.5em; margin: .83em 0; font-weight: bold }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold }
h4 { margin: 1.33em 0; font-weight: bold }
h5 { font-size: .83em; margin: 1.67em 0; font-weight: bold }
h6 { font-size: .67em; margin: 2.33em 0; font-weight: bold }
ul, ol, menu, dir { padding-left: 40px }
ul, menu, dir { list-style-type: disc }
ol { list-style-type: decimal }
ul ul, ol ul { list-style-type: circle }
ul ul ul, ol ul ul, ul ol ul { list-style-type: square }
li ul, li ol { margin-top: 0; margin-bottom: 0 }
dd { margin-left: 40px }
b, strong, th, legend, summary, dt { font-weight: bold }
i, em, cite, var, dfn, address { font-style: italic }
code, kbd, samp, pre, tt, xmp, listing, plaintext { font-family: monospace }
code, kbd, samp, tt { font-size: .9em }
pre, xmp, listing, plaintext { white-space: pre }
a:link { color: #1a0dab; text-decoration: underline }
u, ins { text-decoration: underline }
s, strike, del { text-decoration: line-through }
small, sub, sup { font-size: .83em }
big { font-size: 1.17em }
mark { background-color: yellow; color: black }
hr { border: 0; border-top: 1px solid #b8b8b8; margin: .5em 0 }
center { text-align: -webkit-center }
nobr { white-space: nowrap }
th { text-align: center }
fieldset { margin: 0 2px; padding: .35em .75em .625em; border: 1px solid #c0c0c0 }
input, button, select, textarea { font-size: 13.33px; font-family: sans-serif }
textarea { font-family: monospace }
button, input[type=submit], input[type=button], input[type=reset] { padding: 2px 8px; border: 1px solid #a0a0a0;
  background-color: #efefef; border-radius: 4px }
input:not([type]), input[type=text], input[type=search], input[type=email], input[type=url], input[type=password],
input[type=number], input[type=tel], textarea, select { border: 1px solid #a0a0a0; padding: 2px 4px; background-color: #fff }
abbr[title] { text-decoration: none }
"#;

/// Extra rules for pages without a doctype (quirks mode): tables don't
/// inherit text alignment or font settings.
pub const QUIRKS_CSS: &str = r#"
table { text-align: left; font-size: medium; font-weight: normal; font-style: normal; white-space: normal; line-height: normal }
"#;

/// Where a declaration came from, weakest first.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Origin {
    Agent,
    Hint,
    Author,
    Inline,
}

struct Indexed {
    origin: Origin,
    sheet: usize,
    rule: usize,
    selector: usize,
}

/// The rules of all sheets, indexed by their subject's id/class/tag.
pub struct Cascade<'a> {
    sheets: Vec<(&'a Stylesheet, Origin)>,
    by_key: [BTreeMap<String, Vec<usize>>; 3],
    universal: Vec<usize>,
    entries: Vec<Indexed>,
    /// Per sheet, per rule: does its @media hold?
    media_ok: Vec<Vec<bool>>,
}

impl<'a> Cascade<'a> {
    pub fn new(agent: &'a Stylesheet, author: &[&'a Stylesheet], width: i32, height: i32) -> Cascade<'a> {
        let mut c = Cascade {
            sheets: Vec::new(),
            by_key: [BTreeMap::new(), BTreeMap::new(), BTreeMap::new()],
            universal: Vec::new(),
            entries: Vec::new(),
            media_ok: Vec::new(),
        };
        c.sheets.push((agent, Origin::Agent));
        for s in author {
            c.sheets.push((s, Origin::Author));
        }
        for (si, (sheet, origin)) in c.sheets.iter().enumerate() {
            let mut ok = Vec::with_capacity(sheet.rules.len());
            for (ri, rule) in sheet.rules.iter().enumerate() {
                let m = rule.media.iter().all(|q| css::media_matches(q, width, height));
                ok.push(m);
                if !m {
                    continue;
                }
                for (xi, sel) in rule.selectors.iter().enumerate() {
                    let idx = c.entries.len();
                    c.entries.push(Indexed { origin: *origin, sheet: si, rule: ri, selector: xi });
                    let (kind, key) = css::bucket_key(sel);
                    if kind == 3 {
                        c.universal.push(idx);
                    } else {
                        c.by_key[kind as usize].entry(key).or_default().push(idx);
                    }
                }
            }
            c.media_ok.push(ok);
        }
        c
    }

    /// The matching declarations for `node`, in cascade order (weakest first).
    fn declarations(&self, doc: &Document, node: NodeId) -> Vec<(u32, (u16, u16, u16), usize, &'a Declaration)> {
        let Some(e) = doc.element(node) else { return Vec::new() };
        let mut candidates: Vec<usize> = self.universal.clone();
        if let Some(id) = e.id() {
            if let Some(v) = self.by_key[0].get(id) {
                candidates.extend(v);
            }
        }
        for cl in e.classes() {
            if let Some(v) = self.by_key[1].get(cl) {
                candidates.extend(v);
            }
        }
        if let Some(v) = self.by_key[2].get(e.tag.as_str()) {
            candidates.extend(v);
        }
        candidates.sort_unstable();
        candidates.dedup();
        let mut out = Vec::new();
        for idx in candidates {
            let ent = &self.entries[idx];
            let (sheet, _) = self.sheets[ent.sheet];
            let rule = &sheet.rules[ent.rule];
            let sel = &rule.selectors[ent.selector];
            if css::matches(doc, node, sel) {
                for d in &rule.decls {
                    // Order: sheet, then rule, then declaration position.
                    out.push((rank(ent.origin, d.important), sel.specificity, (ent.sheet << 40) | (ent.rule << 12), d));
                }
            }
        }
        out
    }
}

fn rank(origin: Origin, important: bool) -> u32 {
    // Important declarations beat normal ones; among them, author beats agent.
    origin as u32 + if important { 10 } else { 0 }
}

/// Computes every element's style. Text nodes share their parent's.
pub fn compute(doc: &Document, cascade: &Cascade, viewport: (i32, i32)) -> Vec<Option<Rc<Style>>> {
    let mut styles: Vec<Option<Rc<Style>>> = alloc::vec![None; doc.nodes.len()];
    let root = Rc::new(Style::root());
    let mut root_font = 16.0;
    let order = doc.descendants(Document::ROOT);
    for n in order {
        let parent_style = doc.parent(n).and_then(|p| styles[p].clone()).unwrap_or_else(|| root.clone());
        match &doc.nodes[n].data {
            NodeData::Text(_) => styles[n] = Some(parent_style),
            NodeData::Element(e) => {
                // Hidden subtrees need no styles.
                if parent_style.display == Display::None {
                    styles[n] = Some(parent_style);
                    continue;
                }
                let mut decls = cascade.declarations(doc, n);
                let hints = hints(doc, n);
                for (i, d) in hints.iter().enumerate() {
                    decls.push((rank(Origin::Hint, false), (0, 0, 0), i, d));
                }
                let inline = e.attr("style").map(css::parse_declarations).unwrap_or_default();
                for (i, d) in inline.iter().enumerate() {
                    decls.push((rank(Origin::Inline, d.important), (1, 0, 0), i, d));
                }
                // Stable sort keeps declaration order within a rule.
                decls.sort_by(|a, b| (a.0, a.1, a.2).cmp(&(b.0, b.1, b.2)));
                let mut s = parent_style.inherit();
                let units = |s: &Style, root_font: f32| Units {
                    em: s.font_size,
                    rem: root_font,
                    vw: viewport.0 as f32,
                    vh: viewport.1 as f32,
                };
                // Custom properties first, so var() sees them.
                let custom: Vec<&Declaration> = decls.iter().map(|d| d.3).filter(|d| d.name.starts_with("--")).collect();
                if !custom.is_empty() {
                    let mut vars = (*s.vars).clone();
                    for d in custom {
                        vars.insert(d.name.clone(), d.value.clone());
                    }
                    s.vars = Rc::new(vars);
                }
                let vars = s.vars.clone();
                let lookup = |name: &str| vars.get(name).cloned();
                let resolved: Vec<(String, String)> = decls
                    .iter()
                    .map(|d| d.3)
                    .filter(|d| !d.name.starts_with("--"))
                    .filter_map(|d| Some((d.name.clone(), values::substitute_vars(&d.value, &lookup, 0)?)))
                    .collect();
                // Font size first: em units depend on it.
                for (name, value) in resolved.iter().filter(|(n, _)| n == "font-size" || n == "font") {
                    apply(&mut s, name, value, &parent_style, units(&parent_style, root_font));
                }
                if e.tag == "html" {
                    root_font = s.font_size;
                }
                let own = units(&s, root_font);
                for (name, value) in resolved.iter().filter(|(n, _)| n != "font-size" && n != "font") {
                    apply(&mut s, name, value, &parent_style, own);
                }
                fixups(&mut s, &e.tag, &parent_style);
                styles[n] = Some(Rc::new(s));
            }
            NodeData::Document => {}
        }
    }
    styles
}

/// Post-cascade adjustments.
fn fixups(s: &mut Style, tag: &str, parent: &Style) {
    // Floats and absolutely positioned boxes are blockified.
    if (s.float != Float::None || matches!(s.position, Position::Absolute | Position::Fixed)) && s.display.is_inline() {
        s.display = Display::Block;
    }
    // Visually hidden patterns (screen-reader text, off-screen skip links).
    if matches!(s.position, Position::Absolute | Position::Fixed) {
        let tiny = |l: Len| matches!(l, Len::Calc(p, px) if p == 0.0 && px <= 1.0);
        let off = |l: Len| matches!(l, Len::Calc(_, px) if px <= -500.0);
        if (tiny(s.width) && tiny(s.height)) || s.offsets.iter().any(|o| off(*o)) {
            s.clipped = true;
        }
    }
    if s.overflow_hidden && matches!(s.height, Len::Calc(p, px) if p == 0.0 && px <= 1.0) {
        s.clipped = true;
    }
    if s.opacity <= 0.01 && tag != "html" && tag != "body" {
        s.visible = false;
    }
    let _ = parent;
}

/// Presentational attributes as declarations.
fn hints(doc: &Document, node: NodeId) -> Vec<Declaration> {
    let e = doc.element(node).unwrap();
    let mut out = Vec::new();
    let mut add = |name: &str, value: String| out.push(Declaration { name: String::from(name), value, important: false });
    let dim = |v: &str| {
        let v = v.trim();
        if v.ends_with('%') {
            String::from(v)
        } else {
            let n: String = v.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
            if n.is_empty() {
                String::new()
            } else {
                format!("{n}px")
            }
        }
    };
    let tag = e.tag.as_str();
    if let Some(c) = e.attr("bgcolor") {
        add("background-color", legacy_color(c));
    }
    if tag == "body" {
        if let Some(c) = e.attr("text") {
            add("color", legacy_color(c));
        }
    }
    if tag == "font" {
        if let Some(c) = e.attr("color") {
            add("color", legacy_color(c));
        }
        if let Some(sz) = e.attr("size") {
            let sz = sz.trim();
            let n: i32 = sz.trim_start_matches(['+', '-']).parse().unwrap_or(3);
            let n = if sz.starts_with('+') { 3 + n } else if sz.starts_with('-') { 3 - n } else { n };
            let px = [10, 10, 13, 16, 18, 24, 32, 48][n.clamp(0, 7) as usize];
            add("font-size", format!("{px}px"));
        }
        if let Some(face) = e.attr("face") {
            add("font-family", String::from(face));
        }
    }
    if let Some(a) = e.attr("align") {
        let a = a.to_ascii_lowercase();
        match tag {
            "table" if a == "center" => {
                add("margin-left", String::from("auto"));
                add("margin-right", String::from("auto"));
            }
            "img" | "table" if a == "left" || a == "right" => add("float", a),
            _ => add("text-align", a),
        }
    }
    if matches!(tag, "td" | "th" | "tr" | "tbody" | "thead") {
        if let Some(v) = e.attr("valign") {
            add("vertical-align", String::from(v));
        }
    }
    if matches!(tag, "table" | "td" | "th" | "img" | "hr" | "col" | "iframe" | "input" | "canvas" | "video" | "embed") {
        if let Some(w) = e.attr("width") {
            let w = dim(w);
            if !w.is_empty() {
                add("width", w);
            }
        }
        if let Some(h) = e.attr("height") {
            let h = dim(h);
            if !h.is_empty() && tag != "td" && tag != "th" && tag != "table" {
                add("height", h);
            }
        }
    }
    if matches!(tag, "td" | "th") && e.attr("nowrap").is_some() {
        add("white-space", String::from("nowrap"));
    }
    if tag == "table" {
        if let Some(b) = e.attr("border") {
            let w: i32 = b.trim().parse().unwrap_or(1);
            if w > 0 {
                add("border", format!("{w}px outset #888"));
            }
        }
    }
    if matches!(tag, "td" | "th") {
        // Attributes of the enclosing table: border, cellpadding.
        let mut p = doc.parent(node);
        while let Some(n) = p {
            if doc.tag(n) == "table" {
                let t = doc.element(n).unwrap();
                if t.attr("border").and_then(|b| b.trim().parse::<i32>().ok()).unwrap_or(0) > 0 {
                    add("border", String::from("1px inset #888"));
                }
                if let Some(cp) = t.attr("cellpadding") {
                    add("padding", dim(cp));
                }
                break;
            }
            p = doc.parent(n);
        }
    }
    if tag == "hr" {
        if let Some(c) = e.attr("color") {
            add("border-top-color", legacy_color(c));
        }
        if e.attr("noshade").is_some() {
            add("border-top-width", String::from("2px"));
        }
    }
    if matches!(tag, "ul" | "ol") {
        if let Some(t) = e.attr("type") {
            let v = match t {
                "1" => "decimal",
                "a" => "lower-alpha",
                "A" => "upper-alpha",
                "i" => "lower-roman",
                "I" => "upper-roman",
                "circle" => "circle",
                "square" => "square",
                _ => "disc",
            };
            add("list-style-type", String::from(v));
        }
    }
    out
}

/// Legacy attribute colours: names, or hex without '#'.
fn legacy_color(c: &str) -> String {
    let c = c.trim();
    if c.len() == 6 && c.chars().all(|x| x.is_ascii_hexdigit()) {
        format!("#{c}")
    } else {
        String::from(c)
    }
}

fn side_values(v: &str) -> Vec<&str> {
    css::split_top(v, b' ').into_iter().map(str::trim).filter(|s| !s.is_empty()).collect()
}

/// Expands 1–4 values to top/right/bottom/left.
fn four<T: Copy>(vals: &[T]) -> Option<[T; 4]> {
    Some(match vals.len() {
        1 => [vals[0]; 4],
        2 => [vals[0], vals[1], vals[0], vals[1]],
        3 => [vals[0], vals[1], vals[2], vals[1]],
        4 => [vals[0], vals[1], vals[2], vals[3]],
        _ => return None,
    })
}

fn side_index(s: &str) -> Option<usize> {
    Some(match s {
        "top" | "block-start" => 0,
        "right" | "inline-end" => 1,
        "bottom" | "block-end" => 2,
        "left" | "inline-start" => 3,
        _ => return None,
    })
}

fn border_width(v: &str, u: &Units) -> Option<f32> {
    match v {
        "thin" => Some(1.0),
        "medium" => Some(3.0),
        "thick" => Some(5.0),
        v => values::px_length(v, u).map(|w| w.max(0.0)),
    }
}

const BORDER_STYLES: &[&str] =
    &["none", "hidden", "solid", "dashed", "dotted", "double", "groove", "ridge", "inset", "outset"];

/// "1px solid red" → (width, style on, colour), each optional.
fn border_parts(v: &str, current: u32, u: &Units) -> (Option<f32>, Option<bool>, Option<u32>) {
    let (mut w, mut on, mut c) = (None, None, None);
    for part in side_values(v) {
        let lower = part.to_ascii_lowercase();
        if BORDER_STYLES.contains(&lower.as_str()) {
            on = Some(lower != "none" && lower != "hidden");
        } else if let Some(x) = border_width(&lower, u) {
            w = Some(x);
        } else if let Some(col) = values::color(part, current) {
            c = Some(col);
        }
    }
    (w, on, c)
}

fn font_size(v: &str, parent: &Style, u: &Units) -> Option<f32> {
    let base = parent.font_size;
    Some(match v {
        "xx-small" => 9.0,
        "x-small" => 10.0,
        "small" => 13.0,
        "medium" => 16.0,
        "large" => 18.0,
        "x-large" => 24.0,
        "xx-large" => 32.0,
        "xxx-large" => 48.0,
        "smaller" => base / 1.2,
        "larger" => base * 1.2,
        v => {
            // em and % are relative to the parent's size.
            let pu = Units { em: base, ..*u };
            match values::length(v, &pu)? {
                Len::Calc(p, px) => p * base / 100.0 + px,
                Len::Auto => return None,
            }
        }
    })
}

fn is_mono(family: &str) -> bool {
    let f = family.to_ascii_lowercase();
    let first = f.split(',').next().unwrap_or("");
    ["mono", "courier", "consolas", "menlo", "monaco", "code", "console"].iter().any(|k| first.contains(k))
}

/// Applies one declaration.
fn apply(s: &mut Style, name: &str, value: &str, parent: &Style, u: Units) {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    let lv = lower.as_str();
    if lv == "inherit" {
        inherit_property(s, name, parent);
        return;
    }
    if lv == "initial" || lv == "unset" || lv == "revert" || lv == "revert-layer" {
        let fresh = if lv == "unset" { parent.inherit() } else { Style::root() };
        inherit_property(s, name, &fresh);
        return;
    }
    let len = |v: &str| values::length(v, &u);
    match name {
        "display" => {
            let first = lv.split_whitespace().next().unwrap_or("");
            s.display = match (first, lv) {
                ("none", _) => Display::None,
                (_, "inline-block") | (_, "inline flow-root") => Display::InlineBlock,
                (_, "inline-flex") | (_, "inline-grid") => Display::InlineFlex,
                ("inline", _) | ("contents", _) | ("ruby", _) => Display::Inline,
                ("list-item", _) => Display::ListItem,
                ("table", _) => Display::Table,
                ("inline-table", _) => Display::InlineBlock,
                ("table-row-group", _) | ("table-header-group", _) | ("table-footer-group", _) => Display::TableRowGroup,
                ("table-row", _) => Display::TableRow,
                ("table-cell", _) => Display::TableCell,
                ("table-caption", _) => Display::TableCaption,
                ("flex", _) | ("-webkit-box", _) | ("-webkit-flex", _) | ("-ms-flexbox", _) => Display::Flex,
                ("grid", _) | ("-ms-grid", _) => Display::Grid,
                ("block", _) | ("flow-root", _) | ("flow", _) => Display::Block,
                _ => s.display,
            }
        }
        "color" => {
            if let Some(c) = values::color(v, parent.color) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = values::color(v, s.color) {
                s.background = c;
            }
        }
        "background" => {
            if lv == "none" {
                s.background = 0;
            }
            if lv.contains("url(") || lv.contains("gradient(") {
                s.background_image = true;
            }
            // The colour is usually the last component.
            for part in side_values(v).iter().rev() {
                if let Some(c) = values::color(part, s.color) {
                    s.background = c;
                    break;
                }
            }
            // A gradient: take its first colour stop.
            if let Some(p) = lv.find("gradient(") {
                let args = &v[p + 9..];
                for arg in css::split_top(args.trim_end_matches(')'), b',') {
                    let first = side_values(arg).first().copied().unwrap_or("");
                    if let Some(c) = values::color(first, s.color) {
                        s.background = c;
                        break;
                    }
                }
            }
        }
        "background-image" => s.background_image = lv != "none",
        "mask" | "mask-image" | "-webkit-mask" | "-webkit-mask-image" => s.masked = lv.contains("url("),
        "font-size" => {
            if let Some(px) = font_size(lv, parent, &u) {
                s.font_size = px.clamp(1.0, 400.0);
            }
        }
        "font-weight" => {
            s.bold = match lv {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => n.parse::<u32>().map(|w| w >= 600).unwrap_or(s.bold),
            }
        }
        "font-style" => s.italic = lv.starts_with("italic") || lv.starts_with("oblique"),
        "font-family" => s.mono = is_mono(lv),
        "font" => {
            // [style] [variant] [weight] size[/line-height] family
            let parts = side_values(v);
            let mut italic = false;
            let mut bold = false;
            for (i, p) in parts.iter().enumerate() {
                let pl = p.to_ascii_lowercase();
                match pl.as_str() {
                    "italic" | "oblique" => italic = true,
                    "bold" | "bolder" => bold = true,
                    "600" | "700" | "800" | "900" => bold = true,
                    _ => {
                        let (size, lh) = pl.split_once('/').map_or((pl.as_str(), None), |(a, b)| (a, Some(b)));
                        if let Some(px) = font_size(size, parent, &u) {
                            s.font_size = px.clamp(1.0, 400.0);
                            s.italic = italic;
                            s.bold = bold;
                            if let Some(lh) = lh {
                                apply(s, "line-height", lh, parent, u);
                            } else {
                                s.line_height = LineHeight::Normal;
                            }
                            s.mono = is_mono(&parts[i + 1..].join(" "));
                            break;
                        }
                    }
                }
            }
        }
        "text-decoration" | "text-decoration-line" => {
            if lv.contains("none") {
                s.underline = false;
                s.strike = false;
            }
            if lv.contains("underline") {
                s.underline = true;
            }
            if lv.contains("line-through") {
                s.strike = true;
            }
        }
        "text-align" => {
            s.center_blocks = matches!(lv, "-webkit-center" | "-moz-center");
            s.align = match lv {
                "center" | "-webkit-center" | "-moz-center" => Align::Center,
                "right" | "end" | "-webkit-right" => Align::Right,
                "justify" => Align::Justify,
                "left" | "start" | "-webkit-left" => Align::Left,
                _ => s.align,
            }
        }
        "line-height" => {
            s.line_height = if lv == "normal" {
                LineHeight::Normal
            } else if let Ok(f) = lv.parse::<f32>() {
                LineHeight::Factor(f)
            } else {
                match len(lv) {
                    Some(Len::Calc(p, px)) => LineHeight::Px(p * s.font_size / 100.0 + px),
                    _ => s.line_height,
                }
            }
        }
        "white-space" | "white-space-collapse" | "text-wrap-mode" => {
            s.white_space = match lv {
                "pre" => WhiteSpace::Pre,
                "nowrap" => WhiteSpace::NoWrap,
                "pre-wrap" | "break-spaces" | "preserve" => WhiteSpace::PreWrap,
                "pre-line" | "preserve-breaks" => WhiteSpace::PreLine,
                "normal" | "collapse" | "wrap" => WhiteSpace::Normal,
                _ => s.white_space,
            }
        }
        "list-style-type" | "list-style" => {
            for part in lv.split_whitespace() {
                s.list_style = match part {
                    "none" => ListStyle::None,
                    "disc" => ListStyle::Disc,
                    "circle" => ListStyle::Circle,
                    "square" => ListStyle::Square,
                    "decimal" | "decimal-leading-zero" => ListStyle::Decimal,
                    "lower-alpha" | "lower-latin" => ListStyle::LowerAlpha,
                    "upper-alpha" | "upper-latin" => ListStyle::UpperAlpha,
                    "lower-roman" => ListStyle::LowerRoman,
                    "upper-roman" => ListStyle::UpperRoman,
                    _ => continue,
                };
            }
        }
        "text-transform" => {
            s.transform = match lv {
                "uppercase" => Transform::Upper,
                "lowercase" => Transform::Lower,
                "capitalize" => Transform::Capitalize,
                _ => Transform::None,
            }
        }
        "visibility" => s.visible = lv == "visible",
        "opacity" => {
            if let Ok(o) = lv.trim_end_matches('%').parse::<f32>() {
                s.opacity = if lv.ends_with('%') { o / 100.0 } else { o };
            }
        }
        "margin" | "padding" => {
            let vals: Option<Vec<Len>> = side_values(v).iter().map(|p| len(p)).collect();
            if let Some(sides) = vals.as_deref().and_then(four) {
                if name == "margin" {
                    s.margin = sides;
                } else {
                    s.padding = sides.map(|l| if l.is_auto() { Len::ZERO } else { l });
                }
            }
        }
        "margin-inline" | "padding-inline" | "margin-block" | "padding-block" => {
            let vals: Option<Vec<Len>> = side_values(v).iter().map(|p| len(p)).collect();
            if let Some(vals) = vals {
                let (a, b) = (vals[0], *vals.get(1).unwrap_or(&vals[0]));
                let (i, j) = if name.ends_with("inline") { (3, 1) } else { (0, 2) };
                let target = if name.starts_with("margin") { &mut s.margin } else { &mut s.padding };
                target[i] = a;
                target[j] = b;
            }
        }
        "border" => {
            let (w, on, c) = border_parts(v, s.color, &u);
            s.border_width = [w.unwrap_or(3.0); 4];
            s.border_on = [on.unwrap_or(false); 4];
            s.border_color = [c; 4];
        }
        "border-width" => {
            let vals: Option<Vec<f32>> = side_values(lv).iter().map(|p| border_width(p, &u)).collect();
            if let Some(w) = vals.as_deref().and_then(four) {
                s.border_width = w;
            }
        }
        "border-style" => {
            let vals: Vec<bool> = side_values(lv).iter().map(|p| *p != "none" && *p != "hidden").collect();
            if let Some(on) = four(&vals) {
                s.border_on = on;
            }
        }
        "border-color" => {
            let vals: Option<Vec<u32>> = side_values(v).iter().map(|p| values::color(p, s.color)).collect();
            if let Some(c) = vals.as_deref().and_then(four) {
                s.border_color = c.map(Some);
            }
        }
        "border-radius" => {
            if let Some(first) = side_values(v).first() {
                if let Some(Len::Calc(p, px)) = len(first) {
                    s.radius = if p > 0.0 { 9999.0 } else { px };
                }
            }
        }
        "width" => s.width = len(v).unwrap_or(s.width),
        "min-width" => s.min_width = len(v).unwrap_or(s.min_width),
        "max-width" => s.max_width = if lv == "none" { Len::Auto } else { len(v).unwrap_or(s.max_width) },
        "height" => s.height = len(v).unwrap_or(s.height),
        "min-height" => s.min_height = len(v).unwrap_or(s.min_height),
        "max-height" => s.max_height = if lv == "none" { Len::Auto } else { len(v).unwrap_or(s.max_height) },
        "inline-size" => s.width = len(v).unwrap_or(s.width),
        "max-inline-size" => s.max_width = len(v).unwrap_or(s.max_width),
        "box-sizing" => s.border_box = lv == "border-box",
        "position" => {
            s.position = match lv {
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" | "-webkit-sticky" => Position::Sticky,
                _ => Position::Static,
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let (Some(i), Some(l)) = (side_index(name), len(v)) {
                s.offsets[i] = l;
            }
        }
        "float" => {
            s.float = match lv {
                "left" | "inline-start" => Float::Left,
                "right" | "inline-end" => Float::Right,
                _ => Float::None,
            }
        }
        "clear" => s.clear = lv != "none",
        "overflow" | "overflow-y" => s.overflow_hidden = lv.starts_with("hidden") || lv == "clip",
        "clip" => s.clipped |= lv.starts_with("rect(0") || lv.starts_with("rect(1px"),
        "clip-path" => s.clipped |= lv.starts_with("inset(50%") || lv.starts_with("inset(100%"),
        "flex-direction" => s.flex_column = lv.starts_with("column"),
        "flex-flow" => {
            s.flex_column = lv.contains("column");
            s.flex_wrap = lv.contains("wrap") && !lv.contains("nowrap");
        }
        "flex-wrap" => s.flex_wrap = lv == "wrap" || lv == "wrap-reverse",
        "justify-content" => {
            s.justify = match lv {
                "center" => Justify::Center,
                "flex-end" | "end" | "right" => Justify::End,
                "space-between" => Justify::SpaceBetween,
                "space-around" | "space-evenly" => Justify::SpaceAround,
                _ => Justify::Start,
            }
        }
        "align-items" => s.center_items = lv == "center",
        "gap" | "column-gap" | "grid-gap" | "grid-column-gap" => {
            if let Some(first) = side_values(v).last() {
                if let Some(px) = values::px_length(first, &u) {
                    s.gap = px;
                }
            }
        }
        "flex" => {
            let first = lv.split_whitespace().next().unwrap_or("");
            s.flex_grow = match first {
                "auto" => 1.0,
                "none" => 0.0,
                n => n.parse().unwrap_or(0.0),
            };
        }
        "flex-grow" => s.flex_grow = lv.parse().unwrap_or(0.0),
        "vertical-align" => s.vertical_middle = lv == "middle",
        _ => {
            if let Some(rest) = name.strip_prefix("margin-").or(name.strip_prefix("padding-")) {
                if let (Some(i), Some(l)) = (side_index(rest), len(v)) {
                    if name.starts_with("margin") {
                        s.margin[i] = l;
                    } else if !l.is_auto() {
                        s.padding[i] = l;
                    }
                }
                return;
            }
            if let Some(rest) = name.strip_prefix("border-") {
                let (side, prop) = rest.split_once('-').map_or((rest, ""), |(a, b)| (a, b));
                if let Some(i) = side_index(side) {
                    match prop {
                        "" => {
                            let (w, on, c) = border_parts(v, s.color, &u);
                            s.border_width[i] = w.unwrap_or(3.0);
                            s.border_on[i] = on.unwrap_or(false);
                            s.border_color[i] = c;
                        }
                        "width" => {
                            if let Some(w) = border_width(lv, &u) {
                                s.border_width[i] = w;
                            }
                        }
                        "style" => s.border_on[i] = lv != "none" && lv != "hidden",
                        "color" => s.border_color[i] = values::color(v, s.color),
                        _ => {}
                    }
                }
            }
        }
    }
}

/// Copies one property from `from` (for `inherit`, `initial`, `unset`).
fn inherit_property(s: &mut Style, name: &str, from: &Style) {
    match name {
        "color" => s.color = from.color,
        "background-color" | "background" => s.background = from.background,
        "font-size" => s.font_size = from.font_size,
        "font-weight" => s.bold = from.bold,
        "font-style" => s.italic = from.italic,
        "font-family" => s.mono = from.mono,
        "text-align" => s.align = from.align,
        "line-height" => s.line_height = from.line_height,
        "white-space" => s.white_space = from.white_space,
        "display" => s.display = from.display,
        "text-decoration" => {
            s.underline = from.underline;
            s.strike = from.strike;
        }
        "width" => s.width = from.width,
        "max-width" => s.max_width = from.max_width,
        "height" => s.height = from.height,
        "margin" => s.margin = from.margin,
        "padding" => s.padding = from.padding,
        "border" => {
            s.border_on = from.border_on;
            s.border_width = from.border_width;
            s.border_color = from.border_color;
        }
        "visibility" => s.visible = from.visible,
        "list-style-type" | "list-style" => s.list_style = from.list_style,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::html;

    fn styled(src: &str, css_src: &str, id: &str) -> Style {
        let doc = html::parse(src);
        let ua = css::parse(UA_CSS);
        let author = css::parse(css_src);
        let cascade = Cascade::new(&ua, &[&author], 1000, 800);
        let styles = compute(&doc, &cascade, (1000, 800));
        let n = doc.descendants(Document::ROOT).into_iter().find(|n| doc.element(*n).is_some_and(|e| e.id() == Some(id))).unwrap();
        (*styles[n].clone().unwrap()).clone()
    }

    #[test]
    fn cascade_order() {
        let css_src = "p { color: red } #x { color: blue } .c { color: green !important } p { font-size: 20px; margin: 1em 2px }";
        let s = styled("<p id=x class=c style='color: black'>t</p>", css_src, "x");
        assert_eq!(s.color, 0xFF00_8000); // !important wins
        assert_eq!(s.font_size, 20.0);
        assert_eq!(s.margin[0], Len::px(20.0)); // em against its own font size
        assert_eq!(s.margin[1], Len::px(2.0));
        let s = styled("<p id=x style='color: black'>t</p>", css_src, "x");
        assert_eq!(s.color, 0xFF00_0000); // style attribute beats #id
    }

    #[test]
    fn inheritance_vars_and_hints() {
        let css_src = ":root { --fg: #123456; --pad: 3px } div { color: var(--fg); padding: var(--pad) var(--missing, 7px) }";
        let s = styled("<html><body><div><span id=s>x</span></div></body></html>", css_src, "s");
        assert_eq!(s.color, 0xFF12_3456);
        assert_eq!(s.padding[0], Len::ZERO); // padding doesn't inherit
        let s = styled("<div id=d>x</div>", css_src, "d");
        assert_eq!(s.padding, [Len::px(3.0), Len::px(7.0), Len::px(3.0), Len::px(7.0)]);
        let s = styled("<table><tr><td id=t bgcolor=ff6600 align=center>x</td></tr></table>", "", "t");
        assert_eq!(s.background, 0xFFFF_6600);
        assert_eq!(s.align, Align::Center);
        let s = styled("<h1 id=h>x</h1>", "", "h");
        assert_eq!((s.font_size, s.bold, s.display), (32.0, true, Display::Block));
        let s = styled("<span id=sr style='position:absolute;width:1px;height:1px;overflow:hidden'>x</span>", "", "sr");
        assert!(s.hidden());
    }
}
