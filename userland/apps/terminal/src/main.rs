//! Terminal — the Aurora shell.
//!
//! Built-in commands (`cd`, `clear`, `help`, `exit`, `theme`, `history`) run
//! inside the shell. Anything else is a program: `/System/Bin/<name>` (or a
//! path), started as its own process with stdout/stderr connected to a pipe
//! that the terminal reads while the program runs. Supports pipelines
//! (`ls | grep txt`), output redirection (`>`, `>>`), command lists
//! (`a && b`, `a || b`, `a; b`) and Ctrl+C.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use aurorakit::canvas::{rgb, Canvas};
use aurorakit::geom::Rect;
use aurorakit::theme;
use aurorakit::{App, Env, KeyCode, KeyEvent, Request};
use corekit::abi::open;
use corekit::fs;
use corekit::process::{self, Stdio};

const BG: u32 = rgb(0x14, 0x16, 0x1D);
const FG: u32 = rgb(0xD8, 0xDE, 0xE9);
const DIM: u32 = rgb(0x7D, 0x86, 0x9A);
const GREEN: u32 = rgb(0x7E, 0xE7, 0x87);
const VIOLET: u32 = rgb(0xA7, 0x8B, 0xFA);
const RED: u32 = rgb(0xFF, 0x7B, 0x72);
const LINE_H: i32 = 19;
const PAD: i32 = 12;
const BIN: &str = "/System/Bin";

/// A running pipeline.
struct Job {
    pids: Vec<u32>,
    /// Read end of the pipe carrying the last stage's output.
    output: u64,
    partial: String,
}

pub struct Terminal {
    lines: Vec<(String, u32)>,
    input: String,
    history: Vec<String>,
    hist_pos: usize,
    scroll_back: i32,
    caret_on: bool,
    last_blink: u64,
    job: Option<Job>,
    /// The rest of a command list, each with the condition that runs it.
    queue: VecDeque<(Then, String)>,
    /// Exit status of the last command (0 = success).
    status: i32,
    /// A job ended: continue the command list on the next tick.
    list_pending: bool,
}

/// How a command in a list depends on the one before it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Then {
    Always,
    IfOk,
    IfFailed,
}

/// Splits a command line on `;`, `&&` and `||` outside quotes.
fn command_list(line: &str) -> VecDeque<(Then, String)> {
    let mut out = VecDeque::new();
    let mut cur = String::new();
    let mut then = Then::Always;
    let mut quote: Option<char> = None;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => {
                quote = None;
                cur.push(c);
            }
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.push(c);
            }
            (None, ';') => {
                out.push_back((then, core::mem::take(&mut cur)));
                then = Then::Always;
            }
            (None, '&') if chars.peek() == Some(&'&') => {
                chars.next();
                out.push_back((then, core::mem::take(&mut cur)));
                then = Then::IfOk;
            }
            (None, '|') if chars.peek() == Some(&'|') => {
                chars.next();
                out.push_back((then, core::mem::take(&mut cur)));
                then = Then::IfFailed;
            }
            (None, c) => cur.push(c),
        }
    }
    out.push_back((then, cur));
    out.retain(|(_, c)| !c.trim().is_empty());
    out
}

impl Terminal {
    pub fn new() -> Self {
        let mut t = Self {
            lines: Vec::new(),
            input: String::new(),
            history: Vec::new(),
            hist_pos: 0,
            scroll_back: 0,
            caret_on: true,
            last_blink: 0,
            job: None,
            queue: VecDeque::new(),
            status: 0,
            list_pending: false,
        };
        let info = process::sys_info();
        t.out(&format!("WaveOS Aurora {} — aurora-sh", process::fixed_str(&info.version, info.version_len)), VIOLET);
        t.out("Type `help` to see what you can do.", DIM);
        t.out("", FG);
        t
    }

    fn out(&mut self, s: &str, color: u32) {
        for line in s.split('\n') {
            self.lines.push((line.to_string(), color));
        }
        if self.lines.len() > 2000 {
            self.lines.drain(..self.lines.len() - 2000);
        }
    }

    fn prompt(&self) -> String {
        if self.job.is_some() {
            return String::new();
        }
        format!("aurora:{}$ ", fs::cwd())
    }

    fn run(&mut self, cmdline: &str, env: &mut Env) {
        self.queue = command_list(cmdline);
        self.status = 0;
        self.run_queue(env);
    }

    /// Runs queued commands until one starts a program (or the list ends).
    fn run_queue(&mut self, env: &mut Env) {
        while self.job.is_none() {
            let Some((then, cmd)) = self.queue.pop_front() else { return };
            let go = match then {
                Then::Always => true,
                Then::IfOk => self.status == 0,
                Then::IfFailed => self.status != 0,
            };
            if go {
                self.run_one(&cmd, env);
            }
        }
    }

    fn run_one(&mut self, cmdline: &str, env: &mut Env) {
        let stages: Vec<Vec<String>> = cmdline.split('|').map(tokenize).collect();
        if stages.iter().any(|s| s.is_empty()) {
            if stages.len() > 1 {
                self.out("aurora-sh: empty command in pipeline", RED);
            }
            self.status = 2;
            return;
        }
        self.status = 0;
        if stages.len() == 1 && self.builtin(&stages[0], env) {
            return;
        }
        if let Err(msg) = self.start(stages) {
            self.out(&msg, RED);
            self.status = 1;
        }
    }

    fn builtin(&mut self, argv: &[String], env: &mut Env) -> bool {
        let arg = |i: usize| argv.get(i).map(String::as_str);
        match argv[0].as_str() {
            "help" => self.out(
                "Built-in:\n  cd DIR · clear · history · theme light|dark · exit\n\nPrograms in /System/Bin (each runs as \
                 its own process):\n  files: ls cat echo mkdir rm mv cp touch grep wc df sync\n  system: ps kill uname \
                 uptime date mem cpuinfo lspci lsusb dmesg battery neofetch\n  sound: play volume\n  network: ping \
                 ifconfig nslookup fetch (http and https)\n\nAlso: open APP|FILE · shutdown · reboot\nPipelines and \
                 redirection: ls | grep txt, echo hi > note.txt, cmd >> log\nLists: a && b (b if a worked), a || b (b \
                 if a failed), a; b\nCtrl+C stops the running program, Ctrl+L clears the screen.",
                FG,
            ),
            "clear" => self.lines.clear(),
            "exit" => env.requests.push(Request::Close),
            "cd" => {
                let target = fs::resolve(&fs::cwd(), arg(1).unwrap_or("/"));
                if let Err(e) = fs::chdir(&target) {
                    self.out(&format!("cd: {target}: {e}"), RED);
                    self.status = 1;
                }
            }
            "history" => {
                let h: Vec<String> =
                    self.history.iter().enumerate().map(|(i, c)| format!("{:>4}  {}", i + 1, c)).collect();
                self.out(&h.join("\n"), FG);
            }
            "theme" => match arg(1) {
                Some("dark") => env.requests.push(Request::SetDark(true)),
                Some("light") => env.requests.push(Request::SetDark(false)),
                _ => self.out("usage: theme light|dark", RED),
            },
            "open" => match arg(1) {
                None => self.out("usage: open APP|FILE", RED),
                Some(target) => {
                    let path = fs::resolve(&fs::cwd(), target);
                    let is_file = fs::stat(&path).is_ok_and(|s| s.kind == corekit::abi::KIND_FILE);
                    if is_file && (path.ends_with(".elf") || path.starts_with("/System/Bin/")) {
                        env.requests.push(Request::OpenApp(path));
                    } else if is_file {
                        env.requests.push(Request::OpenFile(path));
                    } else {
                        let mut name = String::from(target);
                        if let Some(c) = name.get_mut(0..1) {
                            c.make_ascii_uppercase();
                        }
                        if fs::exists(&format!("/System/Apps/{name}.elf")) {
                            env.requests.push(Request::OpenApp(name));
                        } else {
                            self.out(&format!("open: no app or file called '{target}'"), RED);
                        }
                    }
                }
            },
            "shutdown" | "poweroff" => env.requests.push(Request::Shutdown),
            "reboot" | "restart" => env.requests.push(Request::Reboot),
            _ => return false,
        }
        true
    }

    /// Starts a pipeline of programs.
    fn start(&mut self, mut stages: Vec<Vec<String>>) -> Result<(), String> {
        // Output redirection on the last stage.
        let mut redirect: Option<fs::File> = None;
        let last = stages.last_mut().unwrap();
        if let Some(pos) = last.iter().position(|t| t == ">" || t == ">>") {
            let append = last[pos] == ">>";
            let target = last.get(pos + 1).ok_or("aurora-sh: missing file after >")?.clone();
            let path = fs::resolve(&fs::cwd(), &target);
            let flags = open::WRITE | open::CREATE | if append { open::APPEND } else { open::TRUNCATE };
            redirect = Some(fs::File::open(&path, flags).map_err(|e| format!("aurora-sh: {target}: {e}"))?);
            last.truncate(pos);
        }

        let [out_r, out_w] = process::pipe(true).map_err(|e| format!("aurora-sh: pipe: {e}"))?;
        let mut pids = Vec::new();
        let mut prev_read: Option<u64> = None;
        let n = stages.len();
        for (i, argv) in stages.iter().enumerate() {
            let program =
                if argv[0].contains('/') { fs::resolve(&fs::cwd(), &argv[0]) } else { format!("{BIN}/{}", argv[0]) };
            let (stdout, next_read) = if i + 1 < n {
                let [r, w] = process::pipe(false).map_err(|e| format!("aurora-sh: pipe: {e}"))?;
                (w, Some(r))
            } else {
                (redirect.as_ref().map(|f| f.fd()).unwrap_or(out_w), None)
            };
            let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
            let res =
                process::spawn(&program, &args, Stdio { stdin: prev_read, stdout: Some(stdout), stderr: Some(out_w) });
            // The children hold their own references now.
            if let Some(r) = prev_read {
                process::close(r);
            }
            if i + 1 < n {
                process::close(stdout);
            }
            prev_read = next_read;
            match res {
                Ok(pid) => pids.push(pid),
                Err(e) if e.is(corekit::abi::err::ENOENT) => {
                    self.out(&format!("{}: command not found (try `help`)", argv[0]), RED);
                    self.status = 127;
                }
                Err(e) => {
                    self.out(&format!("{}: {e}", argv[0]), RED);
                    self.status = 126;
                }
            }
        }
        if let Some(r) = prev_read {
            process::close(r);
        }
        process::close(out_w);
        drop(redirect);
        if pids.is_empty() {
            process::close(out_r);
        } else {
            self.job = Some(Job { pids, output: out_r, partial: String::new() });
        }
        Ok(())
    }

    /// Moves program output into the scrollback; finishes the job at EOF.
    fn poll_job(&mut self) -> bool {
        let Some(job) = self.job.as_mut() else { return false };
        let mut buf = [0u8; 4096];
        let mut changed = false;
        let mut eof = false;
        for _ in 0..16 {
            match corekit::io::read_fd(job.output, &mut buf) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    job.partial.push_str(&String::from_utf8_lossy(&buf[..n]));
                    changed = true;
                }
                Err(_) => break, // EAGAIN: nothing yet
            }
        }
        if changed {
            let text = core::mem::take(&mut job.partial);
            match text.rfind('\n') {
                Some(i) => {
                    self.job.as_mut().unwrap().partial = text[i + 1..].to_string();
                    self.out(&text[..i], FG);
                }
                None => self.job.as_mut().unwrap().partial = text,
            }
        }
        if eof {
            let job = self.job.take().unwrap();
            if !job.partial.is_empty() {
                self.out(&job.partial, FG);
            }
            process::close(job.output);
            // A list continues after the last stage; its status decides && and ||.
            let last = job.pids.len().saturating_sub(1);
            for (k, pid) in job.pids.iter().enumerate() {
                let code = process::wait(*pid, Some(2000));
                match code {
                    Ok(0) => {}
                    Ok(-11) => self.out("[program crashed]", RED),
                    Ok(-9) => {
                        self.out("^C", DIM);
                        self.queue.clear();
                    }
                    Ok(code) if self.queue.is_empty() => self.out(&format!("[exit code {code}]"), DIM),
                    _ => {}
                }
                if k == last {
                    self.status = code.map_or(1, |c| c as i32);
                }
            }
            self.list_pending = true;
            return true;
        }
        changed
    }
}

/// Splits a command line into words, honouring single and double quotes.
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    out.push(core::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, '>') => {
                if !cur.is_empty() && cur != ">" {
                    out.push(core::mem::take(&mut cur));
                }
                cur.push('>');
                has = true;
            }
            (None, c) => {
                if cur.starts_with('>') {
                    out.push(core::mem::take(&mut cur));
                }
                cur.push(c);
                has = true;
            }
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

impl App for Terminal {
    fn title(&self) -> String {
        format!("Terminal — {}", fs::cwd())
    }
    fn size(&self) -> (i32, i32) {
        (680, 420)
    }

    fn draw(&mut self, cv: &mut Canvas, area: Rect, env: &Env) {
        cv.fill_rect(area, BG);
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
            if start + i + 1 == screen.len() && !prompt.is_empty() && s.starts_with("aurora:") && self.scroll_back == 0
            {
                let pw = cv.text(area.x + PAD, y, &prompt, f, GREEN);
                cv.text(area.x + PAD + pw, y, s.get(prompt.len()..).unwrap_or(""), f, *c);
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
            if let Some(job) = &self.job {
                for pid in &job.pids {
                    let _ = process::kill(*pid);
                }
            } else {
                let line = format!("{}{}^C", self.prompt(), self.input);
                self.out(&line, FG);
                self.input.clear();
            }
            return true;
        }
        if self.job.is_some() {
            return false; // no stdin forwarding yet
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
            KeyCode::Tab => self.complete(),
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
        let mut changed = self.poll_job();
        if core::mem::take(&mut self.list_pending) {
            self.run_queue(env);
        }
        if env.focused && env.now_ms - self.last_blink >= 530 {
            self.caret_on = !self.caret_on;
            self.last_blink = env.now_ms;
            changed = true;
        }
        changed
    }

    fn tick_interval(&self) -> u64 {
        if self.job.is_some() {
            20
        } else {
            100
        }
    }
}

impl Terminal {
    /// Tab completion for program names and paths.
    fn complete(&mut self) {
        let start = self.input.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let word = self.input[start..].to_string();
        let (dir, prefix) = if start == 0 && !word.contains('/') {
            (String::from(BIN), word.clone())
        } else {
            match word.rfind('/') {
                Some(i) => (fs::resolve(&fs::cwd(), if i == 0 { "/" } else { &word[..i] }), word[i + 1..].to_string()),
                None => (fs::cwd(), word.clone()),
            }
        };
        let Ok(entries) = fs::read_dir(&dir) else { return };
        let matches: Vec<&fs::Entry> = entries.iter().filter(|e| e.name.starts_with(&prefix)).collect();
        if matches.len() == 1 {
            let e = matches[0];
            self.input.push_str(&e.name[prefix.len()..]);
            self.input.push(if e.is_dir { '/' } else { ' ' });
        } else if matches.len() > 1 {
            let names: Vec<&str> = matches.iter().map(|e| e.name.as_str()).collect();
            let line = format!("{}{}", self.prompt(), self.input);
            self.out(&line, FG);
            self.out(&names.join("  "), DIM);
        }
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let _ = fs::chdir("/Documents");
    aurorakit::run(Terminal::new())
}
