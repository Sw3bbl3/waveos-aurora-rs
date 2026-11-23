//! Users & Login: your account (name, picture, password), the other
//! accounts (add, remove, make administrator) and how the computer starts.
//! Changes that take an administrator go through the kernel, which shows
//! the system prompt when this process doesn't hold the capability.

use super::{card, footer, page, pref_toggle, titled, Msg, Settings};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use corekit::abi::pref;
use corekit::users::{self, User};
use waveui::lumen::theme;
use waveui::prelude::*;

/// Gradients for account pictures (as Shore draws them).
pub const TONES: [(u32, u32); 6] = [
    (0xFF4A_63FF, 0xFF8D_2CF0),
    (0xFF12_B5A8, 0xFF2F_6BFF),
    (0xFFFF_8A4C, 0xFFE0_3E7A),
    (0xFF3D_DC97, 0xFF14_8F77),
    (0xFFFF_C14D, 0xFFFF_6B3D),
    (0xFF7A_8BA8, 0xFF39_4560),
];

#[derive(Clone)]
pub enum UserMsg {
    /// Start or stop changing your password.
    EditPassword(bool),
    Field(usize, String),
    SavePassword,
    /// The next picture gradient.
    NextPicture,
    /// Start or stop adding an account.
    Adding(bool),
    AddAdmin(bool),
    Add,
    SetAdmin(u32, bool),
    /// Ask to remove (Some), or stop asking (None).
    AskRemove(Option<u32>),
    Remove(u32),
}

/// What the pane is editing.
#[derive(Default)]
pub struct UsersState {
    pub list: Vec<User>,
    /// Your password: current, new, again (while changing it).
    password: Option<[String; 3]>,
    /// A new account: full name, account name, password, administrator.
    adding: Option<([String; 3], bool)>,
    removing: Option<u32>,
}

impl UsersState {
    pub fn refresh(&mut self) {
        self.list = users::list();
    }
}

fn tone_of(u: &User) -> usize {
    u.avatar.strip_prefix("tone:").and_then(|t| t.parse::<usize>().ok()).unwrap_or(u.uid as usize) % TONES.len()
}

fn initials(u: &User) -> String {
    let name = if u.full.is_empty() { &u.name } else { &u.full };
    name.split_whitespace().filter_map(|w| w.chars().next()).take(2).collect::<String>().to_uppercase()
}

fn avatar<'a>(u: &User, size: i32) -> View<'a, Msg> {
    let (c0, c1) = TONES[tone_of(u)];
    let letters = initials(u);
    Canvas::new(move |cv, r| {
        cv.fill_round_rect_dgradient(r, r.w / 2, c0, c1);
        cv.text_centered(r, &letters, theme::ui_bold(if r.w > 40 { 18 } else { 13 }), 0xFFFF_FFFF);
    })
    .size(size, size)
}

fn role(u: &User) -> String {
    let kind = if u.admin { "Administrator" } else { "Standard" };
    let lock = if u.has_password { "password protected" } else { "no password" };
    format!("{kind} · {lock}")
}

impl Settings {
    pub(super) fn users_pane(&self) -> View<'_, Msg> {
        let st = &self.users;
        let me = users::uid();
        let mine = st.list.iter().find(|u| u.uid == me);
        let mut sections = Vec::new();

        // You.
        if let Some(u) = mine {
            let mut rows = alloc::vec![HStack::new()
                .push(avatar(u, 52).on_tap(Msg::Users(UserMsg::NextPicture)))
                .push(
                    VStack::new()
                        .push(Text::new(u.full.clone()).style(TextStyle::Title2))
                        .push(Text::new(format!("{} · {}", u.name, role(u))).foreground(Color::SecondaryLabel))
                        .spacing(2)
                        .alignment(Align::Start)
                        .grow(1),
                )
                .push(
                    Button::new("Change Password…")
                        .action(Msg::Users(UserMsg::EditPassword(true)))
                        .id("login/password")
                )
                .spacing(16)
                .padding_xy(0, 14)
                .fill_width()];
            if let Some(f) = &st.password {
                let field = |i: usize, label: &str| {
                    TextField::secure(label, f[i].clone())
                        .on_change(move |s: String| Msg::Users(UserMsg::Field(i, s)))
                        .grow(1)
                };
                let mut form = HStack::new().spacing(8).padding_xy(0, 8).fill_width();
                if u.has_password {
                    form = form.push(field(0, "Current password"));
                }
                rows.push(
                    form.push(field(1, "New password"))
                        .push(field(2, "Again"))
                        .push(Button::new("Cancel").action(Msg::Users(UserMsg::EditPassword(false))))
                        .push(
                            Button::new("Save")
                                .action(Msg::Users(UserMsg::SavePassword))
                                .button_style(ButtonStyle::Prominent),
                        ),
                );
            }
            sections.push(card(rows));
            sections
                .push(footer("Click your picture to change its colours. Leave the new password empty to remove it."));
        }

        // Everyone else.
        let mut rows: Vec<View<'_, Msg>> = Vec::new();
        for u in st.list.iter().filter(|u| u.uid != me) {
            let uid = u.uid;
            let mut row = HStack::new()
                .push(avatar(u, 34))
                .push(
                    VStack::new()
                        .push(Text::new(u.full.clone()))
                        .push(
                            Text::new(format!("{} · {}", u.name, role(u)))
                                .style(TextStyle::Subheadline)
                                .foreground(Color::SecondaryLabel),
                        )
                        .spacing(2)
                        .alignment(Align::Start)
                        .grow(1),
                )
                .push(Toggle::new("Administrator", u.admin).on_change(move |on| Msg::Users(UserMsg::SetAdmin(uid, on))))
                .spacing(12)
                .min_height(52)
                .fill_width();
            row = if st.removing == Some(uid) {
                row.push(Button::new("Keep").action(Msg::Users(UserMsg::AskRemove(None)))).push(
                    Button::new("Remove")
                        .action(Msg::Users(UserMsg::Remove(uid)))
                        .button_style(ButtonStyle::Destructive),
                )
            } else {
                row.push(Button::new("Remove…").action(Msg::Users(UserMsg::AskRemove(Some(uid)))))
            };
            rows.push(row);
        }
        match &st.adding {
            Some((f, admin)) => {
                let field = |i: usize, label: &str, secure: bool| {
                    let tf = if secure {
                        TextField::secure(label, f[i].clone())
                    } else {
                        TextField::new(label, f[i].clone())
                    };
                    tf.on_change(move |s: String| Msg::Users(UserMsg::Field(i, s))).grow(1)
                };
                rows.push(
                    HStack::new()
                        .push(field(0, "Full name", false))
                        .push(field(1, "Account name", false))
                        .push(field(2, "Password (optional)", true))
                        .spacing(8)
                        .padding_xy(0, 8)
                        .fill_width(),
                );
                rows.push(
                    HStack::new()
                        .push(Toggle::new("Administrator", *admin).on_change(|on| Msg::Users(UserMsg::AddAdmin(on))))
                        .push(Spacer::new())
                        .push(Button::new("Cancel").action(Msg::Users(UserMsg::Adding(false))))
                        .push(
                            Button::new("Add Account")
                                .action(Msg::Users(UserMsg::Add))
                                .button_style(ButtonStyle::Prominent),
                        )
                        .spacing(8)
                        .padding_xy(0, 6)
                        .fill_width(),
                );
            }
            None => rows.push(
                HStack::new()
                    .push(Spacer::new())
                    .push(Button::new("Add Account…").action(Msg::Users(UserMsg::Adding(true))).id("login/add"))
                    .padding_xy(0, 8)
                    .fill_width(),
            ),
        }
        sections.push(titled("Accounts", rows));
        sections.push(footer(
            "Each account has its own files: new accounts get a home folder in /Users. Administrators can \
             change settings for everyone, manage accounts and install apps for everyone; anyone else is \
             asked for an administrator's password first.",
        ));
        sections.push(card(alloc::vec![pref_toggle("login/auto", "Log in automatically", pref::AUTO_LOGIN)]));
        sections.push(footer(
            "With automatic login, WaveOS opens straight to the desktop of the first account. Lock Screen \
             (Super L) and waking from sleep still ask for the password. An account without a password \
             logs in by itself when it is the only one.",
        ));
        page("Users & Login", sections)
    }

    pub(super) fn users_update(&mut self, msg: UserMsg, cx: &mut Context<Msg>) {
        let me = users::uid();
        match msg {
            UserMsg::EditPassword(on) => self.users.password = on.then(Default::default),
            UserMsg::Field(i, s) => {
                if let Some(f) = self.users.password.as_mut() {
                    f[i.min(2)] = s;
                } else if let Some((f, _)) = self.users.adding.as_mut() {
                    f[i.min(2)] = s;
                }
            }
            UserMsg::SavePassword => {
                let Some([old, new, again]) = self.users.password.clone() else { return };
                if new != again {
                    self.flash(cx, String::from("The new passwords don't match"));
                    return;
                }
                if !new.is_empty() && new.chars().count() < 4 {
                    self.flash(cx, String::from("Use at least 4 characters, or none"));
                    return;
                }
                match users::set_password(me, &old, &new) {
                    Ok(()) => {
                        self.users.password = None;
                        self.flash(
                            cx,
                            String::from(if new.is_empty() { "Password removed" } else { "Password changed" }),
                        );
                    }
                    Err(e) if e.is(corekit::abi::err::EACCES) => {
                        self.flash(cx, String::from("That isn't your current password"))
                    }
                    Err(e) if e.is(corekit::abi::err::EAGAIN) => {
                        self.flash(cx, String::from("Too many tries: wait a little"))
                    }
                    Err(e) => self.flash(cx, format!("Couldn't change the password: {e}")),
                }
            }
            UserMsg::NextPicture => {
                if let Some(u) = self.users.list.iter().find(|u| u.uid == me) {
                    let next = format!("tone:{}", (tone_of(u) + 1) % TONES.len());
                    if let Err(e) = users::set_details(me, "", "", &next) {
                        self.flash(cx, format!("Couldn't change the picture: {e}"));
                    }
                }
            }
            UserMsg::Adding(on) => {
                self.users.adding = on.then(Default::default);
                self.users.password = None;
            }
            UserMsg::AddAdmin(on) => {
                if let Some(a) = self.users.adding.as_mut() {
                    a.1 = on;
                }
            }
            UserMsg::Add => {
                let Some(([full, name, password], admin)) = self.users.adding.clone() else { return };
                if full.trim().is_empty() {
                    self.flash(cx, String::from("Enter the person's name"));
                    return;
                }
                match users::add(name.trim(), full.trim(), &password, admin) {
                    Ok(_) => {
                        self.users.adding = None;
                        self.flash(cx, format!("Added {}", full.trim()));
                    }
                    Err(e) if e.is(corekit::abi::err::EEXIST) => {
                        self.flash(cx, String::from("That account name is taken"))
                    }
                    Err(e) if e.is(corekit::abi::err::EINVAL) => {
                        self.flash(cx, String::from("Account names are lowercase letters, digits, - and _"))
                    }
                    Err(e) => self.flash(cx, format!("Couldn't add the account: {e}")),
                }
            }
            UserMsg::SetAdmin(uid, on) => {
                if let Err(e) = users::set_admin(uid, on) {
                    self.flash(cx, format!("Couldn't change that: {e}"));
                }
            }
            UserMsg::AskRemove(uid) => self.users.removing = uid,
            UserMsg::Remove(uid) => {
                self.users.removing = None;
                match users::remove(uid) {
                    Ok(()) => self.flash(cx, String::from("The account was removed; its home folder is kept")),
                    Err(e) => self.flash(cx, format!("Couldn't remove the account: {e}")),
                }
            }
        }
        self.users.refresh();
    }
}
