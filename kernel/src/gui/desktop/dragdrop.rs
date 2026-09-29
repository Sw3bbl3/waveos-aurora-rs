//! The compositor's half of drag and drop (see `gui::dnd` for the protocol).

use super::shell::DockItem;
use super::Desktop;
use crate::gui::apps::{self, AppKind};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::dnd::{self, Payload};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::server;
use crate::gui::theme;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::{clip, drag, event, Event};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Nothing,
    /// A window (desktop id) whose content is under the pointer.
    Window(u32),
    DockApp(AppKind),
    Trash,
}

pub struct Session {
    pub payload: Payload,
    label: String,
    pub target: Target,
    /// Where the drag image was last drawn (repainted when it moves).
    image: Rect,
}

const IMAGE: i32 = 48;

impl Desktop {
    /// Picks up a payload from `drag_start` if the button is still down.
    pub(super) fn begin_drag(&mut self) {
        let Some(payload) = dnd::take_pending() else { return };
        if self.buttons & crate::drivers::input::BUTTON_LEFT == 0 {
            return; // released before we got here: nothing to drag
        }
        self.grab = None;
        let label = payload.label();
        let image = self.drag_image_rect();
        self.dnd = Some(Session { payload, label, target: Target::Nothing, image });
        let (x, y) = self.cursor;
        self.drag_moved(x, y);
    }

    fn drag_image_rect(&self) -> Rect {
        let (x, y) = self.cursor;
        Rect::new(x - IMAGE / 2 - 90, y - IMAGE / 2 - 12, IMAGE + 180, IMAGE + 48)
    }

    fn accepts(&self, kind: AppKind, p: &Payload) -> bool {
        p.kind == clip::FILES && matches!(kind, AppKind::Notes | AppKind::Preview)
    }

    fn target_at(&self, x: i32, y: i32) -> Target {
        let Some(s) = &self.dnd else { return Target::Nothing };
        let (dock, items) = self.dock_layout();
        if dock.contains(x, y) {
            for (item, r) in items {
                if !r.inset(-5).contains(x, y) {
                    continue;
                }
                return match item {
                    DockItem::App(k) if self.accepts(k, &s.payload) => Target::DockApp(k),
                    DockItem::Trash if s.payload.kind == clip::FILES => Target::Trash,
                    _ => Target::Nothing,
                };
            }
            return Target::Nothing;
        }
        match self.window_at(x, y) {
            Some(i) if self.windows[i].content().contains(x, y) && self.windows[i].app.client_id().is_some() => {
                Target::Window(self.windows[i].id)
            }
            _ => Target::Nothing,
        }
    }

    fn send_to_window(&mut self, id: u32, kind: u32, x: i32, y: i32) {
        let Some(i) = self.index_of(id) else { return };
        let Some(client) = self.windows[i].app.client_id() else { return };
        let c = self.windows[i].content();
        let Some(s) = &self.dnd else { return };
        let mods = crate::drivers::input::modifiers().bits();
        server::push_event(
            client,
            Event {
                kind,
                x: x - c.x,
                y: y - c.y,
                a: s.payload.kind as u32,
                b: s.payload.count,
                d: mods,
                ..Default::default()
            },
        );
    }

    /// Pointer motion during a drag.
    pub(super) fn drag_moved(&mut self, x: i32, y: i32) {
        if let Some(old) = self.dnd.as_ref().map(|s| s.image) {
            self.damage(old);
        }
        let target = self.target_at(x, y);
        let old = self.dnd.as_ref().map(|s| s.target).unwrap_or(Target::Nothing);
        if let Target::Window(w) = old {
            if target != old {
                self.send_to_window(w, event::DRAG_LEAVE, x, y);
            }
        }
        if let Target::Window(w) = target {
            self.send_to_window(w, event::DRAG_OVER, x, y);
        }
        if let Some(s) = &mut self.dnd {
            s.target = target;
        }
        // Dock highlight follows the drop target.
        let (dock, items) = self.dock_layout();
        let dh = match target {
            Target::DockApp(k) => items.iter().position(|(i, _)| *i == DockItem::App(k)),
            Target::Trash => items.iter().position(|(i, _)| *i == DockItem::Trash),
            _ => None,
        };
        if dh != self.dock_hover {
            self.dock_hover = dh;
            self.damage(dock.inset(-44));
        }
        let new_image = self.drag_image_rect();
        self.damage(new_image);
        if let Some(s) = &mut self.dnd {
            s.image = new_image;
        }
    }

    fn end_drag(&mut self, dropped: bool) {
        let Some(s) = self.dnd.take() else { return };
        self.damage(s.image);
        let image = self.drag_image_rect();
        self.damage(image);
        self.dock_hover = None;
        let d = self.dock_rect();
        self.damage(d.inset(-44));
        // Tell the source (e.g. Files refreshes after a move).
        server::push_event(s.payload.source, Event { kind: event::DRAG_END, a: dropped as u32, ..Default::default() });
    }

    /// Button released during a drag.
    pub(super) fn drop_drag(&mut self, x: i32, y: i32) {
        let Some(s) = &self.dnd else { return };
        let target = self.target_at(x, y);
        let paths = s.payload.paths();
        let dropped = match target {
            Target::Window(id) => {
                let client = self.index_of(id).and_then(|i| self.windows[i].app.client_id());
                match client.and_then(server::pid_of) {
                    Some(pid) => {
                        dnd::deliver(pid, s.payload.data.clone());
                        self.send_to_window(id, event::DROP, x, y);
                        self.raise(id);
                        true
                    }
                    None => false,
                }
            }
            Target::DockApp(kind) => {
                let program = apps::info(kind).path;
                for p in paths.iter().take(8) {
                    self.spawn_program(program, &[p.as_str()]);
                }
                !paths.is_empty()
            }
            Target::Trash => {
                let mut moved = 0;
                for p in &paths {
                    match crate::fs::trash::move_to_trash(p) {
                        Ok(_) => moved += 1,
                        Err(e) => log!("gui", "could not move {} to the Trash: {}", p, aurora_abi::err::name(e)),
                    }
                }
                self.refresh_trash();
                moved > 0
            }
            Target::Nothing => false,
        };
        self.end_drag(dropped);
    }

    pub(super) fn cancel_drag(&mut self) {
        if let Some(Target::Window(w)) = self.dnd.as_ref().map(|s| s.target) {
            let (x, y) = self.cursor;
            self.send_to_window(w, event::DRAG_LEAVE, x, y);
        }
        self.end_drag(false);
    }

    pub(super) fn refresh_trash(&mut self) {
        let full = crate::fs::trash::count() > 0;
        if full != self.trash_full {
            self.trash_full = full;
            let d = self.dock_rect();
            self.damage(d.inset(-44));
        }
    }

    /// The translucent icon, count badge and label following the pointer.
    pub(super) fn paint_drag(&self, cv: &mut Canvas) {
        let Some(s) = &self.dnd else { return };
        if !self.drag_image_rect().intersects(&cv.clip) {
            return;
        }
        let t = theme::current();
        let (x, y) = self.cursor;
        let icon_r = Rect::new(x - IMAGE / 2 + 6, y - IMAGE / 2 + 6, IMAGE, IMAGE);
        let icon = match s.payload.icon {
            drag::FOLDER => Icon::Folder,
            drag::PICTURE => Icon::Picture,
            drag::TEXT => Icon::Document,
            _ => Icon::Document,
        };
        // Draw the icon at ~75% opacity by blending a copy of the area.
        let area = icon_r.intersect(&cv.clip);
        let mut saved: Vec<u32> = Vec::with_capacity((area.w * area.h).max(0) as usize);
        for yy in area.y..area.bottom() {
            let row = (yy * cv.width) as usize;
            saved.extend_from_slice(&cv.buf[row + area.x as usize..row + area.right() as usize]);
        }
        icons::draw(cv, icon, icon_r);
        for (k, yy) in (area.y..area.bottom()).enumerate() {
            let row = (yy * cv.width) as usize;
            for (j, xx) in (area.x..area.right()).enumerate() {
                let i = row + xx as usize;
                cv.buf[i] = crate::gui::canvas::blend(saved[k * area.w as usize + j], cv.buf[i], 190);
            }
        }
        if s.payload.count > 1 {
            let n = alloc::format!("{}", s.payload.count);
            let f = theme::ui_bold(11);
            let w = (f.width(&n) + 12).max(20);
            let badge = Rect::new(icon_r.right() - w / 2 - 2, icon_r.y - 6, w, 20);
            cv.fill_round_rect(badge, 10, theme::CLOSE);
            cv.text_centered(badge, &n, f, 0xFFFF_FFFF);
        }
        if !s.label.is_empty() {
            let f = theme::ui(12);
            let w = f.width(&s.label).min(170) + 16;
            let pill = Rect::new(icon_r.x + IMAGE / 2 - w / 2, icon_r.bottom() + 4, w, 20);
            let bg = if s.target == Target::Nothing {
                with_alpha(0x202028, 0xC8)
            } else {
                with_alpha(theme::accent(), 0xE8)
            };
            cv.fill_round_rect(pill, 10, bg);
            cv.text_clipped(pill.x + 8, pill.y + 14, &s.label, f, 0xFFFF_FFFF, w - 16);
        }
        let _ = t;
    }
}
