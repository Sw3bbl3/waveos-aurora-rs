//! Window-owned transfers. Only the worker performs I/O; the UI polls snapshots.
use alloc::{format, string::String, sync::Arc, vec, vec::Vec};
use core::sync::atomic::{AtomicU64, Ordering};
static CONFLICT_ID: AtomicU64 = AtomicU64::new(1);
use corekit::{
    abi::{err, open},
    fs,
    sync::Mutex,
    thread::JoinHandle,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Choice {
    Replace,
    KeepBoth,
    Skip,
}
#[derive(Clone)]
pub struct Conflict {
    pub name: String,
    pub replace: bool,
}
#[derive(Default)]
pub struct State {
    pub running: bool,
    pub cancel: bool,
    pub committing: bool,
    pub current: String,
    pub bytes: u64,
    pub completed: usize,
    pub total: usize,
    pub skipped: usize,
    pub errors: Vec<String>,
    pub moved: Vec<String>,
    pub conflict_serial: u64,
    pub conflict: Option<Conflict>,
    pub decision: Option<Choice>,
    pub apply_all: bool,
    pub remembered: Option<Choice>,
    pub summary: String,
}
pub struct Manager {
    pub state: Arc<Mutex<State>>,
    handle: Option<JoinHandle<()>>,
}
pub type Shared = Arc<Mutex<Manager>>;
impl Manager {
    pub fn new() -> Shared {
        Arc::new(Mutex::new(Self { state: Arc::new(Mutex::new(State::default())), handle: None }))
    }
    pub fn reap(&mut self) -> bool {
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            self.handle.take().unwrap().join();
            return true;
        }
        false
    }
    pub fn busy(&self) -> bool {
        self.handle.is_some()
    }
    pub fn start(&mut self, sources: Vec<(String, String)>, moving: bool) -> bool {
        self.reap();
        if self.busy() {
            return false;
        }
        *self.state.lock() = State { running: true, total: sources.len(), ..State::default() };
        let state = self.state.clone();
        match corekit::thread::spawn(move || run(&state, sources, moving)) {
            Ok(h) => {
                self.handle = Some(h);
                true
            }
            Err(e) => {
                let mut s = self.state.lock();
                s.running = false;
                s.summary = format!("Couldn't start transfer: {e}");
                false
            }
        }
    }
}
fn cancelled(s: &Arc<Mutex<State>>) -> bool {
    s.lock().cancel
}
fn check(s: &Arc<Mutex<State>>) -> Result<(), String> {
    if cancelled(s) {
        Err(String::from("Cancelled"))
    } else {
        Ok(())
    }
}
fn unique(dir: &str, name: &str) -> String {
    fs::unique_name(dir, name, "")
}
fn decide(s: &Arc<Mutex<State>>, name: &str, replace: bool) -> Option<Choice> {
    {
        let mut state = s.lock();
        if let Some(c) = state.remembered {
            if c != Choice::Replace || replace {
                return Some(c);
            }
        }
        state.conflict_serial = CONFLICT_ID.fetch_add(1, Ordering::Relaxed);
        state.conflict = Some(Conflict { name: String::from(name), replace });
        state.decision = None;
    }
    loop {
        {
            let mut state = s.lock();
            if state.cancel {
                state.conflict = None;
                return None;
            }
            if let Some(c) = state.decision.take() {
                if c == Choice::Replace && !replace {
                    continue;
                }
                if state.apply_all {
                    state.remembered = Some(c);
                }
                state.conflict = None;
                return Some(c);
            }
        }
        corekit::time::sleep_ms(50);
    }
}
// A single reusable buffer per transfer, including recursive directory copies.
fn copy_tree(
    src: &str,
    dst: &str,
    buffer: &mut [u8],
    s: &Arc<Mutex<State>>,
    depth: usize,
    manifest: &mut Vec<(String, u64, u64)>,
) -> Result<(), String> {
    check(s)?;
    if depth > 128 {
        return Err(String::from("Folder nesting exceeds 128 levels"));
    }
    let st = fs::stat(src).map_err(|e| format!("{src}: {e}"))?;
    manifest.push((String::from(src), st.size, st.mtime));
    if st.kind == corekit::abi::KIND_DIR {
        fs::mkdir(dst).map_err(|e| format!("{dst}: {e}"))?;
        for entry in fs::read_dir(src).map_err(|e| format!("{src}: {e}"))? {
            copy_tree(&fs::join(src, &entry.name), &fs::join(dst, &entry.name), buffer, s, depth + 1, manifest)?;
        }
    } else {
        let mut input = fs::File::open(src, open::READ).map_err(|e| format!("{src}: {e}"))?;
        let mut output =
            fs::File::open(dst, open::WRITE | open::CREATE | open::EXCLUSIVE).map_err(|e| format!("{dst}: {e}"))?;
        let mut copied = 0;
        loop {
            check(s)?;
            let n = input.read(buffer).map_err(|e| format!("Read {src}: {e}"))?;
            if n == 0 {
                break;
            }
            let mut written = 0;
            while written < n {
                check(s)?;
                let count = output.write(&buffer[written..n]).map_err(|e| format!("Write {dst}: {e}"))?;
                if count == 0 {
                    return Err(String::from("Write returned zero bytes"));
                }
                written += count;
                copied += count as u64;
                s.lock().bytes += count as u64;
            }
        }
        let after = fs::stat(src).map_err(|e| format!("{src}: {e}"))?;
        if copied != st.size || after.size != st.size || after.mtime != st.mtime {
            return Err(format!("Source changed while copying: {src}"));
        }
    }
    Ok(())
}
fn one(src: &str, dir: &str, moving: bool, buffer: &mut [u8], s: &Arc<Mutex<State>>) -> Result<bool, String> {
    check(s)?;
    let src = fs::resolve("/", src);
    let dir = fs::resolve("/", dir);
    if dir == src || dir.starts_with(&(String::from(src.trim_end_matches('/')) + "/")) {
        return Err(String::from("Can't put a folder inside itself"));
    }
    if moving && fs::parent(&src) == dir {
        return Ok(false);
    }
    let name = fs::file_name(&src);
    let mut dst = fs::join(&dir, name);
    s.lock().current = src.clone();
    let mut allow_replace = false;
    if fs::exists(&dst) {
        let replace = !fs::is_dir(&src) && !fs::is_dir(&dst) && src != dst;
        match decide(s, name, replace) {
            Some(Choice::Replace) => allow_replace = true,
            Some(Choice::KeepBoth) => {
                let (stem, ext) = super::split_ext(name, fs::is_dir(&src));
                dst = fs::unique_name(&dir, &format!("{stem} copy"), &ext);
            }
            Some(Choice::Skip) => return Ok(false),
            None => return Err(String::from("Cancelled")),
        }
    }
    check(s)?;
    // Rename is the fast path; cross-volume EINVAL falls through to staging.
    if moving {
        match fs::rename(&src, &dst) {
            Ok(()) => {
                s.lock().moved.push(src);
                fs::try_sync().map_err(|e| format!("Moved, but sync failed: {e}"))?;
                return Ok(true);
            }
            Err(e) if e.is(err::EINVAL) => {}
            Err(e) => return Err(format!("Move: {e}")),
        }
    }
    let stage = unique(&dir, &format!(".waveos-transfer-{}", corekit::time::uptime_ms()));
    fs::mkdir(&stage).map_err(|e| format!("Create staging folder: {e}"))?;
    let payload = fs::join(&stage, "payload");
    let mut manifest = Vec::new();
    let result = (|| {
        copy_tree(&src, &payload, buffer, s, 0, &mut manifest)?;
        check(s)?;
        fs::try_sync().map_err(|e| format!("Sync before publish: {e}"))?;
        check(s)?;
        if !allow_replace && fs::exists(&dst) {
            return Err(String::from("Destination appeared during copy; retry to choose a conflict action"));
        }
        s.lock().committing = true;
        fs::rename(&payload, &dst).map_err(|e| format!("Publish: {e}"))?;
        fs::try_sync().map_err(|e| format!("Destination saved; sync failed, source retained: {e}"))?;
        if moving {
            // Remove only entries we actually copied. A newly added child
            // makes directory removal fail rather than being silently deleted.
            for (path, size, mtime) in manifest.iter().rev() {
                let st = fs::stat(path).map_err(|e| format!("Destination saved; source retained: {e}"))?;
                if st.kind != corekit::abi::KIND_DIR && (st.size != *size || st.mtime != *mtime) {
                    return Err(format!("Destination saved; source changed and retained: {path}"));
                }
                fs::remove(path).map_err(|e| format!("Destination saved; source retained at {path}: {e}"))?;
            }
            s.lock().moved.push(src.clone());
        }
        Ok(true)
    })();
    s.lock().committing = false;
    if fs::exists(&stage) {
        if let Err(e) = fs::remove_all(&stage) {
            s.lock().errors.push(format!("Temporary item retained at {stage}: {e}"));
        }
    }
    result
}
pub fn run(s: &Arc<Mutex<State>>, sources: Vec<(String, String)>, moving: bool) {
    let mut buffer = vec![0; 64 * 1024];
    for (src, dir) in sources {
        if cancelled(s) {
            break;
        }
        match one(&src, &dir, moving, &mut buffer, s) {
            Ok(true) => s.lock().completed += 1,
            Ok(false) => s.lock().skipped += 1,
            Err(e) => {
                if e != "Cancelled" {
                    s.lock().errors.push(e);
                }
            }
        }
    }
    let mut state = s.lock();
    state.running = false;
    state.conflict = None;
    state.summary = format!(
        "{}: {} completed, {} skipped, {} errors",
        if state.cancel { "Cancelled" } else { "Finished" },
        state.completed,
        state.skipped,
        state.errors.len()
    );
}
