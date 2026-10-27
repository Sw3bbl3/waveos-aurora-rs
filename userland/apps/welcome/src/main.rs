//! Welcome — first-run introduction.

#![no_std]
#![no_main]

extern crate alloc;

use waveui::lumen::canvas::{mix, rgb};
use waveui::lumen::icons::{self, Icon};
use waveui::lumen::theme;
use waveui::prelude::*;

pub struct Welcome;

#[derive(Clone)]
pub enum Msg {
    GetStarted,
    About,
}

const TIPS: [(Icon, &str, &str); 3] = [
    (Icon::Launcher, "Open apps from Shoal", "Click the grid at the left of Harbor, or press the Super key."),
    (
        Icon::Files,
        "Windows feel familiar",
        "Drag the title bar to move, double-click it to zoom, drag a corner to resize.",
    ),
    (
        Icon::Terminal,
        "Explore under the hood",
        "Every app is its own process. Open Terminal and try ps, neofetch or ls.",
    ),
];

impl App for Welcome {
    type Message = Msg;

    fn title(&self) -> String {
        "Welcome to WaveOS Aurora".into()
    }
    fn size(&self) -> (i32, i32) {
        (640, 440)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        match msg {
            Msg::GetStarted => {
                cx.open_app("Learn");
                cx.close();
            }
            Msg::About => cx.open_app("About"),
        }
    }

    fn view(&self, _env: &Env) -> View<'_, Msg> {
        // The hero banner, in the aurora gradient.
        let hero = Canvas::new(|cv, hero| {
            cv.fill_round_rect_with(hero, 14, |x, y| {
                let tx = (x - hero.x) * 256 / hero.w.max(1);
                let ty = (y - hero.y) * 256 / hero.h.max(1);
                let a = mix(rgb(0x5B, 0x3F, 0xE0), rgb(0x16, 0xB8, 0xA6), tx);
                mix(a, rgb(0x0B, 0x12, 0x33), ty / 2)
            });
            let r = |x, y, w, h| waveui::lumen::geom::Rect::new(hero.right() - x, hero.y + y, w, h);
            icons::wave(cv, r(190, 30, 160, 70), 2, 5, 0x50FF_FFFF);
            icons::wave(cv, r(170, 52, 130, 50), 2, 3, 0x38FF_FFFF);
            cv.text(hero.x + 28, hero.y + 62, "Welcome to WaveOS Aurora", theme::ui_bold(28), 0xFFFF_FFFF);
            cv.text(
                hero.x + 28,
                hero.y + 96,
                "A brand-new operating system, built from scratch.",
                theme::ui(16),
                0xDDFF_FFFF,
            );
        })
        .height(132)
        .fill_width();
        let tips = TIPS.iter().map(|&(icon, title, body)| {
            HStack::new()
                .push(Image::app_icon(icon, 40))
                .push(
                    VStack::new()
                        .push(Text::new(title).style(TextStyle::Headline))
                        .push(Text::new(body).foreground(Color::SecondaryLabel))
                        .spacing(3)
                        .alignment(Align::Start)
                        .grow(1),
                )
                .spacing(16)
                .fill_width()
        });
        VStack::new()
            .push(hero)
            .push(VStack::with(tips.collect()).spacing(18).padding_xy(16, 6))
            .push(Spacer::new())
            .push(
                HStack::new()
                    .push(Spacer::new())
                    .push(Button::new("About This Computer").action(Msg::About).min_width(148))
                    .push(
                        Button::new("Get Started")
                            .action(Msg::GetStarted)
                            .button_style(ButtonStyle::Prominent)
                            .shortcut(Shortcut::default_action())
                            .min_width(148),
                    )
                    .spacing(14),
            )
            .spacing(18)
            .alignment(Align::Stretch)
            .padding_insets(Insets { top: 8, leading: 20, bottom: 22, trailing: 24 })
            .fill()
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    waveui::run(Welcome)
}
