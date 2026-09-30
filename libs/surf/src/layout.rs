//! Layout: styled nodes to a display list of positioned rectangles, text
//! runs and images.
//!
//! Normal flow (blocks with sibling margin collapsing, inline formatting with
//! line breaking), lists, tables (auto layout), flex rows and floats (both as
//! rows of shrink-to-fit boxes). Text is measured through [`Host`].

use crate::dom::{Document, NodeData, NodeId};
use crate::style::{Align, Display, Float, Justify, ListStyle, Style, Transform};
use crate::values::{round_i, Len};
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontSpec {
    pub size: u16,
    pub bold: bool,
    pub italic: bool,
    pub mono: bool,
}

impl FontSpec {
    pub fn of(s: &Style) -> FontSpec {
        FontSpec { size: round_i(s.font_size).clamp(1, 400) as u16, bold: s.bold, italic: s.italic, mono: s.mono }
    }
}

/// Font metrics in px: ascent and descent both positive.
#[derive(Clone, Copy, Debug)]
pub struct Metrics {
    pub ascent: i32,
    pub descent: i32,
}

/// What layout needs from its environment.
pub trait Host {
    /// The advance width of `text` in px.
    fn measure(&self, font: FontSpec, text: &str) -> i32;
    /// The advance width in 1/64 px (for exact positions along a line).
    fn measure64(&self, font: FontSpec, text: &str) -> i32 {
        self.measure(font, text) * 64
    }
    fn metrics(&self, font: FontSpec) -> Metrics;
    /// An image's natural size, once it has loaded.
    fn image_size(&self, src: &str) -> Option<(u32, u32)>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }
    pub fn right(&self) -> i32 {
        self.x + self.w
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Rect { rect: Rect, color: u32, radius: i32 },
    /// A text run with its baseline at `y`.
    Text { x: i32, y: i32, text: String, font: FontSpec, color: u32, underline: bool, strike: bool },
    Image { rect: Rect, src: String },
    /// A list bullet: filled (disc), hollow (circle) or square.
    Bullet { rect: Rect, color: u32, kind: ListStyle },
}

impl Item {
    fn shift(&mut self, dx: i32, dy: i32) {
        match self {
            Item::Rect { rect, .. } | Item::Image { rect, .. } | Item::Bullet { rect, .. } => {
                rect.x += dx;
                rect.y += dy;
            }
            Item::Text { x, y, .. } => {
                *x += dx;
                *y += dy;
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Link {
    pub rect: Rect,
    /// The <a> element.
    pub node: NodeId,
}

/// A form control's place on the page.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub rect: Rect,
    pub node: NodeId,
}

#[derive(Clone, Debug, Default)]
pub struct Layout {
    pub items: Vec<Item>,
    pub links: Vec<Link>,
    pub fields: Vec<Field>,
    /// Element ids (and <a name>s) with their y, for #fragments.
    pub anchors: Vec<(String, i32)>,
    pub width: i32,
    pub height: i32,
    /// The canvas colour (from <html> or <body>).
    pub background: u32,
}

impl Layout {
    pub fn link_at(&self, x: i32, y: i32) -> Option<NodeId> {
        self.links.iter().rev().find(|l| l.rect.contains(x, y)).map(|l| l.node)
    }
    pub fn field_at(&self, x: i32, y: i32) -> Option<NodeId> {
        self.fields.iter().rev().find(|f| f.rect.contains(x, y)).map(|f| f.node)
    }
    pub fn anchor(&self, name: &str) -> Option<i32> {
        self.anchors.iter().find(|(n, _)| n == name).map(|(_, y)| *y)
    }
}

/// Where an <img> loads from: src, else the first srcset candidate, else lazy-loading attributes.
pub fn image_source(e: &crate::dom::Element) -> Option<String> {
    let lazy = ["data-src", "data-lazy-src", "data-original"].iter().find_map(|a| e.attr(a));
    let src = e.attr("src").map(str::trim).filter(|s| !s.is_empty());
    match src {
        Some(s) if !(s.starts_with("data:") && lazy.is_some()) => return Some(String::from(s)),
        _ => {}
    }
    if let Some(l) = lazy {
        return Some(String::from(l.trim()));
    }
    let set = e.attr("srcset").or(e.attr("data-srcset"))?;
    let first = set.split(',').next()?.split_whitespace().next()?;
    Some(String::from(first))
}

struct BoxOut {
    height: i32,
    /// The background placeholder item, to be stretched (table cells).
    bg: Option<usize>,
}

enum Atom {
    /// A word (or pre-formatted run). `space`: collapsible space before it;
    /// `brk`: a break opportunity before it without a space (pre-wrap).
    Text { text: String, style: Rc<Style>, link: Option<NodeId>, bg: u32, space: bool, brk: bool },
    Break,
    /// Inline padding/border/margin; `space` if a collapsible space precedes it.
    Gap { w: i32, space: bool },
    Box { w: i32, h: i32, baseline: i32, items: Vec<Item>, links: Vec<Link>, fields: Vec<Field>, link: Option<NodeId>, space: bool },
}

struct Placed {
    atom: usize,
    x: i32,
    w: i32,
    /// Part of a split text atom.
    text: Option<String>,
}

/// Generated content (::before / ::after) gets ids with this bit set:
/// `GEN | owner << 1 | side`.
const GEN: usize = 1 << 62;

pub type Generated = BTreeMap<NodeId, [Option<(Rc<Style>, String)>; 2]>;

pub struct Engine<'a> {
    doc: &'a Document,
    styles: &'a [Option<Rc<Style>>],
    generated: &'a Generated,
    host: &'a dyn Host,
    out: Layout,
    link: Option<NodeId>,
    first_baseline: Option<i32>,
    pref_cache: Vec<Option<(i32, i32)>>,
    depth: u32,
    /// The top of the line being built (for inline anchors).
    pending_y: i32,
}

pub fn layout(doc: &Document, styles: &[Option<Rc<Style>>], generated: &Generated, host: &dyn Host, width: i32) -> Layout {
    let mut e = Engine {
        doc,
        styles,
        generated,
        host,
        out: Layout { width, ..Default::default() },
        link: None,
        first_baseline: None,
        pref_cache: alloc::vec![None; doc.nodes.len()],
        depth: 0,
        pending_y: 0,
    };
    let html = doc.find("html");
    let body = doc.find("body");
    let bg_of = |n: Option<NodeId>| n.and_then(|n| styles[n].as_ref()).map_or(0, |s| s.background);
    e.out.background = if bg_of(html) >> 24 != 0 { bg_of(html) } else { bg_of(body) };
    if e.out.background >> 24 == 0 {
        e.out.background = 0xFFFF_FFFF;
    }
    let root = html.unwrap_or(Document::ROOT);
    let h = if root == Document::ROOT {
        e.children(root, 0, 0, width)
    } else {
        let st = e.style(root);
        let mt = st.margin[0].or_zero(width);
        let mb = st.margin[2].or_zero(width);
        mt + e.block(root, 0, mt, width, None).height + mb
    };
    e.out.height = h;
    e.out
}

impl<'a> Engine<'a> {
    /// A generated piece: its style and text.
    fn gen(&self, n: NodeId) -> Option<&'a (Rc<Style>, String)> {
        if n & GEN == 0 {
            return None;
        }
        let owner = (n & !GEN) >> 1;
        self.generated.get(&owner)?[n & 1].as_ref()
    }

    /// A node's children, with its ::before and ::after around them.
    fn kids(&self, n: NodeId) -> Vec<NodeId> {
        let mut out = Vec::new();
        let g = self.generated.get(&n);
        if g.is_some_and(|g| g[0].is_some()) {
            out.push(GEN | n << 1);
        }
        out.extend_from_slice(&self.doc.nodes[n].children);
        if g.is_some_and(|g| g[1].is_some()) {
            out.push(GEN | n << 1 | 1);
        }
        out
    }

    fn style(&self, n: NodeId) -> Rc<Style> {
        if let Some((st, _)) = self.gen(n) {
            return st.clone();
        }
        self.styles[n].clone().unwrap_or_else(|| Rc::new(Style::root()))
    }

    fn is_block_level(&self, n: NodeId) -> bool {
        if n & GEN != 0 {
            return false;
        }
        match &self.doc.nodes[n].data {
            NodeData::Element(_) => {
                let s = self.style(n);
                !s.display.is_inline() && !s.hidden()
            }
            _ => false,
        }
    }

    fn hidden(&self, n: NodeId) -> bool {
        if n & GEN != 0 {
            return false;
        }
        match &self.doc.nodes[n].data {
            NodeData::Element(_) => self.style(n).hidden(),
            NodeData::Text(_) => false,
            NodeData::Document => false,
        }
    }

    fn is_blank_text(&self, n: NodeId) -> bool {
        n & GEN == 0
            && matches!(&self.doc.nodes[n].data, NodeData::Text(t) if t.chars().all(|c| c.is_ascii_whitespace()))
    }

    fn link_of(&self, n: NodeId) -> Option<NodeId> {
        if n & GEN != 0 {
            return None;
        }
        let e = self.doc.element(n)?;
        (matches!(e.tag.as_str(), "a" | "area") && e.attr("href").is_some()).then_some(n)
    }

    // ----------------------------------------------------------- block flow

    /// Lays out the children of `node` in its content box; returns the content height.
    fn children(&mut self, node: NodeId, x: i32, y: i32, w: i32) -> i32 {
        let kids: Vec<NodeId> = self.kids(node);
        let parent_style = self.style(node);
        let mut cursor = y;
        let mut pending_margin = 0i32;
        let mut margin_neg = 0i32;
        let mut i = 0;
        while i < kids.len() {
            let c = kids[i];
            if self.hidden(c) {
                i += 1;
                continue;
            }
            if self.is_block_level(c) {
                let st = self.style(c);
                // Consecutive floats form a row.
                if st.float != Float::None {
                    let mut j = i;
                    let mut row = Vec::new();
                    while j < kids.len() {
                        let k = kids[j];
                        if self.hidden(k) || self.is_blank_text(k) {
                            j += 1;
                            continue;
                        }
                        if self.is_block_level(k) && self.style(k).float != Float::None {
                            row.push(k);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    cursor += pending_margin + margin_neg;
                    pending_margin = 0;
                    margin_neg = 0;
                    let all_right = row.iter().all(|k| self.style(*k).float == Float::Right);
                    let justify = if all_right { Justify::End } else { Justify::Start };
                    cursor += self.row(&row, x, cursor, w, true, justify, 0, false);
                    i = j;
                    continue;
                }
                let mt = st.margin[0].or_zero(w);
                let mb = st.margin[2].or_zero(w);
                // Collapse with the previous sibling's bottom margin.
                let (pos, neg) = (pending_margin.max(mt.max(0)), margin_neg.min(mt.min(0)));
                let top = cursor + pos + neg;
                let out = self.block(c, x, top, w, None);
                cursor = top + out.height;
                pending_margin = mb.max(0);
                margin_neg = mb.min(0);
                i += 1;
                continue;
            }
            // A run of inline-level content.
            let mut j = i;
            while j < kids.len() && !self.is_block_level(kids[j]) {
                j += 1;
            }
            let run: Vec<NodeId> = kids[i..j].iter().copied().filter(|k| !self.hidden(*k)).collect();
            i = j;
            if run.iter().all(|k| self.is_blank_text(*k)) {
                continue;
            }
            let top = cursor + pending_margin + margin_neg;
            let h = self.inline(&run, x, top, w, &parent_style);
            if h > 0 {
                cursor = top + h;
                pending_margin = 0;
                margin_neg = 0;
            }
        }
        cursor + pending_margin + margin_neg - y
    }

    /// Lays out one block-level box with its border-box top-left at (x + margin-left, y).
    /// `forced` sets the border-box width (table cells, flex items).
    fn block(&mut self, node: NodeId, x: i32, y: i32, cw: i32, forced: Option<i32>) -> BoxOut {
        self.depth += 1;
        if self.depth > 120 {
            self.depth -= 1;
            return BoxOut { height: 0, bg: None };
        }
        let st = self.style(node);
        let tag = self.doc.tag(node);
        // Replaced elements (controls, images) size and paint themselves.
        let replaced = matches!(tag, "img" | "input" | "select" | "textarea");
        let pad: [i32; 4] = if replaced { [0; 4] } else { core::array::from_fn(|i| st.padding[i].or_zero(cw).max(0)) };
        let border: [i32; 4] = if replaced { [0; 4] } else { st.borders().map(round_i) };
        let hext = pad[1] + pad[3] + border[1] + border[3];
        let (mut ml, mut mr) = (st.margin[3].resolve(cw), st.margin[1].resolve(cw));
        let width = if let Some(f) = forced {
            ml = Some(0);
            mr = Some(0);
            (f - hext).max(0)
        } else if st.display == Display::Table && st.width.is_auto() {
            // Tables shrink to fit their content.
            let avail = cw - ml.unwrap_or(0) - mr.unwrap_or(0) - hext;
            let (_, max) = self.pref(node);
            (max - hext).clamp(0, avail.max(0))
        } else {
            match st.width.resolve(cw) {
                Some(w) => {
                    if st.border_box {
                        (w - hext).max(0)
                    } else {
                        w.max(0)
                    }
                }
                None => (cw - ml.unwrap_or(0) - mr.unwrap_or(0) - hext).max(0),
            }
        };
        let mut width = width;
        if let Some(maxw) = st.max_width.resolve(cw) {
            let maxw = if st.border_box { maxw - hext } else { maxw };
            width = width.min(maxw.max(0));
        }
        if let Some(minw) = st.min_width.resolve(cw) {
            let minw = if st.border_box { minw - hext } else { minw };
            width = width.max(minw);
        }
        // Auto margins centre (or push right) a box narrower than its container;
        // so does a <center> (or -webkit-center) parent.
        let used = width + hext;
        let parent_centers = forced.is_none()
            && self.doc.parent(node).and_then(|p| self.styles[p].as_ref()).is_some_and(|p| p.center_blocks)
            && ml == Some(0)
            && mr == Some(0);
        let (ml, mr) = if parent_centers { (None, None) } else { (ml, mr) };
        let (ml, _mr) = match (ml, mr) {
            (None, None) => (((cw - used) / 2).max(0), 0),
            (None, Some(r)) => ((cw - used - r).max(0), r),
            (Some(l), r) => (l, r.unwrap_or(0)),
        };
        let bx = x + ml;
        let cx = bx + border[3] + pad[3];
        let cy = y + border[0] + pad[0];
        let bg_index = self.out.items.len();
        self.out.items.push(Item::Rect { rect: Rect::default(), color: 0, radius: 0 });
        if let Some(e) = self.doc.element(node) {
            if let Some(id) = e.id().or(if tag == "a" { e.attr("name") } else { None }) {
                self.out.anchors.push((String::from(id), y));
            }
        }
        let saved_link = self.link;
        let links_start = self.out.links.len();
        if let Some(l) = self.link_of(node) {
            self.link = Some(l);
        }
        let saved_baseline = self.first_baseline.take();
        let content_h = match st.display {
            Display::Table => self.table(node, cx, cy, width),
            Display::Flex | Display::InlineFlex if !st.flex_column => {
                let kids: Vec<NodeId> = self.doc.nodes[node]
                    .children
                    .iter()
                    .copied()
                    .filter(|k| !self.hidden(*k) && !self.is_blank_text(*k))
                    .collect();
                if kids.iter().all(|k| self.doc.element(*k).is_some()) {
                    let gap = round_i(st.gap);
                    self.row(&kids, cx, cy, width, st.flex_wrap, st.justify, gap, st.center_items)
                } else {
                    self.children(node, cx, cy, width)
                }
            }
            _ if matches!(tag, "img" | "input" | "select" | "textarea") => {
                // A replaced element made block-level.
                self.inline(&[node], cx, cy, width, &st)
            }
            _ => self.children(node, cx, cy, width),
        };
        // The list marker sits left of the first line.
        if st.display == Display::ListItem && st.list_style != ListStyle::None && st.visible {
            let baseline = self.first_baseline.unwrap_or(cy + self.host.metrics(FontSpec::of(&st)).ascent);
            self.marker(node, &st, cx, baseline);
        }
        self.first_baseline = saved_baseline.or(self.first_baseline);
        self.link = saved_link;
        let mut content_h = content_h;
        if let Some(h) = st.height.resolve(0).filter(|_| matches!(st.height, Len::Calc(p, _) if p == 0.0)) {
            let h = if st.border_box { h - pad[0] - pad[2] - border[0] - border[2] } else { h };
            if st.overflow_hidden || h > content_h || matches!(tag, "img" | "hr") {
                content_h = h.max(0);
            } else {
                content_h = content_h.max(h);
            }
        }
        if let Some(h) = st.min_height.resolve(0).filter(|_| matches!(st.min_height, Len::Calc(p, _) if p == 0.0)) {
            content_h = content_h.max(h);
        }
        let height = border[0] + pad[0] + content_h + pad[2] + border[2];
        let rect = Rect::new(bx, y, used, height);
        if st.visible && !replaced {
            let is_canvas = matches!(tag, "html" | "body");
            if st.background >> 24 != 0 && !is_canvas && !st.masked {
                self.out.items[bg_index] = Item::Rect { rect, color: st.background, radius: st.radius.min(height as f32 / 2.0) as i32 };
            }
            self.borders(&st, rect, &border);
        }
        if tag == "button" {
            self.out.fields.push(Field { rect, node });
        }
        // A block-level link is clickable across its box.
        if let Some(l) = self.link_of(node) {
            self.out.links.insert(links_start, Link { rect, node: l });
        }
        self.depth -= 1;
        BoxOut { height, bg: Some(bg_index) }
    }

    fn borders(&mut self, st: &Style, r: Rect, b: &[i32; 4]) {
        let color = |i: usize| st.border_color[i].unwrap_or(st.color);
        if b[0] > 0 {
            self.out.items.push(Item::Rect { rect: Rect::new(r.x, r.y, r.w, b[0]), color: color(0), radius: 0 });
        }
        if b[2] > 0 {
            self.out.items.push(Item::Rect { rect: Rect::new(r.x, r.bottom() - b[2], r.w, b[2]), color: color(2), radius: 0 });
        }
        if b[3] > 0 {
            self.out.items.push(Item::Rect { rect: Rect::new(r.x, r.y, b[3], r.h), color: color(3), radius: 0 });
        }
        if b[1] > 0 {
            self.out.items.push(Item::Rect { rect: Rect::new(r.right() - b[1], r.y, b[1], r.h), color: color(1), radius: 0 });
        }
    }

    fn marker(&mut self, node: NodeId, st: &Style, cx: i32, baseline: i32) {
        let font = FontSpec::of(st);
        let m = self.host.metrics(font);
        match st.list_style {
            ListStyle::Disc | ListStyle::Circle | ListStyle::Square => {
                let size = round_i(st.font_size * 0.35).max(4);
                let x = cx - size - (st.font_size * 0.5) as i32;
                let y = baseline - (m.ascent * 2 / 5) - size / 2;
                self.out.items.push(Item::Bullet { rect: Rect::new(x, y, size, size), color: st.color, kind: st.list_style });
            }
            _ => {
                let n = self.list_number(node);
                let text = format!("{}.", counter_text(n, st.list_style));
                let w = self.host.measure(font, &text);
                let x = cx - w - (st.font_size * 0.4) as i32;
                self.out.items.push(Item::Text { x, y: baseline, text, font, color: st.color, underline: false, strike: false });
            }
        }
    }

    fn list_number(&self, node: NodeId) -> i32 {
        let doc = self.doc;
        let parent = doc.parent(node);
        let start = parent.and_then(|p| doc.element(p)).and_then(|e| e.attr("start")).and_then(|s| s.trim().parse().ok()).unwrap_or(1);
        let reversed = parent.and_then(|p| doc.element(p)).is_some_and(|e| e.attr("reversed").is_some());
        if let Some(v) = doc.element(node).and_then(|e| e.attr("value")).and_then(|v| v.trim().parse().ok()) {
            return v;
        }
        let before = doc.previous_elements(node).filter(|s| doc.tag(*s) == "li").count() as i32;
        if reversed {
            let total = parent.map_or(0, |p| doc.nodes[p].children.iter().filter(|c| doc.tag(**c) == "li").count() as i32);
            total - before
        } else {
            start + before
        }
    }

    // ----------------------------------------------------------- rows (flex, floats)

    /// Lays out `items` side by side (wrapping if allowed); returns the height.
    #[allow(clippy::too_many_arguments)]
    fn row(&mut self, items: &[NodeId], x: i32, y: i32, w: i32, wrap: bool, justify: Justify, gap: i32, center: bool) -> i32 {
        // Each item's outer width: explicit, else shrink-to-fit.
        let mut widths = Vec::with_capacity(items.len());
        let mut mins = Vec::with_capacity(items.len());
        let mut margins = Vec::with_capacity(items.len());
        for &n in items {
            let st = self.style(n);
            let (min, max) = self.pref(n);
            let ml = st.margin[3].or_zero(w);
            let mr = st.margin[1].or_zero(w);
            let hext: i32 = (st.padding[1].or_zero(w) + st.padding[3].or_zero(w)) + st.borders()[1] as i32 + st.borders()[3] as i32;
            let explicit = st.width.resolve(w).map(|v| if st.border_box { v } else { v + hext });
            let mut outer = explicit.unwrap_or(max).min(w - ml - mr).max(0);
            if let Some(maxw) = st.max_width.resolve(w) {
                outer = outer.min(if st.border_box { maxw } else { maxw + hext });
            }
            if let Some(minw) = st.min_width.resolve(w) {
                outer = outer.max(minw);
            }
            widths.push(outer);
            mins.push(if explicit.is_some() { outer } else { min.min(outer) });
            margins.push((ml, mr));
        }
        // Break into lines.
        let mut lines: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        let mut used = 0;
        for k in 0..items.len() {
            let outer = widths[k] + margins[k].0 + margins[k].1;
            if wrap && k > start && used + gap + outer > w {
                lines.push((start, k));
                start = k;
                used = 0;
            }
            used += outer + if k > start { gap } else { 0 };
        }
        lines.push((start, items.len()));
        let mut cy = y;
        for (a, b) in lines {
            let n = b - a;
            let gaps = gap * (n as i32 - 1).max(0);
            let total: i32 = (a..b).map(|k| widths[k] + margins[k].0 + margins[k].1).sum::<i32>() + gaps;
            // Shrink to fit (no-wrap rows), not below min-content.
            if total > w {
                let over = total - w;
                let shrinkable: i32 = (a..b).map(|k| widths[k] - mins[k]).sum();
                if shrinkable > 0 {
                    for k in a..b {
                        let share = (widths[k] - mins[k]) as i64 * over.min(shrinkable) as i64 / shrinkable as i64;
                        widths[k] -= share as i32;
                    }
                }
            }
            let total: i32 = (a..b).map(|k| widths[k] + margins[k].0 + margins[k].1).sum::<i32>() + gaps;
            let mut free = (w - total).max(0);
            // Grow.
            let grow: f32 = (a..b).map(|k| self.style(items[k]).flex_grow).sum();
            if grow > 0.0 && free > 0 {
                for k in a..b {
                    let g = self.style(items[k]).flex_grow;
                    widths[k] += (free as f32 * g / grow) as i32;
                }
                free = 0;
            }
            let (mut px, spacing) = match justify {
                Justify::Start => (x, 0),
                Justify::End => (x + free, 0),
                Justify::Center => (x + free / 2, 0),
                Justify::SpaceBetween if n > 1 => (x, free / (n as i32 - 1)),
                Justify::SpaceAround if n > 0 => (x + free / (2 * n as i32), free / n as i32),
                _ => (x, 0),
            };
            let mut line_h = 0;
            let mut placed = Vec::new();
            for k in a..b {
                let st = self.style(items[k]);
                let mt = st.margin[0].or_zero(w);
                let mb = st.margin[2].or_zero(w);
                let (i0, l0, f0) = (self.out.items.len(), self.out.links.len(), self.out.fields.len());
                let out = self.block(items[k], px + margins[k].0, cy + mt, w, Some(widths[k]));
                placed.push((i0, l0, f0, mt + out.height + mb));
                line_h = line_h.max(mt + out.height + mb);
                px += widths[k] + margins[k].0 + margins[k].1 + gap + spacing;
            }
            if center {
                for (k, &(i0, l0, f0, h)) in placed.iter().enumerate() {
                    let dy = (line_h - h) / 2;
                    // Later items' ranges start after earlier ones: shift only this item's own range.
                    let end_i = placed.get(k + 1).map_or(self.out.items.len(), |p| p.0);
                    let end_l = placed.get(k + 1).map_or(self.out.links.len(), |p| p.1);
                    let end_f = placed.get(k + 1).map_or(self.out.fields.len(), |p| p.2);
                    for it in &mut self.out.items[i0..end_i] {
                        it.shift(0, dy);
                    }
                    for l in &mut self.out.links[l0..end_l] {
                        l.rect.y += dy;
                    }
                    for f in &mut self.out.fields[f0..end_f] {
                        f.rect.y += dy;
                    }
                }
            }
            cy += line_h + if wrap { gap } else { 0 };
        }
        (cy - y - if wrap { gap } else { 0 }).max(0)
    }

    // ----------------------------------------------------------- tables

    fn table(&mut self, node: NodeId, x: i32, y: i32, w: i32) -> i32 {
        let doc = self.doc;
        let spacing = doc
            .element(node)
            .and_then(|e| e.attr("cellspacing"))
            .and_then(|v| v.trim().parse::<i32>().ok())
            .unwrap_or(2);
        let mut rows: Vec<NodeId> = Vec::new();
        let mut captions = Vec::new();
        for &c in &doc.nodes[node].children {
            if self.hidden(c) {
                continue;
            }
            match self.style(c).display {
                Display::TableRow => rows.push(c),
                Display::TableRowGroup => {
                    for &r in &doc.nodes[c].children {
                        if !self.hidden(r) && doc.element(r).is_some() {
                            rows.push(r);
                        }
                    }
                }
                Display::TableCaption => captions.push(c),
                _ if doc.element(c).is_some() => rows.push(c),
                _ => {}
            }
        }
        let mut cy = y;
        for c in captions {
            cy += self.block(c, x, cy, w, None).height;
        }
        // Cells, with colspans.
        let cells: Vec<Vec<(NodeId, usize)>> = rows
            .iter()
            .map(|r| {
                doc.nodes[*r]
                    .children
                    .iter()
                    .copied()
                    .filter(|c| doc.element(*c).is_some() && !self.hidden(*c))
                    .map(|c| {
                        let span = doc.element(c).and_then(|e| e.attr("colspan")).and_then(|s| s.trim().parse::<usize>().ok()).unwrap_or(1);
                        (c, span.clamp(1, 100))
                    })
                    .collect()
            })
            .collect();
        let ncols = cells.iter().map(|r| r.iter().map(|c| c.1).sum::<usize>()).max().unwrap_or(0);
        if ncols == 0 {
            return cy - y;
        }
        let mut min = alloc::vec![0i32; ncols];
        let mut max = alloc::vec![0i32; ncols];
        let mut fixed = alloc::vec![None::<i32>; ncols];
        let inner = w - spacing * (ncols as i32 + 1);
        for pass in 0..2 {
            for row in &cells {
                let mut col = 0;
                for &(c, span) in row {
                    if col >= ncols {
                        break;
                    }
                    let span = span.min(ncols - col);
                    if (span == 1) == (pass == 0) {
                        let (mn, mx) = self.pref(c);
                        let st = self.style(c);
                        let explicit = st.width.resolve(inner.max(0));
                        if span == 1 {
                            min[col] = min[col].max(mn);
                            max[col] = max[col].max(mx.max(mn));
                            if let Some(e) = explicit {
                                let e = e.max(mn);
                                fixed[col] = Some(fixed[col].unwrap_or(0).max(e));
                            }
                        } else {
                            // Spread a spanning cell's excess evenly.
                            let cur_min: i32 = min[col..col + span].iter().sum::<i32>() + spacing * (span as i32 - 1);
                            let cur_max: i32 = max[col..col + span].iter().sum::<i32>() + spacing * (span as i32 - 1);
                            // Spread the excess in proportion to the columns' widths.
                            let spread = |v: &mut [i32], need: i32| {
                                let total: i32 = v.iter().map(|x| (*x).max(1)).sum();
                                for x in v.iter_mut() {
                                    *x += (need as i64 * (*x).max(1) as i64 / total as i64) as i32;
                                }
                            };
                            if mn > cur_min {
                                spread(&mut min[col..col + span], mn - cur_min);
                            }
                            if mx > cur_max {
                                spread(&mut max[col..col + span], mx - cur_max);
                            }
                        }
                    }
                    col += span;
                }
            }
        }
        for c in 0..ncols {
            if let Some(f) = fixed[c] {
                max[c] = f.max(min[c]);
                min[c] = min[c].max(f.min(max[c]));
            }
        }
        let st = self.style(node);
        let target = inner.max(0);
        let sum_min: i32 = min.iter().sum();
        let sum_max: i32 = max.iter().sum();
        let widths: Vec<i32> = if sum_max <= target {
            if st.width.is_auto() {
                max.clone()
            } else {
                // Spread the extra width, favouring columns without a fixed width.
                let extra = target - sum_max;
                let flex: Vec<usize> = (0..ncols).filter(|c| fixed[*c].is_none()).collect();
                let pool: Vec<usize> = if flex.is_empty() { (0..ncols).collect() } else { flex };
                let weight: i32 = pool.iter().map(|c| max[*c].max(1)).sum();
                let mut w2 = max.clone();
                for c in &pool {
                    w2[*c] += (extra as i64 * max[*c].max(1) as i64 / weight.max(1) as i64) as i32;
                }
                w2
            }
        } else if sum_min < target {
            let room = target - sum_min;
            let span = (sum_max - sum_min).max(1);
            (0..ncols).map(|c| min[c] + ((max[c] - min[c]) as i64 * room as i64 / span as i64) as i32).collect()
        } else {
            min.clone()
        };
        cy += spacing;
        for (ri, row) in cells.iter().enumerate() {
            let row_style = self.style(rows[ri]);
            let row_bg_index = self.out.items.len();
            self.out.items.push(Item::Rect { rect: Rect::default(), color: 0, radius: 0 });
            let mut cx = x + spacing;
            let mut col = 0;
            let mut placed = Vec::new();
            let mut row_h = 0;
            for &(c, span) in row {
                if col >= ncols {
                    break;
                }
                let span = span.min(ncols - col);
                let cw: i32 = widths[col..col + span].iter().sum::<i32>() + spacing * (span as i32 - 1);
                let (i0, l0, f0) = (self.out.items.len(), self.out.links.len(), self.out.fields.len());
                let out = self.block(c, cx, cy, cw, Some(cw));
                row_h = row_h.max(out.height);
                placed.push((c, i0, l0, f0, out, cx, cw));
                cx += cw + spacing;
                col += span;
            }
            if let Some(h) = row_style.height.resolve(0).filter(|_| matches!(row_style.height, Len::Calc(p, _) if p == 0.0)) {
                row_h = row_h.max(h);
            }
            // Stretch cell backgrounds to the row and centre cells that ask for it.
            for (k, (c, i0, l0, f0, out, pcx, pcw)) in placed.iter().enumerate() {
                if let Some(bg) = out.bg {
                    if let Item::Rect { rect, .. } = &mut self.out.items[bg] {
                        if rect.w > 0 {
                            rect.h = row_h;
                        }
                    }
                }
                let cst = self.style(*c);
                let valign_top = self
                    .doc
                    .element(*c)
                    .and_then(|e| e.attr("valign"))
                    .or_else(|| self.doc.element(rows[ri]).and_then(|e| e.attr("valign")))
                    .is_some_and(|v| v.eq_ignore_ascii_case("top"));
                if cst.vertical_middle && !valign_top && out.height < row_h {
                    let dy = (row_h - out.height) / 2;
                    let end_i = placed.get(k + 1).map_or(self.out.items.len(), |p| p.1);
                    let end_l = placed.get(k + 1).map_or(self.out.links.len(), |p| p.2);
                    let end_f = placed.get(k + 1).map_or(self.out.fields.len(), |p| p.3);
                    for (idx, it) in self.out.items[*i0..end_i].iter_mut().enumerate() {
                        // The cell's own background stays put.
                        if Some(*i0 + idx) != out.bg {
                            it.shift(0, dy);
                        }
                    }
                    for l in &mut self.out.links[*l0..end_l] {
                        l.rect.y += dy;
                    }
                    for f in &mut self.out.fields[*f0..end_f] {
                        f.rect.y += dy;
                    }
                }
                let _ = (pcx, pcw);
            }
            if row_style.background >> 24 != 0 && row_style.visible {
                let rw: i32 = widths.iter().sum::<i32>() + spacing * (ncols as i32 - 1);
                self.out.items[row_bg_index] = Item::Rect { rect: Rect::new(x + spacing, cy, rw, row_h), color: row_style.background, radius: 0 };
            }
            cy += row_h + spacing;
        }
        cy - y
    }

    // ----------------------------------------------------------- preferred widths

    /// (min-content, max-content) outer widths.
    fn pref(&mut self, n: NodeId) -> (i32, i32) {
        if let Some((st, text)) = self.gen(n) {
            let font = FontSpec::of(st);
            let max = (self.host.measure64(font, text) + 63) >> 6;
            let min = text.split_ascii_whitespace().map(|w| (self.host.measure64(font, w) + 63) >> 6).max().unwrap_or(0);
            return (min, max);
        }
        if let Some(v) = self.pref_cache[n] {
            return v;
        }
        self.depth += 1;
        let v = if self.depth > 120 { (0, 0) } else { self.compute_pref(n) };
        self.depth -= 1;
        self.pref_cache[n] = Some(v);
        v
    }

    fn compute_pref(&mut self, n: NodeId) -> (i32, i32) {
        let doc = self.doc;
        match &doc.nodes[n].data {
            NodeData::Text(t) => {
                let st = self.style(n);
                let font = FontSpec::of(&st);
                let text = transform(t, st.transform);
                // Measured like lines are (1/64 px), rounded up so content always fits.
                let ceil = |v: i32| (v + 63) >> 6;
                if st.white_space.keeps_newlines() && !st.white_space.wraps() {
                    let w = text.lines().map(|l| self.host.measure64(font, &expand_tabs(l))).max().unwrap_or(0);
                    return (ceil(w), ceil(w));
                }
                let space = self.host.measure64(font, " ");
                let mut min = 0;
                let mut max = 0;
                for (i, word) in text.split_ascii_whitespace().enumerate() {
                    let ww = self.host.measure64(font, word);
                    min = min.max(ww);
                    max += ww + if i > 0 { space } else { 0 };
                }
                if !st.white_space.wraps() {
                    min = max;
                }
                (ceil(min), ceil(max))
            }
            NodeData::Element(e) => {
                let st = self.style(n);
                if st.hidden() {
                    return (0, 0);
                }
                let ext = st.padding[1].or_zero(0) + st.padding[3].or_zero(0) + st.borders()[1] as i32 + st.borders()[3] as i32
                    + st.margin[1].or_zero(0).max(0) + st.margin[3].or_zero(0).max(0);
                if let Some(Len::Calc(p, px)) = Some(st.width).filter(|w| !w.is_auto()) {
                    if p == 0.0 {
                        let w = px as i32 + if st.border_box { 0 } else { ext };
                        return (w, w);
                    }
                }
                match e.tag.as_str() {
                    "img" => {
                        let (w, _) = self.replaced_size(n, i32::MAX / 4);
                        return (w + ext, w + ext);
                    }
                    "input" | "select" | "textarea" | "button" if e.tag != "button" => {
                        let (w, _, _) = self.control_size(n);
                        return (w + ext, w + ext);
                    }
                    "br" => return (0, 0),
                    _ => {}
                }
                let kids: Vec<NodeId> = self.kids(n);
                let horizontal = matches!(st.display, Display::TableRow)
                    || (matches!(st.display, Display::Flex | Display::InlineFlex) && !st.flex_column);
                let (mut min, mut max, mut line) = (0, 0, 0);
                for c in kids {
                    if self.hidden(c) {
                        continue;
                    }
                    let (cmin, cmax) = self.pref(c);
                    min = min.max(cmin);
                    if horizontal {
                        max += cmax + st.gap as i32;
                        if st.display == Display::TableRow {
                            min = min.max(0);
                        }
                    } else if self.is_block_level(c) {
                        max = max.max(line).max(cmax);
                        line = 0;
                    } else {
                        line += cmax;
                    }
                }
                if horizontal && st.display == Display::TableRow {
                    // A row's minimum is the sum of its cells' minimums.
                    min = 0;
                    for c in doc.nodes[n].children.clone() {
                        if !self.hidden(c) {
                            min += self.pref(c).0;
                        }
                    }
                }
                max = max.max(line);
                if st.display == Display::Table {
                    // Rows are inside row groups: take the widest row.
                    let mut rows = Vec::new();
                    for &c in &doc.nodes[n].children {
                        match self.style(c).display {
                            Display::TableRowGroup => rows.extend(doc.nodes[c].children.iter().copied()),
                            _ => rows.push(c),
                        }
                    }
                    for r in rows {
                        if self.doc.element(r).is_none() || self.hidden(r) {
                            continue;
                        }
                        let cells: Vec<NodeId> = doc.nodes[r].children.iter().copied().filter(|c| doc.element(*c).is_some()).collect();
                        let (rmin, rmax) = cells.iter().fold((0, 0), |(a, b), c| {
                            let (m, x) = self.pref(*c);
                            (a + m + 2, b + x + 2)
                        });
                        min = min.max(rmin);
                        max = max.max(rmax);
                    }
                }
                if !st.white_space.wraps() {
                    min = max;
                }
                (min + ext, max.max(min) + ext)
            }
            NodeData::Document => (0, 0),
        }
    }

    // ----------------------------------------------------------- inline formatting

    fn replaced_size(&self, n: NodeId, avail: i32) -> (i32, i32) {
        let st = self.style(n);
        let e = self.doc.element(n).unwrap();
        let natural = image_source(e).and_then(|s| self.host.image_size(&s));
        let w = st.width.resolve(avail.min(100_000));
        let h = match st.height {
            Len::Calc(p, px) if p == 0.0 => Some(px as i32),
            _ => None,
        };
        let (mut w, mut h) = match (w, h, natural) {
            (Some(w), Some(h), _) => (w, h),
            (Some(w), None, Some((nw, nh))) if nw > 0 => (w, (w as i64 * nh as i64 / nw as i64) as i32),
            (None, Some(h), Some((nw, nh))) if nh > 0 => ((h as i64 * nw as i64 / nh as i64) as i32, h),
            (None, None, Some((nw, nh))) => (nw as i32, nh as i32),
            (Some(w), None, _) => (w, 0),
            (None, Some(h), _) => (0, h),
            _ => (0, 0),
        };
        if let Some(maxw) = st.max_width.resolve(avail.min(100_000)) {
            if w > maxw && w > 0 {
                h = (h as i64 * maxw as i64 / w as i64) as i32;
                w = maxw;
            }
        }
        // Never wider than the line (like max-width: 100%).
        if w > avail && w > 0 {
            h = (h as i64 * avail as i64 / w as i64) as i32;
            w = avail;
        }
        (w.max(0), h.max(0))
    }

    /// A form control's (width, height, label).
    fn control_size(&self, n: NodeId) -> (i32, i32, String) {
        let st = self.style(n);
        let e = self.doc.element(n).unwrap();
        let font = FontSpec::of(&st);
        let m = self.host.metrics(font);
        let line = m.ascent + m.descent;
        let pad_v = st.padding[0].or_zero(0) + st.padding[2].or_zero(0) + st.borders()[0] as i32 + st.borders()[2] as i32;
        let pad_h = st.padding[1].or_zero(0) + st.padding[3].or_zero(0) + st.borders()[1] as i32 + st.borders()[3] as i32;
        let kind = e.attr("type").unwrap_or("text").to_ascii_lowercase();
        let (label, content_w, content_h) = match (e.tag.as_str(), kind.as_str()) {
            ("input", "checkbox" | "radio") => (String::new(), 13, 13),
            ("input", "submit" | "button" | "reset") => {
                let label = String::from(e.attr("value").unwrap_or(if kind == "reset" { "Reset" } else { "Submit" }));
                (label.clone(), self.host.measure(font, &label), line)
            }
            ("input", "image") => (String::new(), 20, 20),
            ("select", _) => {
                let doc = self.doc;
                let options: Vec<NodeId> = doc.descendants(n).into_iter().filter(|o| doc.tag(*o) == "option").collect();
                let selected = options.iter().find(|o| doc.element(**o).unwrap().attr("selected").is_some()).or(options.first());
                let label = selected.map(|o| String::from(doc.text_content(*o).trim())).unwrap_or_default();
                let widest = options.iter().map(|o| self.host.measure(font, doc.text_content(*o).trim())).max().unwrap_or(40);
                (label, widest + 20, line)
            }
            ("textarea", _) => {
                let cols = e.attr("cols").and_then(|c| c.parse::<i32>().ok()).unwrap_or(20);
                let rows = e.attr("rows").and_then(|c| c.parse::<i32>().ok()).unwrap_or(2);
                (self.doc.text_content(n), cols * self.host.measure(font, "m") * 3 / 5, rows * line)
            }
            _ => {
                let size = e.attr("size").and_then(|c| c.parse::<i32>().ok()).unwrap_or(20);
                let label = String::from(e.attr("value").unwrap_or(""));
                (label, size * self.host.measure(font, "n") * 4 / 5, line)
            }
        };
        let w = match st.width.resolve(0) {
            Some(w) if matches!(st.width, Len::Calc(p, _) if p == 0.0) => if st.border_box { w } else { w + pad_h },
            _ => content_w + pad_h,
        };
        let h = match st.height {
            Len::Calc(p, px) if p == 0.0 => px as i32 + if st.border_box { 0 } else { pad_v },
            _ => content_h + pad_v,
        };
        (w.max(0), h.max(0), label)
    }

    fn collect(&mut self, n: NodeId, out: &mut Vec<Atom>, bg: u32, space: &mut bool, avail: i32) {
        if let Some((st, text)) = self.gen(n) {
            let st = st.clone();
            if st.white_space.keeps_spaces() {
                self.spaced_words(text, &st, bg, out, space);
            } else {
                self.words(text, &st, bg, out, space);
            }
            return;
        }
        let doc = self.doc;
        match &doc.nodes[n].data {
            NodeData::Text(t) => {
                let st = self.style(n);
                let text = transform(t, st.transform);
                let ws = st.white_space;
                if ws.keeps_newlines() {
                    for (i, line) in text.split('\n').enumerate() {
                        if i > 0 {
                            out.push(Atom::Break);
                            *space = false;
                        }
                        let line = if ws.keeps_spaces() { expand_tabs(line) } else { collapse(line) };
                        if ws.keeps_spaces() && ws.wraps() {
                            self.spaced_words(&line, &st, bg, out, space);
                        } else if ws.keeps_spaces() && !ws.wraps() {
                            if !line.is_empty() {
                                out.push(Atom::Text { text: line, style: st.clone(), link: self.link, bg, space: false, brk: false });
                            }
                        } else {
                            self.words(&line, &st, bg, out, space);
                        }
                    }
                } else {
                    self.words(&text, &st, bg, out, space);
                }
            }
            NodeData::Element(e) => {
                let st = self.style(n);
                if st.hidden() {
                    return;
                }
                let tag = e.tag.as_str();
                let saved_link = self.link;
                if let Some(l) = self.link_of(n) {
                    self.link = Some(l);
                }
                match tag {
                    "br" => {
                        out.push(Atom::Break);
                        *space = false;
                    }
                    "img" => {
                        let (w, h) = self.replaced_size(n, avail);
                        let alt = e.attr("alt").unwrap_or("").trim();
                        let src = image_source(e);
                        let loaded = src.as_deref().is_some_and(|s| self.host.image_size(s).is_some());
                        if !loaded && !alt.is_empty() && (w == 0 || h == 0) {
                            // No picture (yet): show the alternative text.
                            self.words(alt, &st, bg, out, space);
                        } else if w > 0 && h > 0 {
                            let mut items = Vec::new();
                            if st.visible {
                                if let Some(src) = src {
                                    items.push(Item::Image { rect: Rect::new(0, 0, w, h), src });
                                }
                            }
                            out.push(Atom::Box { w, h, baseline: h, items, links: Vec::new(), fields: Vec::new(), link: self.link, space: core::mem::take(space) });
                        }
                    }
                    "input" | "select" | "textarea" => {
                        let (w, h, label) = self.control_size(n);
                        let items = if st.visible { self.control_items(n, &st, w, h, &label) } else { Vec::new() };
                        let font = FontSpec::of(&st);
                        let m = self.host.metrics(font);
                        let baseline = (h + m.ascent - m.descent) / 2;
                        let fields = alloc::vec![Field { rect: Rect::new(0, 0, w, h), node: n }];
                        out.push(Atom::Box { w, h, baseline, items, links: Vec::new(), fields, link: self.link, space: core::mem::take(space) });
                    }
                    _ if matches!(st.display, Display::InlineBlock | Display::InlineFlex) || tag == "button" => {
                        // An atomic inline: lay it out as a block and place it as one piece.
                        let (_, max) = self.pref(n);
                        let explicit = st.width.resolve(avail);
                        let hext = st.padding[1].or_zero(avail) + st.padding[3].or_zero(avail) + st.borders()[1] as i32 + st.borders()[3] as i32;
                        let ml = st.margin[3].or_zero(avail);
                        let mr = st.margin[1].or_zero(avail);
                        let w = match explicit {
                            Some(w) => if st.border_box { w } else { w + hext },
                            None => max - ml.max(0) - mr.max(0),
                        }
                        .min(avail - ml - mr)
                        .max(0);
                        let (i0, l0, f0) = (self.out.items.len(), self.out.links.len(), self.out.fields.len());
                        let saved_baseline = self.first_baseline.take();
                        let res = self.block(n, 0, 0, w, Some(w));
                        let baseline = self.first_baseline.unwrap_or(res.height);
                        self.first_baseline = saved_baseline;
                        let items: Vec<Item> = self.out.items.drain(i0..).collect();
                        let links: Vec<Link> = self.out.links.drain(l0..).collect();
                        let fields: Vec<Field> = self.out.fields.drain(f0..).collect();
                        if ml > 0 {
                            out.push(Atom::Gap { w: ml, space: core::mem::take(space) });
                        }
                        out.push(Atom::Box { w, h: res.height, baseline, items, links, fields, link: self.link, space: core::mem::take(space) });
                        if mr > 0 {
                            out.push(Atom::Gap { w: mr, space: false });
                        }
                    }
                    _ => {
                        let lead = st.margin[3].or_zero(avail).max(0) + st.padding[3].or_zero(avail) + st.borders()[3] as i32;
                        let trail = st.margin[1].or_zero(avail).max(0) + st.padding[1].or_zero(avail) + st.borders()[1] as i32;
                        let bg = if st.background >> 24 != 0 { st.background } else { bg };
                        if lead > 0 {
                            out.push(Atom::Gap { w: lead, space: core::mem::take(space) });
                        }
                        if let Some(id) = e.id().or(if tag == "a" { e.attr("name") } else { None }) {
                            // Inline anchors: record at the current line (approximately).
                            let y = self.pending_y;
                            self.out.anchors.push((String::from(id), y));
                        }
                        for c in self.kids(n) {
                            self.collect(c, out, bg, space, avail);
                        }
                        if trail > 0 {
                            out.push(Atom::Gap { w: trail, space: false });
                        }
                    }
                }
                self.link = saved_link;
            }
            NodeData::Document => {}
        }
    }

    fn words(&mut self, text: &str, st: &Rc<Style>, bg: u32, out: &mut Vec<Atom>, space: &mut bool) {
        if text.starts_with(|c: char| c.is_ascii_whitespace()) {
            *space = true;
        }
        for (i, w) in text.split_ascii_whitespace().enumerate() {
            let sp = core::mem::take(space) || i > 0;
            out.push(Atom::Text { text: String::from(w), style: st.clone(), link: self.link, bg, space: sp, brk: false });
        }
        if text.ends_with(|c: char| c.is_ascii_whitespace()) {
            *space = true;
        }
    }

    /// pre-wrap text: spaces kept as their own runs, breaks allowed after them.
    fn spaced_words(&mut self, text: &str, st: &Rc<Style>, bg: u32, out: &mut Vec<Atom>, space: &mut bool) {
        let mut after_ws = core::mem::take(space);
        let mut rest = text;
        while !rest.is_empty() {
            let ws = rest.starts_with(' ');
            let end = rest.find(|c: char| (c == ' ') != ws).unwrap_or(rest.len());
            let (tok, tail) = rest.split_at(end);
            out.push(Atom::Text { text: String::from(tok), style: st.clone(), link: self.link, bg, space: false, brk: after_ws });
            after_ws = ws;
            rest = tail;
        }
    }

    fn control_items(&self, n: NodeId, st: &Style, w: i32, h: i32, label: &str) -> Vec<Item> {
        let e = self.doc.element(n).unwrap();
        let mut items = Vec::new();
        let font = FontSpec::of(st);
        let m = self.host.metrics(font);
        let kind = e.attr("type").unwrap_or("text").to_ascii_lowercase();
        let bg = if st.background >> 24 != 0 { st.background } else { 0xFFFF_FFFF };
        let border = st.border_color[0].unwrap_or(0xFF76_7676);
        let radius = st.radius.min(h as f32 / 2.0) as i32;
        if matches!(kind.as_str(), "checkbox" | "radio") && e.tag == "input" {
            let r = if kind == "radio" { 7 } else { 3 };
            items.push(Item::Rect { rect: Rect::new(0, 0, w, h), color: 0xFF76_7676, radius: r });
            items.push(Item::Rect { rect: Rect::new(1, 1, w - 2, h - 2), color: 0xFFFF_FFFF, radius: r - 1 });
            if e.attr("checked").is_some() {
                items.push(Item::Rect { rect: Rect::new(3, 3, w - 6, h - 6), color: 0xFF1A_73E8, radius: r - 2 });
            }
            return items;
        }
        if st.borders()[0] > 0.0 {
            items.push(Item::Rect { rect: Rect::new(0, 0, w, h), color: border, radius });
            items.push(Item::Rect { rect: Rect::new(1, 1, w - 2, h - 2), color: bg, radius: (radius - 1).max(0) });
        } else {
            items.push(Item::Rect { rect: Rect::new(0, 0, w, h), color: bg, radius });
        }
        let pl = st.padding[3].or_zero(0) + st.borders()[3] as i32;
        let baseline = (h + m.ascent - m.descent) / 2;
        let is_button = e.tag == "input" && matches!(kind.as_str(), "submit" | "button" | "reset");
        let (text, color) = if !label.is_empty() {
            (String::from(label), st.color)
        } else if let Some(p) = e.attr("placeholder") {
            (String::from(p), 0xFF75_7575)
        } else {
            (String::new(), st.color)
        };
        if e.tag == "select" {
            let aw = self.host.measure(font, "▾");
            items.push(Item::Text { x: w - aw - 6, y: baseline, text: String::from("▾"), font, color: st.color, underline: false, strike: false });
        }
        if !text.is_empty() {
            let x = if is_button { (w - self.host.measure(font, &text)) / 2 } else { pl };
            let text = if kind == "password" { "•".repeat(text.chars().count()) } else { text };
            let first_line = text.lines().next().unwrap_or("").chars().take(200).collect::<String>();
            items.push(Item::Text { x, y: if e.tag == "textarea" { pl + m.ascent } else { baseline }, text: first_line, font, color, underline: false, strike: false });
        }
        items
    }

    /// Whether a line may break before atom `k`.
    fn breakable(atoms: &[Atom], k: usize) -> bool {
        let own = match &atoms[k] {
            Atom::Text { space, brk, .. } => *space || *brk,
            Atom::Gap { space, .. } => *space,
            Atom::Box { .. } | Atom::Break => true,
        };
        own || (k > 0 && matches!(atoms[k - 1], Atom::Box { .. }))
    }

    /// An inline formatting context over `nodes`; returns its height.
    /// Horizontal positions within a line are kept in 1/64 px.
    fn inline(&mut self, nodes: &[NodeId], x: i32, y: i32, w: i32, block_style: &Rc<Style>) -> i32 {
        let mut atoms = Vec::new();
        let mut space = false;
        self.pending_y = y;
        for &n in nodes {
            self.collect(n, &mut atoms, 0, &mut space, w);
        }
        if atoms.is_empty() {
            return 0;
        }
        let strut_font = FontSpec::of(block_style);
        let strut = self.host.metrics(strut_font);
        let strut_lh = round_i(block_style.line_px());
        let wraps = block_style.white_space.wraps();
        let align = block_style.align;
        let w64 = w * 64;

        let mut cy = y;
        let mut line: Vec<Placed> = Vec::new();
        let mut lx = 0; // used on this line, 1/64 px
        let mut k = 0;
        let mut pending_split: Option<String> = None;
        while k < atoms.len() {
            let (aw, spw, text_now) = match &atoms[k] {
                Atom::Text { text, style, space, .. } => {
                    let font = FontSpec::of(style);
                    let t = pending_split.take().unwrap_or_else(|| text.clone());
                    let tw = self.host.measure64(font, &t);
                    (tw, if *space { self.host.measure64(font, " ") } else { 0 }, Some(t))
                }
                Atom::Box { w, space, .. } => (*w * 64, if *space { self.host.measure64(strut_font, " ") } else { 0 }, None),
                Atom::Gap { w, space } => (*w * 64, if *space { self.host.measure64(strut_font, " ") } else { 0 }, None),
                Atom::Break => {
                    cy = self.finish_line(&mut line, &atoms, x, cy, w, lx, align, strut, strut_lh, true);
                    lx = 0;
                    k += 1;
                    continue;
                }
            };
            let sp = if line.is_empty() { 0 } else { spw };
            let atom_wraps = match &atoms[k] {
                Atom::Text { style, .. } => style.white_space.wraps(),
                _ => wraps,
            };
            if atom_wraps && !line.is_empty() && lx + sp + aw > w64 {
                if Self::breakable(&atoms, k) {
                    cy = self.finish_line(&mut line, &atoms, x, cy, w, lx, align, strut, strut_lh, false);
                    lx = 0;
                    pending_split = text_now.filter(|t| matches!(&atoms[k], Atom::Text { text, .. } if text != t));
                    continue;
                }
                // Glued to what precedes it: carry the tail since the last break opportunity.
                if let Some(j) = (1..line.len()).rev().find(|&j| Self::breakable(&atoms, line[j].atom)) {
                    let carried = line.split_off(j);
                    let used = line.last().map_or(0, |p| p.x + p.w);
                    cy = self.finish_line(&mut line, &atoms, x, cy, w, used, align, strut, strut_lh, false);
                    let shift = carried[0].x;
                    for mut p in carried {
                        p.x -= shift;
                        line.push(p);
                    }
                    lx = line.last().map_or(0, |p| p.x + p.w);
                    pending_split = text_now.filter(|t| matches!(&atoms[k], Atom::Text { text, .. } if text != t));
                    continue;
                }
            }
            // A single word wider than the line: break it by characters.
            if atom_wraps && line.is_empty() && aw > w64 + 64 && w > 20 {
                if let (Some(t), Atom::Text { style, .. }) = (&text_now, &atoms[k]) {
                    let font = FontSpec::of(style);
                    let mut cut = 0;
                    let mut acc = 0;
                    for (i, c) in t.char_indices() {
                        let mut buf = [0u8; 4];
                        let cw = self.host.measure64(font, c.encode_utf8(&mut buf));
                        if acc + cw > w64 && i > 0 {
                            cut = i;
                            break;
                        }
                        acc += cw;
                    }
                    if cut > 0 {
                        let (head, tail) = t.split_at(cut);
                        line.push(Placed { atom: k, x: 0, w: acc, text: Some(String::from(head)) });
                        cy = self.finish_line(&mut line, &atoms, x, cy, w, acc, align, strut, strut_lh, false);
                        lx = 0;
                        pending_split = Some(String::from(tail));
                        continue;
                    }
                }
            }
            let px = lx + sp;
            let split = match (&atoms[k], text_now) {
                (Atom::Text { text, .. }, Some(t)) if *text != t => Some(t),
                _ => None,
            };
            line.push(Placed { atom: k, x: px, w: aw, text: split });
            lx = px + aw;
            k += 1;
        }
        cy = self.finish_line(&mut line, &atoms, x, cy, w, lx, align, strut, strut_lh, false);
        cy - y
    }

    /// Emits one line box (`used` and positions in 1/64 px); returns the y below it.
    #[allow(clippy::too_many_arguments)]
    fn finish_line(
        &mut self,
        line: &mut Vec<Placed>,
        atoms: &[Atom],
        x: i32,
        y: i32,
        w: i32,
        used: i32,
        align: Align,
        strut: Metrics,
        strut_lh: i32,
        forced: bool,
    ) -> i32 {
        if line.is_empty() && !forced {
            return y;
        }
        // Half the leading goes above; the rest (with any odd pixel) below.
        let split = |lh: i32, m: Metrics| {
            let above = m.ascent + (lh - (m.ascent + m.descent)) / 2;
            (above, lh - above)
        };
        let (mut above, mut below) = split(strut_lh, strut);
        for p in line.iter() {
            match &atoms[p.atom] {
                Atom::Text { style, .. } => {
                    let m = self.host.metrics(FontSpec::of(style));
                    let (a, b) = split(round_i(style.line_px()), m);
                    above = above.max(a);
                    below = below.max(b);
                }
                Atom::Box { h, baseline, .. } => {
                    above = above.max(*baseline);
                    below = below.max(h - baseline);
                }
                _ => {}
            }
        }
        let baseline = y + above;
        if self.first_baseline.is_none() {
            self.first_baseline = Some(baseline);
        }
        let used_px = (used + 63) >> 6;
        let dx = match align {
            Align::Center => ((w - used_px) / 2).max(0),
            Align::Right => (w - used_px).max(0),
            _ => 0,
        };
        let px = |v: i32| (v + 32) >> 6;
        let mut k = 0;
        while k < line.len() {
            let p = &line[k];
            match &atoms[p.atom] {
                Atom::Text { text, style, link, bg, .. } => {
                    let font = FontSpec::of(style);
                    let m = self.host.metrics(font);
                    // Merge following atoms of the same style into one run.
                    let mut run = p.text.clone().unwrap_or_else(|| text.clone());
                    let start = p.x;
                    let mut end = p.x + p.w;
                    let mut j = k + 1;
                    while j < line.len() {
                        let q = &line[j];
                        match &atoms[q.atom] {
                            Atom::Text { text: t2, style: s2, link: l2, bg: b2, .. }
                                if Rc::ptr_eq(s2, style) && l2 == link && b2 == bg && q.x >= end =>
                            {
                                if q.x > end {
                                    run.push(' ');
                                }
                                run.push_str(q.text.as_deref().unwrap_or(t2));
                                end = q.x + q.w;
                                j += 1;
                            }
                            _ => break,
                        }
                    }
                    let rx = x + dx + px(start);
                    let rw = px(end) - px(start);
                    if *bg >> 24 != 0 && style.visible {
                        self.out.items.push(Item::Rect {
                            rect: Rect::new(rx - 1, baseline - m.ascent - 1, rw + 2, m.ascent + m.descent + 2),
                            color: *bg,
                            radius: 2,
                        });
                    }
                    if style.visible {
                        self.out.items.push(Item::Text {
                            x: rx,
                            y: baseline,
                            text: run,
                            font,
                            color: style.color,
                            underline: style.underline,
                            strike: style.strike,
                        });
                    }
                    if let Some(l) = link {
                        let lh = round_i(style.line_px());
                        self.out.links.push(Link {
                            rect: Rect::new(rx, baseline - m.ascent - (lh - m.ascent - m.descent).max(0) / 2, rw, lh.max(m.ascent + m.descent)),
                            node: *l,
                        });
                    }
                    k = j;
                    continue;
                }
                Atom::Box { baseline: b, h, items, links, fields, link, w: bw, .. } => {
                    let bx = x + dx + px(p.x);
                    let by = baseline - b;
                    for it in items {
                        let mut it = it.clone();
                        it.shift(bx, by);
                        self.out.items.push(it);
                    }
                    for l in links {
                        let mut l = l.clone();
                        l.rect.x += bx;
                        l.rect.y += by;
                        self.out.links.push(l);
                    }
                    for f in fields {
                        let mut f = f.clone();
                        f.rect.x += bx;
                        f.rect.y += by;
                        self.out.fields.push(f);
                    }
                    if let Some(l) = link {
                        self.out.links.push(Link { rect: Rect::new(bx, by, *bw, *h), node: *l });
                    }
                }
                _ => {}
            }
            k += 1;
        }
        line.clear();
        self.pending_y = baseline + below;
        baseline + below
    }
}

fn transform(t: &str, tr: Transform) -> String {
    match tr {
        Transform::None => String::from(t),
        Transform::Upper => t.to_uppercase(),
        Transform::Lower => t.to_lowercase(),
        Transform::Capitalize => {
            let mut out = String::with_capacity(t.len());
            let mut start = true;
            for c in t.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    if c.is_whitespace() {
                        start = true;
                    }
                    out.push(c);
                }
            }
            out
        }
    }
}

fn expand_tabs(s: &str) -> String {
    if !s.contains('\t') && !s.contains('\r') {
        return String::from(s);
    }
    let mut out = String::new();
    let mut col = 0;
    for c in s.chars() {
        match c {
            '\t' => {
                let n = 8 - col % 8;
                for _ in 0..n {
                    out.push(' ');
                }
                col += n;
            }
            '\r' => {}
            c => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// Collapses runs of spaces (pre-line).
fn collapse(s: &str) -> String {
    let mut out = String::new();
    let mut last_space = false;
    for c in s.chars() {
        if c == ' ' || c == '\t' || c == '\r' {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    out
}

fn counter_text(n: i32, style: ListStyle) -> String {
    match style {
        ListStyle::LowerAlpha | ListStyle::UpperAlpha if n > 0 => {
            let mut s = Vec::new();
            let mut v = n;
            while v > 0 {
                v -= 1;
                s.push((b'a' + (v % 26) as u8) as char);
                v /= 26;
            }
            let s: String = s.into_iter().rev().collect();
            if style == ListStyle::UpperAlpha {
                s.to_uppercase()
            } else {
                s
            }
        }
        ListStyle::LowerRoman | ListStyle::UpperRoman if n > 0 && n < 4000 => {
            let table = [(1000, "m"), (900, "cm"), (500, "d"), (400, "cd"), (100, "c"), (90, "xc"), (50, "l"), (40, "xl"), (10, "x"), (9, "ix"), (5, "v"), (4, "iv"), (1, "i")];
            let mut s = String::new();
            let mut v = n;
            for (val, sym) in table {
                while v >= val {
                    s.push_str(sym);
                    v -= val;
                }
            }
            if style == ListStyle::UpperRoman {
                s.to_uppercase()
            } else {
                s
            }
        }
        _ => format!("{n}"),
    }
}
