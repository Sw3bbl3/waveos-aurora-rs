//! Shoal: every app, full screen. Type to narrow the grid, pick a group to
//! browse by kind, and Enter opens the first match. Harbor stays afloat
//! below, and the account and power sit at the top corners.

use super::harbor::DockItem;
use super::Desktop;
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::gui::apps::{AppKind, CATALOG};
use crate::gui::canvas::{with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{self, MENUBAR_H};
use alloc::string::String;
use alloc::vec::Vec;

const COLS: i32 = 7;
const ROWS: i32 = 3;
const TILE_W: i32 = 132;
const TILE_H: i32 = 128;
const ICON: i32 = 72;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Group {
    All,
    Work,
    Create,
    Play,
    System,
    Installed,
}

const GROUPS: [(Group, &str); 6] = [
    (Group::All, "All"),
    (Group::Work, "Work"),
    (Group::Create, "Create"),
    (Group::Play, "Play"),
    (Group::System, "System"),
    (Group::Installed, "Installed"),
];

fn group_of(kind: Option<AppKind>) -> Group {
    match kind {
        None => Group::Installed,
        Some(
            AppKind::Files | AppKind::Notes | AppKind::Calculator | AppKind::Clock | AppKind::Nebula | AppKind::Firefox,
        ) => Group::Work,
        Some(AppKind::Studio | AppKind::Paint | AppKind::Preview) => Group::Create,
        Some(AppKind::Doom) => Group::Play,
        Some(_) => Group::System,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hover {
    Tile(usize),
    Group(usize),
    Page(usize),
    Power,
}

#[derive(Clone)]
struct Entry {
    kind: Option<AppKind>,
    name: String,
    path: String,
    icon: Icon,
}

pub struct Launcher {
    apps: Vec<Entry>,
    query: String,
    group: Group,
    hover: Option<Hover>,
    page: usize,
}

impl Launcher {
    fn matches(&self) -> Vec<&Entry> {
        let q = self.query.to_ascii_lowercase();
        self.apps
            .iter()
            .filter(|a| self.group == Group::All || group_of(a.kind) == self.group)
            .filter(|a| a.name.to_ascii_lowercase().contains(q.as_str()))
            .collect()
    }

    fn results(&self) -> Vec<&Entry> {
        let per = (COLS * ROWS) as usize;
        self.matches().into_iter().skip(self.page * per).take(per).collect()
    }

    fn pages(&self) -> usize {
        self.matches().len().div_ceil((COLS * ROWS) as usize).max(1)
    }
}

impl Desktop {
    fn shoal_search(&self) -> Rect {
        Rect::new((self.w - 440) / 2, MENUBAR_H + 30, 440, 44)
    }

    fn shoal_groups(&self) -> Vec<Rect> {
        let f = theme::ui_bold(13);
        let widths: Vec<i32> = GROUPS.iter().map(|(_, n)| f.width(n) + 32).collect();
        let total = widths.iter().sum::<i32>() + 8 * (GROUPS.len() as i32 - 1);
        let mut x = (self.w - total) / 2;
        let y = self.shoal_search().bottom() + 18;
        widths
            .into_iter()
            .map(|w| {
                let r = Rect::new(x, y, w, 32);
                x += w + 8;
                r
            })
            .collect()
    }

    fn shoal_tiles(&self, n: usize) -> Vec<Rect> {
        let x0 = (self.w - COLS * TILE_W) / 2;
        let y0 = self.shoal_search().bottom() + 80;
        (0..n as i32).map(|i| Rect::new(x0 + (i % COLS) * TILE_W, y0 + (i / COLS) * TILE_H, TILE_W, TILE_H)).collect()
    }

    fn shoal_dots(&self, pages: usize) -> Vec<Rect> {
        if pages < 2 {
            return Vec::new();
        }
        let y = self.shoal_search().bottom() + 80 + ROWS * TILE_H + 8;
        let x0 = self.w / 2 - (pages as i32 * 20) / 2;
        (0..pages as i32).map(|i| Rect::new(x0 + i * 20, y, 20, 20)).collect()
    }

    fn shoal_power(&self) -> Rect {
        Rect::new(self.w - 64, MENUBAR_H + 34, 36, 36)
    }

    pub fn toggle_launcher(&mut self) {
        if self.launcher.take().is_none() {
            let mut apps: Vec<Entry> = CATALOG
                .iter()
                .filter(|a| a.listed)
                .map(|a| Entry {
                    kind: Some(a.kind),
                    // The full name is for menus; the grid wants a short one.
                    name: String::from(if a.kind == AppKind::About { "About" } else { a.name }),
                    path: String::from(a.path),
                    icon: a.icon,
                })
                .collect();
            apps.sort_by(|a, b| a.name.cmp(&b.name));
            apps.extend(crate::gui::installed::list().into_iter().map(|a| Entry {
                kind: None,
                name: a.manifest.name,
                path: a.path,
                icon: Icon::Aurora,
            }));
            self.launcher = Some(Launcher { apps, query: String::new(), group: Group::All, hover: None, page: 0 });
            self.shoal_backdrop = None;
            self.close_menu();
            self.current = None;
        }
        self.damage_all();
    }

    fn shoal_open(&mut self, entry: Entry) {
        self.toggle_launcher();
        match entry.kind {
            Some(kind) => self.open(kind),
            None => self.spawn_program(&entry.path, &[]),
        }
    }

    pub(super) fn launcher_key(&mut self, k: &KeyEvent) {
        if !k.pressed {
            return;
        }
        let Some(l) = self.launcher.as_mut() else { return };
        match k.code {
            KeyCode::Escape => {
                if l.query.is_empty() {
                    self.toggle_launcher();
                    return;
                }
                l.query.clear();
                l.page = 0;
            }
            KeyCode::Enter => {
                if let Some(first) = l.results().first().map(|e| (*e).clone()) {
                    self.shoal_open(first);
                }
                return;
            }
            KeyCode::Backspace => {
                l.query.pop();
                l.page = 0;
            }
            KeyCode::Left => l.page = l.page.saturating_sub(1),
            KeyCode::Right => l.page = (l.page + 1).min(l.pages() - 1),
            _ => match k.ch {
                Some(c) if !c.is_control() && !k.mods.ctrl && l.query.len() < 32 => {
                    l.query.push(c);
                    l.page = 0;
                }
                _ => return,
            },
        }
        self.damage_all();
    }

    /// A click while Shoal is open. True if handled.
    pub(super) fn shoal_press(&mut self, x: i32, y: i32) -> bool {
        let Some(l) = self.launcher.as_ref() else { return false };
        let results: Vec<Entry> = l.results().into_iter().cloned().collect();
        let pages = l.pages();
        if let Some(i) = self.shoal_tiles(results.len()).iter().position(|r| r.contains(x, y)) {
            self.shoal_open(results[i].clone());
            return true;
        }
        if let Some(i) = self.shoal_groups().iter().position(|r| r.contains(x, y)) {
            if let Some(l) = self.launcher.as_mut() {
                l.group = GROUPS[i].0;
                l.page = 0;
            }
            self.damage_all();
            return true;
        }
        if let Some(i) = self.shoal_dots(pages).iter().position(|r| r.contains(x, y)) {
            if let Some(l) = self.launcher.as_mut() {
                l.page = i;
            }
            self.damage_all();
            return true;
        }
        if self.shoal_power().contains(x, y) {
            self.toggle_launcher();
            self.open(AppKind::Power);
            return true;
        }
        if self.shoal_search().contains(x, y) {
            return true;
        }
        // Harbor stays usable: its Shoal icon closes, anything else closes and opens.
        let (dock, items) = self.dock_layout();
        if dock.inset(-4).contains(x, y) {
            let on_shoal = items.iter().any(|(i, r)| *i == DockItem::Launcher && r.inset(-4).contains(x, y));
            self.toggle_launcher();
            if !on_shoal {
                self.harbor_press(x, y);
            }
            return true;
        }
        // Anywhere else (empty water) closes Shoal; the bar still works.
        self.toggle_launcher();
        y >= MENUBAR_H
    }

    pub(super) fn shoal_hover(&mut self, x: i32, y: i32) {
        let Some(l) = self.launcher.as_ref() else { return };
        let n = l.results().len();
        let pages = l.pages();
        let h = if let Some(i) = self.shoal_tiles(n).iter().position(|r| r.contains(x, y)) {
            Some(Hover::Tile(i))
        } else if let Some(i) = self.shoal_groups().iter().position(|r| r.contains(x, y)) {
            Some(Hover::Group(i))
        } else if let Some(i) = self.shoal_dots(pages).iter().position(|r| r.contains(x, y)) {
            Some(Hover::Page(i))
        } else if self.shoal_power().contains(x, y) {
            Some(Hover::Power)
        } else {
            None
        };
        let Some(l) = self.launcher.as_mut() else { return };
        if h != l.hover {
            let old = l.hover;
            l.hover = h;
            for hv in [old, h].into_iter().flatten() {
                let r = match hv {
                    Hover::Tile(i) => self.shoal_tiles(i + 1)[i],
                    Hover::Group(i) => self.shoal_groups()[i],
                    Hover::Page(i) => self.shoal_dots(pages)[i],
                    Hover::Power => self.shoal_power(),
                };
                self.damage(r.inset(-6));
            }
        }
    }

    pub(super) fn paint_shoal(&mut self, cv: &mut Canvas) {
        if self.launcher.is_none() {
            self.shoal_backdrop = None;
            return;
        }
        let t = theme::current();
        let screen = Rect::new(0, 0, self.w, self.h);
        match &self.shoal_backdrop {
            Some(bg) if bg.len() == cv.buf.len() => cv.blit_same(bg, cv.clip),
            _ => {
                cv.frosted(screen, 0, if t.dark { 0xB00A_0E1E } else { 0x99F4_F6FC });
                if cv.clip == screen {
                    self.shoal_backdrop = Some(cv.buf.to_vec());
                }
            }
        }
        let Some(l) = &self.launcher else { return };

        // Account (left) and power (right).
        if let Some(acc) = &self.account {
            let av = Rect::new(28, MENUBAR_H + 32, 40, 40);
            cv.fill_round_rect_dgradient(av, 20, theme::accent(), theme::ACCENT_2);
            cv.text_centered(av, &acc.initials(), theme::ui_bold(15), 0xFFFF_FFFF);
            cv.text(av.right() + 12, av.y + 18, &acc.full, theme::ui_bold(14), t.text);
            cv.text(av.right() + 12, av.y + 36, "WaveOS Aurora", theme::ui(12), t.text_secondary);
        }
        let pw = self.shoal_power();
        cv.fill_round_rect(pw, 18, if l.hover == Some(Hover::Power) { t.hover } else { with_alpha(t.text, 0x10) });
        let bg = cv.pixel(pw.x + pw.w / 2, pw.y);
        let (cx, cy) = pw.center();
        icons::power(cv, cx, cy, 16, t.text, bg);

        // Search.
        let s = self.shoal_search();
        cv.shadow(s, 22, 16, 4, t.shadow / 4);
        cv.fill_round_rect(s, 22, if t.dark { 0xFF1A_2036 } else { 0xFFFF_FFFF });
        cv.stroke_round_rect(s.inset(-1), 23, with_alpha(theme::accent(), 0x90));
        super::bar::paint_seek(cv, s.x + 26, s.y + s.h / 2, t.text_secondary);
        let f = theme::ui(15);
        let tx = s.x + 46;
        let baseline = s.y + s.h / 2 + 6;
        if l.query.is_empty() {
            cv.text(tx, baseline, "Search apps", f, t.text_secondary);
            cv.fill_rect(Rect::new(tx - 2, s.y + 12, 2, s.h - 24), theme::accent());
        } else {
            let w = cv.text_clipped(tx, baseline, &l.query, f, t.text, s.w - 70);
            cv.fill_rect(Rect::new(tx + w + 1, s.y + 12, 2, s.h - 24), theme::accent());
        }

        // Groups.
        let gf = theme::ui_bold(13);
        for (i, ((g, name), r)) in GROUPS.iter().zip(self.shoal_groups()).enumerate() {
            let on = *g == l.group;
            if on {
                cv.fill_round_rect(r, 16, theme::accent());
                cv.text_centered(r, name, gf, 0xFFFF_FFFF);
            } else {
                let bg = if l.hover == Some(Hover::Group(i)) { t.hover } else { with_alpha(t.text, 0x0C) };
                cv.fill_round_rect(r, 16, bg);
                cv.text_centered(r, name, gf, t.text);
            }
        }

        // The grid.
        let results = l.results();
        if results.is_empty() {
            let msg = if l.query.is_empty() { "Nothing here yet" } else { "No apps match" };
            let f = theme::ui(15);
            let y = self.shoal_search().bottom() + 180;
            cv.text((self.w - f.width(msg)) / 2, y, msg, f, t.text_secondary);
        }
        let first = !l.query.is_empty();
        for (i, (app, r)) in results.iter().zip(self.shoal_tiles(results.len())).enumerate() {
            let hovered = l.hover == Some(Hover::Tile(i));
            if hovered || (first && i == 0) {
                cv.fill_round_rect(r.inset(6), 20, if hovered { t.hover } else { with_alpha(theme::accent(), 0x26) });
            }
            let lift = if hovered { 3 } else { 0 };
            icons::draw(cv, app.icon, Rect::new(r.x + (r.w - ICON) / 2, r.y + 14 - lift, ICON, ICON));
            let f = theme::ui_bold(13);
            let w = f.width(&app.name).min(r.w - 12);
            cv.text_clipped(r.x + (r.w - w) / 2, r.y + 108, &app.name, f, t.text, r.w - 12);
        }

        // Pages.
        for (i, r) in self.shoal_dots(l.pages()).into_iter().enumerate() {
            let (cx, cy) = r.center();
            let c = if i == l.page {
                theme::accent()
            } else if l.hover == Some(Hover::Page(i)) {
                with_alpha(t.text, 0x80)
            } else {
                with_alpha(t.text, 0x40)
            };
            cv.fill_circle(cx, cy, 4, c);
        }
    }
}
