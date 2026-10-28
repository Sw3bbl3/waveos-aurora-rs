//! Notes — a text editor.
//!
//! Selection with mouse and keyboard, the clipboard, undo/redo, find with
//! highlighted matches, adjustable font and size, word count, Save As, and a
//! reminder about unsaved changes when closing.

#![no_std]
#![no_main]

extern crate alloc;

use corekit::fs;
use waveui::prelude::*;
use waveui::style::Weight;

const SIZES: [u16; 7] = [12, 13, 14, 15, 17, 20, 24];
const FOLDERS: [&str; 4] = ["/Documents", "/Desktop", "/Downloads", "/Pictures"];
const TEXT: &str = "text";
const FIND: &str = "find";

enum Sheet {
    SaveAs { name: String, folder: usize, then_close: bool },
    ConfirmClose,
}

struct Find {
    query: String,
    matches: Vec<(usize, usize)>,
    current: usize,
}

pub struct Notes {
    text: String,
    /// Bumped on every edit; compared with `saved_rev` for "Edited".
    rev: u64,
    saved_rev: u64,
    path: Option<String>,
    mono: bool,
    size: usize,
    find: Option<Find>,
    sheet: Option<Sheet>,
    status: Option<String>,
    caret: (usize, usize),
}

#[derive(Clone)]
pub enum Msg {
    Edit(String),
    Caret((usize, usize)),
    Save,
    SaveAs,
    Font(usize),
    Smaller,
    Larger,
    ToggleFind,
    Query(String),
    FindStep(i32),
    CloseFind,
    SheetName(String),
    SheetFolder(usize),
    SelectStem(usize),
    SheetConfirm,
    SheetCancel,
    Discard,
    NewWindow,
    ClearStatus,
}

impl Notes {
    pub fn new(path: Option<String>) -> Self {
        let text = path
            .as_deref()
            .and_then(|p| fs::read(p).ok())
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        Self {
            text,
            rev: 0,
            saved_rev: 0,
            path,
            mono: false,
            size: 3,
            find: None,
            sheet: None,
            status: None,
            caret: (1, 1),
        }
    }

    fn dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    fn name(&self) -> String {
        match &self.path {
            Some(p) => String::from(fs::file_name(p)),
            None => String::from("Untitled"),
        }
    }

    fn flash(&mut self, cx: &mut Context<Msg>, s: String) {
        self.status = Some(s);
        cx.cancel(|m| matches!(m, Msg::ClearStatus));
        cx.after(4000, Msg::ClearStatus);
    }

    fn write_to(&mut self, cx: &mut Context<Msg>, path: String) -> bool {
        match fs::write(&path, self.text.as_bytes()) {
            Ok(()) => {
                self.flash(cx, format!("Saved to {path}"));
                self.path = Some(path);
                self.saved_rev = self.rev;
                true
            }
            Err(e) => {
                self.flash(cx, format!("Couldn't save: {e}"));
                false
            }
        }
    }

    fn save(&mut self, cx: &mut Context<Msg>) {
        match self.path.clone() {
            Some(p) => {
                self.write_to(cx, p);
            }
            None => self.open_save_as(cx, false),
        }
    }

    fn open_save_as(&mut self, cx: &mut Context<Msg>, then_close: bool) {
        let base = String::from(self.path.as_deref().map(fs::file_name).unwrap_or("Untitled.txt"));
        let folder = self.path.as_deref().and_then(|p| FOLDERS.iter().position(|f| fs::parent(p) == *f)).unwrap_or(0);
        self.sheet = Some(Sheet::SaveAs { name: base.clone(), folder, then_close });
        // Select the name without its extension, like other systems do, once the field shows.
        cx.focus("save-name");
        let stem = base.rfind('.').filter(|&i| i > 0).unwrap_or(base.len());
        cx.after(40, Msg::SelectStem(stem));
    }

    /// Recomputes the matches of the find query.
    fn refresh_find(&mut self, caret: usize) {
        let Some(f) = &mut self.find else { return };
        f.matches.clear();
        let q = f.query.to_lowercase();
        if !q.is_empty() {
            let hay = self.text.to_lowercase();
            if hay.len() == self.text.len() {
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

    fn confirm_sheet(&mut self, cx: &mut Context<Msg>) {
        match self.sheet.take() {
            Some(Sheet::SaveAs { name, folder, then_close }) => {
                let mut n = String::from(name.trim());
                if n.is_empty() {
                    n = String::from("Untitled.txt");
                }
                if !n.contains('.') {
                    n.push_str(".txt");
                }
                let path = fs::join(FOLDERS[folder.min(FOLDERS.len() - 1)], &n);
                if self.write_to(cx, path) && then_close {
                    cx.close();
                }
            }
            Some(Sheet::ConfirmClose) => {
                if self.path.is_some() {
                    self.save(cx);
                    if !self.dirty() {
                        cx.close();
                    }
                } else {
                    self.open_save_as(cx, true);
                }
            }
            None => {}
        }
    }

    fn font_style(&self) -> TextStyle {
        TextStyle::Sized(SIZES[self.size], if self.mono { Weight::Mono } else { Weight::Regular })
    }
}

impl App for Notes {
    type Message = Msg;

    fn title(&self) -> String {
        format!("{} — {}", self.name(), if self.dirty() { "Edited" } else { "Notes" })
    }

    fn size(&self) -> (i32, i32) {
        (640, 480)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Edit(t) => {
                self.text = t;
                self.rev += 1;
                let caret = cx.editor(TEXT).map(|e| e.edit.caret()).unwrap_or(0);
                self.refresh_find(caret);
            }
            Msg::Caret(at) => self.caret = at,
            Msg::Save => self.save(cx),
            Msg::SaveAs => self.open_save_as(cx, false),
            Msg::Font(i) => self.mono = i == 1,
            Msg::Smaller => self.size = self.size.saturating_sub(1),
            Msg::Larger => self.size = (self.size + 1).min(SIZES.len() - 1),
            Msg::ToggleFind => {
                // Seed the query with the selection, then type into the find field.
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
            Msg::SheetName(n) => {
                if let Some(Sheet::SaveAs { name, .. }) = &mut self.sheet {
                    *name = n;
                }
            }
            Msg::SelectStem(stem) => {
                if let Some(f) = cx.field("save-name") {
                    f.edit.select(0, stem.min(f.text().len()));
                }
            }
            Msg::SheetFolder(i) => {
                if let Some(Sheet::SaveAs { folder, .. }) = &mut self.sheet {
                    *folder = i;
                }
            }
            Msg::SheetConfirm => self.confirm_sheet(cx),
            Msg::SheetCancel => self.sheet = None,
            Msg::Discard => cx.close(),
            Msg::NewWindow => cx.open_app("Notes"),
            Msg::ClearStatus => self.status = None,
        }
    }

    fn key(&self, ev: &KeyEvent) -> Option<Msg> {
        if !(ev.mods.ctrl || ev.mods.super_key) {
            return None;
        }
        match ev.ch.map(|c| c.to_ascii_lowercase()) {
            Some('s') if ev.mods.shift => Some(Msg::SaveAs),
            Some('n') => Some(Msg::NewWindow),
            Some('=' | '+') => Some(Msg::Larger),
            Some('-') => Some(Msg::Smaller),
            Some('g') if ev.mods.shift => Some(Msg::FindStep(-1)),
            Some('g') => Some(Msg::FindStep(1)),
            _ => None,
        }
    }

    fn close_requested(&mut self, _cx: &mut Context<Msg>) -> bool {
        if self.dirty() {
            self.sheet = Some(Sheet::ConfirmClose);
            return false;
        }
        true
    }

    fn started(&mut self, cx: &mut Context<Msg>) {
        cx.focus(TEXT);
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        Some(match self.sheet.as_ref()? {
            Sheet::SaveAs { name, folder, .. } => VStack::new()
                .push(Text::new("Save As").style(TextStyle::Title2))
                .push(
                    TextField::new("Name", name.clone())
                        .on_change(Msg::SheetName)
                        .on_submit(Msg::SheetConfirm)
                        .id("save-name"),
                )
                .push(row(
                    "Where",
                    Picker::new(FOLDERS.iter().map(|f| f.trim_start_matches('/')), *folder)
                        .on_change(Msg::SheetFolder)
                        .picker_style(PickerStyle::Menu)
                        .width(160),
                ))
                .push(
                    HStack::new()
                        .push(Spacer::new())
                        .push(Button::new("Cancel").action(Msg::SheetCancel).shortcut(Shortcut::cancel()).min_width(84))
                        .push(
                            Button::new("Save")
                                .action(Msg::SheetConfirm)
                                .button_style(ButtonStyle::Prominent)
                                .shortcut(Shortcut::default_action())
                                .min_width(84),
                        )
                        .spacing(8),
                )
                .spacing(14)
                .alignment(Align::Stretch)
                .width(380),
            Sheet::ConfirmClose => Alert::new(
                format!("Save changes to “{}”?", self.name()),
                "Your changes will be lost if you don't save them.",
            )
            .button("Don't Save", Msg::Discard, AlertRole::Destructive)
            .button("Cancel", Msg::SheetCancel, AlertRole::Cancel)
            .button("Save", Msg::SheetConfirm, AlertRole::Default)
            .view(),
        })
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let label = match &self.status {
            Some(s) => s.clone(),
            None => self.path.clone().unwrap_or_else(|| String::from("Not saved yet")),
        };
        let bar = toolbar(vec![
            Text::new(label).foreground(Color::SecondaryLabel).grow(1),
            Picker::new(["Sans", "Mono"], self.mono as usize).on_change(Msg::Font).width(120).help("Typeface"),
            Button::with(Text::new("A").style(TextStyle::Sized(11, Weight::Bold)))
                .action(Msg::Smaller)
                .button_style(ButtonStyle::Toolbar)
                .shortcut(Shortcut::ctrl('-'))
                .help("Smaller text"),
            Button::with(Text::new("A").style(TextStyle::Sized(17, Weight::Bold)))
                .action(Msg::Larger)
                .button_style(ButtonStyle::Toolbar)
                .help("Larger text"),
            Button::symbol(Symbol::Search).action(Msg::ToggleFind).shortcut(Shortcut::ctrl('f')).help("Find (Ctrl+F)"),
            Button::new("Save").action(Msg::Save).button_style(ButtonStyle::Prominent).shortcut(Shortcut::ctrl('s')),
        ]);
        let find = self.find.as_ref().map(|f| {
            let count = if f.query.is_empty() {
                String::new()
            } else if f.matches.is_empty() {
                String::from("Not found")
            } else {
                format!("{} of {}", f.current + 1, f.matches.len())
            };
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
                        .padding_xy(16, 7)
                        .fill_width(),
                )
                .push(Divider::new())
                .spacing(0)
        });
        let (highlights, current) = match &self.find {
            Some(f) => (f.matches.clone(), (!f.matches.is_empty()).then_some(f.current)),
            None => (Vec::new(), None),
        };
        let editor = TextEditor::new(self.text.clone())
            .on_change(Msg::Edit)
            .on_caret(Msg::Caret)
            .highlights(highlights, current)
            .placeholder("Start typing…")
            .style(self.font_style())
            .id(TEXT)
            .padding_insets(Insets { top: 12, leading: 20, bottom: 8, trailing: 12 });
        let words = self.text.split_whitespace().count();
        let chars = self.text.chars().count();
        let (line, col) = self.caret;
        let info = format!(
            "Line {line}, Column {col}   ·   {words} word{}   ·   {chars} character{}",
            if words == 1 { "" } else { "s" },
            if chars == 1 { "" } else { "s" }
        );
        let status = HStack::new()
            .push(Text::new(info).style(TextStyle::Footnote).foreground(Color::SecondaryLabel).grow(1))
            .push(
                Text::new(if self.dirty() { "Edited" } else { "Saved" })
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel),
            )
            .padding_xy(16, 0)
            .height(26)
            .fill_width();
        VStack::new()
            .push(bar)
            .push(Divider::new())
            .push_if(find)
            .push(editor)
            .push(Divider::new())
            .push(status)
            .spacing(0)
            .fill()
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    let path = args.get(1).map(|p| fs::resolve(&fs::cwd(), p));
    waveui::run(Notes::new(path))
}
