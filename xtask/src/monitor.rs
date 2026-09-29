//! `cargo xtask run --monitor`: streams live telemetry to the System Explorer.
//!
//! ```text
//!  QEMU COM1 (console) ──stdout──┐
//!  QEMU COM2 (telemetry JSON) ───┼──> history + subscribers ──SSE──> http://127.0.0.1:7777
//!                                └──> target/telemetry.jsonl (recording)
//! ```
//!
//! Everything is std-only: a Unix socket the kernel's COM2 connects to, a
//! tiny HTTP server that serves `docs/explorer/index.html`, and a
//! Server-Sent-Events endpoint (`/events`) that first replays history (so a
//! page opened late still sees the whole boot) and then streams live.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_HISTORY: usize = 6000;

#[derive(Default)]
struct Hub {
    history: Vec<String>,
    subscribers: Vec<Sender<String>>,
    recording: Option<std::fs::File>,
}

impl Hub {
    fn publish(&mut self, line: String) {
        if let Some(f) = &mut self.recording {
            let _ = writeln!(f, "{line}");
        }
        self.subscribers.retain(|s| s.send(line.clone()).is_ok());
        self.history.push(line);
        if self.history.len() > MAX_HISTORY {
            // Keep every non-snapshot event (boot stages, processes, …); thin old snapshots.
            let cut = self.history.len() - MAX_HISTORY / 2;
            let mut i = 0;
            self.history.retain(|l| {
                i += 1;
                i > cut || !l.starts_with("{\"ev\":\"snap\"")
            });
        }
    }
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\r' => {}
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// Strips terminal escape sequences from firmware output.
fn clean(line: &str) -> String {
    let mut out = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for d in chars.by_ref() {
                    if d.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
            continue;
        }
        if c != '\r' {
            out.push(c);
        }
    }
    out
}

pub struct Monitor {
    hub: Arc<Mutex<Hub>>,
    pub socket: PathBuf,
    pub url: String,
}

impl Monitor {
    /// Binds the telemetry socket and the web server.
    pub fn start(root: &Path) -> Monitor {
        let socket = root.join("target/telemetry.sock");
        let _ = std::fs::remove_file(&socket);
        let hub = Arc::new(Mutex::new(Hub::default()));
        hub.lock().unwrap().recording = std::fs::File::create(root.join("target/telemetry.jsonl")).ok();

        // Telemetry from the kernel (QEMU connects to us).
        let listener = UnixListener::bind(&socket).expect("bind telemetry socket");
        let h = hub.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                for line in BufReader::new(stream).lines().map_while(Result::ok) {
                    if line.starts_with('{') {
                        h.lock().unwrap().publish(line);
                    }
                }
            }
        });

        // Web server.
        let page = root.join("docs/explorer/index.html");
        let (tcp, port) = (7777..7800)
            .find_map(|p| TcpListener::bind(("127.0.0.1", p)).ok().map(|l| (l, p)))
            .expect("no free port for the explorer");
        let h = hub.clone();
        std::thread::spawn(move || {
            for stream in tcp.incoming().flatten() {
                let h = h.clone();
                let page = page.clone();
                std::thread::spawn(move || serve(stream, &page, h));
            }
        });
        Monitor { hub, socket, url: format!("http://127.0.0.1:{port}/") }
    }

    /// Mirrors QEMU's console (COM1) to our stdout and into the event stream.
    pub fn pump_console(&self, child: &mut Child) {
        let Some(out) = child.stdout.take() else { return };
        let hub = self.hub.clone();
        std::thread::spawn(move || {
            let start = Instant::now();
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                println!("{line}");
                let text = clean(&line);
                if text.trim().is_empty() {
                    continue;
                }
                let ev = format!(
                    "{{\"ev\":\"console\",\"host_ms\":{},\"line\":{}}}",
                    start.elapsed().as_millis(),
                    json_escape(&text)
                );
                hub.lock().unwrap().publish(ev);
            }
        });
    }

    pub fn finish(&self) {
        self.hub.lock().unwrap().publish(String::from("{\"ev\":\"bye\"}"));
        std::thread::sleep(Duration::from_millis(300));
    }
}

pub fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let _ = Command::new(opener).arg(url).spawn();
}

fn serve(mut stream: TcpStream, page: &Path, hub: Arc<Mutex<Hub>>) {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).unwrap_or(0);
    let req = String::from_utf8_lossy(&buf[..n]);
    let path = req.split_whitespace().nth(1).unwrap_or("/");
    match path {
        "/events" => {
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\
                  Access-Control-Allow-Origin: *\r\nConnection: keep-alive\r\n\r\n",
            );
            let (tx, rx) = channel();
            let history = {
                let mut h = hub.lock().unwrap();
                h.subscribers.push(tx);
                h.history.clone()
            };
            let mut out = String::from("event: replay-start\ndata: {}\n\n");
            for line in history {
                out.push_str("data: ");
                out.push_str(&line);
                out.push_str("\n\n");
            }
            out.push_str("event: replay-end\ndata: {}\n\n");
            if stream.write_all(out.as_bytes()).is_err() {
                return;
            }
            loop {
                match rx.recv_timeout(Duration::from_secs(10)) {
                    Ok(line) => {
                        let mut msg = format!("data: {line}\n\n");
                        // Batch whatever else is already queued.
                        while let Ok(more) = rx.try_recv() {
                            msg.push_str(&format!("data: {more}\n\n"));
                        }
                        if stream.write_all(msg.as_bytes()).is_err() {
                            return;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        if stream.write_all(b": keep-alive\n\n").is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                }
            }
        }
        "/demo.js" => {
            let body = std::fs::read(page.with_file_name("demo.js")).unwrap_or_default();
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/javascript\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
        _ => {
            let (status, body, ty) = match std::fs::read(page) {
                Ok(b) if path == "/" || path.starts_with("/?") || path == "/index.html" => {
                    ("200 OK", b, "text/html; charset=utf-8")
                }
                Ok(_) => ("404 Not Found", b"not found".to_vec(), "text/plain"),
                Err(_) => ("500 Internal Server Error", b"docs/explorer/index.html missing".to_vec(), "text/plain"),
            };
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes());
            let _ = stream.write_all(&body);
        }
    }
}
