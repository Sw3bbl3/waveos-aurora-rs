//! Shell chrome: the menus that drop from the left bubble (the WayV menu
//! and the app's window menu).

use super::Desktop;
use crate::gui::apps::AppKind;
use crate::gui::canvas::{mix, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme::{self, MENUBAR_H};
use alloc::vec::Vec;

const MENU_ITEM_H: i32 = 26;

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    About,
    Settings,
    Launcher,
    Power,
    Sleep,
    Lock,
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
    /// The app's window menu (as opposed to the WayV menu).
    pub(super) fn is_app_menu(&self) -> bool {
        self.kind == MenuKind::App
    }

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

impl Desktop {
    // --------------------------------------------------------------- menus

    fn menubar_targets(&self) -> [(MenuKind, Rect); 2] {
        let (_, mark, app) = self.left_bubble();
        [(MenuKind::Aurora, mark), (MenuKind::App, app)]
    }

    fn open_menu(&mut self, kind: MenuKind, x: i32) {
        let items = match kind {
            MenuKind::Aurora => alloc::vec![
                item("About This Computer", "", MenuAction::About),
                item("Settings…", "", MenuAction::Settings),
                item("Learn — WaveOS Guide", "", MenuAction::Open(AppKind::Learn)),
                SEPARATOR,
                item("Shoal", "Super", MenuAction::Launcher),
                SEPARATOR,
                item("Lock Screen", "Super L", MenuAction::Lock),
                item("Sleep", "", MenuAction::Sleep),
                item("Restart…", "", MenuAction::Power),
                item("Shut Down…", "", MenuAction::Power),
            ],
            MenuKind::App if self.focused().is_some() => alloc::vec![
                item("Shrink", "Super ↓", MenuAction::Minimize),
                item("Expand", "Super ↑", MenuAction::Zoom),
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
            MenuAction::Lock => self.lock(),
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
            return self.shoal_press(x, y);
        }


        if let Some((k, r)) = self.menubar_targets().into_iter().find(|(_, r)| r.contains(x, y)) {
            self.open_menu(k, r.x);
            return true;
        }
        if y < MENUBAR_H {
            return true;
        }
        self.harbor_press(x, y)
    }

    pub(super) fn update_shell_hover(&mut self, x: i32, y: i32) {
        self.harbor_hover(x, y);
        // Menus.
        if let Some(m) = self.menu.as_mut() {
            let h = m.item_at(x, y);
            if h != m.hover {
                m.hover = h;
                let r = m.rect();
                self.damage(r);
            }
        }
        if self.launcher.is_some() {
            self.shoal_hover(x, y);
        }
    }

    // ------------------------------------------------------------ painting

    /// The open menu, above the bar.
    pub(super) fn paint_menu(&self, cv: &mut Canvas) {
        let t = theme::current();
        let clip = cv.clip;
        if let Some(m) = &self.menu {
            let r = m.rect();
            if r.inset(-24).intersects(&clip) {
                cv.shadow(r, 16, 24, 8, t.shadow * 2 / 3);
                cv.frosted(r, 16, t.panel_tint);
                cv.stroke_round_rect(r, 16, if t.dark { 0x24FF_FFFF } else { 0x80FF_FFFF });
                let f = theme::ui(13);
                for (i, (it, ir)) in m.items.iter().zip(m.item_rects()).enumerate() {
                    if it.action.is_none() {
                        cv.fill_rect(Rect::new(ir.x + 6, ir.y + 4, ir.w - 12, 1), t.separator);
                        continue;
                    }
                    let hovered = m.hover == Some(i);
                    if hovered {
                        cv.fill_round_rect(ir, 10, mix(theme::accent(), 0xFFFF_FFFF, 20));
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
