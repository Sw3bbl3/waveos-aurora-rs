//! Drawing views: text, images and the controls, in the WaveOS look.

use crate::layout::{baseline, line_height, Laid};
use crate::runtime::{Hit, HitKind, Rt, ScrollKind, Scroller};
use crate::style::{radius, Align, Color, TextStyle};
use crate::view::{Axis, ButtonStyle, ControlSize, ImageSource, Node, PickerStyle, Symbol, View};
use alloc::format;
use alloc::vec::Vec;
use lumen::canvas::{mix, rgb, with_alpha, Canvas};
use lumen::geom::Rect;
use lumen::theme;

const WHITE: u32 = 0xFFFF_FFFF;

/// What drawing inherits from the views around.
#[derive(Clone, Copy)]
pub(crate) struct Paint {
    pub style: TextStyle,
    pub fg: Option<Color>,
    pub alpha: u8,
    /// On an accent-coloured selection: everything draws white.
    pub on_accent: bool,
    pub disabled: bool,
    pub layer: u8,
}

impl Paint {
    pub fn root(layer: u8) -> Paint {
        Paint { style: TextStyle::Body, fg: None, alpha: 255, on_accent: false, disabled: false, layer }
    }

    /// The colour for content whose default is `default`.
    pub fn ink(&self, default: Color) -> u32 {
        let c = if self.on_accent { WHITE } else { self.fg.unwrap_or(default).argb() };
        fade(c, self.alpha)
    }
}

pub(crate) fn fade(c: u32, a: u8) -> u32 {
    if a == 255 {
        return c;
    }
    let ca = (c >> 24) * a as u32 / 255;
    (c & 0x00FF_FFFF) | (ca << 24)
}

fn mul(a: u8, b: u8) -> u8 {
    (a as u32 * b as u32 / 255) as u8
}

/// The selection colour: accent when the window is active, grey otherwise.
pub(crate) fn selection(active: bool) -> u32 {
    if active {
        theme::accent()
    } else {
        let t = theme::current();
        mix(t.window_bg, t.text_secondary, 70)
    }
}

/// The track behind segmented controls and switches.
fn track() -> u32 {
    let t = theme::current();
    mix(t.window_bg, t.text, if t.dark { 40 } else { 26 })
}

/// A raised surface: segments, knobs.
fn raised() -> u32 {
    let t = theme::current();
    if t.dark {
        mix(t.window_bg, WHITE, 52)
    } else {
        WHITE
    }
}

fn focus_ring(cv: &mut Canvas, r: Rect, rad: i32) {
    let c = with_alpha(theme::accent(), 0x90);
    cv.stroke_round_rect(r.inset(-2), rad + 2, c);
    cv.stroke_round_rect(r.inset(-3), rad + 3, c);
}

fn knob(cv: &mut Canvas, cx: i32, cy: i32, rad: i32, alpha: u8) {
    let t = theme::current();
    cv.shadow(Rect::new(cx - rad, cy - rad, 2 * rad, 2 * rad), rad, 4, 1, if t.dark { 120 } else { 60 });
    cv.fill_circle(cx, cy, rad, fade(raised(), alpha));
    if !t.dark {
        cv.stroke_round_rect(
            Rect::new(cx - rad, cy - rad, 2 * rad, 2 * rad),
            rad,
            fade(with_alpha(t.text, 0x18), alpha),
        );
    }
}

/// Draws `v` (placed at `l`) and records what can be interacted with.
pub(crate) fn render<M>(v: &View<'_, M>, l: &Laid<'_, M>, cv: &mut Canvas, p: Paint, rt: &mut Rt, path: &mut Vec<u32>) {
    if v.mods.hidden {
        return;
    }
    let m = &v.mods;
    let mut p = p;
    if let Some(s) = m.style {
        p.style = s;
    }
    if let Some(c) = m.foreground {
        p.fg = Some(c);
    }
    if let Some(a) = m.opacity {
        p.alpha = mul(p.alpha, a);
    }
    if m.disabled {
        p.disabled = true;
        p.alpha = mul(p.alpha, 100);
    }
    rt.seen.insert(l.key);
    let r = l.rect;
    if !r.intersects(&cv.clip) && !matches!(v.node, Node::Scroll { .. } | Node::List { .. }) {
        return;
    }
    let t = theme::current();
    let rad = m.corner.or(m.background.map(|b| b.1)).or(m.border.map(|b| b.1)).unwrap_or(radius::CONTROL);
    if m.shadow {
        cv.shadow(r, rad, 18, 6, if t.dark { 150 } else { 60 });
    }
    if m.material {
        rt.materials.push(r);
        cv.fill_rect(r, with_alpha(t.window_bg_alt, 24));
    }
    if let Some((c, rr)) = m.background {
        cv.fill_round_rect(r, m.corner.unwrap_or(rr), fade(c.argb(), p.alpha));
    }
    let tappable = !p.disabled && (m.on_tap.is_some() || m.on_double_tap.is_some());
    let hovered = rt.hover == Some(l.key);
    let pressed = rt.pressed == Some(l.key);
    if m.selected {
        cv.fill_round_rect(r, rad, fade(selection(rt.active), p.alpha));
        p.on_accent = rt.active;
    } else if tappable && (hovered || pressed) {
        cv.fill_round_rect(r, rad, if pressed { with_alpha(t.text, 0x22) } else { t.hover });
    }
    if let Some((c, rr)) = m.border {
        cv.stroke_round_rect(r, m.corner.unwrap_or(rr), fade(c.argb(), p.alpha));
    }
    if rt.drop_target == Some(l.key) {
        focus_ring(cv, r.inset(2), rad);
    }
    if tappable && rt.focus == Some(l.key) && rt.focus_visible {
        focus_ring(cv, r, rad);
    }
    let wants_hit = tappable
        || m.on_hover.is_some()
        || !m.context_menu.is_empty()
        || m.help.is_some()
        || m.on_drop.is_some()
        || m.drag.is_some();
    if wants_hit && !p.disabled {
        rt.hit(Hit {
            rect: r.intersect(&cv.clip),
            full: r,
            key: l.key,
            path: path.clone(),
            layer: p.layer,
            kind: HitKind::Tap,
            focusable: tappable,
            shortcut: if m.on_tap.is_some() { m.shortcut } else { None },
            tap: m.on_tap.is_some(),
            double: m.on_double_tap.is_some(),
            menu: !m.context_menu.is_empty(),
            hover: m.on_hover.is_some(),
            help: m.help.clone(),
            drop: m.on_drop.is_some(),
            drag: m.drag.is_some(),
            appear: false,
        });
    }
    if m.clip {
        cv.with_clip(r, |cv| node(v, l, cv, p, rt, path));
    } else {
        node(v, l, cv, p, rt, path);
    }
    if let Some(f) = &m.custom {
        f(cv, r);
    }
}

fn control_hit<M>(v: &View<'_, M>, l: &Laid<'_, M>, cv: &Canvas, p: Paint, path: &[u32], kind: HitKind) -> Hit {
    Hit {
        rect: l.node.intersect(&cv.clip),
        full: l.node,
        key: l.key,
        path: path.to_vec(),
        layer: p.layer,
        kind,
        focusable: true,
        shortcut: v.mods.shortcut,
        tap: false,
        double: false,
        menu: false,
        hover: false,
        help: None,
        drop: false,
        drag: false,
        appear: v.mods.focus_on_appear,
    }
}

fn kids<M>(children: &[View<'_, M>], l: &Laid<'_, M>, cv: &mut Canvas, p: Paint, rt: &mut Rt, path: &mut Vec<u32>) {
    for (i, (c, cl)) in children.iter().zip(&l.kids).enumerate() {
        path.push(i as u32);
        render(c, cl, cv, p, rt, path);
        path.pop();
    }
}

fn node<M>(v: &View<'_, M>, l: &Laid<'_, M>, cv: &mut Canvas, p: Paint, rt: &mut Rt, path: &mut Vec<u32>) {
    let n = l.node;
    let t = theme::current();
    let body = p.style.font();
    let key = l.key;
    let enabled = !p.disabled;
    let focused = rt.focus == Some(key);
    let ring = focused && rt.focus_visible;
    match &v.node {
        Node::Empty | Node::Spacer { .. } => {}
        Node::Divider => cv.fill_rect(n, fade(t.separator, p.alpha)),
        Node::Text { text, lines, align, .. } => {
            let c = p.ink(Color::Label);
            let lh = line_height(body);
            let draw_line = |cv: &mut Canvas, i: usize, s: &str| {
                let w = body.width(s).min(n.w);
                let x = match align {
                    Align::Center => n.x + (n.w - w) / 2,
                    Align::End => n.right() - w,
                    _ => n.x,
                };
                cv.text_clipped(x, baseline(body, n.y + i as i32 * lh, lh), s, body, c, n.w);
            };
            if l.lines.is_empty() {
                let take = if *lines == Some(1) { 1 } else { usize::MAX };
                for (i, s) in text.lines().take(take).enumerate() {
                    draw_line(cv, i, s);
                }
            } else {
                for (i, &(s, e)) in l.lines.iter().enumerate() {
                    draw_line(cv, i, text[s..e].trim_end());
                }
            }
        }
        Node::Image { source, mode, .. } => match source {
            ImageSource::Symbol(s) => crate::symbol::draw(cv, *s, n, p.ink(Color::Label)),
            ImageSource::AppIcon(icon) => lumen::icons::draw(cv, *icon, n),
            ImageSource::Pixmap(px) => {
                let (iw, ih) = (px.width as i64, px.height as i64);
                if iw == 0 || ih == 0 {
                    return;
                }
                let fit_w = n.w as i64 * ih <= n.h as i64 * iw;
                let fill = *mode == crate::view::ContentMode::Fill;
                let (w, h) =
                    if fit_w != fill { (n.w as i64, n.w as i64 * ih / iw) } else { (n.h as i64 * iw / ih, n.h as i64) };
                let dst = Rect::new(n.x + (n.w - w as i32) / 2, n.y + (n.h - h as i32) / 2, w as i32, h as i32);
                cv.with_clip(n, |cv| cv.draw_image(&px.pixels, px.width, px.height, dst, p.alpha as u32));
            }
        },
        Node::Button { label, style, size, menu, .. } => {
            let hovered = enabled && rt.hover == Some(key);
            let pressed = enabled && rt.pressed == Some(key);
            let rad = v.mods.corner.unwrap_or(match size {
                ControlSize::Small => 6,
                ControlSize::Regular => radius::CONTROL,
                ControlSize::Large => 10,
            });
            let tint = v.mods.tint.map(|c| c.argb());
            let mut lp = p;
            match style {
                ButtonStyle::Bordered => {
                    let mut bg = tint.unwrap_or(t.control_bg);
                    if pressed {
                        bg = mix(bg, t.text, 40);
                    } else if hovered {
                        bg = mix(bg, t.text, 16);
                    }
                    cv.fill_round_rect(n, rad, fade(bg, p.alpha));
                    if tint.is_none() || !t.dark {
                        cv.stroke_round_rect(n, rad, fade(t.control_border, p.alpha));
                    }
                }
                ButtonStyle::Prominent | ButtonStyle::Destructive => {
                    let mut bg = match (style, tint) {
                        (_, Some(c)) => c,
                        (ButtonStyle::Prominent, None) => theme::accent(),
                        _ => Color::Red.argb(),
                    };
                    if pressed {
                        bg = mix(bg, rgb(0, 0, 0), 48);
                    } else if hovered {
                        bg = mix(bg, WHITE, 28);
                    }
                    cv.fill_round_rect(n, rad, fade(bg, p.alpha));
                    lp.on_accent = true;
                }
                ButtonStyle::Plain => {
                    if lp.fg.is_none() {
                        lp.fg = Some(v.mods.tint.unwrap_or(Color::AccentText));
                    }
                    if pressed {
                        lp.alpha = mul(lp.alpha, 150);
                    } else if hovered {
                        lp.alpha = mul(lp.alpha, 210);
                    }
                }
                ButtonStyle::Toolbar | ButtonStyle::Row => {
                    if pressed {
                        cv.fill_round_rect(n, rad, with_alpha(t.text, 0x22));
                    } else if hovered || !menu.is_empty() && rt.menu_owner() == Some(key) {
                        cv.fill_round_rect(n, rad, t.hover);
                    }
                }
            }
            if ring {
                focus_ring(cv, n, rad);
            }
            if let Some(ll) = l.kids.first() {
                path.push(0);
                render(label, ll, cv, lp, rt, path);
                path.pop();
            }
            if enabled {
                rt.hit(control_hit(v, l, cv, p, path, HitKind::Button { menu: !menu.is_empty() }));
            }
        }
        Node::Toggle { label, on, .. } => {
            let sw = Rect::new(n.right() - 40, n.y + (n.h - 24) / 2, 40, 24);
            if !label.is_empty() {
                cv.text_clipped(n.x, baseline(body, n.y, n.h), label, body, p.ink(Color::Label), n.w - 52);
            }
            let k = rt.anim(key, if *on { 1.0 } else { 0.0 });
            let fill = mix(track(), theme::accent(), (k * 256.0) as i32);
            cv.fill_round_rect(sw, 12, fade(fill, p.alpha));
            let x = sw.x + 12 + (k * 16.0) as i32;
            knob(cv, x, sw.y + 12, 10, p.alpha);
            if ring {
                focus_ring(cv, sw, 12);
            }
            if enabled {
                let mut h = control_hit(v, l, cv, p, path, HitKind::Toggle(*on));
                if label.is_empty() {
                    h.rect = sw.intersect(&cv.clip);
                }
                rt.hit(h);
            }
        }
        Node::Checkbox { label, on, .. } => {
            let b = Rect::new(n.x, n.y + (n.h - 16) / 2, 16, 16);
            if *on {
                cv.fill_round_rect(b, 4, fade(theme::accent(), p.alpha));
                crate::symbol::draw(cv, Symbol::Check, b.inset(2), fade(WHITE, p.alpha));
            } else {
                cv.fill_round_rect(b, 4, fade(t.control_bg, p.alpha));
                cv.stroke_round_rect(b, 4, fade(t.control_border, p.alpha));
            }
            if ring {
                focus_ring(cv, b, 4);
            }
            cv.text_clipped(n.x + 24, baseline(body, n.y, n.h), label, body, p.ink(Color::Label), n.w - 24);
            if enabled {
                rt.hit(control_hit(v, l, cv, p, path, HitKind::Toggle(*on)));
            }
        }
        Node::Slider { value, min, max, step, .. } => {
            let track_r = Rect::new(n.x + 10, n.y + n.h / 2 - 2, (n.w - 20).max(1), 4);
            let span = (max - min).abs().max(f32::EPSILON);
            let k = ((value - min) / span).clamp(0.0, 1.0);
            let x = track_r.x + (k * track_r.w as f32) as i32;
            cv.fill_round_rect(track_r, 2, fade(track(), p.alpha));
            cv.fill_round_rect(Rect::new(track_r.x, track_r.y, x - track_r.x, 4), 2, fade(theme::accent(), p.alpha));
            knob(cv, x, n.y + n.h / 2, 10, p.alpha);
            if ring {
                focus_ring(cv, Rect::new(x - 10, n.y + n.h / 2 - 10, 20, 20), 10);
            }
            if enabled {
                let kind = HitKind::Slider { min: *min, max: *max, step: *step, value: *value, track: track_r };
                rt.hit(control_hit(v, l, cv, p, path, kind));
            }
        }
        Node::Stepper { label, value, min, max, step, .. } => {
            let c = Rect::new(n.right() - 72, n.y + (n.h - 26) / 2, 72, 26);
            let text = format!("{value}");
            let vw = body.width(&text);
            let base = baseline(body, n.y, n.h);
            if !label.is_empty() {
                cv.text_clipped(n.x, base, label, body, p.ink(Color::Label), c.x - vw - 24 - n.x);
            }
            cv.text(c.x - 12 - vw, base, &text, body, p.ink(Color::Label));
            cv.fill_round_rect(c, 7, fade(t.control_bg, p.alpha));
            cv.stroke_round_rect(c, 7, fade(t.control_border, p.alpha));
            cv.fill_rect(Rect::new(c.x + 36, c.y + 5, 1, c.h - 10), t.separator);
            let minus = Rect::new(c.x, c.y, 36, c.h);
            let plus = Rect::new(c.x + 36, c.y, 36, c.h);
            let can_dec = *value - step >= *min;
            let can_inc = *value + step <= *max;
            let ink = |on: bool| if on { p.ink(Color::Label) } else { p.ink(Color::TertiaryLabel) };
            crate::symbol::draw(cv, Symbol::Minus, Rect::new(minus.x + 11, minus.y + 6, 14, 14), ink(can_dec));
            crate::symbol::draw(cv, Symbol::Plus, Rect::new(plus.x + 11, plus.y + 6, 14, 14), ink(can_inc));
            if ring {
                focus_ring(cv, c, 7);
            }
            if enabled {
                let kind = HitKind::Stepper { value: *value, min: *min, max: *max, step: *step, minus, plus };
                rt.hit(control_hit(v, l, cv, p, path, kind));
            }
        }
        Node::Picker { options, selected, style, .. } => match style {
            PickerStyle::Segmented => {
                let r = Rect::new(n.x, n.y + (n.h - 28) / 2, n.w, 28);
                cv.fill_round_rect(r, 8, fade(track(), p.alpha));
                let count = options.len().max(1) as i32;
                let seg_w = r.w / count;
                let k = rt.anim(key, *selected as f32);
                let pill = Rect::new(r.x + 2 + (k * seg_w as f32) as i32, r.y + 2, seg_w - 4, r.h - 4);
                cv.shadow(pill, 6, 3, 1, if t.dark { 90 } else { 40 });
                cv.fill_round_rect(pill, 6, fade(raised(), p.alpha));
                for (i, o) in options.iter().enumerate() {
                    let sr = Rect::new(r.x + i as i32 * seg_w, r.y, seg_w, r.h);
                    if i > 0 && i != *selected && i != selected + 1 {
                        cv.fill_rect(Rect::new(sr.x, sr.y + 7, 1, sr.h - 14), fade(t.separator, p.alpha));
                    }
                    let f = if i == *selected { theme::ui_bold(body.size) } else { body };
                    let c = if i == *selected { p.ink(Color::Label) } else { p.ink(Color::SecondaryLabel) };
                    let w = f.width(o).min(sr.w - 8);
                    cv.text_clipped(sr.x + (sr.w - w) / 2, baseline(f, sr.y, sr.h), o, f, c, sr.w - 8);
                }
                if ring {
                    focus_ring(cv, r, 8);
                }
                if enabled {
                    let kind = HitKind::Segmented { n: options.len(), selected: *selected };
                    let mut h = control_hit(v, l, cv, p, path, kind);
                    h.full = r;
                    rt.hit(h);
                }
            }
            PickerStyle::Menu => {
                let hovered = enabled && rt.hover == Some(key);
                let mut bg = t.control_bg;
                if hovered || rt.menu_owner() == Some(key) {
                    bg = mix(bg, t.text, 16);
                }
                cv.fill_round_rect(n, radius::CONTROL, fade(bg, p.alpha));
                cv.stroke_round_rect(n, radius::CONTROL, fade(t.control_border, p.alpha));
                let s = options.get(*selected).map(|s| s.as_str()).unwrap_or("");
                cv.text_clipped(n.x + 12, baseline(body, n.y, n.h), s, body, p.ink(Color::Label), n.w - 40);
                let ch = Rect::new(n.right() - 24, n.y + (n.h - 14) / 2, 12, 14);
                crate::symbol::draw(
                    cv,
                    Symbol::ChevronUp,
                    Rect::new(ch.x, ch.y - 2, 12, 9),
                    p.ink(Color::SecondaryLabel),
                );
                crate::symbol::draw(
                    cv,
                    Symbol::ChevronDown,
                    Rect::new(ch.x, ch.y + 7, 12, 9),
                    p.ink(Color::SecondaryLabel),
                );
                if ring {
                    focus_ring(cv, n, radius::CONTROL);
                }
                if enabled {
                    rt.hit(control_hit(v, l, cv, p, path, HitKind::PickerMenu));
                }
            }
            PickerStyle::Inline => {
                for (i, o) in options.iter().enumerate() {
                    let row = Rect::new(n.x, n.y + i as i32 * 30, n.w, 30);
                    if i == *selected {
                        crate::symbol::draw(
                            cv,
                            Symbol::Check,
                            Rect::new(row.x + 4, row.y + 8, 14, 14),
                            p.ink(Color::Accent),
                        );
                    }
                    cv.text_clipped(row.x + 28, baseline(body, row.y, row.h), o, body, p.ink(Color::Label), row.w - 28);
                }
                if enabled {
                    rt.hit(control_hit(v, l, cv, p, path, HitKind::PickerInline { n: options.len() }));
                }
            }
        },
        Node::TextField { value, placeholder, secure, search, .. } => {
            let active = focused && rt.active && enabled;
            let f = rt.field(key, value, placeholder, *secure, *search);
            f.draw(cv, n, active);
            if *search {
                crate::symbol::draw(
                    cv,
                    Symbol::Search,
                    Rect::new(n.x + 10, n.y + (n.h - 14) / 2, 14, 14),
                    p.ink(Color::SecondaryLabel),
                );
            }
            if enabled {
                rt.hit(control_hit(v, l, cv, p, path, HitKind::Field));
            }
        }
        Node::TextEditor { value, mono, highlights, current, placeholder, rust, .. } => {
            let font = if *mono { theme::mono(13) } else { body };
            let active = focused && rt.active && enabled;
            let e = rt.editor(key, value, font);
            if e.highlights != *highlights {
                e.highlights = highlights.clone();
            }
            e.current_highlight = *current;
            if e.placeholder != *placeholder {
                e.placeholder = placeholder.clone();
            }
            e.rust_syntax = *rust;
            e.draw(cv, n, active);
            if enabled {
                rt.hit(control_hit(v, l, cv, p, path, HitKind::Editor));
                rt.scrollers.push(Scroller {
                    rect: n.intersect(&cv.clip),
                    key,
                    path: path.clone(),
                    layer: p.layer,
                    kind: ScrollKind::Editor,
                });
            }
        }
        Node::Progress { value, circular } => {
            if *circular {
                let (cx, cy) = n.center();
                let spin = (rt.now / 2 % 1024) as i32;
                cv.stroke_arc(
                    cx * 16,
                    cy * 16,
                    (n.w.min(n.h) / 2 - 2) * 16,
                    2 * 16,
                    spin,
                    700,
                    p.ink(Color::SecondaryLabel),
                );
                rt.animating = true;
            } else {
                cv.fill_round_rect(n, 3, fade(track(), p.alpha));
                match value {
                    Some(k) => {
                        let w = (k.clamp(0.0, 1.0) * n.w as f32) as i32;
                        cv.fill_round_rect(Rect::new(n.x, n.y, w, n.h), 3, fade(theme::accent(), p.alpha));
                    }
                    None => {
                        let w = n.w / 3;
                        let at = (rt.now % 1400) as i32 * (n.w + w) / 1400 - w;
                        cv.with_clip(n, |cv| {
                            cv.fill_round_rect(Rect::new(n.x + at, n.y, w, n.h), 3, fade(theme::accent(), p.alpha))
                        });
                        rt.animating = true;
                    }
                }
            }
        }
        Node::Stack { children, .. } | Node::ZStack { children, .. } | Node::Grid { children, .. } => {
            kids(children, l, cv, p, rt, path)
        }
        Node::Card { children, separators, inset } => {
            let bg = if p.on_accent { with_alpha(WHITE, 0x20) } else { t.window_bg_alt };
            cv.fill_round_rect(n, radius::CARD, fade(bg, p.alpha));
            if t.dark {
                cv.stroke_round_rect(n, radius::CARD, fade(with_alpha(WHITE, 0x10), p.alpha));
            }
            if *separators {
                let mut first = true;
                for (c, cl) in children.iter().zip(&l.kids) {
                    if c.mods.hidden {
                        continue;
                    }
                    if !first {
                        cv.fill_rect(Rect::new(n.x + inset, cl.rect.y - 1, n.w - inset, 1), fade(t.separator, p.alpha));
                    }
                    first = false;
                }
            }
            kids(children, l, cv, p, rt, path)
        }
        Node::Scroll { axis, child, .. } => {
            let Some(cl) = l.kids.first() else { return };
            rt.scrollers.push(Scroller {
                rect: n.intersect(&cv.clip),
                key,
                path: path.clone(),
                layer: p.layer,
                kind: ScrollKind::View(*axis),
            });
            cv.with_clip(n, |cv| {
                path.push(0);
                render(child, cl, cv, p, rt, path);
                path.pop();
            });
            scrollbar(cv, n, key, *axis, rt);
        }
        Node::List { count, row_height, selection: sel, on_activate, .. } => {
            let off = rt.scroll_offset(key);
            rt.scrollers.push(Scroller {
                rect: n.intersect(&cv.clip),
                key,
                path: path.clone(),
                layer: p.layer,
                kind: ScrollKind::List,
            });
            if enabled {
                let kind = HitKind::List {
                    count: *count,
                    row_h: *row_height,
                    top: n.y - off,
                    selection: *sel,
                    activate: on_activate.is_some(),
                };
                rt.hit(control_hit(v, l, cv, p, path, kind));
            }
            let list_focused = focused && rt.active;
            cv.with_clip(n, |cv| {
                for (i, row, rl) in &l.rows {
                    let mut rp = p;
                    if *sel == Some(*i) {
                        let r = Rect::new(rl.rect.x + 6, rl.rect.y + 1, rl.rect.w - 12, rl.rect.h - 2);
                        cv.fill_round_rect(r, 6, fade(selection(list_focused || rt.active), p.alpha));
                        rp.on_accent = rt.active;
                    }
                    path.push(*i as u32);
                    render(row, rl, cv, rp, rt, path);
                    path.pop();
                }
            });
            scrollbar(cv, n, key, Axis::Vertical, rt);
        }
        Node::Split { sidebar, detail, .. } => {
            if let [sl, dl] = &l.kids[..] {
                rt.materials.push(sl.rect);
                cv.fill_rect(sl.rect, with_alpha(t.window_bg_alt, 24));
                cv.fill_rect(Rect::new(sl.rect.right(), n.y, 1, n.h), t.separator);
                path.push(0);
                render(sidebar, sl, cv, p, rt, path);
                path.pop();
                path.push(1);
                render(detail, dl, cv, p, rt, path);
                path.pop();
            }
        }
        Node::Canvas { draw, input, focusable } => {
            cv.with_clip(n, |cv| draw(cv, n));
            if input.is_some() && enabled {
                let mut h = control_hit(v, l, cv, p, path, HitKind::Canvas);
                h.focusable = *focusable;
                rt.hit(h);
                rt.scrollers.push(Scroller {
                    rect: n.intersect(&cv.clip),
                    key,
                    path: path.clone(),
                    layer: p.layer,
                    kind: ScrollKind::Canvas,
                });
                rt.canvas_size(key, path, n);
            }
        }
    }
}

fn scrollbar(cv: &mut Canvas, n: Rect, key: u64, axis: Axis, rt: &mut Rt) {
    let Some(st) = rt.scroll.get(&key) else { return };
    let (content, viewport, pos, last_active) = (st.content, st.viewport, st.pos, st.last_active);
    if content <= viewport || viewport <= 0 {
        return;
    }
    let recent = rt.now.saturating_sub(last_active) < 1200;
    let (px, py) = rt.pointer;
    if !recent && !n.contains(px, py) {
        return;
    }
    if recent {
        rt.wake_at(last_active + 1200);
    }
    let len = (viewport * viewport / content).max(28).min(viewport);
    let at = (crate::runtime::round(pos) as i32) * (viewport - len) / (content - viewport).max(1);
    let t = theme::current();
    let c = with_alpha(t.text, if t.dark { 0x70 } else { 0x55 });
    let thumb = match axis {
        Axis::Vertical => Rect::new(n.right() - 8, n.y + 2 + at, 6, len - 4),
        Axis::Horizontal => Rect::new(n.x + 2 + at, n.bottom() - 8, len - 4, 6),
    };
    cv.fill_round_rect(thumb, 3, c);
}
