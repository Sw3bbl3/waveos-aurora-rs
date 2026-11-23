//! End-to-end checks of `cargo xtask launch --no-open`: the page and API
//! are served, and (ignored by default, since it boots WaveOS) a launch
//! posted the way the page posts it reaches the desktop and stops cleanly.
//!
//!   cargo test -p xtask --test launcher                          # the page and the API
//!   cargo xtask build && cargo test -p xtask --test launcher -- --ignored   # plus a headless boot
//!
//! The boot uses `--no-build` (cargo test holds the build lock, so the run
//! can't build), its own disk (target/launcher-e2e.img), QEMU session
//! directory (AURORA_SESSION_DIR) and presets file.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

struct Launcher {
    child: Child,
    port: u16,
}

impl Launcher {
    fn start(store: &str) -> Launcher {
        let store = std::env::temp_dir().join(format!("{store}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&store);
        let mut child = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(["launch", "--no-open"])
            .env("WAVEOS_LAUNCHER_STORE", &store)
            // Its own QEMU sockets and firmware variables, apart from any VM the developer has open.
            // (In the temporary directory: macOS limits socket paths to 104 bytes.)
            .env("AURORA_SESSION_DIR", std::env::temp_dir().join(format!("waveos-e2e-{}", std::process::id())))
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the launcher");
        let mut first = String::new();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        out.read_line(&mut first).unwrap();
        // Keep reading, so the launcher's stdout stays open.
        std::thread::spawn(move || std::io::copy(&mut out, &mut std::io::sink()));
        let port = first
            .trim()
            .strip_prefix("WaveOS launcher: http://127.0.0.1:")
            .and_then(|p| p.trim_end_matches('/').parse().ok())
            .unwrap_or_else(|| panic!("unexpected first line {first:?}"));
        Launcher { child, port }
    }

    /// One HTTP request; returns (status code, body).
    fn request(&self, method: &str, path: &str, content_type: &str, body: &str) -> (u16, String) {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            self.port,
            body.len()
        )
        .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        let code = out.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
        let body = out.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
        (code, body)
    }

    fn post(&self, path: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
        let (code, text) = self.request("POST", path, "application/json", &body.to_string());
        (code, serde_json::from_str(&text).unwrap_or(serde_json::Value::Null))
    }

    /// Quits through the page's Quit button and waits for the launcher to exit.
    fn quit(mut self) {
        let _ = self.post("/api/quit", serde_json::json!({}));
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "launcher exited with {status}");
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        panic!("the launcher didn't quit");
    }
}

impl Drop for Launcher {
    /// On a failed check: stop WaveOS (the launcher's Quit does), then make sure.
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            if let Ok(mut s) = TcpStream::connect(("127.0.0.1", self.port)) {
                let _ = write!(
                    s,
                    "POST /api/quit HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\n\
                     Content-Length: 2\r\nConnection: close\r\n\r\n{{}}",
                    self.port
                );
                let _ = s.read_to_end(&mut Vec::new());
            }
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(20) && self.child.try_wait().ok().flatten().is_none() {
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = self.child.kill();
    }
}

#[test]
fn launcher_serves_the_page_and_the_command() {
    let l = Launcher::start("launcher-page");
    let (code, page) = l.request("GET", "/", "text/plain", "");
    assert_eq!(code, 200);
    assert!(page.contains("<title>WaveOS Launcher</title>") && page.contains("data:image/png;base64,"));

    let (code, state) = l.request("GET", "/api/state", "text/plain", "");
    assert_eq!(code, 200);
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    let disks: Vec<&str> =
        state["options"]["disk"].as_array().unwrap().iter().map(|d| d["value"].as_str().unwrap()).collect();
    assert_eq!(disks, ["ahci", "virtio", "nvme", "sd", "emmc"]);
    assert!(state["presets"].as_array().unwrap().iter().any(|p| p["name"] == "Dual display"));

    let (code, cmd) = l.post(
        "/api/command",
        serde_json::json!({"displays": ["1920x1080", "1280x800"], "gpu": "std", "smp": 2, "headless": true}),
    );
    assert_eq!(code, 200);
    assert_eq!(cmd["command"], "cargo xtask run --displays 2 --resolution 1920x1080,1280x800 --smp 2 --headless");
    assert_eq!(cmd["displays"].as_array().unwrap().len(), 2 + 1, "a line per display, and the note");

    // Only the page may make changes: no form posts, no fresh disk without confirming.
    let (code, _) = l.request("POST", "/api/launch", "text/plain", "{}");
    assert_eq!(code, 403);
    let (code, err) = l.post("/api/launch", serde_json::json!({"config": {"fresh_disk": true}}));
    assert_eq!(code, 400, "{err}");
    let (code, _) = l.post("/api/command", serde_json::json!({"disk": "floppy"}));
    assert_eq!(code, 400);

    // A preset survives a restart of the launcher (in its own store file).
    let (code, _) = l.post("/api/presets/save", serde_json::json!({"name": "Mine", "config": {"smp": 1}}));
    assert_eq!(code, 200);
    let (code, _) = l.post("/api/presets/save", serde_json::json!({"name": "Default", "config": {}}));
    assert_eq!(code, 400, "built-in names are taken");
    l.quit();
}

#[test]
#[ignore = "boots WaveOS in QEMU; run `cargo xtask build` first"]
fn launcher_boots_headless_to_the_desktop_and_stops() {
    let l = Launcher::start("launcher-boot");
    let mut events = TcpStream::connect(("127.0.0.1", l.port)).unwrap();
    write!(events, "GET /events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\r\n", l.port).unwrap();
    events.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut events = BufReader::new(events);

    let config = serde_json::json!({
        "headless": true, "build": false, "linux": false, "fresh_disk": true,
        "disk_image": "target/launcher-e2e.img", "smp": 2, "memory_mib": 2048,
    });
    let (code, reply) = l.post("/api/launch", serde_json::json!({"config": config, "confirm_erase": true}));
    assert_eq!(code, 200, "{reply}");
    assert_eq!(
        reply["command"],
        "cargo xtask run --smp 2 --memory-mib 2048 --disk-image target/launcher-e2e.img --fresh-disk --no-linux-root \
         --headless --no-build"
    );
    let (code, _) = l.post("/api/launch", serde_json::json!({"config": config, "confirm_erase": true}));
    assert_eq!(code, 409, "one run at a time");

    let mut seen = Vec::new();
    wait_for(&mut events, &mut seen, "running", Duration::from_secs(300));
    assert!(seen.iter().any(|l| l.starts_with("starting QEMU:")));
    assert!(seen.iter().any(|l| l.contains("Aurora desktop ready")));

    let (code, reply) = l.post("/api/stop", serde_json::json!({}));
    assert_eq!((code, &reply["stopping"]), (200, &serde_json::json!(true)));
    wait_for(&mut events, &mut seen, "stopped", Duration::from_secs(30));
    // QEMU went with it.
    let left = Command::new("pgrep").args(["-f", "launcher-e2e.img"]).output().unwrap();
    assert!(left.stdout.is_empty(), "QEMU is still running: {}", String::from_utf8_lossy(&left.stdout));
    let (_, state) = l.request("GET", "/api/state", "text/plain", "");
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(state["run"]["running"], false);
    assert_eq!(state["last"]["fresh_disk"], false, "a fresh disk is never remembered");
    let _ = std::fs::remove_file(root().join("target/launcher-e2e.img"));
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join(format!("waveos-e2e-{}", std::process::id())));
    l.quit();
}

/// Reads the event stream until `want` shows up as the run's state, failing on errors.
fn wait_for(events: &mut BufReader<TcpStream>, seen: &mut Vec<String>, want: &str, limit: Duration) {
    let start = Instant::now();
    let mut line = String::new();
    while start.elapsed() < limit {
        line.clear();
        if events.read_line(&mut line).is_err() {
            continue;
        }
        let Some(data) = line.trim().strip_prefix("data: ") else { continue };
        let ev: serde_json::Value = serde_json::from_str(data).unwrap();
        if ev["ev"] == "line" {
            seen.push(ev["text"].as_str().unwrap_or("").to_string());
        }
        if ev["ev"] == "state" {
            let state = ev["state"].as_str().unwrap_or("");
            assert!(state != "failed", "the run failed: {}\n{}", ev["message"], seen.join("\n"));
            if state == want {
                return;
            }
        }
    }
    panic!("no {want} after {limit:?}:\n{}", seen.join("\n"));
}
