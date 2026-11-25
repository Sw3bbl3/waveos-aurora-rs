//! Store — find, install, update and remove apps from the repositories
//! (`corekit::apps::repo`), with each app's page (pictures, description,
//! what's new, the permissions it asks for), a queue of downloads that can
//! be paused, resumed and cancelled, Updates (with Update All, and WaveOS
//! itself) and the Library of what is installed.
//!
//! The Store adds no trust of its own: catalogues, pictures and packages
//! are checked by `corekit` against the repositories' signatures before
//! use. Pictures load in the background and are cached on disk; pages move
//! with the desktop's springs (`lumen::spring`); arrows, Enter and Escape —
//! or a game controller's d-pad, A and B — get everywhere.
//!
//! `Store.elf --category game` opens on a category; `Store.elf --check` is
//! the background check for updates the desktop runs (no window).

#![no_std]
#![no_main]

extern crate alloc;

mod check;
mod model;

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use core::cell::{Cell, RefCell};
use corekit::apps::{self, repo::Catalog, Installed, Scope};
use corekit::gamepad::Button as Pad;
use gina::repo::{Asset, Entry};
use model::{category_name, human, Job, JobState, Pic, Shelf, State, SystemState, Work};
use waveui::lumen::icons::Icon;
use waveui::lumen::spring::{self, Spring};
use waveui::prelude::*;
use waveui::style::Weight;
use waveui::Pixmap;

/// Card width in the grids.
const CARD_W: i32 = 210;
/// Icons are decoded at most this big; screenshots at most this wide.
const ICON_PX: u32 = 128;
const SHOT_PX: u32 = 720;

#[derive(Clone, PartialEq, Eq, Debug)]
enum Page {
    Discover,
    Category(&'static str),
    Search,
    App(String),
    Updates,
    Library,
}

#[derive(Clone)]
enum Msg {
    Go(Page),
    Back,
    Search(String),
    Open(String),
    Get(String),
    Update(String),
    UpdateAll,
    Pause(String),
    Resume(String),
    Cancel(String),
    Launch(String),
    AskRemove(Option<String>),
    DeleteData(bool),
    Remove(String),
    Rollback(String),
    Scope(usize),
    Refresh,
    OpenSoftwareUpdate,
    Move(i32, i32),
    Activate,
    Section(i32),
    NewSource(String),
    AddSource,
    Dismiss,
}

/// Something the keyboard or a controller can land on.
#[derive(Clone)]
struct Target {
    key: String,
    open: Msg,
}

struct Store {
    state: State,
    page: Page,
    history: Vec<Page>,
    query: String,
    shelf: Shelf,
    shelf_gen: u64,
    jobs: Vec<Job>,
    jobs_gen: u64,
    system: SystemState,
    system_gen: u64,
    installed: Vec<Installed>,
    /// Pictures decoded so far (by hash), and what the last view asked for.
    pictures: BTreeMap<[u8; 32], Rc<Pixmap>>,
    pictures_gen: u64,
    asked: RefCell<Vec<(Catalog, Asset, u32)>>,
    local_icons: RefCell<BTreeMap<String, Option<Rc<Pixmap>>>>,
    /// Page transitions and the featured banner's crossfade.
    page_t: Spring,
    hero: usize,
    hero_t: Spring,
    hero_at: u64,
    scope: usize,
    removing: Option<String>,
    delete_data: bool,
    note: Option<String>,
    new_source: String,
    /// The keyboard/controller position among the page's targets.
    cursor: Option<usize>,
    targets: RefCell<Vec<Target>>,
    columns: Cell<i32>,
    pad_prev: u32,
    pad_at: u64,
    active: Cell<bool>,
    reduce_motion: Cell<bool>,
}

fn now() -> u64 {
    corekit::time::uptime_ms()
}

impl Store {
    fn new(start: Page) -> Store {
        let state = model::new_state();
        model::refresh(&state);
        model::start_worker(&state);
        model::start_pictures(&state);
        model::check_system(&state);
        let t = now();
        let mut page_t = Spring::new(0, spring::SMOOTH);
        page_t.set_target(1000, t);
        Store {
            state,
            page: start,
            history: Vec::new(),
            query: String::new(),
            shelf: Shelf::default(),
            shelf_gen: 0,
            jobs: Vec::new(),
            jobs_gen: 0,
            system: SystemState::Unknown,
            system_gen: 0,
            installed: apps::list(),
            pictures: BTreeMap::new(),
            pictures_gen: 0,
            asked: RefCell::new(Vec::new()),
            local_icons: RefCell::new(BTreeMap::new()),
            page_t,
            hero: 0,
            hero_t: Spring::new(1000, spring::SMOOTH),
            hero_at: t,
            scope: if corekit::users::is_admin() { 0 } else { 1 },
            removing: None,
            delete_data: false,
            note: None,
            new_source: String::new(),
            cursor: None,
            targets: RefCell::new(Vec::new()),
            columns: Cell::new(3),
            pad_prev: 0,
            pad_at: 0,
            active: Cell::new(true),
            reduce_motion: Cell::new(false),
        }
    }

    fn go(&mut self, page: Page, cx: &mut Context<Msg>) {
        if page == self.page {
            return;
        }
        let old = core::mem::replace(&mut self.page, page);
        if !matches!(old, Page::Search) || !matches!(self.page, Page::Search) {
            self.history.push(old);
        }
        if self.history.len() > 32 {
            self.history.remove(0);
        }
        self.cursor = None;
        self.removing = None;
        self.enter(cx);
    }

    /// A new page slides and fades in.
    fn enter(&mut self, cx: &mut Context<Msg>) {
        let t = now();
        if self.reduce_motion.get() {
            self.page_t.snap(1000, t);
        } else {
            self.page_t.snap(0, t);
            self.page_t.set_target(1000, t);
        }
        cx.scroll_to(&"page", 0.0);
        if matches!(self.page, Page::Library | Page::Updates) {
            self.installed = apps::list();
        }
    }

    fn installed(&self, id: &str) -> Option<&Installed> {
        self.installed.iter().find(|a| a.manifest.id == id)
    }

    fn job(&self, id: &str) -> Option<&Job> {
        self.jobs.iter().rev().find(|j| j.id == id)
    }

    fn updates(&self) -> Vec<(&Installed, usize, &Entry)> {
        let mut out = Vec::new();
        for a in &self.installed {
            if let Some((ci, e)) = self.shelf.app(&a.manifest.id) {
                if gina::compare_versions(&e.version, &a.manifest.version).is_gt() {
                    out.push((a, *ci, e));
                }
            }
        }
        out
    }

    /// The picture with this hash, or ask for it.
    fn picture(&self, ci: usize, asset: &Asset, max: u32) -> Option<Rc<Pixmap>> {
        if let Some(p) = self.pictures.get(&asset.sha256) {
            return Some(p.clone());
        }
        if let Some(c) = self.shelf.catalogs.get(ci) {
            self.asked.borrow_mut().push((c.clone(), asset.clone(), max));
        }
        None
    }

    fn queue_install(&mut self, id: &str, cx: &mut Context<Msg>) {
        let Some((ci, e)) = self.shelf.app(id).cloned() else { return };
        let Some(c) = self.shelf.catalogs.get(ci).cloned() else { return };
        let scope = if self.scope == 0 { Scope::System } else { Scope::User };
        let name = e.name.clone();
        model::queue(&self.state, id, &name, Work::Install(scope), Some((c, e)));
        self.sync(cx);
    }

    /// Takes what the background threads changed. True if anything did.
    fn sync(&mut self, _cx: &mut Context<Msg>) -> bool {
        let mut changed = false;
        let mut s = self.state.lock();
        if s.shelf_gen != self.shelf_gen {
            self.shelf_gen = s.shelf_gen;
            if let Some(shelf) = &s.shelf {
                self.shelf = shelf.clone();
            }
            changed = true;
        }
        if s.jobs_gen != self.jobs_gen {
            let finished = self.jobs.iter().any(|j| j.active())
                && s.jobs
                    .iter()
                    .any(|j| j.state == JobState::Done && self.jobs.iter().any(|o| o.id == j.id && o.active()));
            self.jobs_gen = s.jobs_gen;
            self.jobs = s.jobs.clone();
            if finished {
                self.installed = apps::list();
            }
            changed = true;
        }
        if s.system_gen != self.system_gen {
            self.system_gen = s.system_gen;
            self.system = s.system.clone();
            changed = true;
        }
        if s.pictures_gen != self.pictures_gen {
            self.pictures_gen = s.pictures_gen;
            for (k, p) in s.pictures.iter_mut() {
                if let Pic::Ready(pic) = p {
                    if !self.pictures.contains_key(k) {
                        let (w, h, px) = core::mem::take(pic);
                        self.pictures.insert(*k, Rc::new(Pixmap { width: w, height: h, pixels: px }));
                    }
                }
            }
            changed = true;
        }
        drop(s);
        for (c, a, max) in self.asked.borrow_mut().drain(..) {
            model::want_picture(&self.state, &c, &a, max);
        }
        changed
    }

    fn moving(&self) -> bool {
        !self.page_t.settled() || !self.hero_t.settled()
    }

    // ----------------------------------------------------------- pieces

    fn icon<'a>(&self, ci: usize, e: &Entry, size: i32) -> View<'a, Msg> {
        // An installed app's own icon is on disk already.
        let pic = match self.installed(&e.id).and_then(|a| a.icon()) {
            Some(path) => self.local_icon(&path),
            None => e.icon.as_ref().and_then(|a| self.picture(ci, a, ICON_PX)),
        };
        match pic {
            Some(p) => Image::new(p).size(size, size),
            None => {
                let symbol = if e.categories.iter().any(|c| c == "game") { Symbol::Gamepad } else { Symbol::Grid };
                Image::symbol(symbol)
                    .symbol_size(size * 2 / 5)
                    .foreground(Color::Custom(0xFFFF_FFFF))
                    .size(size, size)
                    .background_rounded(Color::Custom(tint(&e.id)), size * 9 / 40)
            }
        }
    }

    /// The button an app shows: Get, Update, Open, or its download.
    fn action<'a>(&self, id: &str, large: bool) -> View<'a, Msg> {
        let size = if large { ControlSize::Large } else { ControlSize::Small };
        if let Some(j) = self.job(id).filter(|j| j.active()) {
            let fraction = j.fraction();
            let label = match &j.state {
                JobState::Queued => String::from("Waiting"),
                JobState::Downloading(n, t) => format!("{} of {}", human(*n), human(*t)),
                JobState::Paused(..) => String::from("Paused"),
                JobState::Verifying => String::from("Checking"),
                JobState::Installing(..) => String::from("Installing"),
                _ => String::new(),
            };
            let paused = matches!(j.state, JobState::Paused(..));
            let downloading = matches!(j.state, JobState::Downloading(..) | JobState::Queued);
            let ring = progress_ring(fraction, if large { 30 } else { 24 });
            let mut row = HStack::new().push(ring).push(
                Text::new(label)
                    .style(TextStyle::Sized(12, Weight::Regular))
                    .foreground(Color::SecondaryLabel)
                    .lines(1),
            );
            if matches!(j.work, Work::Install(_)) {
                if paused {
                    row = row.push(
                        Button::symbol(Symbol::Play)
                            .action(Msg::Resume(id.into()))
                            .help("Resume")
                            .control_size(ControlSize::Small),
                    );
                } else if downloading {
                    row = row.push(
                        Button::symbol(Symbol::Pause)
                            .action(Msg::Pause(id.into()))
                            .help("Pause")
                            .control_size(ControlSize::Small),
                    );
                }
                if paused || downloading {
                    row = row.push(
                        Button::symbol(Symbol::Close)
                            .action(Msg::Cancel(id.into()))
                            .help("Cancel")
                            .control_size(ControlSize::Small),
                    );
                }
            }
            return row.spacing(6);
        }
        let failed = self.job(id).and_then(|j| if let JobState::Failed(e) = &j.state { Some(e.clone()) } else { None });
        let latest = self.shelf.app(id).map(|(_, e)| e.version.clone());
        let button = match (self.installed(id), latest) {
            (Some(a), Some(v)) if gina::compare_versions(&v, &a.manifest.version).is_gt() => {
                Button::new("Update").action(Msg::Update(id.into())).button_style(ButtonStyle::Prominent)
            }
            (Some(_), _) => Button::new("Open").action(Msg::Launch(id.into())),
            (None, Some(_)) => Button::new("Get").action(Msg::Get(id.into())).button_style(ButtonStyle::Prominent),
            (None, None) => Button::new("Unavailable").disabled(true),
        }
        .control_size(size)
        .min_width(if large { 120 } else { 72 });
        match failed {
            Some(e) => VStack::new()
                .push(button)
                .push(Image::symbol(Symbol::Warning).symbol_size(12).foreground(Color::Orange).help(e))
                .spacing(4),
            None => button,
        }
    }

    fn card<'a>(&self, ci: usize, e: &Entry) -> View<'a, Msg> {
        let id = e.id.clone();
        let cat = e.categories.first().map(|c| category_name(c)).unwrap_or_else(|| String::from("App"));
        self.targets.borrow_mut().push(Target { key: format!("card:{id}"), open: Msg::Open(id.clone()) });
        VStack::new()
            .push(self.icon(ci, e, 64))
            .push(
                VStack::new()
                    .push(Text::new(e.name.clone()).style(TextStyle::Sized(15, Weight::Bold)).lines(1))
                    .push(
                        Text::new(e.author.clone())
                            .style(TextStyle::Footnote)
                            .foreground(Color::SecondaryLabel)
                            .lines(1),
                    )
                    .push(Text::new(cat).style(TextStyle::Footnote).foreground(Color::TertiaryLabel).lines(1))
                    .spacing(2)
                    .alignment(Align::Start)
                    .fill_width(),
            )
            .push(Spacer::new())
            .push(HStack::new().push(Spacer::new()).push(self.action(&id, false)).fill_width())
            .spacing(10)
            .alignment(Align::Start)
            .padding(14)
            .height(214)
            .fill_width()
            .background_rounded(Color::Fill, 16)
            .border(Color::Separator, 16)
            .on_tap(Msg::Open(id.clone()))
            .cursor(waveui::cursor::POINTER)
            .id(format!("card:{id}"))
    }

    fn grid<'a>(&self, list: &[(usize, Entry)]) -> View<'a, Msg> {
        Grid::new(CARD_W).spacing(14).extend(list.iter().map(|(ci, e)| self.card(*ci, e))).fill_width()
    }

    fn heading<'a>(title: &str, subtitle: Option<String>) -> View<'a, Msg> {
        VStack::new()
            .push(Text::new(title).style(TextStyle::Sized(30, Weight::Display)))
            .push_if(
                subtitle.map(|s| {
                    Text::paragraph(s).style(TextStyle::Callout).foreground(Color::SecondaryLabel).fill_width()
                }),
            )
            .spacing(4)
            .alignment(Align::Start)
            .fill_width()
    }

    fn section<'a>(title: &str, body: View<'a, Msg>) -> View<'a, Msg> {
        VStack::new()
            .push(Text::new(title).style(TextStyle::Sized(19, Weight::Bold)))
            .push(body)
            .spacing(12)
            .alignment(Align::Stretch)
            .fill_width()
    }

    /// The featured banner: one app at a time, cross-fading every few seconds.
    fn hero<'a>(&self) -> Option<View<'a, Msg>> {
        let featured: Vec<&(usize, Entry)> = self.shelf.featured.iter().filter_map(|id| self.shelf.app(id)).collect();
        let (ci, e) = *featured.get(self.hero % featured.len().max(1))?;
        let fade = (self.hero_t.value().clamp(0, 1000) * 255 / 1000) as u8;
        let (from, to) = (tint(&e.id), mix(tint(&e.id), 0xFF10_1828, 150));
        let backdrop = Canvas::new(move |cv, r| {
            cv.fill_round_rect_dgradient(r, 22, from, to);
            // Soft rings, like light on water.
            for i in 0..3 {
                let rr = r.h / 2 + i * 46;
                arc(cv, r.right() - 120, r.y + r.h / 2, rr, 2, 0, 360, 0x22FF_FFFF);
            }
        });
        self.targets.borrow_mut().push(Target { key: String::from("hero"), open: Msg::Open(e.id.clone()) });
        let text = VStack::new()
            .push(
                Text::new("FEATURED").style(TextStyle::Sized(11, Weight::Bold)).foreground(Color::Custom(0xCCFF_FFFF)),
            )
            .push(
                Text::new(e.name.clone())
                    .style(TextStyle::Sized(34, Weight::Display))
                    .foreground(Color::Custom(0xFFFF_FFFF)),
            )
            .push(
                Text::paragraph(if e.about.is_empty() { e.description.clone() } else { first_line(&e.about) })
                    .style(TextStyle::Callout)
                    .foreground(Color::Custom(0xE6FF_FFFF))
                    .lines(2),
            )
            .push(
                HStack::new()
                    .push(self.action(&e.id, true))
                    .push(Text::new(e.author.clone()).style(TextStyle::Footnote).foreground(Color::Custom(0xCCFF_FFFF)))
                    .spacing(14),
            )
            .spacing(8)
            .alignment(Align::Start)
            .grow(1);
        let content = HStack::new()
            .push(self.icon(*ci, e, 112))
            .push(text)
            .spacing(24)
            .padding_insets(Insets { top: 28, leading: 32, bottom: 28, trailing: 32 })
            .fill_width()
            .opacity(fade);
        Some(
            ZStack::with(vec![backdrop.fill(), content])
                .height(220)
                .fill_width()
                .on_tap(Msg::Open(e.id.clone()))
                .id("hero"),
        )
    }

    fn discover<'a>(&self) -> View<'a, Msg> {
        let mut v = VStack::new().spacing(28).alignment(Align::Stretch);
        v = v.push(Self::heading(
            "Discover",
            Some(String::from("Apps and games, checked against their publishers' signatures.")),
        ));
        v = v.push_if(self.hero());
        let mut games: Vec<(usize, Entry)> =
            self.shelf.apps.iter().filter(|(_, e)| e.categories.iter().any(|c| c == "game")).cloned().collect();
        let mut apps_: Vec<(usize, Entry)> =
            self.shelf.apps.iter().filter(|(_, e)| !e.categories.iter().any(|c| c == "game")).cloned().collect();
        let mut new: Vec<(usize, Entry)> = self.shelf.apps.clone();
        new.sort_by_key(|e| core::cmp::Reverse(e.1.published));
        new.truncate(6);
        if !new.is_empty() {
            v = v.push(Self::section("New and Updated", self.grid(&new)));
        }
        games.truncate(8);
        apps_.truncate(8);
        if !games.is_empty() {
            v = v.push(Self::section("Games", self.grid(&games)));
        }
        if !apps_.is_empty() {
            v = v.push(Self::section("Apps", self.grid(&apps_)));
        }
        let chips = self.shelf.categories().into_iter().map(|c| {
            Button::new(category_name(c)).action(Msg::Go(Page::Category(c))).control_size(ControlSize::Regular)
        });
        v.push(Self::section("Categories", HStack::new().extend(chips).spacing(8)))
    }

    fn category<'a>(&self, c: &'static str) -> View<'a, Msg> {
        let list: Vec<(usize, Entry)> =
            self.shelf.apps.iter().filter(|(_, e)| e.categories.iter().any(|x| x == c)).cloned().collect();
        let n = list.len();
        VStack::new()
            .push(Self::heading(&category_name(c), Some(format!("{n} {}", if n == 1 { "app" } else { "apps" }))))
            .push(if list.is_empty() {
                empty("Nothing here yet.", "No repository offers apps in this category.")
            } else {
                self.grid(&list)
            })
            .spacing(22)
            .alignment(Align::Stretch)
    }

    fn search<'a>(&self) -> View<'a, Msg> {
        let found = self.shelf.search(&self.query);
        VStack::new()
            .push(Self::heading(&format!("Results for “{}”", self.query), None))
            .push(if found.is_empty() {
                empty("No results", "Try a shorter word, or a category in the sidebar.")
            } else {
                self.grid(&found)
            })
            .spacing(22)
            .alignment(Align::Stretch)
    }

    fn app<'a>(&self, id: &str) -> View<'a, Msg> {
        let Some((ci, e)) = self.shelf.app(id).cloned() else {
            return empty("This app isn't available", "No configured repository offers it now.");
        };
        let installed = self.installed(id).cloned();
        let catalog = self.shelf.catalogs.get(ci);
        let cats: Vec<String> = e.categories.iter().map(|c| category_name(c)).collect();
        self.targets.borrow_mut().push(Target { key: String::from("primary"), open: Msg::Get(id.into()) });
        let header = HStack::new()
            .push(self.icon(ci, &e, 112))
            .push(
                VStack::new()
                    .push(Text::new(e.name.clone()).style(TextStyle::Sized(30, Weight::Display)))
                    .push(Text::new(e.author.clone()).style(TextStyle::Callout).foreground(Color::AccentText))
                    .push(Text::new(cats.join(" · ")).style(TextStyle::Footnote).foreground(Color::SecondaryLabel))
                    .push(
                        HStack::new()
                            .push(self.action(id, true))
                            .push_if(installed.is_none().then(|| {
                                Picker::new(["For everyone", "Only for me"], self.scope)
                                    .on_change(Msg::Scope)
                                    .help("Everyone needs an administrator's approval")
                            }))
                            .spacing(12)
                            .id("primary"),
                    )
                    .spacing(4)
                    .alignment(Align::Start)
                    .grow(1),
            )
            .spacing(24)
            .alignment(Align::Start)
            .fill_width();
        let fact = |label: &str, value: String| {
            VStack::new()
                .push(
                    Text::new(String::from(label))
                        .style(TextStyle::Sized(11, Weight::Bold))
                        .foreground(Color::SecondaryLabel),
                )
                .push(Text::new(value).style(TextStyle::Sized(15, Weight::Bold)).lines(1))
                .spacing(4)
                .alignment(Align::Center)
                .grow(1)
        };
        let facts = HStack::new()
            .push(fact("VERSION", e.version.clone()))
            .push(Divider::new().height(36).width(1))
            .push(fact("SIZE", human(if e.installed_size > 0 { e.installed_size } else { e.size })))
            .push(Divider::new().height(36).width(1))
            .push(fact("CATEGORY", cats.first().cloned().unwrap_or_else(|| String::from("App"))))
            .push(Divider::new().height(36).width(1))
            .push(fact(
                "REQUIRES",
                if e.min_os.is_empty() { String::from("Any WaveOS") } else { format!("WaveOS {}", e.min_os) },
            ))
            .spacing(8)
            .padding_xy(8, 14)
            .fill_width()
            .background_rounded(Color::Fill, 14);
        let mut page = VStack::new()
            .push(
                HStack::new()
                    .push(
                        Button::with(label(Symbol::ChevronLeft, "Back"))
                            .action(Msg::Back)
                            .button_style(ButtonStyle::Plain),
                    )
                    .push(Spacer::new())
                    .fill_width(),
            )
            .push(header)
            .push(facts)
            .spacing(22)
            .alignment(Align::Stretch);
        if !e.screenshots.is_empty() {
            let shots = HStack::new()
                .extend(e.screenshots.iter().map(|a| match self.picture(ci, a, SHOT_PX) {
                    Some(p) => {
                        let h = 260;
                        let w = (p.width as i32 * h / p.height.max(1) as i32).clamp(120, 520);
                        Image::new(p).size(w, h).corner_radius(14).border(Color::Separator, 14)
                    }
                    None => shimmer(420, 260),
                }))
                .spacing(14)
                .padding_xy(0, 4);
            page = page.push(Self::section("Pictures", ScrollView::horizontal(shots).height(276).fill_width()));
        }
        let about = if e.about.is_empty() { e.description.clone() } else { e.about.clone() };
        if !about.is_empty() {
            page = page.push(Self::section("About", Text::paragraph(about).fill_width()));
        }
        if !e.notes.is_empty() {
            page = page.push(Self::section(
                "What's New",
                VStack::new()
                    .push(
                        Text::new(format!("Version {}", e.version))
                            .style(TextStyle::Footnote)
                            .foreground(Color::SecondaryLabel),
                    )
                    .push(Text::paragraph(e.notes.clone()).fill_width())
                    .spacing(6)
                    .alignment(Align::Start),
            ));
        }
        let mut perms = VStack::new().spacing(0).alignment(Align::Stretch);
        if e.permissions.is_empty() {
            perms = perms.push(
                row(
                    "It asks for no special permissions",
                    Image::symbol(Symbol::Check).symbol_size(14).foreground(Color::Green),
                )
                .height(size::ROW),
            );
        }
        for (i, p) in e.permissions.iter().enumerate() {
            let sensitive = gina::sensitive_permission(p);
            if i > 0 {
                perms = perms.push(Divider::new());
            }
            perms = perms.push(
                HStack::new()
                    .push(
                        Image::symbol(if sensitive { Symbol::Warning } else { Symbol::Lock })
                            .symbol_size(14)
                            .foreground(if sensitive { Color::Orange } else { Color::SecondaryLabel })
                            .width(20),
                    )
                    .push(Text::new(gina::describe_permission(p)).grow(1))
                    .push(Text::new(p.clone()).style(TextStyle::Footnote).foreground(Color::TertiaryLabel))
                    .spacing(10)
                    .height(size::ROW)
                    .fill_width(),
            );
        }
        let perms = perms.padding_xy(16, 2).fill_width().background_rounded(Color::Fill, 14);
        page = page.push(Self::section("Permissions", perms));
        let mut info = Section::new()
            .push(row("Developer", Text::new(e.author.clone()).foreground(Color::SecondaryLabel)))
            .push(row("Identifier", Text::new(e.id.clone()).foreground(Color::SecondaryLabel)))
            .push(row(
                "Download",
                Text::new(format!("{} · installed {}", human(e.size), human(e.installed_size)))
                    .foreground(Color::SecondaryLabel),
            ));
        if let Some(c) = catalog {
            info = info.push(row(
                "Repository",
                Text::new(format!("{} — catalogue signed, version {}", c.index.name, c.index.version))
                    .foreground(Color::SecondaryLabel),
            ));
        }
        let versions = self.shelf.versions(id);
        if versions.len() > 1 {
            let list: Vec<String> = versions.iter().map(|v| v.version.clone()).collect();
            info = info.push(row("Versions", Text::new(list.join(", ")).foreground(Color::SecondaryLabel)));
        }
        page = page.push(Self::section("Information", info));
        if let Some(a) = installed {
            page = page.push(self.installed_card(&a));
        }
        page
    }

    /// An installed app's card: where it is, and Open, Remove, Revert.
    fn installed_card<'a>(&self, a: &Installed) -> View<'a, Msg> {
        let id = a.manifest.id.clone();
        let who = if a.scope == Scope::System { "everyone" } else { "you only" };
        let mut buttons = HStack::new().push(Button::new("Open").action(Msg::Launch(id.clone()))).spacing(8);
        if let Some(prev) = apps::previous_version(&id) {
            buttons = buttons.push(Button::new(format!("Revert to {prev}")).action(Msg::Rollback(id.clone())));
        }
        let removing = self.removing.as_deref() == Some(id.as_str());
        buttons = if removing {
            buttons
                .push(Toggle::checkbox("Also delete its data", self.delete_data).on_change(Msg::DeleteData))
                .push(Button::new("Keep").action(Msg::AskRemove(None)))
                .push(Button::new("Remove").action(Msg::Remove(id.clone())).button_style(ButtonStyle::Destructive))
        } else {
            buttons.push(Button::new("Remove…").action(Msg::AskRemove(Some(id.clone()))))
        };
        Self::section(
            "Installed",
            VStack::new()
                .push(Text::new(format!("Version {} for {who} · {}", a.manifest.version, human(a.receipt.size))))
                .push(
                    Text::new(if a.receipt.publisher.is_empty() {
                        String::from("Not signed by a publisher you trust")
                    } else {
                        format!("Signed by {}", a.receipt.publisher)
                    })
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel),
                )
                .push(buttons)
                .spacing(8)
                .alignment(Align::Start)
                .padding(16)
                .fill_width()
                .background_rounded(Color::Fill, 14),
        )
    }

    fn updates_page<'a>(&self) -> View<'a, Msg> {
        let ups = self.updates();
        let mut v = VStack::new().spacing(22).alignment(Align::Stretch);
        let head = HStack::new()
            .push(
                Self::heading(
                    "Updates",
                    Some(match ups.len() {
                        0 => String::from("Your apps are up to date."),
                        1 => String::from("1 update is available."),
                        n => format!("{n} updates are available."),
                    }),
                )
                .grow(1),
            )
            .push(Button::symbol(Symbol::Refresh).action(Msg::Refresh).help("Check again"))
            .push_if((ups.len() > 1).then(|| {
                Button::new("Update All")
                    .action(Msg::UpdateAll)
                    .button_style(ButtonStyle::Prominent)
                    .control_size(ControlSize::Large)
            }))
            .spacing(10)
            .fill_width();
        v = v.push(head);
        // WaveOS itself, as Software Update sees it.
        let system = match &self.system {
            SystemState::Available { version, notes, download } => Some(
                HStack::new()
                    .push(Image::app_icon(Icon::Aurora, 48))
                    .push(
                        VStack::new()
                            .push(Text::new(format!("WaveOS {version}")).style(TextStyle::Sized(15, Weight::Bold)))
                            .push(
                                Text::new(format!(
                                    "{} to download · installs alongside, then a restart",
                                    human(*download)
                                ))
                                .style(TextStyle::Footnote)
                                .foreground(Color::SecondaryLabel),
                            )
                            .push_if(
                                (!notes.is_empty())
                                    .then(|| Text::paragraph(first_line(notes)).style(TextStyle::Callout).lines(2)),
                            )
                            .spacing(3)
                            .alignment(Align::Start)
                            .grow(1),
                    )
                    .push(
                        Button::new("Software Update…")
                            .action(Msg::OpenSoftwareUpdate)
                            .button_style(ButtonStyle::Prominent),
                    )
                    .spacing(14)
                    .padding(16)
                    .fill_width()
                    .background_rounded(Color::Fill, 16)
                    .id("system"),
            ),
            SystemState::Checking => Some(
                HStack::new()
                    .push(ProgressView::spinner())
                    .push(Text::new("Checking for WaveOS updates…").foreground(Color::SecondaryLabel))
                    .spacing(10),
            ),
            SystemState::Failed(e) => Some(
                Text::paragraph(format!("Couldn't check for WaveOS updates: {e}"))
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel),
            ),
            _ => None,
        };
        if matches!(self.system, SystemState::Available { .. }) {
            self.targets.borrow_mut().push(Target { key: String::from("system"), open: Msg::OpenSoftwareUpdate });
        }
        v = v.push_if(system);
        if ups.is_empty() {
            v = v.push(empty_with(
                Symbol::Check,
                "Everything is up to date",
                "New versions of your apps appear here as their repositories publish them.",
            ));
        }
        for (a, ci, e) in ups {
            self.targets.borrow_mut().push(Target { key: format!("row:{}", e.id), open: Msg::Open(e.id.clone()) });
            v = v.push(
                HStack::new()
                    .push(self.icon(ci, e, 52))
                    .push(
                        VStack::new()
                            .push(Text::new(e.name.clone()).style(TextStyle::Sized(15, Weight::Bold)))
                            .push(
                                Text::new(format!("{} → {} · {}", a.manifest.version, e.version, human(e.size)))
                                    .style(TextStyle::Footnote)
                                    .foreground(Color::SecondaryLabel),
                            )
                            .push_if(
                                (!e.notes.is_empty())
                                    .then(|| Text::paragraph(e.notes.clone()).style(TextStyle::Callout).lines(3)),
                            )
                            .spacing(3)
                            .alignment(Align::Start)
                            .grow(1),
                    )
                    .push(self.action(&e.id, false))
                    .spacing(14)
                    .padding(14)
                    .fill_width()
                    .background_rounded(Color::Fill, 16)
                    .on_tap(Msg::Open(e.id.clone()))
                    .id(format!("row:{}", e.id)),
            );
        }
        let catalogs = &self.shelf.catalogs;
        let revoked: Vec<&Installed> = self
            .installed
            .iter()
            .filter(|a| catalogs.iter().any(|c| c.index.is_revoked(&a.manifest.id, &a.manifest.version)))
            .collect();
        for a in revoked {
            v = v.push(
                HStack::new()
                    .push(Image::symbol(Symbol::Warning).symbol_size(16).foreground(Color::Orange))
                    .push(
                        Text::paragraph(format!(
                            "{} {} was withdrawn by its repository. Remove it or update it.",
                            a.manifest.name, a.manifest.version
                        ))
                        .grow(1),
                    )
                    .spacing(10)
                    .padding(14)
                    .fill_width()
                    .background_rounded(Color::Fill, 14),
            );
        }
        v
    }

    fn library<'a>(&self) -> View<'a, Msg> {
        let mut v = VStack::new()
            .push(Self::heading(
                "Library",
                Some(match self.installed.len() {
                    0 => String::from("Apps you install appear here."),
                    1 => String::from("1 app installed."),
                    n => format!("{n} apps installed."),
                }),
            ))
            .spacing(14)
            .alignment(Align::Stretch);
        if self.installed.is_empty() {
            return v.push(empty("Nothing installed yet", "Find something in Discover."));
        }
        for a in &self.installed {
            let id = a.manifest.id.clone();
            let icon = a.icon().and_then(|p| self.local_icon(&p));
            let pic = match icon {
                Some(p) => Image::new(p).size(48, 48),
                None => Image::app_icon(Icon::Aurora, 48),
            };
            self.targets.borrow_mut().push(Target { key: format!("lib:{id}"), open: Msg::Open(id.clone()) });
            let removing = self.removing.as_deref() == Some(id.as_str());
            let mut row = HStack::new()
                .push(pic)
                .push(
                    VStack::new()
                        .push(Text::new(a.manifest.name.clone()).style(TextStyle::Sized(15, Weight::Bold)))
                        .push(
                            Text::new(format!(
                                "{} · {} · {}",
                                a.manifest.version,
                                human(a.receipt.size),
                                if a.scope == Scope::System { "Everyone" } else { "Only you" }
                            ))
                            .style(TextStyle::Footnote)
                            .foreground(Color::SecondaryLabel),
                        )
                        .spacing(3)
                        .alignment(Align::Start)
                        .grow(1),
                )
                .spacing(14);
            row = if removing {
                row.push(Toggle::checkbox("Delete data", self.delete_data).on_change(Msg::DeleteData))
                    .push(Button::new("Keep").action(Msg::AskRemove(None)))
                    .push(Button::new("Remove").action(Msg::Remove(id.clone())).button_style(ButtonStyle::Destructive))
            } else if self.job(&id).is_some_and(|j| j.active()) {
                row.push(self.action(&id, false))
            } else {
                row.push(Button::new("Open").action(Msg::Launch(id.clone())))
                    .push(Button::symbol(Symbol::Trash).action(Msg::AskRemove(Some(id.clone()))).help("Remove"))
            };
            v = v.push(
                row.padding(12)
                    .fill_width()
                    .background_rounded(Color::Fill, 14)
                    .on_tap(Msg::Open(id.clone()))
                    .id(format!("lib:{id}")),
            );
        }
        v
    }

    /// An installed app's icon (from its own folder), decoded once.
    fn local_icon(&self, path: &str) -> Option<Rc<Pixmap>> {
        self.local_icons.borrow_mut().entry(String::from(path)).or_insert_with(|| Pixmap::load(path)).clone()
    }

    fn sidebar<'a>(&self) -> View<'a, Msg> {
        let item = |symbol: Symbol, title: String, page: Page, badge: usize| {
            let selected = self.page == page;
            let mut row = sidebar_item(symbol, title, selected);
            if badge > 0 {
                row = HStack::new()
                    .push(row.grow(1))
                    .push(
                        Text::new(format!("{badge}"))
                            .style(TextStyle::Sized(11, Weight::Bold))
                            .foreground(Color::OnAccent)
                            .padding_xy(7, 2)
                            .background_rounded(Color::Accent, 9),
                    )
                    .spacing(0)
                    .fill_width();
            }
            row.on_tap(Msg::Go(page))
        };
        let mut v = VStack::new()
            .push(
                HStack::new()
                    .push(Image::app_icon(Icon::Store, 32))
                    .push(Text::new("Store").style(TextStyle::Sized(17, Weight::Bold)))
                    .spacing(10)
                    .padding_insets(Insets { top: 6, leading: 6, bottom: 10, trailing: 0 }),
            )
            .push(
                TextField::search("Search apps and games", self.query.clone())
                    .on_change(Msg::Search)
                    .shortcut(Shortcut::ctrl('f'))
                    .id("search")
                    .fill_width()
                    .padding_insets(Insets { top: 0, leading: 0, bottom: 8, trailing: 0 }),
            )
            .push(item(Symbol::Star, String::from("Discover"), Page::Discover, 0))
            .spacing(2)
            .alignment(Align::Stretch);
        let cats = self.shelf.categories();
        if !cats.is_empty() {
            v = v.push(Text::label("Categories").padding_insets(Insets {
                top: 14,
                leading: 10,
                bottom: 4,
                trailing: 0,
            }));
            for c in cats {
                v = v.push(item(category_symbol(c), category_name(c), Page::Category(c), 0));
            }
        }
        let updates = self.updates().len() + matches!(self.system, SystemState::Available { .. }) as usize;
        v = v
            .push(Text::label("Your Apps").padding_insets(Insets { top: 14, leading: 10, bottom: 4, trailing: 0 }))
            .push(item(Symbol::Download, String::from("Updates"), Page::Updates, updates))
            .push(item(Symbol::Grid, String::from("Library"), Page::Library, 0))
            .push(Spacer::new());
        // Downloads in progress, at a glance.
        let active: Vec<&Job> = self.jobs.iter().filter(|j| j.active()).collect();
        if !active.is_empty() {
            let total: f32 = active.iter().map(|j| j.fraction().unwrap_or(0.0)).sum::<f32>() / active.len() as f32;
            v = v.push(
                VStack::new()
                    .push(
                        Text::new(match active.as_slice() {
                            [j] => format!("{} {}", verb(&j.work), j.name),
                            many => format!("{} in progress", many.len()),
                        })
                        .style(TextStyle::Footnote)
                        .foreground(Color::SecondaryLabel)
                        .lines(1),
                    )
                    .push(ProgressView::new(Some(total)).fill_width())
                    .spacing(6)
                    .padding(10)
                    .background_rounded(Color::Fill, 10),
            );
        }
        let status = if self.state.lock().loading {
            String::from("Checking repositories…")
        } else if self.shelf.offline {
            String::from("Offline: showing what was saved")
        } else if self.shelf.catalogs.is_empty() {
            String::from("No repository")
        } else {
            let n = self.shelf.catalogs.len();
            format!("{n} {} · up to date", if n == 1 { "repository" } else { "repositories" })
        };
        v.push(Text::new(status).style(TextStyle::Footnote).foreground(Color::TertiaryLabel).padding_xy(10, 6))
            .padding_insets(Insets { top: 18, leading: 12, bottom: 12, trailing: 12 })
            .fill()
    }

    /// No repository, or none reachable: say so, and offer to add one.
    fn no_shelf<'a>(&self) -> Option<View<'a, Msg>> {
        if !self.shelf.catalogs.is_empty() {
            return None;
        }
        let loading = self.state.lock().loading;
        if loading && self.shelf_gen == 0 {
            return Some(
                VStack::new()
                    .push(ProgressView::spinner())
                    .push(Text::new("Opening the Store…").foreground(Color::SecondaryLabel))
                    .spacing(12)
                    .padding(60)
                    .fill(),
            );
        }
        let none = apps::repo::sources().is_empty();
        let (title, body) = if none {
            (
                "No app repository yet",
                String::from("The Store shows apps from repositories you choose. Add one by its address — your organisation's, or one served from a computer on your network (cargo xtask serve-repo).")
            )
        } else {
            let why: Vec<String> = self.shelf.errors.iter().map(|(s, e)| format!("{s}: {e}")).collect();
            ("Can't reach the repositories", format!("Check the network and try again.\n\n{}", why.join("\n")))
        };
        Some(
            VStack::new()
                .push(
                    Image::symbol(if none { Symbol::Globe } else { Symbol::Wifi })
                        .symbol_size(44)
                        .foreground(Color::SecondaryLabel),
                )
                .push(Text::new(title).style(TextStyle::Sized(22, Weight::Bold)))
                .push(
                    Text::paragraph(body)
                        .style(TextStyle::Callout)
                        .foreground(Color::SecondaryLabel)
                        .text_align(Align::Center)
                        .max_width(520),
                )
                .push(
                    HStack::new()
                        .push(
                            TextField::new("https://example.com/apps/", self.new_source.clone())
                                .on_change(Msg::NewSource)
                                .on_submit(Msg::AddSource)
                                .focused()
                                .width(320),
                        )
                        .push(Button::new("Add Repository").action(Msg::AddSource).button_style(ButtonStyle::Prominent))
                        .spacing(8),
                )
                .push_if((!none).then(|| Button::new("Try Again").action(Msg::Refresh)))
                .spacing(14)
                .alignment(Align::Center)
                .padding(48)
                .fill(),
        )
    }

    fn content<'a>(&self) -> View<'a, Msg> {
        self.targets.borrow_mut().clear();
        if let Some(v) = self.no_shelf().filter(|_| !matches!(self.page, Page::Library)) {
            return v;
        }
        let body = match &self.page {
            Page::Discover => self.discover(),
            Page::Category(c) => self.category(c),
            Page::Search => self.search(),
            Page::App(id) => self.app(id),
            Page::Updates => self.updates_page(),
            Page::Library => self.library(),
        };
        // The page rises and fades in on the shared spring.
        let t = self.page_t.value().clamp(0, 1000);
        let rise = (1000 - t) * 22 / 1000;
        let mut stack = VStack::new().spacing(14).alignment(Align::Stretch);
        if self.shelf.offline {
            stack = stack.push(banner(
                Symbol::Wifi,
                "You're offline. The Store shows what it saved; installing needs the network.",
                Some(Msg::Refresh),
            ));
        }
        for (source, e) in self.shelf.errors.iter().take(2) {
            stack = stack.push(banner(Symbol::Warning, &format!("{source}: {e}"), None));
        }
        if let Some(n) = &self.note {
            stack = stack.push(banner(Symbol::Info, n, Some(Msg::Dismiss)));
        }
        let page = stack
            .push(body)
            .padding_insets(Insets { top: 26 + rise, leading: 34, bottom: 34, trailing: 34 })
            .opacity((t.max(0) * 255 / 1000) as u8)
            .fill_width();
        ScrollView::new(page).id("page").fill()
    }

    fn launch(&mut self, id: &str, cx: &mut Context<Msg>) {
        if let Err(e) = apps::launch(id) {
            self.flash(e, cx);
        }
    }

    fn flash(&mut self, text: String, cx: &mut Context<Msg>) {
        self.note = Some(text);
        cx.cancel(|m| matches!(m, Msg::Dismiss));
        cx.after(8000, Msg::Dismiss);
    }

    /// The keyboard or a controller moved the cursor.
    fn step(&mut self, dx: i32, dy: i32, cx: &mut Context<Msg>) {
        let targets = self.targets.borrow().clone();
        if targets.is_empty() {
            return;
        }
        let cols = self.columns.get().max(1);
        let i = match self.cursor {
            None => 0,
            Some(i) => {
                let i = i as i32 + dx + dy * cols;
                i.clamp(0, targets.len() as i32 - 1) as usize
            }
        };
        self.cursor = Some(i);
        cx.focus_ring(&targets[i].key);
    }
}

fn verb(w: &Work) -> &'static str {
    match w {
        Work::Install(_) => "Getting",
        Work::Remove { .. } => "Removing",
        Work::Rollback => "Reverting",
    }
}

fn category_symbol(c: &str) -> Symbol {
    match c {
        "game" => Symbol::Gamepad,
        "productivity" => Symbol::Document,
        "utility" => Symbol::Gear,
        "developer" => Symbol::Terminal,
        "graphics" => Symbol::Photo,
        "media" => Symbol::Play,
        "education" => Symbol::Star,
        "network" => Symbol::Globe,
        "social" => Symbol::Person,
        _ => Symbol::Folder,
    }
}

/// A stable colour for an app without a picture (and the banner's wash).
fn tint(id: &str) -> u32 {
    const PALETTE: [u32; 8] =
        [0xFF5E5CE6, 0xFF0A84FF, 0xFF30B0C7, 0xFF34C759, 0xFFFF9F0A, 0xFFFF375F, 0xFFBF5AF2, 0xFF1F9E9A];
    let h = id.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

fn mix(a: u32, b: u32, t: i32) -> u32 {
    waveui::lumen::canvas::mix(a, b, t)
}

fn first_line(s: &str) -> String {
    s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").into()
}

fn empty<'a>(title: &str, body: &str) -> View<'a, Msg> {
    empty_with(Symbol::Search, title, body)
}

fn empty_with<'a>(symbol: Symbol, title: &str, body: &str) -> View<'a, Msg> {
    VStack::new()
        .push(Image::symbol(symbol).symbol_size(34).foreground(Color::TertiaryLabel))
        .push(Text::new(String::from(title)).style(TextStyle::Sized(18, Weight::Bold)))
        .push(
            Text::paragraph(String::from(body))
                .style(TextStyle::Callout)
                .foreground(Color::SecondaryLabel)
                .text_align(Align::Center),
        )
        .spacing(10)
        .alignment(Align::Center)
        .padding(40)
        .fill_width()
}

fn banner<'a>(symbol: Symbol, text: &str, action: Option<Msg>) -> View<'a, Msg> {
    HStack::new()
        .push(Image::symbol(symbol).symbol_size(15).foreground(Color::SecondaryLabel))
        .push(Text::paragraph(String::from(text)).style(TextStyle::Callout).grow(1))
        .push_if(action.map(|m| {
            let title = if matches!(m, Msg::Dismiss) { "OK" } else { "Try Again" };
            Button::new(title).action(m).control_size(ControlSize::Small)
        }))
        .spacing(10)
        .padding_xy(14, 10)
        .fill_width()
        .background_rounded(Color::Fill, 12)
}

/// A picture still on its way: a soft band sweeping across.
fn shimmer<'a>(w: i32, h: i32) -> View<'a, Msg> {
    let t = (now() % 1400) as i32;
    Canvas::new(move |cv, r| {
        let theme = waveui::lumen::theme::current();
        let base = waveui::lumen::canvas::with_alpha(theme.text_secondary, 0x18);
        cv.fill_round_rect(r, 14, base);
        let x = r.x - 120 + (r.w + 240) * t / 1400;
        cv.with_clip(r, |cv| {
            cv.fill_rect(
                waveui::lumen::geom::Rect::new(x, r.y, 80, r.h),
                waveui::lumen::canvas::with_alpha(theme.text_secondary, 0x10),
            );
        });
    })
    .size(w, h)
}

/// An arc in points and degrees (`stroke_arc` takes 1/16 points and
/// 1024ths of a turn, from 3 o'clock, clockwise).
#[allow(clippy::too_many_arguments)]
fn arc(cv: &mut waveui::lumen::canvas::Canvas, cx: i32, cy: i32, r: i32, w: i32, start: i32, sweep: i32, c: u32) {
    cv.stroke_arc(cx * 16, cy * 16, r * 16, w * 16, start * 1024 / 360, sweep * 1024 / 360, c);
}

/// A ring that fills as a download goes (spins while the amount is unknown).
fn progress_ring<'a>(fraction: Option<f32>, size: i32) -> View<'a, Msg> {
    let spin = (now() % 1000) as i32 * 360 / 1000;
    Canvas::new(move |cv, r| {
        let theme = waveui::lumen::theme::current();
        let (cx, cy) = r.center();
        let radius = r.w.min(r.h) / 2 - 2;
        arc(cv, cx, cy, radius, 3, 0, 360, waveui::lumen::canvas::with_alpha(theme.text_secondary, 0x40));
        match fraction {
            Some(f) => {
                arc(cv, cx, cy, radius, 3, -90, (f.clamp(0.0, 1.0) * 360.0) as i32, waveui::lumen::theme::accent())
            }
            None => arc(cv, cx, cy, radius, 3, spin, 90, waveui::lumen::theme::accent()),
        }
    })
    .size(size, size)
}

impl App for Store {
    type Message = Msg;

    fn title(&self) -> String {
        "Store".into()
    }

    fn size(&self) -> (i32, i32) {
        (1120, 740)
    }

    fn min_size(&self) -> (i32, i32) {
        (760, 500)
    }

    fn started(&mut self, cx: &mut Context<Msg>) {
        let t = now();
        self.page_t.snap(0, t);
        self.page_t.set_target(1000, t);
        let _ = cx;
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Go(page) => self.go(page, cx),
            Msg::Back => {
                if let Some(p) = self.history.pop() {
                    self.page = p;
                    self.cursor = None;
                    self.enter(cx);
                } else if self.page != Page::Discover {
                    self.page = Page::Discover;
                    self.enter(cx);
                }
            }
            Msg::Search(q) => {
                self.query = q;
                if self.query.trim().is_empty() {
                    if self.page == Page::Search {
                        self.page = Page::Discover;
                        self.enter(cx);
                    }
                } else if self.page != Page::Search {
                    self.go(Page::Search, cx);
                }
            }
            Msg::Open(id) => self.go(Page::App(id), cx),
            Msg::Get(id) | Msg::Update(id) => {
                if self.installed(&id).is_some() && !self.updates().iter().any(|(a, _, _)| a.manifest.id == id) {
                    self.launch(&id, cx);
                } else {
                    self.queue_install(&id, cx);
                }
            }
            Msg::UpdateAll => {
                let ids: Vec<String> = self.updates().iter().map(|(a, _, _)| a.manifest.id.clone()).collect();
                for id in ids {
                    self.queue_install(&id, cx);
                }
            }
            Msg::Pause(id) => model::pause(&self.state, &id),
            Msg::Resume(id) => model::resume(&self.state, &id),
            Msg::Cancel(id) => model::cancel(&self.state, &id),
            Msg::Launch(id) => self.launch(&id, cx),
            Msg::AskRemove(id) => {
                self.removing = id;
                self.delete_data = false;
            }
            Msg::DeleteData(on) => self.delete_data = on,
            Msg::Remove(id) => {
                let name = self.installed(&id).map(|a| a.manifest.name.clone()).unwrap_or_default();
                model::queue(&self.state, &id, &name, Work::Remove { delete_data: self.delete_data }, None);
                self.removing = None;
            }
            Msg::Rollback(id) => {
                let name = self.installed(&id).map(|a| a.manifest.name.clone()).unwrap_or_default();
                model::queue(&self.state, &id, &name, Work::Rollback, None);
            }
            Msg::Scope(i) => self.scope = i,
            Msg::Refresh => {
                model::refresh(&self.state);
                model::check_system(&self.state);
                self.installed = apps::list();
            }
            Msg::OpenSoftwareUpdate => {
                let _ = corekit::process::spawn("/System/Apps/Settings.elf", &["update"], Default::default());
            }
            Msg::Move(dx, dy) => self.step(dx, dy, cx),
            Msg::Activate => {
                let t = self.cursor.and_then(|i| self.targets.borrow().get(i).cloned());
                if let Some(t) = t {
                    cx.send(t.open);
                }
            }
            Msg::Section(d) => {
                let mut pages = vec![Page::Discover];
                pages.extend(self.shelf.categories().into_iter().map(Page::Category));
                pages.push(Page::Updates);
                pages.push(Page::Library);
                let at = pages.iter().position(|p| *p == self.page).unwrap_or(0) as i32;
                let next = (at + d).rem_euclid(pages.len() as i32) as usize;
                self.go(pages[next].clone(), cx);
            }
            Msg::NewSource(s) => self.new_source = s,
            Msg::AddSource => {
                let url = String::from(self.new_source.trim());
                match apps::repo::add_source(&url) {
                    Ok(()) => {
                        self.new_source.clear();
                        model::refresh(&self.state);
                        model::check_system(&self.state);
                    }
                    Err(e) => self.flash(e, cx),
                }
            }
            Msg::Dismiss => self.note = None,
        }
        self.sync(cx);
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        use waveui::KeyCode;
        if !ev.pressed {
            return None;
        }
        match ev.code {
            KeyCode::Left => Some(Msg::Move(-1, 0)),
            KeyCode::Right => Some(Msg::Move(1, 0)),
            KeyCode::Up => Some(Msg::Move(0, -1)),
            KeyCode::Down => Some(Msg::Move(0, 1)),
            KeyCode::Enter => Some(Msg::Activate),
            KeyCode::Escape | KeyCode::Backspace => Some(Msg::Back),
            KeyCode::PageDown => Some(Msg::Section(1)),
            KeyCode::PageUp => Some(Msg::Section(-1)),
            _ => None,
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        let busy = self.jobs.iter().any(|j| j.active()) || matches!(self.system, SystemState::Checking);
        Some(if self.moving() || busy { 16 } else { 120 })
    }

    fn poll(&mut self, cx: &mut Context<Msg>) -> bool {
        let t = now();
        let mut redraw = self.sync(cx);
        if self.reduce_motion.get() {
            self.page_t.snap(1000, t);
            self.hero_t.snap(1000, t);
        }
        redraw |= self.page_t.advance(t) | !self.page_t.settled();
        // The banner shows the next featured app every few seconds.
        if self.page == Page::Discover && self.shelf.featured.len() > 1 && t > self.hero_at + 7000 {
            if self.hero_t.target() == 1000 {
                self.hero_t.set_target(0, t);
            } else if self.hero_t.value() <= 20 {
                self.hero = (self.hero + 1) % self.shelf.featured.len();
                self.hero_t.set_target(1000, t);
                self.hero_at = t;
            }
        }
        redraw |= self.hero_t.advance(t);
        // Downloads and pictures still arriving keep their spinners turning.
        redraw |= self.jobs.iter().any(|j| j.active());
        redraw |= self.state.lock().pictures.values().any(|p| matches!(p, Pic::Loading));
        // A game controller (when this window is in front and not in Wave Play,
        // which turns the d-pad into arrow keys itself).
        if self.active.get() {
            let pads = corekit::gamepad::connected();
            if let Some(s) = pads.first().and_then(|p| p.state()) {
                let mut bits = 0u32;
                let (lx, ly) = s.left_stick();
                for (i, (b, alt)) in [
                    (Pad::Left, lx < -0.6),
                    (Pad::Right, lx > 0.6),
                    (Pad::Up, ly < -0.6),
                    (Pad::Down, ly > 0.6),
                    (Pad::South, false),
                    (Pad::East, false),
                    (Pad::LeftShoulder, false),
                    (Pad::RightShoulder, false),
                ]
                .into_iter()
                .enumerate()
                {
                    if s.pressed(b) || alt {
                        bits |= 1 << i;
                    }
                }
                let fresh = bits & !self.pad_prev;
                let repeat = bits & 0xF != 0 && bits == self.pad_prev && t > self.pad_at + 180;
                if fresh != 0 || repeat {
                    self.pad_at = t + if fresh != 0 { 220 } else { 0 };
                    let pressed = if fresh != 0 { fresh } else { bits };
                    let msgs = [
                        Msg::Move(-1, 0),
                        Msg::Move(1, 0),
                        Msg::Move(0, -1),
                        Msg::Move(0, 1),
                        Msg::Activate,
                        Msg::Back,
                        Msg::Section(-1),
                        Msg::Section(1),
                    ];
                    for (i, m) in msgs.into_iter().enumerate() {
                        if pressed & (1 << i) != 0 {
                            cx.send(m);
                        }
                    }
                }
                self.pad_prev = bits;
            }
        }
        redraw
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        self.active.set(env.active);
        self.reduce_motion.set(env.reduce_motion);
        self.columns.set(((env.size.0 - 250 - 68 + 14) / (CARD_W + 14)).max(1));
        SplitView::new(self.sidebar(), self.content()).sidebar_width(250)
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned();
    if args.iter().any(|a| a == "--check") {
        return check::run(args.iter().any(|a| a == "--now"));
    }
    let start = if let Some(c) = arg("--category") {
        gina::CATEGORIES.iter().find(|x| **x == c).map(|c| Page::Category(c)).unwrap_or(Page::Discover)
    } else if let Some(id) = arg("--app") {
        Page::App(id)
    } else if args.iter().any(|a| a == "--updates") || check::opened_for_updates() {
        Page::Updates
    } else {
        Page::Discover
    };
    waveui::run(Store::new(start))
}
