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
    /// The WayV logo for the footer, scaled once.
    maker: Option<Rc<Pixmap>>,
}

#[derive(Clone)]
pub enum Msg {}

impl About {
    fn new() -> Self {
        let maker = corekit::fs::read("/System/Brand/wayv-logo.png")
            .ok()
            .and_then(|d| aurora_image::decode(&d).ok())
            .map(|img| img.thumbnail(92, 40))
            .map(|img| Rc::new(Pixmap { width: img.width, height: img.height, pixels: img.pixels }));
        Self { info: corekit::process::sys_info(), maker }
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
        (520, 560)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, _cx: &mut Context<Msg>) {
        match msg {}
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
        let rows = [
            ("Kernel", format!("Aster {version} · x86_64")),
            ("Processor", fixed_str(&i.cpu, i.cpu_len)),
            ("Memory", format!("{} · {} in use", mib(i.mem_total), mib(i.mem_used))),
            ("Processes", format!("{} running", i.processes)),
            ("Volume", fixed_str(&i.root, i.root_len)),
            ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
            ("Uptime", corekit::time::format_uptime(env.now_ms)),
        ];
        let dark = env.dark;
        // The WVOS mark with a soft glow.
        let mark = ZStack::with(vec![
            Canvas::new(move |cv, r| {
                cv.glow(r.inset(16), 26, 28, waveui::lumen::theme::accent_glow(), if dark { 70 } else { 40 })
            })
            .size(124, 124),
            Image::app_icon(Icon::Aurora, 100),
        ]);
        let details = Section::new()
            .extend(rows.into_iter().map(|(k, v)| row(k, Text::new(v).foreground(Color::SecondaryLabel)).height(34)));
        let maker = match &self.maker {
            Some(img) => HStack::new()
                .push(Text::new("Made by").style(TextStyle::Callout).foreground(Color::SecondaryLabel))
                .push(Image::new(img.clone()).size(img.width as i32, img.height as i32)),
            None => Text::new("Made by WayV").style(TextStyle::Callout).foreground(Color::SecondaryLabel),
        };
        VStack::new()
            .push(mark)
            .push(
                VStack::new()
                    .push(Text::new("WaveOS Aurora").style(TextStyle::Sized(30, waveui::style::Weight::Display)))
                    .push(Text::new(format!("Version {version}")).foreground(Color::SecondaryLabel))
                    .spacing(4),
            )
            .push(details.padding_xy(28, 0))
            .push(Spacer::new())
            .push(maker)
            .push(
                Text::new("© 2026 WaveOS Aurora contributors · MIT License")
                    .style(TextStyle::Footnote)
                    .foreground(Color::SecondaryLabel),
            )
            .spacing(10)
            .padding_insets(Insets { top: 14, leading: 0, bottom: 12, trailing: 0 })
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(About::new())
}
