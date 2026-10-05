//! Constellation Studio: native projects, editing, visual layouts and GINA tools.
#![no_std]
#![no_main]
extern crate alloc;
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use aurorakit::{
    canvas::Canvas,
    geom::Rect,
    text::{TextField, TextView},
    theme,
    ui::{self, View},
    App, Env, KeyCode, KeyEvent, Request,
};
use constellation_sdk as sdk;
use core::sync::atomic::{AtomicBool, Ordering};
use corekit::{apps, fs, process, sync::Mutex, thread};

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
#[derive(Default)]
struct Job {
    running: bool,
    log: String,
    pid: Option<u32>,
}
struct Studio {
    project: TextField,
    project_focused: bool,
    files: Vec<String>,
    file: Option<String>,
    editor: TextView,
    saved: String,
    controls: ui::State,
    preview: ui::State,
    mode: Mode,
    status: String,
    area: Rect,
    editor_area: Rect,
    design_drag: Option<(usize, i32)>,
    property: TextField,
    property_focused: bool,
    selected: Option<usize>,
    pending: Option<Pending>,
    job: Arc<Mutex<Job>>,
    cancel: Arc<AtomicBool>,
    log_view: TextView,
}
impl Studio {
    fn new(path: Option<String>) -> Self {
        let mut s = Self {
            project: TextField::new(path.as_deref().unwrap_or("/Documents/Projects/MyApp"), "Project folder"),
            project_focused: false,
            files: Vec::new(),
            file: None,
            editor: TextView::new("", theme::mono(14)),
            saved: String::new(),
            controls: ui::State::default(),
            preview: ui::State::default(),
            mode: Mode::Guide,
            status: "Create or open a project to begin.".into(),
            area: Rect::default(),
            editor_area: Rect::default(),
            design_drag: None,
            property: TextField::new("", "Component label"),
            property_focused: false,
            selected: None,
            pending: None,
            job: Arc::new(Mutex::new(Job::default())),
            cancel: Arc::new(AtomicBool::new(false)),
            log_view: TextView::new("", theme::mono(12)),
        };
        s.editor.rust_syntax = true;
        if path.is_some() {
            s.open_project();
        }
        s
    }
    fn dirty(&self) -> bool {
        self.editor.edit.text() != self.saved
    }
    fn open_project(&mut self) {
        match sdk::manifest(self.project.text()) {
            Ok(_) => {
                self.files.clear();
                let root = self.project.text().to_string();
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
            } else if e.name.ends_with(".rs")
                || e.name.ends_with(".toml")
                || e.name.ends_with(".ui")
                || e.name.ends_with(".md")
            {
                self.files.push(name);
            }
        }
        self.files.sort();
    }
    fn load(&mut self, i: usize) {
        if let Some(file) = self.files.get(i).cloned() {
            match fs::read_to_string(&format!("{}/{file}", self.project.text())) {
                Ok(text) => {
                    self.editor.edit.set_text(&text);
                    self.saved = text;
                    self.editor.rust_syntax = file.ends_with(".rs");
                    self.file = Some(file);
                    self.editor.scroll = 0;
                }
                Err(e) => self.status = format!("Cannot open file: {e}"),
            }
        }
    }
    fn save(&mut self) -> bool {
        let Some(file) = &self.file else { return false };
        let path = format!("{}/{file}", self.project.text());
        match fs::write(&path, self.editor.edit.text().as_bytes()) {
            Ok(()) => {
                self.saved = self.editor.edit.text().into();
                self.status = "Saved.".into();
                if file == "layouts/main.ui" {
                    if let Err(e) = sdk::generate_ui(self.project.text()) {
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
    fn finish_pending(&mut self, env: &mut Env) {
        match self.pending.take() {
            Some(Pending::File(i)) => self.load(i),
            Some(Pending::Close) => env.requests.push(Request::Close),
            None => {}
        }
    }
    fn select_file(&mut self, i: usize) {
        if self.dirty() {
            self.pending = Some(Pending::File(i));
        } else {
            self.load(i);
        }
    }
    fn design(&mut self) {
        if self.dirty() && !self.save() {
            return;
        }
        if let Some(i) = self.files.iter().position(|f| f == "layouts/main.ui") {
            self.load(i);
            self.mode = Mode::Design;
            self.selected = None;
        }
    }
    fn perform(&mut self, operation: u64) {
        if operation == 9 {
            self.cancel.store(true, Ordering::Relaxed);
            if let Some(pid) = self.job.lock().pid.take() {
                let _ = process::kill(pid);
            }
            return;
        }
        if self.job.lock().running {
            return;
        }
        if self.dirty() && !self.save() {
            return;
        }
        self.cancel.store(false, Ordering::Relaxed);
        let project = self.project.text().to_string();
        let job = self.job.clone();
        let cancel = self.cancel.clone();
        {
            let mut j = job.lock();
            j.running = true;
            j.log.clear();
        }
        let spawn = thread::spawn(move || {
            let result = (|| -> sdk::Result<()> {
                if operation == 7 || operation == 8 {
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
                if operation == 8 || operation == 10 || operation == 11 {
                    let (path, bytes) = sdk::package(&project)?;
                    job.lock().log.push_str(&format!("Exported {path}\n"));
                    if operation == 8 || operation == 11 {
                        let app = apps::install(&bytes, |n, t| {
                            job.lock().log.push_str(&format!("Installed file {n}/{t}\n"));
                        })?;
                        if operation == 8 {
                            let pipe = process::pipe(true).map_err(|e| format!("{e}"))?;
                            let pid = process::spawn(
                                &app.executable,
                                &[],
                                process::Stdio { stdin: None, stdout: Some(pipe[1]), stderr: Some(pipe[1]) },
                            )
                            .map_err(|e| format!("{e}"))?;
                            let _ = process::close(pipe[1]);
                            job.lock().pid = Some(pid);
                            job.lock()
                                .log
                                .push_str(&format!("Running {} on this desktop (pid {pid}).\n", app.manifest.name));
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
                    }
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
    fn action(&mut self, action: Option<ui::Action>, env: &mut Env) -> bool {
        let Some(ui::Action::Activate(id)) = action else { return false };
        match id {
            1 => {
                if self.dirty() {
                    self.status = "Save your current file before creating a project.".into();
                } else {
                    let slug = self.project.text().rsplit('/').next().unwrap_or("app").to_ascii_lowercase();
                    let id = format!("dev.local.{slug}");
                    match sdk::new_project(self.project.text(), &id, "My App") {
                        Ok(()) => self.open_project(),
                        Err(e) => self.status = e,
                    }
                }
            }
            2 => {
                if self.dirty() {
                    self.status = "Save your current file before opening another project.".into();
                } else {
                    self.open_project();
                }
            }
            3 => {
                self.save();
            }
            4 => self.mode = Mode::Code,
            5 => self.design(),
            6 => self.mode = Mode::Guide,
            7 | 8 | 9 | 10 | 11 => self.perform(id),
            20 => {
                if self.save() {
                    self.finish_pending(env);
                }
            }
            21 => self.finish_pending(env),
            22 => self.pending = None,
            30..=33 => {
                let node = match id {
                    30 => "heading Heading",
                    31 => "label Label",
                    32 => "button Button",
                    _ => "toggle Toggle",
                };
                let mut text = self.editor.edit.text().to_string();
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                text.push_str(node);
                text.push('\n');
                self.editor.edit.set_text(&text);
            }
            34 => {
                if let Some(i) = self.selected {
                    let mut lines: Vec<String> = self.editor.edit.text().lines().map(String::from).collect();
                    if let Some(line) = lines.get_mut(i) {
                        let kind = line.split_once(' ').map(|p| p.0).unwrap_or("label");
                        *line = format!("{kind} {}", self.property.text().replace('\n', " "));
                        self.editor.edit.set_text(&(lines.join("\n") + "\n"));
                    }
                }
            }
            35 => {
                if let Some(i) = self.selected {
                    let mut lines: Vec<&str> = self.editor.edit.text().lines().collect();
                    if i < lines.len() {
                        lines.remove(i);
                        let text = lines.join("\n") + "\n";
                        self.editor.edit.set_text(&text);
                        self.selected = None;
                    }
                }
            }
            100..=299 => self.select_file((id - 100) as usize),
            _ => {}
        }
        true
    }
}
impl App for Studio {
    fn title(&self) -> String {
        format!("Constellation Studio{}", if self.dirty() { " — Edited" } else { "" })
    }
    fn size(&self) -> (i32, i32) {
        (1180, 740)
    }
    fn draw(&mut self, cv: &mut Canvas, r: Rect, _: &Env) {
        self.area = r;
        let t = theme::current();
        cv.fill_rect(r, t.window_bg);
        self.project.draw(cv, Rect::new(r.x + 16, r.y + 12, (r.w - 500).max(200), 34), self.project_focused);
        let buttons = View::row(alloc::vec![
            View::button(1, "New"),
            View::button(2, "Open"),
            View::button(3, "Save"),
            View::button(7, "Build"),
            View::button(8, "Build & Run"),
            View::button(9, "Stop")
        ])
        .gap(8);
        // One control tree includes toolbar, navigation, files, palette, and modal.
        let body = Rect::new(r.x + 220, r.y + 108, (r.w - 236).max(1), (r.h - 300).max(1));
        self.editor_area = body;
        let mut controls = View::column(alloc::vec![
            buttons,
            View::row(alloc::vec![
                View::button(4, "Code"),
                View::button(5, "Design"),
                View::button(6, "Guide"),
                View::button(10, "Export .gina"),
                View::button(11, "Install built app")
            ])
            .gap(8)
        ])
        .gap(12);
        controls.height = Some(88);
        self.controls.draw(cv, &controls, Rect::new(r.x + 16, r.y + 54, r.w - 32, 88));
        // File navigator uses shared button rendering; coordinates are also used below.
        cv.fill_rect(Rect::new(r.x, r.y + 146, 204, r.h - 174), t.window_bg_alt);
        cv.text(r.x + 16, r.y + 170, "PROJECT", theme::ui_bold(11), t.text_secondary);
        for (i, file) in self.files.iter().take(18).enumerate() {
            let row = Rect::new(r.x + 8, r.y + 184 + i as i32 * 25, 188, 24);
            if self.file.as_ref() == Some(file) {
                cv.fill_round_rect(row, 6, t.hover);
            }
            cv.text_clipped(row.x + 8, row.y + 17, file, theme::ui(12), t.text, row.w - 16);
        }
        let editor = Rect::new(body.x, r.y + 154, body.w, (r.h - 340).max(80));
        self.editor_area = editor;
        match self.mode {
            Mode::Code => {
                cv.fill_round_rect(editor.inset(-4), 10, t.window_bg_alt);
                self.editor.draw(cv, editor, !self.project_focused && !self.property_focused && self.pending.is_none());
            }
            Mode::Guide => {
                let lines = [
                    "Constellation Studio",
                    "Develop directly on WaveOS",
                    "1. Choose a project folder, then New or Open.",
                    "2. Edit Rust source, or use Design for the interface.",
                    "3. Build & Run installs and launches the real app.",
                    "4. Export a .gina package to share your app.",
                    "",
                    "Toolchain status",
                ];
                for (i, line) in lines.iter().enumerate() {
                    cv.text(
                        editor.x + 16,
                        editor.y + 30 + i as i32 * 27,
                        line,
                        if i == 0 { theme::ui_bold(24) } else { theme::ui(14) },
                        t.text,
                    );
                }
                let missing = sdk::doctor();
                let note = if missing.is_empty() {
                    "Native compiler executables found."
                } else {
                    "Native Rust tools are not installed in this image."
                };
                cv.text(editor.x + 16, editor.y + 258, note, theme::ui(13), t.text_secondary);
                cv.text(
                    editor.x + 16,
                    editor.y + 284,
                    "Build & Run reports this prerequisite; it never simulates a build.",
                    theme::ui(12),
                    t.text_secondary,
                );
            }
            Mode::Design => {
                let preview = Rect::new(editor.x, editor.y, (editor.w - 192).max(160), editor.h);
                cv.fill_round_rect(preview, 14, t.window_bg_alt);
                let mut nodes = Vec::new();
                for (i, line) in self.editor.edit.text().lines().enumerate() {
                    if let Some((kind, text)) = line.split_once(' ') {
                        nodes.push(match kind {
                            "heading" => View::heading(text),
                            "button" => View::button(i as u64 + 1, text),
                            "toggle" => View::toggle(i as u64 + 1, text, false),
                            _ => View::label(text),
                        });
                    }
                }
                self.preview.draw(cv, &View::column(nodes).padding(20), preview);
                let x = preview.right() + 16;
                cv.text(x, editor.y + 20, "COMPONENTS", theme::ui_bold(11), t.text_secondary);
                for (i, label) in ["+ Heading", "+ Label", "+ Button", "+ Toggle"].iter().enumerate() {
                    aurorakit::widgets::button(
                        cv,
                        Rect::new(x, editor.y + 36 + i as i32 * 40, 160, 32),
                        label,
                        aurorakit::widgets::ButtonStyle::Secondary,
                        false,
                    );
                }
                cv.text(x, editor.y + 218, "INSPECTOR", theme::ui_bold(11), t.text_secondary);
                self.property.draw(cv, Rect::new(x, editor.y + 232, 160, 34), self.property_focused);
                aurorakit::widgets::button(
                    cv,
                    Rect::new(x, editor.y + 278, 76, 32),
                    "Apply",
                    aurorakit::widgets::ButtonStyle::Secondary,
                    false,
                );
                aurorakit::widgets::button(
                    cv,
                    Rect::new(x + 84, editor.y + 278, 76, 32),
                    "Remove",
                    aurorakit::widgets::ButtonStyle::Secondary,
                    false,
                );
            }
        }
        let output = Rect::new(body.x, r.bottom() - 166, body.w, 126);
        cv.fill_round_rect(output.inset(-4), 10, t.window_bg_alt);
        self.log_view.draw(cv, output, false);
        let job = self.job.lock();
        cv.text_clipped(
            r.x + 16,
            r.bottom() - 12,
            if job.running { "Working…" } else { &self.status },
            theme::ui(12),
            t.text_secondary,
            r.w - 32,
        );
        drop(job);
        if self.pending.is_some() {
            cv.fill_rect(r, 0x99000000);
            let panel = Rect::new(r.x + (r.w - 460) / 2, r.y + (r.h - 170) / 2, 460, 170);
            cv.fill_round_rect(panel, 16, t.window_bg);
            cv.text(panel.x + 24, panel.y + 42, "Save your changes?", theme::ui_bold(22), t.text);
            cv.text(panel.x + 24, panel.y + 75, "Your edits have not been saved.", theme::ui(14), t.text_secondary);
            for (i, label) in ["Save", "Discard", "Cancel"].iter().enumerate() {
                aurorakit::widgets::button(
                    cv,
                    Rect::new(panel.x + 24 + i as i32 * 140, panel.y + 112, 128, 34),
                    label,
                    aurorakit::widgets::ButtonStyle::Secondary,
                    false,
                );
            }
        }
    }
    fn click(&mut self, x: i32, y: i32, _: Rect, env: &mut Env) -> bool {
        let r = self.area;
        if self.pending.is_some() {
            let p = Rect::new(r.x + (r.w - 460) / 2, r.y + (r.h - 170) / 2, 460, 170);
            for i in 0..3 {
                if Rect::new(p.x + 24 + i * 140, p.y + 112, 128, 34).contains(x, y) {
                    return self.action(Some(ui::Action::Activate(20 + i as u64)), env);
                }
            }
            return true;
        }
        self.project_focused = self.project.click(x, y, env.mods.shift, env.now_ms);
        if self.project_focused {
            return true;
        }
        if let Some(a) = self.controls.click(x, y) {
            return self.action(Some(a), env);
        }
        if x < r.x + 204 && y >= r.y + 184 {
            let i = ((y - r.y - 184) / 25) as usize;
            if i < self.files.len() {
                self.select_file(i);
                return true;
            }
        }
        self.property_focused = false;
        if self.mode == Mode::Code {
            return self.editor.click(x, y, env.mods.shift, env.now_ms);
        }
        if self.mode == Mode::Design {
            let editor = self.editor_area;
            let px = editor.x + (editor.w - 192).max(160) + 16;
            for i in 0..4 {
                if Rect::new(px, editor.y + 36 + i * 40, 160, 32).contains(x, y) {
                    return self.action(Some(ui::Action::Activate(30 + i as u64)), env);
                }
            }
            self.property_focused = self.property.click(x, y, env.mods.shift, env.now_ms);
            if self.property_focused {
                return true;
            }
            for i in 0..2 {
                if Rect::new(px + i * 84, editor.y + 278, 76, 32).contains(x, y) {
                    return self.action(Some(ui::Action::Activate(34 + i as u64)), env);
                }
            }
            if x >= editor.x && x < px - 16 {
                let mut top = editor.y + 20;
                for (i, line) in self.editor.edit.text().lines().enumerate() {
                    let height = match line.split_once(' ').map(|p| p.0).unwrap_or("") {
                        "heading" => 34,
                        "button" => 36,
                        "toggle" => 44,
                        _ => 24,
                    };
                    if y >= top && y < top + height {
                        self.selected = Some(i);
                        self.design_drag = Some((i, y));
                        self.property.edit.set_text(line.split_once(' ').map(|p| p.1).unwrap_or(""));
                        return true;
                    }
                    top += height + 12;
                }
            }
        }
        false
    }
    fn hover(&mut self, x: i32, y: i32, _: Rect) -> bool {
        self.controls.hover(x, y)
    }
    fn drag(&mut self, x: i32, y: i32, _: Rect, _: &mut Env) -> bool {
        if let Some((_, at)) = self.design_drag.as_mut() {
            *at = y;
            return true;
        }
        if self.project_focused {
            self.project.drag(x)
        } else if self.property_focused {
            self.property.drag(x)
        } else {
            self.editor.drag(x, y)
        }
    }
    fn release(&mut self, _: i32, y: i32, _: Rect, _: &mut Env) -> bool {
        if let Some((from, _)) = self.design_drag.take() {
            let mut lines: Vec<String> = self.editor.edit.text().lines().map(String::from).collect();
            let mut top = self.editor_area.y + 20;
            let mut to = lines.len().saturating_sub(1);
            for (i, line) in lines.iter().enumerate() {
                let h = match line.split_once(' ').map(|p| p.0).unwrap_or("") {
                    "heading" => 34,
                    "button" => 36,
                    "toggle" => 44,
                    _ => 24,
                };
                if y < top + h + 6 {
                    to = i;
                    break;
                }
                top += h + 12;
            }
            if from < lines.len() && from != to {
                let line = lines.remove(from);
                lines.insert(to.min(lines.len()), line);
                self.editor.edit.set_text(&(lines.join("\n") + "\n"));
                self.selected = Some(to);
            }
        }
        self.editor.release();
        self.project.release();
        self.property.release();
        false
    }
    fn scroll(&mut self, d: i32, _: Rect) -> bool {
        self.editor.scroll_by(d)
    }
    fn key(&mut self, k: &KeyEvent, env: &mut Env) -> bool {
        if self.pending.is_some() {
            if k.pressed && k.code == KeyCode::Escape {
                self.pending = None;
                return true;
            }
            return false;
        }
        if k.pressed && k.mods.ctrl {
            match k.ch.map(|c| c.to_ascii_lowercase()) {
                Some('s') => {
                    self.save();
                    return true;
                }
                Some('r') => {
                    self.perform(8);
                    return true;
                }
                _ => {}
            }
        }
        if self.project_focused {
            self.project.key(k, env.now_ms).handled
        } else if self.property_focused {
            self.property.key(k, env.now_ms).handled
        } else if self.mode == Mode::Code {
            self.editor.key(k, env.now_ms).handled
        } else {
            false
        }
    }
    fn close_requested(&mut self, _: &mut Env) -> bool {
        if self.dirty() {
            self.pending = Some(Pending::Close);
            false
        } else {
            true
        }
    }
    fn tick(&mut self, env: &mut Env) -> bool {
        let log = self.job.lock().log.clone();
        let changed = log != self.log_view.edit.text();
        if changed {
            self.log_view.edit.set_text(&log);
            self.log_view.scroll = i32::MAX / 4;
        }
        self.editor.tick(env.now_ms, env.focused)
            | self.project.tick(env.now_ms, self.project_focused)
            | self.property.tick(env.now_ms, self.property_focused)
            | changed
    }
}
corekit::entry!(main);
fn main(args: corekit::Args) -> i32 {
    aurorakit::run(Studio::new(args.get(1).cloned()))
}
