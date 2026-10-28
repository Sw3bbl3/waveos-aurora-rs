use super::*;
use transfer::{Choice, Manager};
use waveui::lumen::canvas::Canvas as Surface;
use waveui::lumen::geom::Rect;
use waveui::prelude::*;
use waveui::{KeyCode, KeyEvent};

mod checks;

const SIDEBAR_W: i32 = 200;
/// The information panel beside the content.
const INFO_W: i32 = 270;

/// What the transfer panel shows, to notice when it changes.
type ProgressSnapshot = (u64, usize, usize, bool, bool, bool, u64, String);

#[derive(Clone)]
pub enum Msg {
    Tab(usize),
    NewTab,
    CloseTab(usize),
    Back,
    Forward,
    Up,
    Go(String),
    Eject(String),
    ViewMode(usize),
    Sort(SortKey),
    Query(String),
    Click(usize),
    Open(usize),
    ItemAction(usize, Action),
    Action(Action),
    Deselect,
    RenameText(String),
    RenameCommit,
    RenameCancel,
    DropFiles(String, Vec<String>),
    DropText(String, String),
    Confirm,
    CancelConfirm,
    CloseQuickLook,
    Move(i32, bool),
    TypeChar(char),
    SelectAll,
    Decision(usize),
    Dismiss,
    NextError,
    KeepOpen,
    CancelClose,
}

pub struct Files {
    tabs: Vec<Folder>,
    active: usize,
    transfers: transfer::Shared,
    worker: SearchWorker,
    cut: Arc<Mutex<Vec<String>>>,
    conflict_serial: u64,
    progress: Option<ProgressSnapshot>,
    close_prompt: bool,
    closing: bool,
    error_index: usize,
    /// Icons per row and the visible height at the last layout.
    viewport: core::cell::Cell<(i32, i32)>,
}

impl Files {
    pub fn new(start: &str) -> Self {
        let transfers = Manager::new();
        let worker = Arc::new(Mutex::new(None));
        let cut = Arc::new(Mutex::new(Vec::new()));
        Self {
            tabs: alloc::vec![Folder::new(start, transfers.clone(), worker.clone(), cut.clone())],
            active: 0,
            transfers,
            worker,
            cut,
            conflict_serial: 0,
            progress: None,
            close_prompt: false,
            closing: false,
            error_index: 0,
            viewport: core::cell::Cell::new((5, 400)),
        }
    }

    fn tab(&mut self) -> &mut Folder {
        &mut self.tabs[self.active]
    }

    fn switch(&mut self, next: usize) {
        if next == self.active || next >= self.tabs.len() {
            return;
        }
        let old = &mut self.tabs[self.active];
        let resume = !old.search_done() || old.search_at_pending();
        old.stop_search();
        old.set_resume(resume);
        old.thumbs.clear();
        old.quicklook = None;
        old.finish_rename(true);
        self.active = next;
        let tab = &mut self.tabs[next];
        if tab.take_resume() {
            tab.start_search();
        } else {
            tab.refresh();
        }
    }

    fn add(&mut self) {
        if self.tabs.len() == 8 {
            self.tab().flash(String::from("Eight tabs are already open"));
            return;
        }
        let path = self.tabs[self.active].cwd.clone();
        self.tabs.push(Folder::new(&path, self.transfers.clone(), self.worker.clone(), self.cut.clone()));
        self.switch(self.tabs.len() - 1);
    }

    fn close_tab(&mut self, i: usize) -> bool {
        if self.tabs.len() == 1 {
            return false;
        }
        self.tabs[i].stop_search();
        self.tabs.remove(i);
        if self.active >= self.tabs.len() || i < self.active {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        let tab = &mut self.tabs[self.active];
        if tab.take_resume() {
            tab.start_search();
        }
        true
    }

    pub fn cancel_close(&mut self) {
        self.close_prompt = false;
        self.closing = true;
        self.transfers.lock().state.lock().cancel = true;
        for tab in &mut self.tabs {
            tab.stop_search();
        }
    }

    /// The close button: refuses while work is under way.
    pub fn ask_close(&mut self) -> bool {
        if self.transfers.lock().busy() {
            self.close_prompt = true;
            return false;
        }
        for tab in &mut self.tabs {
            tab.stop_search();
        }
        if self.worker.lock().is_some() {
            self.closing = true;
            return false;
        }
        true
    }

    fn decision(&mut self, index: usize) {
        let manager = self.transfers.lock();
        let mut s = manager.state.lock();
        match index {
            0 if s.conflict.as_ref().is_some_and(|c| c.replace) => s.decision = Some(Choice::Replace),
            1 => s.decision = Some(Choice::KeepBoth),
            2 => s.decision = Some(Choice::Skip),
            3 => s.apply_all = !s.apply_all,
            4 => s.cancel = true,
            _ => {}
        }
    }

    /// Transfers, searches and thumbnails; true to redraw. Returns whether
    /// the window should close (a deferred close finished).
    pub fn background(&mut self, now: u64) -> (bool, bool) {
        let finished = self.transfers.lock().reap();
        let progress_changed;
        {
            let m = self.transfers.lock();
            let s = m.state.lock();
            let progress = (
                s.bytes,
                s.completed,
                s.errors.len(),
                s.running,
                s.cancel,
                s.committing,
                s.conflict_serial,
                s.current.clone(),
            );
            progress_changed = self.progress.as_ref() != Some(&progress);
            self.progress = Some(progress);
            self.conflict_serial = s.conflict_serial;
        }
        if finished {
            let manager = self.transfers.lock();
            let s = manager.state.lock();
            {
                let mut cut = self.cut.lock();
                let owns_clipboard = corekit::clipboard::files().is_some_and(|paths| paths == *cut);
                cut.retain(|p| !s.moved.contains(p));
                if owns_clipboard {
                    corekit::clipboard::set_files(&cut);
                }
            }
            for e in &s.errors {
                corekit::eprintln!("Files transfer: {}", e);
            }
            drop(s);
            drop(manager);
            for tab in &mut self.tabs {
                tab.refresh();
            }
        }
        let changed = self.tabs[self.active].background(now);
        let mut close = false;
        if self.closing && !self.transfers.lock().busy() && self.worker.lock().is_none() {
            if self.transfers.lock().state.lock().errors.is_empty() {
                close = true;
            } else {
                self.closing = false;
            }
        }
        (changed || finished || progress_changed, close)
    }

    /// Scrolls the content so the item asked for shows.
    fn reveal(&mut self, cx: &mut Context<Msg>) {
        let tab = &mut self.tabs[self.active];
        let Some(i) = tab.reveal.take() else { return };
        let (cols, height) = self.viewport.get();
        let (top, h) = match tab.view {
            ViewMode::Icons => ((i as i32 / cols.max(1)) * TILE_H, TILE_H),
            ViewMode::List => (i as i32 * ROW_H, ROW_H),
        };
        let id = ("content", self.active, tab.view == ViewMode::Icons);
        let at = cx.scroll_offset(&id);
        if top < at {
            cx.scroll_to(&id, top as f32);
        } else if top + h > at + height {
            cx.scroll_to(&id, (top + h - height) as f32);
        }
    }

    // ------------------------------------------------------------ views

    fn place_row<'a>(&self, label: &str, path: &str, symbol: Symbol, volume: bool) -> View<'a, Msg> {
        let tab = &self.tabs[self.active];
        let target = String::from(path);
        let drop_target = target.clone();
        let mut row = HStack::new()
            .push(Image::symbol(symbol).symbol_size(17))
            .push(Text::new(label).style(TextStyle::Sized(14, waveui::style::Weight::Bold)).grow(1))
            .spacing(10)
            .padding_xy(10, 0)
            .height(size::SIDEBAR_ROW)
            .fill_width()
            .corner_radius(radius::ITEM)
            .selected(!tab.searching() && tab.cwd == path)
            .on_tap(Msg::Go(target.clone()));
        if volume {
            row = row.push(Button::symbol(Symbol::Eject).action(Msg::Eject(target)).help("Eject"));
        }
        if !read_only(path) {
            row = row.on_drop(move |d| match d.items {
                corekit::dnd::Dropped::Files(paths) => Some(Msg::DropFiles(drop_target.clone(), paths)),
                corekit::dnd::Dropped::Text(t) => Some(Msg::DropText(drop_target.clone(), t)),
            });
        }
        row
    }

    fn sidebar(&self) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let heading = |s: &str| Text::label(s).padding_insets(Insets { top: 10, leading: 10, bottom: 4, trailing: 0 });
        let mut v = VStack::new().push(heading("Places")).spacing(2).alignment(Align::Stretch);
        for p in &FAVORITES {
            v = v.push(self.place_row(p.label, p.path, p.symbol, false));
        }
        v = v.push(heading("Locations"));
        for p in &LOCATIONS[..LOCATIONS.len() - 1] {
            v = v.push(self.place_row(p.label, p.path, p.symbol, false));
        }
        for (name, path) in &tab.volumes {
            v = v.push(self.place_row(name, path, Symbol::Drive, true));
        }
        let trash = &LOCATIONS[LOCATIONS.len() - 1];
        v = v.push(self.place_row(trash.label, trash.path, trash.symbol, false));
        ScrollView::new(v.padding_insets(Insets { top: 10, leading: 10, bottom: 10, trailing: 10 }).fill_width())
    }

    fn toolbar(&self) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let mut bar = vec![
            Button::symbol(Symbol::ChevronLeft).action(Msg::Back).disabled(!tab.can_go_back()).help("Back (Alt+Left)"),
            Text::new(tab.title()).style(TextStyle::Headline),
            Spacer::new(),
        ];
        if tab.in_trash() {
            bar.push(
                Button::new("Empty Trash…")
                    .action(Msg::Action(Action::EmptyTrash))
                    .button_style(ButtonStyle::Destructive)
                    .control_size(ControlSize::Small)
                    .disabled(tab.items.is_empty()),
            );
        }
        bar.push(
            Picker::new(["Grid", "List"], (tab.view == ViewMode::List) as usize)
                .on_change(Msg::ViewMode)
                .help("View as a grid or as a list"),
        );
        bar.push(
            TextField::search(format!("Seek in {}", tab.title()), tab.query.clone())
                .on_change(Msg::Query)
                .shortcut(Shortcut::ctrl('f'))
                .id(("search", self.active))
                .width(200),
        );
        toolbar(bar)
    }

    fn tab_bar(&self) -> Option<View<'_, Msg>> {
        if self.tabs.len() < 2 {
            return None;
        }
        let mut bar = HStack::new().spacing(4);
        for (i, tab) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            bar = bar.push(
                HStack::new()
                    .push(
                        Text::new(tab.title())
                            .style(if active { TextStyle::Headline } else { TextStyle::Body })
                            .grow(1),
                    )
                    .push(
                        Button::symbol(Symbol::Close)
                            .action(Msg::CloseTab(i))
                            .control_size(ControlSize::Small)
                            .help("Close Tab"),
                    )
                    .spacing(4)
                    .padding_insets(Insets { top: 0, leading: 12, bottom: 0, trailing: 4 })
                    .height(30)
                    .background_rounded(if active { Color::Background } else { Color::Clear }, 8)
                    .on_tap(Msg::Tab(i))
                    .max_width(220)
                    .grow(1),
            );
        }
        Some(bar.padding_xy(6, 4).background(Color::GroupedBackground).fill_width())
    }

    /// One card of the grid: the picture (or the item's icon) and its name.
    fn tile(&self, i: usize, width: i32) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let item = &tab.items[i];
        let selected = tab.selection.contains(&i);
        let thumb = tab.thumbs.get(&item.path).and_then(|t| t.as_ref());
        let picture: View<'_, Msg> = match thumb {
            Some(img) => Canvas::new(move |cv: &mut Surface, r: Rect| cover(cv, img, r, 12)).height(104).fill_width(),
            None => ZStack::with(vec![Image::app_icon(icon_of(item), 56)])
                .height(104)
                .fill_width()
                .background_rounded(Color::Fill, 12),
        };
        let name: View<'_, Msg> = match &tab.rename {
            Some((r, text)) if *r == i => TextField::new("Name", text.clone())
                .on_change(Msg::RenameText)
                .on_submit(Msg::RenameCommit)
                .focused()
                .id(("rename", i))
                .fill_width(),
            _ => Text::new(item.name.clone()).style(TextStyle::Sized(13, waveui::style::Weight::Bold)),
        };
        let card = VStack::new()
            .push(picture)
            .push(name.padding_xy(2, 0))
            .spacing(7)
            .alignment(Align::Stretch)
            .padding(6)
            .size(width, TILE_H - 8)
            .corner_radius(16)
            .selected(selected);
        self.item_modifiers(card, i)
    }

    /// Clicks, menus, dragging and dropping for item `i`.
    fn item_modifiers<'a>(&self, v: View<'a, Msg>, i: usize) -> View<'a, Msg> {
        let tab = &self.tabs[self.active];
        let item = &tab.items[i];
        let selected = tab.selection.contains(&i);
        // The menu describes the selection a right click would act on.
        let menu: Vec<MenuItem<Msg>> = if selected { tab.menu(true) } else { tab.menu_for(i) }
            .into_iter()
            .map(|m| match m {
                Some((a, label)) => MenuItem::new(label, Msg::ItemAction(i, a)),
                None => MenuItem::Separator,
            })
            .collect();
        let paths = if selected { tab.selected_paths() } else { alloc::vec![item.path.clone()] };
        let icon = if item.is_dir {
            corekit::abi::drag::FOLDER
        } else if is_image(&item.name) {
            corekit::abi::drag::PICTURE
        } else {
            corekit::abi::drag::DOCUMENT
        };
        let mut v = v
            .on_tap(Msg::Click(i))
            .on_double_tap(Msg::Open(i))
            .context_menu(menu)
            .draggable(DragItem::Files(paths, icon))
            .help(format!("{} · {}", item.name, kind_of(item)));
        if item.is_dir && !selected && !read_only(&item.path) {
            let dir = item.path.clone();
            v = v.on_drop(move |d| match d.items {
                corekit::dnd::Dropped::Files(paths) => Some(Msg::DropFiles(dir.clone(), paths)),
                corekit::dnd::Dropped::Text(t) => Some(Msg::DropText(dir.clone(), t)),
            });
        }
        v
    }

    fn row_view(&self, i: usize) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let item = &tab.items[i];
        let selected = tab.selection.contains(&i);
        let name: View<'_, Msg> = match &tab.rename {
            Some((r, text)) if *r == i => TextField::new("Name", text.clone())
                .on_change(Msg::RenameText)
                .on_submit(Msg::RenameCommit)
                .focused()
                .id(("rename", i))
                .height(24)
                .grow(1),
            _ => Text::new(item.name.clone()).grow(1),
        };
        let size = if item.is_dir { String::from("—") } else { human_size(item.size) };
        let row = HStack::new()
            .push(Image::app_icon(icon_of(item), 18))
            .push(name)
            .push(Text::new(human_date(item.mtime)).foreground(Color::SecondaryLabel).width(170))
            .push(Text::new(size).text_align(Align::End).foreground(Color::SecondaryLabel).width(80))
            .push(
                Text::new(kind_of(item))
                    .foreground(Color::SecondaryLabel)
                    .padding_insets(Insets { leading: 16, ..Default::default() })
                    .width(126),
            )
            .spacing(8)
            .padding_xy(10, 0)
            .height(ROW_H - 2)
            .fill_width()
            .selected(selected)
            .corner_radius(6);
        self.item_modifiers(row, i).padding_xy(8, 1)
    }

    fn content(&self, env: &Env) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        // Cards fill the width: as many columns as fit at TILE_W.
        let width = env.size.0 - SIDEBAR_W - 1 - self.info_width(env) - 32;
        let cols = ((width + 8) / (TILE_W + 8)).max(1);
        let tile_w = (width - 8 * (cols - 1)) / cols;
        let chrome = size::TOOLBAR + 28 + if self.tabs.len() > 1 { 38 } else { 0 } + self.panel_height();
        self.viewport.set((cols, env.size.1 - chrome - if tab.view == ViewMode::List { 30 } else { 0 }));
        let cwd = tab.cwd.clone();
        let here = cwd.clone();
        let background_menu: Vec<MenuItem<Msg>> = tab
            .menu(false)
            .into_iter()
            .map(|m| match m {
                Some((a, label)) => MenuItem::new(label, Msg::Action(a)),
                None => MenuItem::Separator,
            })
            .collect();
        let n = tab.items.len();
        let body: View<'_, Msg> = if n == 0 {
            let (title, detail) = if tab.searching() {
                if tab.search_done() {
                    ("No results", "Nothing here matches your search.")
                } else {
                    ("Searching…", "")
                }
            } else {
                ("This folder is empty", "Drop files here, or create a folder with Ctrl+Shift+N.")
            };
            VStack::new()
                .push(Text::new(title).style(TextStyle::Title2).foreground(Color::SecondaryLabel))
                .push(Text::new(detail).foreground(Color::SecondaryLabel))
                .spacing(6)
                .fill()
        } else {
            match tab.view {
                ViewMode::Icons => {
                    let rows = n.div_ceil(cols as usize);
                    List::new(rows, TILE_H, move |r| {
                        let from = r * cols as usize;
                        HStack::with((from..(from + cols as usize).min(n)).map(|i| self.tile(i, tile_w)).collect())
                            .spacing(8)
                            .alignment(Align::Start)
                            .padding_insets(Insets { top: 8, leading: 16, bottom: 0, trailing: 0 })
                            .fill()
                    })
                    .id(("content", self.active, true))
                }
                ViewMode::List => {
                    let header = HStack::with(
                        core::iter::once(View::empty().width(18))
                            .chain(
                                [
                                    (SortKey::Name, "Name", 0),
                                    (SortKey::Date, "Date Modified", 170),
                                    (SortKey::Size, "Size", 80),
                                    (SortKey::Kind, "Kind", 126),
                                ]
                                .into_iter()
                                .map(|(k, label, w)| {
                                    let arrow = if tab.sort.0 == k {
                                        if tab.sort.1 {
                                            " ↑"
                                        } else {
                                            " ↓"
                                        }
                                    } else {
                                        ""
                                    };
                                    let t = Text::new(format!("{label}{arrow}"))
                                        .style(if tab.sort.0 == k {
                                            TextStyle::Headline
                                        } else {
                                            TextStyle::Subheadline
                                        })
                                        .foreground(Color::SecondaryLabel)
                                        .text_align(if k == SortKey::Size { Align::End } else { Align::Start })
                                        .padding_insets(Insets {
                                            leading: if k == SortKey::Kind { 16 } else { 0 },
                                            ..Default::default()
                                        })
                                        .fill_height()
                                        .on_tap(Msg::Sort(k))
                                        .corner_radius(6);
                                    if w == 0 {
                                        t.grow(1)
                                    } else {
                                        t.width(w)
                                    }
                                }),
                            )
                            .collect(),
                    )
                    .spacing(8)
                    .padding_insets(Insets { top: 0, leading: 18, bottom: 0, trailing: 18 })
                    .height(30)
                    .fill_width();
                    VStack::new()
                        .push(header)
                        .push(Divider::new())
                        .push(List::new(n, ROW_H, move |i| self.row_view(i)).id(("content", self.active, false)))
                        .spacing(0)
                        .fill()
                }
            }
        };
        let mut body = body.on_tap(Msg::Deselect).context_menu(background_menu);
        if tab.writable_here() {
            body = body.on_drop(move |d| match d.items {
                corekit::dnd::Dropped::Files(paths) => Some(Msg::DropFiles(here.clone(), paths)),
                corekit::dnd::Dropped::Text(t) => Some(Msg::DropText(here.clone(), t)),
            });
        }
        body.fill()
    }

    fn status_bar(&self) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let n = tab.items.len();
        let mut info = format!("{n} item{}", if n == 1 { "" } else { "s" });
        if !tab.selection.is_empty() {
            info.push_str(&format!(" · {} selected", tab.selection.len()));
        }
        if tab.searching() && !tab.search_done() {
            info.push_str(" · searching…");
        }
        let free = corekit::process::sys_info().disk_free;
        let right = match &tab.status {
            Some((s, _)) => s.clone(),
            None if free > 0 => format!("{} free", human_size(free)),
            None => String::new(),
        };
        HStack::new()
            .push(Text::new(info).style(TextStyle::Caption).foreground(Color::TertiaryLabel).grow(1))
            .push(Text::new(right).style(TextStyle::Caption).foreground(Color::TertiaryLabel))
            .padding_xy(18, 0)
            .height(34)
            .fill_width()
    }

    /// The information panel's width (none in narrow windows).
    fn info_width(&self, env: &Env) -> i32 {
        if env.size.0 >= 960 {
            INFO_W + 1
        } else {
            0
        }
    }

    /// The selected item (or the folder): a picture, its name and kind,
    /// what is known about it, and a way to open it.
    fn info_panel(&self) -> View<'_, Msg> {
        let tab = &self.tabs[self.active];
        let one = (tab.selection.len() == 1).then(|| *tab.selection.iter().next().unwrap());
        let item = one.and_then(|i| tab.items.get(i));
        let preview: View<'_, Msg> = match item.and_then(|it| tab.thumbs.get(&it.path)).and_then(|t| t.as_ref()) {
            Some(img) => Canvas::new(move |cv: &mut Surface, r: Rect| cover(cv, img, r, 14)).height(170).fill_width(),
            None => {
                let icon = item.map(icon_of).unwrap_or(Icon::Folder);
                ZStack::with(vec![Image::app_icon(icon, 72)])
                    .height(170)
                    .fill_width()
                    .background_rounded(Color::Fill, 14)
            }
        };
        let kv = |k: &str, v: String| {
            HStack::new()
                .push(Text::new(k).style(TextStyle::Callout).foreground(Color::SecondaryLabel).grow(1))
                .push(Text::new(v).style(TextStyle::Callout))
                .padding_xy(0, 7)
                .fill_width()
        };
        let folder = tab.title();
        let (name, kind, facts): (String, String, Vec<View<'_, Msg>>) = match item {
            Some(it) => (
                it.name.clone(),
                if it.is_dir {
                    String::from(kind_of(it))
                } else {
                    format!("{} · {}", kind_of(it), human_size(it.size))
                },
                vec![kv("Modified", human_date(it.mtime)), kv("Where", folder.clone())],
            ),
            None if tab.selection.len() > 1 => {
                (format!("{} items", tab.selection.len()), String::from("Selected"), vec![kv("Where", folder.clone())])
            }
            None => (folder.clone(), String::from("Folder"), vec![kv("Items", format!("{}", tab.items.len()))]),
        };
        let open: Option<View<'_, Msg>> = item.map(|it| {
            let label = if it.is_dir {
                String::from("Open")
            } else if is_image(&it.name) {
                String::from("Open in Lens")
            } else {
                String::from("Open")
            };
            HStack::new()
                .push(Button::new(label).action(Msg::Action(Action::Open)).button_style(ButtonStyle::Prominent).grow(1))
                .push(Button::symbol(Symbol::Copy).action(Msg::Action(Action::Copy)).help("Copy (Ctrl+C)"))
                .spacing(8)
                .fill_width()
        });
        VStack::new()
            .push(preview)
            .push(
                VStack::new()
                    .push(Text::new(name).style(TextStyle::Headline))
                    .push(Text::new(kind).style(TextStyle::Caption).foreground(Color::TertiaryLabel))
                    .spacing(2)
                    .alignment(Align::Start),
            )
            .push(Divider::new())
            .extend(facts)
            .push(Spacer::new())
            .push_if(open)
            .spacing(10)
            .alignment(Align::Stretch)
            .padding(18)
            .width(INFO_W)
            .fill_height()
    }

    fn panel_height(&self) -> i32 {
        let s = self.transfers.lock();
        let s = s.state.lock();
        if s.running || !s.summary.is_empty() || self.closing {
            if s.conflict.is_some() {
                112
            } else {
                76
            }
        } else {
            0
        }
    }

    fn panel(&self) -> Option<View<'_, Msg>> {
        let manager = self.transfers.lock();
        let s = manager.state.lock();
        if !(s.running || !s.summary.is_empty() || self.closing) {
            return None;
        }
        let heading = if self.closing {
            String::from("Cleaning up before closing…")
        } else if s.running {
            String::from("Copying files")
        } else {
            s.summary.clone()
        };
        let mut v =
            VStack::new().push(Text::new(heading).style(TextStyle::Headline)).spacing(8).alignment(Align::Stretch);
        if let Some(c) = &s.conflict {
            v = v.push(Text::new(format!("“{}” already exists here.", c.name)).foreground(Color::SecondaryLabel)).push(
                HStack::new()
                    .push(Toggle::checkbox("Apply to all", s.apply_all).on_change(|_: bool| Msg::Decision(3)))
                    .push(Spacer::new())
                    .push(Button::new("Stop").action(Msg::Decision(4)).shortcut(Shortcut::cancel()))
                    .push(Button::new("Skip").action(Msg::Decision(2)))
                    .push(Button::new("Keep Both").action(Msg::Decision(1)))
                    .push(
                        Button::new("Replace")
                            .action(Msg::Decision(0))
                            .button_style(ButtonStyle::Prominent)
                            .disabled(!c.replace),
                    )
                    .spacing(8),
            );
        } else if s.running {
            let progress = if s.total > 0 { Some(s.completed as f32 / s.total as f32) } else { None };
            v = v.push(
                HStack::new()
                    .push(
                        VStack::new()
                            .push(ProgressView::new(progress))
                            .push(
                                Text::new(format!(
                                    "{} of {} items · {} · {}",
                                    s.completed,
                                    s.total,
                                    human_size(s.bytes),
                                    s.current
                                ))
                                .style(TextStyle::Footnote)
                                .foreground(Color::SecondaryLabel),
                            )
                            .spacing(6)
                            .alignment(Align::Stretch)
                            .grow(1),
                    )
                    .push(
                        Button::new(if s.committing {
                            "Finishing…"
                        } else if s.cancel {
                            "Stopping…"
                        } else {
                            "Stop"
                        })
                        .action(Msg::Dismiss)
                        .disabled(s.committing || s.cancel),
                    )
                    .spacing(16),
            );
        } else {
            let line = if s.errors.is_empty() {
                format!("{} copied", human_size(s.bytes))
            } else {
                let i = self.error_index % s.errors.len();
                format!("Problem {} of {}: {}", i + 1, s.errors.len(), s.errors[i])
            };
            v = v.push(
                HStack::new()
                    .push(Text::new(line).foreground(Color::SecondaryLabel).grow(1))
                    .push_if((s.errors.len() > 1).then(|| Button::new("Next Problem").action(Msg::NextError)))
                    .push(Button::new("OK").action(Msg::Dismiss))
                    .spacing(8),
            );
        }
        Some(v.padding_xy(16, 12).background(Color::GroupedBackground).fill_width())
    }
}

impl App for Files {
    type Message = Msg;

    fn title(&self) -> String {
        self.tabs[self.active].title()
    }

    fn size(&self) -> (i32, i32) {
        (1140, 728)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        let mut out = Vec::new();
        let mods = cx.mods();
        let ctrl = mods.ctrl || mods.super_key;
        match msg {
            Msg::Tab(i) => self.switch(i),
            Msg::NewTab => self.add(),
            Msg::CloseTab(i) => {
                if !self.close_tab(i) && self.ask_close() {
                    cx.close();
                }
            }
            Msg::Back => self.tab().go_back(),
            Msg::Forward => self.tab().go_forward(),
            Msg::Up => self.tab().go_up(),
            Msg::Go(path) => self.tab().navigate(&path, true),
            Msg::Eject(path) => match desktop::eject(&path) {
                Ok(()) => {
                    let tab = self.tab();
                    tab.flash(format!("Ejected “{}”", fs::file_name(&path)));
                    tab.refresh_volumes();
                }
                Err(e) => self.tab().flash(format!("Couldn't eject: {e}")),
            },
            Msg::ViewMode(i) => self.tab().view = if i == 0 { ViewMode::Icons } else { ViewMode::List },
            Msg::Sort(k) => self.tab().set_sort(k),
            Msg::Query(q) => {
                let tab = self.tab();
                tab.query = q;
                tab.start_search();
            }
            Msg::Click(i) => self.tab().click(i, ctrl, mods.shift),
            Msg::Open(i) => {
                let tab = self.tab();
                tab.select_only(i);
                tab.open(i, &mut out);
            }
            Msg::ItemAction(i, a) => {
                let tab = self.tab();
                tab.target(i);
                tab.run(a, &mut out);
            }
            Msg::Action(a) => self.tab().run(a, &mut out),
            Msg::Deselect => {
                let tab = self.tab();
                tab.finish_rename(true);
                if !ctrl && !mods.shift {
                    tab.selection.clear();
                }
            }
            Msg::RenameText(t) => {
                if let Some((_, name)) = &mut self.tab().rename {
                    *name = t;
                }
            }
            Msg::RenameCommit => self.tab().finish_rename(true),
            Msg::RenameCancel => self.tab().finish_rename(false),
            Msg::DropFiles(dir, paths) => self.tab().dropped(&dir, corekit::dnd::Dropped::Files(paths), ctrl),
            Msg::DropText(dir, text) => self.tab().dropped(&dir, corekit::dnd::Dropped::Text(text), false),
            Msg::Confirm => self.tab().confirmed(),
            Msg::CancelConfirm => self.tab().confirm = None,
            Msg::CloseQuickLook => self.tab().quicklook = None,
            Msg::Move(d, extend) => {
                let cols = self.viewport.get().0;
                let tab = self.tab();
                let d = if d.abs() == 1000 { d.signum() * cols } else { d };
                tab.move_cursor(d, extend);
            }
            Msg::TypeChar(c) => self.tab().type_select(c, cx.now()),
            Msg::SelectAll => self.tab().select_all(),
            Msg::Decision(i) => self.decision(i),
            Msg::Dismiss => {
                let manager = self.transfers.lock();
                let mut s = manager.state.lock();
                if s.running {
                    s.cancel = true;
                } else {
                    s.summary.clear();
                }
            }
            Msg::NextError => self.error_index += 1,
            Msg::KeepOpen => self.close_prompt = false,
            Msg::CancelClose => self.cancel_close(),
        }
        for r in out {
            cx.request(r);
        }
        self.reveal(cx);
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        let tab = &self.tabs[self.active];
        let ctrl = ev.mods.ctrl || ev.mods.super_key;
        let lower = ev.ch.map(|c| c.to_ascii_lowercase());
        if tab.rename.is_some() && ev.code == KeyCode::Escape {
            return Some(Msg::RenameCancel);
        }
        if ctrl {
            if ev.code == KeyCode::Tab {
                let n = self.tabs.len();
                return Some(Msg::Tab((self.active + if ev.mods.shift { n - 1 } else { 1 }) % n));
            }
            if let Some(n) = ev.ch.and_then(|c| c.to_digit(10)).filter(|n| (1..=8).contains(n)) {
                if !ev.mods.shift && (n as usize) <= self.tabs.len() {
                    return Some(Msg::Tab(n as usize - 1));
                }
            }
            return Some(match (lower, ev.mods.shift) {
                (Some('a'), _) => Msg::SelectAll,
                (Some('c'), _) => Msg::Action(Action::Copy),
                (Some('x'), _) => Msg::Action(Action::Cut),
                (Some('v'), _) => Msg::Action(Action::Paste),
                (Some('d'), _) => Msg::Action(Action::Duplicate),
                (Some('o'), _) => Msg::Action(Action::Open),
                (Some('t'), false) => Msg::NewTab,
                (Some('n'), true) if tab.writable_here() => Msg::Action(Action::NewFolder),
                (Some('1' | '!'), true) => Msg::ViewMode(0),
                (Some('2' | '@'), true) => Msg::ViewMode(1),
                _ if ev.code == KeyCode::Backspace => Msg::Action(Action::Trash),
                _ => return None,
            });
        }
        if ev.mods.alt {
            return match ev.code {
                KeyCode::Left => Some(Msg::Back),
                KeyCode::Right => Some(Msg::Forward),
                KeyCode::Up => Some(Msg::Up),
                _ => None,
            };
        }
        let shift = ev.mods.shift;
        let icons = tab.view == ViewMode::Icons;
        Some(match ev.code {
            KeyCode::Right if icons => Msg::Move(1, shift),
            KeyCode::Left if icons => Msg::Move(-1, shift),
            KeyCode::Down => Msg::Move(if icons { 1000 } else { 1 }, shift),
            KeyCode::Up => Msg::Move(if icons { -1000 } else { -1 }, shift),
            KeyCode::Home => Msg::Move(-(tab.items.len() as i32), shift),
            KeyCode::End => Msg::Move(tab.items.len() as i32, shift),
            KeyCode::Enter => Msg::Action(Action::Open),
            KeyCode::F2 => Msg::Action(Action::Rename),
            KeyCode::Delete => Msg::Action(if tab.in_trash() { Action::DeleteForever } else { Action::Trash }),
            KeyCode::Backspace => Msg::Back,
            KeyCode::Escape if tab.quicklook.is_some() => Msg::CloseQuickLook,
            KeyCode::Escape if tab.searching() || !tab.query.is_empty() => Msg::Query(String::new()),
            KeyCode::Escape => Msg::Deselect,
            _ => match ev.ch {
                Some(' ') => Msg::Action(Action::QuickLook),
                Some(c) if !c.is_control() => Msg::TypeChar(c),
                _ => return None,
            },
        })
    }

    fn poll_interval(&self) -> Option<u64> {
        let tab = &self.tabs[self.active];
        let busy = self.transfers.lock().busy() || self.worker.lock().is_some() || self.closing || tab.busy();
        Some(if busy { 100 } else { 2000 })
    }

    fn poll(&mut self, cx: &mut Context<Msg>) -> bool {
        let (changed, close) = self.background(cx.now());
        if close {
            cx.close();
        }
        changed
    }

    fn close_requested(&mut self, _cx: &mut Context<Msg>) -> bool {
        self.ask_close()
    }

    fn close_tab_requested(&mut self, cx: &mut Context<Msg>) -> bool {
        if self.close_tab(self.active) {
            return false;
        }
        self.close_requested(cx)
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        if self.close_prompt {
            return Some(
                Alert::new("A transfer is running", "Keep this window open until it finishes, or stop it and close.")
                    .button("Stop and Close", Msg::CancelClose, AlertRole::Destructive)
                    .button("Keep Open", Msg::KeepOpen, AlertRole::Default)
                    .view(),
            );
        }
        let tab = &self.tabs[self.active];
        if let Some(c) = &tab.confirm {
            let (title, message) = match c {
                Confirm::DeleteForever(paths) => (
                    if paths.len() == 1 {
                        format!("Delete “{}” immediately?", fs::file_name(&paths[0]))
                    } else {
                        format!("Delete {} items immediately?", paths.len())
                    },
                    "You can't undo this action.",
                ),
                Confirm::EmptyTrash => (
                    String::from("Empty the Trash?"),
                    "The items in the Trash will be deleted immediately. You can't undo this action.",
                ),
            };
            return Some(
                Alert::new(title, message)
                    .button("Cancel", Msg::CancelConfirm, AlertRole::Cancel)
                    .button("Delete", Msg::Confirm, AlertRole::Destructive)
                    .view(),
            );
        }
        let q = tab.quicklook.as_ref()?;
        let item = tab.items.get(q.index)?;
        let preview: View<'_, Msg> = if let Some(img) = &q.image {
            let (w, h) = aurora_image::fit(img.width, img.height, 560, 400);
            Canvas::new(move |cv, r| cv.draw_image(&img.pixels, img.width, img.height, r, 255)).size(w as i32, h as i32)
        } else if let Some(text) = &q.text {
            ScrollView::new(
                Text::paragraph(text.clone())
                    .style(TextStyle::Sized(12, waveui::style::Weight::Mono))
                    .padding(12)
                    .fill_width(),
            )
            .size(560, 320)
            .background_rounded(Color::GroupedBackground, 10)
        } else {
            Image::app_icon(icon_of(item), 96).padding(20)
        };
        let detail = if item.is_dir {
            String::from(kind_of(item))
        } else {
            format!("{} · {}", kind_of(item), human_size(item.size))
        };
        Some(
            VStack::new()
                .push(Text::new(item.name.clone()).style(TextStyle::Title2))
                .push(Text::new(detail).foreground(Color::SecondaryLabel))
                .push(preview)
                .push(
                    HStack::new()
                        .push(Spacer::new())
                        .push(Button::new("Open").action(Msg::Open(q.index)))
                        .push(
                            Button::new("Close")
                                .action(Msg::CloseQuickLook)
                                .button_style(ButtonStyle::Prominent)
                                .shortcut(Shortcut::cancel()),
                        )
                        .spacing(8),
                )
                .spacing(10)
                .alignment(Align::Center),
        )
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let main = VStack::new()
            .push(self.toolbar())
            .push(self.content(env))
            .push(Divider::new())
            .push(self.status_bar())
            .spacing(0)
            .fill();
        let detail = if self.info_width(env) > 0 {
            HStack::new()
                .push(main)
                .push(View::empty().width(1).fill_height().background(Color::Separator))
                .push(self.info_panel())
                .spacing(0)
                .fill()
        } else {
            main
        };
        VStack::new()
            .push_if(self.tab_bar())
            .push(SplitView::new(self.sidebar(), detail).sidebar_width(SIDEBAR_W))
            .push_if(self.panel().map(|p| VStack::new().push(Divider::new()).push(p).spacing(0)))
            .spacing(0)
            .fill()
    }
}

/// Draws `img` to cover `r` (cropping the overflow), with rounded corners.
fn cover(cv: &mut Surface, img: &Picture, r: Rect, radius: i32) {
    let (iw, ih) = (img.width as i64, img.height as i64);
    if iw == 0 || ih == 0 {
        return;
    }
    // Scale so the picture covers the box, centred.
    let (w, h) = if iw * r.h as i64 > ih * r.w as i64 {
        (iw * r.h as i64 / ih, r.h as i64)
    } else {
        (r.w as i64, ih * r.w as i64 / iw)
    };
    let dst = Rect::new(r.x + (r.w - w as i32) / 2, r.y + (r.h - h as i32) / 2, w as i32, h as i32);
    cv.draw_image_rounded(&img.pixels, img.width, img.height, dst, r, radius);
}
