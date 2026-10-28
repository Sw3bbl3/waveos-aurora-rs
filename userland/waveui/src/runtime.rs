//! The runtime: runs an [`App`] in a window. Each frame it asks the app
//! for its views, lays them out and draws them, remembering what can be
//! clicked, focused and scrolled. Input is routed to those places and
//! turned into the app's messages, which go to `update`; then the next
//! frame shows the result.
//!
//! State that belongs to the interface rather than the app — scroll
//! positions, the caret of the field being edited, which control has
//! focus, animations in flight — lives here, keyed by each view's place
//! in the tree (or its `.id()`).

use crate::layout::{key_of, measure, place, Inherit, Lay};
use crate::render::{fade, render, Paint};
use crate::style::{Color, TextStyle};
use crate::view::{find, CanvasEvent, DragItem, DropInfo, Key, MenuItem, Node, Shortcut, View};
use crate::{App, Context, Env};
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use aurorakit::text::{TextField, TextView};
use aurorakit::{KeyCode, KeyEvent, Request};
use lumen::canvas::{with_alpha, Canvas};
use lumen::font::Font;
use lumen::geom::Rect;
use lumen::theme;

const ROOT: [u64; 2] = [0x5EED_0000, 0x5EED_0001];
const SHEET_ANIM: u64 = 0x5EED_00A0;
/// One wheel notch, in pixels.
const NOTCH_PX: i32 = 48;
const BLINK_MS: u64 = 530;

pub(crate) fn round(x: f32) -> f32 {
    if x >= 0.0 {
        (x + 0.5) as i64 as f32
    } else {
        (x - 0.5) as i64 as f32
    }
}

pub(crate) fn abs(x: f32) -> f32 {
    if x < 0.0 {
        -x
    } else {
        x
    }
}

/// A scrolling region's position.
pub(crate) struct ScrollState {
    pub pos: f32,
    pub target: f32,
    pub content: i32,
    pub viewport: i32,
    pub last_active: u64,
    fresh: bool,
    at_end: bool,
}

impl Default for ScrollState {
    fn default() -> Self {
        ScrollState { pos: 0.0, target: 0.0, content: 0, viewport: 0, last_active: 0, fresh: true, at_end: true }
    }
}

impl ScrollState {
    fn max(&self) -> f32 {
        (self.content - self.viewport).max(0) as f32
    }

    /// Records the sizes and returns the offset to draw at.
    pub fn layout(&mut self, content: i32, viewport: i32, stick_to_end: bool) -> i32 {
        self.content = content;
        self.viewport = viewport;
        let max = self.max();
        if stick_to_end && (self.fresh || self.at_end) {
            self.pos = max;
            self.target = max;
        }
        self.fresh = false;
        self.target = self.target.clamp(0.0, max);
        self.pos = self.pos.clamp(0.0, max);
        self.at_end = self.target >= max - 1.0;
        round(self.pos) as i32
    }

    pub fn by(&mut self, d: f32, smooth: bool, now: u64) {
        self.target = (self.target + d).clamp(0.0, self.max());
        if !smooth || theme::reduce_motion() {
            self.pos = self.target;
        }
        self.last_active = now;
    }

    /// Jumps to `y` (clamped at the next layout).
    pub fn to(&mut self, y: f32) {
        self.target = y.max(0.0);
        self.pos = self.target;
    }

    /// One animation step; true while still moving.
    fn step(&mut self) -> bool {
        let d = self.target - self.pos;
        if abs(d) < 0.5 {
            self.pos = self.target;
            false
        } else {
            self.pos += d * 0.35;
            true
        }
    }
}

struct Anim {
    from: f32,
    to: f32,
    start: u64,
}

const ANIM_MS: u64 = 180;

impl Anim {
    fn value(&self, now: u64) -> (f32, bool) {
        let t = now.saturating_sub(self.start);
        if t >= ANIM_MS {
            return (self.to, false);
        }
        // Ease out (cubic).
        let k = 1.0 - t as f32 / ANIM_MS as f32;
        let e = 1.0 - k * k * k;
        (self.from + (self.to - self.from) * e, true)
    }
}

#[derive(Clone, Debug)]
pub(crate) enum HitKind {
    /// A view with modifiers (`on_tap`, `context_menu`, `help`, …).
    Tap,
    Button {
        menu: bool,
    },
    Toggle(bool),
    Slider {
        min: f32,
        max: f32,
        step: Option<f32>,
        value: f32,
        track: Rect,
    },
    Stepper {
        value: i64,
        min: i64,
        max: i64,
        step: i64,
        minus: Rect,
        plus: Rect,
    },
    Segmented {
        n: usize,
        selected: usize,
    },
    PickerMenu,
    PickerInline {
        n: usize,
    },
    Field,
    Editor,
    List {
        count: usize,
        row_h: i32,
        top: i32,
        selection: Option<usize>,
        activate: bool,
    },
    Canvas,
}

/// Something on screen that reacts to the pointer or the keyboard.
#[derive(Clone, Debug)]
pub(crate) struct Hit {
    /// The visible part.
    pub rect: Rect,
    /// The whole view.
    pub full: Rect,
    pub key: u64,
    /// Where it is in the tree (first element: the layer).
    pub path: Vec<u32>,
    pub layer: u8,
    pub kind: HitKind,
    pub focusable: bool,
    pub shortcut: Option<Shortcut>,
    pub tap: bool,
    pub double: bool,
    pub menu: bool,
    pub hover: bool,
    pub help: Option<String>,
    pub drop: bool,
    pub drag: bool,
    /// Takes focus when it first appears.
    pub appear: bool,
}

impl Hit {
    fn clickable(&self) -> bool {
        !matches!(self.kind, HitKind::Tap) || self.tap || self.double || self.drag
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScrollKind {
    View(crate::view::Axis),
    List,
    Editor,
    Canvas,
}

pub(crate) struct Scroller {
    pub rect: Rect,
    pub key: u64,
    pub path: Vec<u32>,
    pub layer: u8,
    pub kind: ScrollKind,
}

struct MenuEntry {
    label: String,
    enabled: bool,
    destructive: bool,
    checked: bool,
    shortcut: Option<String>,
    separator: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuSource {
    Context,
    Button,
    Picker,
}

struct Menu {
    entries: Vec<MenuEntry>,
    rect: Rect,
    hovered: Option<usize>,
    source: MenuSource,
    path: Vec<u32>,
    owner: u64,
}

const MENU_ROW: i32 = 26;
const MENU_SEP: i32 = 9;
const MENU_PAD: i32 = 5;

impl Menu {
    fn row_rect(&self, i: usize) -> Rect {
        let mut y = self.rect.y + MENU_PAD;
        for e in &self.entries[..i] {
            y += if e.separator { MENU_SEP } else { MENU_ROW };
        }
        let h = if self.entries[i].separator { MENU_SEP } else { MENU_ROW };
        Rect::new(self.rect.x + MENU_PAD, y, self.rect.w - 2 * MENU_PAD, h)
    }

    fn at(&self, x: i32, y: i32) -> Option<usize> {
        (0..self.entries.len()).find(|&i| {
            let e = &self.entries[i];
            !e.separator && e.enabled && self.row_rect(i).contains(x, y)
        })
    }

    fn step(&mut self, down: bool) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let mut i = self.hovered.unwrap_or(if down { n - 1 } else { 0 });
        for _ in 0..n {
            i = if down { (i + 1) % n } else { (i + n - 1) % n };
            if !self.entries[i].separator && self.entries[i].enabled {
                self.hovered = Some(i);
                return;
            }
        }
    }
}

/// Interface state that outlives a frame.
pub(crate) struct Rt {
    pub now: u64,
    /// The window is the active one.
    pub active: bool,
    pub scroll: BTreeMap<u64, ScrollState>,
    fields: BTreeMap<u64, TextField>,
    editors: BTreeMap<u64, TextView>,
    anims: BTreeMap<u64, Anim>,
    canvas_sizes: BTreeMap<u64, (i32, i32)>,
    /// Where each editor's caret was last reported.
    carets: BTreeMap<u64, (usize, usize)>,
    pending_resize: Vec<(Vec<u32>, i32, i32)>,
    pub seen: BTreeSet<u64>,
    appeared: BTreeSet<u64>,
    pub hits: Vec<Hit>,
    pub scrollers: Vec<Scroller>,
    pub materials: Vec<Rect>,
    pub focus: Option<u64>,
    /// Focus came from the keyboard: draw a ring.
    pub focus_visible: bool,
    pub hover: Option<u64>,
    pub pointer: (i32, i32),
    pub pressed: Option<u64>,
    pub drop_target: Option<u64>,
    /// Scroll the focused control into view after the next frame.
    pub reveal: bool,
    menu: Option<Menu>,
    /// Something is moving: draw again soon.
    pub animating: bool,
    /// The earliest time something wants a frame.
    pub wake: u64,
    top_layer: u8,
}

impl Rt {
    fn new() -> Self {
        Rt {
            now: 0,
            active: true,
            scroll: BTreeMap::new(),
            fields: BTreeMap::new(),
            editors: BTreeMap::new(),
            anims: BTreeMap::new(),
            canvas_sizes: BTreeMap::new(),
            carets: BTreeMap::new(),
            pending_resize: Vec::new(),
            seen: BTreeSet::new(),
            appeared: BTreeSet::new(),
            hits: Vec::new(),
            scrollers: Vec::new(),
            materials: Vec::new(),
            focus: None,
            focus_visible: false,
            hover: None,
            pointer: (-1, -1),
            pressed: None,
            drop_target: None,
            reveal: false,
            menu: None,
            animating: false,
            wake: u64::MAX,
            top_layer: 0,
        }
    }

    pub fn hit(&mut self, h: Hit) {
        self.hits.push(h);
    }

    pub fn wake_at(&mut self, t: u64) {
        self.wake = self.wake.min(t);
    }

    pub fn menu_owner(&self) -> Option<u64> {
        self.menu.as_ref().map(|m| m.owner)
    }

    pub fn scroll_offset(&self, key: u64) -> i32 {
        self.scroll.get(&key).map(|s| round(s.pos) as i32).unwrap_or(0)
    }

    /// An animated value heading for `target`.
    pub fn anim(&mut self, key: u64, target: f32) -> f32 {
        let now = self.now;
        let a = self.anims.entry(key).or_insert(Anim { from: target, to: target, start: 0 });
        if a.to != target {
            let (cur, _) = a.value(now);
            *a = Anim { from: cur, to: target, start: now };
        }
        if theme::reduce_motion() {
            return target;
        }
        let (v, moving) = a.value(now);
        self.animating |= moving;
        v
    }

    fn anim_in(&mut self, key: u64) -> f32 {
        let now = self.now;
        self.anims.entry(key).or_insert(Anim { from: 0.0, to: 1.0, start: now });
        self.anim(key, 1.0)
    }

    pub fn field(&mut self, key: u64, value: &str, placeholder: &str, secure: bool, search: bool) -> &mut TextField {
        let f = self.fields.entry(key).or_insert_with(|| TextField::new(value, placeholder));
        if f.text() != value {
            f.set_text(value);
        }
        if f.placeholder != placeholder {
            f.placeholder = placeholder.to_string();
        }
        f.secure = secure;
        (f.inset, f.radius) = if search { (32, 17) } else { (12, crate::style::radius::CONTROL) };
        f
    }

    pub fn editor(&mut self, key: u64, value: &str, font: Font) -> &mut TextView {
        let e = self.editors.entry(key).or_insert_with(|| TextView::new(value, font));
        if e.edit.text() != value {
            e.edit.set_text(value);
        }
        if e.font != font {
            e.set_font(font);
        }
        e
    }

    pub fn canvas_size(&mut self, key: u64, path: &[u32], r: Rect) {
        if self.canvas_sizes.get(&key) != Some(&(r.w, r.h)) {
            self.canvas_sizes.insert(key, (r.w, r.h));
            self.pending_resize.push((path.to_vec(), r.w, r.h));
        }
    }

    fn begin(&mut self, now: u64, active: bool) {
        self.now = now;
        self.active = active;
        self.hits.clear();
        self.scrollers.clear();
        self.materials.clear();
        self.seen.clear();
        self.animating = false;
        self.wake = u64::MAX;
    }

    fn end(&mut self) {
        let seen = &self.seen;
        self.fields.retain(|k, _| seen.contains(k));
        self.editors.retain(|k, _| seen.contains(k));
        self.anims.retain(|k, _| seen.contains(k));
        self.scroll.retain(|k, _| seen.contains(k));
        self.canvas_sizes.retain(|k, _| seen.contains(k));
        self.carets.retain(|k, _| seen.contains(k));
        self.appeared.retain(|k| seen.contains(k));
        for s in self.scroll.values_mut() {
            self.animating |= s.step();
        }
        let top = self.top_layer;
        if let Some(h) = self.hits.iter().find(|h| h.appear && h.layer == top && !self.appeared.contains(&h.key)) {
            self.focus = Some(h.key);
            self.focus_visible = false;
        }
        for h in &self.hits {
            if h.appear {
                self.appeared.insert(h.key);
            }
        }
        if let Some(f) = self.focus {
            if !self.hits.iter().any(|h| h.key == f && h.focusable && h.layer == top) {
                self.focus = None;
            }
        }
        if core::mem::take(&mut self.reveal) {
            self.reveal_focus();
        }
    }

    /// Scrolls the scroll views around the focused control so it shows.
    fn reveal_focus(&mut self) {
        let Some(h) = self.focused_hit() else { return };
        for s in self.scrollers.iter().rev() {
            if s.key == h.key || !h.path.starts_with(&s.path) || s.layer != h.layer {
                continue;
            }
            let Some(st) = self.scroll.get_mut(&s.key) else { continue };
            let view = s.rect;
            let margin = 16;
            let d = if h.full.bottom() + margin > view.bottom() {
                (h.full.bottom() + margin - view.bottom()).min(h.full.y - margin - view.y)
            } else if h.full.y - margin < view.y {
                h.full.y - margin - view.y
            } else {
                0
            };
            if d != 0 {
                st.by(d as f32, true, self.now);
                self.animating = true;
            }
        }
    }

    fn top_hit(&self, x: i32, y: i32, pred: impl Fn(&Hit) -> bool) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.layer == self.top_layer && h.rect.contains(x, y) && pred(h))
    }

    fn hit_by_key(&self, key: u64) -> Option<&Hit> {
        self.hits.iter().rev().find(|h| h.key == key && !matches!(h.kind, HitKind::Tap) || h.key == key && h.tap)
    }

    fn focused_hit(&self) -> Option<Hit> {
        let f = self.focus?;
        self.hits.iter().rev().find(|h| h.key == f && h.focusable).cloned()
    }
}

struct Press {
    key: u64,
    x: i32,
    y: i32,
    hit: Hit,
    dragged: bool,
}

/// Runs an [`App`] on AuroraKit's window loop.
pub(crate) struct Host<A: App> {
    app: A,
    rt: Rt,
    env: Env,
    timers: Vec<(u64, A::Message)>,
    queue: VecDeque<A::Message>,
    requests: Vec<Request>,
    next_poll: u64,
    press: Option<Press>,
    hover_view: Option<Hit>,
    canvas_hover: Option<Hit>,
    tooltip: Option<(u64, u64, String)>,
    materials: Vec<Rect>,
    redraw: bool,
    started: bool,
}

impl<A: App> Host<A> {
    pub fn new(app: A) -> Self {
        Host {
            app,
            rt: Rt::new(),
            env: Env::default(),
            timers: Vec::new(),
            queue: VecDeque::new(),
            requests: Vec::new(),
            next_poll: 0,
            press: None,
            hover_view: None,
            canvas_hover: None,
            tooltip: None,
            materials: Vec::new(),
            redraw: true,
            started: false,
        }
    }

    fn cx(&mut self) -> (&mut A, Context<'_, A::Message>) {
        let now = corekit::time::uptime_ms();
        (
            &mut self.app,
            Context {
                rt: &mut self.rt,
                requests: &mut self.requests,
                timers: &mut self.timers,
                queue: &mut self.queue,
                now,
                mods: self.env.mods,
            },
        )
    }

    /// Sends a message (and any it leads to) to the app.
    fn send(&mut self, m: Option<A::Message>) -> bool {
        let Some(m) = m else { return false };
        self.queue.push_back(m);
        while let Some(m) = self.queue.pop_front() {
            let (app, mut cx) = self.cx();
            app.update(m, &mut cx);
        }
        self.redraw = true;
        true
    }

    fn flush(&mut self, env: &mut aurorakit::Env) {
        env.requests.append(&mut self.requests);
    }

    /// Looks up the view at `path` in a freshly built tree.
    fn with<R>(&self, path: &[u32], f: impl FnOnce(&View<'_, A::Message>) -> Option<R>) -> Option<R> {
        let (&layer, rest) = path.split_first()?;
        let root = match layer {
            0 => self.app.view(&self.env),
            _ => self.app.sheet(&self.env)?,
        };
        let mut f = Some(f);
        find(&root, rest, &mut |v| f.take().and_then(|f| f(v))).flatten()
    }

    fn message(&self, h: &Hit, f: impl FnOnce(&View<'_, A::Message>) -> Option<A::Message>) -> Option<A::Message> {
        self.with(&h.path, f)
    }

    // ----------------------------------------------------------- actions

    /// Clicks a button, a toggle or a tappable view (or opens its menu).
    fn activate(&mut self, h: &Hit) -> bool {
        match h.kind {
            HitKind::Button { menu: true } => self.open_menu(h, MenuSource::Button),
            HitKind::PickerMenu => self.open_menu(h, MenuSource::Picker),
            HitKind::Button { .. } => {
                let m = self.message(h, |v| match &v.node {
                    Node::Button { action, .. } => action.clone(),
                    _ => None,
                });
                self.send(m)
            }
            HitKind::Toggle(on) => {
                let m = self.message(h, |v| match &v.node {
                    Node::Toggle { action, .. } | Node::Checkbox { action, .. } => action.as_ref().map(|f| f(!on)),
                    _ => None,
                });
                self.send(m)
            }
            HitKind::Tap => {
                let m = self.message(h, |v| v.mods.on_tap.clone());
                self.send(m)
            }
            // A shortcut on a field, a list or a canvas focuses it.
            _ if h.focusable => {
                self.focus(Some(h.key), true);
                self.rt.reveal = true;
                if let Some(f) = self.rt.fields.get_mut(&h.key) {
                    f.edit.select_all();
                }
                true
            }
            _ => false,
        }
    }

    fn choose(&mut self, h: &Hit, i: usize) -> bool {
        let m = self.message(h, |v| match &v.node {
            Node::Picker { action, .. } => action.as_ref().map(|f| f(i)),
            Node::List { on_select, .. } => on_select.as_ref().map(|f| f(i)),
            _ => None,
        });
        self.send(m)
    }

    fn list_activate(&mut self, h: &Hit, i: usize) -> bool {
        let m = self.message(h, |v| match &v.node {
            Node::List { on_activate, .. } => on_activate.as_ref().map(|f| f(i)),
            _ => None,
        });
        self.send(m)
    }

    fn set_slider(&mut self, h: &Hit, x: i32) -> bool {
        let HitKind::Slider { min, max, step, value, track } = h.kind else { return false };
        let k = ((x - track.x) as f32 / track.w.max(1) as f32).clamp(0.0, 1.0);
        let mut v = min + k * (max - min);
        if let Some(s) = step {
            if s > 0.0 {
                v = round(v / s) * s;
            }
        }
        let v = v.clamp(min.min(max), max.max(min));
        if v == value {
            return false;
        }
        if let Some(hh) = self.rt.hits.iter_mut().find(|x| x.key == h.key) {
            if let HitKind::Slider { value, .. } = &mut hh.kind {
                *value = v;
            }
        }
        let m = self.message(h, |vw| match &vw.node {
            Node::Slider { action, .. } => action.as_ref().map(|f| f(v)),
            _ => None,
        });
        self.send(m)
    }

    fn step_value(&mut self, h: &Hit, up: bool) -> bool {
        let HitKind::Stepper { value, min, max, step, .. } = h.kind else { return false };
        let v = if up { value + step } else { value - step };
        if v < min || v > max {
            return false;
        }
        let m = self.message(h, |vw| match &vw.node {
            Node::Stepper { action, .. } => action.as_ref().map(|f| f(v)),
            _ => None,
        });
        self.send(m)
    }

    fn canvas_event(&mut self, h: &Hit, ev: CanvasEvent) -> bool {
        let m = self.message(h, |v| match &v.node {
            Node::Canvas { input: Some(f), .. } => f(ev),
            _ => None,
        });
        self.send(m)
    }

    fn text_changed(&mut self, h: &Hit) -> bool {
        let text = match h.kind {
            HitKind::Field => self.rt.fields.get(&h.key).map(|f| f.text().to_string()),
            HitKind::Editor => self.rt.editors.get(&h.key).map(|e| e.edit.text().to_string()),
            _ => None,
        };
        let Some(text) = text else { return false };
        let m = self.message(h, |v| match &v.node {
            Node::TextField { action, .. } | Node::TextEditor { action, .. } => action.as_ref().map(|f| f(text)),
            _ => None,
        });
        self.send(m);
        true
    }

    /// Tells an editor's `on_caret` where its caret went, if it moved.
    fn caret_moved(&mut self, h: &Hit) {
        let Some(at) = self.rt.editors.get(&h.key).map(|e| e.caret_line_col()) else { return };
        if self.rt.carets.insert(h.key, at) == Some(at) {
            return;
        }
        let m = self.message(h, |v| match &v.node {
            Node::TextEditor { caret: Some(f), .. } => Some(f(at)),
            _ => None,
        });
        self.send(m);
    }

    fn submit(&mut self, h: &Hit) -> Option<bool> {
        let m = self.message(h, |v| match &v.node {
            Node::TextField { submit, .. } => submit.clone(),
            _ => None,
        })?;
        Some(self.send(Some(m)))
    }

    fn focus(&mut self, key: Option<u64>, visible: bool) {
        self.rt.focus = key;
        self.rt.focus_visible = visible;
        self.redraw = true;
    }

    /// Moves focus to the next (or previous) control.
    fn traverse(&mut self, back: bool) -> bool {
        let top = self.rt.top_layer;
        let mut keys: Vec<u64> = Vec::new();
        for h in self.rt.hits.iter().filter(|h| h.focusable && h.layer == top) {
            if !keys.contains(&h.key) {
                keys.push(h.key);
            }
        }
        if keys.is_empty() {
            return false;
        }
        let at = self.rt.focus.and_then(|f| keys.iter().position(|&k| k == f));
        let n = keys.len();
        let next = match (at, back) {
            (None, false) => 0,
            (None, true) => n - 1,
            (Some(i), false) => (i + 1) % n,
            (Some(i), true) => (i + n - 1) % n,
        };
        self.focus(Some(keys[next]), true);
        self.rt.reveal = true;
        if let Some(f) = self.rt.fields.get_mut(&keys[next]) {
            f.edit.select_all();
        }
        true
    }

    // ------------------------------------------------------------- menus

    fn open_menu(&mut self, h: &Hit, source: MenuSource) -> bool {
        let entries = self.with(&h.path, |v| {
            let items: &[MenuItem<A::Message>] = match (source, &v.node) {
                (MenuSource::Context, _) => &v.mods.context_menu,
                (MenuSource::Button, Node::Button { menu, .. }) => menu,
                (MenuSource::Picker, Node::Picker { options, selected, .. }) => {
                    return Some(
                        options
                            .iter()
                            .enumerate()
                            .map(|(i, o)| MenuEntry {
                                label: o.clone(),
                                enabled: true,
                                destructive: false,
                                checked: i == *selected,
                                shortcut: None,
                                separator: false,
                            })
                            .collect::<Vec<_>>(),
                    )
                }
                _ => return None,
            };
            Some(
                items
                    .iter()
                    .map(|it| match it {
                        MenuItem::Action { label, enabled, destructive, shortcut, .. } => MenuEntry {
                            label: label.clone(),
                            enabled: *enabled,
                            destructive: *destructive,
                            checked: false,
                            shortcut: shortcut.map(shortcut_label),
                            separator: false,
                        },
                        MenuItem::Separator => MenuEntry {
                            label: String::new(),
                            enabled: false,
                            destructive: false,
                            checked: false,
                            shortcut: None,
                            separator: true,
                        },
                    })
                    .collect(),
            )
        });
        let Some(entries) = entries else { return false };
        if entries.is_empty() {
            return false;
        }
        let f = theme::ui(13);
        let fs = theme::ui(12);
        let check = entries.iter().any(|e| e.checked) as i32 * 18;
        let w = entries
            .iter()
            .map(|e| f.width(&e.label) + e.shortcut.as_ref().map(|s| fs.width(s) + 28).unwrap_or(0))
            .max()
            .unwrap_or(0)
            + 36
            + check;
        let h_total: i32 =
            entries.iter().map(|e| if e.separator { MENU_SEP } else { MENU_ROW }).sum::<i32>() + 2 * MENU_PAD;
        let w = w.max(170).max(if source == MenuSource::Picker { h.full.w } else { 0 });
        let (ax, ay) = match source {
            MenuSource::Context => self.rt.pointer,
            _ => (h.full.x, h.full.bottom() + 4),
        };
        let area = Rect::new(0, 0, self.env.size.0, self.env.size.1);
        let mut x = ax.min(area.right() - w - 4).max(4);
        let mut y = ay;
        if y + h_total > area.bottom() - 4 {
            y = match source {
                MenuSource::Context => ay - h_total,
                _ => h.full.y - 4 - h_total,
            };
        }
        y = y.max(4);
        if w > area.w - 8 {
            x = 4;
        }
        self.rt.menu = Some(Menu {
            entries,
            rect: Rect::new(x, y, w.min(area.w - 8), h_total),
            hovered: None,
            source,
            path: h.path.clone(),
            owner: h.key,
        });
        self.rt.pressed = None;
        self.redraw = true;
        true
    }

    fn pick_menu(&mut self, i: usize) -> bool {
        let Some(menu) = self.rt.menu.take() else { return false };
        let source = menu.source;
        let m = self.with(&menu.path, |v| match source {
            MenuSource::Context => match v.mods.context_menu.get(i) {
                Some(MenuItem::Action { message, enabled: true, .. }) => Some(message.clone()),
                _ => None,
            },
            MenuSource::Button => match &v.node {
                Node::Button { menu, .. } => match menu.get(i) {
                    Some(MenuItem::Action { message, enabled: true, .. }) => Some(message.clone()),
                    _ => None,
                },
                _ => None,
            },
            MenuSource::Picker => match &v.node {
                Node::Picker { action, .. } => action.as_ref().map(|f| f(i)),
                _ => None,
            },
        });
        self.redraw = true;
        self.send(m);
        true
    }

    fn draw_menu(&self, cv: &mut Canvas) {
        let Some(menu) = &self.rt.menu else { return };
        let t = theme::current();
        let r = menu.rect;
        cv.shadow(r, 10, 22, 8, if t.dark { 170 } else { 80 });
        let bg = if t.dark { lumen::canvas::mix(t.window_bg, 0xFFFF_FFFF, 30) } else { 0xFFFF_FFFF };
        cv.fill_round_rect(r, 10, bg);
        cv.stroke_round_rect(r, 10, with_alpha(t.text, if t.dark { 0x30 } else { 0x1C }));
        let f = theme::ui(13);
        let fs = theme::ui(12);
        let check = menu.entries.iter().any(|e| e.checked) as i32 * 18;
        for (i, e) in menu.entries.iter().enumerate() {
            let row = menu.row_rect(i);
            if e.separator {
                cv.fill_rect(Rect::new(row.x + 6, row.y + MENU_SEP / 2, row.w - 12, 1), t.separator);
                continue;
            }
            let hot = menu.hovered == Some(i);
            if hot {
                cv.fill_round_rect(row, 5, theme::accent());
            }
            let c = if hot {
                0xFFFF_FFFF
            } else if !e.enabled {
                Color::TertiaryLabel.argb()
            } else if e.destructive {
                Color::Red.argb()
            } else {
                t.text
            };
            if e.checked {
                crate::symbol::draw(cv, crate::view::Symbol::Check, Rect::new(row.x + 6, row.y + 7, 12, 12), c);
            }
            cv.text_clipped(
                row.x + 10 + check,
                crate::layout::baseline(f, row.y, row.h),
                &e.label,
                f,
                c,
                row.w - 20 - check,
            );
            if let Some(s) = &e.shortcut {
                let sc = if hot { with_alpha(0xFFFF_FFFF, 0xC0) } else { t.text_secondary };
                cv.text(row.right() - 10 - fs.width(s), crate::layout::baseline(fs, row.y, row.h), s, fs, sc);
            }
        }
    }

    fn draw_tooltip(&mut self, cv: &mut Canvas) {
        let Some((key, since, text)) = &self.tooltip else { return };
        if self.rt.hover != Some(*key) || self.press.is_some() || self.rt.menu.is_some() {
            return;
        }
        let at = since + 700;
        if self.rt.now < at {
            self.rt.wake_at(at);
            return;
        }
        let t = theme::current();
        let f = theme::ui(12);
        let w = f.width(text) + 16;
        let (px, py) = self.rt.pointer;
        let area = Rect::new(0, 0, self.env.size.0, self.env.size.1);
        let mut r = Rect::new(px + 4, py + 22, w, 24);
        r.x = r.x.min(area.right() - w - 4).max(4);
        if r.bottom() > area.bottom() - 4 {
            r.y = py - 30;
        }
        cv.shadow(r, 6, 10, 3, if t.dark { 140 } else { 60 });
        let bg = if t.dark { lumen::canvas::mix(t.window_bg, 0xFFFF_FFFF, 36) } else { 0xFFFF_FFFF };
        cv.fill_round_rect(r, 6, bg);
        cv.stroke_round_rect(r, 6, with_alpha(t.text, 0x20));
        cv.text(r.x + 8, crate::layout::baseline(f, r.y, r.h), text, f, t.text);
    }

    // --------------------------------------------------------- keyboard

    fn key_event(&mut self, ev: &KeyEvent) -> bool {
        let now = corekit::time::uptime_ms();
        let focused = self.rt.focused_hit();
        if !ev.pressed {
            if let Some(h) = focused.filter(|h| matches!(h.kind, HitKind::Canvas)) {
                return self.canvas_event(&h, CanvasEvent::Key(*ev));
            }
            return false;
        }
        self.tooltip = None;
        // An open menu takes the keyboard.
        if let Some(menu) = &mut self.rt.menu {
            match ev.code {
                KeyCode::Escape => self.rt.menu = None,
                KeyCode::Up => menu.step(false),
                KeyCode::Down => menu.step(true),
                KeyCode::Enter | KeyCode::Char if ev.code == KeyCode::Enter || ev.ch == Some(' ') => {
                    if let Some(i) = menu.hovered {
                        return self.pick_menu(i);
                    }
                    self.rt.menu = None;
                }
                _ => {}
            }
            self.redraw = true;
            return true;
        }
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        // Text first: editing keys belong to the field being edited.
        if let Some(h) = &focused {
            match h.kind {
                HitKind::Field if !matches!(ev.code, KeyCode::Tab | KeyCode::Enter | KeyCode::Escape) => {
                    if let Some(f) = self.rt.fields.get_mut(&h.key) {
                        let r = f.key(ev, now);
                        if r.edited {
                            self.text_changed(h);
                        }
                        if r.handled {
                            self.redraw = true;
                            return true;
                        }
                    }
                }
                HitKind::Field if ev.code == KeyCode::Enter => {
                    if let Some(done) = self.submit(h) {
                        return done;
                    }
                }
                HitKind::Editor if ev.code != KeyCode::Escape && !(ev.code == KeyCode::Tab && ctrl) => {
                    if let Some(e) = self.rt.editors.get_mut(&h.key) {
                        let r = e.key(ev, now);
                        if r.edited {
                            self.text_changed(h);
                        }
                        if r.handled {
                            self.caret_moved(h);
                            self.redraw = true;
                            return true;
                        }
                    }
                }
                HitKind::Canvas
                    if !(ctrl && self.shortcut_hit(ev).is_some())
                        && !(ev.code == KeyCode::Tab && ctrl)
                        && self.canvas_event(h, CanvasEvent::Key(*ev)) =>
                {
                    return true;
                }
                _ => {}
            }
        }
        if let Some(h) = self.shortcut_hit(ev) {
            return self.activate(&h);
        }
        if ev.code == KeyCode::Tab {
            return self.traverse(ev.mods.shift);
        }
        if let Some(h) = &focused {
            let space = ev.ch == Some(' ');
            let handled = match h.kind {
                HitKind::Button { .. } | HitKind::Toggle(_) | HitKind::Tap if space => self.activate(h),
                HitKind::PickerMenu if space || ev.code == KeyCode::Down => self.activate(h),
                HitKind::Slider { min, max, step, value, track } => {
                    let d = step.unwrap_or((max - min) / 20.0);
                    let v = match ev.code {
                        KeyCode::Left | KeyCode::Down => value - d,
                        KeyCode::Right | KeyCode::Up => value + d,
                        _ => return self.fallback(ev),
                    };
                    let x = track.x + ((v - min) / (max - min) * track.w as f32) as i32;
                    self.set_slider(h, x)
                }
                HitKind::Stepper { .. } => match ev.code {
                    KeyCode::Up | KeyCode::Right => self.step_value(h, true),
                    KeyCode::Down | KeyCode::Left => self.step_value(h, false),
                    _ => return self.fallback(ev),
                },
                HitKind::Segmented { n, selected } => match ev.code {
                    KeyCode::Left if selected > 0 => self.choose(h, selected - 1),
                    KeyCode::Right if selected + 1 < n => self.choose(h, selected + 1),
                    _ => return self.fallback(ev),
                },
                HitKind::List { count, selection, activate, row_h, .. } if count > 0 => {
                    let page = (h.full.h / row_h.max(1)).max(1) as usize;
                    let sel = selection.unwrap_or(0);
                    let to = match ev.code {
                        KeyCode::Up if selection.is_some() => sel.saturating_sub(1),
                        KeyCode::Up => count - 1,
                        KeyCode::Down if selection.is_some() => (sel + 1).min(count - 1),
                        KeyCode::Down => 0,
                        KeyCode::Home => 0,
                        KeyCode::End => count - 1,
                        KeyCode::PageUp => sel.saturating_sub(page),
                        KeyCode::PageDown => (sel + page).min(count - 1),
                        KeyCode::Enter if activate && selection.is_some() => return self.list_activate(h, sel),
                        _ => return self.fallback(ev),
                    };
                    if let Some(st) = self.rt.scroll.get_mut(&h.key) {
                        let top = to as f32 * row_h as f32;
                        if top < st.target {
                            st.to(top);
                        } else if top + row_h as f32 > st.target + st.viewport as f32 {
                            st.to(top + row_h as f32 - st.viewport as f32);
                        }
                    }
                    self.choose(h, to)
                }
                _ => false,
            };
            if handled {
                return true;
            }
        }
        if self.fallback(ev) {
            return true;
        }
        if ev.code == KeyCode::Escape
            && focused.as_ref().is_some_and(|h| matches!(h.kind, HitKind::Field | HitKind::Editor))
        {
            self.focus(None, false);
            return true;
        }
        false
    }

    fn fallback(&mut self, ev: &KeyEvent) -> bool {
        let m = self.app.key(ev);
        self.send(m)
    }

    fn shortcut_hit(&self, ev: &KeyEvent) -> Option<Hit> {
        let top = self.rt.top_layer;
        self.rt.hits.iter().find(|h| h.layer == top && h.shortcut.is_some_and(|s| matches(s, ev))).cloned()
    }

    // ------------------------------------------------------------- frame

    fn frame(&mut self, cv: &mut Canvas, area: Rect) {
        let t = theme::current();
        let sheet_open = self.app.sheet(&self.env).is_some();
        self.rt.top_layer = sheet_open as u8;
        {
            let view = self.app.view(&self.env);
            let laid = {
                let mut lay = Lay::new(&mut self.rt.scroll);
                place(&view, area, ROOT[0], Inherit { style: TextStyle::Body }, &mut lay)
            };
            render(&view, &laid, cv, Paint::root(0), &mut self.rt, &mut alloc::vec![0]);
        }
        if let Some(sheet) = self.app.sheet(&self.env) {
            let k = self.rt.anim_in(SHEET_ANIM);
            self.rt.seen.insert(SHEET_ANIM);
            cv.fill_rect(area, with_alpha(0, ((if t.dark { 0x70 } else { 0x40 }) as f32 * k) as u8));
            let pad = 22;
            let mut lay = Lay::new(&mut self.rt.scroll);
            let inh = Inherit { style: TextStyle::Body };
            let (w, h) =
                measure(&sheet, (area.w - 64 - 2 * pad).min(640), (area.h - 64 - 2 * pad).max(40), inh, &mut lay);
            let (w, h) = (w + 2 * pad, (h + 2 * pad).min(area.h - 24));
            let y = area.y + 32.min((area.h - h) / 2).max(12) - ((1.0 - k) * 18.0) as i32;
            let card = Rect::new(area.x + (area.w - w) / 2, y, w, h);
            let inner = card.inset(pad);
            let laid = place(&sheet, inner, ROOT[1], inh, &mut lay);
            drop(lay);
            let alpha = (255.0 * k) as u8;
            cv.shadow(card, 16, 30, 10, ((if t.dark { 200 } else { 110 }) as f32 * k) as u32);
            let bg = if t.dark { lumen::canvas::mix(t.window_bg, 0xFFFF_FFFF, 14) } else { t.window_bg };
            cv.fill_round_rect(card, 16, fade(bg, alpha));
            if t.dark {
                cv.stroke_round_rect(card, 16, with_alpha(0xFFFF_FFFF, (0x18 as f32 * k) as u8));
            }
            let mut p = Paint::root(1);
            p.alpha = alpha;
            render(&sheet, &laid, cv, p, &mut self.rt, &mut alloc::vec![1]);
        }
        self.draw_menu(cv);
        self.draw_tooltip(cv);
    }

    fn top_hit(&self, x: i32, y: i32, pred: impl Fn(&Hit) -> bool) -> Option<Hit> {
        self.rt.top_hit(x, y, pred).cloned()
    }

    fn update_hover(&mut self, x: i32, y: i32) -> bool {
        self.rt.pointer = (x, y);
        let mut dirty = false;
        if let Some(menu) = &mut self.rt.menu {
            let h = menu.at(x, y);
            if h != menu.hovered && (h.is_some() || !menu.rect.contains(x, y)) {
                menu.hovered = h;
                dirty = true;
            }
            return dirty;
        }
        let top = if x < 0 { None } else { self.top_hit(x, y, |_| true) };
        let key = top.as_ref().map(|h| h.key);
        if key != self.rt.hover {
            self.rt.hover = key;
            dirty = true;
            self.tooltip = top
                .as_ref()
                .and_then(|h| self.rt.hits.iter().rev().find(|x| x.key == h.key && x.help.is_some()))
                .and_then(|h| h.help.clone().map(|s| (h.key, self.rt.now.max(corekit::time::uptime_ms()), s)));
        }
        // Views listening for hover.
        let listener = if x < 0 { None } else { self.top_hit(x, y, |h| h.hover) };
        if listener.as_ref().map(|h| h.key) != self.hover_view.as_ref().map(|h| h.key) {
            if let Some(old) = self.hover_view.take() {
                let m = self.message(&old, |v| v.mods.on_hover.as_ref().map(|f| f(false)));
                dirty |= self.send(m);
            }
            if let Some(new) = &listener {
                let m = self.message(new, |v| v.mods.on_hover.as_ref().map(|f| f(true)));
                dirty |= self.send(m);
            }
            self.hover_view = listener;
        }
        // Canvases hear the pointer move over them.
        let canvas = top.filter(|h| matches!(h.kind, HitKind::Canvas));
        if let Some(old) = &self.canvas_hover {
            if canvas.as_ref().map(|h| h.key) != Some(old.key) {
                let old = old.clone();
                dirty |= self.canvas_event(&old, CanvasEvent::Leave);
            }
        }
        if let Some(h) = &canvas {
            dirty |= self.canvas_event(h, CanvasEvent::Hover { x: x - h.full.x, y: y - h.full.y });
        }
        self.canvas_hover = canvas;
        // Scroll bars appear while the pointer is over a scroll view.
        if self.rt.scrollers.iter().any(|s| s.rect.contains(x, y) != s.rect.contains(-1, -1)) {
            dirty = true;
        }
        dirty
    }
}

/// Does `ev` press shortcut `s`?
fn matches(s: Shortcut, ev: &KeyEvent) -> bool {
    let ctrl = ev.mods.ctrl || ev.mods.super_key;
    if s.ctrl != ctrl || s.alt != ev.mods.alt {
        return false;
    }
    let key = match s.key {
        Key::Char(c) => {
            return ev.code == KeyCode::Char
                && ev.ch.is_some_and(|e| e.eq_ignore_ascii_case(&c))
                && (s.shift == ev.mods.shift || !c.is_ascii_alphabetic());
        }
        Key::Enter => KeyCode::Enter,
        Key::Escape => KeyCode::Escape,
        Key::Delete => KeyCode::Delete,
        Key::Backspace => KeyCode::Backspace,
        Key::Tab => KeyCode::Tab,
        Key::Up => KeyCode::Up,
        Key::Down => KeyCode::Down,
        Key::Left => KeyCode::Left,
        Key::Right => KeyCode::Right,
        Key::Home => KeyCode::Home,
        Key::End => KeyCode::End,
        Key::PageUp => KeyCode::PageUp,
        Key::PageDown => KeyCode::PageDown,
        Key::F(n) => match n {
            1 => KeyCode::F1,
            2 => KeyCode::F2,
            3 => KeyCode::F3,
            4 => KeyCode::F4,
            5 => KeyCode::F5,
            6 => KeyCode::F6,
            7 => KeyCode::F7,
            8 => KeyCode::F8,
            9 => KeyCode::F9,
            10 => KeyCode::F10,
            11 => KeyCode::F11,
            _ => KeyCode::F12,
        },
    };
    ev.code == key && s.shift == ev.mods.shift
}

/// How a shortcut reads in a menu ("Ctrl+Shift+S").
pub(crate) fn shortcut_label(s: Shortcut) -> String {
    let mut out = String::new();
    if s.ctrl {
        out.push_str("Ctrl+");
    }
    if s.alt {
        out.push_str("Alt+");
    }
    if s.shift {
        out.push_str("Shift+");
    }
    match s.key {
        Key::Char(c) => out.push(c.to_ascii_uppercase()),
        Key::F(n) => out.push_str(&alloc::format!("F{n}")),
        k => out.push_str(match k {
            Key::Enter => "Return",
            Key::Escape => "Esc",
            Key::Delete => "Delete",
            Key::Backspace => "Backspace",
            Key::Tab => "Tab",
            Key::Up => "↑",
            Key::Down => "↓",
            Key::Left => "←",
            Key::Right => "→",
            Key::Home => "Home",
            Key::End => "End",
            Key::PageUp => "Page Up",
            Key::PageDown => "Page Down",
            _ => "",
        }),
    }
    out
}

impl<A: App> aurorakit::App for Host<A> {
    fn materials(&self, _area: Rect) -> Vec<corekit::abi::MaterialRegion> {
        self.materials
            .iter()
            .map(|r| corekit::abi::MaterialRegion { x: r.x, y: r.y, width: r.w, height: r.h })
            .collect()
    }

    fn title(&self) -> String {
        self.app.title()
    }

    fn size(&self) -> (i32, i32) {
        self.app.size()
    }

    fn resizable(&self) -> bool {
        self.app.resizable()
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &aurorakit::Env) {
        let now = corekit::time::uptime_ms();
        self.env = Env::new(env, area, now);
        self.rt.begin(now, env.focused);
        self.frame(cv, area);
        self.rt.end();
        // Materials are declared before drawing; when they moved, draw
        // again so the window server sees the new regions.
        if self.rt.materials != self.materials {
            self.materials = core::mem::take(&mut self.rt.materials);
            self.redraw = true;
        } else {
            self.redraw = false;
        }
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        let r = self.key_event(ev);
        self.flush(env);
        r || self.redraw
    }

    fn click(&mut self, x: i32, y: i32, _area: Rect, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        let now = env.now_ms;
        self.rt.pointer = (x, y);
        self.tooltip = None;
        if let Some(menu) = &self.rt.menu {
            if menu.rect.contains(x, y) {
                if let Some(i) = menu.at(x, y) {
                    self.pick_menu(i);
                }
            } else {
                self.rt.menu = None;
            }
            self.flush(env);
            return true;
        }
        let Some(h) = self.top_hit(x, y, Hit::clickable) else {
            return false;
        };
        let takes_focus = matches!(
            h.kind,
            HitKind::Field | HitKind::Editor | HitKind::List { .. } | HitKind::Canvas | HitKind::Slider { .. }
        ) && h.focusable;
        if takes_focus {
            self.focus(Some(h.key), false);
        } else if self.rt.focus_visible {
            self.rt.focus_visible = false;
        }
        self.press = Some(Press { key: h.key, x, y, hit: h.clone(), dragged: false });
        let shift = env.mods.shift;
        let dirty = match h.kind {
            HitKind::Tap | HitKind::Toggle(_) | HitKind::Button { menu: false } => {
                self.rt.pressed = Some(h.key);
                true
            }
            HitKind::Button { menu: true } | HitKind::PickerMenu => {
                self.press = None;
                self.activate(&h)
            }
            HitKind::Slider { .. } => {
                self.set_slider(&h, x);
                true
            }
            HitKind::Stepper { minus, plus, .. } => {
                if minus.contains(x, y) {
                    self.step_value(&h, false)
                } else if plus.contains(x, y) {
                    self.step_value(&h, true)
                } else {
                    false
                }
            }
            HitKind::Segmented { n, selected } => {
                let i = ((x - h.full.x) * n as i32 / h.full.w.max(1)).clamp(0, n as i32 - 1) as usize;
                i != selected && self.choose(&h, i)
            }
            HitKind::PickerInline { n } => {
                let i = ((y - h.full.y) / 30).clamp(0, n as i32 - 1) as usize;
                self.choose(&h, i)
            }
            HitKind::Field => self.rt.fields.get_mut(&h.key).is_some_and(|f| f.click(x, y, shift, now)),
            HitKind::Editor => {
                let rev = self.rt.editors.get(&h.key).map(|e| e.edit.revision());
                let r = self.rt.editors.get_mut(&h.key).is_some_and(|e| e.click(x, y, shift, now));
                // A click can tick a checklist's box.
                if self.rt.editors.get(&h.key).map(|e| e.edit.revision()) != rev {
                    self.text_changed(&h);
                }
                self.caret_moved(&h);
                r
            }
            HitKind::List { count, row_h, top, selection, .. } => {
                let row = (y - top) / row_h.max(1);
                if row >= 0 && (row as usize) < count && selection != Some(row as usize) {
                    self.choose(&h, row as usize)
                } else {
                    true
                }
            }
            HitKind::Canvas => {
                let ev = CanvasEvent::Press { x: x - h.full.x, y: y - h.full.y, button: 1, clicks: 1 };
                self.canvas_event(&h, ev);
                true
            }
        };
        self.flush(env);
        dirty || self.redraw
    }

    fn double_click(&mut self, x: i32, y: i32, area: Rect, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        let now = env.now_ms;
        if self.rt.menu.is_some() {
            return self.click(x, y, area, env);
        }
        let Some(h) = self.top_hit(x, y, Hit::clickable) else { return false };
        let r = match h.kind {
            HitKind::List { count, row_h, top, activate: true, .. } => {
                let row = (y - top) / row_h.max(1);
                row >= 0 && (row as usize) < count && self.list_activate(&h, row as usize)
            }
            HitKind::Tap if h.double => {
                let m = self.message(&h, |v| v.mods.on_double_tap.clone());
                self.send(m)
            }
            HitKind::Field => {
                self.press = Some(Press { key: h.key, x, y, hit: h.clone(), dragged: false });
                self.rt.fields.get_mut(&h.key).is_some_and(|f| f.double_click(x, y, now))
            }
            HitKind::Editor => {
                self.press = Some(Press { key: h.key, x, y, hit: h.clone(), dragged: false });
                let r = self.rt.editors.get_mut(&h.key).is_some_and(|e| e.double_click(x, y, now));
                self.caret_moved(&h);
                r
            }
            HitKind::Canvas => {
                self.press = Some(Press { key: h.key, x, y, hit: h.clone(), dragged: false });
                let ev = CanvasEvent::Press { x: x - h.full.x, y: y - h.full.y, button: 1, clicks: 2 };
                self.canvas_event(&h, ev);
                true
            }
            _ => return self.click(x, y, area, env),
        };
        self.flush(env);
        r || self.redraw
    }

    fn right_click(&mut self, x: i32, y: i32, _area: Rect, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        self.rt.pointer = (x, y);
        self.tooltip = None;
        self.rt.menu = None;
        let r = if let Some(h) = self.top_hit(x, y, |h| h.menu) {
            self.open_menu(&h, MenuSource::Context)
        } else if let Some(h) = self.top_hit(x, y, |h| matches!(h.kind, HitKind::Canvas)) {
            let ev = CanvasEvent::Press { x: x - h.full.x, y: y - h.full.y, button: 2, clicks: 1 };
            self.canvas_event(&h, ev)
        } else {
            false
        };
        self.flush(env);
        r || self.redraw
    }

    fn release(&mut self, x: i32, y: i32, _area: Rect, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        let Some(p) = self.press.take() else { return false };
        self.rt.pressed = None;
        let h = p.hit;
        let r = match h.kind {
            HitKind::Tap | HitKind::Toggle(_) | HitKind::Button { .. } => {
                let inside = self.rt.hit_by_key(p.key).map(|x| x.rect).unwrap_or(h.rect).contains(x, y);
                (inside && !p.dragged && self.activate(&h)) || true
            }
            HitKind::Field => {
                if let Some(f) = self.rt.fields.get_mut(&h.key) {
                    f.release();
                }
                false
            }
            HitKind::Editor => {
                if let Some(e) = self.rt.editors.get_mut(&h.key) {
                    e.release();
                }
                false
            }
            HitKind::Canvas => self.canvas_event(&h, CanvasEvent::Release { x: x - h.full.x, y: y - h.full.y }),
            _ => true,
        };
        self.flush(env);
        r || self.redraw
    }

    fn hover(&mut self, x: i32, y: i32, _area: Rect) -> bool {
        self.update_hover(x, y) || self.redraw
    }

    fn drag(&mut self, x: i32, y: i32, area: Rect, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        self.rt.pointer = (x, y);
        let Some(p) = &mut self.press else {
            return self.hover(x, y, area);
        };
        let far = (x - p.x).abs() + (y - p.y).abs() > 4;
        let h = p.hit.clone();
        if h.drag && !p.dragged && far {
            p.dragged = true;
            self.rt.pressed = None;
            let item = self.message_drag(&h);
            match item {
                Some(DragItem::Files(paths, icon)) => {
                    corekit::dnd::start_files(&paths, icon);
                }
                Some(DragItem::Text(t)) => {
                    corekit::dnd::start_text(&t);
                }
                None => {}
            }
            return true;
        }
        let r = match h.kind {
            HitKind::Slider { .. } => {
                // Use the latest value so each move compares with the last.
                let cur = self.rt.hits.iter().find(|x| x.key == h.key).cloned().unwrap_or(h);
                self.set_slider(&cur, x)
            }
            HitKind::Field => self.rt.fields.get_mut(&h.key).is_some_and(|f| f.drag(x)),
            HitKind::Editor => {
                let r = self.rt.editors.get_mut(&h.key).is_some_and(|e| e.drag(x, y));
                self.caret_moved(&h);
                r
            }
            HitKind::Canvas => self.canvas_event(&h, CanvasEvent::Drag { x: x - h.full.x, y: y - h.full.y }),
            HitKind::Tap | HitKind::Toggle(_) | HitKind::Button { .. } => {
                let inside = h.rect.contains(x, y) && !self.press.as_ref().is_some_and(|p| p.dragged);
                let want = if inside { Some(h.key) } else { None };
                core::mem::replace(&mut self.rt.pressed, want) != want
            }
            _ => false,
        };
        self.flush(env);
        r || self.redraw
    }

    fn wheel(&mut self, notches: i32, dx: i32, dy: i32, _area: Rect) -> bool {
        let (dx, dy) = if dx == 0 && dy == 0 { (0, notches * 120) } else { (dx, dy) };
        let smooth = dy % 120 == 0 && dx % 120 == 0;
        let (px, py) = self.rt.pointer;
        let top = self.rt.top_layer;
        let Some(s) = self.rt.scrollers.iter().rev().find(|s| s.layer == top && s.rect.contains(px, py)) else {
            return false;
        };
        let (key, kind, path, rect) = (s.key, s.kind, s.path.clone(), s.rect);
        let now = corekit::time::uptime_ms();
        let px_x = dx * NOTCH_PX / 120;
        let px_y = dy * NOTCH_PX / 120;
        match kind {
            ScrollKind::View(axis) => {
                let d = match axis {
                    crate::view::Axis::Vertical => px_y,
                    crate::view::Axis::Horizontal => {
                        if px_x != 0 {
                            px_x
                        } else {
                            px_y
                        }
                    }
                };
                if let Some(st) = self.rt.scroll.get_mut(&key) {
                    st.by(d as f32, smooth, now);
                }
                true
            }
            ScrollKind::List => {
                if let Some(st) = self.rt.scroll.get_mut(&key) {
                    st.by(px_y as f32, smooth, now);
                }
                true
            }
            ScrollKind::Editor => {
                if let Some(e) = self.rt.editors.get_mut(&key) {
                    e.scroll += px_y;
                }
                true
            }
            ScrollKind::Canvas => {
                let h = Hit {
                    rect,
                    full: rect,
                    key,
                    path,
                    layer: top,
                    kind: HitKind::Canvas,
                    focusable: false,
                    shortcut: None,
                    tap: false,
                    double: false,
                    menu: false,
                    hover: false,
                    help: None,
                    drop: false,
                    drag: false,
                    appear: false,
                };
                self.canvas_event(&h, CanvasEvent::Scroll { dx: px_x, dy: px_y })
            }
        }
    }

    fn drag_over(&mut self, x: i32, y: i32, _area: Rect, _env: &mut aurorakit::Env) -> bool {
        let target = if x < 0 { None } else { self.top_hit(x, y, |h| h.drop).map(|h| h.key) };
        core::mem::replace(&mut self.rt.drop_target, target) != target
    }

    fn drop(&mut self, x: i32, y: i32, kind: u32, _area: Rect, env: &mut aurorakit::Env) -> bool {
        self.env.mods = env.mods;
        self.rt.drop_target = None;
        let Some(h) = self.top_hit(x, y, |h| h.drop) else { return true };
        let Some(items) = corekit::dnd::dropped(kind) else { return true };
        let info = DropInfo { x: x - h.full.x, y: y - h.full.y, items };
        let m = self.message(&h, |v| v.mods.on_drop.as_ref().and_then(|f| f(info)));
        self.send(m);
        self.flush(env);
        true
    }

    fn close_requested(&mut self, env: &mut aurorakit::Env) -> bool {
        let (app, mut cx) = self.cx();
        let close = app.close_requested(&mut cx);
        self.flush(env);
        self.redraw = true;
        close
    }

    fn close_tab_requested(&mut self, env: &mut aurorakit::Env) -> bool {
        let (app, mut cx) = self.cx();
        let close = app.close_tab_requested(&mut cx);
        self.flush(env);
        self.redraw = true;
        close
    }

    fn tick(&mut self, env: &mut aurorakit::Env) -> bool {
        let now = env.now_ms;
        self.rt.now = now;
        let mut dirty = core::mem::take(&mut self.redraw);
        if !self.started {
            self.started = true;
            let (app, mut cx) = self.cx();
            app.started(&mut cx);
            dirty = true;
        }
        // Timers that are due, oldest first.
        let mut due = Vec::new();
        let mut i = 0;
        while i < self.timers.len() {
            if self.timers[i].0 <= now {
                due.push(self.timers.remove(i));
            } else {
                i += 1;
            }
        }
        due.sort_by_key(|t| t.0);
        for (_, m) in due {
            dirty |= self.send(Some(m));
        }
        if let Some(every) = self.app.poll_interval() {
            if now >= self.next_poll {
                self.next_poll = now + every;
                let (app, mut cx) = self.cx();
                dirty |= app.poll(&mut cx);
            }
        }
        for (path, w, h) in core::mem::take(&mut self.rt.pending_resize) {
            let hit = Hit {
                rect: Rect::new(0, 0, w, h),
                full: Rect::new(0, 0, w, h),
                key: 0,
                path,
                layer: 0,
                kind: HitKind::Canvas,
                focusable: false,
                shortcut: None,
                tap: false,
                double: false,
                menu: false,
                hover: false,
                help: None,
                drop: false,
                drag: false,
                appear: false,
            };
            dirty |= self.canvas_event(&hit, CanvasEvent::Resize { w, h });
        }
        if self.rt.animating || self.rt.scroll.values().any(|s| abs(s.target - s.pos) >= 0.5) {
            dirty = true;
        }
        if now >= self.rt.wake {
            dirty = true;
        }
        if let Some(f) = self.rt.focus {
            let active = env.focused;
            if let Some(field) = self.rt.fields.get_mut(&f) {
                dirty |= field.tick(now, active);
            } else if let Some(e) = self.rt.editors.get_mut(&f) {
                dirty |= e.tick(now, active);
            }
        }
        self.flush(env);
        dirty || self.redraw
    }

    fn next_wakeup(&self, env: &aurorakit::Env) -> Option<u64> {
        let now = env.now_ms;
        let mut at = now + 60_000;
        if self.redraw
            || !self.started
            || !self.queue.is_empty()
            || !self.rt.pending_resize.is_empty()
            || !self.requests.is_empty()
        {
            return Some(now);
        }
        if self.rt.animating || self.rt.scroll.values().any(|s| abs(s.target - s.pos) >= 0.5) {
            at = at.min(now + 16);
        }
        if let Some(t) = self.timers.iter().map(|t| t.0).min() {
            at = at.min(t);
        }
        if self.app.poll_interval().is_some() {
            at = at.min(self.next_poll.max(now));
        }
        if self.rt.wake != u64::MAX {
            at = at.min(self.rt.wake.max(now));
        }
        if let Some(f) = self.rt.focus {
            if self.rt.fields.contains_key(&f) || self.rt.editors.contains_key(&f) {
                at = at.min(now + BLINK_MS);
            }
        }
        Some(at)
    }
}

impl<A: App> Host<A> {
    fn message_drag(&self, h: &Hit) -> Option<DragItem> {
        self.with(&h.path, |v| v.mods.drag.clone())
    }
}

/// The key a view with `.id(id)` is known by.
pub(crate) fn id_key<K: core::hash::Hash + ?Sized>(id: &K) -> u64 {
    key_of(id)
}

impl<M> Context<'_, M> {
    /// The text editor shown with `.id(id)`, to read or move its caret and selection.
    pub fn editor<K: core::hash::Hash + ?Sized>(&mut self, id: &K) -> Option<&mut TextView> {
        self.rt.editors.get_mut(&id_key(id))
    }

    /// The text field shown with `.id(id)`.
    pub fn field<K: core::hash::Hash + ?Sized>(&mut self, id: &K) -> Option<&mut TextField> {
        self.rt.fields.get_mut(&id_key(id))
    }

    /// Gives keyboard focus to the view shown with `.id(id)` (and scrolls it into view).
    pub fn focus<K: core::hash::Hash + ?Sized>(&mut self, id: &K) {
        self.rt.focus = Some(id_key(id));
        self.rt.focus_visible = false;
        self.rt.reveal = true;
    }

    /// Focuses the view shown with `.id(id)` with a visible ring, as if
    /// reached with Tab: to point something out.
    pub fn focus_ring<K: core::hash::Hash + ?Sized>(&mut self, id: &K) {
        self.focus(id);
        self.rt.focus_visible = true;
    }

    /// Takes focus away from whatever has it.
    pub fn blur(&mut self) {
        self.rt.focus = None;
    }

    /// Is the view shown with `.id(id)` focused?
    pub fn is_focused<K: core::hash::Hash + ?Sized>(&self, id: &K) -> bool {
        self.rt.focus == Some(id_key(id))
    }

    /// Scrolls the scroll view or list shown with `.id(id)` to `y` pixels
    /// (`f32::MAX`: the end).
    pub fn scroll_to<K: core::hash::Hash + ?Sized>(&mut self, id: &K, y: f32) {
        self.rt.scroll.entry(id_key(id)).or_default().to(y);
    }

    /// Where the scroll view or list shown with `.id(id)` is scrolled to.
    pub fn scroll_offset<K: core::hash::Hash + ?Sized>(&self, id: &K) -> i32 {
        self.rt.scroll_offset(id_key(id))
    }
}
