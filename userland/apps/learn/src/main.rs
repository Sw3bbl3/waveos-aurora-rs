//! Offline bilingual handbook. HTML layout is shared with Nebula; no network fetches.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::{
    collections::BTreeMap,
    format,
    string::{String, ToString},
    vec::Vec,
};
use aurora_help::{search, Article};
use aurora_image::Image;
use aurorakit::{
    canvas::Canvas,
    font::{self, Face, Font},
    geom::Rect,
    text::TextField,
    theme,
    widgets::{button, ButtonStyle},
    App, Env, KeyCode, KeyEvent,
};
use nebula_engine::{layout::Item, style::ListStyle, FontSpec, Host, Layout, Metrics, Page};
fn font_for(f: FontSpec) -> Font {
    font::get(
        if f.mono {
            Face::Mono
        } else if f.bold {
            Face::SemiBold
        } else {
            Face::Regular
        },
        f.size,
    )
}
struct Fonts<'a>(&'a BTreeMap<String, Image>);
impl Host for Fonts<'_> {
    fn measure(&self, f: FontSpec, t: &str) -> i32 {
        font_for(f).width(t)
    }
    fn measure64(&self, f: FontSpec, t: &str) -> i32 {
        font_for(f).width64(t)
    }
    fn metrics(&self, f: FontSpec) -> Metrics {
        let v = font_for(f);
        Metrics { ascent: v.ascent as i32, descent: -(v.descent as i32) }
    }
    fn image_size(&self, src: &str) -> Option<(u32, u32)> {
        self.0.get(src).map(|i| (i.width, i.height))
    }
}
#[derive(Clone, PartialEq, Eq)]
enum Action {
    Back,
    Forward,
    Home,
    Language,
    Smaller,
    Larger,
    Contents,
    Search,
    Category(usize),
    Article(usize),
    Link(usize),
}
struct Learn {
    catalog: Vec<Article>,
    locale: String,
    query: TextField,
    results: Vec<usize>,
    category: usize,
    current: Option<usize>,
    history: Vec<(String, i32)>,
    history_pos: usize,
    page: Option<Page>,
    layout: Option<Layout>,
    images: BTreeMap<String, Image>,
    scroll: i32,
    side_scroll: i32,
    size: i32,
    area: Rect,
    view: Rect,
    layout_key: (i32, i32, bool, bool),
    targets: Vec<(Action, Rect)>,
    focus: Action,
    hover: Option<Action>,
    error: Option<String>,
    show_contents: bool,
    started: u64,
    first_draw: bool,
}
const CATEGORIES: [&str; 8] = ["all", "start", "desktop", "apps", "system", "developer", "reference", "release"];
impl Learn {
    fn tr<'a>(&self, en: &'a str, fr: &'a str) -> &'a str {
        if self.locale == "fr-CA" {
            fr
        } else {
            en
        }
    }
    fn new() -> Self {
        let started = corekit::time::uptime_ms();
        let loaded = corekit::fs::read("/System/Help/catalog.json")
            .map_err(|_| "Help library is missing / Bibliothèque absente".to_string())
            .and_then(|b| {
                aurora_help::read_catalog(&b).map_err(|_| "Help library is invalid / Bibliothèque invalide".into())
            });
        let (catalog, error) = match loaded {
            Ok(c) => (c, None),
            Err(e) => (Vec::new(), Some(e)),
        };
        let locale = corekit::prefs::get("learn_language").filter(|s| s == "en" || s == "fr-CA").unwrap_or("en".into());
        let mut s = Self {
            catalog,
            locale,
            query: TextField::new("", "Search / Rechercher"),
            results: Vec::new(),
            category: 0,
            current: None,
            history: Vec::new(),
            history_pos: 0,
            page: None,
            layout: None,
            images: BTreeMap::new(),
            scroll: 0,
            side_scroll: 0,
            size: corekit::prefs::get_int("learn_size", 16).clamp(14, 24) as i32,
            area: Rect::default(),
            view: Rect::default(),
            layout_key: (0, 0, false, false),
            targets: Vec::new(),
            focus: Action::Search,
            hover: None,
            error,
            show_contents: false,
            started,
            first_draw: true,
        };
        s.refresh_results();
        s.open_id("start-welcome", true);
        s
    }
    fn refresh_results(&mut self) {
        let now = corekit::time::uptime_ms();
        self.results = search(&self.catalog, &self.locale, self.query.text());
        if self.category > 0 {
            self.results.retain(|i| self.catalog[*i].category == CATEGORIES[self.category]);
        }
        self.side_scroll = 0;
        corekit::println!("learn-perf search {} ms, {} results", corekit::time::uptime_ms() - now, self.results.len());
    }
    fn remember_scroll(&mut self) {
        if let Some(h) = self.history.get_mut(self.history_pos) {
            h.1 = self.scroll;
        }
    }
    fn open_id(&mut self, id: &str, record: bool) {
        let Some(i) = self.catalog.iter().position(|a| a.id == id && a.locale == self.locale) else { return };
        self.remember_scroll();
        if record {
            if !self.history.is_empty() {
                self.history.truncate(self.history_pos + 1);
            }
            self.history.push((id.into(), 0));
            self.history_pos = self.history.len() - 1;
        }
        self.current = Some(i);
        self.scroll = 0;
        self.show_contents = false;
        self.load();
    }
    fn load(&mut self) {
        self.page = None;
        self.layout = None;
        self.images.clear();
        self.layout_key.0 = 0;
        let Some(i) = self.current else { return };
        let path = format!("/System/Help/{}", self.catalog[i].path);
        match corekit::fs::read(&path) {
            Ok(bytes) => {
                let t = theme::current();
                let html = String::from_utf8_lossy(&bytes)
                    .replace("font-size:16px", &format!("font-size:{}px", self.size))
                    .replace("font-size:30px", &format!("font-size:{}px", self.size * 30 / 16))
                    .replace("font-size:23px", &format!("font-size:{}px", self.size * 23 / 16))
                    .replace("#202331", &format!("#{:06x}", t.text & 0xffffff))
                    .replace("#edf0f6", if t.dark { "#252c3b" } else { "#edf0f6" })
                    .replace("#5550c8", if t.dark { "#b7a8ff" } else { "#5550c8" });
                let page = Page::parse(&html);
                for src in page.images().into_iter().take(12) {
                    if let Some(name) = src.strip_prefix("../images/").filter(|s| !s.contains('/') && !s.contains(".."))
                    {
                        if let Ok(data) = corekit::fs::read(&format!("/System/Help/images/{name}")) {
                            if let Ok(image) = aurora_image::decode(&data) {
                                self.images.insert(src, image);
                            }
                        }
                    }
                }
                self.page = Some(page);
                self.error = None;
            }
            Err(_) => {
                self.error = Some(
                    self.tr(
                        "This guide could not be opened. Rebuild or reinstall the system image.",
                        "Impossible d’ouvrir ce guide. Reconstruisez ou réinstallez l’image système.",
                    )
                    .into(),
                )
            }
        }
    }
    fn ensure_layout(&mut self) {
        let t = theme::current();
        let key = (self.view.w, self.size, t.dark, theme::high_contrast());
        if key != self.layout_key {
            if self.layout_key.0 != 0 && (self.layout_key.2 != t.dark || self.layout_key.3 != theme::high_contrast()) {
                self.load();
            }
            self.layout = self.page.as_ref().map(|p| p.layout(self.view.w, self.view.h, &Fonts(&self.images)));
            self.layout_key = key;
            self.scroll = self.scroll.clamp(0, self.max_scroll());
        }
    }
    fn max_scroll(&self) -> i32 {
        self.layout.as_ref().map_or(0, |l| (l.height - self.view.h).max(0))
    }
    fn action(&mut self, a: Action) {
        match a {
            Action::Back | Action::Forward => {
                let next = if a == Action::Back { self.history_pos.checked_sub(1) } else { Some(self.history_pos + 1) };
                if let Some(n) = next.filter(|n| *n < self.history.len()) {
                    self.remember_scroll();
                    let (id, scroll) = self.history[n].clone();
                    self.open_id(&id, false);
                    self.history_pos = n;
                    self.scroll = scroll;
                }
            }
            Action::Home => {
                self.category = 0;
                self.query.set_text("");
                self.refresh_results();
                self.open_id("start-welcome", true);
            }
            Action::Language => {
                let id = self.current.map(|i| self.catalog[i].id.clone());
                let scroll = self.scroll;
                self.locale = if self.locale == "en" { "fr-CA" } else { "en" }.into();
                if corekit::prefs::set("learn_language", &self.locale).is_err() {
                    self.error = Some(
                        self.tr("Could not save language preference", "Impossible d’enregistrer la langue").into(),
                    );
                }
                self.refresh_results();
                if let Some(id) = id {
                    self.open_id(&id, false);
                    self.scroll = scroll;
                }
            }
            Action::Smaller | Action::Larger => {
                self.size = (self.size + if a == Action::Smaller { -2 } else { 2 }).clamp(14, 24);
                let _ = corekit::prefs::set("learn_size", &format!("{}", self.size));
                self.load();
            }
            Action::Contents => {
                self.show_contents = !self.show_contents;
            }
            Action::Search => {
                self.focus = Action::Search;
            }
            Action::Category(n) => {
                self.category = n;
                self.query.set_text("");
                self.refresh_results();
            }
            Action::Article(i) => {
                let id = self.catalog[i].id.clone();
                self.open_id(&id, true);
            }
            Action::Link(n) => {
                if self.show_contents {
                    if let Some(i) = self.current {
                        if let Some((id, _)) = self.catalog[i].headings.get(n) {
                            if let Some(y) = self.layout.as_ref().and_then(|l| l.anchor(id)) {
                                self.scroll = y.clamp(0, self.max_scroll());
                                self.show_contents = false;
                            }
                        }
                    }
                } else {
                    let href = self
                        .layout
                        .as_ref()
                        .and_then(|l| l.links.get(n))
                        .and_then(|link| self.page.as_ref()?.element(link.node)?.attr("href"))
                        .map(String::from);
                    if let Some(href) = href {
                        self.follow(&href);
                    }
                }
            }
        }
    }
    fn follow(&mut self, href: &str) {
        if href.starts_with("https://") || href.starts_with("http://") {
            let _ = corekit::process::spawn("/System/Apps/Nebula.elf", &[href], corekit::process::Stdio::default());
            return;
        }
        if href.ends_with("index.html") {
            self.action(Action::Home);
            return;
        }
        let (path, anchor) = href.split_once('#').unwrap_or((href, ""));
        if !path.is_empty() {
            let filename = path.rsplit('/').next().unwrap_or("");
            if let Some(id) = filename.strip_suffix(".html") {
                self.open_id(id, true);
                self.ensure_layout();
            }
        }
        if !anchor.is_empty() {
            if let Some(y) = self.layout.as_ref().and_then(|l| l.anchor(anchor)) {
                self.scroll = y.clamp(0, self.max_scroll());
            }
        }
    }
    fn control(&mut self, cv: &mut Canvas, a: Action, r: Rect, label: &str) {
        button(cv, r, label, ButtonStyle::Secondary, self.hover.as_ref() == Some(&a));
        if self.focus == a {
            cv.stroke_round_rect(r.inset(-2), 8, theme::accent());
        }
        self.targets.push((a, r));
    }
    fn paint_document(&self, cv: &mut Canvas) {
        let Some(layout) = &self.layout else { return };
        let v = self.view;
        let (ox, oy) = (v.x, v.y - self.scroll);
        cv.with_clip(v, |cv| {
            for item in &layout.items {
                let rect = |r: &nebula_engine::Rect| Rect::new(ox + r.x, oy + r.y, r.w, r.h);
                match item {
                    Item::Rect { rect: r, color, radius } => cv.fill_round_rect(rect(r), *radius, *color),
                    Item::Text { x, y, text, font: f, color, underline, strike } => {
                        let f = font_for(*f);
                        let y = oy + y;
                        if y + 20 < v.y || y - f.ascent as i32 > v.bottom() {
                            continue;
                        }
                        let w = cv.text(ox + x, y, text, f, *color);
                        if *underline {
                            cv.fill_rect(Rect::new(ox + x, y + 2, w, 1), *color);
                        }
                        if *strike {
                            cv.fill_rect(Rect::new(ox + x, y - f.ascent as i32 / 3, w, 1), *color);
                        }
                    }
                    Item::Image { rect: r, src } => {
                        if let Some(i) = self.images.get(src) {
                            cv.draw_image(&i.pixels, i.width, i.height, rect(r), 255);
                        }
                    }
                    Item::Bullet { rect: r, color, kind } => {
                        let r = rect(r);
                        match kind {
                            ListStyle::Square => cv.fill_rect(r, *color),
                            _ => cv.fill_circle(r.x + r.w / 2, r.y + r.h / 2, r.w / 2, *color),
                        }
                    }
                }
            }
        });
    }
}
impl App for Learn {
    fn title(&self) -> String {
        self.tr("Learn — WaveOS Guide", "Learn — Guide WaveOS").into()
    }
    fn size(&self) -> (i32, i32) {
        (1020, 620)
    }
    fn draw(&mut self, cv: &mut Canvas, area: Rect, _: &Env) {
        self.area = area;
        self.targets.clear();
        let t = theme::current();
        cv.fill_rect(area, t.window_bg);
        let side = if area.w > 750 { 240 } else { 180 };
        cv.fill_rect(Rect::new(area.x, area.y, area.w, 98), t.window_bg_alt);
        cv.text(area.x + 18, area.y + 34, "Learn", theme::ui_bold(24), t.text);
        cv.text(
            area.x + 105,
            area.y + 32,
            self.tr("The WaveOS guide", "Le guide WaveOS"),
            theme::ui(14),
            t.text_secondary,
        );
        let lang = if self.locale == "en" { "English" } else { "Français (Canada)" };
        self.control(cv, Action::Language, Rect::new(area.right() - 186, area.y + 10, 172, 34), lang);
        let labels = [
            (Action::Back, "←"),
            (Action::Forward, "→"),
            (Action::Home, self.tr("Home", "Accueil")),
            (Action::Contents, self.tr("Contents", "Sommaire")),
            (Action::Smaller, "A−"),
            (Action::Larger, "A+"),
        ];
        let mut x = area.x + 14;
        for (a, label) in labels {
            let w = if matches!(a, Action::Home | Action::Contents) { 100 } else { 42 };
            self.control(cv, a, Rect::new(x, area.y + 54, w, 32), label);
            x += w + 8;
        }
        let sidebar = Rect::new(area.x, area.y + 98, side, area.h - 98);
        cv.fill_rect(sidebar, t.window_bg_alt);
        let search_rect = Rect::new(sidebar.x + 12, sidebar.y + 12, side - 24, 34);
        self.query.draw(cv, search_rect, self.focus == Action::Search);
        self.targets.push((Action::Search, search_rect));
        let cats = if self.locale == "en" {
            ["All guides", "Getting started", "Desktop", "Apps", "System", "Developers", "Reference", "What’s New"]
        } else {
            [
                "Tous les guides",
                "Premiers pas",
                "Bureau",
                "Applications",
                "Système",
                "Développement",
                "Référence",
                "Nouveautés",
            ]
        };
        for (i, label) in cats.iter().enumerate() {
            let r = Rect::new(sidebar.x + 12, sidebar.y + 56 + i as i32 * 27, side - 24, 24);
            if self.category == i {
                cv.fill_round_rect(r, 6, t.hover);
            }
            cv.text(r.x + 8, r.y + 17, label, theme::ui(12), t.text);
            if self.focus == Action::Category(i) {
                cv.stroke_round_rect(r, 6, theme::accent());
            }
            self.targets.push((Action::Category(i), r));
        }
        let list = Rect::new(sidebar.x + 8, sidebar.y + 282, side - 16, (sidebar.h - 292).max(0));
        cv.with_clip(list, |cv| {
            if self.results.is_empty() {
                cv.text(
                    list.x + 6,
                    list.y + 22,
                    self.tr("No matching guides", "Aucun guide trouvé"),
                    theme::ui(12),
                    t.text_secondary,
                );
            }
            for (row, i) in self.results.iter().copied().enumerate() {
                let r = Rect::new(list.x, list.y + row as i32 * 36 - self.side_scroll, list.w, 32);
                if !r.intersects(&list) {
                    continue;
                }
                if self.current == Some(i) {
                    cv.fill_round_rect(r, 6, t.hover);
                }
                cv.text_clipped(r.x + 8, r.y + 21, &self.catalog[i].title, theme::ui(12), t.text, r.w - 16);
                if self.focus == Action::Article(i) {
                    cv.stroke_round_rect(r, 6, theme::accent());
                }
                self.targets.push((Action::Article(i), r.intersect(&list)));
            }
        });
        self.view = Rect::new(area.x + side + 8, area.y + 102, area.w - side - 20, area.h - 126);
        self.ensure_layout();
        if let Some(error) = &self.error {
            cv.text_clipped(self.view.x + 18, self.view.y + 45, error, theme::ui(14), t.text, self.view.w - 36);
        } else {
            self.paint_document(cv);
        }
        if let Some(l) = &self.layout {
            for (n, link) in l.links.iter().enumerate() {
                let r = Rect::new(
                    self.view.x + link.rect.x,
                    self.view.y + link.rect.y - self.scroll,
                    link.rect.w,
                    link.rect.h,
                )
                .intersect(&self.view);
                if r.is_empty() {
                    continue;
                }
                if self.focus == Action::Link(n) {
                    cv.stroke_round_rect(r.inset(-1), 2, theme::accent());
                }
                self.targets.push((Action::Link(n), r));
            }
        }
        cv.text_clipped(
            self.view.x + 8,
            area.bottom() - 7,
            self.tr(
                "Offline library · External links open in Nebula and require a network",
                "Bibliothèque hors ligne · Les liens externes exigent le réseau et ouvrent Nebula",
            ),
            theme::ui(10),
            t.text_secondary,
            self.view.w - 16,
        );
        if self.show_contents {
            let panel = Rect::new(self.view.x + 10, self.view.y + 8, (self.view.w - 20).min(460), self.view.h.min(360));
            cv.fill_round_rect(panel, 12, t.window_bg_alt);
            cv.stroke_round_rect(panel, 12, t.separator);
            self.targets.retain(|(a, _)| !matches!(a, Action::Link(_)));
            if let Some(i) = self.current {
                for (n, (_, title)) in self.catalog[i].headings.iter().enumerate() {
                    let r = Rect::new(panel.x + 12, panel.y + 12 + n as i32 * 34, panel.w - 24, 30);
                    if r.bottom() > panel.bottom() {
                        break;
                    }
                    cv.text_clipped(r.x + 6, r.y + 20, title, theme::ui(13), t.text, r.w - 12);
                    if self.focus == Action::Link(n) {
                        cv.stroke_round_rect(r, 6, theme::accent());
                    }
                    self.targets.push((Action::Link(n), r));
                }
            }
        }
        if self.first_draw {
            corekit::println!("learn-perf startup {} ms", corekit::time::uptime_ms() - self.started);
            self.first_draw = false;
        }
    }
    fn click(&mut self, x: i32, y: i32, _: Rect, env: &mut Env) -> bool {
        if self.query.click(x, y, env.mods.shift, env.now_ms) {
            self.focus = Action::Search;
            return true;
        }
        if let Some((a, _)) = self.targets.iter().rev().find(|(_, r)| r.contains(x, y)) {
            let a = a.clone();
            self.focus = a.clone();
            self.action(a);
            return true;
        }
        false
    }
    fn hover(&mut self, x: i32, y: i32, _: Rect) -> bool {
        let h = self.targets.iter().rev().find(|(_, r)| r.contains(x, y)).map(|(a, _)| a.clone());
        let changed = h != self.hover;
        self.hover = h;
        changed
    }
    fn key(&mut self, k: &KeyEvent, env: &mut Env) -> bool {
        if !k.pressed {
            return false;
        }
        if k.mods.ctrl && matches!(k.ch, Some('f') | Some('F')) {
            self.focus = Action::Search;
            return true;
        }
        if k.mods.alt && matches!(k.code, KeyCode::Left | KeyCode::Right) {
            self.action(if k.code == KeyCode::Left { Action::Back } else { Action::Forward });
            return true;
        }
        if k.code == KeyCode::Escape {
            self.show_contents = false;
            self.focus = Action::Home;
            return true;
        }
        if k.code == KeyCode::Tab {
            let n = self.targets.len();
            if n == 0 {
                return false;
            }
            let i = self.targets.iter().position(|(a, _)| *a == self.focus).unwrap_or(0);
            let next = (i + if k.mods.shift { n - 1 } else { 1 }) % n;
            self.focus = self.targets[next].0.clone();
            return true;
        }
        if self.focus == Action::Search {
            if matches!(k.code, KeyCode::Down | KeyCode::Enter) {
                if let Some(i) = self.results.first().copied() {
                    self.focus = Action::Article(i);
                    if k.code == KeyCode::Enter {
                        self.action(Action::Article(i));
                    }
                }
                return true;
            }
            let changed = self.query.key(k, env.now_ms);
            if changed.handled {
                self.refresh_results();
            }
            return changed.handled;
        }
        if let Action::Article(i) = self.focus {
            if matches!(k.code, KeyCode::Down | KeyCode::Up) {
                if let Some(row) = self.results.iter().position(|j| *j == i) {
                    let n = if k.code == KeyCode::Down {
                        (row + 1).min(self.results.len() - 1)
                    } else {
                        row.saturating_sub(1)
                    };
                    self.focus = Action::Article(self.results[n]);
                    let h = (self.area.h - 390).max(36);
                    self.side_scroll = self.side_scroll.min(n as i32 * 36).max((n as i32 * 36 + 36 - h).max(0));
                }
                return true;
            }
        }
        if k.code == KeyCode::Enter || k.ch == Some(' ') {
            let a = self.focus.clone();
            self.action(a);
            return true;
        }
        match k.code {
            KeyCode::PageDown => {
                self.scroll = (self.scroll + self.view.h - 40).min(self.max_scroll());
                true
            }
            KeyCode::PageUp => {
                self.scroll = (self.scroll - self.view.h + 40).max(0);
                true
            }
            KeyCode::Home => {
                self.scroll = 0;
                true
            }
            KeyCode::End => {
                self.scroll = self.max_scroll();
                true
            }
            _ => false,
        }
    }
    fn scroll(&mut self, delta: i32, _: Rect) -> bool {
        let started = corekit::time::uptime_ms();
        if matches!(self.hover, Some(Action::Article(_) | Action::Category(_) | Action::Search)) {
            self.side_scroll = (self.side_scroll + delta * 36)
                .clamp(0, (self.results.len() as i32 * 36 - (self.area.h - 390).max(36)).max(0));
        } else {
            self.scroll = (self.scroll + delta * 48).clamp(0, self.max_scroll());
        }
        corekit::println!("learn-perf scroll-state {} ms", corekit::time::uptime_ms() - started);
        true
    }
    fn tick(&mut self, env: &mut Env) -> bool {
        self.query.tick(env.now_ms, env.focused && self.focus == Action::Search)
    }
}
corekit::entry!(main);
fn main(args: corekit::Args) -> i32 {
    if args.get(1).is_some_and(|s| s == "--check-library") {
        aurorakit::load_fonts();
        let mut learn = Learn::new();
        let mut checked = 0;
        let catalog = learn.catalog.clone();
        if catalog.is_empty() {
            corekit::println!("learn-check FAILED: missing catalog");
            return 1;
        }
        for a in catalog {
            learn.locale = a.locale.clone();
            learn.open_id(&a.id, false);
            learn.view = Rect::new(0, 0, 720, 550);
            learn.ensure_layout();
            if learn.error.is_some() || learn.layout.as_ref().is_none_or(|l| l.items.is_empty()) {
                corekit::println!("learn-check FAILED {} {}", a.locale, a.id);
                return 1;
            }
            checked += 1;
        }
        corekit::println!("learn-check passed: {} offline articles rendered", checked);
        return 0;
    }
    let mut learn = Learn::new();
    if let Some(id) = args.get(1) {
        learn.open_id(id, false);
    }
    aurorakit::run(learn)
}
