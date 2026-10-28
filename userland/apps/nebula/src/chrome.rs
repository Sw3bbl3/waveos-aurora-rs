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
    OpenAddress,
    Share,
    Downloads,
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

/// A page's little colour square: the same colour for the same site.
fn site_color(host: &str) -> Color {
    const COLORS: [Color; 7] =
        [Color::Purple, Color::Red, Color::Orange, Color::Teal, Color::Blue, Color::Pink, Color::Green];
    if host.is_empty() {
        return Color::Accent;
    }
    let h = host.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    COLORS[h as usize % COLORS.len()]
}

impl Browser {
    fn tab_strip(&self) -> View<'_, Msg> {
        let mut strip = HStack::new().spacing(4);
        for (i, tab) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            let color = site_color(tab.url.as_ref().map_or("", |u| u.host.as_str())).argb();
            let loading = tab.loading();
            let mut pill = HStack::new()
                .push(
                    Canvas::new(move |cv, r| {
                        let (x, y) = r.center();
                        let sq = waveui::lumen::geom::Rect::new(x - 7, y - 7, 14, 14);
                        if loading {
                            cv.stroke_round_rect(sq, 4, color);
                        } else {
                            cv.fill_round_rect(sq, 4, color);
                        }
                    })
                    .size(16, 16),
                )
                .push(
                    Text::new(tab.title())
                        .style(TextStyle::Sized(13, waveui::style::Weight::Bold))
                        .foreground(if active { Color::Label } else { Color::SecondaryLabel }),
                )
                .spacing(8)
                .padding_insets(Insets { top: 0, leading: 12, bottom: 0, trailing: if active { 4 } else { 12 } })
                .height(30)
                .on_tap(Msg::Select(i))
                .context_menu(vec![MenuItem::new("New Tab", Msg::NewTab), MenuItem::new("Close Tab", Msg::Close(i))])
                .max_width(240);
            if active {
                pill = pill
                    .push(
                        Button::symbol(Symbol::Close)
                            .action(Msg::Close(i))
                            .control_size(ControlSize::Small)
                            .size(22, 22)
                            .help("Close Tab (Ctrl+W)"),
                    )
                    .background_rounded(Color::Fill, 15);
            }
            strip = strip.push(pill);
        }
        let mut bar = HStack::new().push(strip).push(
            Button::symbol(Symbol::Plus)
                .action(Msg::NewTab)
                .disabled(self.tabs.len() >= MAX_TABS)
                .foreground(Color::SecondaryLabel)
                .help("New Tab (Ctrl+T)"),
        );
        bar = bar.push(Spacer::new());
        if self.notice() {
            bar = bar.push(Text::new("Eight tabs are already open").style(TextStyle::Callout));
        }
        bar.spacing(6).padding_insets(Insets { top: 8, leading: 12, bottom: 0, trailing: 12 }).height(46).fill_width()
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
            MenuItem::new("Start Page", Msg::Home),
            MenuItem::Separator,
            MenuItem::new("Find on Page…", Msg::Find).shortcut(Shortcut::ctrl('f')),
            MenuItem::Separator,
            MenuItem::new("Zoom In", Msg::Zoom(1)).shortcut(Shortcut::ctrl('=')),
            MenuItem::new("Zoom Out", Msg::Zoom(-1)).shortcut(Shortcut::ctrl('-')),
            MenuItem::new(format!("Actual Size ({}%)", tab.zoom), Msg::ZoomReset).shortcut(Shortcut::ctrl('0')),
        ];
        let lock = Image::symbol(if secure { Symbol::Lock } else { Symbol::Globe })
            .symbol_size(13)
            .foreground(if secure { Color::Green } else { Color::TertiaryLabel });
        // At rest the pill reads "host  /path"; clicked, it's a field.
        let at_rest = tab.url.as_ref().filter(|_| !(self.address_open || tab.editing || address.is_empty()));
        let pill = if let Some(url) = at_rest {
            let path = if url.path == "/" { String::new() } else { url.path.clone() };
            let host =
                if url.host.is_empty() { url.scheme.clone() } else { url.host.trim_start_matches("www.").into() };
            HStack::new()
                .push(lock)
                .push(Text::new(host).style(TextStyle::Sized(13, waveui::style::Weight::Bold)))
                .push(
                    Text::new(path)
                        .style(TextStyle::Sized(13, waveui::style::Weight::Regular))
                        .foreground(Color::TertiaryLabel)
                        .grow(1),
                )
                .spacing(8)
                .on_tap(Msg::OpenAddress)
        } else {
            HStack::new()
                .push(lock)
                .push(
                    TextField::new("Search or enter an address", address)
                        .plain()
                        .on_change(Msg::Address)
                        .on_submit(Msg::Go)
                        .id(address_id(tab.id))
                        .grow(1),
                )
                .spacing(8)
        };
        let pill = pill
            .padding_xy(14, 0)
            .height(34)
            .background_rounded(Color::Fill, 17)
            .border(Color::Separator, 17)
            .width(560);
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
            Spacer::new(),
            pill,
            Spacer::new(),
            Button::symbol(Symbol::Share).action(Msg::Share).disabled(tab.url.is_none()).help("Copy Address"),
            Button::symbol(Symbol::Download).action(Msg::Downloads).help("Downloads"),
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
            Msg::Select(i) => {
                self.address_open = false;
                self.select(i);
            }
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
            Msg::OpenAddress => {
                self.address_open = true;
                let id = address_id(self.tabs[self.active].id);
                cx.focus(&id);
                if let Some(f) = cx.field(&id) {
                    f.edit.select_all();
                }
            }
            Msg::Share => {
                if let Some(u) = &self.tabs[self.active].url {
                    cx.copy(&u.to_string());
                }
            }
            Msg::Downloads => cx.open_file("/Downloads"),
            Msg::Address(text) => {
                let tab = &mut self.tabs[self.active];
                tab.editing = true;
                tab.address.set_text(&text);
            }
            Msg::Go => {
                self.address_open = false;
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
                        Some('l') => cx.send(Msg::OpenAddress),
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
                        self.address_open = false;
                        cx.focus(&("page", tab.id));
                    }
                }
            }
            Msg::PageKey(ev) => {
                self.tabs[self.active].key(&ev, now);
            }
            Msg::Press(x, y) => {
                self.address_open = false;
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
            .push_if(self.find_bar().map(|f| VStack::new().push(f).push(Divider::new()).spacing(0)))
            .push(self.page())
            .spacing(0)
            .fill()
    }
}
