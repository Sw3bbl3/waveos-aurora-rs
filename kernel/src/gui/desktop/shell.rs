//! Shell chrome: the menu bar, the dock, the launcher and dropdown menus.

use super::Desktop;
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::drivers::rtc;
use crate::gui::apps::{self, AppKind, CATALOG};
use crate::gui::canvas::{mix, with_alpha, Canvas};
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::theme::{self, DOCK_MARGIN, DOCK_PAD, MENUBAR_H};
use crate::gui::widgets;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

const DOCK_GAP: i32 = 10;
const SEPARATOR_W: i32 = 13;
const LAUNCHER_W: i32 = 620;
const LAUNCHER_H: i32 = 470;
const TILE: i32 = 92;
const MENU_ITEM_H: i32 = 26;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DockItem {
    Launcher,
    App(AppKind),
    External(u32),
    Separator,
    Trash,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    About,
    Settings,
    Launcher,
    Power,
    Sleep,
    Minimize,
    Zoom,
    Close,
    Open(AppKind),
}

struct MenuItem {
    label: &'static str,
    shortcut: &'static str,
    action: Option<MenuAction>,
}

const fn item(label: &'static str, shortcut: &'static str, action: MenuAction) -> MenuItem {
    MenuItem { label, shortcut, action: Some(action) }
}

const SEPARATOR: MenuItem = MenuItem { label: "", shortcut: "", action: None };

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuKind {
    Aurora,
    App,
}

pub struct Menu {
    kind: MenuKind,
    x: i32,
    items: Vec<MenuItem>,
    hover: Option<usize>,
}

impl Menu {
    fn rect(&self) -> Rect {
        let f = theme::ui(13);
        let w = self.items.iter().map(|i| f.width(i.label) + f.width(i.shortcut) + 64).max().unwrap_or(160).max(200);
        let h: i32 = self.items.iter().map(|i| if i.action.is_some() { MENU_ITEM_H } else { 9 }).sum();
        Rect::new(self.x, MENUBAR_H + 4, w, h + 12)
    }

    fn item_rects(&self) -> Vec<Rect> {
        let r = self.rect();
        let mut y = r.y + 6;
        self.items
            .iter()
            .map(|i| {
                let h = if i.action.is_some() { MENU_ITEM_H } else { 9 };
                let ir = Rect::new(r.x + 6, y, r.w - 12, h);
                y += h;
                ir
            })
            .collect()
    }

    fn item_at(&self, x: i32, y: i32) -> Option<usize> {
        self.item_rects().iter().position(|r| r.contains(x, y)).filter(|&i| self.items[i].action.is_some())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LauncherHover {
    Tile(usize),
    Power,
}

#[derive(Clone)]
struct LaunchApp {
    kind: Option<AppKind>,
    name: String,
    path: String,
    icon: Icon,
}

pub struct Launcher {
    apps: Vec<LaunchApp>,
    query: String,
    hover: Option<LauncherHover>,
    page: usize,
}

impl Launcher {
    fn results(&self) -> Vec<&LaunchApp> {
        let q = self.query.to_ascii_lowercase();
        self.apps
            .iter()
            .filter(|a| a.name.to_ascii_lowercase().contains(q.as_str()))
            .skip(self.page * 18)
            .take(18)
            .collect()
    }
    fn pages(&self) -> usize {
        let q = self.query.to_ascii_lowercase();
        self.apps.iter().filter(|a| a.name.to_ascii_lowercase().contains(q.as_str())).count().div_ceil(18).max(1)
    }
}

/// The menu-bar clock, as the Date & Time preferences ask.
pub fn clock_text() -> String {
    use crate::gui::prefs::get_bool;
    use aurora_abi::pref;
    let d = rtc::now();
    let secs = if get_bool(pref::CLOCK_SECONDS) { format!(":{:02}", d.second) } else { String::new() };
    let time = if get_bool(pref::CLOCK_24H) {
        format!("{:02}:{:02}{}", d.hour, d.minute, secs)
    } else {
        let (h12, ampm) = match d.hour {
            0 => (12, "AM"),
            1..=11 => (d.hour, "AM"),
            12 => (12, "PM"),
            h => (h - 12, "PM"),
        };
        format!("{}:{:02}{} {}", h12, d.minute, secs, ampm)
    };
    if get_bool(pref::CLOCK_DATE) {
        format!("{} {} {}   {}", d.weekday_name(), d.month_name(), d.day, time)
    } else {
        time
    }
}

impl Desktop {
    // ---------------------------------------------------------------- dock

    fn dock_items(&self) -> Vec<DockItem> {
        let mut v = alloc::vec![DockItem::Launcher];
        v.extend(CATALOG.iter().filter(|a| a.pinned).map(|a| DockItem::App(a.kind)));
        let mut extra: Vec<AppKind> = Vec::new();
        for w in &self.windows {
            let k = w.app.kind();
            if !apps::info(k).pinned
                && !matches!(k, AppKind::Power | AppKind::Crash | AppKind::Other | AppKind::DisplayConfirm)
                && !extra.contains(&k)
            {
                extra.push(k);
            }
        }
        if !extra.is_empty() {
            v.push(DockItem::Separator);
            v.extend(extra.into_iter().map(DockItem::App));
        }
        v.push(DockItem::Separator);
        v.extend(self.windows.iter().filter(|w| w.app.kind() == AppKind::Other).map(|w| DockItem::External(w.id)));
        v.push(DockItem::Trash);
        v
    }

    pub(super) fn dock_layout(&self) -> (Rect, Vec<(DockItem, Rect)>) {
        let items = self.dock_items();
        let width: i32 = items
            .iter()
            .map(|i| if *i == DockItem::Separator { SEPARATOR_W } else { theme::dock_icon_size() })
            .sum::<i32>()
            + DOCK_GAP * (items.len() as i32 - 1)
            + 2 * DOCK_PAD
            + 4;
        let hidden = theme::dock_autohide() && !self.dock_revealed;
        let dock = Rect::new(
            (self.w - width) / 2,
            if hidden { self.h + 48 } else { self.h - DOCK_MARGIN - theme::dock_height() },
            width,
            theme::dock_height(),
        );
        let mut x = dock.x + DOCK_PAD + 2;
        let y = dock.y + DOCK_PAD;
        let rects = items
            .into_iter()
            .map(|i| {
                let w = if i == DockItem::Separator { SEPARATOR_W } else { theme::dock_icon_size() };
                let r = Rect::new(x, y, w, theme::dock_icon_size());
                x += w + DOCK_GAP;
                (i, r)
            })
            .collect();
        (dock, rects)
    }

    pub(super) fn dock_rect(&self) -> Rect {
        self.dock_layout().0
    }

    fn is_running(&self, kind: AppKind) -> bool {
        self.windows.iter().any(|w| w.app.kind() == kind)
    }

    fn dock_click(&mut self, item: DockItem) {
        match item {
            DockItem::External(id) => {
                if self.focused_id() == Some(id) {
                    self.minimize(id);
                } else {
                    self.raise(id);
                }
            }
            DockItem::Launcher => self.toggle_launcher(),
            DockItem::App(kind) => {
                let focused_kind = self.focused().map(|w| w.app.kind());
                let topmost = self.windows.iter().rev().find(|w| w.app.kind() == kind).map(|w| (w.id, w.minimized));
                match topmost {
                    Some((id, false)) if focused_kind == Some(kind) => self.minimize(id),
                    Some((id, _)) => self.raise(id),
                    None => self.open(kind),
                }
            }
            DockItem::Trash => {
                let files = apps::info(AppKind::Files).path;
                self.spawn_program(files, &["/Trash"]);
            }
            DockItem::Separator => {}
        }
    }

    // ------------------------------------------------------------ launcher

    fn launcher_rect(&self) -> Rect {
        let dock = self.dock_rect();
        let h = LAUNCHER_H.min(dock.y - MENUBAR_H - 24);
        Rect::new((self.w - LAUNCHER_W) / 2, dock.y - 14 - h, LAUNCHER_W, h)
    }

    fn launcher_tiles(&self, n: usize) -> Vec<Rect> {
        let p = self.launcher_rect();
        let cols = 6;
        let gx = p.x + (p.w - cols * TILE) / 2;
        (0..n as i32)
            .map(|i| Rect::new(gx + (i % cols) * TILE, p.y + 118 + (i / cols) * (TILE + 8), TILE, TILE))
            .collect()
    }

    fn launcher_power(&self) -> Rect {
        let p = self.launcher_rect();
        Rect::new(p.right() - 58, p.bottom() - 50, 36, 36)
    }

    pub fn toggle_launcher(&mut self) {
        let r = self.launcher_rect();
        if self.launcher.take().is_none() {
            let mut entries: Vec<LaunchApp> = CATALOG
                .iter()
                .filter(|a| a.listed)
                .map(|a| LaunchApp {
                    kind: Some(a.kind),
                    name: String::from(a.name),
                    path: String::from(a.path),
                    icon: a.icon,
                })
                .collect();
            entries.extend(crate::gui::installed::list().into_iter().map(|a| LaunchApp {
                kind: None,
                name: a.manifest.name,
                path: a.path,
                icon: Icon::Aurora,
            }));
            self.launcher = Some(Launcher { apps: entries, query: String::new(), hover: None, page: 0 });
            self.close_menu();
        }
        self.damage(r.inset(-50));
        let d = self.dock_rect();
        self.damage(d);
    }

    fn launch(&mut self, kind: AppKind) {
        self.toggle_launcher();
        self.open(kind);
    }

    pub(super) fn launcher_key(&mut self, k: &KeyEvent) {
        if !k.pressed {
            return;
        }
        let Some(l) = self.launcher.as_mut() else { return };
        match k.code {
            KeyCode::Escape => {
                self.toggle_launcher();
                return;
            }
            KeyCode::Enter => {
                if let Some(first) = l.results().first() {
                    let app = (*first).clone();
                    self.toggle_launcher();
                    if let Some(kind) = app.kind {
                        self.open(kind);
                    } else {
                        self.spawn_program(&app.path, &[]);
                    }
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
        let r = self.launcher_rect();
        self.damage(r);
    }

    fn launcher_click(&mut self, x: i32, y: i32) {
        let p = self.launcher_rect();
        if let Some(l) = self.launcher.as_mut() {
            if Rect::new(p.right() - 210, p.bottom() - 50, 40, 36).contains(x, y) {
                l.page = l.page.saturating_sub(1);
                self.damage(p);
                return;
            }
            if Rect::new(p.right() - 108, p.bottom() - 50, 40, 36).contains(x, y) {
                l.page = (l.page + 1).min(l.pages() - 1);
                self.damage(p);
                return;
            }
        }
        let Some(l) = self.launcher.as_ref() else { return };
        let results = l.results();
        if let Some(i) = self.launcher_tiles(results.len()).iter().position(|r| r.contains(x, y)) {
            let app = results[i].clone();
            self.toggle_launcher();
            if let Some(kind) = app.kind {
                self.open(kind);
            } else {
                self.spawn_program(&app.path, &[]);
            }
        } else if self.launcher_power().contains(x, y) {
            self.launch(AppKind::Power);
        }
    }

    // --------------------------------------------------------------- menus

    fn menubar_targets(&self) -> [(MenuKind, Rect); 2] {
        let name = self.app_name();
        let w = theme::ui_bold(13).width(&name);
        [
            (MenuKind::Aurora, Rect::new(6, 2, 38, MENUBAR_H - 4)),
            (MenuKind::App, Rect::new(46, 2, w + 20, MENUBAR_H - 4)),
        ]
    }

    fn app_name(&self) -> String {
        match self.focused() {
            Some(w) if w.app.kind() == AppKind::Other => w.app.title(),
            Some(w) => String::from(apps::info(w.app.kind()).name),
            None => String::from("Desktop"),
        }
    }

    fn open_menu(&mut self, kind: MenuKind, x: i32) {
        let items = match kind {
            MenuKind::Aurora => alloc::vec![
                item("About WaveOS Aurora", "", MenuAction::About),
                item("Settings…", "", MenuAction::Settings),
                SEPARATOR,
                item("Launcher", "Super", MenuAction::Launcher),
                SEPARATOR,
                item("Sleep", "", MenuAction::Sleep),
                item("Restart…", "", MenuAction::Power),
                item("Shut Down…", "", MenuAction::Power),
            ],
            MenuKind::App if self.focused().is_some() => alloc::vec![
                item("Minimize", "", MenuAction::Minimize),
                item("Zoom", "", MenuAction::Zoom),
                SEPARATOR,
                item("Close Window", "Ctrl+W", MenuAction::Close),
            ],
            MenuKind::App => alloc::vec![
                item("Open Files", "", MenuAction::Open(AppKind::Files)),
                item("Open Terminal", "", MenuAction::Open(AppKind::Terminal)),
                item("Open Notes", "", MenuAction::Open(AppKind::Notes)),
            ],
        };
        self.menu = Some(Menu { kind, x, items, hover: None });
        let r = self.menu.as_ref().unwrap().rect();
        self.damage(r.inset(-24));
        self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
    }

    pub(super) fn close_menu(&mut self) {
        if let Some(m) = self.menu.take() {
            self.damage(m.rect().inset(-24));
            self.damage(Rect::new(0, 0, self.w, MENUBAR_H));
        }
    }

    fn run_menu_action(&mut self, a: MenuAction) {
        let focused = self.focused_id();
        match a {
            MenuAction::About => self.open(AppKind::About),
            MenuAction::Settings => self.open(AppKind::Settings),
            MenuAction::Launcher => self.toggle_launcher(),
            MenuAction::Power => self.open(AppKind::Power),
            MenuAction::Sleep => crate::power::sleep(),
            MenuAction::Open(k) => self.open(k),
            MenuAction::Minimize => {
                if let Some(id) = focused {
                    self.minimize(id)
                }
            }
            MenuAction::Zoom => {
                if let Some(id) = focused {
                    self.toggle_zoom(id)
                }
            }
            MenuAction::Close => {
                if let Some(id) = focused {
                    self.close(id)
                }
            }
        }
    }

    // --------------------------------------------------------------- input

    /// Returns true if the shell consumed the click.
    pub(super) fn shell_press(&mut self, x: i32, y: i32) -> bool {
        if let Some(m) = self.menu.as_ref() {
            let kind = m.kind;
            if m.rect().contains(x, y) {
                let action = m.item_at(x, y).and_then(|i| m.items[i].action);
                if let Some(a) = action {
                    self.close_menu();
                    self.run_menu_action(a);
                }
                return true;
            }
            self.close_menu();
            let other = self.menubar_targets().into_iter().find(|(k, r)| r.contains(x, y) && *k != kind);
            if let Some((k, r)) = other {
                self.open_menu(k, r.x);
            }
            return true;
        }

        if self.launcher.is_some() {
            if self.launcher_rect().contains(x, y) {
                self.launcher_click(x, y);
                return true;
            }
            let (_, items) = self.dock_layout();
            let on_launcher_icon = items.iter().any(|(i, r)| *i == DockItem::Launcher && r.contains(x, y));
            self.toggle_launcher();
            return on_launcher_icon || y < MENUBAR_H || self.dock_rect().contains(x, y);
        }

        if y < MENUBAR_H {
            if let Some((k, r)) = self.menubar_targets().into_iter().find(|(_, r)| r.contains(x, y)) {
                self.open_menu(k, r.x);
            }
            return true;
        }

        let (dock, items) = self.dock_layout();
        if dock.contains(x, y) {
            if let Some((item, _)) = items.into_iter().find(|(_, r)| r.contains(x, y)) {
                self.dock_click(item);
            }
            return true;
        }
        false
    }

    pub(super) fn update_shell_hover(&mut self, x: i32, y: i32) {
        // Dock.
        let reveal = y >= self.h - theme::dock_height() - DOCK_MARGIN - 16;
        if self.dock_revealed != reveal {
            self.dock_revealed = reveal;
            if theme::dock_autohide() {
                self.damage_all();
            }
        }
        let (dock, items) = self.dock_layout();
        let dh = items.iter().position(|(i, r)| *i != DockItem::Separator && r.inset(-DOCK_GAP / 2).contains(x, y));
        if dh != self.dock_hover {
            self.dock_hover = dh;
            self.damage(dock.inset(-44));
        }
        // Menus.
        if let Some(m) = self.menu.as_mut() {
            let h = m.item_at(x, y);
            if h != m.hover {
                m.hover = h;
                let r = m.rect();
                self.damage(r);
            }
        }
        // Launcher.
        if self.launcher.is_some() {
            let n = self.launcher.as_ref().unwrap().results().len();
            let tiles = self.launcher_tiles(n);
            let power = self.launcher_power();
            let h = if let Some(i) = tiles.iter().position(|r| r.contains(x, y)) {
                Some(LauncherHover::Tile(i))
            } else if power.contains(x, y) {
                Some(LauncherHover::Power)
            } else {
                None
            };
            let l = self.launcher.as_mut().unwrap();
            if h != l.hover {
                l.hover = h;
                let r = self.launcher_rect();
                self.damage(r);
            }
        }
    }

    // ------------------------------------------------------------ painting

    pub(super) fn paint_shell(&mut self, cv: &mut Canvas) {
        let t = theme::current();
        let clip = cv.clip;

        // Dock.
        let (dock, items) = self.dock_layout();
        if dock.inset(-44).intersects(&clip) {
            cv.shadow(dock, 20, 26, 8, t.shadow * 2 / 3);
            cv.glass(&self.blurred, dock, 20, t.glass_tint);
            cv.stroke_round_rect(dock, 20, if t.dark { 0x30FF_FFFF } else { 0x70FF_FFFF });
            for (idx, (item, r)) in items.iter().enumerate() {
                let hovered = self.dock_hover == Some(idx);
                let r = if hovered { r.offset(0, -4) } else { *r };
                match item {
                    DockItem::Separator => cv.fill_rect(
                        Rect::new(r.x + SEPARATOR_W / 2, r.y + 6, 1, r.h - 12),
                        with_alpha(t.text_on_glass, 0x40),
                    ),
                    DockItem::Launcher => {
                        icons::draw(cv, Icon::Launcher, r);
                        if self.launcher.is_some() {
                            cv.fill_circle(r.x + r.w / 2, dock.bottom() - 5, 2, t.text_on_glass);
                        }
                    }
                    DockItem::App(kind) => {
                        icons::draw(cv, apps::info(*kind).icon, r);
                        if self.is_running(*kind) {
                            cv.fill_circle(r.x + r.w / 2, dock.bottom() - 5, 2, t.text_on_glass);
                        }
                    }
                    DockItem::External(_) => {
                        icons::draw(cv, Icon::Aurora, r);
                        cv.fill_circle(r.x + r.w / 2, dock.bottom() - 5, 2, t.text_on_glass);
                    }
                    DockItem::Trash => icons::draw(cv, if self.trash_full { Icon::TrashFull } else { Icon::Trash }, r),
                }
            }
            // Tooltip.
            if let Some((item, r)) = self.dock_hover.and_then(|i| items.get(i)) {
                let external_title = if let DockItem::External(id) = item {
                    self.windows.iter().find(|w| w.id == *id).map(|w| w.app.title()).unwrap_or_default()
                } else {
                    String::new()
                };
                let label = match item {
                    DockItem::External(_) => external_title.as_str(),
                    DockItem::Launcher => "Launcher",
                    DockItem::App(k) => apps::info(*k).name,
                    DockItem::Trash => "Trash",
                    DockItem::Separator => "",
                };
                let f = theme::ui(12);
                let w = f.width(label) + 20;
                let tip = Rect::new(r.x + r.w / 2 - w / 2, dock.y - 34, w, 24);
                cv.fill_round_rect(tip, 8, if t.dark { 0xF03A_3A46 } else { 0xE820_2028 });
                cv.text_centered(tip, label, f, 0xFFFF_FFFF);
            }
        }

        // Menu bar.
        let bar = Rect::new(0, 0, self.w, MENUBAR_H);
        if bar.intersects(&clip) {
            cv.glass(&self.blurred, bar, 0, t.glass_tint);
            cv.fill_rect(Rect::new(0, MENUBAR_H - 1, self.w, 1), t.separator);
            let targets = self.menubar_targets();
            if let Some(m) = &self.menu {
                if let Some((_, r)) = targets.iter().find(|(k, _)| *k == m.kind) {
                    cv.fill_round_rect(*r, 6, t.hover);
                }
            }
            icons::wave(cv, Rect::new(15, 9, 22, 12), 2, 2, t.text_on_glass);
            let name = self.app_name();
            cv.text(56, 20, &name, theme::ui_bold(13), t.text_on_glass);
            if self.clock.is_empty() {
                self.clock = clock_text();
            }
            let f = theme::ui(13);
            cv.text(self.w - 18 - f.width(&self.clock), 20, &self.clock, f, t.text_on_glass);
            self.paint_volume_icon(cv);
            self.paint_battery_item(cv);
            self.paint_network_item(cv);
            self.paint_controls_icon(cv);
        }

        // Launcher.
        if let Some(l) = &self.launcher {
            let p = self.launcher_rect();
            if p.inset(-50).intersects(&clip) {
                cv.shadow(p, 18, 40, 14, t.shadow);
                cv.glass(&self.blurred, p, 18, t.panel_tint);
                cv.stroke_round_rect(p, 18, if t.dark { 0x30FF_FFFF } else { 0x80FF_FFFF });
                let field = Rect::new(p.x + 24, p.y + 22, p.w - 48, 40);
                widgets::text_field(cv, field, &l.query, "Search apps", true, true);
                let results = l.results();
                let header = if l.query.is_empty() { "All apps" } else { "Results" };
                cv.text(p.x + 28, p.y + 100, header, theme::ui_bold(13), t.text);
                if results.is_empty() {
                    let msg = "No apps match your search";
                    let f = theme::ui(13);
                    cv.text(p.x + (p.w - f.width(msg)) / 2, p.y + 170, msg, f, t.text_secondary);
                }
                for (i, (app, r)) in results.iter().zip(self.launcher_tiles(results.len())).enumerate() {
                    if l.hover == Some(LauncherHover::Tile(i)) {
                        cv.fill_round_rect(r.inset(4), 12, t.hover);
                    }
                    icons::draw(cv, app.icon, Rect::new(r.x + (r.w - 48) / 2, r.y + 10, 48, 48));
                    let f = theme::ui(12);
                    let w = f.width(&app.name).min(r.w - 8);
                    cv.text_clipped(r.x + (r.w - w) / 2, r.y + 76, &app.name, f, t.text, r.w - 8);
                }
                // Footer: user and power.
                let foot_y = p.bottom() - 64;
                cv.fill_rect(Rect::new(p.x + 1, foot_y, p.w - 2, 1), t.separator);
                let av = Rect::new(p.x + 24, foot_y + 16, 32, 32);
                cv.fill_round_rect_dgradient(av, 16, theme::accent(), theme::ACCENT_2);
                cv.text_centered(av, "A", theme::ui_bold(14), 0xFFFF_FFFF);
                cv.text(p.x + 66, foot_y + 37, "Aurora User", theme::ui_bold(13), t.text);
                if l.pages() > 1 {
                    widgets::button(
                        cv,
                        Rect::new(p.right() - 210, p.bottom() - 50, 40, 36),
                        "‹",
                        widgets::ButtonStyle::Secondary,
                        false,
                    );
                    cv.text(
                        p.right() - 164,
                        p.bottom() - 27,
                        &alloc::format!("{}/{}", l.page + 1, l.pages()),
                        theme::ui(12),
                        t.text_secondary,
                    );
                    widgets::button(
                        cv,
                        Rect::new(p.right() - 108, p.bottom() - 50, 40, 36),
                        "›",
                        widgets::ButtonStyle::Secondary,
                        false,
                    );
                }
                let pw = self.launcher_power();
                if l.hover == Some(LauncherHover::Power) {
                    cv.fill_round_rect(pw, 10, t.hover);
                }
                let bg = cv.buf[(pw.y * cv.width + pw.x) as usize];
                let (cx, cy) = pw.center();
                icons::power(cv, cx, cy + 1, 18, t.text, bg);
            }
        }

        // Dropdown menu.
        if let Some(m) = &self.menu {
            let r = m.rect();
            if r.inset(-24).intersects(&clip) {
                cv.shadow(r, 10, 20, 8, t.shadow * 2 / 3);
                cv.fill_round_rect(r, 10, with_alpha(t.window_bg, 0xF6));
                cv.stroke_round_rect(r, 10, t.window_border);
                let f = theme::ui(13);
                for (i, (it, ir)) in m.items.iter().zip(m.item_rects()).enumerate() {
                    if it.action.is_none() {
                        cv.fill_rect(Rect::new(ir.x + 6, ir.y + 4, ir.w - 12, 1), t.separator);
                        continue;
                    }
                    let hovered = m.hover == Some(i);
                    if hovered {
                        cv.fill_round_rect(ir, 6, mix(theme::accent(), 0xFFFF_FFFF, 20));
                    }
                    let fg = if hovered { 0xFFFF_FFFF } else { t.text };
                    cv.text(ir.x + 12, ir.y + 18, it.label, f, fg);
                    if !it.shortcut.is_empty() {
                        let sw = f.width(it.shortcut);
                        cv.text(
                            ir.right() - 12 - sw,
                            ir.y + 18,
                            it.shortcut,
                            f,
                            if hovered { 0xDDFF_FFFF } else { t.text_secondary },
                        );
                    }
                }
            }
        }
    }
}
