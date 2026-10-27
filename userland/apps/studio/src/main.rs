//! Studio: WaveSDK projects — edit the code, design the interface, build,
//! run and package apps as `.gina` files.
#![no_std]
#![no_main]
extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, Ordering};
use corekit::{apps, fs, process, sync::Mutex, thread};
use wavesdk as sdk;
use waveui::prelude::*;

const CODE: &str = "code";
const LAYOUT: &str = "layouts/main.ui";

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Code,
    Design,
    Guide,
}

#[derive(Clone)]
enum Pending {
    File(usize),
    Close,
}

/// What the background worker is doing.
#[derive(Clone, Copy, PartialEq)]
pub enum Operation {
    Build,
    BuildRun,
    Export,
    Install,
}

#[derive(Default)]
struct Job {
    running: bool,
    log: String,
    pid: Option<u32>,
}

#[derive(Clone)]
pub enum Msg {
    Project(String),
    New,
    Open,
    Save,
    Mode(usize),
    Run(Operation),
    Stop,
    File(usize),
    Edit(String),
    Caret((usize, usize)),
    PendingSave,
    PendingDiscard,
    PendingCancel,
    Add(&'static str),
    Select(usize),
    Property(String),
    Apply,
    Remove,
    Move(i32),
}

pub struct Studio {
    project: String,
    files: Vec<String>,
    file: Option<String>,
    text: String,
    saved: String,
    caret: (usize, usize),
    mode: Mode,
    status: String,
    property: String,
    selected: Option<usize>,
    pending: Option<Pending>,
    job: Arc<Mutex<Job>>,
    cancel: Arc<AtomicBool>,
    log: String,
}

impl Studio {
    fn new(path: Option<String>) -> Self {
        let mut s = Self {
            project: path.clone().unwrap_or_else(|| String::from("/Documents/Projects/MyApp")),
            files: Vec::new(),
            file: None,
            text: String::new(),
            saved: String::new(),
            caret: (1, 1),
            mode: Mode::Guide,
            status: "Create or open a project to begin.".into(),
            property: String::new(),
            selected: None,
            pending: None,
            job: Arc::new(Mutex::new(Job::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            log: String::new(),
        };
        if path.is_some() {
            s.open_project();
        }
        s
    }

    fn dirty(&self) -> bool {
        self.text != self.saved
    }

    fn open_project(&mut self) {
        match sdk::manifest(&self.project) {
            Ok(_) => {
                self.files.clear();
                let root = self.project.clone();
                self.scan(&root, "");
                self.mode = Mode::Code;
                if let Some(i) = self.files.iter().position(|f| f == "src/main.rs") {
                    self.load(i);
                }
                self.status = "Project opened.".into();
            }
            Err(e) => self.status = e,
        }
    }

    fn scan(&mut self, root: &str, relative: &str) {
        if relative.split('/').count() > 5 || self.files.len() > 128 {
            return;
        }
        let path = if relative.is_empty() { root.into() } else { format!("{root}/{relative}") };
        for e in fs::read_dir(&path).unwrap_or_default() {
            if e.name.starts_with('.') || ["build", "dist", "target"].contains(&e.name.as_str()) {
                continue;
            }
            let name = if relative.is_empty() { e.name.clone() } else { format!("{relative}/{}", e.name) };
            if e.is_dir {
                self.scan(root, &name);
            } else if [".rs", ".toml", ".ui", ".md"].iter().any(|x| e.name.ends_with(x)) {
                self.files.push(name);
            }
        }
        self.files.sort();
    }

    fn load(&mut self, i: usize) {
        if let Some(file) = self.files.get(i).cloned() {
            match fs::read_to_string(&format!("{}/{file}", self.project)) {
                Ok(text) => {
                    self.text = text.clone();
                    self.saved = text;
                    self.file = Some(file);
                }
                Err(e) => self.status = format!("Cannot open file: {e}"),
            }
        }
    }

    fn save(&mut self) -> bool {
        let Some(file) = &self.file else { return false };
        let path = format!("{}/{file}", self.project);
        match fs::write(&path, self.text.as_bytes()) {
            Ok(()) => {
                self.saved = self.text.clone();
                self.status = "Saved.".into();
                if file == LAYOUT {
                    if let Err(e) = sdk::generate_ui(&self.project) {
                        self.status = e;
                    }
                }
                true
            }
            Err(e) => {
                self.status = format!("Could not save: {e}");
                false
            }
        }
    }

    fn finish_pending(&mut self, cx: &mut Context<Msg>) {
        match self.pending.take() {
            Some(Pending::File(i)) => self.load(i),
            Some(Pending::Close) => cx.close(),
            None => {}
        }
    }

    fn design(&mut self) {
        if self.dirty() && !self.save() {
            return;
        }
        if let Some(i) = self.files.iter().position(|f| f == LAYOUT) {
            self.load(i);
            self.mode = Mode::Design;
            self.selected = None;
        } else {
            self.status = "This project has no layouts/main.ui to design.".into();
        }
    }

    /// The layout's lines, edited and written back to the text.
    fn edit_layout(&mut self, f: impl FnOnce(&mut Vec<String>)) {
        let mut lines: Vec<String> = self.text.lines().map(String::from).collect();
        f(&mut lines);
        self.text = lines.join("\n") + "\n";
    }

    fn perform(&mut self, operation: Operation) {
        if self.job.lock().running {
            return;
        }
        if self.dirty() && !self.save() {
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        let project = self.project.clone();
        let job = self.job.clone();
        let cancel = self.cancel.clone();
        {
            let mut j = job.lock();
            j.running = true;
            j.log.clear();
        }
        let spawn = thread::spawn(move || {
            let result = (|| -> sdk::Result<()> {
                if matches!(operation, Operation::Build | Operation::BuildRun) {
                    sdk::build(
                        &project,
                        |s| {
                            let mut j = job.lock();
                            j.log.push_str(s);
                            if j.log.len() > 64000 {
                                j.log = String::from("Earlier build output truncated.\n");
                            }
                        },
                        &cancel,
                    )?;
                }
                if operation == Operation::Build {
                    return Ok(());
                }
                let (path, bytes) = sdk::package(&project)?;
                job.lock().log.push_str(&format!("Exported {path}\n"));
                if operation == Operation::Export {
                    return Ok(());
                }
                let app = apps::install(&bytes, |n, t| {
                    job.lock().log.push_str(&format!("Installed file {n}/{t}\n"));
                })?;
                if operation == Operation::BuildRun {
                    let pipe = process::pipe(true).map_err(|e| format!("{e}"))?;
                    let pid = process::spawn(
                        &app.executable,
                        &[],
                        process::Stdio { stdin: None, stdout: Some(pipe[1]), stderr: Some(pipe[1]) },
                    )
                    .map_err(|e| format!("{e}"))?;
                    process::close(pipe[1]);
                    job.lock().pid = Some(pid);
                    job.lock().log.push_str(&format!("Running {} on this desktop (pid {pid}).\n", app.manifest.name));
                    let log = job.clone();
                    let _ = thread::spawn(move || {
                        let mut reader = unsafe { fs::File::from_owned_fd(pipe[0]) };
                        let mut buf = [0; 1024];
                        loop {
                            while let Ok(n) = reader.read(&mut buf) {
                                if n == 0 {
                                    break;
                                }
                                let mut j = log.lock();
                                if j.log.len() < 64000 {
                                    j.log.push_str(&String::from_utf8_lossy(&buf[..n]));
                                }
                            }
                            if let Ok(code) = process::wait(pid, Some(0)) {
                                let mut j = log.lock();
                                j.pid = None;
                                j.log.push_str(&format!("Process exited: {code}\n"));
                                break;
                            }
                            corekit::time::sleep_ms(50);
                        }
                    });
                }
                Ok(())
            })();
            let mut j = job.lock();
            j.running = false;
            match result {
                Ok(()) => j.log.push_str("Operation complete.\n"),
                Err(e) => j.log.push_str(&format!("Error: {e}\n")),
            }
        });
        if let Err(e) = spawn {
            let mut j = self.job.lock();
            j.running = false;
            j.log = format!("Cannot start worker: {e}");
        }
    }

    fn stop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(pid) = self.job.lock().pid.take() {
            let _ = process::kill(pid);
        }
    }

    // ------------------------------------------------------------ views

    fn sidebar(&self) -> View<'_, Msg> {
        let files = &self.files;
        let list: View<'_, Msg> = if files.is_empty() {
            Text::paragraph("Open a project to see its files.")
                .foreground(Color::SecondaryLabel)
                .padding(10)
                .align(Align::Start)
        } else {
            List::new(files.len(), 30, move |i| {
                let f = &files[i];
                let symbol = if f.ends_with(".rs") { Symbol::Document } else { Symbol::Text };
                label(symbol, f.clone()).padding_xy(12, 0).fill()
            })
            .selection(self.file.as_ref().and_then(|f| files.iter().position(|x| x == f)))
            .on_select(Msg::File)
            .id("files")
        };
        VStack::new()
            .push(Text::new("PROJECT").style(TextStyle::Caption).foreground(Color::SecondaryLabel).padding_xy(12, 0))
            .push(list.grow(1))
            .spacing(8)
            .alignment(Align::Stretch)
            .padding_insets(Insets { top: 14, leading: 6, bottom: 8, trailing: 6 })
            .fill()
    }

    fn guide(&self) -> View<'_, Msg> {
        let missing = sdk::doctor();
        let steps = [
            "Choose a project folder, then New or Open.",
            "Edit Rust source, or use Design for the interface.",
            "Build & Run installs and launches the real app.",
            "Export a .gina package to share your app.",
        ];
        let mut v = VStack::new()
            .push(Text::new("Develop directly on WaveOS").style(TextStyle::LargeTitle))
            .push(
                Text::paragraph(
                    "Apps are written in Rust with WaveUI, the WaveSDK's declarative interface framework: \
                     describe your interface from your app's state, and WaveUI keeps the window in step.",
                )
                .foreground(Color::SecondaryLabel),
            )
            .spacing(12)
            .alignment(Align::Stretch);
        let mut card = Section::new();
        for (i, s) in steps.iter().enumerate() {
            card = card.push(
                HStack::new()
                    .push(
                        Text::centered(format!("{}", i + 1))
                            .style(TextStyle::Headline)
                            .foreground(Color::OnAccent)
                            .size(24, 24)
                            .background_rounded(Color::Accent, 12),
                    )
                    .push(Text::new(*s))
                    .spacing(12)
                    .min_height(size::ROW)
                    .fill_width(),
            );
        }
        v = v.push(card);
        let (title, detail) = if missing.is_empty() {
            ("Toolchain ready", String::from("Native compiler executables were found."))
        } else {
            (
                "Native Rust tools are not installed in this image",
                String::from(
                    "Build & Run reports this prerequisite; it never simulates a build. Export and install work.",
                ),
            )
        };
        v = v.push(Section::titled(
            "Toolchain",
            Section::new().push(row_detail(
                title,
                detail,
                Image::symbol(if missing.is_empty() { Symbol::Check } else { Symbol::Warning })
                    .foreground(if missing.is_empty() { Color::Green } else { Color::Orange }),
            )),
        ));
        ScrollView::new(v.padding(28).fill_width())
    }

    fn design_view(&self) -> View<'_, Msg> {
        let mut preview = VStack::new().spacing(12).alignment(Align::Start);
        for (i, line) in self.text.lines().enumerate() {
            let Some((kind, text)) = line.split_once(' ') else { continue };
            let node: View<'_, Msg> = match kind {
                "heading" => Text::new(text).style(TextStyle::Title),
                "button" => Button::new(text),
                "toggle" => Toggle::new(text, false).width(260),
                _ => Text::paragraph(text),
            };
            preview = preview.push(
                node.padding(4)
                    .border(if self.selected == Some(i) { Color::Accent } else { Color::Clear }, 6)
                    .on_tap(Msg::Select(i))
                    .corner_radius(6),
            );
        }
        let has = self.selected.is_some();
        let palette = VStack::new()
            .push(Text::new("COMPONENTS").style(TextStyle::Caption).foreground(Color::SecondaryLabel))
            .push(Button::with(label(Symbol::Plus, "Heading")).action(Msg::Add("heading Heading")).fill_width())
            .push(Button::with(label(Symbol::Plus, "Label")).action(Msg::Add("label Label")).fill_width())
            .push(Button::with(label(Symbol::Plus, "Button")).action(Msg::Add("button Button")).fill_width())
            .push(Button::with(label(Symbol::Plus, "Toggle")).action(Msg::Add("toggle Toggle")).fill_width())
            .push(Divider::new())
            .push(Text::new("INSPECTOR").style(TextStyle::Caption).foreground(Color::SecondaryLabel))
            .push(
                TextField::new("Component label", self.property.clone())
                    .on_change(Msg::Property)
                    .on_submit(Msg::Apply)
                    .disabled(!has),
            )
            .push(
                HStack::new()
                    .push(Button::new("Apply").action(Msg::Apply).disabled(!has).grow(1))
                    .push(
                        Button::new("Remove")
                            .action(Msg::Remove)
                            .button_style(ButtonStyle::Destructive)
                            .disabled(!has)
                            .grow(1),
                    )
                    .spacing(8),
            )
            .push(
                HStack::new()
                    .push(Button::symbol(Symbol::ChevronUp).action(Msg::Move(-1)).disabled(!has).help("Move up"))
                    .push(Button::symbol(Symbol::ChevronDown).action(Msg::Move(1)).disabled(!has).help("Move down"))
                    .spacing(4),
            )
            .spacing(8)
            .alignment(Align::Stretch)
            .width(190);
        HStack::new()
            .push(
                ScrollView::new(preview.padding(20).fill_width())
                    .background_rounded(Color::GroupedBackground, 14)
                    .grow(1),
            )
            .push(palette)
            .spacing(16)
            .alignment(Align::Start)
            .padding(16)
            .fill()
    }

    fn code_view(&self) -> View<'_, Msg> {
        let rust = self.file.as_deref().is_some_and(|f| f.ends_with(".rs"));
        let mut editor = TextEditor::monospaced(self.text.clone())
            .on_change(Msg::Edit)
            .on_caret(Msg::Caret)
            .placeholder("Choose a file in the project.")
            .style(TextStyle::Sized(14, waveui::style::Weight::Mono))
            .id(CODE);
        if rust {
            editor = editor.rust_syntax();
        }
        editor.padding_insets(Insets { top: 12, leading: 16, bottom: 8, trailing: 8 }).fill()
    }
}

impl App for Studio {
    type Message = Msg;

    fn title(&self) -> String {
        format!("Studio{}", if self.dirty() { " — Edited" } else { "" })
    }

    fn size(&self) -> (i32, i32) {
        (1180, 740)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Project(p) => self.project = p,
            Msg::New => {
                if self.dirty() {
                    self.status = "Save your current file before creating a project.".into();
                } else {
                    let slug = self.project.rsplit('/').next().unwrap_or("app").to_ascii_lowercase();
                    let id = format!("dev.local.{slug}");
                    match sdk::new_project(&self.project, &id, "My App") {
                        Ok(()) => self.open_project(),
                        Err(e) => self.status = e,
                    }
                }
            }
            Msg::Open => {
                if self.dirty() {
                    self.status = "Save your current file before opening another project.".into();
                } else {
                    self.open_project();
                }
            }
            Msg::Save => {
                self.save();
            }
            Msg::Mode(i) => match i {
                0 => self.mode = Mode::Code,
                1 => self.design(),
                _ => self.mode = Mode::Guide,
            },
            Msg::Run(op) => self.perform(op),
            Msg::Stop => self.stop(),
            Msg::File(i) => {
                if self.files.get(i) == self.file.as_ref() {
                    return;
                }
                if self.dirty() {
                    self.pending = Some(Pending::File(i));
                } else {
                    self.load(i);
                }
            }
            Msg::Edit(t) => self.text = t,
            Msg::Caret(at) => self.caret = at,
            Msg::PendingSave => {
                if self.save() {
                    self.finish_pending(cx);
                }
            }
            Msg::PendingDiscard => {
                self.text = self.saved.clone();
                self.finish_pending(cx);
            }
            Msg::PendingCancel => self.pending = None,
            Msg::Add(node) => {
                let mut text = self.text.clone();
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(node);
                text.push('\n');
                self.text = text;
                self.selected = Some(self.text.lines().count() - 1);
                self.property = String::from(node.split_once(' ').map(|p| p.1).unwrap_or(""));
            }
            Msg::Select(i) => {
                self.selected = Some(i);
                self.property = self
                    .text
                    .lines()
                    .nth(i)
                    .and_then(|l| l.split_once(' '))
                    .map(|p| String::from(p.1))
                    .unwrap_or_default();
            }
            Msg::Property(p) => self.property = p,
            Msg::Apply => {
                if let Some(i) = self.selected {
                    let label = self.property.replace('\n', " ");
                    self.edit_layout(|lines| {
                        if let Some(line) = lines.get_mut(i) {
                            let kind = String::from(line.split_once(' ').map(|p| p.0).unwrap_or("label"));
                            *line = format!("{kind} {label}");
                        }
                    });
                }
            }
            Msg::Remove => {
                if let Some(i) = self.selected.take() {
                    self.edit_layout(|lines| {
                        if i < lines.len() {
                            lines.remove(i);
                        }
                    });
                }
            }
            Msg::Move(d) => {
                if let Some(i) = self.selected {
                    let n = self.text.lines().count();
                    let to = (i as i32 + d).clamp(0, n as i32 - 1) as usize;
                    if to != i {
                        self.edit_layout(|lines| lines.swap(i, to));
                        self.selected = Some(to);
                    }
                }
            }
        }
    }

    fn close_requested(&mut self, _cx: &mut Context<Msg>) -> bool {
        if self.dirty() {
            self.pending = Some(Pending::Close);
            false
        } else {
            true
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(250)
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        let job = self.job.lock();
        if job.log != self.log {
            self.log = job.log.clone();
            return true;
        }
        false
    }

    fn sheet(&self, _env: &Env) -> Option<View<'_, Msg>> {
        self.pending.as_ref()?;
        Some(
            Alert::new("Save your changes?", "Your edits to this file have not been saved.")
                .button("Don't Save", Msg::PendingDiscard, AlertRole::Destructive)
                .button("Cancel", Msg::PendingCancel, AlertRole::Cancel)
                .button("Save", Msg::PendingSave, AlertRole::Default)
                .view(),
        )
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let running = self.job.lock().running;
        let mode = match self.mode {
            Mode::Code => 0,
            Mode::Design => 1,
            Mode::Guide => 2,
        };
        let bar = toolbar(vec![
            TextField::new("Project folder", self.project.clone())
                .on_change(Msg::Project)
                .on_submit(Msg::Open)
                .width(300),
            Button::new("New").action(Msg::New),
            Button::new("Open").action(Msg::Open),
            Button::new("Save").action(Msg::Save).disabled(!self.dirty()).shortcut(Shortcut::ctrl('s')),
            Spacer::new(),
            Picker::new(["Code", "Design", "Guide"], mode).on_change(Msg::Mode).width(230),
            Spacer::new(),
            Button::with(label(Symbol::Share, "Share"))
                .button_style(ButtonStyle::Toolbar)
                .menu(vec![
                    MenuItem::new("Export .gina Package", Msg::Run(Operation::Export)),
                    MenuItem::new("Install Built App", Msg::Run(Operation::Install)),
                ])
                .disabled(running),
            Button::new("Build").action(Msg::Run(Operation::Build)).disabled(running).shortcut(Shortcut::ctrl('b')),
            if running {
                Button::with(label(Symbol::Stop, "Stop")).action(Msg::Stop).button_style(ButtonStyle::Destructive)
            } else {
                Button::with(label(Symbol::Play, "Build & Run"))
                    .action(Msg::Run(Operation::BuildRun))
                    .button_style(ButtonStyle::Prominent)
                    .shortcut(Shortcut::ctrl('r'))
            },
        ]);
        let content = match self.mode {
            Mode::Code => self.code_view(),
            Mode::Design => self.design_view(),
            Mode::Guide => self.guide(),
        };
        let output = VStack::new()
            .push(
                HStack::new()
                    .push(Text::new("OUTPUT").style(TextStyle::Caption).foreground(Color::SecondaryLabel).grow(1))
                    .push_if(running.then(ProgressView::spinner))
                    .padding_xy(16, 0)
                    .height(28)
                    .fill_width(),
            )
            .push(
                ScrollView::new(
                    Text::paragraph(if self.log.is_empty() {
                        String::from("Build output appears here.")
                    } else {
                        self.log.clone()
                    })
                    .style(TextStyle::Sized(12, waveui::style::Weight::Mono))
                    .foreground(if self.log.is_empty() { Color::SecondaryLabel } else { Color::Label })
                    .padding_xy(16, 4)
                    .fill_width(),
                )
                .stick_to_end()
                .id("output"),
            )
            .spacing(0)
            .height(170)
            .background(Color::GroupedBackground);
        let status = HStack::new()
            .push(
                Text::new(if running { String::from("Working…") } else { self.status.clone() })
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel)
                    .grow(1),
            )
            .push_if((self.mode == Mode::Code && self.file.is_some()).then(|| {
                Text::new(format!("Line {}, Column {}", self.caret.0, self.caret.1))
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel)
            }))
            .padding_xy(16, 0)
            .height(26)
            .fill_width();
        let detail = VStack::new()
            .push(content)
            .push(Divider::new())
            .push(output)
            .push(Divider::new())
            .push(status)
            .spacing(0)
            .fill();
        VStack::new()
            .push(bar)
            .push(Divider::new())
            .push(SplitView::new(self.sidebar(), detail).sidebar_width(220))
            .spacing(0)
            .fill()
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    waveui::run(Studio::new(args.get(1).cloned()))
}
