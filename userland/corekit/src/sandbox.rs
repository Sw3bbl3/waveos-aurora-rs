//! The app sandbox: where an app keeps its own files, what it may do, and
//! how it gets at the person's files.
//!
//! Every app the desktop starts runs in a sandbox (see
//! docs/developer/sandboxing.md): it may use its own *container* — a
//! private folder per app and account, [`container`] — and what its
//! manifest declares and the person allowed. Other files come from the
//! person: the system open and save panel ([`open_panel`], [`save_panel`],
//! [`choose_folder`]), dropping or pasting files on the app, or opening a
//! document with it. [`bookmark`] keeps such a file reachable across runs.
//!
//! The container's layout:
//!
//! ```text
//! ~/Library/Containers/<app id>/
//!     Documents/     the app's own documents
//!     Preferences/   its settings ([`preferences`])
//!     Caches/        what can be made again (the system may clear it)
//!     tmp/           temporary files (also Linux programs' /tmp)
//!     Home/          its Linux programs' home folder
//! ```
//!
//! The helpers work outside a sandbox too (a program the kernel started,
//! say): the container is then the one the program would have.

use crate::abi::sandbox::{self as cap, Info};
use crate::abi::nr;
use crate::sys::{call, str_args};
use crate::Result;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub use crate::abi::sandbox::{
    ALL, AUDIO_OUT, CAMERA, CLIPBOARD_READ, DISPLAY, DOCUMENTS, DOWNLOADS, GAMEPADS, HOME, IPC, LAUNCH, LINUX,
    MICROPHONE, NET_CLIENT, NET_SERVER, NOTIFY, PICTURES, PREFS_WRITE, SCREEN_CAPTURE, SPAWN,
};

/// The calling process's sandbox.
pub fn info() -> Info {
    let mut i = Info::zeroed();
    let _ = call(nr::SANDBOX, &[cap::INFO as u64, &mut i as *mut Info as u64]);
    i
}

/// True if this process runs in a sandbox.
pub fn sandboxed() -> bool {
    info().flags & cap::SANDBOXED != 0
}

/// The app's id (`org.example.app`).
pub fn app_id() -> String {
    String::from(info().id())
}

/// What it may do right now (`aurora_abi::sandbox` bits). Guarded ones
/// the person has not answered about yet are not in it: using them asks.
pub fn capabilities() -> u64 {
    info().caps
}

/// Whether it may use `bits` now without asking anyone.
pub fn may(bits: u64) -> bool {
    capabilities() & bits == bits
}

/// Gives up every capability not in `keep`, for good (and for programs it
/// starts from now on). An unconfined program becomes sandboxed.
pub fn restrict(keep: u64) -> Result<()> {
    call(nr::SANDBOX, &[cap::RESTRICT as u64, keep]).map(|_| ())
}

fn mkdirs(path: &str) {
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        if !crate::fs::is_dir(&at) {
            let _ = crate::fs::mkdir(&at);
        }
    }
}

/// The app's private folder (made if it is missing).
pub fn container() -> String {
    let c = String::from(info().container());
    mkdirs(&c);
    c
}

fn sub(name: &str) -> String {
    let p = crate::fs::join(&container(), name);
    mkdirs(&p);
    p
}

/// Where the app keeps its own documents.
pub fn documents() -> String {
    sub("Documents")
}

/// Where the app keeps its settings.
pub fn preferences() -> String {
    sub("Preferences")
}

/// A settings file of the app's: `Preferences/<name>`.
pub fn preferences_file(name: &str) -> String {
    crate::fs::join(&preferences(), name)
}

/// What can be made again; the system may clear it.
pub fn caches() -> String {
    sub("Caches")
}

/// Temporary files.
pub fn temporary() -> String {
    sub("tmp")
}

/// A call that returns text in a buffer at `args[out_at]` (with its
/// length after it); asked again with room enough unless `once`.
fn text_call(op: usize, args: &[u64], out_at: usize, once: bool) -> Result<String> {
    let mut buf = vec![0u8; 4096];
    loop {
        let mut a: Vec<u64> = Vec::from(args);
        a.insert(0, op as u64);
        a[out_at] = buf.as_mut_ptr() as u64;
        a[out_at + 1] = buf.len() as u64;
        let n = call(nr::SANDBOX, &a)? as usize;
        if n <= buf.len() || once {
            buf.truncate(n.min(buf.len()));
            return Ok(String::from_utf8_lossy(&buf).into_owned());
        }
        buf.resize(n, 0);
    }
}

fn panel(mode: u64, name: &str) -> Result<String> {
    let [p, l] = str_args(name);
    text_call(cap::PANEL, &[mode, p, l, 0, 0], 4, true)
}

/// The system open panel: the file the person picks, which the app may
/// then read and change (`ECANCELED` if they cancel).
pub fn open_panel() -> Result<String> {
    panel(cap::PANEL_OPEN, "")
}

/// The system save panel, suggesting `name`: where the person wants it.
pub fn save_panel(name: &str) -> Result<String> {
    panel(cap::PANEL_SAVE, name)
}

/// The panel for choosing a folder: the app may use what is in it.
pub fn choose_folder() -> Result<String> {
    panel(cap::PANEL_FOLDER, "")
}

/// A bookmark to a file or folder the person gave the app: keep the
/// returned token (in the container) and [`resolve`] it in a later run.
pub fn bookmark(path: &str) -> Result<String> {
    let [p, l] = str_args(path);
    let mut out = [0u8; 32];
    call(nr::SANDBOX, &[cap::BOOKMARK as u64, p, l, out.as_mut_ptr() as u64])?;
    Ok(String::from_utf8_lossy(&out).into_owned())
}

/// The file or folder a bookmark names; the app may use it again.
pub fn resolve(token: &str) -> Result<String> {
    let [p, l] = str_args(token);
    text_call(cap::RESOLVE, &[p, l, 0, 0], 3, false)
}

/// Guarded capabilities in use now (the microphone while anything records).
pub fn in_use() -> u64 {
    call(nr::SANDBOX, &[cap::IN_USE as u64]).unwrap_or(0)
}

/// An app and what it may do (Settings, Privacy & Security).
#[derive(Clone, Debug, Default)]
pub struct AppPrivacy {
    pub id: String,
    pub name: String,
    /// `system`, `package`, `legacy` or `restricted`.
    pub kind: String,
    pub declared: Vec<String>,
    pub allowed: Vec<String>,
    pub denied: Vec<String>,
}

/// A bookmark an app keeps.
#[derive(Clone, Debug)]
pub struct Bookmark {
    pub app: String,
    pub token: String,
    pub write: bool,
    pub path: String,
}

/// Everything Settings shows.
#[derive(Clone, Debug, Default)]
pub struct Privacy {
    pub apps: Vec<AppPrivacy>,
    pub bookmarks: Vec<Bookmark>,
    /// Recent refusals: (app, what).
    pub denied: Vec<(String, String)>,
}

/// What every app may do, the bookmarks apps keep and recent refusals.
/// Needs the `privacy` capability (Settings has it).
pub fn privacy() -> Result<Privacy> {
    let text = text_call(cap::PRIVACY_LIST, &[0, 0], 1, false)?;
    let list = |s: &str| s.split(',').filter(|x| !x.is_empty()).map(String::from).collect::<Vec<_>>();
    let (mut apps, mut marks, mut denied) = (Vec::new(), Vec::new(), Vec::new());
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["app", id, name, kind, declared, allowed, refused] => apps.push(AppPrivacy {
                id: String::from(*id),
                name: String::from(*name),
                kind: String::from(*kind),
                declared: list(declared),
                allowed: list(allowed),
                denied: list(refused),
            }),
            ["bookmark", app, token, mode, path] => marks.push(Bookmark {
                app: String::from(*app),
                token: String::from(*token),
                write: *mode == "rw",
                path: String::from(*path),
            }),
            ["denied", app, what] => denied.push((String::from(*app), String::from(*what))),
            _ => {}
        }
    }
    Ok(Privacy { apps, bookmarks: marks, denied })
}

/// Allows (`Some(true)`), refuses (`Some(false)`) or forgets (None: the
/// app asks again) permission `perm` for app `id`. Takes effect at once.
pub fn set_permission(id: &str, perm: &str, allowed: Option<bool>) -> Result<()> {
    let answer = match allowed {
        Some(true) => "allow",
        Some(false) => "deny",
        None => "ask",
    };
    let line = alloc::format!("{id}\t{perm}\t{answer}");
    let [p, l] = str_args(&line);
    call(nr::SANDBOX, &[cap::PRIVACY_SET as u64, p, l]).map(|_| ())
}

/// Forgets a bookmark of app `id`.
pub fn forget_bookmark(id: &str, token: &str) -> Result<()> {
    let line = alloc::format!("{id}\tbookmark\t{token}\tforget");
    let [p, l] = str_args(&line);
    call(nr::SANDBOX, &[cap::PRIVACY_SET as u64, p, l]).map(|_| ())
}

/// The person-facing description of a permission name.
pub fn describe(perm: &str) -> &'static str {
    match perm {
        "network" => "Internet and local network",
        "network-server" => "Accepting connections",
        "audio" => "Playing sound",
        "microphone" => "Microphone",
        "camera" => "Camera",
        "screen-capture" => "Screen recording",
        "input-monitoring" => "Input monitoring",
        "gamepad" => "Game controllers",
        "usb" => "USB devices",
        "spawn" => "Starting programs",
        "launch" => "Opening other apps",
        "ipc" => "Talking to other apps",
        "notifications" => "Notifications",
        "background" => "Running in the background",
        "clipboard" => "Reading the clipboard",
        "preferences" => "System preferences",
        "linux" => "Linux programs",
        "elevate" => "Administrator approval",
        "display" => "Driving the display",
        "system" => "System control",
        "admin" => "Administrator capability",
        "privacy" => "Privacy settings",
        "processes" => "Other programs",
        "linux-home" => "Linux home folder",
        "documents" | "documents:read" => "Documents folder",
        "downloads" | "downloads:read" => "Downloads folder",
        "pictures" | "pictures:read" => "Pictures folder",
        "music" | "music:read" => "Music folder",
        "movies" | "movies:read" => "Movies folder",
        "desktop" | "desktop:read" => "Desktop folder",
        "home" => "All of your files",
        "all-files" => "Every file",
        _ => "Other",
    }
}
