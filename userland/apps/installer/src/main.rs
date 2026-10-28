//! Installer — installs `.gina` packages and removes installed apps.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use corekit::{apps, fs, sync::Mutex, thread};
use waveui::prelude::*;

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
    installed: Vec<apps::Installed>,
    selected: Option<usize>,
    delete_data: bool,
    /// What the last frame showed, to redraw only when the phase moves.
    shown: (u8, usize),
}

#[derive(Clone)]
enum Msg {
    Install,
    Open,
    Close,
    Select(usize),
    DeleteData(bool),
    Uninstall,
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
        Self { path, phase, installed: apps::list(), selected: None, delete_data: false, shown: (0, 0) }
    }

    fn package(&self) -> View<'_, Msg> {
        let mut v = VStack::new().spacing(16).alignment(Align::Stretch);
        match self.phase.lock().clone() {
            Phase::Reading => {
                v = v.push(
                    HStack::new()
                        .push(ProgressView::spinner())
                        .push(Text::new("Reading and validating the package…").foreground(Color::SecondaryLabel))
                        .spacing(10),
                )
            }
            Phase::Ready(m) => {
                let update = apps::installed(&m.id).is_ok();
                v = v
                    .push(
                        Section::new().push(
                            VStack::new()
                                .push(Text::new(m.name.clone()).style(TextStyle::Title2))
                                .push(
                                    Text::new(format!("{} · {}", m.developer, m.version))
                                        .foreground(Color::SecondaryLabel),
                                )
                                .push(
                                    Text::new(m.id.clone())
                                        .style(TextStyle::Footnote)
                                        .foreground(Color::SecondaryLabel),
                                )
                                .push(Text::paragraph(if update {
                                    "Updates the installed app; its data is kept."
                                } else {
                                    "Installs in Applications and adds the app to Shoal."
                                }))
                                .spacing(4)
                                .alignment(Align::Start)
                                .padding_xy(0, 14)
                                .fill_width(),
                        ),
                    )
                    .push(
                        HStack::new()
                            .push(Spacer::new())
                            .push(Button::new("Cancel").action(Msg::Close).shortcut(Shortcut::cancel()))
                            .push(
                                Button::new(if update { "Update App" } else { "Install App" })
                                    .action(Msg::Install)
                                    .button_style(ButtonStyle::Prominent)
                                    .shortcut(Shortcut::default_action()),
                            ),
                    );
            }
            Phase::Installing(n, total) => {
                v = v
                    .push(Text::new("Installing…").style(TextStyle::Title2))
                    .push(ProgressView::new(Some(n as f32 / total.max(1) as f32)))
                    .push(Text::new(format!("Writing files: {n} of {total}")).foreground(Color::SecondaryLabel));
            }
            Phase::Installed(_) => {
                v = v
                    .push(Text::new("Installed").style(TextStyle::Title2))
                    .push(Text::new("Your app is ready in Shoal and in Seek.").foreground(Color::SecondaryLabel))
                    .push(
                        HStack::new().push(Spacer::new()).push(Button::new("Done").action(Msg::Close)).push(
                            Button::new("Open App")
                                .action(Msg::Open)
                                .button_style(ButtonStyle::Prominent)
                                .shortcut(Shortcut::default_action()),
                        ),
                    );
            }
            Phase::Failed(e) => {
                v = v
                    .push(Text::new("Could not install").style(TextStyle::Title2))
                    .push(Text::paragraph(e).foreground(Color::SecondaryLabel))
                    .push(HStack::new().push(Spacer::new()).push(Button::new("Close").action(Msg::Close)));
            }
        }
        v
    }

    fn manage(&self) -> View<'_, Msg> {
        let mut v = VStack::new()
            .push(
                Text::new("Open a .gina package in Files to install an application.").foreground(Color::SecondaryLabel),
            )
            .spacing(16)
            .alignment(Align::Stretch);
        if self.installed.is_empty() {
            v = v.push(
                Section::card(
                    Text::centered("No apps from other developers are installed.")
                        .foreground(Color::SecondaryLabel)
                        .padding(28),
                )
                .fill_width(),
            );
        } else {
            let apps = &self.installed;
            v = v
                .push(Section::titled(
                    "Installed apps",
                    List::new(apps.len(), 44, move |i| {
                        let a = &apps[i];
                        HStack::new()
                            .push(Text::new(a.manifest.name.clone()).style(TextStyle::Headline).grow(1))
                            .push(Text::new(a.manifest.version.clone()).foreground(Color::SecondaryLabel))
                            .padding_xy(18, 0)
                            .fill()
                    })
                    .selection(self.selected)
                    .on_select(Msg::Select)
                    .height((apps.len() as i32 * 44).min(264))
                    .background_rounded(Color::GroupedBackground, 12),
                ))
                .push(
                    HStack::new()
                        .push(
                            Toggle::checkbox("Also delete the app's saved data", self.delete_data)
                                .on_change(Msg::DeleteData),
                        )
                        .push(Spacer::new())
                        .push(
                            Button::new("Uninstall…")
                                .action(Msg::Uninstall)
                                .button_style(ButtonStyle::Destructive)
                                .disabled(self.selected.is_none()),
                        ),
                );
        }
        if let Phase::Failed(e) = self.phase.lock().clone() {
            v = v.push(Text::paragraph(e).foreground(Color::Red));
        }
        v
    }
}

impl App for Installer {
    type Message = Msg;

    fn title(&self) -> String {
        "Installer".into()
    }
    fn size(&self) -> (i32, i32) {
        (640, 540)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Install => {
                let Some(path) = self.path.clone() else { return };
                if !matches!(*self.phase.lock(), Phase::Ready(_)) {
                    return;
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
            Msg::Open => {
                let phase = self.phase.lock().clone();
                if let Phase::Installed(id) = phase {
                    if let Err(e) = apps::launch(&id) {
                        *self.phase.lock() = Phase::Failed(e);
                    }
                }
            }
            Msg::Close => cx.close(),
            Msg::Select(i) => self.selected = Some(i),
            Msg::DeleteData(on) => self.delete_data = on,
            Msg::Uninstall => {
                if let Some(app) = self.selected.and_then(|i| self.installed.get(i)) {
                    match apps::uninstall(&app.manifest.id, self.delete_data) {
                        Ok(()) => {
                            self.installed = apps::list();
                            self.selected = None;
                        }
                        Err(e) => *self.phase.lock() = Phase::Failed(e),
                    }
                }
            }
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        self.path.as_ref().map(|_| 300)
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        let now = match *self.phase.lock() {
            Phase::Reading => (0, 0),
            Phase::Ready(_) => (1, 0),
            Phase::Installing(n, _) => (2, n),
            Phase::Installed(_) => (3, 0),
            Phase::Failed(_) => (4, 0),
        };
        core::mem::replace(&mut self.shown, now) != now
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        VStack::new()
            .push(Text::new("Installer").style(TextStyle::Title))
            .push(Text::new("Apps for WaveOS Aurora").foreground(Color::SecondaryLabel))
            .push(
                if self.path.is_some() { self.package() } else { self.manage() }
                    .padding_insets(Insets { top: 10, ..Default::default() }),
            )
            .push(Spacer::new())
            .spacing(4)
            .alignment(Align::Stretch)
            .padding(28)
            .fill()
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    waveui::run(Installer::new(args.get(1).cloned()))
}
