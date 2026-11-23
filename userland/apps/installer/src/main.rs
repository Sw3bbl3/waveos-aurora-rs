//! Installer — shows what a `.gina` package is (publisher, signature,
//! permissions, size), installs it with progress, and removes installed apps.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use alloc::sync::Arc;
use corekit::apps::{self, Options, Review, Scope, Trust};
use corekit::{fs, sync::Mutex, thread};
use waveui::lumen::icons::Icon;
use waveui::prelude::*;
use waveui::Pixmap;

/// The package's icon, decoded (kept as plain pixels to cross threads).
type Picture = (u32, u32, Vec<u32>);

#[derive(Clone)]
enum Phase {
    Reading,
    Ready(alloc::boxed::Box<Review>, Option<Picture>),
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
    /// The person accepted installing an unsigned or untrusted package.
    consent: bool,
    /// Install for this account only.
    just_me: bool,
    icon: Option<Rc<Pixmap>>,
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
    Consent(bool),
    JustMe(bool),
    Uninstall,
}

/// The icon inside the package, if it has a usable one.
fn package_icon(bytes: &[u8]) -> Option<Picture> {
    let p = gina::Package::parse(bytes).ok()?;
    let icon = p.files.iter().find(|f| !p.manifest.icon.is_empty() && f.path == p.manifest.icon)?;
    gina::icon_size(icon.bytes)?;
    let img = aurora_image::decode(icon.bytes).ok()?;
    Some((img.width, img.height, img.pixels))
}

fn human(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{}.{} MB", b >> 20, ((b % (1 << 20)) * 10) >> 20),
        b if b >= 1 << 10 => format!("{} KB", b >> 10),
        b => format!("{b} bytes"),
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

impl Installer {
    fn new(path: Option<String>) -> Self {
        let phase = Arc::new(Mutex::new(Phase::Reading));
        if let Some(path) = path.clone() {
            let state = phase.clone();
            let result = thread::spawn(move || {
                let result = fs::read(&path)
                    .map_err(|e| format!("Cannot read package: {e}"))
                    .and_then(|b| apps::review(&b).map(|r| (r, package_icon(&b))));
                *state.lock() = match result {
                    Ok((r, icon)) => Phase::Ready(alloc::boxed::Box::new(r), icon),
                    Err(e) => Phase::Failed(e),
                };
            });
            if let Err(e) = result {
                *phase.lock() = Phase::Failed(format!("Cannot start installer: {e}"));
            }
        }
        Self {
            path,
            phase,
            installed: apps::list(),
            selected: None,
            delete_data: false,
            consent: false,
            just_me: false,
            icon: None,
            shown: (0, 0),
        }
    }

    /// Where the install has got to: 0 reading, 1 review, 2 installing, 3 done.
    fn step(&self) -> (usize, bool) {
        match &*self.phase.lock() {
            Phase::Reading => (0, false),
            Phase::Ready(..) => (1, false),
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

    fn picture<'a>(icon: Option<Rc<Pixmap>>, size: i32) -> View<'a, Msg> {
        match icon {
            Some(p) => Image::new(p).size(size, size),
            None => Image::symbol(Symbol::Download)
                .symbol_size(size * 2 / 5)
                .foreground(Color::Custom(0xFFFF_FFFF))
                .size(size, size)
                .background_rounded(Color::Accent, size / 4),
        }
    }

    /// A card for an app: its picture, name and detail, and a note on the right.
    fn app_card<'a>(
        icon: Option<Rc<Pixmap>>,
        name: String,
        detail: String,
        note: Option<(String, Color)>,
        selected: bool,
    ) -> View<'a, Msg> {
        let mut row = HStack::new()
            .push(Self::picture(icon, 48))
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

    /// A label and value, as in the details list.
    fn fact<'a>(label: &str, value: View<'a, Msg>) -> View<'a, Msg> {
        HStack::new()
            .push(Text::new(String::from(label)).foreground(Color::SecondaryLabel).width(110))
            .push(value)
            .spacing(12)
            .fill_width()
    }

    fn trust_row<'a>(trust: &Trust) -> View<'a, Msg> {
        let (symbol, color, text) = match trust {
            Trust::Trusted(name) => (Symbol::Check, Color::Green, format!("Signed by {name}")),
            Trust::Unknown(k) => (
                Symbol::Warning,
                Color::Orange,
                format!("Signed by a publisher you don't trust (key {})", gina::fingerprint(k)),
            ),
            Trust::Unsigned => (Symbol::Warning, Color::Orange, String::from("Not signed: nobody vouches for this package")),
        };
        HStack::new()
            .push(Image::symbol(symbol).symbol_size(13).foreground(color))
            .push(Text::new(text).foreground(color))
            .spacing(6)
    }

    fn review<'a>(&'a self, r: &Review) -> (String, String, View<'a, Msg>, Vec<View<'a, Msg>>) {
        let m = &r.manifest;
        let blocked = !r.blockers.is_empty();
        let trusted = matches!(r.trust, Trust::Trusted(_));
        let title = if blocked {
            format!("Can't install “{}”", m.name)
        } else if r.is_update() {
            format!("Update “{}”?", m.name)
        } else {
            format!("Install “{}”?", m.name)
        };
        let subtitle = if !m.description.is_empty() {
            m.description.clone()
        } else if r.is_update() {
            String::from("A newer version of an app you have. Its data is kept.")
        } else {
            String::from("It goes into Applications and shows up in Shoal and Seek.")
        };
        let note = match &r.current {
            Some(_) if r.is_downgrade() => (String::from("Older"), Color::Orange),
            Some(_) => (String::from("Update"), Color::AccentText),
            None => (String::from("New"), Color::AccentText),
        };
        let mut body = VStack::new()
            .push(Self::app_card(
                self.icon.clone(),
                format!("{} · {}", m.name, m.version),
                format!("{} · {}", m.developer, m.id),
                Some(note),
                true,
            ))
            .spacing(10)
            .alignment(Align::Stretch);
        let mut facts = VStack::new()
            .push(Self::fact("Publisher", Text::new(m.developer.clone())))
            .push(Self::fact("Signature", Self::trust_row(&r.trust)))
            .spacing(8)
            .alignment(Align::Stretch);
        let version = match &r.current {
            Some(c) => format!("{} (you have {})", m.version, c.manifest.version),
            None => m.version.clone(),
        };
        facts = facts.push(Self::fact("Version", Text::new(version)));
        facts = facts.push(Self::fact("Size", Text::new(human(r.size))));
        if !m.categories.is_empty() {
            let cats: Vec<String> = m.categories.iter().map(|c| title_case(c)).collect();
            facts = facts.push(Self::fact("Category", Text::new(cats.join(", "))));
        }
        if !m.extensions.is_empty() {
            facts = facts.push(Self::fact("Opens", Text::new(format!(".{}", m.extensions.join(", .")))));
        }
        body = body.push(facts.padding_xy(4, 6));
        let mut perms = VStack::new().spacing(6).alignment(Align::Start);
        if m.permissions.is_empty() {
            perms = perms.push(Text::new("It asks for no special permissions.").foreground(Color::SecondaryLabel));
        } else {
            perms = perms.push(
                Text::new(format!("{} asks to:", m.name)).style(TextStyle::Sized(13, waveui::style::Weight::Bold)),
            );
            for p in &m.permissions {
                let sensitive = gina::sensitive_permission(p);
                perms = perms.push(
                    HStack::new()
                        .push(
                            Image::symbol(if sensitive { Symbol::Warning } else { Symbol::Check })
                                .symbol_size(12)
                                .foreground(if sensitive { Color::Orange } else { Color::SecondaryLabel }),
                        )
                        .push(Text::new(gina::describe_permission(p)))
                        .spacing(8),
                );
            }
        }
        body = body.push(perms.padding(14).background_rounded(Color::Fill, 12).fill_width());
        for b in &r.blockers {
            body = body.push(Text::paragraph(b.clone()).foreground(Color::Red));
        }
        if !blocked {
            if !trusted {
                body = body.push(
                    Toggle::checkbox("I trust where this package came from", self.consent).on_change(Msg::Consent),
                );
            }
            if r.current.is_none() {
                body = body.push(Toggle::checkbox("Install only for me", self.just_me).on_change(Msg::JustMe));
            }
        }
        let mut buttons = vec![Button::new("Cancel").action(Msg::Close).shortcut(Shortcut::cancel())];
        if !blocked {
            buttons.push(
                Button::new(if r.is_update() { "Update" } else { "Install" })
                    .action(Msg::Install)
                    .button_style(ButtonStyle::Prominent)
                    .disabled(!trusted && !self.consent)
                    .shortcut(Shortcut::default_action()),
            );
        }
        (title, subtitle, ScrollView::new(body).grow(1), buttons)
    }

    fn package(&self) -> View<'_, Msg> {
        let note = HStack::new()
            .push(Image::symbol(Symbol::Lock).symbol_size(13).foreground(Color::SecondaryLabel))
            .push(
                Text::new("Every file is checked against the package's signed list before anything is written.")
                    .style(TextStyle::Callout)
                    .foreground(Color::SecondaryLabel),
            )
            .spacing(10);
        let (title, subtitle, body, buttons): (String, String, View<'_, Msg>, Vec<View<'_, Msg>>) =
            match self.phase.lock().clone() {
                Phase::Reading => (
                    String::from("Reading the package"),
                    String::from("Checking that it is whole, signed and made for this system."),
                    HStack::new().push(ProgressView::spinner()).push(Text::new("One moment…")).spacing(10),
                    vec![Button::new("Cancel").action(Msg::Close).shortcut(Shortcut::cancel())],
                ),
                Phase::Ready(r, _) => self.review(&r),
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
            .push(Spacer::new())
            .push(note)
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
                    a.icon().and_then(|p| Pixmap::load(&p)),
                    format!("{} · {}", a.manifest.name, a.manifest.version),
                    format!("{} · {} · for {}", a.manifest.developer, human(a.receipt.size), a.scope.name()),
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
    fn min_size(&self) -> (i32, i32) {
        (560, 420)
    }

    fn size(&self) -> (i32, i32) {
        (940, 660)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Install => {
                let Some(path) = self.path.clone() else { return };
                let review = match &*self.phase.lock() {
                    Phase::Ready(r, _) => r.clone(),
                    _ => return,
                };
                if !review.blockers.is_empty() || (!matches!(review.trust, Trust::Trusted(_)) && !self.consent) {
                    return;
                }
                *self.phase.lock() = Phase::Installing(0, 1);
                let state = self.phase.clone();
                let opts = Options {
                    scope: if self.just_me { Scope::User } else { Scope::System },
                    allow_untrusted: self.consent,
                    allow_downgrade: true,
                    source: path.clone(),
                };
                let result = thread::spawn(move || {
                    let result = fs::read(&path).map_err(|e| format!("Cannot read package: {e}")).and_then(|b| {
                        apps::install_with(&b, &opts, |n, total| *state.lock() = Phase::Installing(n, total))
                    });
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
                    match apps::launch(&id) {
                        Ok(_) => cx.close(),
                        Err(e) => *self.phase.lock() = Phase::Failed(e),
                    }
                }
            }
            Msg::Close => cx.close(),
            Msg::Select(i) => self.selected = Some(i),
            Msg::DeleteData(on) => self.delete_data = on,
            Msg::Consent(on) => self.consent = on,
            Msg::JustMe(on) => self.just_me = on,
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
        self.path.as_ref().map(|_| 200)
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        let now = match &mut *self.phase.lock() {
            Phase::Reading => (0, 0),
            Phase::Ready(_, icon) => {
                if let Some((w, h, pixels)) = icon.take() {
                    self.icon = Some(Rc::new(Pixmap { width: w, height: h, pixels }));
                }
                (1, 0)
            }
            Phase::Installing(n, _) => (2, *n),
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
