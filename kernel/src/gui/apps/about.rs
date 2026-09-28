use super::{App, AppKind, Env};
use crate::gui::canvas::Canvas;
use crate::gui::geom::Rect;
use crate::gui::icons::{self, Icon};
use crate::gui::theme;
use alloc::format;
use alloc::string::String;

pub struct About {
    cpu: String,
    last_second: u64,
}

impl About {
    pub fn new() -> Self {
        Self { cpu: crate::arch::cpu::brand(), last_second: 0 }
    }
}

pub fn format_uptime(ms: u64) -> String {
    let s = ms / 1000;
    let (h, m, s) = (s / 3600, s / 60 % 60, s % 60);
    if h > 0 {
        format!("{h}h {m:02}m {s:02}s")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

pub fn mib(bytes: u64) -> String {
    format!("{} MiB", bytes >> 20)
}

impl App for About {
    fn kind(&self) -> AppKind {
        AppKind::About
    }
    fn title(&self) -> String {
        "About WaveOS Aurora".into()
    }
    fn size(&self) -> (i32, i32) {
        (520, 400)
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
        let ver = format!("Version {} · Milestone 1", crate::VERSION);
        let f2 = theme::ui(13);
        cv.text(cx - f2.width(&ver) / 2, area.y + 160, &ver, f2, t.text_secondary);

        let mem = crate::mm::stats();
        let rows = [
            ("Kernel", format!("Tide {} (x86_64, hybrid)", crate::VERSION)),
            ("Processor", self.cpu.clone()),
            ("Memory", format!("{} total · {} in use", mib(mem.total_bytes), mib(mem.used_bytes))),
            ("Kernel heap", format!("{} of {} used", mib(mem.heap_used.max(1 << 20)), mib(mem.heap_size))),
            ("Display", format!("{} × {}", env.screen.0, env.screen.1)),
            ("Uptime", format_uptime(env.now_ms)),
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
        core::mem::replace(&mut self.last_second, s) != s
    }
}
