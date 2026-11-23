//! Software Update: WaveOS itself (see `corekit::update`). Check, read
//! what's new, download (pausable, resumable), install alongside the
//! running system, restart into it — and what happened if a new version
//! failed to start. Also: the automatic checks, and the repositories the
//! Store and Software Update use (none out of the box).

use super::{card, footer, page, pref_toggle, titled, Msg, Settings};
use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use corekit::apps::download::{Control, Outcome};
use corekit::sync::Mutex;
use corekit::update::{self, Available, Status};
use waveui::lumen::icons::Icon;
use waveui::prelude::*;
use waveui::style::Weight;

#[derive(Clone)]
pub enum UpdMsg {
    Check,
    Download,
    Pause,
    Resume,
    Cancel,
    Install,
    Restart,
    Discard,
    DismissFailure,
    NewSource(String),
    AddSource,
    RemoveSource(String),
}

#[derive(Clone, Default)]
pub enum Phase {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Box<Available>),
    Downloading(Box<Available>, u64, u64),
    Paused(Box<Available>, u64, u64),
    Downloaded(Box<Available>),
    Installing(Box<Available>, u64, u64),
    Failed(String),
}

#[derive(Default)]
pub struct UpdateState {
    shared: Arc<Mutex<Phase>>,
    control: Arc<Control>,
    status: Status,
    sources: Vec<String>,
    new_source: String,
    /// What the last frame showed, to redraw only on change.
    shown: (u8, u64),
}

impl UpdateState {
    pub fn refresh(&mut self) {
        self.status = update::status();
        self.sources = corekit::apps::repo::sources();
    }

    pub fn busy(&self) -> bool {
        matches!(*self.shared.lock(), Phase::Checking | Phase::Downloading(..) | Phase::Installing(..))
    }

    /// True when what the pane shows changed since the last call.
    pub fn changed(&mut self) -> bool {
        let now = match &*self.shared.lock() {
            Phase::Idle => (0, 0),
            Phase::Checking => (1, 0),
            Phase::UpToDate => (2, 0),
            Phase::Available(_) => (3, 0),
            Phase::Downloading(_, n, _) => (4, *n),
            Phase::Paused(_, n, _) => (5, *n),
            Phase::Downloaded(_) => (6, 0),
            Phase::Installing(_, n, _) => (7, *n),
            Phase::Failed(_) => (8, 0),
        };
        if now.0 == 2 || now.0 == 8 || now.0 == 6 {
            self.refresh();
        }
        core::mem::replace(&mut self.shown, now) != now
    }

    fn set(&self, p: Phase) {
        *self.shared.lock() = p;
    }

    fn spawn(&self, f: impl FnOnce(Arc<Mutex<Phase>>, Arc<Control>) + Send + 'static) {
        let (shared, control) = (self.shared.clone(), self.control.clone());
        if corekit::thread::spawn(move || f(shared, control)).is_err() {
            self.set(Phase::Failed(String::from("Couldn't start working on the update")));
        }
    }
}

fn human(b: u64) -> String {
    update::human(b)
}

impl Settings {
    pub(super) fn update_update(&mut self, m: UpdMsg, cx: &mut Context<Msg>) {
        let st = &mut self.update;
        match m {
            UpdMsg::Check => {
                st.set(Phase::Checking);
                st.spawn(|shared, _| {
                    let p = match update::check(None) {
                        Ok(Some(av)) if av.download_size() == 0 => Phase::Downloaded(Box::new(av)),
                        Ok(Some(av)) => Phase::Available(Box::new(av)),
                        Ok(None) => Phase::UpToDate,
                        Err(e) => Phase::Failed(e),
                    };
                    *shared.lock() = p;
                });
            }
            UpdMsg::Download | UpdMsg::Resume => {
                let av = match &*st.shared.lock() {
                    Phase::Available(a) | Phase::Paused(a, _, _) => a.clone(),
                    _ => return,
                };
                st.control.resume();
                st.set(Phase::Downloading(av.clone(), 0, av.download_size()));
                st.spawn(move |shared, control| {
                    let progress = |n, t| {
                        if let Phase::Downloading(_, a, b) = &mut *shared.lock() {
                            (*a, *b) = (n, t);
                        }
                    };
                    let p = match update::download(&av, &control, progress) {
                        Ok(Outcome::Done) => Phase::Downloaded(av),
                        Ok(Outcome::Paused) => {
                            let (n, t) = match &*shared.lock() {
                                Phase::Downloading(_, n, t) => (*n, *t),
                                _ => (0, 0),
                            };
                            Phase::Paused(av, n, t)
                        }
                        Ok(Outcome::Cancelled) => Phase::Available(av),
                        Err(e) => Phase::Failed(e),
                    };
                    *shared.lock() = p;
                });
            }
            UpdMsg::Pause => st.control.pause(),
            UpdMsg::Cancel => {
                st.control.cancel();
                let paused = match &*st.shared.lock() {
                    Phase::Paused(a, _, _) => Some(a.clone()),
                    _ => None,
                };
                if let Some(a) = paused {
                    st.set(Phase::Available(a));
                }
            }
            UpdMsg::Install => {
                let av = match &*st.shared.lock() {
                    Phase::Downloaded(a) => a.clone(),
                    _ => return,
                };
                st.set(Phase::Installing(av.clone(), 0, av.installed_size()));
                st.spawn(move |shared, _| {
                    let progress = |n, t| {
                        if let Phase::Installing(_, a, b) = &mut *shared.lock() {
                            (*a, *b) = (n, t);
                        }
                    };
                    *shared.lock() = match update::install(&av, progress) {
                        Ok(_) => Phase::Idle,
                        Err(e) => Phase::Failed(e),
                    };
                });
            }
            UpdMsg::Restart => match update::restart_to_update() {
                Ok(()) => {}
                Err(e) => self.flash(cx, e),
            },
            UpdMsg::Discard => {
                if let Err(e) = update::discard_ready() {
                    self.flash(cx, e);
                }
                self.update.refresh();
            }
            UpdMsg::DismissFailure => {
                if let Err(e) = update::dismiss_failure() {
                    self.flash(cx, e);
                }
                self.update.refresh();
            }
            UpdMsg::NewSource(s) => st.new_source = s,
            UpdMsg::AddSource => {
                let url = String::from(st.new_source.trim());
                match corekit::apps::repo::add_source(&url) {
                    Ok(()) => {
                        self.update.new_source.clear();
                        self.update.refresh();
                    }
                    Err(e) => self.flash(cx, e),
                }
            }
            UpdMsg::RemoveSource(url) => {
                if let Err(e) = corekit::apps::repo::remove_source(&url) {
                    self.flash(cx, e);
                }
                self.update.refresh();
            }
        }
    }

    pub(super) fn update_pane(&self) -> View<'_, Msg> {
        let st = &self.update;
        let s = &st.status;
        let phase = st.shared.lock().clone();
        let msg = |m: UpdMsg| Msg::Update(m);
        let headline = match (&phase, &s.ready) {
            (_, Some((_, v))) => format!("WaveOS {v} is ready to install"),
            (Phase::Checking, _) => String::from("Checking for updates…"),
            (Phase::UpToDate, _) => String::from("WaveOS is up to date"),
            (Phase::Available(a) | Phase::Paused(a, _, _) | Phase::Downloading(a, _, _), _) => {
                format!("WaveOS {} is available", a.update.version)
            }
            (Phase::Downloaded(a), _) => format!("WaveOS {} is downloaded", a.update.version),
            (Phase::Installing(a, _, _), _) => format!("Installing WaveOS {}…", a.update.version),
            (Phase::Failed(_), _) => String::from("Couldn't check for updates"),
            (Phase::Idle, _) => String::from("Software Update"),
        };
        let header = HStack::new()
            .push(Image::app_icon(Icon::Aurora, 56))
            .push(
                VStack::new()
                    .push(Text::new(headline).style(TextStyle::Sized(17, Weight::Bold)))
                    .push(
                        Text::new(format!(
                            "This computer has WaveOS {} (started from {}{})",
                            s.version,
                            match s.slot.as_str() {
                                "esp" => "its own installation",
                                "a" => "update slot A",
                                _ => "update slot B",
                            },
                            if s.trial { ", on trial" } else { "" }
                        ))
                        .style(TextStyle::Subheadline)
                        .foreground(Color::SecondaryLabel),
                    )
                    .spacing(3)
                    .alignment(Align::Start)
                    .grow(1),
            )
            .push_if(
                (!matches!(phase, Phase::Checking | Phase::Downloading(..) | Phase::Installing(..)) && s.ready.is_none())
                    .then(|| Button::new("Check Now").action(msg(UpdMsg::Check)).id("update/check")),
            )
            .spacing(14)
            .padding_xy(0, 8)
            .fill_width();
        let mut sections: Vec<View<'_, Msg>> = alloc::vec![card(alloc::vec![header])];
        if let Some(v) = &s.failed {
            sections.push(card(alloc::vec![HStack::new()
                .push(Image::symbol(Symbol::Warning).symbol_size(18).foreground(Color::Orange))
                .push(
                    Text::paragraph(format!(
                        "WaveOS {v} couldn't start, so this computer went back to the version before. Your files weren't touched. Try the update again later, or report it."
                    ))
                    .grow(1),
                )
                .push(Button::new("OK").action(msg(UpdMsg::DismissFailure)))
                .spacing(12)
                .padding_xy(0, 10)
                .fill_width()]));
        }
        if let Some((_, v)) = &s.ready {
            sections.push(card(alloc::vec![VStack::new()
                .push(Text::paragraph(format!(
                    "WaveOS {v} is installed alongside the running system. Restart to start using it: if it can't start, your computer goes back to WaveOS {} on its own.",
                    s.version
                )))
                .push(
                    HStack::new()
                        .push(Button::new("Remove Update").action(msg(UpdMsg::Discard)))
                        .push(Spacer::new())
                        .push(
                            Button::new("Restart Now")
                                .action(msg(UpdMsg::Restart))
                                .button_style(ButtonStyle::Prominent)
                                .control_size(ControlSize::Large)
                                .id("update/restart"),
                        )
                        .spacing(8)
                        .fill_width(),
                )
                .spacing(12)
                .alignment(Align::Stretch)
                .padding_xy(0, 10)
                .fill_width()]));
        }
        fn detail<'a>(a: &Available, body: View<'a, Msg>) -> View<'a, Msg> {
            let mut v = VStack::new()
                .push(
                    Text::new(format!(
                        "{} to download · {} installed",
                        human(a.download_size()),
                        human(a.installed_size())
                    ))
                    .style(TextStyle::Subheadline)
                    .foreground(Color::SecondaryLabel),
                )
                .spacing(10)
                .alignment(Align::Stretch)
                .padding_xy(0, 10)
                .fill_width();
            if !a.update.notes.is_empty() {
                v = v.push(Text::new("What's new").style(TextStyle::Headline)).push(Text::paragraph(a.update.notes.clone()));
            }
            card(alloc::vec![v.push(body)])
        }
        match &phase {
            Phase::Available(a) => sections.push(detail(
                a,
                HStack::new()
                    .push(Spacer::new())
                    .push(
                        Button::new("Download")
                            .action(msg(UpdMsg::Download))
                            .button_style(ButtonStyle::Prominent)
                            .id("update/download"),
                    )
                    .fill_width(),
            )),
            Phase::Downloading(a, n, t) | Phase::Paused(a, n, t) => {
                let paused = matches!(phase, Phase::Paused(..));
                sections.push(detail(
                    a,
                    VStack::new()
                        .push(ProgressView::new(Some(*n as f32 / (*t).max(1) as f32)).fill_width())
                        .push(
                            HStack::new()
                                .push(
                                    Text::new(format!(
                                        "{}{} of {}",
                                        if paused { "Paused: " } else { "" },
                                        human(*n),
                                        human(*t)
                                    ))
                                    .style(TextStyle::Subheadline)
                                    .foreground(Color::SecondaryLabel)
                                    .grow(1),
                                )
                                .push(if paused {
                                    Button::new("Resume").action(msg(UpdMsg::Resume))
                                } else {
                                    Button::new("Pause").action(msg(UpdMsg::Pause))
                                })
                                .push(Button::new("Cancel").action(msg(UpdMsg::Cancel)))
                                .spacing(8)
                                .fill_width(),
                        )
                        .spacing(8)
                        .alignment(Align::Stretch),
                ));
            }
            Phase::Downloaded(a) => sections.push(detail(
                a,
                HStack::new()
                    .push(
                        Text::paragraph("Installing puts the new system beside this one; nothing changes until you restart.")
                            .style(TextStyle::Subheadline)
                            .foreground(Color::SecondaryLabel)
                            .grow(1),
                    )
                    .push(
                        Button::new("Install")
                            .action(msg(UpdMsg::Install))
                            .button_style(ButtonStyle::Prominent)
                            .id("update/install"),
                    )
                    .spacing(12)
                    .fill_width(),
            )),
            Phase::Installing(a, n, t) => sections.push(detail(
                a,
                VStack::new()
                    .push(ProgressView::new(Some(*n as f32 / (*t).max(1) as f32)).fill_width())
                    .push(Text::new("Writing and checking every file…").style(TextStyle::Subheadline).foreground(Color::SecondaryLabel))
                    .spacing(8)
                    .alignment(Align::Stretch),
            )),
            Phase::Failed(e) => sections.push(card(alloc::vec![Text::paragraph(e.clone())
                .foreground(Color::Red)
                .padding_xy(0, 10)
                .fill_width()])),
            Phase::Checking => sections.push(card(alloc::vec![HStack::new()
                .push(ProgressView::spinner())
                .push(Text::new("Looking in your repositories…").foreground(Color::SecondaryLabel))
                .spacing(10)
                .padding_xy(0, 12)])),
            Phase::Idle | Phase::UpToDate => {}
        }
        sections.push(titled(
            "Automatic Updates",
            alloc::vec![
                pref_toggle("update/auto-check", "Check for updates automatically", "software_update_check"),
                pref_toggle("update/auto-download", "Download system updates automatically", "software_update_download"),
            ],
        ));
        sections.push(footer(
            "WaveOS installs an update beside the running system and starts it on the next restart. If it fails to start three times, the previous system starts instead. Your files are never part of an update.",
        ));
        let mut rows: Vec<View<'_, Msg>> = st
            .sources
            .iter()
            .map(|s| {
                HStack::new()
                    .push(Image::symbol(Symbol::Globe).symbol_size(14).foreground(Color::SecondaryLabel))
                    .push(Text::new(s.clone()).grow(1).lines(1))
                    .push(Button::new("Remove").action(msg(UpdMsg::RemoveSource(s.clone()))))
                    .spacing(10)
                    .min_height(size::ROW)
                    .fill_width()
            })
            .collect();
        if rows.is_empty() {
            rows.push(
                Text::new("No repositories. WaveOS comes without any.")
                    .foreground(Color::SecondaryLabel)
                    .min_height(size::ROW),
            );
        }
        rows.push(
            HStack::new()
                .push(
                    TextField::new("https://example.com/apps/", st.new_source.clone())
                        .on_change(|s: String| Msg::Update(UpdMsg::NewSource(s)))
                        .on_submit(msg(UpdMsg::AddSource))
                        .id("update/repository")
                        .grow(1),
                )
                .push(Button::new("Add").action(msg(UpdMsg::AddSource)))
                .spacing(8)
                .padding_xy(0, 8)
                .fill_width(),
        );
        sections.push(titled("Repositories", rows));
        sections.push(footer(
            "The Store and Software Update use these addresses. Everything they serve must be signed by a key this computer trusts; adding or removing one needs an administrator.",
        ));
        page("Software Update", sections)
    }
}
