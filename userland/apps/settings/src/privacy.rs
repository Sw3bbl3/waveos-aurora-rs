//! Privacy & Security: what every app may do. Each capability lists the
//! apps that declare it, with a switch; switching one off takes effect at
//! once, also for apps that are running (a recording stops, a folder can
//! no longer be opened). Guarded ones (microphone, camera, screen, input,
//! folders) are off until the person allows them, when the app first
//! asks. Below: the files apps keep bookmarks to, and what was refused
//! lately.

use super::{card, footer, page, titled, Msg, Settings};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::sandbox::{self, AppPrivacy, Bookmark};
use waveui::prelude::*;

#[derive(Clone)]
pub enum PrivacyMsg {
    /// (app, permission, on)
    Set(String, &'static str, bool),
    /// Forget the decision: the app asks again.
    Reset(String, &'static str),
    Forget(String, String),
}

#[derive(Default)]
pub struct PrivacyState {
    apps: Vec<AppPrivacy>,
    bookmarks: Vec<Bookmark>,
    denied: Vec<(String, String)>,
    error: Option<String>,
}

impl PrivacyState {
    pub fn refresh(&mut self) {
        match sandbox::privacy() {
            Ok(p) => {
                self.apps = p.apps;
                self.bookmarks = p.bookmarks;
                self.denied = p.denied;
                self.error = None;
            }
            Err(e) => self.error = Some(format!("{e}")),
        }
    }
}

/// The capabilities Settings shows, in order: (permission, also counts,
/// guarded). A `:read` folder permission shows with its folder.
const SHOWN: [(&str, &str, bool); 24] = [
    ("microphone", "", true),
    ("camera", "", true),
    ("screen-capture", "", true),
    ("input-monitoring", "", true),
    ("usb", "", true),
    ("documents", "documents:read", true),
    ("downloads", "downloads:read", true),
    ("pictures", "pictures:read", true),
    ("music", "music:read", true),
    ("movies", "movies:read", true),
    ("desktop", "desktop:read", true),
    ("home", "", true),
    ("network", "", false),
    ("network-server", "", false),
    ("notifications", "", false),
    ("background", "", false),
    ("gamepad", "", false),
    ("clipboard", "", false),
    ("launch", "", false),
    ("spawn", "", false),
    ("linux", "", false),
    ("ipc", "", false),
    ("preferences", "", false),
    ("elevate", "", false),
];

fn has(list: &[String], p: &str) -> bool {
    list.iter().any(|x| x == p)
}

/// The permission app `a` declares for a shown row (the folder or its `:read`).
fn declared<'a>(a: &AppPrivacy, perm: &'a str, also: &'a str) -> Option<&'a str> {
    if has(&a.declared, perm) {
        Some(perm)
    } else if !also.is_empty() && has(&a.declared, also) {
        Some(also)
    } else {
        None
    }
}

/// Whether `perm` is on for `a`, and whether the person decided it.
fn state(a: &AppPrivacy, perm: &str, guarded: bool) -> (bool, bool) {
    if has(&a.denied, perm) {
        return (false, true);
    }
    if has(&a.allowed, perm) {
        return (true, true);
    }
    // The system's own apps have the folders they declare.
    let folder = perm != "microphone" && perm != "camera" && perm != "screen-capture" && perm != "input-monitoring";
    (!guarded || (a.kind == "system" && folder && perm != "usb"), false)
}

impl Settings {
    pub(super) fn privacy_pane(&self) -> View<'_, Msg> {
        let st = &self.privacy;
        let mut sections: Vec<View<'_, Msg>> = Vec::new();
        if let Some(e) = &st.error {
            sections.push(footer(&format!("Couldn't read the privacy settings: {e}")));
        }
        for (perm, also, guarded) in SHOWN {
            let mut rows: Vec<View<'_, Msg>> = Vec::new();
            for a in &st.apps {
                let Some(p) = declared(a, perm, also) else { continue };
                // Programs without a manifest may do everything; they are
                // listed where the person answered for them.
                if a.kind == "legacy" && !has(&a.allowed, p) && !has(&a.denied, p) {
                    continue;
                }
                let (on, decided) = state(a, p, guarded);
                let note = match (guarded, decided, on) {
                    (true, false, false) => "Asks first",
                    (_, true, true) => "Allowed",
                    (_, true, false) => "Not allowed",
                    _ => "",
                };
                let id = a.id.clone();
                let mut row = HStack::new()
                    .push(
                        VStack::new()
                            .push(Text::new(a.name.clone()))
                            .push(
                                Text::new(format!("{}{}{}", a.id, if note.is_empty() { "" } else { " · " }, note))
                                    .style(TextStyle::Subheadline)
                                    .foreground(Color::SecondaryLabel),
                            )
                            .spacing(2)
                            .alignment(Align::Start)
                            .grow(1),
                    )
                    .spacing(12)
                    .min_height(48)
                    .fill_width();
                if decided {
                    let rid = id.clone();
                    row = row.push(Button::new("Reset").action(Msg::Privacy(PrivacyMsg::Reset(rid, p_static(p)))));
                }
                let pid = id.clone();
                row = row.push(
                    Toggle::new("", on).on_change(move |v| Msg::Privacy(PrivacyMsg::Set(pid.clone(), p_static(p), v))),
                );
                rows.push(row);
            }
            if !rows.is_empty() {
                let section = titled(sandbox::describe(perm), rows);
                sections.push(match id_of(perm) {
                    Some(id) => section.id(id),
                    None => section,
                });
            }
        }
        if st.apps.is_empty() && st.error.is_none() {
            sections.push(footer("No app has asked for anything yet."));
        }
        // Files apps were given and keep.
        let mut marks: Vec<View<'_, Msg>> = Vec::new();
        for b in &st.bookmarks {
            let name = st.apps.iter().find(|a| a.id == b.app).map(|a| a.name.clone()).unwrap_or_else(|| b.app.clone());
            marks.push(
                HStack::new()
                    .push(
                        VStack::new()
                            .push(Text::new(b.path.clone()))
                            .push(
                                Text::new(format!("{name} · {}", if b.write { "read and change" } else { "read" }))
                                    .style(TextStyle::Subheadline)
                                    .foreground(Color::SecondaryLabel),
                            )
                            .spacing(2)
                            .alignment(Align::Start)
                            .grow(1),
                    )
                    .push(
                        Button::new("Forget")
                            .action(Msg::Privacy(PrivacyMsg::Forget(b.app.clone(), b.token.clone()))),
                    )
                    .spacing(12)
                    .min_height(48)
                    .fill_width(),
            );
        }
        if !marks.is_empty() {
            sections.push(titled("Files and folders apps keep", marks).id("privacy/bookmarks"));
        }
        if !st.denied.is_empty() {
            let rows: Vec<View<'_, Msg>> = st
                .denied
                .iter()
                .rev()
                .take(12)
                .map(|(app, what)| {
                    HStack::new()
                        .push(Text::new(app.clone()).grow(1))
                        .push(Text::new(what.clone()).foreground(Color::SecondaryLabel))
                        .spacing(12)
                        .min_height(36)
                        .fill_width()
                })
                .collect();
            sections.push(titled("Recently refused", rows).id("privacy/refused"));
        }
        sections.push(footer(
            "Apps run in a sandbox: each may use its own folder and what you see here. Files you open with an \
             app, drop on it, paste into it or pick in the open and save panel are always theirs to use. \
             Switching something off takes effect at once.",
        ));
        sections.push(card(alloc::vec![HStack::new()
            .push(Text::new("Microphone in use").grow(1))
            .push(Text::new("an orange dot in the top bar").foreground(Color::SecondaryLabel))
            .min_height(44)
            .fill_width()]));
        page("Privacy & Security", sections)
    }

    pub(super) fn privacy_update(&mut self, msg: PrivacyMsg, cx: &mut Context<Msg>) {
        let r = match msg {
            PrivacyMsg::Set(id, perm, on) => sandbox::set_permission(&id, perm, Some(on)),
            PrivacyMsg::Reset(id, perm) => sandbox::set_permission(&id, perm, None),
            PrivacyMsg::Forget(id, token) => sandbox::forget_bookmark(&id, &token),
        };
        if let Err(e) = r {
            self.flash(cx, format!("Couldn't change it: {e}"));
        }
        self.privacy.refresh();
    }
}

/// The permission names, kept for messages.
fn p_static(p: &str) -> &'static str {
    corekit::abi::sandbox::NAMES.iter().find(|(n, _)| *n == p).map(|(n, _)| *n).unwrap_or("")
}

/// The deep link of a section (see `search`).
fn id_of(perm: &str) -> Option<&'static str> {
    Some(match perm {
        "microphone" => "privacy/microphone",
        "camera" => "privacy/camera",
        "documents" => "privacy/documents",
        "network" => "privacy/network",
        _ => return None,
    })
}
