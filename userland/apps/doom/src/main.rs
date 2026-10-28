//! Doom — starts the game (doomgeneric, a Linux program in
//! `/System/Games/Doom`) with a game data file from the Doom folder in
//! Documents, and fetches Freedoom, the free game data, when there is none.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::sync::Arc;
use corekit::{fs, sync::Mutex, thread};
use nebula_web::Url;
use waveui::lumen::icons::Icon;
use waveui::prelude::*;

/// The game itself.
const GAME: &str = "/System/Games/Doom/doom";
/// Where game data lives: Freedoom goes here, and so can your own WAD files.
const FOLDER: &str = "/Documents/Doom";
/// Freedoom 0.13.0 (BSD-3-Clause): both phases and their license.
const FREEDOOM: &str = "https://github.com/freedoom/freedoom/releases/download/v0.13.0/freedoom-0.13.0.zip";
/// The largest file taken out of the archive.
const MAX_WAD: usize = 64 << 20;

/// Game data files the game can start with, and what to call them.
const KNOWN: &[(&str, &str)] = &[
    ("doom.wad", "The Ultimate Doom"),
    ("doom1.wad", "Doom (Shareware)"),
    ("doom2.wad", "Doom II: Hell on Earth"),
    ("plutonia.wad", "Final Doom: The Plutonia Experiment"),
    ("tnt.wad", "Final Doom: TNT: Evilution"),
    ("freedoom1.wad", "Freedoom: Phase 1"),
    ("freedoom2.wad", "Freedoom: Phase 2"),
    ("freedm.wad", "FreeDM"),
];

/// A game found in the Doom folder.
struct Game {
    title: &'static str,
    path: String,
}

fn find_games() -> Vec<Game> {
    let files = fs::read_dir(FOLDER).unwrap_or_default();
    KNOWN
        .iter()
        .filter_map(|(file, title)| {
            let e = files.iter().find(|e| e.name.eq_ignore_ascii_case(file))?;
            Some(Game { title, path: fs::join(FOLDER, &e.name) })
        })
        .collect()
}

#[derive(Clone, PartialEq)]
enum Fetch {
    Idle,
    Downloading,
    Unpacking,
    Failed(String),
    Done,
}

/// Downloads Freedoom and puts its WAD files and license in the Doom folder.
fn get_freedoom(state: &Mutex<Fetch>) -> Result<(), String> {
    let url = Url::parse(FREEDOOM).ok_or("bad address")?;
    let r = corekit::web::fetch(&url).map_err(|e| format!("The download failed ({e:?})."))?;
    if r.status != 200 {
        return Err(format!("The server answered {} {}.", r.status, r.reason));
    }
    *state.lock() = Fetch::Unpacking;
    let zip = aurora_zip::Archive::new(&r.body).map_err(|e| format!("The download can't be opened: {e}."))?;
    let _ = fs::mkdir(FOLDER);
    for (inside, name) in [
        ("freedoom1.wad", "freedoom1.wad"),
        ("freedoom2.wad", "freedoom2.wad"),
        ("COPYING.txt", "Freedoom License.txt"),
    ] {
        let entry = zip.find(inside).ok_or(format!("{inside} is missing from the download."))?;
        let data = zip.read(entry, MAX_WAD).map_err(|e| format!("Can't unpack {inside}: {e}."))?;
        // Written under a temporary name first, so a half-written file is never mistaken for a game.
        let part = fs::join(FOLDER, &format!("{name}.part"));
        fs::write(&part, &data).map_err(|e| format!("Can't save {name}: {e}"))?;
        fs::rename(&part, &fs::join(FOLDER, name)).map_err(|e| format!("Can't save {name}: {e}"))?;
    }
    fs::sync();
    Ok(())
}

/// A row of a card: a title, a smaller line under it, and a button.
fn game_row<'a>(title: &str, detail: &str, button: View<'a, Msg>) -> View<'a, Msg> {
    HStack::new()
        .push(
            VStack::new()
                .push(Text::new(title))
                .push(Text::new(detail).style(TextStyle::Subheadline).foreground(Color::SecondaryLabel))
                .spacing(2)
                .alignment(Align::Start)
                .grow(1),
        )
        .push(button)
        .spacing(12)
        .alignment(Align::Center)
        .min_height(52)
        .fill_width()
}

struct Doom {
    games: Vec<Game>,
    fetch: Arc<Mutex<Fetch>>,
    shown: Fetch,
    error: Option<String>,
}

#[derive(Clone)]
enum Msg {
    Play(usize),
    Download,
    OpenFolder,
}

impl Doom {
    fn new() -> Self {
        Self { games: find_games(), fetch: Arc::new(Mutex::new(Fetch::Idle)), shown: Fetch::Idle, error: None }
    }

    fn games_card(&self) -> View<'_, Msg> {
        let mut card = Section::new();
        for (i, g) in self.games.iter().enumerate() {
            card = card.push(game_row(
                g.title,
                fs::file_name(&g.path),
                Button::new("Play").action(Msg::Play(i)).button_style(ButtonStyle::Prominent),
            ));
        }
        Section::titled("Games", card)
    }

    fn freedoom_card(&self) -> View<'_, Msg> {
        let accessory = match &self.shown {
            Fetch::Downloading => HStack::new()
                .push(ProgressView::spinner())
                .push(Text::new("Downloading…").foreground(Color::SecondaryLabel))
                .spacing(8),
            Fetch::Unpacking => HStack::new()
                .push(ProgressView::spinner())
                .push(Text::new("Unpacking…").foreground(Color::SecondaryLabel))
                .spacing(8),
            _ => Button::new("Download").action(Msg::Download).button_style(if self.games.is_empty() {
                ButtonStyle::Prominent
            } else {
                ButtonStyle::Bordered
            }),
        };
        let mut card =
            Section::new().push(game_row("Freedoom", "Free game data, both phases · 24 MB · BSD license", accessory));
        if let Fetch::Failed(e) = &self.shown {
            card = card.push(Text::new(e.clone()).foreground(Color::Red).padding_xy(0, 10));
        }
        Section::titled("Get game data", card)
    }
}

impl App for Doom {
    type Message = Msg;

    fn title(&self) -> String {
        "Doom".into()
    }
    fn size(&self) -> (i32, i32) {
        // Shorter when there is nothing to download.
        (560, if self.games.iter().any(|g| g.path.contains("freedoom")) { 380 } else { 440 })
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Play(i) => {
                let Some(g) = self.games.get(i) else { return };
                match corekit::process::spawn(GAME, &["-iwad", &g.path], corekit::process::Stdio::default()) {
                    Ok(_) => cx.close(),
                    Err(e) => self.error = Some(format!("Doom couldn't start: {e}")),
                }
            }
            Msg::Download => {
                *self.fetch.lock() = Fetch::Downloading;
                let state = self.fetch.clone();
                let started = thread::spawn(move || {
                    let result = get_freedoom(&state);
                    *state.lock() = match result {
                        Ok(()) => Fetch::Done,
                        Err(e) => Fetch::Failed(e),
                    };
                });
                if let Err(e) = started {
                    *self.fetch.lock() = Fetch::Failed(format!("Can't start the download: {e}"));
                }
                self.shown = self.fetch.lock().clone();
            }
            Msg::OpenFolder => {
                let _ = fs::mkdir(FOLDER);
                cx.open_file(FOLDER);
            }
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(if matches!(self.shown, Fetch::Downloading | Fetch::Unpacking) { 250 } else { 2000 })
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        let now = self.fetch.lock().clone();
        let mut changed = now != self.shown;
        if changed {
            self.shown = now;
        }
        // Files added to the folder (or a finished download) show up here.
        let games = find_games();
        if games.len() != self.games.len() || games.iter().zip(&self.games).any(|(a, b)| a.path != b.path) {
            self.games = games;
            changed = true;
        }
        changed
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        let header = HStack::new()
            .push(Image::app_icon(Icon::Doom, 72))
            .push(
                VStack::new()
                    .push(Text::new("Doom").style(TextStyle::Sized(30, waveui::style::Weight::Display)))
                    .push(
                        Text::new("The 1993 classic, as a Linux program on WaveOS.").foreground(Color::SecondaryLabel),
                    )
                    .spacing(2)
                    .alignment(Align::Start)
                    .grow(1),
            )
            .spacing(18)
            .alignment(Align::Center)
            .fill_width();
        let mut page = VStack::new().push(header).spacing(20).alignment(Align::Stretch);
        if self.games.is_empty() {
            page = page.push(
                Text::paragraph(
                    "Doom needs game data to play. Download Freedoom below, or put DOOM.WAD, DOOM2.WAD or another \
                     game file in the Doom folder in Documents.",
                )
                .foreground(Color::SecondaryLabel),
            );
        } else {
            page = page.push(self.games_card());
        }
        let busy = matches!(self.shown, Fetch::Downloading | Fetch::Unpacking | Fetch::Failed(_));
        if busy || !self.games.iter().any(|g| g.path.contains("freedoom")) {
            page = page.push(self.freedoom_card());
        }
        if let Some(e) = &self.error {
            page = page.push(Text::paragraph(e.clone()).foreground(Color::Red));
        }
        page.push(Spacer::new())
            .push(
                HStack::new()
                    .push(
                        Text::paragraph("Arrows move · Ctrl fires · Space opens · Esc for the menu")
                            .style(TextStyle::Footnote)
                            .foreground(Color::TertiaryLabel)
                            .grow(1),
                    )
                    .push(Button::new("Open Doom Folder").action(Msg::OpenFolder).control_size(ControlSize::Small))
                    .spacing(12)
                    .alignment(Align::Center),
            )
            .padding_insets(Insets { top: 28, leading: 32, bottom: 22, trailing: 32 })
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Doom::new())
}
