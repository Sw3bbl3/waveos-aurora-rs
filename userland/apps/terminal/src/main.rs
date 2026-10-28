//! Terminal — the Aurora shell.
//!
//! Built-in commands (`cd`, `clear`, `help`, `exit`, `theme`, `history`) run
//! inside the shell. Anything else is a program: `/System/Bin/<name>` (or a
//! path), started as its own process with stdout/stderr connected to a pipe
//! that the terminal reads while the program runs. What you type while a
//! program runs goes to its stdin a line at a time (Ctrl+D ends the input).
//! Supports pipelines (`ls | grep txt`), output redirection (`>`, `>>`),
//! command lists (`a && b`, `a || b`, `a; b`) and Ctrl+C.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::Cell;
use corekit::abi::open;
use corekit::fs;
use corekit::process::{self, Stdio};
use waveui::lumen::canvas::{rgb, Canvas as Surface};
use waveui::lumen::geom::Rect;
use waveui::lumen::theme;
use waveui::prelude::*;
use waveui::Request;

// Lines keep a role, not a colour, so they follow the appearance.
const FG: u32 = 0;
const DIM: u32 = 1;
const GREEN: u32 = 2;
const VIOLET: u32 = 3;
const RED: u32 = 4;
/// A command as typed after its prompt: `alex@aurora /Documents ❯ ls`.
const ECHO: u32 = 5;
const BLUE: u32 = 6;
const LINE_H: i32 = 21;
const PAD: i32 = 20;
/// Ends the prompt.
const MARK: &str = "❯ ";

/// The colour of a role in the current appearance.
fn ink(role: u32) -> u32 {
    let dark = theme::current().dark;
    match (role, dark) {
        (DIM, true) => rgb(0x7D, 0x86, 0x9A),
        (DIM, false) => rgb(0x6B, 0x72, 0x84),
        (GREEN, true) => rgb(0x34, 0xD3, 0x99),
        (GREEN, false) => rgb(0x0F, 0x8A, 0x5F),
        (VIOLET, true) => rgb(0xA7, 0x8B, 0xFA),
        (VIOLET, false) => rgb(0x7C, 0x3A, 0xED),
        (RED, true) => rgb(0xFF, 0x7B, 0x72),
        (RED, false) => rgb(0xD1, 0x24, 0x2F),
        (BLUE, true) => rgb(0x7A, 0xA2, 0xF7),
        (BLUE, false) => rgb(0x1D, 0x4E, 0xD8),
        _ => theme::current().text,
    }
}

/// The short name of the account ("alex" for Alex Rivera).
fn user_name() -> String {
    let full = corekit::prefs::get("account_name").unwrap_or_default();
    let first = full.split_whitespace().next().unwrap_or("user").to_lowercase();
    first.chars().filter(|c| c.is_alphanumeric()).collect::<String>()
}
const BIN: &str = "/System/Bin";
/// Where commands are looked for: WaveOS's own, then Linux programs.
const PATH: [&str; 4] = [BIN, "/System/Linux/bin", "/System/Linux/usr/bin", "/Linux/usr/local/bin"];

/// Tabs as spaces to the next multiple of eight columns.
fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    let mut out = String::new();
    for c in line.chars() {
        if c == '\t' {
            let n = 8 - out.chars().count() % 8;
            out.extend(core::iter::repeat_n(' ', n));
        } else {
            out.push(c);
        }
    }
    out
}

/// The program a command name runs (WaveOS's own first).
fn find_program(name: &str) -> String {
    PATH.iter().map(|dir| format!("{dir}/{name}")).find(|p| fs::exists(p)).unwrap_or_else(|| format!("{BIN}/{name}"))
}

/// A running pipeline.
struct Job {
    pids: Vec<u32>,
    /// Read end of the pipe carrying the last stage's output.
    output: u64,
    partial: String,
    /// Write end of the first stage's stdin (closed by Ctrl+D).
    stdin: Option<u64>,
}

/// One shell, in a tab.
pub struct Session {
    /// The working directory (the process has one; each tab keeps its own).
    cwd: String,
    user: String,
    /// The program a running job started with (the tab's name meanwhile).
    running: String,
    lines: Vec<(String, u32)>,
    input: String,
    history: Vec<String>,
    hist_pos: usize,
    scroll_back: Cell<i32>,
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

impl Session {
    fn new() -> Self {
        let mut t = Self {
            cwd: fs::cwd(),
            user: user_name(),
            running: String::new(),
            lines: Vec::new(),
            input: String::new(),
            history: Vec::new(),
            hist_pos: 0,
            scroll_back: Cell::new(0),
            caret_on: true,
            last_blink: 0,
            job: None,
            queue: VecDeque::new(),
            status: 0,
            list_pending: false,
        };
        let info = process::sys_info();
        t.out(
            &format!(
                "WaveOS Aurora {} · aurora-sh · type `help` to see what you can do",
                process::fixed_str(&info.version, info.version_len)
            ),
            DIM,
        );
        t.out("", FG);
        t
    }

    fn out(&mut self, s: &str, color: u32) {
        for line in s.split('\n') {
            self.lines.push((expand_tabs(line.trim_end_matches('\r')), color));
        }
        if self.lines.len() > 2000 {
            self.lines.drain(..self.lines.len() - 2000);
        }
    }

    fn prompt(&self) -> String {
        if let Some(job) = &self.job {
            // A program's unfinished line (like a `> ` prompt) leads the input.
            return job.partial.clone();
        }
        format!("{}@aurora {} {MARK}", self.user, fs::cwd())
    }

    fn run(&mut self, cmdline: &str, env: &mut Vec<Request>) {
        self.queue = command_list(cmdline);
        self.status = 0;
        self.run_queue(env);
    }

    /// Runs queued commands until one starts a program (or the list ends).
    fn run_queue(&mut self, env: &mut Vec<Request>) {
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

    /// What the tab shows: the running program, or the folder.
    fn label(&self) -> String {
        if self.job.is_some() && !self.running.is_empty() {
            return self.running.clone();
        }
        match fs::file_name(&self.cwd) {
            "" => String::from("/"),
            name => String::from(name),
        }
    }

    fn run_one(&mut self, cmdline: &str, env: &mut Vec<Request>) {
        self.running = String::from(cmdline.split_whitespace().next().unwrap_or(""));
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

    fn builtin(&mut self, argv: &[String], env: &mut Vec<Request>) -> bool {
        let arg = |i: usize| argv.get(i).map(String::as_str);
        match argv[0].as_str() {
            "help" => self.out(
                "Built-in:\n  cd DIR · clear · history · theme light|dark · exit\n\nPrograms in /System/Bin (each runs as \
                 its own process):\n  files: ls cat echo mkdir rm mv cp touch grep wc df sync\n  system: ps kill uname \
                 uptime date mem cpuinfo lspci lsusb dmesg battery neofetch\n  sound: play volume\n  network: ping \
                 ifconfig nslookup fetch (http and https)\n\nLinux programs in /System/Linux/bin run too: \
                 busybox ls, busybox vi, busybox awk…\n\nAlso: open APP|FILE · shutdown · reboot\nPipelines and \
                 redirection: ls | grep txt, echo hi > note.txt, cmd >> log\nLists: a && b (b if a worked), a || b (b \
                 if a failed), a; b\nCtrl+C stops the running program, Ctrl+L clears the screen.",
                FG,
            ),
            "clear" => self.lines.clear(),
            "exit" => env.push(Request::Close),
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
                Some("dark") => env.push(Request::SetDark(true)),
                Some("light") => env.push(Request::SetDark(false)),
                _ => self.out("usage: theme light|dark", RED),
            },
            "open" => match arg(1) {
                None => self.out("usage: open APP|FILE", RED),
                Some(target) => {
                    let path = fs::resolve(&fs::cwd(), target);
                    let is_file = fs::stat(&path).is_ok_and(|s| s.kind == corekit::abi::KIND_FILE);
                    if is_file && (path.ends_with(".elf") || path.starts_with("/System/Bin/")) {
                        env.push(Request::OpenApp(path));
                    } else if is_file {
                        env.push(Request::OpenFile(path));
                    } else {
                        let mut name = String::from(target);
                        if let Some(c) = name.get_mut(0..1) {
                            c.make_ascii_uppercase();
                        }
                        if fs::exists(&format!("/System/Apps/{name}.elf")) {
                            env.push(Request::OpenApp(name));
                        } else {
                            self.out(&format!("open: no app or file called '{target}'"), RED);
                        }
                    }
                }
            },
            "shutdown" | "poweroff" => env.push(Request::Shutdown),
            "reboot" | "restart" => env.push(Request::Reboot),
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
        // The first stage reads what is typed while it runs.
        let [in_r, in_w] = process::pipe(false).map_err(|e| format!("aurora-sh: pipe: {e}"))?;
        let mut prev_read: Option<u64> = Some(in_r);
        let n = stages.len();
        for (i, argv) in stages.iter().enumerate() {
            let program =
                if argv[0].contains('/') { fs::resolve(&fs::cwd(), &argv[0]) } else { find_program(&argv[0]) };
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
            process::close(in_w);
        } else {
            self.job = Some(Job { pids, output: out_r, partial: String::new(), stdin: Some(in_w) });
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
            if let Some(w) = job.stdin {
                process::close(w);
            }
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
                    self.status = code.unwrap_or(1);
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

#[derive(Clone)]
pub enum Msg {
    Key(KeyEvent),
    Scroll(i32),
    Tab(usize),
    NewTab,
    CloseTab(usize),
}

impl Session {
    fn draw(&self, cv: &mut Surface, area: Rect, active: bool) {
        let f = theme::mono(13);
        let cw = f.advance('M').max(1);
        let cols = ((area.w - 2 * PAD) / cw).max(10) as usize;
        let rows = ((area.h - 2 * PAD) / LINE_H).max(1) as usize;

        // Wrap scrollback + prompt line into screen rows.
        let mut screen: Vec<(String, u32)> = Vec::new();
        let prompt = self.prompt();
        let input_line = format!("{}{}", prompt, self.input);
        let live = if self.job.is_some() { FG } else { ECHO };
        let all = self.lines.iter().map(|(s, c)| (s.as_str(), *c)).chain(core::iter::once((input_line.as_str(), live)));
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
        let back = self.scroll_back.get().clamp(0, max_back);
        self.scroll_back.set(back);
        let end = screen.len() - back as usize;
        let start = end.saturating_sub(rows);
        for (i, (s, c)) in screen[start..end].iter().enumerate() {
            let y = area.y + PAD + i as i32 * LINE_H + 15;
            let x = area.x + PAD;
            match s.find(MARK).filter(|_| *c == ECHO) {
                Some(m) => {
                    // user@aurora in blue, the folder dim, the mark violet.
                    let user = s[..m].find(' ').unwrap_or(0);
                    let mut x = x + cv.text(x, y, &s[..user], f, ink(BLUE));
                    x += cv.text(x, y, &s[user..m], f, ink(DIM));
                    x += cv.text(x, y, MARK, f, ink(VIOLET));
                    cv.text(x, y, &s[m + MARK.len()..], f, ink(FG));
                }
                None => {
                    cv.text(x, y, s, f, ink(*c));
                }
            }
        }
        if active && self.caret_on && back == 0 {
            let total = input_line.chars().count();
            let row = (end - start - 1) as i32;
            let col = (total % cols) as i32;
            let row = if total % cols == 0 && total > 0 { row + 1 } else { row };
            cv.fill_rect(Rect::new(area.x + PAD + col * cw, area.y + PAD + row * LINE_H + 2, cw, 17), theme::accent());
        }
    }

    fn key(&mut self, ev: &KeyEvent, now: u64, env: &mut Vec<Request>) -> bool {
        if !ev.pressed {
            return false;
        }
        self.caret_on = true;
        self.last_blink = now;
        self.scroll_back.set(0);
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
                self.out(&line, ECHO);
                self.input.clear();
            }
            return true;
        }
        if self.job.is_some() {
            return self.job_key(ev);
        }
        match ev.code {
            KeyCode::Enter => {
                let cmd = core::mem::take(&mut self.input);
                let echo = format!("{}{}", self.prompt(), cmd);
                self.out(&echo, ECHO);
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
}

/// The window: a row of tabs, each a shell.
pub struct Terminal {
    tabs: Vec<Session>,
    active: usize,
}

impl Terminal {
    fn new() -> Self {
        Self { tabs: vec![Session::new()], active: 0 }
    }

    /// Runs `f` on tab `i` inside its own working directory.
    fn with_tab<R>(&mut self, i: usize, f: impl FnOnce(&mut Session) -> R) -> R {
        let s = &mut self.tabs[i];
        let _ = fs::chdir(&s.cwd);
        let r = f(s);
        s.cwd = fs::cwd();
        r
    }

    fn requests(&mut self, i: usize, requests: Vec<Request>, cx: &mut Context<Msg>) {
        for r in requests {
            match r {
                // `exit` closes its tab; the window goes with the last one.
                Request::Close if self.tabs.len() > 1 => self.close_tab(i),
                r => cx.request(r),
            }
        }
    }

    fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if let Some(job) = &self.tabs[i].job {
            for pid in &job.pids {
                let _ = process::kill(*pid);
            }
        }
        self.tabs.remove(i);
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1);
        }
        let _ = fs::chdir(&self.tabs[self.active].cwd);
    }

    fn tab_bar(&self) -> View<'_, Msg> {
        let mut bar = HStack::new().spacing(4);
        for (i, t) in self.tabs.iter().enumerate() {
            let on = i == self.active;
            let tab = HStack::new()
                .push(Image::symbol(Symbol::Terminal).symbol_size(14))
                .push(Text::new(t.label()).style(TextStyle::Sized(13, waveui::style::Weight::Bold)))
                .spacing(8)
                .padding_xy(12, 0)
                .height(30)
                .foreground(if on { Color::Label } else { Color::SecondaryLabel })
                .on_tap(Msg::Tab(i))
                .context_menu(vec![MenuItem::new("New Tab", Msg::NewTab), MenuItem::new("Close Tab", Msg::CloseTab(i))])
                .help(t.cwd.clone());
            bar = bar.push(if on { tab.background_rounded(Color::Fill, 15) } else { tab });
        }
        bar = bar.push(
            Button::symbol(Symbol::Plus)
                .action(Msg::NewTab)
                .shortcut(Shortcut::ctrl('t'))
                .foreground(Color::SecondaryLabel)
                .help("New Tab (Ctrl+T)"),
        );
        VStack::new().push(bar.padding_xy(12, 0).height(48).fill_width()).push(Divider::new()).spacing(0).fill_width()
    }
}

impl App for Terminal {
    type Message = Msg;

    fn title(&self) -> String {
        format!("Terminal — {}", self.tabs[self.active].cwd)
    }
    fn size(&self) -> (i32, i32) {
        (1000, 680)
    }

    fn update(&mut self, msg: Msg, cx: &mut Context<Msg>) {
        let now = cx.now();
        match msg {
            Msg::Key(ev) => {
                if ev.pressed && ev.mods.ctrl && ev.code == KeyCode::Tab && self.tabs.len() > 1 {
                    let n = self.tabs.len();
                    self.active = if ev.mods.shift { (self.active + n - 1) % n } else { (self.active + 1) % n };
                    let _ = fs::chdir(&self.tabs[self.active].cwd);
                    return;
                }
                if ev.pressed && ev.mods.ctrl && ev.mods.shift && matches!(ev.ch, Some('w' | 'W')) {
                    cx.send(Msg::CloseTab(self.active));
                    return;
                }
                let i = self.active;
                let mut requests = Vec::new();
                self.with_tab(i, |s| s.key(&ev, now, &mut requests));
                self.requests(i, requests, cx);
            }
            Msg::Scroll(lines) => {
                let s = &self.tabs[self.active];
                s.scroll_back.set((s.scroll_back.get() - lines).max(0));
            }
            Msg::Tab(i) => {
                self.active = i.min(self.tabs.len() - 1);
                let _ = fs::chdir(&self.tabs[self.active].cwd);
                cx.focus("terminal");
            }
            Msg::NewTab => {
                let _ = fs::chdir(&self.tabs[self.active].cwd);
                self.tabs.push(Session::new());
                self.active = self.tabs.len() - 1;
                cx.focus("terminal");
            }
            Msg::CloseTab(i) => {
                if self.tabs.len() > 1 {
                    self.close_tab(i);
                } else {
                    cx.close();
                }
            }
        }
    }

    fn poll_interval(&self) -> Option<u64> {
        Some(if self.tabs.iter().any(|t| t.job.is_some()) { 20 } else { 100 })
    }

    fn poll(&mut self, cx: &mut Context<Msg>) -> bool {
        let mut changed = false;
        for i in 0..self.tabs.len() {
            if self.tabs[i].job.is_none() && !self.tabs[i].list_pending {
                continue;
            }
            let mut requests = Vec::new();
            changed |= self.with_tab(i, |s| {
                let mut changed = s.poll_job();
                if core::mem::take(&mut s.list_pending) {
                    s.run_queue(&mut requests);
                    changed = true;
                }
                changed
            });
            self.requests(i, requests, cx);
        }
        let _ = fs::chdir(&self.tabs[self.active].cwd);
        let now = cx.now();
        let s = &mut self.tabs[self.active];
        if now - s.last_blink >= 530 {
            s.caret_on = !s.caret_on;
            s.last_blink = now;
            changed = true;
        }
        changed
    }

    fn view(&self, env: &Env) -> View<'_, Msg> {
        let active = env.active;
        let s = &self.tabs[self.active];
        VStack::new()
            .push(self.tab_bar())
            .push(
                Canvas::new(move |cv, r| s.draw(cv, r, active))
                    .on_input(|e| match e {
                        CanvasEvent::Key(k) => Some(Msg::Key(k)),
                        CanvasEvent::Scroll { dy, .. } if dy != 0 => Some(Msg::Scroll(dy / 16)),
                        _ => None,
                    })
                    .focusable()
                    .focused()
                    .id("terminal")
                    .fill(),
            )
            .spacing(0)
            .fill()
    }
}

impl Session {
    /// A key while a program runs: line editing, then the line goes to its stdin.
    fn job_key(&mut self, ev: &KeyEvent) -> bool {
        let Some(job) = self.job.as_mut() else { return false };
        if ev.mods.ctrl && matches!(ev.ch, Some('d') | Some('D')) {
            if let Some(w) = job.stdin.take() {
                process::close(w);
            }
            return true;
        }
        match ev.code {
            KeyCode::Enter => {
                let line = core::mem::take(&mut self.input);
                let echo = format!("{}{}", core::mem::take(&mut job.partial), line);
                if let Some(w) = job.stdin {
                    let _ = corekit::io::write_fd(w, format!("{line}\n").as_bytes());
                }
                self.out(&echo, FG);
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            _ => match ev.ch {
                Some(c) if !c.is_control() && !ev.mods.ctrl => self.input.push(c),
                _ => return false,
            },
        }
        true
    }

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
            self.out(&line, ECHO);
            self.out(&names.join("  "), DIM);
        }
    }
}

corekit::entry!(main);

fn main(_: corekit::Args) -> i32 {
    let _ = fs::chdir("/Documents");
    waveui::run(Terminal::new())
}
