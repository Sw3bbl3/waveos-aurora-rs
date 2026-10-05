//! AuroraKit's portable declarative layout and interaction core.
//! Apps rebuild views from their model; stable IDs preserve keyboard focus.
use crate::{canvas::Canvas, geom::Rect, theme, widgets};
use alloc::{
    string::{String, ToString},
    vec::Vec,
};

#[derive(Clone, Debug)]
pub enum Kind {
    Label,
    Heading,
    Button,
    Toggle(bool),
    Slider(i32),
    Column,
    Row,
    Grid(usize),
    Scroll(i32),
    Card,
    Glass,
}
#[derive(Clone, Debug)]
pub struct View {
    pub id: u64,
    pub kind: Kind,
    pub text: String,
    pub children: Vec<View>,
    pub height: Option<i32>,
    pub padding: i32,
    pub gap: i32,
    pub enabled: bool,
}
impl View {
    pub fn new(id: u64, kind: Kind, text: &str) -> Self {
        Self {
            id,
            kind,
            text: text.to_string(),
            children: Vec::new(),
            height: None,
            padding: 0,
            gap: 12,
            enabled: true,
        }
    }
    pub fn label(text: &str) -> Self {
        Self::new(0, Kind::Label, text)
    }
    pub fn heading(text: &str) -> Self {
        Self::new(0, Kind::Heading, text)
    }
    pub fn button(id: u64, text: &str) -> Self {
        Self::new(id, Kind::Button, text)
    }
    pub fn toggle(id: u64, text: &str, value: bool) -> Self {
        Self::new(id, Kind::Toggle(value), text)
    }
    pub fn slider(id: u64, text: &str, value: i32) -> Self {
        Self::new(id, Kind::Slider(value.clamp(0, 1000)), text)
    }
    pub fn column(children: Vec<View>) -> Self {
        Self::new(0, Kind::Column, "").children(children)
    }
    pub fn row(children: Vec<View>) -> Self {
        Self::new(0, Kind::Row, "").children(children)
    }
    pub fn grid(columns: usize, children: Vec<View>) -> Self {
        Self::new(0, Kind::Grid(columns.max(1)), "").children(children)
    }
    pub fn card(children: Vec<View>) -> Self {
        Self::new(0, Kind::Card, "").children(children).padding(16)
    }
    pub fn glass(children: Vec<View>) -> Self {
        Self::new(0, Kind::Glass, "").children(children).padding(16)
    }
    pub fn children(mut self, children: Vec<View>) -> Self {
        self.children = children;
        self
    }
    pub fn padding(mut self, value: i32) -> Self {
        self.padding = value.max(0);
        self
    }
    pub fn gap(mut self, value: i32) -> Self {
        self.gap = value.max(0);
        self
    }
    pub fn height(mut self, value: i32) -> Self {
        self.height = Some(value.max(0));
        self
    }
    pub fn enabled(mut self, value: bool) -> Self {
        self.enabled = value;
        self
    }
    pub fn intrinsic_height(&self) -> i32 {
        if let Some(h) = self.height {
            return h;
        }
        let content = match self.kind {
            Kind::Heading => 34,
            Kind::Label => 24,
            Kind::Button => 36,
            Kind::Toggle(_) => 44,
            Kind::Slider(_) => 64,
            Kind::Row => self.children.iter().map(Self::intrinsic_height).max().unwrap_or(0),
            Kind::Grid(n) => {
                let n = n.max(1);
                self.children
                    .chunks(n)
                    .map(|row| row.iter().map(Self::intrinsic_height).max().unwrap_or(0))
                    .sum::<i32>()
                    + self.gap * self.children.len().div_ceil(n).saturating_sub(1) as i32
            }
            _ => {
                self.children.iter().map(Self::intrinsic_height).sum::<i32>()
                    + self.gap * self.children.len().saturating_sub(1) as i32
            }
        };
        content + 2 * self.padding
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Action {
    Activate(u64),
    Toggle(u64, bool),
    Change(u64, i32),
}
#[derive(Clone, Copy)]
pub enum Key {
    Next,
    Previous,
    Activate,
    Left,
    Right,
}
#[derive(Clone)]
struct Target {
    id: u64,
    rect: Rect,
    control: Rect,
    kind: Kind,
}
#[derive(Default)]
pub struct State {
    pub focused: Option<u64>,
    pub hovered: Option<u64>,
    targets: Vec<Target>,
    dragging: Option<u64>,
}

impl State {
    pub fn draw(&mut self, cv: &mut Canvas, view: &View, area: Rect) {
        self.targets.clear();
        self.paint(cv, view, area);
        if self.focused.is_some_and(|id| !self.targets.iter().any(|t| t.id == id)) {
            self.focused = None;
        }
    }
    fn paint(&mut self, cv: &mut Canvas, v: &View, r: Rect) {
        let t = theme::current();
        let hover = self.hovered == Some(v.id) && v.enabled;
        let fg = if v.enabled { t.text } else { t.text_secondary };
        let interactive = matches!(v.kind, Kind::Button | Kind::Toggle(_) | Kind::Slider(_));
        let control = match v.kind {
            Kind::Toggle(_) => Rect::new(r.right() - 48, r.y + 10, 44, 24),
            Kind::Slider(_) => Rect::new(r.x + 10, r.y + 36, (r.w - 20).max(1), 24),
            _ => r,
        };
        if interactive && v.enabled && !r.intersect(&cv.clip).is_empty() {
            self.targets.push(Target { id: v.id, rect: r.intersect(&cv.clip), control, kind: v.kind.clone() });
        }
        match v.kind {
            Kind::Label => {
                cv.text_clipped(r.x, r.y + 18, &v.text, theme::ui(13), fg, r.w);
            }
            Kind::Heading => {
                cv.text_clipped(r.x, r.y + 25, &v.text, theme::ui_bold(22), fg, r.w);
            }
            Kind::Button => widgets::button(cv, r, &v.text, widgets::ButtonStyle::Secondary, hover),
            Kind::Toggle(on) => {
                cv.text_clipped(r.x, r.y + 27, &v.text, theme::ui(14), fg, (r.w - 64).max(0));
                widgets::toggle(cv, control, on);
            }
            Kind::Slider(value) => {
                cv.text_clipped(r.x, r.y + 20, &v.text, theme::ui(13), fg, r.w);
                widgets::slider(cv, control, value, hover);
            }
            Kind::Card => {
                cv.fill_round_rect(r, 14, t.window_bg_alt);
                cv.stroke_round_rect(r, 14, t.separator);
            }
            Kind::Glass => cv.frosted(r, 16, t.panel_tint),
            _ => {}
        }
        if interactive && self.focused == Some(v.id) {
            cv.stroke_round_rect(r.inset(-2), 8, theme::accent());
        }
        let inner = r.inset(v.padding);
        cv.with_clip(inner, |cv| match v.kind {
            Kind::Row => {
                let n = v.children.len() as i32;
                if n == 0 {
                    return;
                }
                let space = (inner.w - v.gap * (n - 1)).max(0);
                for (i, c) in v.children.iter().enumerate() {
                    let i = i as i32;
                    let x = inner.x + space * i / n + v.gap * i;
                    let w = space * (i + 1) / n - space * i / n;
                    self.paint(cv, c, Rect::new(x, inner.y, w, inner.h));
                }
            }
            Kind::Grid(columns) => {
                let n = columns.max(1) as i32;
                let w = (inner.w - v.gap * (n - 1)).max(0) / n;
                let mut y = inner.y;
                for row in v.children.chunks(n as usize) {
                    let h = row.iter().map(View::intrinsic_height).max().unwrap_or(0);
                    for (i, c) in row.iter().enumerate() {
                        self.paint(cv, c, Rect::new(inner.x + i as i32 * (w + v.gap), y, w, h));
                    }
                    y += h + v.gap;
                }
            }
            _ => {
                let mut y = inner.y - if let Kind::Scroll(offset) = v.kind { offset.max(0) } else { 0 };
                for c in &v.children {
                    let h = c.intrinsic_height();
                    self.paint(cv, c, Rect::new(inner.x, y, inner.w, h));
                    y += h + v.gap;
                }
            }
        });
    }
    pub fn rect(&self, id: u64) -> Option<Rect> {
        self.targets.iter().find(|t| t.id == id).map(|t| t.rect)
    }
    pub fn click(&mut self, x: i32, y: i32) -> Option<Action> {
        let target = self.targets.iter().rev().find(|t| t.rect.contains(x, y))?;
        self.focused = Some(target.id);
        match target.kind {
            Kind::Toggle(on) => Some(Action::Toggle(target.id, !on)),
            Kind::Slider(_) => {
                self.dragging = Some(target.id);
                Some(Action::Change(target.id, widgets::slider_value(target.control, x)))
            }
            _ => Some(Action::Activate(target.id)),
        }
    }
    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        let next = self.targets.iter().rev().find(|t| t.rect.contains(x, y)).map(|t| t.id);
        let changed = next != self.hovered;
        self.hovered = next;
        changed
    }
    pub fn drag(&self, x: i32) -> Option<Action> {
        let t = self.targets.iter().find(|t| Some(t.id) == self.dragging)?;
        Some(Action::Change(t.id, widgets::slider_value(t.control, x)))
    }
    pub fn release(&mut self) {
        self.dragging = None;
    }
    /// Returns false at a boundary so the containing app can move focus out.
    pub fn traverse(&mut self, reverse: bool) -> bool {
        let i = self.focused.and_then(|id| self.targets.iter().position(|t| t.id == id));
        let next = match i {
            None => {
                if reverse {
                    self.targets.len().checked_sub(1)
                } else {
                    Some(0)
                }
            }
            Some(i) => {
                if reverse {
                    i.checked_sub(1)
                } else {
                    Some(i + 1)
                }
            }
        };
        if let Some(t) = next.and_then(|i| self.targets.get(i)) {
            self.focused = Some(t.id);
            true
        } else {
            self.focused = None;
            false
        }
    }
    pub fn key(&mut self, key: Key) -> Option<Action> {
        if self.targets.is_empty() {
            return None;
        }
        let i = self.focused.and_then(|id| self.targets.iter().position(|t| t.id == id));
        match key {
            Key::Next | Key::Previous => {
                let n = self.targets.len();
                let next =
                    i.map(|i| if matches!(key, Key::Previous) { (i + n - 1) % n } else { (i + 1) % n }).unwrap_or(0);
                self.focused = Some(self.targets[next].id);
                None
            }
            _ => {
                let t = &self.targets[i?];
                match t.kind {
                    Kind::Slider(value) if matches!(key, Key::Left | Key::Right) => Some(Action::Change(
                        t.id,
                        (value + if matches!(key, Key::Left) { -50 } else { 50 }).clamp(0, 1000),
                    )),
                    Kind::Toggle(on) if matches!(key, Key::Activate) => Some(Action::Toggle(t.id, !on)),
                    Kind::Button if matches!(key, Key::Activate) => Some(Action::Activate(t.id)),
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_routes_input_and_preserves_focus() {
        let view = View::column(alloc::vec![View::toggle(1, "One", false), View::button(2, "Two")]).gap(8);
        let mut pixels = alloc::vec![0;200*120];
        let mut state = State::default();
        let mut cv = Canvas::new(&mut pixels, 200, 120);
        state.draw(&mut cv, &view, Rect::new(0, 0, 200, 120));
        assert_eq!(state.click(20, 20), Some(Action::Toggle(1, true)));
        state.key(Key::Next);
        assert_eq!(state.key(Key::Activate), Some(Action::Activate(2)));
        state.draw(&mut cv, &view, Rect::new(0, 0, 150, 120));
        assert_eq!(state.focused, Some(2));
        assert_eq!(state.rect(2).unwrap().y, 52);
    }
    #[test]
    fn slider_activation_is_not_a_value_change_and_focus_can_leave() {
        let view = View::column(alloc::vec![View::slider(1, "Level", 500), View::button(2, "Done")]);
        let mut pixels = alloc::vec![0;200*160];
        let mut state = State::default();
        state.draw(&mut Canvas::new(&mut pixels, 200, 160), &view, Rect::new(0, 0, 200, 160));
        assert!(state.traverse(false));
        assert_eq!(state.key(Key::Activate), None);
        assert_eq!(state.key(Key::Left), Some(Action::Change(1, 450)));
        assert!(state.traverse(false));
        assert!(!state.traverse(false));
        assert_eq!(state.focused, None);
        assert!(state.traverse(true));
        assert_eq!(state.focused, Some(2));
    }
    #[test]
    fn clipped_children_cannot_receive_clicks() {
        let v = View::column(alloc::vec![View::button(1, "One"), View::button(2, "Two")]);
        let mut b = alloc::vec![0;100*40];
        let mut s = State::default();
        s.draw(&mut Canvas::new(&mut b, 100, 40), &v, Rect::new(0, 0, 100, 40));
        assert!(s.rect(2).is_none());
    }
}
