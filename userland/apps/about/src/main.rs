//! About This Computer — the WVOS mark, version and system information.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;
use corekit::abi::SysInfo;
use corekit::process::fixed_str;
use waveui::lumen::icons::Icon;
use waveui::prelude::*;

pub struct About {
    info: SysInfo,
    tab: usize,
    /// The WayV logo for the footer, scaled once.
    maker: Option<Rc<Pixmap>>,
}

#[derive(Clone)]
pub enum Msg {
    Tab(usize),
    Report,
    Updates,
    Guide(&'static str),
    Web(&'static str),
}

/// Opens a guide in Learn, or a page in Firefox.
fn spawn(program: &str, arg: &str) {
    let _ = corekit::process::spawn(program, &[arg], corekit::process::Stdio::default());
}

impl About {
    fn new() -> Self {
        let maker = corekit::fs::read("/System/Brand/wayv-logo.png")
            .ok()
            .and_then(|d| aurora_image::decode(&d).ok())
            .map(|img| img.thumbnail(80, 30))
            .map(|img| Rc::new(Pixmap { width: img.width, height: img.height, pixels: img.pixels }));
        Self { info: corekit::process::sys_info(), tab: 0, maker }
    }
}

pub fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes >> 20)
}

impl App for About {
    type Message = Msg;

    fn title(&self) -> String {
        "About This Computer".into()
    }
    fn size(&self) -> (i32, i32) {
        (760, 500)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::Tab(t) => self.tab = t,
            Msg::Report => cx.open_app("Pulse"),
            Msg::Updates => spawn("/System/Apps/Learn.elf", "start-installation"),
            Msg::Guide(id) => spawn("/System/Apps/Learn.elf", id),
            Msg::Web(url) => spawn("/System/Apps/Firefox.elf", url),
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(1000)
    }

    fn poll(&mut self, _cx: &mut Context<Msg>) -> bool {
        self.info = corekit::process::sys_info();
        true
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let i = &self.info;
        let version = fixed_str(&i.version, i.version_len);
        let gb = |b: u64| {
            if b >= 1 << 30 {
                format!("{:.1} GB", b as f64 / (1u64 << 30) as f64)
            } else {
                mib(b)
            }
        };
        let rows: Vec<(&str, String)> = match self.tab {
            0 => vec![
                ("Processor", fixed_str(&i.cpu, i.cpu_len)),
                ("Memory", format!("{} · {} in use", gb(i.mem_total), gb(i.mem_used))),
                ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
                ("Startup disk", fixed_str(&i.root, i.root_len)),
                ("Kernel", format!("aster {version}")),
                ("Uptime", corekit::time::format_uptime(env.now_ms)),
            ],
            1 => vec![
                ("Resolution", format!("{} × {}", env.screen.0, env.screen.1)),
                ("Appearance", String::from(if env.dark { "Dark" } else { "Light" })),
                (
                    "Wallpaper",
                    String::from(
                        waveui::lumen::wallpaper::NAMES
                            .get(waveui::lumen::theme::wallpaper() as usize)
                            .copied()
                            .unwrap_or("Your picture"),
                    ),
                ),
                ("Compositor", String::from("Lumen")),
            ],
            2 => vec![
                ("Volume", fixed_str(&i.root, i.root_len)),
                ("Capacity", gb(i.disk_total)),
                ("Available", gb(i.disk_free)),
                ("Used", gb(i.disk_total.saturating_sub(i.disk_free))),
            ],
            _ => Vec::new(),
        };
        let mut facts = VStack::new().spacing(9).alignment(Align::Stretch);
        for (k, v) in rows {
            facts = facts.push(
                HStack::new()
                    .push(
                        Text::new(k)
                            .style(TextStyle::Sized(14, waveui::style::Weight::Regular))
                            .foreground(Color::SecondaryLabel)
                            .text_align(Align::End)
                            .width(110),
                    )
                    .push(Text::new(v).style(TextStyle::Sized(14, waveui::style::Weight::Bold)).grow(1))
                    .spacing(18)
                    .fill_width(),
            );
        }
        let extra: View<'_, Msg> = match self.tab {
            0 => HStack::new()
                .push(Button::new("System Report…").action(Msg::Report).control_size(ControlSize::Small))
                .push(Button::new("Software Update…").action(Msg::Updates).control_size(ControlSize::Small))
                .spacing(10),
            1 => {
                Button::new("Display Settings…").action(Msg::Guide("system-hardware")).control_size(ControlSize::Small)
            }
            2 => ProgressView::new(Some(i.disk_total.saturating_sub(i.disk_free) as f32 / i.disk_total.max(1) as f32))
                .width(300),
            _ => VStack::new()
                .push(
                    Text::new("Help is close at hand.")
                        .style(TextStyle::Sized(14, waveui::style::Weight::Regular))
                        .foreground(Color::SecondaryLabel),
                )
                .push(
                    Button::new("Open the WaveOS Guide")
                        .action(Msg::Guide("start-welcome"))
                        .control_size(ControlSize::Small),
                )
                .push(
                    Button::new("Troubleshooting and Recovery")
                        .action(Msg::Guide("reference-recovery"))
                        .control_size(ControlSize::Small),
                )
                .push(
                    Button::new("Report a Problem…")
                        .action(Msg::Web("https://github.com/Sw3bbl3/waveos-aurora-rs/issues"))
                        .control_size(ControlSize::Small),
                )
                .spacing(10)
                .alignment(Align::Start),
        };
        let dark = env.dark;
        // The WVOS mark with a soft glow.
        let mark = ZStack::with(vec![
            Canvas::new(move |cv, r| {
                cv.glow(r.inset(20), 30, 34, waveui::lumen::theme::accent_glow(), if dark { 70 } else { 40 })
            })
            .size(170, 170),
            Image::app_icon(Icon::Aurora, 128),
        ]);
        let name = HStack::new()
            .push(Text::new("WaveOS").style(TextStyle::Sized(38, waveui::style::Weight::Display)))
            .push(Text::new("Aurora").style(TextStyle::Sized(38, waveui::style::Weight::Light)))
            .spacing(10);
        let info = VStack::new()
            .push(name)
            .push(
                Text::new(format!("Version {version}"))
                    .style(TextStyle::Sized(14, waveui::style::Weight::Regular))
                    .foreground(Color::SecondaryLabel),
            )
            .push(facts.padding_insets(Insets { top: 14, leading: 0, bottom: 6, trailing: 0 }))
            .push(extra)
            .spacing(4)
            .alignment(Align::Start);
        let maker = match &self.maker {
            Some(img) => Image::new(img.clone()).size(img.width as i32, img.height as i32),
            None => Text::new("WayV").style(TextStyle::Headline),
        };
        VStack::new()
            .push(toolbar(vec![
                Spacer::new(),
                Picker::new(["Overview", "Displays", "Storage", "Support"], self.tab).on_change(Msg::Tab).width(330),
                Spacer::new(),
            ]))
            .push(Spacer::new())
            .push(HStack::new().push(mark).push(info).spacing(40).alignment(Align::Center))
            .push(Spacer::new())
            .push(maker)
            .push(
                Text::new("™ and © 2026 WayV. Made with Rust. MIT License.")
                    .style(TextStyle::Sized(12, waveui::style::Weight::Regular))
                    .foreground(Color::TertiaryLabel),
            )
            .spacing(6)
            .padding_insets(Insets { top: 0, leading: 0, bottom: 18, trailing: 0 })
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(About::new())
}
