//! About This Computer — the WVOS mark, version and system information.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use aurorakit::canvas::Canvas;
use aurorakit::geom::Rect;
use aurorakit::icons::{self, Icon};
use aurorakit::theme;
use aurorakit::{App, Env};

use corekit::abi::SysInfo;
use corekit::process::fixed_str;

pub struct About {
    info: SysInfo,
    last_second: u64,
    /// The WayV logo for the footer, scaled once.
    maker: Option<aurora_image::Image>,
}

impl About {
    fn new() -> Self {
        let maker = corekit::fs::read("/System/Brand/wayv-logo.png")
            .ok()
            .and_then(|d| aurora_image::decode(&d).ok())
            .map(|img| img.thumbnail(92, 40));
        Self { info: corekit::process::sys_info(), last_second: 0, maker }
    }
}

pub fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes >> 20)
}

impl App for About {
    fn title(&self) -> String {
        "About This Computer".into()
    }
    fn size(&self) -> (i32, i32) {
        (520, 560)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let (cx, _) = area.center();
        // The WVOS mark with a soft glow, the name and the version.
        let mark = Rect::new(cx - 52, area.y + 36, 104, 104);
        cv.glow(mark, 26, 28, theme::accent_glow(), if t.dark { 70 } else { 40 });
        icons::draw(cv, Icon::Aurora, mark);
        let title = "WaveOS Aurora";
        let f = theme::display(30);
        cv.text(cx - f.width(title) / 2, area.y + 182, title, f, t.text);
        let i = &self.info;
        let version = fixed_str(&i.version, i.version_len);
        let ver = format!("Version {version}");
        let f2 = theme::ui(14);
        cv.text(cx - f2.width(&ver) / 2, area.y + 206, &ver, f2, t.text_secondary);

        let rows = [
            ("Kernel", format!("Aster {version} · x86_64")),
            ("Processor", fixed_str(&i.cpu, i.cpu_len)),
            ("Memory", format!("{} · {} in use", mib(i.mem_total), mib(i.mem_used))),
            ("Processes", format!("{} running", i.processes)),
            ("Volume", fixed_str(&i.root, i.root_len)),
            ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
            ("Uptime", corekit::time::format_uptime(env.now_ms)),
        ];
        let card = Rect::new(area.x + 28, area.y + 232, area.w - 56, rows.len() as i32 * 34 + 12);
        cv.fill_round_rect(card, 18, t.window_bg_alt);
        let fk = theme::ui_bold(13);
        let fv = theme::ui(13);
        for (n, (k, v)) in rows.iter().enumerate() {
            let y = card.y + 6 + n as i32 * 34;
            if n > 0 {
                cv.fill_rect(Rect::new(card.x + 16, y, card.w - 32, 1), t.separator);
            }
            cv.text(card.x + 18, y + 22, k, fk, t.text);
            let w = fv.width(v).min(card.w - 150);
            cv.text_clipped(card.right() - 18 - w, y + 22, v, fv, t.text_secondary, card.w - 150);
        }

        // Made by WayV.
        let foot_y = area.bottom() - 40;
        let made = "Made by";
        let f3 = theme::ui(12);
        match &self.maker {
            Some(img) => {
                let total = f3.width(made) + 8 + img.width as i32;
                let x = cx - total / 2;
                cv.text(x, foot_y + 4, made, f3, t.text_secondary);
                let r = Rect::new(x + f3.width(made) + 8, foot_y + 4 - img.height as i32 / 2 - 4, img.width as i32, img.height as i32);
                cv.draw_image(&img.pixels, img.width, img.height, r, 255);
            }
            None => {
                let s = "Made by WayV";
                cv.text(cx - f3.width(s) / 2, foot_y + 4, s, f3, t.text_secondary);
            }
        }
        let foot = "© 2026 WaveOS Aurora contributors · MIT License";
        let f4 = theme::ui(11);
        cv.text(cx - f4.width(foot) / 2, area.bottom() - 14, foot, f4, t.text_secondary);
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        let s = env.now_ms / 1000;
        if core::mem::replace(&mut self.last_second, s) != s {
            self.info = corekit::process::sys_info();
            return true;
        }
        false
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    aurorakit::run(About::new())
}
