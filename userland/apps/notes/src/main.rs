//! Notes — a library of notes.
//!
//! Notes live as text files in /Documents/Notes; the first line of each is
//! its title. The sidebar lists them newest first and seeks through them;
//! the editor saves as you type, renames the file after the title, and
//! draws `- [ ] ` lines as checkboxes. A file opened from Files is edited in
//! place and shown at the top of the list.

#![no_std]
#![no_main]

extern crate alloc;

use corekit::fs;
use corekit::process::desktop;
use waveui::prelude::*;
use waveui::style::Weight;

const LIBRARY: &str = "/Documents/Notes";
const SIZES: [u16; 7] = [12, 13, 14, 15, 17, 20, 24];
const TITLE: &str = "title";
const TEXT: &str = "text";
const FIND: &str = "find";
const BULLET: &str = "• ";
const TASK: &str = waveui::text::TASK_OPEN;
/// Saves this long after the last keystroke.
const AUTOSAVE_MS: u64 = 800;

const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A note in the sidebar.
struct Entry {
    path: String,
    title: String,
    preview: String,
    /// Title and body in lower case, for seeking.
    haystack: String,
    mtime: u64,
}

struct Find {
    query: String,
    matches: Vec<(usize, usize)>,
    current: usize,
}

pub struct Notes {
    library: Vec<Entry>,
    /// A file opened from elsewhere (not in the library).
    outside: Option<Entry>,
    /// The note being edited; `None` for a new note not saved yet.
    path: Option<String>,
    title: String,
    body: String,
    mtime: u64,
    /// Bumped on every edit; compared with `saved_rev`.
    rev: u64,
    saved_rev: u64,
    query: String,
    find: Option<Find>,
    size: usize,
    caret: (usize, usize),
    status: Option<String>,
}

#[derive(Clone)]
pub enum Msg {
    Open(String),
    New,
    Title(String),
    Edit(String),
    Caret((usize, usize)),
    FocusBody,
    Autosave,
    Save,
    Seek(String),
    Wrap(&'static str),
    Lines(&'static str),
    Share,
    Trash(String),
    Smaller,
    Larger,
    ToggleFind,
    Query(String),
    FindStep(i32),
    CloseFind,
    NewWindow,
    ClearStatus,
}

/// Seconds since 2000-01-01, local time (as file times are kept).
fn now_secs() -> u64 {
    let d = corekit::time::now();
    let days = days_since_2000(d.year as i64, d.month as i64, d.day as i64);
    (days.max(0) as u64) * 86400 + d.hour as u64 * 3600 + d.minute as u64 * 60 + d.second as u64
}

fn days_since_2000(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468 - 10957
}

/// "Today", "Yesterday" or "Oct 22".
fn short_date(secs: u64) -> String {
    let today = now_secs() / 86400;
    let day = secs / 86400;
    if day == today {
        String::from("Today")
    } else if day + 1 == today {
        String::from("Yesterday")
    } else {
        let d = corekit::time::from_seconds(secs);
        format!("{} {}", corekit::time::MONTHS[(d.month.clamp(1, 12) - 1) as usize], d.day)
    }
}

/// "Monday 26 October 2026".
fn long_date(secs: u64) -> String {
    let d = corekit::time::from_seconds(secs);
    format!("{} {} {} {}", WEEKDAYS[d.weekday as usize % 7], d.day, MONTHS[(d.month.clamp(1, 12) - 1) as usize], d.year)
}

fn clock(secs: u64) -> String {
    let d = corekit::time::from_seconds(secs);
    format!("{}:{:02}", d.hour, d.minute)
}

/// The first line with words in it, without list or checkbox marks.
fn preview_of(body: &str) -> String {
    let line = body.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let line = line.trim_start_matches(BULLET).trim_start_matches(TASK);
    let line = line.strip_prefix("- [x] ").or_else(|| line.strip_prefix("- [X] ")).unwrap_or(line);
    let mut s: String = line.chars().take(80).collect();
    if s.is_empty() {
        s.push_str("No more text");
    }
    s
}

/// Splits a library file into its title (the first line) and body.
fn split(text: &str) -> (String, String) {
    match text.split_once('\n') {
        Some((t, b)) => (String::from(t.trim_end_matches('\r')), String::from(b)),
        None => (String::from(text), String::new()),
    }
}

fn entry(path: String, mtime: u64, outside: bool) -> Entry {
    let text = fs::read(&path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let (title, body) = if outside { (String::from(fs::file_name(&path)), text) } else { split(&text) };
    let title = if title.trim().is_empty() { String::from("New Note") } else { title };
    let mut haystack = title.to_lowercase();
    haystack.push('\n');
    haystack.push_str(&body.chars().take(4000).collect::<String>().to_lowercase());
    Entry { path, preview: preview_of(&body), title, haystack, mtime }
}

/// A file name from a title: no slashes, not too long.
fn file_stem(title: &str) -> String {
    let s: String =
        title.trim().chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '-' } else { c }).take(60).collect();
    let s = String::from(s.trim().trim_start_matches('.'));
    if s.is_empty() {
        String::from("Untitled")
    } else {
        s
    }
}

impl Notes {
    pub fn new(path: Option<String>) -> Self {
        if !fs::is_dir(LIBRARY) {
            if !fs::is_dir("/Documents") {
                let _ = fs::mkdir("/Documents");
            }
            let _ = fs::mkdir(LIBRARY);
        }
        let mut n = Self {
            library: Vec::new(),
            outside: None,
            path: None,
            title: String::new(),
            body: String::new(),
            mtime: now_secs(),
            rev: 0,
            saved_rev: 0,
            query: String::new(),
            find: None,
            size: 3,
            caret: (1, 1),
            status: None,
        };
        n.reload();
        if let Some(p) = path.filter(|p| fs::exists(p)) {
            if !n.in_library(&p) {
                let mtime = fs::stat(&p).map(|s| s.mtime).unwrap_or(0);
                n.outside = Some(entry(p.clone(), mtime, true));
            }
            n.load(&p);
        } else if let Some(first) = n.library.first().map(|e| e.path.clone()) {
            n.load(&first);
        }
        n
    }

    fn in_library(&self, path: &str) -> bool {
        fs::parent(path) == LIBRARY
    }

    fn is_outside(&self) -> bool {
        self.path.as_deref().is_some_and(|p| !self.in_library(p))
    }

    fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    fn reload(&mut self) {
        self.library = fs::read_dir(LIBRARY)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.is_dir && !e.name.starts_with('.'))
            .filter(|e| e.name.ends_with(".txt") || e.name.ends_with(".md") || !e.name.contains('.'))
            .map(|e| entry(fs::join(LIBRARY, &e.name), e.mtime, false))
            .collect();
        self.library.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.title.cmp(&b.title)));
    }

    fn load(&mut self, path: &str) {
        let text = fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
        if self.in_library(path) {
            (self.title, self.body) = split(&text);
        } else {
            self.title = String::from(fs::file_name(path));
            self.body = text;
        }
        self.mtime = fs::stat(path).map(|s| s.mtime).unwrap_or_else(|_| now_secs());
        self.path = Some(String::from(path));
        self.rev = 0;
        self.saved_rev = 0;
        self.find = None;
    }

    fn flash(&mut self, cx: &mut Context<Msg>, s: String) {
        self.status = Some(s);
        cx.cancel(|m| matches!(m, Msg::ClearStatus));
        cx.after(3000, Msg::ClearStatus);
    }

    /// Writes the note if it changed. A library note is renamed after its
    /// title when that name is free.
    fn save(&mut self, cx: &mut Context<Msg>) -> bool {
        cx.cancel(|m| matches!(m, Msg::Autosave));
        if !self.dirty() {
            return true;
        }
        let outside = self.is_outside();
        if self.path.is_none() && self.title.trim().is_empty() && self.body.trim().is_empty() {
            self.saved_rev = self.rev;
            return true;
        }
        let content = if outside { self.body.clone() } else { format!("{}\n{}", self.title, self.body) };
        let stem = file_stem(&self.title);
        let target = match &self.path {
            Some(p) if outside => p.clone(),
            Some(p) => {
                let wanted = fs::join(LIBRARY, &format!("{stem}.txt"));
                if *p != wanted && !fs::exists(&wanted) && fs::rename(p, &wanted).is_ok() {
                    wanted
                } else {
                    p.clone()
                }
            }
            None => fs::unique_name(LIBRARY, &stem, ".txt"),
        };
        match fs::write(&target, content.as_bytes()) {
            Ok(()) => {
                self.path = Some(target.clone());
                self.saved_rev = self.rev;
                self.mtime = fs::stat(&target).map(|s| s.mtime).unwrap_or_else(|_| now_secs());
                if outside {
                    self.outside = Some(entry(target, self.mtime, true));
                } else {
                    self.reload();
                }
                true
            }
            Err(e) => {
                self.flash(cx, format!("Couldn't save: {e}"));
                false
            }
        }
    }

    fn edited(&mut self, cx: &mut Context<Msg>) {
        self.rev += 1;
        cx.cancel(|m| matches!(m, Msg::Autosave));
        cx.after(AUTOSAVE_MS, Msg::Autosave);
    }

    /// Wraps the selection in `mark` (`**` bold, `_` italic).
    fn wrap(&mut self, cx: &mut Context<Msg>, mark: &str) {
        let now = cx.now();
        let Some(e) = cx.editor(TEXT) else { return };
        let (a, b) = e.edit.selection().unwrap_or((e.edit.caret(), e.edit.caret()));
        let inner = String::from(&e.edit.text()[a..b]);
        e.edit.select(a, b);
        e.edit.insert(&format!("{mark}{inner}{mark}"), now);
        if inner.is_empty() {
            e.edit.set_caret(a + mark.len(), false);
        } else {
            e.edit.select(a + mark.len(), b + mark.len());
        }
        self.body = String::from(e.edit.text());
        cx.focus(TEXT);
        self.edited(cx);
    }

    /// Turns the selected lines into a list (`• `) or a checklist (`- [ ] `),
    /// or back into plain lines when they all already are.
    fn toggle_lines(&mut self, cx: &mut Context<Msg>, mark: &'static str) {
        let now = cx.now();
        let Some(e) = cx.editor(TEXT) else { return };
        let (a, b) = e.edit.selection().unwrap_or((e.edit.caret(), e.edit.caret()));
        let text = e.edit.text();
        let start = text[..a].rfind('\n').map_or(0, |i| i + 1);
        let end = text[b..].find('\n').map_or(text.len(), |i| b + i);
        let marks = [BULLET, TASK, "- [x] ", "- [X] "];
        let strip = |l: &str| -> String {
            let m = marks.iter().find(|m| l.starts_with(**m)).map_or(0, |m| m.len());
            String::from(&l[m..])
        };
        let lines: Vec<&str> = text[start..end].split('\n').collect();
        let all = lines.iter().all(|l| {
            if mark == TASK {
                l.starts_with(TASK) || l.starts_with("- [x] ") || l.starts_with("- [X] ")
            } else {
                l.starts_with(mark)
            }
        });
        let out: Vec<String> =
            lines.iter().map(|l| if all { strip(l) } else { format!("{mark}{}", strip(l)) }).collect();
        let out = out.join("\n");
        e.edit.select(start, end);
        e.edit.insert(&out, now);
        self.body = String::from(e.edit.text());
        cx.focus(TEXT);
        self.edited(cx);
    }

    /// What starts the line the caret is on.
    fn caret_line(&self) -> &str {
        self.body.split('\n').nth(self.caret.0.saturating_sub(1)).unwrap_or("")
    }

    fn refresh_find(&mut self, caret: usize) {
        let Some(f) = &mut self.find else { return };
        f.matches.clear();
        let q = f.query.to_lowercase();
        if !q.is_empty() {
            let hay = self.body.to_lowercase();
            if hay.len() == self.body.len() {
                let mut from = 0;
                while let Some(i) = hay[from..].find(&q) {
                    let a = from + i;
                    f.matches.push((a, a + q.len()));
                    from = a + q.len().max(1);
                    if f.matches.len() >= 5000 {
                        break;
                    }
                }
            }
        }
        f.current = f.matches.iter().position(|&(a, _)| a >= caret).unwrap_or(0);
    }

    fn find_step(&mut self, cx: &mut Context<Msg>, delta: i32) {
        let Some(f) = &mut self.find else { return };
        if f.matches.is_empty() {
            return;
        }
        let n = f.matches.len() as i32;
        f.current = (f.current as i32 + delta).rem_euclid(n) as usize;
        let (a, b) = f.matches[f.current];
        if let Some(e) = cx.editor(TEXT) {
            e.edit.select(a, b);
            e.reveal(a);
        }
    }

    fn row<'a>(&self, e: &'a Entry, selected: bool) -> View<'a, Msg> {
        VStack::new()
            .push(Text::new(e.title.clone()).style(TextStyle::Sized(14, Weight::Bold)).foreground(Color::Label))
            .push(
                Text::new(format!("{} · {}", short_date(e.mtime), e.preview))
                    .style(TextStyle::Sized(12, Weight::Bold))
                    .foreground(Color::TertiaryLabel),
            )
            .spacing(3)
            .alignment(Align::Start)
            .padding_xy(14, 12)
            .fill_width()
            .corner_radius(14)
            .selected(selected)
            .on_tap(Msg::Open(e.path.clone()))
            .context_menu(vec![
                MenuItem::new("Open in New Window", Msg::NewWindow),
                MenuItem::new("Move to Trash", Msg::Trash(e.path.clone())),
            ])
    }

    fn sidebar(&self) -> View<'_, Msg> {
        let q = self.query.to_lowercase();
        let mut list = VStack::new().spacing(2).alignment(Align::Stretch);
        if self.path.is_none() {
            list = list.push(
                VStack::new()
                    .push(
                        Text::new(if self.title.trim().is_empty() { "New Note" } else { self.title.as_str() })
                            .style(TextStyle::Sized(14, Weight::Bold))
                            .foreground(Color::Label),
                    )
                    .push(
                        Text::new(format!("Today · {}", preview_of(&self.body)))
                            .style(TextStyle::Sized(12, Weight::Bold))
                            .foreground(Color::TertiaryLabel),
                    )
                    .spacing(3)
                    .alignment(Align::Start)
                    .padding_xy(14, 12)
                    .fill_width()
                    .corner_radius(14)
                    .selected(true),
            );
        }
        let shown = self.outside.iter().chain(self.library.iter()).filter(|e| q.is_empty() || e.haystack.contains(&q));
        let mut any = false;
        for e in shown {
            any = true;
            list = list.push(self.row(e, self.path.as_deref() == Some(e.path.as_str())));
        }
        if !any && self.path.is_some() {
            list = list.push(
                Text::centered(if q.is_empty() { "No notes yet" } else { "No notes match" })
                    .style(TextStyle::Callout)
                    .foreground(Color::SecondaryLabel)
                    .padding_xy(12, 24)
                    .fill_width(),
            );
        }
        VStack::new()
            .push(
                HStack::new()
                    .push(
                        TextField::search("Seek in Notes", self.query.clone())
                            .on_change(Msg::Seek)
                            .shortcut(Shortcut::ctrl_shift('f'))
                            .id("seek")
                            .grow(1),
                    )
                    .push(
                        Button::symbol(Symbol::Pencil)
                            .action(Msg::New)
                            .shortcut(Shortcut::ctrl('n'))
                            .background_rounded(Color::Fill, 15)
                            .corner_radius(15)
                            .size(30, 30)
                            .help("New Note (Ctrl+N)"),
                    )
                    .spacing(8)
                    .padding_insets(Insets { top: 12, leading: 12, bottom: 10, trailing: 12 })
                    .fill_width(),
            )
            .push(ScrollView::new(list.padding_insets(Insets { top: 0, leading: 10, bottom: 12, trailing: 10 })))
            .spacing(0)
            .fill()
    }

    fn toolbar<'a>(&'a self) -> View<'a, Msg> {
        let line = self.caret_line();
        let in_list = line.starts_with(BULLET);
        let in_tasks = line.starts_with(TASK) || line.starts_with("- [x] ") || line.starts_with("- [X] ");
        let tool = |v: View<'a, Msg>, on: bool| -> View<'a, Msg> {
            let b = v.size(30, 30).corner_radius(15);
            if on {
                b.background_rounded(Color::SecondaryFill, 15)
            } else {
                b
            }
        };
        let bold =
            Button::with(Text::new("B").style(TextStyle::Sized(14, Weight::Bold))).button_style(ButtonStyle::Toolbar);
        // An italic I, slanted by hand.
        let italic = Button::with(
            Canvas::new(|s, r| {
                let f = TextStyle::Sized(15, Weight::Regular).font();
                let y = r.y + (r.h + f.ascent as i32 + f.descent as i32) / 2;
                s.text_slanted(r.x + (r.w - f.width("I")) / 2, y, "I", f, waveui::lumen::theme::current().text);
            })
            .size(14, 20),
        )
        .button_style(ButtonStyle::Toolbar);
        let status = match &self.status {
            Some(s) => s.clone(),
            None if self.path.is_none() && !self.dirty() => String::new(),
            None => format!("Edited {}", clock(self.mtime)),
        };
        toolbar(vec![
            tool(bold.action(Msg::Wrap("**")).shortcut(Shortcut::ctrl('b')).help("Bold (Ctrl+B)"), false),
            tool(italic.action(Msg::Wrap("_")).shortcut(Shortcut::ctrl('i')).help("Italic (Ctrl+I)"), false),
            tool(Button::symbol(Symbol::List).action(Msg::Lines(BULLET)).help("Bulleted List"), in_list),
            tool(
                Button::symbol(Symbol::Check)
                    .action(Msg::Lines(TASK))
                    .shortcut(Shortcut::ctrl_shift('l'))
                    .help("Checklist (Ctrl+Shift+L)"),
                in_tasks,
            ),
            tool(
                Button::symbol(Symbol::Search)
                    .action(Msg::ToggleFind)
                    .shortcut(Shortcut::ctrl('f'))
                    .help("Find (Ctrl+F)"),
                self.find.is_some(),
            ),
            Spacer::new(),
            Text::new(status).style(TextStyle::Caption).foreground(Color::TertiaryLabel),
            tool(Button::symbol(Symbol::Share).action(Msg::Share).help("Copy Note"), false),
        ])
    }

    fn find_bar(&self) -> Option<View<'_, Msg>> {
        let f = self.find.as_ref()?;
        let count = if f.query.is_empty() {
            String::new()
        } else if f.matches.is_empty() {
            String::from("Not found")
        } else {
            format!("{} of {}", f.current + 1, f.matches.len())
        };
        Some(
            VStack::new()
                .push(
                    HStack::new()
                        .push(
                            TextField::search("Find", f.query.clone())
                                .on_change(Msg::Query)
                                .on_submit(Msg::FindStep(1))
                                .id(FIND)
                                .grow(1),
                        )
                        .push(Text::new(count).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                        .push(Button::symbol(Symbol::ChevronUp).action(Msg::FindStep(-1)).help("Previous match"))
                        .push(Button::symbol(Symbol::ChevronDown).action(Msg::FindStep(1)).help("Next match"))
                        .push(Button::new("Done").action(Msg::CloseFind).shortcut(Shortcut::cancel()))
                        .spacing(8)
                        .padding_xy(18, 8)
                        .fill_width(),
                )
                .push(Divider::new())
                .spacing(0),
        )
    }

    fn page(&self) -> View<'_, Msg> {
        let (highlights, current) = match &self.find {
            Some(f) => (f.matches.clone(), (!f.matches.is_empty()).then_some(f.current)),
            None => (Vec::new(), None),
        };
        let title = if self.is_outside() {
            Text::new(self.title.clone()).style(TextStyle::Sized(32, Weight::Display))
        } else {
            TextField::new("Title", self.title.clone())
                .plain()
                .style(TextStyle::Sized(32, Weight::Display))
                .on_change(Msg::Title)
                .on_submit(Msg::FocusBody)
                .id(TITLE)
        };
        let column = VStack::new()
            .push(
                Text::new(long_date(self.mtime))
                    .style(TextStyle::Sized(12, Weight::Bold))
                    .foreground(Color::TertiaryLabel),
            )
            .push(title.fill_width())
            .push(
                TextEditor::new(self.body.clone())
                    .on_change(Msg::Edit)
                    .on_caret(Msg::Caret)
                    .highlights(highlights, current)
                    .checklists()
                    .placeholder("Start writing…")
                    .style(TextStyle::Sized(SIZES[self.size], Weight::Regular))
                    .line_spacing(SIZES[self.size] as i32 / 2)
                    .id(TEXT)
                    .fill()
                    .grow(1),
            )
            .spacing(10)
            .alignment(Align::Stretch)
            .fill()
            .max_width(720);
        HStack::new()
            .push(column)
            .push(Spacer::new())
            .padding_insets(Insets { top: 34, leading: 56, bottom: 12, trailing: 40 })
            .fill()
    }
}

impl App for Notes {
    type Message = Msg;

    fn title(&self) -> String {
        if self.title.trim().is_empty() {
            String::from("Notes")
        } else {
            format!("{} — Notes", self.title)
        }
    }

    fn size(&self) -> (i32, i32) {
        (1140, 728)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Open(p) => {
                if self.path.as_deref() != Some(p.as_str()) && self.save(cx) {
                    self.load(&p);
                }
            }
            Msg::New => {
                if self.save(cx) {
                    self.path = None;
                    self.title.clear();
                    self.body.clear();
                    self.mtime = now_secs();
                    self.rev = 0;
                    self.saved_rev = 0;
                    self.find = None;
                    self.query.clear();
                    cx.focus(TITLE);
                }
            }
            Msg::Title(t) => {
                self.title = t.replace('\n', " ");
                self.edited(cx);
            }
            Msg::Edit(t) => {
                self.body = t;
                self.edited(cx);
                let caret = cx.editor(TEXT).map(|e| e.edit.caret()).unwrap_or(0);
                self.refresh_find(caret);
            }
            Msg::Caret(at) => self.caret = at,
            Msg::FocusBody => {
                cx.focus(TEXT);
                if let Some(e) = cx.editor(TEXT) {
                    e.edit.set_caret(0, false);
                }
            }
            Msg::Autosave | Msg::Save => {
                self.save(cx);
            }
            Msg::Seek(q) => self.query = q,
            Msg::Wrap(mark) => self.wrap(cx, mark),
            Msg::Lines(mark) => self.toggle_lines(cx, mark),
            Msg::Share => {
                let text = if self.is_outside() { self.body.clone() } else { format!("{}\n{}", self.title, self.body) };
                cx.copy(&text);
                self.flash(cx, String::from("Copied"));
            }
            Msg::Trash(p) => {
                if self.path.as_deref() == Some(p.as_str()) {
                    // Nothing left to save: the note is going away.
                    self.saved_rev = self.rev;
                    cx.cancel(|m| matches!(m, Msg::Autosave));
                }
                match desktop::trash(&p) {
                    Ok(()) => {
                        if self.outside.as_ref().is_some_and(|e| e.path == p) {
                            self.outside = None;
                        }
                        self.reload();
                        if self.path.as_deref() == Some(p.as_str()) {
                            match self.library.first().map(|e| e.path.clone()) {
                                Some(next) => self.load(&next),
                                None => cx.send(Msg::New),
                            }
                        }
                    }
                    Err(e) => self.flash(cx, format!("Couldn't move to Trash: {e}")),
                }
            }
            Msg::Smaller => self.size = self.size.saturating_sub(1),
            Msg::Larger => self.size = (self.size + 1).min(SIZES.len() - 1),
            Msg::ToggleFind => {
                let selected = cx.editor(TEXT).map(|e| String::from(e.edit.selected_text())).unwrap_or_default();
                let caret = cx.editor(TEXT).map(|e| e.edit.caret()).unwrap_or(0);
                let seed = if !selected.is_empty() && !selected.contains('\n') { Some(selected) } else { None };
                match (&mut self.find, seed) {
                    (Some(f), Some(s)) => f.query = s,
                    (Some(_), None) => {}
                    (None, s) => {
                        self.find = Some(Find { query: s.unwrap_or_default(), matches: Vec::new(), current: 0 })
                    }
                }
                self.refresh_find(caret);
                cx.focus(FIND);
                if let Some(f) = cx.field(FIND) {
                    f.edit.select_all();
                }
            }
            Msg::Query(q) => {
                if let Some(f) = &mut self.find {
                    f.query = q;
                }
                let caret = cx.editor(TEXT).map(|e| e.edit.caret()).unwrap_or(0);
                self.refresh_find(caret);
            }
            Msg::FindStep(d) => self.find_step(cx, d),
            Msg::CloseFind => {
                self.find = None;
                cx.focus(TEXT);
            }
            Msg::NewWindow => cx.open_app("Notes"),
            Msg::ClearStatus => self.status = None,
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        if !(ev.mods.ctrl || ev.mods.super_key) {
            return None;
        }
        match ev.ch.map(|c| c.to_ascii_lowercase()) {
            Some('s') => Some(Msg::Save),
            Some('=' | '+') => Some(Msg::Larger),
            Some('-') => Some(Msg::Smaller),
            Some('g') if ev.mods.shift => Some(Msg::FindStep(-1)),
            Some('g') => Some(Msg::FindStep(1)),
            _ => None,
        }
    }

    fn close_requested(&mut self, cx: &mut Context<Msg>) -> bool {
        self.save(cx)
    }

    fn started(&mut self, cx: &mut Context<Msg>) {
        if self.path.is_none() {
            cx.focus(TITLE);
        } else {
            cx.focus(TEXT);
        }
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let detail = VStack::new().push(self.toolbar()).push_if(self.find_bar()).push(self.page()).spacing(0).fill();
        SplitView::new(self.sidebar(), detail).sidebar_width(290)
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    waveui::run(Notes::new(path))
}
