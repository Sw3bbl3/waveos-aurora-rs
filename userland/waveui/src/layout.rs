//! Layout: sizing views and placing them in the window.
//!
//! A parent proposes a size to each child; the child answers with the
//! size it wants (text with its natural width, a stack with its children,
//! a scroll view with everything it is offered). Stacks share their space
//! fairly: children that fit keep their size, the rest share what is left,
//! and flexible children (spacers, `.grow()`, scroll views, lists, …) take
//! the remainder. Answers are cached for the frame, so deep trees stay
//! linear.

use crate::style::{Align, Insets, TextStyle};
use crate::view::{Axis, ButtonStyle, ControlSize, ImageSource, Node, PickerStyle, View};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::vec::Vec;
use core::hash::{Hash, Hasher};
use lumen::font::Font;
use lumen::geom::Rect;

/// "As much as you like".
pub(crate) const INF: i32 = 1 << 28;

/// FNV-1a, for state keys.
#[derive(Clone, Copy)]
pub(crate) struct KeyHasher(u64);

impl Default for KeyHasher {
    fn default() -> Self {
        KeyHasher(0xcbf2_9ce4_8422_2325)
    }
}

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 = (self.0 ^ *b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
}

/// The state key for an explicit `.id()`.
pub(crate) fn key_of<K: Hash + ?Sized>(k: &K) -> u64 {
    let mut h = KeyHasher::default();
    0xA11u16.hash(&mut h);
    k.hash(&mut h);
    h.finish()
}

/// The key of the `i`th child of `parent`.
pub(crate) fn child_key(parent: u64, i: usize) -> u64 {
    let mut h = KeyHasher(parent);
    (i as u64).hash(&mut h);
    h.finish()
}

fn style_key(s: TextStyle) -> u32 {
    let f = s.font();
    (f.size as u32) << 8 | f.face as u32
}

/// What a view inherits from those around it.
#[derive(Clone, Copy)]
pub(crate) struct Inherit {
    pub style: TextStyle,
}

impl Inherit {
    pub fn with<M>(self, v: &View<'_, M>) -> Inherit {
        Inherit { style: v.mods.style.unwrap_or(self.style) }
    }
}

pub(crate) fn line_height(f: Font) -> i32 {
    f.line_height as i32 + 2
}

/// The baseline for one line of `f` in a box `h` tall starting at `y`.
pub(crate) fn baseline(f: Font, y: i32, h: i32) -> i32 {
    y + (h + f.ascent as i32 + f.descent as i32) / 2
}

/// A placed view: where it went, and where its children went.
pub(crate) struct Laid<'a, M> {
    /// The whole frame (padding included): backgrounds and hits use this.
    pub rect: Rect,
    /// Where the content itself sits.
    pub node: Rect,
    pub key: u64,
    pub kids: Vec<Laid<'a, M>>,
    /// Built list rows: index, view, placement.
    pub rows: Vec<(usize, Box<View<'a, M>>, Laid<'a, M>)>,
    /// Wrapped text lines (byte ranges).
    pub lines: Vec<(usize, usize)>,
}

impl<M> Laid<'_, M> {
    fn leaf(rect: Rect, node: Rect, key: u64) -> Self {
        Laid { rect, node, key, kids: Vec::new(), rows: Vec::new(), lines: Vec::new() }
    }
}

/// Per-frame layout context.
pub(crate) struct Lay<'s> {
    cache: BTreeMap<(usize, i32, i32, u32), (i32, i32)>,
    pub scroll: &'s mut BTreeMap<u64, crate::runtime::ScrollState>,
}

impl<'s> Lay<'s> {
    pub fn new(scroll: &'s mut BTreeMap<u64, crate::runtime::ScrollState>) -> Self {
        Lay { cache: BTreeMap::new(), scroll }
    }
}

pub(crate) fn control_height(s: ControlSize) -> i32 {
    match s {
        ControlSize::Small => crate::style::size::CONTROL_SMALL,
        ControlSize::Regular => crate::style::size::CONTROL,
        ControlSize::Large => crate::style::size::CONTROL_LARGE,
    }
}

pub(crate) fn button_padding(s: ControlSize) -> i32 {
    match s {
        ControlSize::Small => 10,
        ControlSize::Regular => 14,
        ControlSize::Large => 20,
    }
}

fn propose(p: i32, fixed: Option<i32>, min: Option<i32>, max: Option<i32>) -> i32 {
    if let Some(f) = fixed {
        return f;
    }
    let mut p = p;
    if let Some(m) = max {
        p = p.min(m);
    }
    if let Some(m) = min {
        p = p.max(m);
    }
    p
}

fn resolve(child: i32, p: i32, fixed: Option<i32>, min: Option<i32>, max: Option<i32>) -> i32 {
    if let Some(f) = fixed {
        return f;
    }
    let mut w = child;
    if let Some(m) = max {
        w = if p < INF { w.max(p.min(m)) } else { w.min(m) };
    }
    if let Some(m) = min {
        w = w.max(m);
    }
    w
}

/// How eagerly a view takes extra space along `axis` (0: not at all).
pub(crate) fn flex<M>(v: &View<'_, M>, axis: Axis) -> u16 {
    if v.mods.hidden {
        return 0;
    }
    let fr = &v.mods.frame;
    let (fixed, max) = match axis {
        Axis::Horizontal => (fr.width, fr.max_width),
        Axis::Vertical => (fr.height, fr.max_height),
    };
    if fixed.is_some() {
        return 0;
    }
    if v.mods.grow > 0 {
        return v.mods.grow;
    }
    if max == Some(i32::MAX) {
        return 1;
    }
    if max.is_some() {
        return 0;
    }
    match &v.node {
        Node::Spacer { .. } => 1,
        Node::Scroll { axis: a, .. } if *a == axis => 1,
        Node::Scroll { .. } => 0,
        Node::List { .. } | Node::Split { .. } | Node::TextEditor { .. } | Node::Canvas { .. } => 1,
        Node::Stack { axis: a, children, .. } if *a == axis => {
            children.iter().map(|c| flex(c, axis)).max().unwrap_or(0)
        }
        Node::Stack { children, .. } | Node::ZStack { children, .. } => {
            children.iter().map(|c| flex(c, axis)).max().unwrap_or(0)
        }
        Node::TextField { .. } | Node::Slider { .. } | Node::Progress { circular: false, .. } => {
            (axis == Axis::Horizontal) as u16
        }
        Node::Toggle { label, .. } | Node::Stepper { label, .. } => {
            (axis == Axis::Horizontal && !label.is_empty()) as u16
        }
        Node::Card { .. } | Node::Grid { .. } => (axis == Axis::Horizontal) as u16,
        _ => 0,
    }
}

/// Stretches across a stack (dividers, spacers).
fn stretches<M>(v: &View<'_, M>) -> bool {
    matches!(v.node, Node::Divider | Node::Spacer { .. })
}

/// The size `v` wants, offered `pw` × `ph`.
pub(crate) fn measure<M>(v: &View<'_, M>, pw: i32, ph: i32, inh: Inherit, lay: &mut Lay) -> (i32, i32) {
    if v.mods.hidden {
        return (0, 0);
    }
    let key = (v as *const View<'_, M> as usize, pw, ph, style_key(inh.style));
    if let Some(s) = lay.cache.get(&key) {
        return *s;
    }
    let inh = inh.with(v);
    let fr = v.mods.frame;
    let pad = v.mods.padding;
    let cw = sub(propose(pw, fr.width, fr.min_width, fr.max_width), pad.horizontal());
    let ch = sub(propose(ph, fr.height, fr.min_height, fr.max_height), pad.vertical());
    let (w, h) = measure_node(v, cw, ch, inh, lay);
    let out = (
        resolve(add(w, pad.horizontal()), pw, fr.width, fr.min_width, fr.max_width),
        resolve(add(h, pad.vertical()), ph, fr.height, fr.min_height, fr.max_height),
    );
    lay.cache.insert(key, out);
    out
}

fn sub(a: i32, b: i32) -> i32 {
    if a >= INF {
        INF
    } else {
        (a - b).max(0)
    }
}

fn add(a: i32, b: i32) -> i32 {
    if a >= INF {
        INF
    } else {
        a + b
    }
}

fn bounded(p: i32, ideal: i32) -> i32 {
    if p < INF {
        p
    } else {
        ideal
    }
}

pub(crate) fn text_size(text: &str, lines: Option<u32>, font: Font, pw: i32) -> (i32, i32, Vec<(usize, usize)>) {
    let lh = line_height(font);
    if lines == Some(1) || pw >= INF {
        let w = font.width(text.lines().next().unwrap_or(""));
        let n = if lines == Some(1) { 1 } else { text.lines().count().max(1) as i32 };
        let w = if n > 1 { text.lines().map(|l| font.width(l)).max().unwrap_or(0) } else { w };
        return (w.min(pw), lh * n, Vec::new());
    }
    let mut wrapped = lumen::widgets::wrap(text, font, pw.max(8));
    if let Some(n) = lines {
        wrapped.truncate(n as usize);
    }
    let w = wrapped.iter().map(|&(s, e)| font.width(text[s..e].trim_end())).max().unwrap_or(0);
    (w.min(pw), lh * wrapped.len().max(1) as i32, wrapped)
}

fn measure_node<M>(v: &View<'_, M>, pw: i32, ph: i32, inh: Inherit, lay: &mut Lay) -> (i32, i32) {
    let body = inh.style.font();
    match &v.node {
        Node::Empty => (0, 0),
        Node::Spacer { min } => (*min, *min),
        Node::Divider => (1, 1),
        Node::Text { text, lines, .. } => {
            let (w, h, _) = text_size(text, *lines, body, pw);
            (w, h)
        }
        Node::Image { source, size, mode } => match (source, size) {
            (_, Some(s)) => *s,
            (ImageSource::Pixmap(p), None) => {
                let (iw, ih) = (p.width as i32, p.height as i32);
                if *mode == crate::view::ContentMode::Fill && pw < INF && ph < INF {
                    return (pw, ph);
                }
                let scale_w = if pw < iw { pw as i64 * 1024 / iw.max(1) as i64 } else { 1024 };
                let scale_h = if ph < ih { ph as i64 * 1024 / ih.max(1) as i64 } else { 1024 };
                let s = scale_w.min(scale_h);
                ((iw as i64 * s / 1024) as i32, (ih as i64 * s / 1024) as i32)
            }
            _ => (16, 16),
        },
        Node::Button { label, style, size, .. } => {
            let h = control_height(*size);
            match style {
                ButtonStyle::Plain => measure(label, pw, ph, inh, lay),
                ButtonStyle::Toolbar => {
                    let (lw, lh) = measure(label, sub(pw, 14), ph, inh, lay);
                    ((lw + 14).max(h), (lh + 14).max(h))
                }
                ButtonStyle::Row => {
                    let (lw, lh) = measure(label, sub(pw, 20), ph, inh, lay);
                    (bounded(pw, lw + 20), (lh + 12).max(32))
                }
                _ => {
                    let p = button_padding(*size);
                    let (lw, lh) = measure(label, sub(pw, 2 * p), ph, inh, lay);
                    ((lw + 2 * p).max(h), (lh + 8).max(h))
                }
            }
        }
        Node::Toggle { label, .. } => {
            if label.is_empty() {
                (40, 24)
            } else {
                let (tw, th, _) = text_size(label, Some(1), body, sub(pw, 52));
                (bounded(pw, tw + 52), th.max(24))
            }
        }
        Node::Checkbox { label, .. } => {
            let (tw, th, _) = text_size(label, Some(1), body, sub(pw, 24));
            (tw + 24, th.max(18))
        }
        Node::Slider { .. } => (bounded(pw, 200).max(60), 24),
        Node::Stepper { label, value, .. } => {
            let vw = body.width(&format!("{value}")).max(16);
            let control = vw + 12 + 72;
            if label.is_empty() {
                (control, 26)
            } else {
                let (tw, th, _) = text_size(label, Some(1), body, sub(pw, control + 12));
                (bounded(pw, tw + 12 + control), th.max(26))
            }
        }
        Node::Picker { options, style, .. } => match style {
            PickerStyle::Segmented => {
                let ideal: i32 = options.iter().map(|o| body.width(o) + 28).sum();
                (ideal.min(pw).max(40), 28)
            }
            PickerStyle::Menu => {
                let w = options.iter().map(|o| body.width(o)).max().unwrap_or(0) + 28 + 18;
                (w.min(pw), crate::style::size::CONTROL)
            }
            PickerStyle::Inline => {
                let w = options.iter().map(|o| body.width(o)).max().unwrap_or(0) + 36;
                (bounded(pw, w), 30 * options.len() as i32)
            }
        },
        Node::TextField { .. } => (bounded(pw, 220).max(60), crate::style::size::CONTROL),
        Node::TextEditor { .. } => (bounded(pw, 320), bounded(ph, 160)),
        Node::Progress { circular: true, .. } => (20, 20),
        Node::Progress { .. } => (bounded(pw, 160), 6),
        Node::Stack { axis, spacing, align, children } => {
            let (sizes, main, cross) = allot(children, *axis, *spacing, *align, pw, ph, inh, lay);
            let _ = sizes;
            match axis {
                Axis::Vertical => (cross, main),
                Axis::Horizontal => (main, cross),
            }
        }
        Node::ZStack { children, .. } => children.iter().fold((0, 0), |(w, h), c| {
            let (cw, ch) = measure(c, pw, ph, inh, lay);
            (w.max(cw), h.max(ch))
        }),
        Node::Scroll { axis, child, .. } => {
            let (cw, ch) = match axis {
                Axis::Vertical => measure(child, pw, INF, inh, lay),
                Axis::Horizontal => measure(child, INF, ph, inh, lay),
            };
            match axis {
                Axis::Vertical => (bounded(pw, cw), bounded(ph, ch)),
                Axis::Horizontal => (bounded(pw, cw), ch.min(ph)),
            }
        }
        Node::List { count, row_height, .. } => (bounded(pw, 240), bounded(ph, *count as i32 * row_height)),
        Node::Grid { min_width, spacing, children } => {
            let w = bounded(pw, (*min_width + spacing) * 3);
            let (cols, col_w) = grid_columns(w, *min_width, *spacing);
            let mut h = 0;
            for row in children.chunks(cols) {
                let rh = row.iter().map(|c| measure(c, col_w, INF, inh, lay).1).max().unwrap_or(0);
                h += rh + spacing;
            }
            (w, (h - spacing).max(0))
        }
        Node::Card { children, separators, inset } => {
            let cw = sub(pw, 2 * inset);
            let mut w = 0;
            let mut h = 0;
            let mut n = 0;
            for c in children.iter().filter(|c| !c.mods.hidden) {
                let (a, b) = measure(c, cw, INF, inh, lay);
                w = w.max(a);
                h += b + if *separators && n > 0 { 1 } else { 0 };
                n += 1;
            }
            let vpad = if *separators { 4 } else { 0 };
            (bounded(pw, w + 2 * inset), h + 2 * vpad)
        }
        Node::Split { width, .. } => (bounded(pw, width + 400), bounded(ph, 320)),
        Node::Canvas { .. } => (bounded(pw, 200), bounded(ph, 200)),
    }
}

pub(crate) fn grid_columns(w: i32, min_width: i32, spacing: i32) -> (usize, i32) {
    let cols = ((w + spacing) / (min_width + spacing).max(1)).max(1);
    let col_w = (w - spacing * (cols - 1)) / cols;
    (cols as usize, col_w)
}

/// Shares a stack's main axis among its children. Returns each child's
/// size (0 for hidden ones), the stack's main and cross sizes.
#[allow(clippy::too_many_arguments)]
fn allot<M>(
    children: &[View<'_, M>],
    axis: Axis,
    spacing: i32,
    align: Align,
    pw: i32,
    ph: i32,
    inh: Inherit,
    lay: &mut Lay,
) -> (Vec<(i32, i32)>, i32, i32) {
    let (pmain, pcross) = match axis {
        Axis::Horizontal => (pw, ph),
        Axis::Vertical => (ph, pw),
    };
    let size = |c: &View<'_, M>, main: i32, cross: i32, lay: &mut Lay| -> (i32, i32) {
        let (w, h) = match axis {
            Axis::Horizontal => measure(c, main, cross, inh, lay),
            Axis::Vertical => measure(c, cross, main, inh, lay),
        };
        match axis {
            Axis::Horizontal => (w, h),
            Axis::Vertical => (h, w),
        }
    };
    let visible = children.iter().filter(|c| !c.mods.hidden).count() as i32;
    let gaps = spacing * (visible - 1).max(0);
    let avail = sub(pmain, gaps);
    let mut out = alloc::vec![(0, 0); children.len()];
    let flexes: Vec<u16> = children.iter().map(|c| flex(c, axis)).collect();
    let flex_min = |c: &View<'_, M>| match c.node {
        Node::Spacer { min } => min,
        _ => 0,
    };
    let reserved: i32 = children.iter().zip(&flexes).filter(|(_, f)| **f > 0).map(|(c, _)| flex_min(c)).sum();

    // Fixed children: their ideal size, or a fair share when they don't all fit.
    let fixed: Vec<usize> = (0..children.len()).filter(|&i| flexes[i] == 0 && !children[i].mods.hidden).collect();
    let mut ideals: Vec<(usize, (i32, i32))> =
        fixed.iter().map(|&i| (i, size(&children[i], avail, pcross, lay))).collect();
    let total: i32 = ideals.iter().map(|(_, s)| s.0).sum();
    if avail < INF && total > sub(avail, reserved) {
        ideals.sort_by_key(|(_, s)| s.0);
        let mut left = sub(avail, reserved);
        let mut n = ideals.len() as i32;
        for (i, s) in ideals.iter_mut() {
            let share = left / n.max(1);
            if s.0 > share {
                *s = size(&children[*i], share, pcross, lay);
                s.0 = s.0.min(share);
            }
            left -= s.0;
            n -= 1;
        }
    }
    let mut used = 0;
    for (i, s) in ideals {
        out[i] = s;
        used += s.0;
    }
    // Flexible children share the rest by weight.
    let weight: i32 = flexes.iter().map(|&f| f as i32).sum();
    if weight > 0 {
        let mut left = if avail < INF { (avail - used).max(0) } else { INF };
        let mut wleft = weight;
        for i in 0..children.len() {
            let f = flexes[i] as i32;
            if f == 0 {
                continue;
            }
            let c = &children[i];
            let share = if left >= INF { INF } else { left * f / wleft.max(1) };
            let s = match c.node {
                Node::Spacer { min } => (if share >= INF { min } else { share.max(min) }, 0),
                _ => {
                    let s = size(c, share, pcross, lay);
                    if share < INF {
                        (share.max(s.0.min(share)), s.1)
                    } else {
                        s
                    }
                }
            };
            out[i] = s;
            if left < INF {
                left -= s.0;
            }
            wleft -= f;
            used += s.0;
        }
    }
    let mut cross = children.iter().zip(&out).filter(|(c, _)| !stretches(c)).map(|(_, s)| s.1).max().unwrap_or(0);
    if align == Align::Stretch && pcross < INF {
        cross = pcross;
    }
    let main = if weight > 0 && pmain < INF { pmain } else { used + gaps };
    (out, main, cross)
}

fn align_in(outer: Rect, w: i32, h: i32, a: Align) -> Rect {
    let w = w.min(outer.w);
    let h = h.min(outer.h);
    match a {
        Align::Start => Rect::new(outer.x, outer.y, w, h),
        Align::Center => Rect::new(outer.x + (outer.w - w) / 2, outer.y + (outer.h - h) / 2, w, h),
        Align::End => Rect::new(outer.right() - w, outer.bottom() - h, w, h),
        Align::Stretch => outer,
    }
}

fn inset(r: Rect, i: Insets) -> Rect {
    Rect::new(r.x + i.leading, r.y + i.top, (r.w - i.horizontal()).max(0), (r.h - i.vertical()).max(0))
}

/// Does `v` fill its frame rather than sit at its natural size in it?
/// Everything but text and images stretches to the frame it is given.
fn fills<M>(v: &View<'_, M>) -> bool {
    !matches!(v.node, Node::Text { .. } | Node::Image { .. } | Node::Empty)
}

/// Places `v` in `rect` (the size its parent gave it).
pub(crate) fn place<'a, M>(v: &View<'a, M>, rect: Rect, key: u64, inh: Inherit, lay: &mut Lay) -> Laid<'a, M> {
    let key = v.mods.id.unwrap_or(key);
    if v.mods.hidden {
        return Laid::leaf(Rect::new(rect.x, rect.y, 0, 0), Rect::new(rect.x, rect.y, 0, 0), key);
    }
    let inh = inh.with(v);
    let inner = inset(rect, v.mods.padding);
    let node = if fills(v) || v.mods.frame.align == Align::Stretch {
        inner
    } else if matches!(v.node, Node::Text { .. }) {
        // Aligned text spans the width; its lines align within it.
        let (_, h) = measure_node(v, inner.w, inner.h, inh, lay);
        let r = align_in(inner, inner.w, h, v.mods.frame.align);
        Rect::new(inner.x, r.y, inner.w, r.h)
    } else {
        let (w, h) = measure_node(v, inner.w, inner.h, inh, lay);
        align_in(inner, w, h, v.mods.frame.align)
    };
    let mut out = Laid::leaf(rect, node, key);
    match &v.node {
        Node::Text { text, lines, .. } => {
            if *lines != Some(1) {
                out.lines = text_size(text, *lines, inh.style.font(), node.w.max(1)).2;
            }
        }
        Node::Button { label, style, size, .. } => {
            let r = match style {
                ButtonStyle::Plain => node,
                ButtonStyle::Row => Rect::new(node.x + 10, node.y, node.w - 20, node.h),
                ButtonStyle::Toolbar => node.inset(7),
                _ => Rect::new(node.x + button_padding(*size), node.y, node.w - 2 * button_padding(*size), node.h),
            };
            let (lw, lh) = measure(label, r.w, r.h, inh, lay);
            let a = if *style == ButtonStyle::Row { Align::Start } else { Align::Center };
            let lr = match a {
                Align::Start => Rect::new(r.x, r.y + (r.h - lh) / 2, lw.min(r.w), lh),
                _ => align_in(r, lw, lh, Align::Center),
            };
            out.kids.push(place(label, lr, child_key(key, 0), inh, lay));
        }
        Node::Stack { axis, spacing, align, children } => {
            let (sizes, _, _) = allot(children, *axis, *spacing, *align, node.w, node.h, inh, lay);
            let mut at = match axis {
                Axis::Horizontal => node.x,
                Axis::Vertical => node.y,
            };
            for (i, c) in children.iter().enumerate() {
                let (main, cross) = sizes[i];
                let stretch = *align == Align::Stretch
                    || stretches(c)
                    || match axis {
                        Axis::Horizontal => c.mods.frame.max_height == Some(i32::MAX),
                        Axis::Vertical => c.mods.frame.max_width == Some(i32::MAX),
                    };
                let r = match axis {
                    Axis::Horizontal => {
                        let h = if stretch { node.h } else { cross.min(node.h) };
                        let y = match align {
                            Align::Start | Align::Stretch => node.y,
                            Align::Center => node.y + (node.h - h) / 2,
                            Align::End => node.bottom() - h,
                        };
                        Rect::new(at, if stretch { node.y } else { y }, main, h)
                    }
                    Axis::Vertical => {
                        let w = if stretch { node.w } else { cross.min(node.w) };
                        let x = match align {
                            Align::Start | Align::Stretch => node.x,
                            Align::Center => node.x + (node.w - w) / 2,
                            Align::End => node.right() - w,
                        };
                        Rect::new(if stretch { node.x } else { x }, at, w, main)
                    }
                };
                out.kids.push(place(c, r, child_key(key, i), inh, lay));
                if !c.mods.hidden {
                    at += main + spacing;
                }
            }
        }
        Node::ZStack { align, children } => {
            for (i, c) in children.iter().enumerate() {
                let (w, h) = measure(c, node.w, node.h, inh, lay);
                let r = if flex(c, Axis::Horizontal) > 0 && flex(c, Axis::Vertical) > 0 {
                    node
                } else {
                    align_in(node, w, h, *align)
                };
                out.kids.push(place(c, r, child_key(key, i), inh, lay));
            }
        }
        Node::Scroll { axis, child, stick_to_end } => {
            let (cw, ch) = match axis {
                Axis::Vertical => measure(child, node.w, INF, inh, lay),
                Axis::Horizontal => measure(child, INF, node.h, inh, lay),
            };
            let (content, viewport) = match axis {
                Axis::Vertical => (ch.max(node.h), node.h),
                Axis::Horizontal => (cw.max(node.w), node.w),
            };
            let off = lay.scroll.entry(key).or_default().layout(content, viewport, *stick_to_end);
            let r = match axis {
                Axis::Vertical => Rect::new(node.x, node.y - off, node.w, content),
                Axis::Horizontal => Rect::new(node.x - off, node.y, content, node.h),
            };
            out.kids.push(place(child, r, child_key(key, 0), inh, lay));
        }
        Node::List { count, row_height, rows, .. } => {
            let rh = (*row_height).max(1);
            let content = *count as i32 * rh;
            let off = lay.scroll.entry(key).or_default().layout(content, node.h, false);
            let first = (off / rh).max(0) as usize;
            let last = (((off + node.h) / rh + 1).max(0) as usize).min(*count);
            for i in first..last {
                let row = Box::new(rows(i));
                let r = Rect::new(node.x, node.y + i as i32 * rh - off, node.w, rh);
                let laid = place(&row, r, child_key(key, i), inh, lay);
                out.rows.push((i, row, laid));
            }
        }
        Node::Grid { min_width, spacing, children } => {
            let (cols, col_w) = grid_columns(node.w, *min_width, *spacing);
            let mut y = node.y;
            for (r, row) in children.chunks(cols).enumerate() {
                let rh = row.iter().map(|c| measure(c, col_w, INF, inh, lay).1).max().unwrap_or(0);
                for (k, c) in row.iter().enumerate() {
                    let cell = Rect::new(node.x + k as i32 * (col_w + spacing), y, col_w, rh);
                    out.kids.push(place(c, cell, child_key(key, r * cols + k), inh, lay));
                }
                y += rh + spacing;
            }
        }
        Node::Card { children, separators, inset } => {
            let vpad = if *separators { 4 } else { 0 };
            let mut y = node.y + vpad;
            let mut n = 0;
            for (i, c) in children.iter().enumerate() {
                if c.mods.hidden {
                    out.kids.push(place(c, Rect::new(node.x, y, 0, 0), child_key(key, i), inh, lay));
                    continue;
                }
                if *separators && n > 0 {
                    y += 1;
                }
                let w = node.w - 2 * inset;
                let (_, h) = measure(c, w, INF, inh, lay);
                out.kids.push(place(c, Rect::new(node.x + inset, y, w, h), child_key(key, i), inh, lay));
                y += h;
                n += 1;
            }
        }
        Node::Split { sidebar, detail, width } => {
            let w = (*width).min(node.w / 2);
            out.kids.push(place(sidebar, Rect::new(node.x, node.y, w, node.h), child_key(key, 0), inh, lay));
            let d = Rect::new(node.x + w + 1, node.y, node.w - w - 1, node.h);
            out.kids.push(place(detail, d, child_key(key, 1), inh, lay));
        }
        _ => {}
    }
    out
}
