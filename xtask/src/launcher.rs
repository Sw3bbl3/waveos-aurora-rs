//! `cargo xtask launch` (`make run`): a page in the browser to choose how to
//! run WaveOS, then build and boot it with the output streamed live.
//!
//! ```text
//!  browser ──HTTP──> 127.0.0.1:<random port> ──spawns──> xtask run <args> ──> cargo build, QEMU
//!     ^                                                        │ stdout + stderr
//!     └──────────────── SSE /events (replayed, then live) <────┘
//! ```
//!
//! The launcher runs `xtask run` (or `test`) with exactly the arguments the
//! page shows as `cargo xtask run …`, so what you see is what you'd type.
//! Std only: a tiny HTTP server like the System Explorer's (monitor.rs).
//!
//! Lifetime: closing the page leaves WaveOS running. The launcher keeps
//! going while WaveOS runs and exits by itself once WaveOS has stopped and
//! no page has been open for half a minute. Ctrl-C in the terminal stops
//! everything: the launcher, the build and QEMU share the terminal's process
//! group, so they all get the interrupt (QEMU shuts down cleanly on it).
//! The page's Stop button sends SIGTERM to the run and everything it started.
//!
//! `cargo xtask launch --last` (`make quick`) boots the last configuration
//! launched from the page, in the terminal, without the page.
//!
//! Presets and the last configuration are kept in `.waveos-launcher.json` at
//! the top of the repository (git-ignored, and not in target/ so that
//! `cargo clean` keeps them).

use crate::displays::Displays;
use crate::arch::Arch;
use crate::runconfig::{self, Action, QemuCaps, RunConfig};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{exit, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PAGE: &str = include_str!("../../docs/launcher/index.html");
const MARK: &[u8] = include_bytes!("../../assets/brand/wayv-mark.png");
/// The desktop's Tide wallpaper, small: the page's background.
const WALLPAPER: &[u8] = include_bytes!("../../assets/wallpaper-thumbs/tide.jpg");
const MAX_HISTORY: usize = 20000;
/// How long the launcher waits for a page to come back before exiting.
const IDLE_EXIT: Duration = Duration::from_secs(30);

/// A line on stdout that never panics: the terminal may be gone while
/// WaveOS keeps running.
macro_rules! say {
    ($($t:tt)*) => {{
        let mut out = std::io::stdout();
        let _ = writeln!(out, $($t)*);
        let _ = out.flush();
    }};
}

pub fn main(args: &[String]) {
    let flag = |f: &str| args.iter().any(|a| a == f);
    if flag("--last") {
        boot_last();
    }
    let port = match args.iter().position(|a| a == "--port") {
        None => 0,
        Some(i) => args.get(i + 1).and_then(|p| p.parse().ok()).unwrap_or_else(|| {
            eprintln!("--port needs a port number");
            exit(2)
        }),
    };
    serve(port, !flag("--no-open"));
}

// ---- Presets and the last configuration ------------------------------------

/// `.waveos-launcher.json`, or `WAVEOS_LAUNCHER_STORE` (the tests use their own).
fn store_path() -> PathBuf {
    std::env::var_os("WAVEOS_LAUNCHER_STORE")
        .map(PathBuf::from)
        .unwrap_or_else(|| crate::root().join(".waveos-launcher.json"))
}

#[derive(Default, Debug, PartialEq)]
pub struct Store {
    pub last: Option<RunConfig>,
    pub presets: Vec<(String, RunConfig)>,
}

impl Store {
    pub fn from_json(v: &Value) -> Store {
        let last = v.get("last").and_then(|l| RunConfig::from_json(l).ok());
        let presets = v
            .get("presets")
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(|p| {
                        let name = p.get("name")?.as_str()?.to_string();
                        Some((name, RunConfig::from_json(p.get("config")?).ok()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Store { last, presets }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "version": 1,
            "last": self.last.as_ref().map(RunConfig::to_json),
            "presets": self.presets.iter().map(|(n, c)| json!({"name": n, "config": c.to_json()})).collect::<Vec<_>>(),
        })
    }

    fn load() -> Store {
        std::fs::read_to_string(store_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .map(|v| Store::from_json(&v))
            .unwrap_or_default()
    }

    fn save(&self) -> Result<(), String> {
        let text = serde_json::to_string_pretty(&self.to_json()).map_err(|e| e.to_string())?;
        std::fs::write(store_path(), text + "\n").map_err(|e| format!("could not save {}: {e}", store_path().display()))
    }

    /// Adds or replaces preset `name`.
    pub fn put(&mut self, name: &str, config: RunConfig) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() || name.len() > 60 {
            return Err("a preset needs a name of 1 to 60 characters".into());
        }
        if runconfig::builtin_presets().iter().any(|(n, _, _)| n.eq_ignore_ascii_case(name)) {
            return Err(format!("\"{name}\" is a built-in preset; choose another name"));
        }
        match self.presets.iter_mut().find(|(n, _)| n == name) {
            Some(p) => p.1 = config,
            None => self.presets.push((name.to_string(), config)),
        }
        Ok(())
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        let config = self.presets.iter().find(|(n, _)| n == from).map(|p| p.1.clone()).ok_or("no such preset")?;
        if from != to.trim() && self.presets.iter().any(|(n, _)| n == to.trim()) {
            return Err(format!("there's already a preset called \"{}\"", to.trim()));
        }
        self.put(to, config)?;
        if from != to.trim() {
            self.presets.retain(|(n, _)| n != from);
        }
        Ok(())
    }

    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let before = self.presets.len();
        self.presets.retain(|(n, _)| n != name);
        if self.presets.len() == before {
            Err("no such preset".into())
        } else {
            Ok(())
        }
    }

    /// Remembers `config` as the last one launched. A fresh disk is a
    /// one-off: `make quick` must never erase the disk again.
    pub fn remember(&mut self, config: &RunConfig) {
        self.last = Some(RunConfig { fresh_disk: false, ..config.clone() });
    }
}

/// `--last`: boots the last configuration in this terminal and exits with its status.
fn boot_last() -> ! {
    let store = Store::load();
    let config = store.last.unwrap_or_else(|| {
        eprintln!("No configuration launched from `make run` yet: booting the defaults.");
        RunConfig::default()
    });
    eprintln!("{}", config.command_line());
    let status = xtask_command(&config).status().unwrap_or_else(|e| {
        eprintln!("could not start xtask: {e}");
        exit(1)
    });
    exit(status.code().unwrap_or(1));
}

/// `xtask run|test <args>`: this very program, with the arguments the page shows.
fn xtask_command(config: &RunConfig) -> Command {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("xtask"));
    let mut cmd = Command::new(exe);
    // The configuration says which processor (`--arch` when it isn't the
    // default), not the architecture this launcher was started with.
    cmd.current_dir(crate::root()).arg(config.subcommand()).args(config.args()).env_remove("AURORA_ARCH");
    cmd
}

// ---- The running WaveOS -----------------------------------------------------

#[derive(Default)]
struct Hub {
    history: VecDeque<String>,
    subscribers: Vec<Sender<String>>,
}

impl Hub {
    fn publish(&mut self, ev: Value) {
        let line = ev.to_string();
        self.subscribers.retain(|s| s.send(line.clone()).is_ok());
        self.history.push_back(line);
        if self.history.len() > MAX_HISTORY {
            // Keep the run's opening event.
            self.history.remove(1);
        }
    }
}

#[derive(Default)]
struct Run {
    pid: Option<u32>,
    generation: u64,
    state: String,
    config: Option<RunConfig>,
    command: String,
    stopping: bool,
    /// What Stop signalled: the run and everything under it.
    stopped: Vec<u32>,
}

struct Launcher {
    /// What each architecture's QEMU offers (x86_64 first).
    caps: Vec<(Arch, QemuCaps)>,
    hub: Mutex<Hub>,
    run: Mutex<Run>,
    store: Mutex<Store>,
    port: u16,
    /// Open pages (event streams), whether one ever connected, and when the last one left.
    pages: AtomicUsize,
    page_seen: AtomicBool,
    page_left: Mutex<Instant>,
}

impl Launcher {
    fn caps(&self, arch: Arch) -> &QemuCaps {
        self.caps.iter().find(|(a, _)| *a == arch).map(|(_, c)| c).unwrap_or(&self.caps[0].1)
    }

    fn state(&self, state: &str, message: &str) {
        self.run.lock().unwrap().state = state.into();
        self.hub.lock().unwrap().publish(json!({"ev": "state", "state": state, "message": message}));
        if !message.is_empty() {
            say!("launcher: {message}");
        }
    }

    fn launch(self: &Arc<Self>, config: RunConfig) -> Result<String, String> {
        config.validate()?;
        let mut run = self.run.lock().unwrap();
        if run.pid.is_some() {
            return Err("WaveOS is already running: stop it first".into());
        }
        let command = config.command_line();
        let mut cmd = xtask_command(&config);
        cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| format!("could not start xtask: {e}"))?;
        run.generation += 1;
        run.pid = Some(child.id());
        run.config = Some(config.clone());
        run.command = command.clone();
        run.stopping = false;
        let generation = run.generation;
        drop(run);
        {
            let mut store = self.store.lock().unwrap();
            store.remember(&config);
            if let Err(e) = store.save() {
                say!("launcher: {e}");
            }
        }
        {
            let mut hub = self.hub.lock().unwrap();
            hub.history.clear();
            hub.publish(json!({"ev": "run", "generation": generation, "command": command, "config": config.to_json()}));
        }
        say!("launcher: {command}");
        let first = if config.build { "building" } else { "preparing" };
        self.state(first, "");
        let test = config.action == Action::Test;
        let mut readers = Vec::new();
        for (src, pipe) in [
            ("out", child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
            ("err", child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>)),
        ] {
            let Some(pipe) = pipe else { continue };
            let me = self.clone();
            readers.push(std::thread::spawn(move || me.pump(src, pipe, test)));
        }
        let me = self.clone();
        std::thread::spawn(move || {
            let status = child.wait();
            for r in readers {
                let _ = r.join();
            }
            let (stopping, last, stopped) = {
                let run = me.run.lock().unwrap();
                (run.stopping, run.state.clone(), run.stopped.clone())
            };
            if stopping {
                // QEMU outlives its parent by a moment: wait for it, and
                // make sure of it after ten seconds.
                let start = Instant::now();
                while stopped.iter().any(|&p| alive(p)) {
                    if start.elapsed() > Duration::from_secs(10) {
                        signal(&stopped, "KILL");
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
            me.run.lock().unwrap().pid = None;
            let status = status.ok();
            match (stopping, status.and_then(|s| s.code())) {
                (true, _) => me.state("stopped", "Stopped."),
                (false, Some(0)) if test => me.state("passed", "All kernel tests passed."),
                (false, Some(0)) => me.state("exited", "WaveOS has shut down."),
                (false, code) => {
                    use std::os::unix::process::ExitStatusExt;
                    let status = match (code, status.and_then(|s| s.signal())) {
                        (Some(c), _) => format!("status {c}"),
                        (None, Some(6)) => String::from("it crashed: SIGABRT"),
                        (None, Some(s)) => format!("signal {s}"),
                        (None, None) => String::from("an unknown status"),
                    };
                    let message = match (last.as_str(), test) {
                        ("building", _) => format!("The build failed ({status}); the output shows why."),
                        ("preparing", _) => {
                            format!("xtask stopped before QEMU started ({status}); the output shows why.")
                        }
                        (_, true) => format!("The kernel tests failed ({status})."),
                        _ => format!("QEMU stopped with {status}; the output shows why."),
                    };
                    me.state("failed", &message)
                }
            }
        });
        Ok(command)
    }

    /// Streams one of the run's pipes into the page, noticing where it's got to.
    fn pump(self: &Arc<Self>, src: &str, pipe: Box<dyn Read + Send>, test: bool) {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let text = crate::monitor::clean(String::from_utf8_lossy(&buf).trim_end_matches('\n'));
            // `run` announces QEMU; `test` says what it's testing once it's built.
            let starting = text.starts_with("starting QEMU:") || (test && text.starts_with("running kernel tests"));
            let desktop = text.contains("Aurora desktop ready") && !test;
            self.hub.lock().unwrap().publish(json!({"ev": "line", "src": src, "text": text}));
            if starting {
                self.state(if test { "testing" } else { "booting" }, "");
            } else if desktop {
                self.state("running", "WaveOS is at the desktop.");
            }
        }
    }

    /// Stops the run: SIGTERM to it and everything it started (QEMU, cargo);
    /// the run's waiter then makes sure they've all gone (SIGKILL after ten
    /// seconds) before saying it stopped.
    fn stop(self: &Arc<Self>) -> bool {
        let mut run = self.run.lock().unwrap();
        let Some(pid) = run.pid else { return false };
        if run.stopping {
            return true;
        }
        run.stopping = true;
        run.stopped = tree(pid);
        let stopped = run.stopped.clone();
        drop(run);
        self.state("stopping", "Stopping…");
        signal(&stopped, "TERM");
        true
    }

    /// Waits up to `limit` for the run to end.
    fn wait_stopped(&self, limit: Duration) -> bool {
        let start = Instant::now();
        while self.run.lock().unwrap().pid.is_some() {
            if start.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        true
    }

    fn state_json(&self) -> Value {
        let store = self.store.lock().unwrap();
        let mut presets: Vec<Value> = runconfig::builtin_presets()
            .into_iter()
            .map(|(name, about, c)| json!({"name": name, "description": about, "builtin": true, "config": c.to_json()}))
            .collect();
        presets.extend(store.presets.iter().map(
            |(n, c)| json!({"name": n, "description": c.command_line(), "builtin": false, "config": c.to_json()}),
        ));
        let run = self.run.lock().unwrap();
        let caps = self.caps(crate::arch::current());
        let two = Displays::plan(vec![crate::displays::DEFAULT_SIZE; 2], false, cfg!(target_os = "macos"), caps);
        json!({
            "options": runconfig::options_json(&|a| self.caps(a).clone()),
            "presets": presets,
            "last": store.last.as_ref().map(RunConfig::to_json),
            "store": store_path().display().to_string(),
            "host": if cfg!(target_os = "macos") { "macos" } else { std::env::consts::OS },
            "qemu": {
                "found": caps.found,
                "version": caps.version,
                "displays": caps.displays,
                "vnc": caps.vnc,
                "secondary_vga": caps.has_device("secondary-vga"),
                "windows": format!("{:?}", two.windows),
            },
            "run": {"state": run.state, "command": run.command, "running": run.pid.is_some()},
        })
    }

    fn command_json(&self, config: &RunConfig) -> Value {
        let displays = if config.action == Action::Test {
            Vec::new()
        } else {
            Displays::plan(config.displays.clone(), config.headless, cfg!(target_os = "macos"), self.caps(config.arch()))
                .describe()
        };
        json!({"command": config.command_line(), "args": config.args(), "displays": displays})
    }
}

/// `pid` and all its descendants (found with `pgrep -P`).
fn tree(pid: u32) -> Vec<u32> {
    let mut all = vec![pid];
    let mut i = 0;
    while i < all.len() {
        if let Ok(out) = Command::new("pgrep").arg("-P").arg(all[i].to_string()).output() {
            all.extend(String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|p| p.parse::<u32>().ok()));
        }
        i += 1;
    }
    all
}

/// Sends `signal` (TERM, KILL) to `pids`.
fn signal(pids: &[u32], signal: &str) {
    let _ = Command::new("kill")
        .arg(format!("-{signal}"))
        .args(pids.iter().map(u32::to_string))
        .stderr(Stdio::null())
        .status();
}

fn alive(pid: u32) -> bool {
    Command::new("kill").args(["-0", &pid.to_string()]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

// ---- The web server ---------------------------------------------------------

fn serve(port: u16, open: bool) {
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap_or_else(|e| {
        eprintln!("could not listen on 127.0.0.1:{port}: {e}");
        exit(1)
    });
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(port);
    let caps: Vec<(Arch, QemuCaps)> = crate::arch::ALL.into_iter().map(|a| (a, QemuCaps::probe_for(a))).collect();
    for (a, c) in &caps {
        if !c.found {
            eprintln!("warning: {} isn't on the PATH; install QEMU (see docs/BUILDING.md) to boot WaveOS", a.qemu_binary());
        }
    }
    let launcher = Arc::new(Launcher {
        caps,
        hub: Mutex::new(Hub::default()),
        run: Mutex::new(Run { state: "idle".into(), ..Run::default() }),
        store: Mutex::new(Store::load()),
        port,
        pages: AtomicUsize::new(0),
        page_seen: AtomicBool::new(false),
        page_left: Mutex::new(Instant::now()),
    });
    let url = format!("http://127.0.0.1:{port}/");
    say!("WaveOS launcher: {url}");
    say!("Keep this terminal open while WaveOS runs; Ctrl-C stops WaveOS and the launcher.");
    let _ = std::io::stdout().flush();
    if open {
        crate::monitor::open_browser(&url);
    }
    // Leave once WaveOS isn't running and the page has been gone a while.
    let me = launcher.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(1));
        let idle = me.run.lock().unwrap().pid.is_none()
            && me.page_seen.load(Ordering::SeqCst)
            && me.pages.load(Ordering::SeqCst) == 0
            && me.page_left.lock().unwrap().elapsed() > IDLE_EXIT;
        if idle {
            say!("The launcher page is closed and WaveOS isn't running: the launcher is done.");
            exit(0);
        }
    });
    for stream in listener.incoming().flatten() {
        let me = launcher.clone();
        std::thread::spawn(move || {
            if let Some(req) = Request::read(&stream) {
                me.handle(stream, req);
            }
        });
    }
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    fn read(mut stream: &TcpStream) -> Option<Request> {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
        let mut data = Vec::new();
        let mut buf = [0u8; 8192];
        let head_end = loop {
            if let Some(i) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            if data.len() > 64 * 1024 {
                return None;
            }
            let n = stream.read(&mut buf).ok()?;
            if n == 0 {
                return None;
            }
            data.extend_from_slice(&buf[..n]);
        };
        let mut req = Request::parse_head(&String::from_utf8_lossy(&data[..head_end]))?;
        let length: usize = req.header("content-length").and_then(|l| l.parse().ok()).unwrap_or(0);
        if length > 1 << 20 {
            return None;
        }
        req.body = data[head_end..].to_vec();
        while req.body.len() < length {
            let n = stream.read(&mut buf).ok()?;
            if n == 0 {
                return None;
            }
            req.body.extend_from_slice(&buf[..n]);
        }
        req.body.truncate(length);
        Some(req)
    }

    pub fn parse_head(head: &str) -> Option<Request> {
        let mut lines = head.lines();
        let mut first = lines.next()?.split_whitespace();
        let method = first.next()?.to_string();
        let path = first.next()?.to_string();
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        Some(Request { method, path, headers, body: Vec::new() })
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }

    /// Only this page may use the launcher: the Host must be ours (no DNS
    /// rebinding), and changes must come as JSON from our own origin (a
    /// cross-site page can't send that without a CORS preflight we refuse).
    pub fn allowed(&self, port: u16) -> bool {
        let ours = |h: &str| h == format!("127.0.0.1:{port}") || h == format!("localhost:{port}");
        let host_ok = self.header("host").is_some_and(ours);
        if self.method == "GET" {
            return host_ok;
        }
        let json = self.header("content-type").is_some_and(|t| t.starts_with("application/json"));
        let origin_ok = self.header("origin").is_none_or(|o| o.strip_prefix("http://").is_some_and(ours));
        host_ok && json && origin_ok
    }
}

fn respond(mut stream: TcpStream, status: &str, ty: &str, body: &[u8]) {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body);
}

fn respond_json(stream: TcpStream, result: Result<Value, (u16, String)>) {
    match result {
        Ok(v) => respond(stream, "200 OK", "application/json", v.to_string().as_bytes()),
        Err((code, message)) => {
            let status = match code {
                403 => "403 Forbidden",
                404 => "404 Not Found",
                409 => "409 Conflict",
                _ => "400 Bad Request",
            };
            respond(stream, status, "application/json", json!({"error": message}).to_string().as_bytes())
        }
    }
}

/// The page, with the WayV mark inlined so it needs nothing else.
pub fn page() -> String {
    PAGE.replace("{{WAYV_MARK}}", &format!("data:image/png;base64,{}", base64(MARK)))
        .replace("{{WALLPAPER}}", &format!("data:image/jpeg;base64,{}", base64(WALLPAPER)))
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

impl Launcher {
    fn handle(self: Arc<Self>, stream: TcpStream, req: Request) {
        if !req.allowed(self.port) {
            return respond_json(stream, Err((403, "requests must come from the launcher page".into())));
        }
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let config = |v: &Value| RunConfig::from_json(v).map_err(|e| (400, e));
        let path = req.path.split('?').next().unwrap_or("/");
        let result = match (req.method.as_str(), path) {
            ("GET", "/" | "/index.html") => {
                return respond(stream, "200 OK", "text/html; charset=utf-8", page().as_bytes());
            }
            ("GET", "/events") => return self.events(stream),
            ("GET", "/api/state") => Ok(self.state_json()),
            ("POST", "/api/command") => config(&body).map(|c| self.command_json(&c)),
            ("POST", "/api/launch") => config(&body["config"]).and_then(|c| {
                if c.fresh_disk && body["confirm_erase"] != json!(true) {
                    return Err((400, "a fresh disk erases the files saved inside WaveOS: confirm it first".into()));
                }
                self.launch(c).map(|command| json!({"command": command})).map_err(|e| (409, e))
            }),
            ("POST", "/api/stop") => Ok(json!({"stopping": self.stop()})),
            ("POST", "/api/restart") => {
                let Some(c) = self.run.lock().unwrap().config.clone() else {
                    return respond_json(stream, Err((409, "nothing has been launched yet".into())));
                };
                self.stop();
                if !self.wait_stopped(Duration::from_secs(20)) {
                    return respond_json(stream, Err((409, "WaveOS didn't stop".into())));
                }
                let c = RunConfig { fresh_disk: false, ..c };
                self.launch(c).map(|command| json!({"command": command})).map_err(|e| (409, e))
            }
            ("POST", "/api/presets/save") => config(&body["config"]).and_then(|c| {
                let name = body["name"].as_str().unwrap_or("");
                self.edit_store(|s| s.put(name, c))
            }),
            ("POST", "/api/presets/rename") => {
                let (from, to) = (body["from"].as_str().unwrap_or(""), body["to"].as_str().unwrap_or(""));
                self.edit_store(|s| s.rename(from, to))
            }
            ("POST", "/api/presets/delete") => {
                let name = body["name"].as_str().unwrap_or("");
                self.edit_store(|s| s.delete(name))
            }
            ("POST", "/api/quit") => {
                respond_json(stream, Ok(json!({"quitting": true})));
                self.stop();
                self.wait_stopped(Duration::from_secs(15));
                say!("Quit from the launcher page.");
                exit(0);
            }
            _ => Err((404, format!("no {} {}", req.method, path))),
        };
        respond_json(stream, result);
    }

    fn edit_store(&self, f: impl FnOnce(&mut Store) -> Result<(), String>) -> Result<Value, (u16, String)> {
        let mut store = self.store.lock().unwrap();
        f(&mut store).map_err(|e| (400, e))?;
        store.save().map_err(|e| (400, e))?;
        drop(store);
        Ok(self.state_json())
    }

    /// Server-Sent Events: the current run so far, then live.
    fn events(self: Arc<Self>, mut stream: TcpStream) {
        let (tx, rx) = channel();
        let history: Vec<String> = {
            let mut hub = self.hub.lock().unwrap();
            hub.subscribers.push(tx);
            hub.history.iter().cloned().collect()
        };
        self.pages.fetch_add(1, Ordering::SeqCst);
        self.page_seen.store(true, Ordering::SeqCst);
        let mut out = String::from(
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: keep-alive\r\n\r\n",
        );
        out.push_str("event: replay-start\ndata: {}\n\n");
        for line in history {
            out.push_str(&format!("data: {line}\n\n"));
        }
        out.push_str("event: replay-end\ndata: {}\n\n");
        let mut ok = stream.write_all(out.as_bytes()).is_ok();
        while ok {
            ok = match rx.recv_timeout(Duration::from_secs(5)) {
                Ok(line) => {
                    let mut msg = format!("data: {line}\n\n");
                    while let Ok(more) = rx.try_recv() {
                        msg.push_str(&format!("data: {more}\n\n"));
                    }
                    stream.write_all(msg.as_bytes()).is_ok()
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => stream.write_all(b": keep-alive\n\n").is_ok(),
                Err(_) => false,
            };
        }
        if self.pages.fetch_sub(1, Ordering::SeqCst) == 1 {
            *self.page_left.lock().unwrap() = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_save_rename_delete_and_round_trip() {
        let mut s = Store::default();
        let small = RunConfig { smp: 2, memory_mib: 1024, ..Default::default() };
        s.put("Mine", small.clone()).unwrap();
        s.put("Two screens", RunConfig { displays: vec![(1920, 1080); 2], ..Default::default() }).unwrap();
        assert!(s.put("default", RunConfig::default()).is_err(), "built-in names are taken");
        assert!(s.put("  ", RunConfig::default()).is_err());
        assert!(s.rename("Mine", "Two screens").is_err());
        s.rename("Mine", "Small").unwrap();
        assert_eq!(s.presets[1], ("Small".to_string(), small.clone()));
        s.remember(&RunConfig { fresh_disk: true, ..small.clone() });
        assert_eq!(s.last, Some(small), "a fresh disk is never remembered");
        let text = serde_json::to_string(&s.to_json()).unwrap();
        assert_eq!(Store::from_json(&serde_json::from_str(&text).unwrap()), s);
        s.delete("Small").unwrap();
        assert!(s.delete("Small").is_err());
        assert_eq!(s.presets.len(), 1);
    }

    #[test]
    fn only_the_page_may_use_the_launcher() {
        let req = |head: &str| Request::parse_head(head).unwrap();
        let ok = req(
            "POST /api/launch HTTP/1.1\r\nHost: 127.0.0.1:4000\r\nContent-Type: application/json\r\nOrigin: http://127.0.0.1:4000\r\n",
        );
        assert!(ok.allowed(4000));
        assert!(!ok.allowed(4001));
        let form = req("POST /api/launch HTTP/1.1\r\nHost: 127.0.0.1:4000\r\nContent-Type: text/plain\r\n");
        assert!(!form.allowed(4000));
        let other = req(
            "POST /api/launch HTTP/1.1\r\nHost: 127.0.0.1:4000\r\nContent-Type: application/json\r\nOrigin: http://evil.example\r\n",
        );
        assert!(!other.allowed(4000));
        let rebound = req("GET /api/state HTTP/1.1\r\nHost: evil.example:4000\r\n");
        assert!(!rebound.allowed(4000));
        assert!(req("GET / HTTP/1.1\r\nHost: localhost:4000\r\n").allowed(4000));
    }

    #[test]
    fn the_page_is_self_contained() {
        let p = page();
        assert!(p.contains("data:image/png;base64,iVBORw0KGgo"));
        assert!(p.contains("data:image/jpeg;base64,/9j/"));
        assert!(!p.contains("{{"), "every placeholder is filled");
        for external in ["http://", "https://", "<script src", "stylesheet", "@import", "url('"] {
            assert!(!p.contains(external), "the page must not load {external}");
        }
        assert_eq!(base64(b"Wave"), "V2F2ZQ==");
        assert_eq!(base64(b"WaveOS"), "V2F2ZU9T");
    }
}
