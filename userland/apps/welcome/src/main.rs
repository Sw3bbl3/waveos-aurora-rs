//! Welcome — first-run introduction.

#![no_std]
#![no_main]

extern crate alloc;

use aurorakit::canvas::{rgb, Canvas};
use aurorakit::geom::Rect;
use aurorakit::icons::{self, Icon};
use aurorakit::theme;
use aurorakit::widgets::{button, ButtonStyle};
use aurorakit::{hit, App, Env, Request};

pub struct Welcome {
    hovered: Option<usize>,
}

const TIPS: [(Icon, &str, &str); 3] = [
    (
        Icon::Launcher,
        "Open apps from the launcher",
        "Click the grid at the left of the dock, or press the Windows / Super key.",
    ),
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

impl Welcome {
    pub fn new() -> Self {
        Self { hovered: None }
    }

    fn buttons(area: Rect) -> [Rect; 2] {
        let y = area.bottom() - 58;
        [Rect::new(area.right() - 172, y, 148, 36), Rect::new(area.right() - 334, y, 148, 36)]
    }
}

impl App for Welcome {
    fn title(&self) -> alloc::string::String {
        "Welcome to WaveOS Aurora".into()
    }
    fn size(&self) -> (i32, i32) {
        (640, 440)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, _env: &Env) {
        let t = theme::current();
        // Hero banner with the aurora gradient.
        let hero = Rect::new(area.x + 20, area.y + 8, area.w - 40, 132);
        cv.fill_round_rect_with(hero, 14, |x, y| {
            let tx = (x - hero.x) * 256 / hero.w;
            let ty = (y - hero.y) * 256 / hero.h;
            let a = aurorakit::canvas::mix(rgb(0x5B, 0x3F, 0xE0), rgb(0x16, 0xB8, 0xA6), tx);
            aurorakit::canvas::mix(a, rgb(0x0B, 0x12, 0x33), ty / 2)
        });
        icons::wave(cv, Rect::new(hero.right() - 190, hero.y + 30, 160, 70), 2, 5, 0x50FF_FFFF);
        icons::wave(cv, Rect::new(hero.right() - 170, hero.y + 52, 130, 50), 2, 3, 0x38FF_FFFF);
        cv.text(hero.x + 28, hero.y + 62, "Welcome to WaveOS Aurora", theme::ui_bold(28), 0xFFFF_FFFF);
        cv.text(
            hero.x + 28,
            hero.y + 96,
            "A brand-new operating system, built from scratch.",
            theme::ui(16),
            0xDDFF_FFFF,
        );

        let mut y = hero.bottom() + 24;
        for (icon, title, body) in TIPS {
            icons::draw(cv, icon, Rect::new(area.x + 36, y, 40, 40));
            cv.text(area.x + 92, y + 16, title, theme::ui_bold(14), t.text);
            cv.text(area.x + 92, y + 35, body, theme::ui(13), t.text_secondary);
            y += 58;
        }

        let [start, about] = Self::buttons(area);
        button(cv, start, "Get Started", ButtonStyle::Primary, self.hovered == Some(0));
        button(cv, about, "About Aurora", ButtonStyle::Secondary, self.hovered == Some(1));
    }

    fn click(&mut self, x: i32, y: i32, area: Rect, env: &mut Env) -> bool {
        match hit(&Self::buttons(area), x, y) {
            Some(0) => env.requests.push(Request::Close),
            Some(1) => env.requests.push(Request::OpenApp("About".into())),
            _ => {}
        }
        false
    }

    fn hover(&mut self, x: i32, y: i32, area: Rect) -> bool {
        let h = hit(&Self::buttons(area), x, y);
        core::mem::replace(&mut self.hovered, h) != h
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    aurorakit::run(Welcome::new())
}
