use super::{App, AppKind, Env, Request};
use crate::drivers::input::{KeyCode, KeyEvent};
use crate::fs;
use crate::gui::canvas::{rgb, Canvas};
use crate::gui::geom::Rect;
use crate::gui::theme;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

const BG: u32 = rgb(0x14, 0x16, 0x1D);
const FG: u32 = rgb(0xD8, 0xDE, 0xE9);
const DIM: u32 = rgb(0x7D, 0x86, 0x9A);
const GREEN: u32 = rgb(0x7E, 0xE7, 0x87);
const CYAN: u32 = rgb(0x5E, 0xEA, 0xD4);
const VIOLET: u32 = rgb(0xA7, 0x8B, 0xFA);
const RED: u32 = rgb(0xFF, 0x7B, 0x72);
const LINE_H: i32 = 19;
const PAD: i32 = 12;

pub struct Terminal {
    lines: Vec<(String, u32)>,
    input: String,
    cwd: String,
    history: Vec<String>,
    hist_pos: usize,
    scroll_back: i32,
    caret_on: bool,
    last_blink: u64,
}

impl Terminal {
    pub fn new() -> Self {
        let mut t = Self {
            lines: Vec::new(),
            input: String::new(),
            cwd: String::from("/"),
            history: Vec::new(),
            hist_pos: 0,
            scroll_back: 0,
            caret_on: true,
            last_blink: 0,
        };
        t.out(&format!("WaveOS Aurora {} — Tide kernel shell", crate::VERSION), VIOLET);
        t.out("Type `help` to see what you can do.", DIM);
        t.out("", FG);
        t
    }

    fn out(&mut self, s: &str, color: u32) {
        for line in s.split('\n') {
            self.lines.push((line.to_string(), color));
        }
        if self.lines.len() > 1000 {
            self.lines.drain(..self.lines.len() - 1000);
        }
    }

    fn prompt(&self) -> String {
        format!("aurora:{}$ ", self.cwd)
    }

    fn run(&mut self, cmdline: &str, env: &mut Env) {
        let mut parts = cmdline.split_whitespace();
        let Some(cmd) = parts.next() else { return };
        let args: Vec<&str> = parts.collect();
        let rest = cmdline.trim_start()[cmd.len()..].trim();
        match cmd {
            "help" => self.out(
                "Built-in commands:\n  help              this list\n  clear             clear the screen\n  echo TEXT         print text (echo TEXT > FILE writes a file)\n  ls [DIR]          list a directory\n  cd DIR            change directory\n  cat FILE          print a file\n  mkdir DIR         create a directory\n  rm PATH           remove a file or empty directory\n  open APP|FILE     open an app (files, notes, settings…) or a text file\n  neofetch          system summary\n  uname, uptime, date, mem, ps\n  theme light|dark  switch appearance\n  reboot, shutdown\n  panic             test the kernel crash screen",
                FG,
            ),
            "clear" => self.lines.clear(),
            "echo" => {
                if let Some((text, file)) = rest.split_once('>') {
                    let path = fs::resolve(&self.cwd, file.trim());
                    let mut data = String::from(text.trim());
                    data.push('\n');
                    if !fs::write(&path, data.as_bytes()) {
                        self.out(&format!("echo: cannot write {path}"), RED);
                    }
                } else {
                    self.out(rest, FG);
                }
            }
            "ls" => {
                let dir = fs::resolve(&self.cwd, args.first().copied().unwrap_or("."));
                match fs::list(&dir) {
                    Some(entries) if entries.is_empty() => self.out("(empty)", DIM),
                    Some(entries) => {
                        for e in entries {
                            if e.is_dir {
                                self.out(&format!("{}/", e.name), CYAN);
                            } else {
                                self.out(&format!("{:<28} {:>6} B", e.name, e.size), FG);
                            }
                        }
                    }
                    None => self.out(&format!("ls: no such directory: {dir}"), RED),
                }
            }
            "cd" => {
                let dir = fs::resolve(&self.cwd, args.first().copied().unwrap_or("/"));
                if fs::is_dir(&dir) {
                    self.cwd = dir;
                } else {
                    self.out(&format!("cd: no such directory: {dir}"), RED);
                }
            }
            "cat" => {
                if rest.is_empty() {
                    self.out("usage: cat FILE", RED);
                } else {
                    let path = fs::resolve(&self.cwd, rest);
                    match fs::read(&path) {
                        Some(d) => {
                            let s = String::from_utf8_lossy(&d).into_owned();
                            self.out(s.trim_end_matches('\n'), FG);
                        }
                        None => self.out(&format!("cat: no such file: {path}"), RED),
                    }
                }
            }
            "mkdir" => {
                let path = fs::resolve(&self.cwd, rest);
                if rest.is_empty() || !fs::mkdir(&path) {
                    self.out(&format!("mkdir: cannot create {path}"), RED);
                }
            }
            "rm" => {
                let path = fs::resolve(&self.cwd, rest);
                if rest.is_empty() || !fs::remove(&path) {
                    self.out(&format!("rm: cannot remove {path}"), RED);
                }
            }
            "open" => {
                let target = rest.to_ascii_lowercase();
                let app = super::CATALOG
                    .iter()
                    .filter(|a| a.listed)
                    .find(|a| a.name.to_ascii_lowercase().starts_with(&target) && !target.is_empty());
                if let Some(a) = app {
                    env.requests.push(Request::Open(a.kind));
                } else {
                    let path = fs::resolve(&self.cwd, rest);
                    if fs::read(&path).is_some() {
                        env.requests.push(Request::OpenFile(path));
                    } else {
                        self.out(&format!("open: nothing called '{rest}'"), RED);
                    }
                }
            }
            "uname" => self.out(&format!("WaveOS Aurora {} Tide x86_64", crate::VERSION), FG),
            "uptime" => self.out(&format!("up {}", super::about::format_uptime(env.now_ms)), FG),
            "date" => {
                let d = crate::drivers::rtc::now();
                self.out(
                    &format!("{} {} {} {:02}:{:02}:{:02} {}", d.weekday_name(), d.month_name(), d.day, d.hour, d.minute, d.second, d.year),
                    FG,
                );
            }
            "mem" => {
                let m = crate::mm::stats();
                self.out(
                    &format!(
                        "RAM   {:>6} MiB total  {:>6} MiB used\nHeap  {:>6} MiB total  {:>6} KiB used",
                        m.total_bytes >> 20,
                        m.used_bytes >> 20,
                        m.heap_size >> 20,
                        m.heap_used >> 10
                    ),
                    FG,
                );
            }
            "ps" => {
                self.out("  ID  STATE        CPU(ms)  NAME", DIM);
                for t in crate::sched::list() {
                    let state = match t.state {
                        crate::sched::State::Running => "running",
                        crate::sched::State::Ready => "ready",
                        crate::sched::State::Sleeping(_) => "sleeping",
                        crate::sched::State::Dead => "dead",
                    };
                    self.out(&format!("{:>4}  {:<10} {:>9}  {}", t.id, state, t.cpu_ticks, t.name), FG);
                }
            }
            "neofetch" => self.neofetch(env),
            "theme" => match args.first() {
                Some(&"dark") => env.requests.push(Request::SetDark(true)),
                Some(&"light") => env.requests.push(Request::SetDark(false)),
                _ => self.out("usage: theme light|dark", RED),
            },
            "reboot" | "restart" => env.requests.push(Request::Reboot),
            "shutdown" | "poweroff" => env.requests.push(Request::Shutdown),
            "exit" => env.requests.push(Request::Close),
            "panic" => panic!("panic requested from Terminal (testing the crash screen)"),
            _ => self.out(&format!("{cmd}: command not found (try `help`)"), RED),
        }
    }

    fn neofetch(&mut self, env: &Env) {
        let m = crate::mm::stats();
        let logo = [
            "     ~~~~~~~~      ",
            "   ~~        ~~    ",
            "  ~   ~~~~~~   ~   ",
            " ~  ~~      ~~  ~  ",
            "  ~~   ~~~~   ~~   ",
            "     ~~    ~~      ",
            "   ~~~~~~~~~~~~    ",
        ];
        let info = [
            format!("aurora@waveos"),
            format!("-------------"),
            format!("OS: WaveOS Aurora {}", crate::VERSION),
            format!("Kernel: Tide (hybrid, x86_64)"),
            format!("Uptime: {}", super::about::format_uptime(env.now_ms)),
            format!("Shell: aurora-sh"),
            format!("Resolution: {}x{}", env.screen.0, env.screen.1),
            format!("CPU: {}", crate::arch::cpu::brand()),
            format!("Memory: {} MiB / {} MiB", m.used_bytes >> 20, m.total_bytes >> 20),
        ];
        for i in 0..info.len().max(logo.len()) {
            let l = logo.get(i).copied().unwrap_or("                   ");
            let r = info.get(i).map(|s| s.as_str()).unwrap_or("");
            self.lines.push((format!("{l} {r}"), if i < 2 { VIOLET } else { FG }));
        }
        let _ = CYAN;
    }
}

impl App for Terminal {
    fn kind(&self) -> AppKind {
        AppKind::Terminal
    }
    fn size(&self) -> (i32, i32) {
        (680, 420)
    }
    fn single_instance(&self) -> bool {
        false
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        cv.fill_rect_round_bottom(area, theme::WINDOW_RADIUS, BG);
        let f = theme::mono(14);
        let cw = f.advance('M').max(1);
        let cols = ((area.w - 2 * PAD) / cw).max(10) as usize;
        let rows = ((area.h - 2 * PAD) / LINE_H).max(1) as usize;

        // Wrap scrollback + prompt line into screen rows.
        let mut screen: Vec<(String, u32)> = Vec::new();
        let prompt = self.prompt();
        let input_line = format!("{}{}", prompt, self.input);
        let all = self.lines.iter().map(|(s, c)| (s.as_str(), *c)).chain(core::iter::once((input_line.as_str(), FG)));
        for (s, c) in all {
            let chars: Vec<char> = s.chars().collect();
            if chars.is_empty() {
                screen.push((String::new(), c));
            }
            for chunk in chars.chunks(cols) {
                screen.push((chunk.iter().collect(), c));
            }
        }
        let max_back = screen.len().saturating_sub(rows) as i32;
        self.scroll_back = self.scroll_back.clamp(0, max_back);
        let end = screen.len() - self.scroll_back as usize;
        let start = end.saturating_sub(rows);
        for (i, (s, c)) in screen[start..end].iter().enumerate() {
            let y = area.y + PAD + i as i32 * LINE_H + 14;
            // Colour the prompt of the live input line.
            if start + i + 1 == screen.len() && s.starts_with("aurora:") && self.scroll_back == 0 {
                let pw = cv.text(area.x + PAD, y, &prompt, f, GREEN);
                cv.text(area.x + PAD + pw, y, &s[prompt.len().min(s.len())..], f, *c);
            } else {
                cv.text(area.x + PAD, y, s, f, *c);
            }
        }
        if env.focused && self.caret_on && self.scroll_back == 0 {
            let total = input_line.chars().count();
            let row = (end - start - 1) as i32;
            let col = (total % cols) as i32;
            let row = if total % cols == 0 && total > 0 { row + 1 } else { row };
            cv.fill_rect(Rect::new(area.x + PAD + col * cw, area.y + PAD + row * LINE_H + 2, cw, 16), 0xC07E_E787);
        }
    }

    fn key(&mut self, ev: &KeyEvent, env: &mut Env) -> bool {
        if !ev.pressed {
            return false;
        }
        self.caret_on = true;
        self.last_blink = env.now_ms;
        self.scroll_back = 0;
        if ev.mods.ctrl && matches!(ev.ch, Some('l') | Some('L')) {
            self.lines.clear();
            return true;
        }
        if ev.mods.ctrl && matches!(ev.ch, Some('c') | Some('C')) {
            let line = format!("{}{}^C", self.prompt(), self.input);
            self.out(&line, FG);
            self.input.clear();
            return true;
        }
        match ev.code {
            KeyCode::Enter => {
                let cmd = core::mem::take(&mut self.input);
                let echo = format!("{}{}", self.prompt(), cmd);
                self.out(&echo, FG);
                if !cmd.trim().is_empty() {
                    self.history.push(cmd.clone());
                }
                self.hist_pos = self.history.len();
                self.run(&cmd, env);
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Up if self.hist_pos > 0 => {
                self.hist_pos -= 1;
                self.input = self.history[self.hist_pos].clone();
            }
            KeyCode::Down if self.hist_pos < self.history.len() => {
                self.hist_pos += 1;
                self.input = self.history.get(self.hist_pos).cloned().unwrap_or_default();
            }
            KeyCode::Tab => {}
            _ => match ev.ch {
                Some(c) if !c.is_control() && !ev.mods.ctrl => self.input.push(c),
                _ => return false,
            },
        }
        true
    }

    fn scroll(&mut self, delta: i32, _area: Rect) -> bool {
        self.scroll_back = (self.scroll_back - delta * 3).max(0);
        true
    }

    fn tick(&mut self, env: &mut Env) -> bool {
        if env.focused && env.now_ms - self.last_blink >= 530 {
            self.caret_on = !self.caret_on;
            self.last_blink = env.now_ms;
            return true;
        }
        false
    }
}
