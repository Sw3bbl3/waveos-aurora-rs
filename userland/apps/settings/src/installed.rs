//! Apps: the apps installed from packages, with their size, publisher and
//! permissions; keep one in the dock, or uninstall it (keeping or deleting
//! its data). Removing an app installed for everyone takes an
//! administrator, which `corekit::apps` asks for.

use super::{card, footer, page, titled, Msg, Settings};
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::apps::{self, Installed};
use waveui::prelude::*;
use waveui::Pixmap;

#[derive(Clone)]
pub enum AppMsg {
    Pin(String, bool),
    /// Ask to uninstall (Some), or stop asking (None).
    AskRemove(Option<String>),
    DeleteData(bool),
    Remove(String),
    Open(String),
}

#[derive(Default)]
pub struct AppsState {
    list: Vec<(Installed, Option<Rc<Pixmap>>)>,
    pinned: Vec<String>,
    removing: Option<String>,
    delete_data: bool,
}

impl AppsState {
    pub fn refresh(&mut self) {
        self.list = apps::list()
            .into_iter()
            .map(|a| {
                let icon = a.icon().and_then(|p| Pixmap::load(&p));
                (a, icon)
            })
            .collect();
        self.pinned = apps::pinned();
    }
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{}.{} MB", b >> 20, ((b % (1 << 20)) * 10) >> 20),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

fn icon<'a>(p: &Option<Rc<Pixmap>>) -> View<'a, Msg> {
    match p {
        Some(p) => Image::new(p.clone()).size(40, 40),
        None => Image::app_icon(waveui::lumen::icons::Icon::Aurora, 40),
    }
}

impl Settings {
    pub(super) fn apps_pane(&self) -> View<'_, Msg> {
        let st = &self.apps;
        let mut rows: Vec<View<'_, Msg>> = Vec::new();
        if st.list.is_empty() {
            rows.push(
                Text::paragraph(
                    "No apps are installed from packages yet. Open a .gina file in Files, or use pkg in Terminal.",
                )
                .foreground(Color::SecondaryLabel)
                .padding_xy(0, 14),
            );
        }
        for (a, pic) in &st.list {
            let m = &a.manifest;
            let id = m.id.clone();
            let publisher = if a.receipt.publisher.is_empty() {
                if a.receipt.signer.is_empty() {
                    String::from("Not signed")
                } else {
                    String::from("Untrusted signature")
                }
            } else {
                format!("Signed by {}", a.receipt.publisher)
            };
            let detail = format!(
                "{} · {} · {}{}",
                m.version,
                human(a.receipt.size),
                if a.scope == apps::Scope::System { "Everyone" } else { "Only you" },
                if m.is_game() { " · Game" } else { "" }
            );
            let mut row = HStack::new()
                .push(icon(pic))
                .push(
                    VStack::new()
                        .push(Text::new(m.name.clone()))
                        .push(Text::new(detail).style(TextStyle::Subheadline).foreground(Color::SecondaryLabel))
                        .push(Text::new(publisher).style(TextStyle::Subheadline).foreground(Color::SecondaryLabel))
                        .spacing(2)
                        .alignment(Align::Start)
                        .grow(1),
                )
                .push(Button::new("Open").action(Msg::Apps(AppMsg::Open(id.clone()))))
                .push({
                    let id = id.clone();
                    Toggle::new("In Dock", st.pinned.contains(&id))
                        .on_change(move |on| Msg::Apps(AppMsg::Pin(id.clone(), on)))
                })
                .spacing(12)
                .min_height(56)
                .fill_width();
            row = if st.removing.as_deref() == Some(id.as_str()) {
                row.push(Button::new("Keep").action(Msg::Apps(AppMsg::AskRemove(None)))).push(
                    Button::new("Uninstall")
                        .action(Msg::Apps(AppMsg::Remove(id.clone())))
                        .button_style(ButtonStyle::Destructive),
                )
            } else {
                row.push(Button::new("Uninstall…").action(Msg::Apps(AppMsg::AskRemove(Some(id.clone())))))
            };
            rows.push(row);
            if st.removing.as_deref() == Some(id.as_str()) {
                let mut perms = String::new();
                if !m.permissions.is_empty() {
                    let list: Vec<&str> = m.permissions.iter().map(|p| gina::describe_permission(p)).collect();
                    perms = format!("It was allowed to: {}.", list.join("; "));
                }
                rows.push(
                    HStack::new()
                        .push(
                            Toggle::checkbox(format!("Also delete its data ({})", a.data_dir()), st.delete_data)
                                .on_change(|on| Msg::Apps(AppMsg::DeleteData(on))),
                        )
                        .push(Spacer::new())
                        .push(Text::new(perms).style(TextStyle::Subheadline).foreground(Color::SecondaryLabel))
                        .spacing(12)
                        .padding_xy(52, 6)
                        .fill_width(),
                );
            }
        }
        let total: u64 = st.list.iter().map(|(a, _)| a.receipt.size).sum();
        let sections = alloc::vec![
            titled(&format!("Installed apps · {}", human(total)), rows).id("apps/list"),
            footer(
                "Apps installed for everyone live in /Applications and only administrators can change them; \
                 apps just for you live in your Library. Uninstalling keeps an app's data (in \
                 Library/Containers) unless you choose to delete it.",
            ),
            card(alloc::vec![HStack::new()
                .push(Text::new("Find and update apps").grow(1))
                .push(Text::new("pkg search, pkg update in Terminal").foreground(Color::SecondaryLabel))
                .min_height(44)
                .fill_width()]),
        ];
        page("Apps", sections)
    }

    pub(super) fn apps_update(&mut self, msg: AppMsg, cx: &mut Context<Msg>) {
        match msg {
            AppMsg::Pin(id, on) => {
                if let Err(e) = apps::set_pinned(&id, on) {
                    self.flash(cx, format!("Couldn't change the dock: {e}"));
                }
            }
            AppMsg::AskRemove(id) => {
                self.apps.removing = id;
                self.apps.delete_data = false;
            }
            AppMsg::DeleteData(on) => self.apps.delete_data = on,
            AppMsg::Remove(id) => {
                self.apps.removing = None;
                let name = self.apps.list.iter().find(|(a, _)| a.manifest.id == id).map(|(a, _)| a.manifest.name.clone());
                match apps::uninstall(&id, self.apps.delete_data) {
                    Ok(()) => self.flash(cx, format!("Uninstalled {}", name.unwrap_or(id))),
                    Err(e) => self.flash(cx, format!("Couldn't uninstall: {e}")),
                }
            }
            AppMsg::Open(id) => {
                if let Err(e) = apps::launch(&id) {
                    self.flash(cx, format!("Couldn't open it: {e}"));
                }
            }
        }
        self.apps.refresh();
    }
}
