//! The person's decisions: which guarded capabilities each app may use
//! (asked once, the first time the app tries), what they switched off in
//! Settings, and the bookmarks apps keep to files they were given.
//!
//! Each account's decisions are kept in `/Security/Privacy/<uid>.conf`
//! (the system's, like the account database: no app can write them), one
//! line each:
//!
//! ```text
//! decision<TAB><app id><TAB><permission><TAB>allow|deny
//! bookmark<TAB><token><TAB><app id><TAB>rw|r<TAB><path>
//! ```
//!
//! Asking works like the administrator prompt: the asking task waits while
//! the window server shows the question (`gui::apps::ConsentDialog`).

use super::Profile;
use crate::fs::{self, perm};
use crate::sync::IrqMutex;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::err::*;
use aurora_abi::sandbox as cap;

const DIR: &str = "/Security/Privacy";
/// How long a question waits for an answer.
const TIMEOUT_MS: u64 = 120_000;
/// Bookmarks an account keeps (the oldest go first).
const MAX_BOOKMARKS: usize = 1024;

#[derive(Clone)]
pub struct Bookmark {
    pub token: String,
    pub app: String,
    pub path: String,
    pub write: bool,
}

#[derive(Clone, Default)]
struct Store {
    /// (app id, bit) → allowed.
    decisions: BTreeMap<(String, u64), bool>,
    bookmarks: Vec<Bookmark>,
}

static STORES: IrqMutex<BTreeMap<u32, Store>> = IrqMutex::new(BTreeMap::new());

fn path_of(uid: u32) -> String {
    format!("{DIR}/{uid}.conf")
}

fn parse(text: &str) -> Store {
    let mut s = Store::default();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        match f.as_slice() {
            ["decision", app, perm, answer] => {
                if let Some(bit) = cap::bit(perm) {
                    s.decisions.insert((String::from(*app), bit), *answer == "allow");
                }
            }
            ["bookmark", token, app, mode, path] if path.starts_with('/') => s.bookmarks.push(Bookmark {
                token: String::from(*token),
                app: String::from(*app),
                path: String::from(*path),
                write: *mode == "rw",
            }),
            _ => {}
        }
    }
    s
}

fn encode(s: &Store) -> String {
    let mut out = String::from("# Privacy decisions (see docs/developer/sandboxing.md)\n");
    for ((app, bit), allowed) in &s.decisions {
        let name = cap::name(*bit).unwrap_or("?");
        out += &format!("decision\t{app}\t{name}\t{}\n", if *allowed { "allow" } else { "deny" });
    }
    for b in &s.bookmarks {
        out += &format!("bookmark\t{}\t{}\t{}\t{}\n", b.token, b.app, if b.write { "rw" } else { "r" }, b.path);
    }
    out
}

/// Loads account `uid`'s decisions once.
fn load(uid: u32) {
    if STORES.lock().contains_key(&uid) {
        return;
    }
    let text = perm::system(|| fs::read_all(&path_of(uid)).ok());
    let store = text.map(|t| parse(&String::from_utf8_lossy(&t))).unwrap_or_default();
    STORES.lock().entry(uid).or_insert(store);
}

fn save(uid: u32) {
    let Some(text) = STORES.lock().get(&uid).map(encode) else { return };
    perm::system(|| {
        if !fs::is_dir(DIR) {
            let _ = perm::ensure_system_dir("/Security");
            let _ = perm::ensure_system_dir(DIR);
        }
        if let Err(e) = fs::write_atomic(&path_of(uid), text.as_bytes()) {
            log!("sandbox", "could not keep the privacy decisions: {}", aurora_abi::err::name(e));
        }
    });
}

/// (allowed, denied) guarded capabilities of app `id` for account `uid`.
pub fn decisions(uid: u32, id: &str) -> (u64, u64) {
    load(uid);
    let stores = STORES.lock();
    let Some(s) = stores.get(&uid) else { return (0, 0) };
    let (mut allowed, mut denied) = (0, 0);
    for ((app, bit), yes) in s.decisions.range((String::from(id), 0)..) {
        if app != id {
            break;
        }
        if *yes {
            allowed |= bit;
        } else {
            denied |= bit;
        }
    }
    (allowed, denied)
}

/// [`decisions`] without reading the file: (0, 0) if the account's
/// decisions are not loaded yet (for callers that may not block).
pub fn decisions_cached(uid: u32, id: &str) -> (u64, u64) {
    let stores = STORES.lock();
    let Some(s) = stores.get(&uid) else { return (0, 0) };
    s.decisions
        .range((String::from(id), 0)..)
        .take_while(|((app, _), _)| app == id)
        .fold((0, 0), |(a, d), ((_, bit), yes)| if *yes { (a | bit, d) } else { (a, d | bit) })
}

/// Records a decision (None: forget it, so the app asks again).
pub fn decide(uid: u32, id: &str, bit: u64, allowed: Option<bool>) {
    load(uid);
    let changed = {
        let mut stores = STORES.lock();
        let s = stores.entry(uid).or_default();
        let key = (String::from(id), bit);
        match allowed {
            Some(a) => s.decisions.insert(key, a) != Some(a),
            None => s.decisions.remove(&key).is_some(),
        }
    };
    if changed {
        save(uid);
        log!("sandbox", "{} {} for {}", id, cap::name(bit).unwrap_or("?"), match allowed {
            Some(true) => "allowed",
            Some(false) => "refused",
            None => "forgotten",
        });
    }
}

/// Every app account `uid` has decided something about: (app, allowed, denied).
pub fn all(uid: u32) -> Vec<(String, u64, u64)> {
    load(uid);
    let stores = STORES.lock();
    let mut out: Vec<(String, u64, u64)> = Vec::new();
    if let Some(s) = stores.get(&uid) {
        for ((app, bit), yes) in &s.decisions {
            if out.last().is_none_or(|(a, _, _)| a != app) {
                out.push((app.clone(), 0, 0));
            }
            let last = out.last_mut().unwrap();
            if *yes {
                last.1 |= bit;
            } else {
                last.2 |= bit;
            }
        }
    }
    out
}

// ------------------------------------------------------------- bookmarks

/// Keeps a bookmark to `path` for app `app` and returns its token.
pub fn add_bookmark(uid: u32, app: &str, path: &str, write: bool) -> String {
    load(uid);
    let existing = STORES.lock().get(&uid).and_then(|s| {
        s.bookmarks.iter().find(|b| b.app == app && b.path == path && b.write == write).map(|b| b.token.clone())
    });
    if let Some(t) = existing {
        return t;
    }
    let mut raw = [0u8; 16];
    crate::random::fill(&mut raw);
    let token: String = raw.iter().map(|b| format!("{b:02x}")).collect();
    {
        let mut stores = STORES.lock();
        let s = stores.entry(uid).or_default();
        if s.bookmarks.len() >= MAX_BOOKMARKS {
            s.bookmarks.remove(0);
        }
        s.bookmarks.push(Bookmark { token: token.clone(), app: String::from(app), path: String::from(path), write });
    }
    save(uid);
    token
}

/// The bookmark `token` of app `app`.
pub fn bookmark(uid: u32, app: &str, token: &str) -> Option<Bookmark> {
    load(uid);
    STORES.lock().get(&uid)?.bookmarks.iter().find(|b| b.app == app && b.token == token).cloned()
}

pub fn bookmarks(uid: u32) -> Vec<Bookmark> {
    load(uid);
    STORES.lock().get(&uid).map(|s| s.bookmarks.clone()).unwrap_or_default()
}

pub fn forget_bookmark(uid: u32, app: &str, token: &str) -> bool {
    load(uid);
    let gone = {
        let mut stores = STORES.lock();
        let Some(s) = stores.get_mut(&uid) else { return false };
        let before = s.bookmarks.len();
        s.bookmarks.retain(|b| !(b.app == app && b.token == token));
        s.bookmarks.len() != before
    };
    if gone {
        save(uid);
    }
    gone
}

// ------------------------------------------------------------- asking

/// A question waiting for the prompt.
#[derive(Clone)]
pub struct Request {
    pub id: u64,
    pub pid: u32,
    pub uid: u32,
    pub app: String,
    /// The app's name, as the person knows it.
    pub name: String,
    pub bit: u64,
}

struct Slot {
    req: Request,
    /// (allowed, remember it).
    answer: Option<(bool, bool)>,
}

static QUEUE: IrqMutex<VecDeque<Slot>> = IrqMutex::new(VecDeque::new());
static NEXT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);
/// Self-tests answer for the person: Some(answer).
static AUTO: IrqMutex<Option<bool>> = IrqMutex::new(None);

/// Self-tests: answer every question with `answer` (None: nobody answers).
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn set_auto_answer(answer: Option<bool>) {
    *AUTO.lock() = answer;
}

/// Asks whether app `p` may use `bit` (blocks until answered). The answer
/// is remembered; no answer (nobody to ask, timeout) is a refusal that is not.
pub fn ask(p: &Profile, bit: u64) -> Result<(), isize> {
    let auto = *AUTO.lock();
    if let Some(a) = auto {
        decide(p.uid, &p.id, bit, Some(a));
        return if a { Ok(()) } else { Err(EACCES) };
    }
    let blocking_ok = x86_64::instructions::interrupts::are_enabled();
    let Some(me) = crate::proc::current() else { return Err(EACCES) };
    if !crate::gui::server::running() || !blocking_ok {
        return Err(EACCES);
    }
    let decided = || {
        let (a, d) = decisions(p.uid, &p.id);
        (a | d) & bit != 0
    };
    // The same question already waits: its answer is ours.
    let same = |s: &Slot| s.req.uid == p.uid && s.req.app == p.id && s.req.bit == bit;
    let waiting = QUEUE.lock().iter().any(same);
    let deadline = crate::time::uptime_ms() + TIMEOUT_MS;
    if waiting {
        while !decided() && QUEUE.lock().iter().any(same) && !crate::proc::interrupted() {
            let left = deadline.saturating_sub(crate::time::uptime_ms());
            if left == 0 {
                break;
            }
            crate::sched::wait_until(left.min(200), || decided() || crate::proc::interrupted());
        }
        let (a, _) = decisions(p.uid, &p.id);
        return if a & bit != 0 { Ok(()) } else { Err(EACCES) };
    }
    let id = NEXT.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    let req = Request { id, pid: me.pid, uid: p.uid, app: p.id.clone(), name: p.name.clone(), bit };
    QUEUE.lock().push_back(Slot { req, answer: None });
    crate::gui::server::command(crate::gui::server::Command::Consent);
    let answered = || QUEUE.lock().iter().find(|s| s.req.id == id).is_none_or(|s| s.answer.is_some());
    while !answered() && !crate::proc::interrupted() && crate::time::uptime_ms() < deadline {
        crate::sched::wait_until(deadline.saturating_sub(crate::time::uptime_ms()), || {
            answered() || crate::proc::interrupted()
        });
    }
    let answer = {
        let mut q = QUEUE.lock();
        let i = q.iter().position(|s| s.req.id == id);
        i.and_then(|i| q.remove(i)).and_then(|s| s.answer)
    };
    crate::gui::server::command(crate::gui::server::Command::Consent);
    match answer {
        Some((a, remember)) => {
            if remember {
                decide(p.uid, &p.id, bit, Some(a));
            }
            if a {
                Ok(())
            } else {
                Err(EACCES)
            }
        }
        None => Err(EACCES),
    }
}

/// The question the prompt should show now.
pub fn pending() -> Option<Request> {
    QUEUE.lock().iter().find(|s| s.answer.is_none()).map(|s| s.req.clone())
}

/// The prompt's answer to request `id` (a dismissed prompt is a refusal
/// that is not remembered).
pub fn answer(id: u64, allowed: bool, remember: bool) {
    let pid = {
        let mut q = QUEUE.lock();
        let Some(s) = q.iter_mut().find(|s| s.req.id == id) else { return };
        s.answer = Some((allowed, remember));
        s.req.pid
    };
    crate::sched::wake_process(pid);
}

/// What the prompt asks about `bit`: "use the microphone".
pub fn question(bit: u64) -> &'static str {
    match bit {
        cap::MICROPHONE => "use the microphone",
        cap::CAMERA => "use the camera",
        cap::SCREEN_CAPTURE => "record this computer's screen",
        cap::INPUT_MONITOR => "observe keys you press in other apps",
        cap::USB => "use USB devices directly",
        cap::DOCUMENTS | cap::DOCUMENTS_READ => "access files in your Documents folder",
        cap::DOWNLOADS | cap::DOWNLOADS_READ => "access files in your Downloads folder",
        cap::PICTURES | cap::PICTURES_READ => "access files in your Pictures folder",
        cap::MUSIC | cap::MUSIC_READ => "access files in your Music folder",
        cap::MOVIES | cap::MOVIES_READ => "access files in your Movies folder",
        cap::DESKTOP | cap::DESKTOP_READ => "access files on your Desktop",
        cap::HOME => "access all of your files",
        _ => "use a protected part of this computer",
    }
}
