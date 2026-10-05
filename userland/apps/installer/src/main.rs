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
    ui::{self, View},
    App, Env, KeyCode, KeyEvent,
};
use corekit::{apps, fs, sync::Mutex, thread};

#[derive(Clone)]
enum Phase {
    Reading,
    Ready(gina::Manifest),
    Installing(usize, usize),
    Installed(String),
    Failed(String),
}
struct Installer {
    path: Option<String>,
    phase: Arc<Mutex<Phase>>,
    controls: ui::State,
    installed: Vec<apps::Installed>,
    selected: usize,
    delete_data: bool,
}
impl Installer {
    fn new(path: Option<String>) -> Self {
        let phase = Arc::new(Mutex::new(Phase::Reading));
        if let Some(path) = path.clone() {
            let state = phase.clone();
            let result = thread::spawn(move || {
                let result = fs::read(&path)
                    .map_err(|e| format!("Cannot read package: {e}"))
                    .and_then(|b| gina::Package::parse(&b).map(|p| p.manifest).map_err(|e| e.to_string()));
                *state.lock() = match result {
                    Ok(m) => Phase::Ready(m),
                    Err(e) => Phase::Failed(e),
                };
            });
            if let Err(e) = result {
                *phase.lock() = Phase::Failed(format!("Cannot start installer: {e}"));
            }
        }
        Self { path, phase, controls: ui::State::default(), installed: apps::list(), selected: 0, delete_data: false }
    }
    fn act(&mut self, action: Option<ui::Action>, env: &mut Env) -> bool {
        match action {
            Some(ui::Action::Activate(1)) => {
                let Some(path) = self.path.clone() else { return false };
                if !matches!(*self.phase.lock(), Phase::Ready(_)) {
                    return false;
                }
                *self.phase.lock() = Phase::Installing(0, 1);
                let state = self.phase.clone();
                let result = thread::spawn(move || {
                    let result = fs::read(&path)
                        .map_err(|e| format!("Cannot read package: {e}"))
                        .and_then(|b| apps::install(&b, |n, total| *state.lock() = Phase::Installing(n, total)));
                    *state.lock() = match result {
                        Ok(a) => Phase::Installed(a.manifest.id),
                        Err(e) => Phase::Failed(e),
                    };
                });
                if let Err(e) = result {
                    *self.phase.lock() = Phase::Failed(format!("Cannot start installer: {e}"));
                }
            }
            Some(ui::Action::Activate(2)) => {
                let phase = self.phase.lock().clone();
                if let Phase::Installed(id) = phase {
                    if let Err(e) = apps::launch(&id) {
                        *self.phase.lock() = Phase::Failed(e);
                    }
                }
            }
            Some(ui::Action::Activate(3)) => env.requests.push(aurorakit::Request::Close),
            Some(ui::Action::Activate(10)) => {
                if let Some(app) = self.installed.get(self.selected) {
                    match apps::uninstall(&app.manifest.id, self.delete_data) {
                        Ok(()) => {
                            self.installed = apps::list();
                            self.selected = 0;
                        }
                        Err(e) => *self.phase.lock() = Phase::Failed(e),
                    }
                }
            }
            Some(ui::Action::Toggle(11, on)) => self.delete_data = on,
            Some(ui::Action::Activate(id)) if id >= 100 => self.selected = (id - 100) as usize,
            _ => return false,
        }
        true
    }
}
impl App for Installer {
    fn title(&self) -> String {
        "GINA Apps".into()
    }
    fn size(&self) -> (i32, i32) {
        (640, 540)
    }
    fn draw(&mut self, cv: &mut Canvas, r: Rect, _: &Env) {
        let mut rows = alloc::vec![View::heading("GINA Apps"), View::label("Native applications for WaveOS Aurora")];
        if self.path.is_some() {
            match self.phase.lock().clone() {
                Phase::Reading => rows.push(View::label("Reading and validating package…")),
                Phase::Ready(m) => {
                    let update = apps::installed(&m.id).is_ok();
                    rows.push(View::card(alloc::vec![
                        View::heading(&m.name),
                        View::label(&format!("{} · {}", m.developer, m.version)),
                        View::label(&m.id),
                        View::label(if update {
                            "Updates the installed app; app data is preserved."
                        } else {
                            "Installs in Applications and adds the app to Launcher."
                        })
                    ]));
                    rows.push(View::row(alloc::vec![
                        View::button(3, "Cancel"),
                        View::button(1, if update { "Update app" } else { "Install app" })
                    ]));
                }
                Phase::Installing(n, total) => {
                    rows.push(View::heading("Installing…"));
                    rows.push(View::label(&format!("Writing files: {n} of {total}")));
                }
                Phase::Installed(_) => {
                    rows.push(View::heading("Installed"));
                    rows.push(View::label("Your app is ready in Launcher and desktop search."));
                    rows.push(View::row(alloc::vec![View::button(3, "Done"), View::button(2, "Open app")]));
                }
                Phase::Failed(e) => {
                    rows.push(View::heading("Could not install"));
                    for line in aurorakit::widgets::wrap(&e, aurorakit::theme::ui(13), r.w - 64) {
                        rows.push(View::label(&e[line.0..line.1]));
                    }
                    rows.push(View::button(3, "Close"));
                }
            }
        } else {
            rows.push(View::label("Open a .gina package in Files to install an application."));
            for (i, a) in self.installed.iter().take(6).enumerate() {
                rows.push(View::button(
                    100 + i as u64,
                    &format!(
                        "{}{} · {}",
                        if i == self.selected { "✓ " } else { "" },
                        a.manifest.name,
                        a.manifest.version
                    ),
                ));
            }
            if self.installed.is_empty() {
                rows.push(View::label("No third-party applications installed."));
            } else {
                rows.push(View::toggle(11, "Also delete this app's saved data", self.delete_data));
                rows.push(View::button(10, "Uninstall selected app"));
            }
            if let Phase::Failed(e) = self.phase.lock().clone() {
                rows.push(View::label(&e));
            }
        }
        self.controls.draw(cv, &View::column(rows).padding(28).gap(16), r);
    }
    fn click(&mut self, x: i32, y: i32, _: Rect, env: &mut Env) -> bool {
        let a = self.controls.click(x, y);
        self.act(a, env)
    }
    fn hover(&mut self, x: i32, y: i32, _: Rect) -> bool {
        self.controls.hover(x, y)
    }
    fn key(&mut self, k: &KeyEvent, env: &mut Env) -> bool {
        if !k.pressed {
            return false;
        }
        let key = match k.code {
            KeyCode::Tab => {
                if k.mods.shift {
                    ui::Key::Previous
                } else {
                    ui::Key::Next
                }
            }
            KeyCode::Enter => ui::Key::Activate,
            _ => return false,
        };
        let a = self.controls.key(key);
        self.act(a, env);
        true
    }
    fn tick(&mut self, _: &mut Env) -> bool {
        self.path.is_some()
    }
    fn tick_interval(&self) -> u64 {
        300
    }
}
corekit::entry!(main);
fn main(args: corekit::Args) -> i32 {
    aurorakit::run(Installer::new(args.get(1).cloned()))
}
