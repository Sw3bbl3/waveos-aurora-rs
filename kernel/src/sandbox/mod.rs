//! Per-app sandboxing: what a program may do beyond computing and drawing
//! its own windows (see docs/developer/sandboxing.md).
//!
//! Accounts keep people apart; a sandbox keeps one person's *apps* apart.
//! A sandboxed process carries a [`Sandbox`]: its app's [`Profile`] (the
//! capabilities and folders its manifest declares, `aurora_abi::sandbox`),
//! the capabilities it has given up ([`Sandbox::restrict`]) and the files
//! and folders the person handed it (the open and save panel, a drop, a
//! paste, opening a document with it). Whether a *guarded* capability may
//! be used is the person's decision, asked once and remembered per account
//! and app ([`consent`]); Settings changes it, and the change reaches
//! running programs at once.
//!
//! The kernel checks at the boundary:
//! - files: every VFS entry point asks [`check_path`] about the normalised
//!   path (after the Linux layer followed its links), so native and Linux
//!   calls are both covered, and descriptors keep working as capabilities;
//! - other calls: the system call handlers ask [`require`] for the
//!   capability they need (network, sound capture, controllers, starting
//!   programs, other apps' processes, notifications, preferences, …);
//! - IPC: local sockets and shared memory of a sandboxed app live in a
//!   namespace of its own ([`ipc_key`]) unless it may talk to other apps.
//!
//! Children inherit their parent's sandbox (a copy: dropping rights later
//! affects only the one dropping them); a process can only ever drop
//! rights. Kernel tasks, kernel code running for the system
//! (`fs::perm::system`) and programs the kernel itself starts (the self
//! tests) are not sandboxed; everything the desktop starts is.

pub mod consent;
pub mod manifests;
pub mod panel;
pub mod syscall;

use crate::fs::{self, perm, Owner};
use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;
use aurora_abi::sandbox as cap;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Where an app's rules come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One of the system's own apps: its manifest comes with the system,
    /// and the folders it declares need no question.
    System,
    /// An installed app with a format-2 manifest.
    Package,
    /// A program without a manifest of its own (format-1 packages, Linux
    /// programs and tools the desktop starts): it may do what its account
    /// may, except use the guarded devices without asking.
    Legacy,
    /// A program that sandboxed itself (`sandbox(RESTRICT)`).
    Restricted,
}

/// An app's rules for one account. Shared by all the processes the app
/// runs as; the person's decisions are looked up live in [`consent`].
pub struct Profile {
    /// `org.example.app` (`legacy.<name>` for programs without one).
    pub id: String,
    /// What the person sees ("Notes").
    pub name: String,
    pub uid: u32,
    pub home: String,
    /// The app's private folder (`~/Library/Containers/<id>`).
    pub container: String,
    /// The app's own files (its package), readable.
    pub bundle: Option<String>,
    /// What the manifest declares.
    pub declared: u64,
    pub kind: Kind,
    /// Older places a system app keeps its files: (path, writable).
    pub paths: Vec<(String, bool)>,
}

/// A file or folder the person gave a process.
#[derive(Clone)]
pub struct Grant {
    pub path: String,
    pub write: bool,
}

/// What one process may do.
pub struct Sandbox {
    pub profile: Arc<Profile>,
    /// Capabilities not given up.
    keep: AtomicU64,
    grants: IrqMutex<Vec<Grant>>,
    /// The process has had a window (see [`window_closed`]).
    pub had_window: AtomicBool,
}

/// How a file is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Its metadata, or walking through it.
    Stat,
    Read,
    /// Writing, creating, removing, renaming, changing owner or mode.
    Write,
}

/// Grants a process may hold (the rest are refused).
const MAX_GRANTS: usize = 256;
/// The folder permissions (`documents` … `home`): the system's own apps
/// have the ones they declare without asking; devices are always asked.
const FOLDER_BITS: u64 = 0x1FFF << 40;

/// `true` if `path` is `dir` or inside it.
pub fn under(path: &str, dir: &str) -> bool {
    if dir == "/" {
        return path.starts_with('/');
    }
    path == dir || (path.starts_with(dir) && path.as_bytes().get(dir.len()) == Some(&b'/'))
}

/// `dir` with `rest` appended (`rest` relative).
fn join(dir: &str, rest: &str) -> String {
    fs::join(dir, rest)
}

/// The home folder of account `uid`.
pub fn home_of(uid: u32) -> String {
    crate::users::db::get(uid).map(|u| u.home).unwrap_or_else(|| String::from("/"))
}

/// Folders in the home folder: (name, read bit, write bit).
const FOLDERS: [(&str, u64, u64); 6] = [
    ("Documents", cap::DOCUMENTS_READ, cap::DOCUMENTS),
    ("Downloads", cap::DOWNLOADS_READ, cap::DOWNLOADS),
    ("Pictures", cap::PICTURES_READ, cap::PICTURES),
    ("Music", cap::MUSIC_READ, cap::MUSIC),
    ("Movies", cap::MOVIES_READ, cap::MOVIES),
    ("Desktop", cap::DESKTOP_READ, cap::DESKTOP),
];

/// The read-only Linux system folders (packages installed on the disk).
const LINUX_SYSTEM: [&str; 8] =
    ["/Linux/bin", "/Linux/etc", "/Linux/lib", "/Linux/lib64", "/Linux/opt", "/Linux/sbin", "/Linux/usr", "/Linux/.links"];

impl Profile {
    /// A profile for account `uid` from what `m` says. Makes the container.
    pub fn new(m: &manifests::AppManifest, uid: u32) -> Arc<Profile> {
        let home = home_of(uid);
        let container = join(&home, &format!("Library/Containers/{}", m.id));
        let p = Arc::new(Profile {
            id: m.id.clone(),
            name: m.name.clone(),
            uid,
            container,
            home,
            bundle: m.bundle.clone(),
            declared: m.declared,
            kind: m.kind,
            paths: m.paths.clone(),
        });
        p.make_container();
        p
    }

    /// The container and its folders, the account's (`rwx------`).
    pub fn make_container(&self) {
        let uid = self.uid;
        perm::system(|| {
            let lib = join(&self.home, "Library");
            let containers = join(&lib, "Containers");
            for (dir, mode) in [(&lib, 0o755), (&containers, 0o755)] {
                if !fs::is_dir(dir) && fs::mkdir(dir).is_ok() {
                    let _ = perm::set_owner_path(dir, Owner { uid, gid: uid, mode });
                }
            }
            if !fs::is_dir(&self.container) && fs::mkdir(&self.container).is_ok() {
                let _ = perm::set_owner_path(&self.container, Owner { uid, gid: uid, mode: 0o700 });
            }
            for sub in CONTAINER_LAYOUT {
                let p = join(&self.container, sub);
                if !fs::is_dir(&p) && fs::mkdir(&p).is_ok() {
                    let _ = perm::set_owner_path(&p, Owner { uid, gid: uid, mode: 0o700 });
                }
            }
        });
    }

    /// Guarded capabilities it needs nobody's answer for: the folders a
    /// system app declares; everything a program that sandboxed itself
    /// kept (it could use them all before).
    fn preapproved(&self) -> u64 {
        match self.kind {
            Kind::System => FOLDER_BITS,
            Kind::Restricted => cap::GUARDED,
            Kind::Package | Kind::Legacy => 0,
        }
    }

    /// The bits of the folder class `path` is in: (read, write).
    fn folder(&self, path: &str) -> Option<(u64, u64)> {
        FOLDERS.iter().find(|(name, _, _)| under(path, &join(&self.home, name))).map(|&(_, r, w)| (r, w))
    }
}

/// The folders every container has: the app's documents, its settings,
/// caches the system may clear, temporary files (Linux programs' `/tmp`),
/// and Linux programs' home folder.
pub const CONTAINER_LAYOUT: [&str; 5] = ["Documents", "Preferences", "Caches", "tmp", "Home"];

impl Sandbox {
    pub fn new(profile: Arc<Profile>) -> Arc<Sandbox> {
        Arc::new(Sandbox {
            profile,
            keep: AtomicU64::new(cap::ALL),
            grants: IrqMutex::new(Vec::new()),
            had_window: AtomicBool::new(false),
        })
    }

    /// A child's copy: the same app, what this one gave up, its grants.
    pub fn child(&self) -> Arc<Sandbox> {
        Arc::new(Sandbox {
            profile: self.profile.clone(),
            keep: AtomicU64::new(self.keep.load(Ordering::Acquire)),
            grants: IrqMutex::new(self.grants.lock().clone()),
            had_window: AtomicBool::new(false),
        })
    }

    /// Gives up every capability not in `keep` (never to come back).
    pub fn restrict(&self, keep: u64) {
        self.keep.fetch_and(keep, Ordering::AcqRel);
    }

    /// Capabilities not given up.
    pub fn kept(&self) -> u64 {
        self.keep.load(Ordering::Acquire)
    }

    /// What it may do right now, without asking anyone.
    pub fn caps(&self) -> u64 {
        let p = &self.profile;
        let (allowed, denied) = consent::decisions(p.uid, &p.id);
        let base = p.declared & self.kept() & !denied;
        (base & !cap::GUARDED) | (base & cap::GUARDED & (allowed | p.preapproved()))
    }

    /// May it use capability `bit` (one bit)? Asks the person the first
    /// time for a guarded one (blocking; refused if nobody can answer).
    pub fn require(&self, bit: u64) -> Result<(), isize> {
        let p = &self.profile;
        if p.declared & self.kept() & bit != bit {
            note_denied(p, cap::name(bit).unwrap_or("capability"));
            return Err(EACCES);
        }
        let (allowed, denied) = consent::decisions(p.uid, &p.id);
        if denied & bit != 0 {
            note_denied(p, cap::name(bit).unwrap_or("capability"));
            return Err(EACCES);
        }
        if bit & cap::GUARDED == 0 || (allowed | p.preapproved()) & bit == bit {
            return Ok(());
        }
        consent::ask(p, bit).inspect_err(|_| note_denied(p, cap::name(bit).unwrap_or("capability")))
    }

    /// Like [`require`](Self::require), but never asks.
    pub fn has(&self, bit: u64) -> bool {
        self.caps() & bit == bit
    }

    /// The person gave it `path` (and what is in it, for a folder).
    pub fn grant(&self, path: &str, write: bool) {
        let path = fs::resolve("/", path);
        let mut g = self.grants.lock();
        if let Some(old) = g.iter_mut().find(|x| x.path == path) {
            old.write |= write;
            return;
        }
        if g.len() < MAX_GRANTS {
            g.push(Grant { path, write });
        }
    }

    /// Whether the person gave it `path` (directly or a folder it is in).
    pub fn granted(&self, path: &str, write: bool) -> bool {
        self.grants.lock().iter().any(|g| under(path, &g.path) && (g.write || !write))
    }

    #[cfg_attr(not(feature = "ktest"), allow(dead_code))] // the self-tests look
    pub fn grants(&self) -> Vec<Grant> {
        self.grants.lock().clone()
    }

    /// May it use `path` this way? (The path is normalised: the VFS and
    /// the Linux layer have taken `.`, `..` and links out of it.)
    pub fn check_path(&self, path: &str, access: Access) -> Result<(), isize> {
        let p = &*self.profile;
        let write = access == Access::Write;
        // The system image: read-only for everyone anyway.
        if under(path, "/System") {
            return if write { self.refuse(path) } else { Ok(()) };
        }
        if under(path, &p.container) {
            return Ok(());
        }
        if !write && p.bundle.as_deref().is_some_and(|b| under(path, b)) {
            return Ok(());
        }
        if self.granted(path, write) || p.paths.iter().any(|(d, w)| under(path, d) && (*w || !write)) {
            return Ok(());
        }
        if !write && LINUX_SYSTEM.iter().any(|d| under(path, d)) {
            return Ok(());
        }
        if self.has_declared(cap::LINUX_HOME) && linux_home(path, p) {
            return self.require(cap::LINUX_HOME);
        }
        if self.has_declared(cap::ALL_FILES) {
            return self.require(cap::ALL_FILES);
        }
        let containers = join(&p.home, "Library/Containers");
        if under(path, &p.home) && !under(path, &containers) {
            if self.has_declared(cap::HOME) {
                return self.require(cap::HOME);
            }
            if let Some((r, w)) = p.folder(path) {
                if self.has_declared(w) {
                    return self.require(w);
                }
                if !write && self.has_declared(r) {
                    return self.require(r);
                }
            }
        }
        // Walking to what it may use: the folders on the way can be looked at.
        if access == Access::Stat && self.leads_somewhere(path, &containers) {
            return Ok(());
        }
        self.refuse(path)
    }

    fn has_declared(&self, bit: u64) -> bool {
        self.profile.declared & self.kept() & bit == bit
    }

    /// `path` is a folder on the way to something it may use.
    fn leads_somewhere(&self, path: &str, containers: &str) -> bool {
        let p = &*self.profile;
        let toward = |target: &str| under(target, path) && target != path;
        if toward(&p.container) || toward(containers) || p.bundle.as_deref().is_some_and(toward) {
            return true;
        }
        if self.grants.lock().iter().any(|g| toward(&g.path)) || p.paths.iter().any(|(d, _)| toward(d)) {
            return true;
        }
        let declared = p.declared & self.kept();
        if declared & (cap::HOME | cap::ALL_FILES) != 0 || LINUX_SYSTEM.iter().any(|d| toward(d)) {
            return true;
        }
        FOLDERS.iter().any(|(name, r, w)| declared & (r | w) != 0 && toward(&join(&p.home, name)))
    }

    fn refuse(&self, path: &str) -> Result<(), isize> {
        note_denied(&self.profile, path);
        Err(EACCES)
    }
}

/// The writable Linux tree as Linux programs had it before sandboxes:
/// `/tmp`, `/var`, `/run` and the rest, and the account's own home folder
/// (not other people's).
fn linux_home(path: &str, p: &Profile) -> bool {
    let user = crate::users::db::get(p.uid).map(|u| u.name).unwrap_or_default();
    ["/Linux/tmp", "/Linux/var", "/Linux/run", "/Linux/root", "/Linux/media", "/Linux/mnt", "/Linux/srv"]
        .iter()
        .any(|d| under(path, d))
        || (!user.is_empty() && under(path, &format!("/Linux/home/{user}")))
}

// ------------------------------------------------------------- the caller

/// The calling process's sandbox (None: unconfined, a kernel task, or
/// kernel code acting for the system).
pub fn current() -> Option<Arc<Sandbox>> {
    if perm::acting_as_system() {
        return None;
    }
    let p = crate::proc::current()?;
    let s = p.sandbox.lock().clone();
    s
}

/// The sandbox of process `pid`.
pub fn of(pid: u32) -> Option<Arc<Sandbox>> {
    crate::proc::get(pid).and_then(|p| p.sandbox.lock().clone())
}

/// May the caller use `path` this way?
pub fn check_path(path: &str, access: Access) -> Result<(), isize> {
    match current() {
        Some(s) => s.check_path(path, access),
        None => Ok(()),
    }
}

/// May the caller use capability `bit`? (Asks the person first for a
/// guarded one.) Unconfined callers may.
pub fn require(bit: u64) -> Result<(), isize> {
    match current() {
        Some(s) => s.require(bit),
        None => Ok(()),
    }
}

/// Whether the caller may use `bit` now, without asking anyone.
pub fn allows(bit: u64) -> bool {
    current().is_none_or(|s| s.has(bit))
}

/// The caller may observe input meant for other apps (for input devices
/// a program reads directly, like the Linux layer's event devices).
#[allow(dead_code)] // for the input devices of the Linux layer
pub fn may_monitor_input() -> bool {
    require(cap::INPUT_MONITOR).is_ok()
}

/// May process `pid` read keyboards and pointers while another program has
/// the focus? Only a sandboxed app with `input-monitoring` that the person
/// allowed (never asks: called while devices are locked, so it uses the
/// decisions already loaded). Unconfined programs follow the focus.
pub fn monitors_input(pid: u32) -> bool {
    let Some(s) = of(pid) else { return false };
    let p = &s.profile;
    if p.declared & s.kept() & cap::INPUT_MONITOR == 0 {
        return false;
    }
    let (allowed, denied) = consent::decisions_cached(p.uid, &p.id);
    denied & cap::INPUT_MONITOR == 0 && (allowed | p.preapproved()) & cap::INPUT_MONITOR != 0
}

/// May the caller make a keyboard or pointer (`/dev/uinput`), whose input
/// goes to whatever has the focus? With `input-monitoring`; programs
/// without a manifest may, as their account always could (Steam Input,
/// remote-desktop tools).
pub fn may_inject_input() -> Result<(), isize> {
    match current() {
        Some(s) if s.profile.kind == Kind::Legacy => Ok(()),
        Some(s) => s.require(cap::INPUT_MONITOR),
        None => Ok(()),
    }
}

/// May the caller name `target` as the program a window of its shows
/// (`win_set_program`), which gives `target` that window's focus? The same
/// rule as signals: a sandboxed bridge only its own app's processes.
pub fn may_designate(target: &crate::proc::Process) -> bool {
    may_signal(target)
}

/// May the caller signal or end `target`? Its own app's processes, or any
/// with the `processes` capability (the account check is `users`').
pub fn may_signal(target: &crate::proc::Process) -> bool {
    let Some(me) = current() else { return true };
    if me.has(cap::PROCESSES) {
        return true;
    }
    let theirs = target.sandbox.lock().clone();
    theirs.is_some_and(|t| t.profile.id == me.profile.id && t.profile.uid == me.profile.uid)
}

/// The name under which the caller's local sockets and shared memory are
/// kept: a sandboxed app without `ipc` gets a namespace of its own.
pub fn ipc_key(name: &str) -> String {
    match ipc_namespace() {
        Some(ns) => format!("\u{1}{ns}\u{1}{name}"),
        None => String::from(name),
    }
}

/// Names in the caller's namespace among `keys` (as the caller sees them).
pub fn ipc_visible(keys: Vec<String>) -> Vec<String> {
    match ipc_namespace() {
        Some(ns) => {
            let prefix = format!("\u{1}{ns}\u{1}");
            keys.into_iter().filter_map(|k| k.strip_prefix(prefix.as_str()).map(String::from)).collect()
        }
        None => keys.into_iter().filter(|k| !k.starts_with('\u{1}')).collect(),
    }
}

fn ipc_namespace() -> Option<String> {
    let s = current()?;
    (!s.has(cap::IPC)).then(|| format!("{}:{}", s.profile.uid, s.profile.id))
}

/// For Linux programs in a sandbox without `linux-home`: where their
/// `/tmp`, `/var/tmp`, `/run` and `/home/<user>` are (inside the container).
pub fn linux_private(guest: &str) -> Option<String> {
    let s = current()?;
    if s.has_declared(cap::LINUX_HOME) {
        return None;
    }
    let c = &s.profile.container;
    let rest = |prefix: &str| guest.strip_prefix(prefix).filter(|r| r.is_empty() || r.starts_with('/'));
    if let Some(r) = rest("/tmp").or_else(|| rest("/var/tmp")) {
        return Some(format!("{c}/tmp{r}"));
    }
    // (udev's records are the system's, made up for everyone: `compat::linux::input`.)
    if let Some(r) = rest("/run").filter(|_| !under(guest, "/run/udev")) {
        return Some(format!("{c}/tmp/run{r}"));
    }
    if let Some(r) = rest("/home") {
        // `/home/<user>/x` → `Home/x` (any user: there is only theirs).
        let r = r.trim_start_matches('/');
        let r = r.split_once('/').map(|(_, x)| x).unwrap_or("");
        return Some(if r.is_empty() { format!("{c}/Home") } else { format!("{c}/Home/{r}") });
    }
    None
}

/// The Linux path for a container path made by [`linux_private`].
pub fn linux_guest(host: &str) -> Option<String> {
    let s = current()?;
    if s.has_declared(cap::LINUX_HOME) {
        return None;
    }
    let c = &s.profile.container;
    let rest = host.strip_prefix(c.as_str())?;
    let user = crate::compat::linux::fs::account_user();
    let after = |p: &str| rest.strip_prefix(p).filter(|r| r.is_empty() || r.starts_with('/'));
    if let Some(r) = after("/tmp/run") {
        return Some(format!("/run{r}"));
    }
    if let Some(r) = after("/tmp") {
        return Some(format!("/tmp{r}"));
    }
    after("/Home").map(|r| format!("/home/{user}{r}"))
}

// ------------------------------------------------------------- launching

/// The sandbox a program the desktop starts for the person runs in:
/// its app's, or the default profile for programs without a manifest.
pub fn for_launch(path: &str) -> Arc<Sandbox> {
    let uid = crate::users::session().uid;
    let m = manifests::for_program(path).unwrap_or_else(|| manifests::legacy(path));
    Sandbox::new(Profile::new(&m, uid))
}

/// How a program a process starts is confined.
pub enum Confine {
    /// As its parent (a copy of the parent's sandbox, or none).
    Inherit,
    /// In this sandbox (None: unconfined; only the kernel starts those).
    With(Option<Arc<Sandbox>>),
}

/// `spawn` by the calling process `me` of `path` with `args`: another
/// app (with `launch`) runs as that app, given the files among `args` the
/// caller may use itself; anything else (with `spawn`) inherits the
/// caller's sandbox.
pub fn spawn_policy(me: &crate::proc::Process, path: &str, args: &[&str]) -> Result<Confine, isize> {
    let Some(sb) = me.sandbox.lock().clone() else { return Ok(Confine::Inherit) };
    if let Some(m) = manifests::for_program(path).filter(|m| m.id != sb.profile.id) {
        if sb.has_declared(cap::LAUNCH) {
            sb.require(cap::LAUNCH)?;
            let child = Sandbox::new(Profile::new(&m, sb.profile.uid));
            pass_documents(&sb, &child, args);
            return Ok(Confine::With(Some(child)));
        }
    }
    if fs::resolve("/", path) != me.path() {
        sb.require(cap::SPAWN)?;
    }
    Ok(Confine::Inherit)
}

/// Grants `to` the documents among `args` that `from` may use itself (as
/// far as `from` may: reading, or also writing).
pub fn pass_documents(from: &Sandbox, to: &Sandbox, args: &[&str]) {
    for a in args.iter().filter(|a| a.starts_with('/')) {
        let path = fs::resolve("/", a);
        if from.check_path(&path, Access::Write).is_ok() {
            to.grant(&path, true);
        } else if from.check_path(&path, Access::Read).is_ok() {
            to.grant(&path, false);
        }
    }
}

/// The administrator capability a sandboxed process keeps: only system
/// apps that declare `admin`, and programs without a manifest (as before).
pub fn narrow_cred(s: &Sandbox, cred: &mut crate::users::Cred) {
    let p = &s.profile;
    if p.kind == Kind::Legacy || (p.declared & s.kept() & cap::ADMIN != 0) {
        return;
    }
    cred.admin = false;
    cred.elevated_until = 0;
    let groups: Vec<u32> = cred.groups().iter().copied().filter(|&g| g != crate::users::ADMIN_GID).collect();
    cred.set_groups(&groups);
}

// ------------------------------------------------------- running on

/// Programs to end if they still have no window (pid, deadline).
static WINDOWLESS: IrqMutex<Vec<(u32, u64)>> = IrqMutex::new(Vec::new());
/// How long an app without `background` may stay without a window.
const WINDOWLESS_MS: u64 = 10_000;

/// A window of `pid` closed: an app that may not run in the background
/// ends once it has had no window for a while — unless it still shows
/// something some other way (see [`busy`]).
pub fn window_closed(pid: u32) {
    let Some(s) = of(pid) else { return };
    if matches!(s.profile.kind, Kind::Legacy | Kind::Restricted) || s.has(cap::BACKGROUND) {
        return;
    }
    if busy(pid, &s) {
        return;
    }
    WINDOWLESS.lock().push((pid, crate::time::uptime_ms() + WINDOWLESS_MS));
    crate::sched::wake(SWEEPER.load(Ordering::Relaxed));
}

/// A window of `pid` opened.
pub fn window_opened(pid: u32) {
    if let Some(s) = of(pid) {
        s.had_window.store(true, Ordering::Release);
    }
    WINDOWLESS.lock().retain(|(p, _)| *p != pid);
}

/// Whether windowless `pid` (of sandbox `s`) is still at work for the
/// person: a process of its app has a window (a launcher whose game, or a
/// bridge whose client, shows one), it has programs of its own running
/// (it waits for them, or they start up), or it drives a display itself
/// (a full-screen DRM program never has a window).
fn busy(pid: u32, s: &Sandbox) -> bool {
    let same_app = |other: u32| of(other).is_some_and(|o| o.profile.id == s.profile.id && o.profile.uid == s.profile.uid);
    if crate::gui::server::windows().iter().any(|(_, p, _)| *p == pid || same_app(*p)) {
        return true;
    }
    let all = crate::proc::all();
    if all.iter().any(|c| c.parent == pid && c.exit_code().is_none()) {
        return true;
    }
    all.iter().find(|p| p.pid == pid).is_some_and(|p| {
        p.fds.lock().iter().flatten().any(|h| match &**h {
            crate::proc::Handle::Special(sp) => matches!(&*sp.lock(), crate::compat::linux::Special::Drm(_)),
            _ => false,
        })
    })
}

static SWEEPER: AtomicU64 = AtomicU64::new(u64::MAX);

/// Starts the task that ends windowless apps.
pub fn init() {
    SWEEPER.store(crate::sched::spawn("sandbox", sweeper), Ordering::Relaxed);
}

fn sweeper() {
    loop {
        let next = WINDOWLESS.lock().iter().map(|(_, t)| *t).min();
        let now = crate::time::uptime_ms();
        match next {
            None => crate::sched::wait_until(60_000, || !WINDOWLESS.lock().is_empty()),
            Some(t) if t > now => crate::sched::sleep_ms((t - now).min(60_000)),
            Some(_) => {
                let due: Vec<u32> = {
                    let mut w = WINDOWLESS.lock();
                    let due = w.iter().filter(|(_, t)| *t <= now).map(|(p, _)| *p).collect();
                    w.retain(|(_, t)| *t > now);
                    due
                };
                for pid in due {
                    let Some(s) = of(pid) else { continue };
                    let running = crate::proc::get(pid).is_some_and(|p| p.exit_code().is_none());
                    if running && !busy(pid, &s) && !s.has(cap::BACKGROUND) {
                        log!("sandbox", "pid {} has no window and may not run in the background: ending it", pid);
                        let _ = crate::proc::kill(pid);
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------- auditing

/// The latest refusals: (app id, what).
static DENIED: IrqMutex<VecDeque<(String, String)>> = IrqMutex::new(VecDeque::new());
const DENIED_KEPT: usize = 64;

fn note_denied(p: &Profile, what: &str) {
    let fresh = {
        let mut d = DENIED.lock();
        let fresh = !d.iter().any(|(id, w)| *id == p.id && w == what);
        if fresh {
            if d.len() >= DENIED_KEPT {
                d.pop_front();
            }
            d.push_back((p.id.clone(), String::from(what)));
        }
        fresh
    };
    if fresh {
        log!("sandbox", "{} may not use {}", p.id, what);
    }
}

/// The latest refusals, oldest first.
pub fn denied() -> Vec<(String, String)> {
    DENIED.lock().iter().cloned().collect()
}

#[cfg(feature = "ktest")]
pub mod selftest;
