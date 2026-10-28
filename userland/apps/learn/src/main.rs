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
use core::cell::RefCell;
use nebula_engine::{layout::Item, style::ListStyle, FontSpec, Host, Layout, Metrics, Page};
use waveui::lumen::{
    canvas::Canvas as Surface,
    font::{self, Face, Font},
    geom::Rect,
    theme,
};
use waveui::prelude::*;
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
#[derive(Clone)]
pub enum Msg {
    Back,
    Forward,
    Home,
    Language(usize),
    Smaller,
    Larger,
    Heading(usize),
    Query(String),
    OpenFirst,
    Category(usize),
    Article(usize),
    Resize(i32, i32),
    Scroll(i32),
    Click(i32, i32),
    Page(i32),
}

/// The open guide: parsed, its pictures, and laid out for the view.
#[derive(Default)]
struct Doc {
    page: Option<Page>,
    layout: Option<Layout>,
    images: BTreeMap<String, Image>,
    /// What the layout was made for: width, text size, dark, high contrast.
    key: (i32, i32, bool, bool),
}

pub struct Learn {
    catalog: Vec<Article>,
    locale: String,
    query: String,
    results: Vec<usize>,
    category: usize,
    current: Option<usize>,
    history: Vec<(String, i32)>,
    history_pos: usize,
    doc: RefCell<Doc>,
    scroll: i32,
    size: i32,
    /// The size of the reading area.
    view: (i32, i32),
    error: Option<String>,
    started: u64,
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
            query: String::new(),
            results: Vec::new(),
            category: 0,
            current: None,
            history: Vec::new(),
            history_pos: 0,
            doc: RefCell::new(Doc::default()),
            scroll: 0,
            size: corekit::prefs::get_int("learn_size", 16).clamp(14, 24) as i32,
            view: (720, 500),
            error,
            started,
        };
        s.refresh_results();
        s.open_id("start-welcome", true);
        s
    }

    fn refresh_results(&mut self) {
        let now = corekit::time::uptime_ms();
        self.results = search(&self.catalog, &self.locale, &self.query);
        if self.category > 0 {
            self.results.retain(|i| self.catalog[*i].category == CATEGORIES[self.category]);
        }
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
        self.load();
    }

    /// Reads and parses the current guide (in the current appearance).
    fn load(&mut self) {
        let error = self.load_doc();
        if let Some(e) = error {
            self.error = Some(e);
        } else if self.current.is_some() {
            self.error = None;
        }
    }

    fn load_doc(&self) -> Option<String> {
        let mut doc = self.doc.borrow_mut();
        *doc = Doc::default();
        let i = self.current?;
        let path = format!("/System/Help/{}", self.catalog[i].path);
        let Ok(bytes) = corekit::fs::read(&path) else {
            return Some(
                self.tr(
                    "This guide could not be opened. Rebuild or reinstall the system image.",
                    "Impossible d’ouvrir ce guide. Reconstruisez ou réinstallez l’image système.",
                )
                .into(),
            );
        };
        let t = theme::current();
        let hex = |c: u32| format!("#{:06x}", c & 0xffffff);
        // The handbook's style tokens (xtask/src/help.rs) for this reader's
        // text size and appearance.
        let html = scale_font_sizes(&String::from_utf8_lossy(&bytes), self.size)
            .replace("#f6f7fb", &hex(t.window_bg_alt))
            .replace("#ffffff", &hex(t.window_bg))
            .replace("#202331", &hex(t.text))
            .replace("#6b7084", &hex(t.text_secondary))
            .replace("#e3e6ee", if t.dark { "#2c3448" } else { "#e3e6ee" })
            .replace("#edf0f6", if t.dark { "#252c3b" } else { "#edf0f6" })
            .replace("#eeedfb", if t.dark { "#27264a" } else { "#eeedfb" })
            .replace("#d9d6f6", if t.dark { "#3b3870" } else { "#d9d6f6" })
            .replace("#5550c8", if t.dark { "#b7a8ff" } else { "#5550c8" });
        let page = Page::parse(&html);
        let mut missing_image = false;
        for src in page.images().into_iter().take(12) {
            if let Some(name) = src.strip_prefix("../images/").filter(|s| !s.contains('/') && !s.contains("..")) {
                if let Ok(data) = corekit::fs::read(&format!("/System/Help/images/{name}")) {
                    if let Ok(image) = aurora_image::decode(&data) {
                        doc.images.insert(src.clone(), image);
                    }
                }
            }
            missing_image |= !doc.images.contains_key(&src);
        }
        doc.page = Some(page);
        doc.key = (0, 0, t.dark, theme::high_contrast());
        missing_image.then(|| {
            self.tr(
                "An image is missing or invalid. Reinstall the help library.",
                "Une image est absente ou invalide. Réinstallez la bibliothèque d’aide.",
            )
            .into()
        })
    }

    /// Lays the guide out for the view (again when the width, the text
    /// size or the appearance changed).
    fn ensure_layout(&self) {
        let t = theme::current();
        let key = (self.view.0, self.size, t.dark, theme::high_contrast());
        let stale_theme = {
            let doc = self.doc.borrow();
            if doc.key == key && doc.layout.is_some() {
                return;
            }
            (doc.key.2, doc.key.3) != (key.2, key.3)
        };
        if stale_theme {
            let _ = self.load_doc();
        }
        let mut doc = self.doc.borrow_mut();
        let layout = doc.page.as_ref().map(|p| p.layout(self.view.0, self.view.1, &Fonts(&doc.images)));
        doc.layout = layout;
        doc.key = key;
    }

    fn max_scroll(&self) -> i32 {
        self.ensure_layout();
        self.doc.borrow().layout.as_ref().map_or(0, |l| (l.height - self.view.1).max(0))
    }

    fn go(&mut self, back: bool) {
        let next = if back { self.history_pos.checked_sub(1) } else { Some(self.history_pos + 1) };
        if let Some(n) = next.filter(|n| *n < self.history.len()) {
            self.remember_scroll();
            let (id, scroll) = self.history[n].clone();
            self.open_id(&id, false);
            self.history_pos = n;
            self.scroll = scroll;
        }
    }

    fn follow(&mut self, href: &str) {
        if href.starts_with("https://") || href.starts_with("http://") {
            let _ = corekit::process::spawn("/System/Apps/Nebula.elf", &[href], corekit::process::Stdio::default());
            return;
        }
        if href.ends_with("index.html") {
            self.home();
            return;
        }
        let (path, anchor) = href.split_once('#').unwrap_or((href, ""));
        if !path.is_empty() {
            let filename = path.rsplit('/').next().unwrap_or("");
            if let Some(id) = filename.strip_suffix(".html") {
                self.open_id(id, true);
            }
        }
        if !anchor.is_empty() {
            self.ensure_layout();
            let y = self.doc.borrow().layout.as_ref().and_then(|l| l.anchor(anchor));
            if let Some(y) = y {
                self.scroll = y.clamp(0, self.max_scroll());
            }
        }
    }

    fn home(&mut self) {
        self.category = 0;
        self.query.clear();
        self.refresh_results();
        self.open_id("start-welcome", true);
    }

    fn paint_document(&self, cv: &mut Surface, v: Rect) {
        self.ensure_layout();
        let doc = self.doc.borrow();
        let Some(layout) = &doc.layout else { return };
        let (ox, oy) = (v.x, v.y - self.scroll);
        cv.fill_rect(v, theme::current().window_bg_alt);
        cv.with_clip(v, |cv| {
            for item in &layout.items {
                let rect = |r: &nebula_engine::Rect| Rect::new(ox + r.x, oy + r.y, r.w, r.h);
                match item {
                    Item::Rect { rect: r, color, radius } => cv.fill_round_rect(rect(r), *radius, *color),
                    Item::Border { rect: r, color, radius, width } => {
                        let r = rect(r);
                        let radius = (*radius).min(r.w / 2).min(r.h / 2);
                        for inset in 0..(*width).min(r.w.min(r.h) / 2) {
                            cv.stroke_round_rect(r.inset(inset), (radius - inset).max(0), *color);
                        }
                    }
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
                    Item::Canvas { .. } | Item::Video { .. } => {} // Learn does not execute page scripts.
                    Item::Image { rect: r, src } => {
                        if let Some(i) = doc.images.get(src) {
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

    fn sidebar(&self) -> View<'_, Msg> {
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
        let categories = Picker::new(cats, self.category).on_change(Msg::Category).picker_style(PickerStyle::Menu);
        let results = &self.results;
        let catalog = &self.catalog;
        let list: View<'_, Msg> = if results.is_empty() {
            Text::new(self.tr("No matching guides", "Aucun guide trouvé")).foreground(Color::SecondaryLabel).padding(8)
        } else {
            List::new(results.len(), 34, move |row| {
                Text::new(catalog[results[row]].title.clone()).padding_xy(14, 0).fill()
            })
            .selection(self.current.and_then(|c| results.iter().position(|&i| i == c)))
            .on_select(Msg::Article)
            .id("guides")
        };
        VStack::new()
            .push(
                TextField::search(self.tr("Search", "Rechercher"), self.query.clone())
                    .on_change(Msg::Query)
                    .on_submit(Msg::OpenFirst)
                    .shortcut(Shortcut::ctrl('f'))
                    .id("search"),
            )
            .push(categories)
            .push(list.grow(1))
            .spacing(10)
            .alignment(Align::Stretch)
            .padding_insets(Insets { top: 14, leading: 10, bottom: 10, trailing: 10 })
            .fill()
    }
}

impl App for Learn {
    type Message = Msg;

    fn title(&self) -> String {
        self.tr("Learn — WaveOS Guide", "Learn — Guide WaveOS").into()
    }

    fn size(&self) -> (i32, i32) {
        (1020, 620)
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {
            Msg::Back => self.go(true),
            Msg::Forward => self.go(false),
            Msg::Home => self.home(),
            Msg::Language(i) => {
                let locale = if i == 0 { "en" } else { "fr-CA" };
                if locale == self.locale {
                    return;
                }
                let id = self.current.map(|i| self.catalog[i].id.clone());
                let scroll = self.scroll;
                self.locale = locale.into();
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
            Msg::Smaller | Msg::Larger => {
                self.size = (self.size + if matches!(msg, Msg::Smaller) { -2 } else { 2 }).clamp(14, 24);
                let _ = corekit::prefs::set("learn_size", &format!("{}", self.size));
                self.load();
            }
            Msg::Heading(n) => {
                if let Some(i) = self.current {
                    if let Some((id, _)) = self.catalog[i].headings.get(n) {
                        self.ensure_layout();
                        let y = self.doc.borrow().layout.as_ref().and_then(|l| l.anchor(id));
                        if let Some(y) = y {
                            self.scroll = y.clamp(0, self.max_scroll());
                        }
                    }
                }
            }
            Msg::Query(q) => {
                self.query = q;
                self.refresh_results();
            }
            Msg::OpenFirst => {
                if let Some(&i) = self.results.first() {
                    let id = self.catalog[i].id.clone();
                    self.open_id(&id, true);
                }
            }
            Msg::Category(n) => {
                self.category = n;
                self.query.clear();
                self.refresh_results();
            }
            Msg::Article(row) => {
                if let Some(&i) = self.results.get(row) {
                    let id = self.catalog[i].id.clone();
                    self.open_id(&id, true);
                }
            }
            Msg::Resize(w, h) => {
                self.view = (w, h);
                self.scroll = self.scroll.clamp(0, self.max_scroll());
            }
            Msg::Scroll(dy) => {
                let t = corekit::time::uptime_ms();
                self.scroll = (self.scroll + dy).clamp(0, self.max_scroll());
                corekit::println!("learn-perf scroll-update {} ms", corekit::time::uptime_ms() - t);
            }
            Msg::Page(d) => {
                let page = self.view.1 - 40;
                self.scroll = match d {
                    i32::MIN => 0,
                    i32::MAX => self.max_scroll(),
                    d => (self.scroll + d * page).clamp(0, self.max_scroll()),
                };
            }
            Msg::Click(x, y) => {
                self.ensure_layout();
                let href = {
                    let doc = self.doc.borrow();
                    doc.layout.as_ref().and_then(|l| {
                        let link = l.links.iter().find(|k| {
                            k.rect.x <= x
                                && x < k.rect.x + k.rect.w
                                && k.rect.y <= y + self.scroll
                                && y + self.scroll < k.rect.y + k.rect.h
                        })?;
                        doc.page.as_ref()?.element(link.node)?.attr("href").map(String::from)
                    })
                };
                if let Some(href) = href {
                    self.follow(&href);
                }
            }
        }
    }

    fn key(&self, k: &KeyEvent) -> Option<Msg> {
        if k.mods.alt && matches!(k.code, KeyCode::Left | KeyCode::Right) {
            return Some(if k.code == KeyCode::Left { Msg::Back } else { Msg::Forward });
        }
        match k.code {
            KeyCode::PageDown => Some(Msg::Page(1)),
            KeyCode::PageUp => Some(Msg::Page(-1)),
            KeyCode::Home => Some(Msg::Page(i32::MIN)),
            KeyCode::End => Some(Msg::Page(i32::MAX)),
            KeyCode::Down => Some(Msg::Scroll(48)),
            KeyCode::Up => Some(Msg::Scroll(-48)),
            _ => None,
        }
    }

    fn started(&mut self, _cx: &mut Context<Msg>) {
        corekit::println!("learn-perf startup {} ms", corekit::time::uptime_ms() - self.started);
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let headings: Vec<MenuItem<Msg>> = self
            .current
            .map(|i| {
                self.catalog[i]
                    .headings
                    .iter()
                    .enumerate()
                    .map(|(n, (_, t))| MenuItem::new(t.clone(), Msg::Heading(n)))
                    .collect()
            })
            .unwrap_or_default();
        let bar = toolbar(vec![
            Button::symbol(Symbol::ChevronLeft)
                .action(Msg::Back)
                .disabled(self.history_pos == 0)
                .help(self.tr("Back", "Précédent")),
            Button::symbol(Symbol::ChevronRight)
                .action(Msg::Forward)
                .disabled(self.history_pos + 1 >= self.history.len())
                .help(self.tr("Forward", "Suivant")),
            Button::symbol(Symbol::Home).action(Msg::Home).help(self.tr("Home", "Accueil")),
            Button::with(label(Symbol::List, self.tr("Contents", "Sommaire")))
                .button_style(ButtonStyle::Toolbar)
                .menu(headings),
            Spacer::new(),
            Button::with(Text::new("A").style(TextStyle::Sized(11, waveui::style::Weight::Bold)))
                .action(Msg::Smaller)
                .button_style(ButtonStyle::Toolbar)
                .help(self.tr("Smaller text", "Texte plus petit")),
            Button::with(Text::new("A").style(TextStyle::Sized(17, waveui::style::Weight::Bold)))
                .action(Msg::Larger)
                .button_style(ButtonStyle::Toolbar)
                .help(self.tr("Larger text", "Texte plus grand")),
            Picker::new(["English", "Français (Canada)"], (self.locale != "en") as usize)
                .on_change(Msg::Language)
                .picker_style(PickerStyle::Menu)
                .width(180),
        ]);
        let reading: View<'_, Msg> = match &self.error {
            Some(e) => Text::paragraph(e.clone()).padding(24).fill(),
            None => Canvas::new(move |cv, r| self.paint_document(cv, r))
                .on_input(|e| match e {
                    CanvasEvent::Scroll { dy, .. } if dy != 0 => Some(Msg::Scroll(dy)),
                    CanvasEvent::Press { x, y, button: 1, .. } => Some(Msg::Click(x, y)),
                    CanvasEvent::Resize { w, h } => Some(Msg::Resize(w, h)),
                    _ => None,
                })
                .padding_insets(Insets { top: 4, leading: 12, bottom: 0, trailing: 12 })
                .fill(),
        };
        let footer = Text::new(self.tr(
            "Offline library · External links open in Nebula and require a network",
            "Bibliothèque hors ligne · Les liens externes exigent le réseau et ouvrent Nebula",
        ))
        .style(TextStyle::Caption)
        .foreground(Color::SecondaryLabel)
        .padding_xy(20, 6);
        let detail = VStack::new().push(bar).push(Divider::new()).push(reading).push(footer).spacing(0).fill();
        SplitView::new(self.sidebar(), detail).sidebar_width(250)
    }
}
corekit::entry!(main);
/// Scales every `font-size:Npx` in a handbook page from the 16 px it was
/// written for to the reader's text size.
fn scale_font_sizes(html: &str, size: i32) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("font-size:") {
        let (head, tail) = rest.split_at(i + "font-size:".len());
        out.push_str(head);
        let digits = tail.bytes().take_while(u8::is_ascii_digit).count();
        match (tail[..digits].parse::<i32>(), tail[digits..].starts_with("px")) {
            (Ok(n), true) => {
                out.push_str(&format!("{}", n * size / 16));
                rest = &tail[digits..];
            }
            _ => rest = tail,
        }
    }
    out.push_str(rest);
    out
}

fn main(args: corekit::Args) -> i32 {
    if args.get(1).is_some_and(|s| s == "--check-library") {
        waveui::load_fonts();
        let mut learn = Learn::new();
        let mut checked = 0;
        let catalog = learn.catalog.clone();
        if catalog.is_empty() {
            corekit::println!("learn-check FAILED: missing catalog");
            return 1;
        }
        learn.view = (720, 550);
        for a in catalog {
            learn.locale = a.locale.clone();
            learn.open_id(&a.id, false);
            learn.ensure_layout();
            if learn.error.is_some() || learn.doc.borrow().layout.as_ref().is_none_or(|l| l.items.is_empty()) {
                corekit::println!("learn-check FAILED {} {}", a.locale, a.id);
                return 1;
            }
            checked += 1;
        }
        // Exercise the same error path used for an incomplete system image.
        learn.current = Some(0);
        learn.catalog[0].path = "missing-guide.html".into();
        learn.load();
        if learn.error.is_none() || learn.doc.borrow().page.is_some() {
            corekit::println!("learn-check FAILED: missing article did not produce an error");
            return 1;
        }
        corekit::println!("learn-check passed: {} offline articles rendered; missing content handled", checked);
        return 0;
    }
    let mut learn = Learn::new();
    if let Some(id) = args.get(1) {
        learn.open_id(id, false);
    }
    waveui::run(learn)
}
