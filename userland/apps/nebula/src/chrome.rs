//! The browser around the page: tabs, the toolbar and address field, the
//! find bar, and the page itself (a canvas the engine paints).

use super::*;
use waveui::prelude::*;
use waveui::{Canvas, Image};

#[derive(Clone)]
pub enum Msg {
    Select(usize),
    Close(usize),
    NewTab,
    Reopen,
    Back,
    Forward,
    Reload,
    Home,
    Address(String),
    Go,
    FindQuery(String),
    FindStep(i32),
    FindDone,
    Find,
    Zoom(i32),
    ZoomReset,
    Key(KeyEvent),
    PageKey(KeyEvent),
    Press(i32, i32),
    Hover(i32, i32),
    Leave,
    Scroll(i32),
    Resize(i32, i32),
}

fn address_id(tab: u64) -> (&'static str, u64) {
    ("address", tab)
}

fn find_id(tab: u64) -> (&'static str, u64) {
    ("find", tab)
}

/// Keys the browser itself answers even while the page has focus.
fn browser_key(k: &KeyEvent) -> bool {
    let ctrl = k.mods.ctrl || k.mods.super_key;
    (ctrl && (k.code == KeyCode::Tab || k.ch.is_some_and(|c| "tlfgr[]=+-0123456789w".contains(c.to_ascii_lowercase()))))
        || k.mods.alt && matches!(k.code, KeyCode::Left | KeyCode::Right | KeyCode::Home)
        || k.code == KeyCode::F5
}

impl Browser {
    fn tab_strip(&self) -> View<'_, Msg> {
        let mut strip = HStack::new().spacing(4);
        for (i, tab) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            let dot = if tab.loading() {
                Color::Accent
            } else if active {
                Color::Teal
            } else {
                Color::TertiaryLabel
            };
            strip = strip.push(
                HStack::new()
                    .push(
                        Canvas::new(move |cv, r| {
                            let (x, y) = r.center();
                            cv.fill_circle(x, y, 3, dot.argb());
                        })
                        .size(10, 10),
                    )
                    .push(
                        Text::new(tab.title())
                            .style(TextStyle::Callout)
                            .foreground(if active { Color::Label } else { Color::SecondaryLabel })
                            .grow(1),
                    )
                    .push(
                        Button::symbol(Symbol::Close)
                            .action(Msg::Close(i))
                            .control_size(ControlSize::Small)
                            .help("Close Tab"),
                    )
                    .spacing(6)
                    .padding_insets(Insets { top: 0, leading: 10, bottom: 0, trailing: 2 })
                    .height(30)
                    .background_rounded(if active { Color::ControlBackground } else { Color::Clear }, 8)
                    .on_tap(Msg::Select(i))
                    .max_width(210)
                    .grow(1),
            );
        }
        let count = if self.notice() {
            Text::new("Eight tabs are already open").style(TextStyle::Callout)
        } else {
            Text::new(format!("{} / {} tabs", self.tabs.len(), MAX_TABS))
                .style(TextStyle::Callout)
                .foreground(Color::SecondaryLabel)
        };
        HStack::new()
            .push(strip.grow(1))
            .push(
                Button::symbol(Symbol::Plus)
                    .action(Msg::NewTab)
                    .disabled(self.tabs.len() >= MAX_TABS)
                    .help("New Tab (Ctrl+T)"),
            )
            .push(count)
            .spacing(8)
            .padding_xy(8, 4)
            .fill_width()
    }

    fn toolbar(&self) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let secure = tab.url.as_ref().is_some_and(|u| u.scheme == "https");
        let address = if tab.address.text() == HOME { String::new() } else { String::from(tab.address.text()) };
        let menu = vec![
            MenuItem::new("New Tab", Msg::NewTab).shortcut(Shortcut::ctrl('t')),
            MenuItem::new("Reopen Closed Tab", Msg::Reopen)
                .enabled(!self.closed.is_empty())
                .shortcut(Shortcut::ctrl_shift('t')),
            MenuItem::Separator,
            MenuItem::new("Find on Page…", Msg::Find).shortcut(Shortcut::ctrl('f')),
            MenuItem::Separator,
            MenuItem::new("Zoom In", Msg::Zoom(1)).shortcut(Shortcut::ctrl('=')),
            MenuItem::new("Zoom Out", Msg::Zoom(-1)).shortcut(Shortcut::ctrl('-')),
            MenuItem::new(format!("Actual Size ({}%)", tab.zoom), Msg::ZoomReset).shortcut(Shortcut::ctrl('0')),
        ];
        toolbar(vec![
            Button::symbol(Symbol::ChevronLeft).action(Msg::Back).disabled(tab.pos == 0).help("Back (Alt+←)"),
            Button::symbol(Symbol::ChevronRight)
                .action(Msg::Forward)
                .disabled(tab.pos + 1 >= tab.history.len())
                .help("Forward (Alt+→)"),
            if tab.loading() {
                Button::symbol(Symbol::Close).action(Msg::Reload).help("Stop")
            } else {
                Button::symbol(Symbol::Refresh).action(Msg::Reload).help("Reload (Ctrl+R)")
            },
            Button::symbol(Symbol::Home).action(Msg::Home).help("Start Page"),
            HStack::new()
                .push(
                    Image::symbol(if secure { Symbol::Lock } else { Symbol::Globe })
                        .symbol_size(14)
                        .foreground(Color::SecondaryLabel),
                )
                .push(
                    TextField::new("Search or enter an address", address)
                        .on_change(Msg::Address)
                        .on_submit(Msg::Go)
                        .shortcut(Shortcut::ctrl('l'))
                        .id(address_id(tab.id))
                        .grow(1),
                )
                .spacing(8)
                .padding_insets(Insets { leading: 8, ..Default::default() })
                .grow(1),
            Button::symbol(Symbol::More).menu(menu).help("More"),
        ])
    }

    fn find_bar(&self) -> Option<View<'_, Msg>> {
        let tab = &self.tabs[self.active];
        let f = tab.find.as_ref()?;
        let count = if f.edit.text().is_empty() {
            String::new()
        } else if f.matches.is_empty() {
            String::from("No matches")
        } else {
            format!("{} of {}", f.current + 1, f.matches.len())
        };
        Some(
            HStack::new()
                .push(Spacer::new())
                .push(
                    TextField::search("Find on page", String::from(f.edit.text()))
                        .on_change(Msg::FindQuery)
                        .on_submit(Msg::FindStep(1))
                        .id(find_id(tab.id))
                        .width(260),
                )
                .push(Text::new(count).style(TextStyle::Callout).foreground(if f.matches.is_empty() {
                    Color::Red
                } else {
                    Color::SecondaryLabel
                }))
                .push(Button::symbol(Symbol::ChevronUp).action(Msg::FindStep(-1)).help("Previous"))
                .push(Button::symbol(Symbol::ChevronDown).action(Msg::FindStep(1)).help("Next"))
                .push(Button::new("Done").action(Msg::FindDone))
                .spacing(8)
                .padding_xy(12, 6)
                .fill_width(),
        )
    }

    fn page(&self) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        Canvas::new(move |cv, r| tab.paint(cv, r))
            .on_input(|e| match e {
                CanvasEvent::Key(k) if browser_key(&k) => None,
                CanvasEvent::Key(k) => Some(Msg::PageKey(k)),
                CanvasEvent::Press { x, y, button: 1, .. } => Some(Msg::Press(x, y)),
                CanvasEvent::Hover { x, y } => Some(Msg::Hover(x, y)),
                CanvasEvent::Leave => Some(Msg::Leave),
                CanvasEvent::Scroll { dy, .. } if dy != 0 => Some(Msg::Scroll(dy)),
                CanvasEvent::Resize { w, h } => Some(Msg::Resize(w, h)),
                _ => None,
            })
            .focusable()
            .id(("page", tab.id))
            .fill()
    }
}

impl App for Browser {
    type Message = Msg;

    fn title(&self) -> String {
        format!("{} — Nebula", self.tabs[self.active].title())
    }

    fn size(&self) -> (i32, i32) {
        (1100, 760)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        let now = cx.now();
        let mods = cx.mods();
        let had_find = self.tabs[self.active].find.is_some();
        match msg {
            Msg::Select(i) => self.select(i),
            Msg::Close(i) => self.close_tab(i),
            Msg::NewTab => {
                if self.open(HOME, false) {
                    cx.focus(&address_id(self.tabs[self.active].id));
                }
            }
            Msg::Reopen => {
                if let Some(url) = self.closed.last().cloned() {
                    if self.open(&url, false) {
                        self.closed.pop();
                    }
                }
            }
            Msg::Back => self.tabs[self.active].back(),
            Msg::Forward => self.tabs[self.active].forward(),
            Msg::Reload => {
                let tab = &mut self.tabs[self.active];
                if tab.loading() {
                    tab.stop();
                } else {
                    tab.reload();
                }
            }
            Msg::Home => self.tabs[self.active].go(HOME, true),
            Msg::Address(text) => {
                let tab = &mut self.tabs[self.active];
                tab.editing = true;
                tab.address.set_text(&text);
            }
            Msg::Go => {
                let tab = &mut self.tabs[self.active];
                let t = String::from(tab.address.text());
                tab.go(&t, true);
                cx.focus(&("page", tab.id));
            }
            Msg::FindQuery(q) => {
                let tab = &mut self.tabs[self.active];
                if let Some(f) = &mut tab.find {
                    f.edit.set_text(&q);
                }
                tab.search(true);
            }
            Msg::FindStep(d) => self.tabs[self.active].find_step(d),
            Msg::FindDone => {
                let tab = &mut self.tabs[self.active];
                tab.find = None;
                cx.focus(&("page", tab.id));
            }
            Msg::Find => {
                let ev = KeyEvent {
                    code: KeyCode::Char,
                    ch: Some('f'),
                    pressed: true,
                    mods: waveui::Modifiers { ctrl: true, ..Default::default() },
                };
                self.tabs[self.active].key(&ev, now);
            }
            Msg::Zoom(d) => self.tabs[self.active].zoom_step(d),
            Msg::ZoomReset => self.tabs[self.active].set_zoom(100),
            Msg::Key(ev) => {
                let ctrl = ev.mods.ctrl || ev.mods.super_key;
                let tabs = self.tabs.len();
                if ctrl && ev.code == KeyCode::Tab {
                    let next = if ev.mods.shift { (self.active + tabs - 1) % tabs } else { (self.active + 1) % tabs };
                    self.select(next);
                } else if ctrl && ev.code == KeyCode::Char {
                    match ev.ch.map(|c| c.to_ascii_lowercase()) {
                        Some('t') if ev.mods.shift => cx.send(Msg::Reopen),
                        Some('t') => cx.send(Msg::NewTab),
                        Some(c @ '1'..='8') => self.select((c as u8 - b'1') as usize),
                        Some('9') => self.select(tabs - 1),
                        Some('l') => cx.focus(&address_id(self.tabs[self.active].id)),
                        _ => {
                            self.tabs[self.active].key(&ev, now);
                        }
                    }
                } else {
                    let tab = &mut self.tabs[self.active];
                    let editing = tab.editing;
                    tab.key(&ev, now);
                    // Escape in the address field puts the page's address back.
                    if editing && !tab.editing {
                        cx.focus(&("page", tab.id));
                    }
                }
            }
            Msg::PageKey(ev) => {
                self.tabs[self.active].key(&ev, now);
            }
            Msg::Press(x, y) => {
                let ctrl = mods.ctrl || mods.super_key;
                let tab = &self.tabs[self.active];
                let target = ctrl
                    .then(|| tab.page_point(x, y))
                    .flatten()
                    .and_then(|(px, py)| tab.layout.as_ref()?.link_at(px, py))
                    .and_then(|n| tab.href(n));
                if let Some(url) = target {
                    // Ctrl-click opens behind (Shift: in front); with no room
                    // left, the link opens here.
                    if !self.open(&url, !mods.shift) {
                        self.tabs[self.active].go(&url, true);
                    }
                } else {
                    self.tabs[self.active].click(x, y);
                }
            }
            Msg::Hover(x, y) => {
                self.tabs[self.active].hover(x, y);
            }
            Msg::Leave => self.tabs[self.active].hover = None,
            Msg::Scroll(dy) => {
                self.tabs[self.active].scroll(dy);
            }
            Msg::Resize(w, h) => self.tabs[self.active].prepare(w, h),
        }
        // Ctrl+F (from the page, the menu or a shortcut) types into the find bar.
        let tab = &self.tabs[self.active];
        if tab.find.is_some() && !had_find {
            cx.focus(&find_id(tab.id));
            if let Some(f) = cx.field(&find_id(tab.id)) {
                f.edit.select_all();
            }
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        Some(Msg::Key(*ev))
    }

    fn started(&mut self, cx: &mut Context<Msg>) {
        let tab = &self.tabs[self.active];
        if tab.address.text() == HOME {
            cx.focus(&("page", tab.id));
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(self.tick_interval())
    }

    fn poll(&mut self, cx: &mut Context<Msg>) -> bool {
        let dirty = self.tick_all(cx.now());
        // Pages lay out at the size they are shown.
        let tab = &mut self.tabs[self.active];
        let (w, h) = (tab.view.w, tab.view.h);
        tab.prepare(w, h);
        dirty
    }

    fn close_tab_requested(&mut self, _cx: &mut Context<Msg>) -> bool {
        if self.tabs.len() > 1 {
            self.close_tab(self.active);
            false
        } else {
            true
        }
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let progress = tab.loading().then(|| {
            let elapsed = env.now_ms.saturating_sub(tab.loading_since) as f32;
            ProgressView::new(Some(1.0 - 1000.0 / (1000.0 + elapsed))).height(3).corner_radius(0)
        });
        VStack::new()
            .push(self.tab_strip())
            .push(self.toolbar())
            .push_if(progress)
            .push(Divider::new())
            .push_if(self.find_bar().map(|f| VStack::new().push(f).push(Divider::new()).spacing(0)))
            .push(self.page())
            .spacing(0)
            .fill()
    }
}
