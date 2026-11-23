//! sbxtest: the sandbox seen from inside (run by the kernel self-tests, in a
//! sandbox they set up; see `kernel/src/sandbox/selftest.rs`).
//!
//! ```text
//! sbxtest deny VICTIM     nothing declared: everything beyond its container is refused
//! sbxtest grants          what the person gives it: the open panel, a drop, a paste, bookmarks
//! sbxtest resolve TOKEN   a later run brings a bookmark back
//! sbxtest allow           declared and allowed; then taken away while it runs
//! ```
//!
//! `/SandboxOracle` holds files it must never reach unless given them.
//! Exit status: the number of failed checks.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use core::sync::atomic::{AtomicU32, Ordering};
use corekit::abi::{self, err::*, nr};
use corekit::sys::{call, str_args};
use corekit::{fs, println, process, sandbox, users};

corekit::entry!(main);

const ORACLE: &str = "/SandboxOracle";
static FAILED: AtomicU32 = AtomicU32::new(0);

fn fail(what: &str) {
    println!("sbxtest: FAIL {what}");
    FAILED.fetch_add(1, Ordering::Relaxed);
}

fn refused<T>(what: &str, r: corekit::Result<T>) {
    match r {
        Err(e) if e.is(EACCES) || e.is(EPERM) => {}
        Err(e) => fail(&format!("{what}: refused with {e}, not EACCES/EPERM")),
        Ok(_) => fail(&format!("{what}: allowed")),
    }
}

fn allowed<T>(what: &str, r: corekit::Result<T>) -> Option<T> {
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            fail(&format!("{what}: {e}"));
            None
        }
    }
}

fn secret() -> String {
    format!("{ORACLE}/secret.txt")
}

fn desktop(req: usize, arg: &str) -> corekit::Result<u64> {
    let [p, l] = str_args(arg);
    call(nr::DESKTOP, &[req as u64, p, l])
}

fn clipboard_get(kind: usize) -> corekit::Result<String> {
    let mut buf = [0u8; 512];
    let n = call(nr::CLIPBOARD_GET, &[kind as u64, buf.as_mut_ptr() as u64, buf.len() as u64])? as usize;
    Ok(String::from_utf8_lossy(&buf[..n.min(buf.len())]).into_owned())
}

/// Nothing declared: only its container (and the system image) is there.
fn deny(victim: u32) {
    let info = sandbox::info();
    if info.flags & abi::sandbox::SANDBOXED == 0 {
        fail("not sandboxed");
    }
    let c = sandbox::container();
    if c.is_empty() || !c.contains("/Library/Containers/") {
        fail(&format!("container {c}"));
    }
    // Its own files.
    let mine = fs::join(&c, "mine.txt");
    allowed("write in the container", fs::write(&mine, b"mine"));
    if allowed("read in the container", fs::read(&mine)).as_deref() != Some(&b"mine"[..]) {
        fail("container file content");
    }
    allowed("read the system image", fs::read("/System/version.txt"));
    allowed("container folders", fs::read_dir(&sandbox::documents()));
    // Everyone else's.
    refused("read the oracle", fs::read(&secret()));
    refused("stat the oracle", fs::stat(&secret()));
    refused("list /", fs::read_dir("/"));
    refused("list the oracle", fs::read_dir(ORACLE));
    refused("write the oracle", fs::write(&secret(), b"x"));
    refused("chmod the oracle", users::chmod(&secret(), 0o777));
    refused("make a file in Documents", fs::write("/Documents/sbxtest.txt", b"x"));
    refused("make a folder at /", fs::mkdir("/sbxtest-dir"));
    refused("rename out of the container", fs::rename(&mine, "/sbxtest-escaped.txt"));
    refused("rename the oracle in", fs::rename(&secret(), &fs::join(&c, "stolen.txt")));
    let up = format!("{c}/../../../..{ORACLE}/secret.txt");
    refused("walk out with ..", fs::read(&up));
    refused("remove the oracle", fs::remove(&secret()));
    refused("another app's container", fs::read_dir(&c.replace("org.waveos.test", "org.waveos.other")));
    // Capabilities it did not declare.
    refused("socket", call(nr::SOCKET, &[abi::net::TCP]));
    refused("microphone", call(nr::AUDIO_CAPTURE, &[0]));
    refused("sound", call(nr::AUDIO_OPEN, &[]));
    let [t, tl] = str_args("sbxtest");
    refused("notify", call(nr::NOTIFY, &[t, tl, t, tl]));
    refused("spawn", process::spawn("/System/Bin/hello", &[], Default::default()));
    refused("controllers", call(nr::GAMEPADS, &[0, 0]));
    refused("preferences", corekit::prefs::set("sbxtest", "1"));
    refused("dark mode", desktop(abi::desktop::SET_DARK, ""));
    refused("shut down", desktop(abi::desktop::SHUTDOWN, ""));
    refused("open another app", desktop(abi::desktop::OPEN_APP, "/System/Apps/Notes.elf"));
    refused("open a document", desktop(abi::desktop::OPEN_FILE, &secret()));
    refused("end another app's process", process::kill(victim));
    refused("signal another app's process", process::signal(victim as i64, 15));
    refused("read the clipboard from behind", clipboard_get(abi::clip::TEXT));
    refused("administrator prompt", users::elevate("test"));
    refused("kernel log", {
        let mut b = [0u8; 16];
        call(nr::SYS_REPORT, &[abi::report::LOG, b.as_mut_ptr() as u64, 16])
    });
    refused("privacy settings", sandbox::set_permission("org.waveos.test.sbx", "microphone", Some(true)));
    // A descriptor it was given still works: it is a capability.
    let mut b = [0u8; 64];
    match corekit::io::read_fd(0, &mut b) {
        Ok(n) if b[..n].starts_with(b"inherited") => {}
        other => fail(&format!("the inherited descriptor: {other:?}")),
    }
    // Dropping rights is always allowed, and is for good.
    allowed("restrict", sandbox::restrict(0));
    if sandbox::capabilities() != 0 {
        fail("capabilities after restrict(0)");
    }
    allowed("the container after restrict", fs::read(&mine));
    let _ = fs::remove(&mine);
}

/// What the person gives it.
fn grants() {
    let picked = format!("{ORACLE}/picked.txt");
    match sandbox::open_panel() {
        Ok(p) if p == picked => {}
        other => fail(&format!("open panel: {other:?}")),
    }
    allowed("read the picked file", fs::read(&picked));
    allowed("change the picked file", fs::write(&picked, b"picked, changed"));
    refused("a file next to the picked one", fs::read(&secret()));
    refused("the picked file's folder", fs::read_dir(ORACLE));
    match sandbox::bookmark(&picked) {
        Ok(t) if t.len() == 32 && t.bytes().all(|b| b.is_ascii_hexdigit()) => println!("sbxtest: token {t}"),
        other => fail(&format!("bookmark: {other:?}")),
    }
    refused("bookmark something not given", sandbox::bookmark(&secret()));
    // A drop.
    let mut buf = [0u8; 256];
    match call(nr::DRAG_DATA, &[buf.as_mut_ptr() as u64, buf.len() as u64]) {
        Ok(n) => {
            let path = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
            allowed("read the dropped file", fs::read(path.trim()));
        }
        Err(e) => fail(&format!("drag_data: {e}")),
    }
    // A paste (the app is in front).
    match clipboard_get(abi::clip::FILES) {
        Ok(list) => {
            allowed("read the pasted file", fs::read(list.lines().next().unwrap_or("")));
        }
        Err(e) => fail(&format!("paste: {e}")),
    }
    let s = secret();
    let [p, l] = str_args(&s);
    refused("copy a file it may not read", call(nr::CLIPBOARD_SET, &[abi::clip::FILES as u64, p, l]));
    refused("the oracle, still", fs::read(&secret()));
}

/// A later run of the same app brings the bookmark back.
fn resolve(token: &str) {
    refused("the picked file before resolving", fs::read(&format!("{ORACLE}/picked.txt")));
    match sandbox::resolve(token) {
        Ok(p) => {
            allowed("read the bookmarked file", fs::read(&p));
        }
        Err(e) => fail(&format!("resolve: {e}")),
    }
    match sandbox::resolve("00000000000000000000000000000000") {
        Err(e) if e.is(ENOENT) => {}
        other => fail(&format!("a forged bookmark: {other:?}")),
    }
    refused("the oracle, still", fs::read(&secret()));
}

/// Declared and allowed; then the person switches Documents off.
fn allow() {
    allowed("socket", call(nr::SOCKET, &[abi::net::TCP]));
    let doc = "/Documents/sbxtest.txt";
    allowed("write in Documents", fs::write(doc, b"allowed"));
    allowed("read in Documents", fs::read(doc));
    let [t, tl] = str_args("sbxtest");
    allowed("notify", call(nr::NOTIFY, &[t, tl, t, tl]));
    if let Some(pid) = allowed("spawn", process::spawn("/System/Bin/hello", &["from a sandbox"], Default::default())) {
        allowed("wait", process::wait(pid, Some(10_000)));
    }
    refused("the oracle, still", fs::read(&secret()));
    // The harness takes Documents away now.
    println!("sbxtest: ready");
    let mut b = [0u8; 8];
    let _ = corekit::io::read_fd(0, &mut b);
    refused("Documents after it was switched off", fs::read(doc));
    allowed("socket, still", call(nr::SOCKET, &[abi::net::TCP]));
}

fn main(args: corekit::Args) -> i32 {
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    match mode {
        "deny" => deny(args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1)),
        "grants" => grants(),
        "resolve" => resolve(args.get(2).map(String::as_str).unwrap_or("")),
        "allow" => allow(),
        _ => fail("unknown mode"),
    }
    let n = FAILED.load(Ordering::Relaxed);
    println!("sbxtest {mode}: {} failed", n);
    n as i32
}
