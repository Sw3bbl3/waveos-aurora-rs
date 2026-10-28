//! Installer — installs `.gina` packages and removes installed apps.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use corekit::{apps, fs, sync::Mutex, thread};
use waveui::lumen::icons::Icon;
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

    /// Where the install has got to: 0 reading, 1 review, 2 installing, 3 done.
    fn step(&self) -> (usize, bool) {
        match &*self.phase.lock() {
            Phase::Reading => (0, false),
            Phase::Ready(_) => (1, false),
            Phase::Installing(..) => (2, false),
            Phase::Installed(_) => (4, false),
            Phase::Failed(_) => (if self.path.is_some() { 1 } else { 0 }, true),
        }
    }

    fn sidebar(&self) -> View<'_, Msg> {
        let (at, failed) = self.step();
        let steps: &[&str] =
            if self.path.is_some() { &["Read the package", "Review", "Install", "Ready"] } else { &["Installed apps"] };
        let mut list = VStack::new().spacing(4).alignment(Align::Stretch);
        for (i, name) in steps.iter().enumerate() {
            let (done, current) = if self.path.is_some() { (i < at, i == at) } else { (false, true) };
            let bad = current && failed;
            let badge = Canvas::new(move |cv, r| {
                let (x, y) = r.center();
                let t = waveui::lumen::theme::current();
                let fill = if bad {
                    0xFFFF_5A5F
                } else if done {
                    0xFF34_D399
                } else if current {
                    waveui::lumen::theme::accent()
                } else {
                    waveui::lumen::canvas::with_alpha(t.text_secondary, 0x40)
                };
                cv.fill_circle(x, y, 11, fill);
                if done {
                    cv.polyline(&[(x - 5, y), (x - 1, y + 4), (x + 5, y - 3)], 2, 0xFFFF_FFFF);
                } else {
                    let f = waveui::lumen::theme::ui_bold(11);
                    cv.text_centered(r, &format!("{}", i + 1), f, if current { 0xFFFF_FFFF } else { t.text });
                }
            })
            .size(24, 24);
            let row = HStack::new()
                .push(badge)
                .push(
                    Text::new(*name)
                        .style(TextStyle::Sized(14, waveui::style::Weight::Bold))
                        .foreground(if current || done { Color::Label } else { Color::SecondaryLabel }),
                )
                .spacing(12)
                .padding_xy(12, 0)
                .height(44)
                .fill_width();
            list = list.push(if current { row.background_rounded(Color::Fill, 12) } else { row });
        }
        VStack::new()
            .push(Image::app_icon(Icon::Installer, 40))
            .push(
                VStack::new()
                    .push(Text::new("Installer").style(TextStyle::Sized(15, waveui::style::Weight::Bold)))
                    .push(
                        Text::new("Apps for WaveOS Aurora")
                            .style(TextStyle::Sized(12, waveui::style::Weight::Regular))
                            .foreground(Color::SecondaryLabel),
                    )
                    .spacing(2)
                    .alignment(Align::Start)
                    .padding_insets(Insets { top: 12, leading: 0, bottom: 14, trailing: 0 }),
            )
            .push(list)
            .push(Spacer::new())
            .alignment(Align::Start)
            .padding_insets(Insets { top: 24, leading: 16, bottom: 16, trailing: 16 })
            .fill()
    }

    /// A card for an app: its square, name and detail, and a note on the right.
    fn app_card<'a>(name: String, detail: String, note: Option<(&'a str, Color)>, selected: bool) -> View<'a, Msg> {
        let mut row = HStack::new()
            .push(
                Image::symbol(Symbol::Download)
                    .symbol_size(20)
                    .foreground(Color::Custom(0xFFFF_FFFF))
                    .size(48, 48)
                    .background_rounded(Color::Accent, 12),
            )
            .push(
                VStack::new()
                    .push(Text::new(name).style(TextStyle::Sized(15, waveui::style::Weight::Bold)))
                    .push(
                        Text::new(detail)
                            .style(TextStyle::Sized(12, waveui::style::Weight::Regular))
                            .foreground(Color::SecondaryLabel),
                    )
                    .spacing(3)
                    .alignment(Align::Start)
                    .grow(1),
            )
            .spacing(14)
            .padding(16)
            .background_rounded(Color::Fill, 16)
            .fill_width();
        if let Some((text, color)) = note {
            row = row.push(Text::new(text).style(TextStyle::Sized(12, waveui::style::Weight::Bold)).foreground(color));
        }
        if selected {
            row.border(Color::Accent, 16)
        } else {
            row.border(Color::Separator, 16)
        }
    }

    fn package(&self) -> View<'_, Msg> {
        let note = HStack::new()
            .push(Image::symbol(Symbol::Lock).symbol_size(13).foreground(Color::SecondaryLabel))
            .push(
                Text::new(
                    "Packages are checked before anything is written. Each app keeps its data in its own folder.",
                )
                .style(TextStyle::Callout)
                .foreground(Color::SecondaryLabel),
            )
            .spacing(10);
        let (title, subtitle, body, buttons): (String, String, View<'_, Msg>, Vec<View<'_, Msg>>) =
            match self.phase.lock().clone() {
                Phase::Reading => (
                    String::from("Reading the package"),
                    String::from("Checking that it is whole and made for this system."),
                    HStack::new().push(ProgressView::spinner()).push(Text::new("One moment…")).spacing(10),
                    vec![Button::new("Cancel").action(Msg::Close).shortcut(Shortcut::cancel())],
                ),
                Phase::Ready(m) => {
                    let update = apps::installed(&m.id).is_ok();
                    (
                        format!("{} “{}”?", if update { "Update" } else { "Install" }, m.name),
                        String::from(if update {
                            "A newer version of an app you have. Its data is kept."
                        } else {
                            "It goes into Applications and shows up in Shoal and Seek."
                        }),
                        Self::app_card(
                            format!("{} · {}", m.name, m.version),
                            format!("{} · {}", m.developer, m.id),
                            Some(if update { ("Update", Color::AccentText) } else { ("New", Color::AccentText) }),
                            true,
                        ),
                        vec![
                            Button::new("Cancel").action(Msg::Close).shortcut(Shortcut::cancel()),
                            Button::new(if update { "Update" } else { "Install" })
                                .action(Msg::Install)
                                .button_style(ButtonStyle::Prominent)
                                .shortcut(Shortcut::default_action()),
                        ],
                    )
                }
                Phase::Installing(n, total) => (
                    String::from("Installing…"),
                    format!("Writing files: {n} of {total}"),
                    ProgressView::new(Some(n as f32 / total.max(1) as f32)).fill_width(),
                    Vec::new(),
                ),
                Phase::Installed(_) => (
                    String::from("Ready to go"),
                    String::from("Your app is in Shoal and in Seek."),
                    Spacer::new().height(1),
                    vec![
                        Button::new("Done").action(Msg::Close),
                        Button::new("Open App")
                            .action(Msg::Open)
                            .button_style(ButtonStyle::Prominent)
                            .shortcut(Shortcut::default_action()),
                    ],
                ),
                Phase::Failed(e) => (
                    String::from("Couldn't install"),
                    e,
                    Spacer::new().height(1),
                    vec![Button::new("Close").action(Msg::Close).shortcut(Shortcut::default_action())],
                ),
            };
        let buttons: Vec<View<'_, Msg>> =
            buttons.into_iter().map(|b| b.control_size(ControlSize::Large).min_width(96)).collect();
        VStack::new()
            .push(Text::new(title).style(TextStyle::Sized(30, waveui::style::Weight::Display)))
            .push(Text::paragraph(subtitle).style(TextStyle::Callout).foreground(Color::SecondaryLabel))
            .push(body.padding_insets(Insets { top: 10, leading: 0, bottom: 4, trailing: 0 }))
            .push(note)
            .push(Spacer::new())
            .push(HStack::new().push(Spacer::new()).extend(buttons).spacing(10).fill_width())
            .spacing(8)
            .alignment(Align::Stretch)
            .fill()
    }

    fn manage(&self) -> View<'_, Msg> {
        let mut list = VStack::new().spacing(10).alignment(Align::Stretch);
        if self.installed.is_empty() {
            list = list.push(
                Text::centered(
                    "No apps from other developers are installed yet. Open a .gina package in Files to install one.",
                )
                .foreground(Color::SecondaryLabel)
                .padding(28)
                .background_rounded(Color::Fill, 16)
                .border(Color::Separator, 16)
                .fill_width(),
            );
        }
        for (i, a) in self.installed.iter().enumerate() {
            list = list.push(
                Self::app_card(
                    format!("{} · {}", a.manifest.name, a.manifest.version),
                    format!("{} · {}", a.manifest.developer, a.manifest.id),
                    None,
                    self.selected == Some(i),
                )
                .on_tap(Msg::Select(i)),
            );
        }
        let mut v = VStack::new()
            .push(Text::new("Installed apps").style(TextStyle::Sized(30, waveui::style::Weight::Display)))
            .push(
                Text::new("Apps you added from .gina packages. Choose one to remove it.")
                    .style(TextStyle::Callout)
                    .foreground(Color::SecondaryLabel),
            )
            .push(ScrollView::new(list.padding_insets(Insets { top: 10, leading: 0, bottom: 4, trailing: 0 })).grow(1))
            .spacing(8)
            .alignment(Align::Stretch);
        if let Phase::Failed(e) = self.phase.lock().clone() {
            v = v.push(Text::paragraph(e).foreground(Color::Red));
        }
        v.push(
            HStack::new()
                .push(Toggle::checkbox("Also delete the app's saved data", self.delete_data).on_change(Msg::DeleteData))
                .push(Spacer::new())
                .push(
                    Button::new("Uninstall…")
                        .action(Msg::Uninstall)
                        .button_style(ButtonStyle::Destructive)
                        .control_size(ControlSize::Large)
                        .disabled(self.selected.is_none()),
                )
                .fill_width(),
        )
        .fill()
    }
}

impl App for Installer {
    type Message = Msg;

    fn title(&self) -> String {
        "Installer".into()
    }
    fn size(&self) -> (i32, i32) {
        (940, 620)
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
        let content = if self.path.is_some() { self.package() } else { self.manage() };
        SplitView::new(
            self.sidebar(),
            content.padding_insets(Insets { top: 40, leading: 40, bottom: 32, trailing: 40 }),
        )
        .sidebar_width(250)
    }
}

corekit::entry!(main);

fn main(args: corekit::Args) -> i32 {
    waveui::run(Installer::new(args.get(1).cloned()))
}
