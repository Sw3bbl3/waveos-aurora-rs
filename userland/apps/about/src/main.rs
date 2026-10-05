//! About WaveOS Aurora — version and system information.

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
}

impl About {
    pub fn new() -> Self {
        Self { info: corekit::process::sys_info(), last_second: 0 }
    }
}

pub fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes >> 20)
}

impl App for About {
    fn title(&self) -> String {
        "About WaveOS Aurora".into()
    }
    fn size(&self) -> (i32, i32) {
        (520, 420)
    }
    fn resizable(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        let t = theme::current();
        let (cx, _) = area.center();
        icons::draw(cv, Icon::Aurora, Rect::new(cx - 44, area.y + 12, 88, 88));
        let title = "WaveOS Aurora";
        let f = theme::ui_bold(28);
        cv.text(cx - f.width(title) / 2, area.y + 138, title, f, t.text);
        let i = &self.info;
        let version = fixed_str(&i.version, i.version_len);
        let ver = format!("Version {}", version);
        let f2 = theme::ui(13);
        cv.text(cx - f2.width(&ver) / 2, area.y + 160, &ver, f2, t.text_secondary);

        let rows = [
            ("Kernel", format!("Aster {} (x86_64, hybrid)", version)),
            ("Processor", fixed_str(&i.cpu, i.cpu_len)),
            ("Memory", format!("{} total · {} in use", mib(i.mem_total), mib(i.mem_used))),
            ("Processes", format!("{} running", i.processes)),
            ("Home volume", fixed_str(&i.root, i.root_len)),
            ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
            ("Uptime", corekit::time::format_uptime(env.now_ms)),
        ];
        let mut y = area.y + 196;
        let label_x = cx - 16;
        for (k, v) in rows.iter() {
            let fk = theme::ui_bold(13);
            cv.text(label_x - fk.width(k), y, k, fk, t.text);
            cv.text_clipped(label_x + 12, y, v, theme::ui(13), t.text_secondary, area.right() - label_x - 32);
            y += 22;
        }
        let foot = "© 2026 WaveOS Aurora contributors · MIT License";
        let f3 = theme::ui(11);
        cv.text(cx - f3.width(foot) / 2, area.bottom() - 16, foot, f3, t.text_secondary);
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
