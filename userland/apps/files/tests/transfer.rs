//! Run: rustc --edition=2021 --test userland/apps/files/tests/transfer.rs -o target/file-transfer-tests
//! Uses the production worker with a host I/O adapter and injected failures.
extern crate alloc;
extern crate self as corekit;
use std::{
    io::{Read, Write},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
static SHORT: AtomicUsize = AtomicUsize::new(usize::MAX);
static FAIL_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);
static WRITTEN: AtomicUsize = AtomicUsize::new(0);
static MAX_BUFFER: AtomicUsize = AtomicUsize::new(0);
static FAIL_SYNC: AtomicUsize = AtomicUsize::new(0);
static SYNC_CALLS: AtomicUsize = AtomicUsize::new(0);
static FAIL_SYNC_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
static FAIL_CLEANUP: AtomicUsize = AtomicUsize::new(0);
static ZERO: AtomicUsize = AtomicUsize::new(0);
static CROSS: AtomicUsize = AtomicUsize::new(0);
static CANCEL_AT: AtomicUsize = AtomicUsize::new(usize::MAX);
static CANCEL_STATE: std::sync::Mutex<Option<Arc<sync::Mutex<transfer::State>>>> = std::sync::Mutex::new(None);
#[derive(Debug)]
pub struct Error(pub i32);
impl Error {
    pub fn is(&self, n: i32) -> bool {
        self.0 == n
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "I/O {}", self.0)
    }
}
pub type Result<T> = std::result::Result<T, Error>;
fn io<T>(r: std::io::Result<T>) -> Result<T> {
    r.map_err(|e| Error(e.raw_os_error().unwrap_or(5)))
}
pub mod abi {
    pub const KIND_DIR: u32 = 2;
    pub mod err {
        pub const EINVAL: i32 = 22;
    }
    pub mod open {
        pub const READ: usize = 1;
        pub const WRITE: usize = 2;
        pub const CREATE: usize = 4;
        pub const EXCLUSIVE: usize = 32;
    }
}
pub mod sync {
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub fn new(v: T) -> Self {
            Self(std::sync::Mutex::new(v))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}
pub mod thread {
    pub struct JoinHandle<T>(std::thread::JoinHandle<T>);
    impl<T> JoinHandle<T> {
        pub fn join(self) -> T {
            self.0.join().ok().unwrap()
        }
        pub fn is_finished(&self) -> bool {
            self.0.is_finished()
        }
    }
    pub fn spawn<F: FnOnce() -> T + Send + 'static, T: Send + 'static>(f: F) -> super::Result<JoinHandle<T>> {
        Ok(JoinHandle(std::thread::spawn(f)))
    }
}
pub mod time {
    pub fn uptime_ms() -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64
    }
    pub fn sleep_ms(ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}
pub mod fs {
    use super::*;
    pub struct Stat {
        pub kind: u32,
        pub size: u64,
        pub mtime: u64,
    }
    pub struct Entry {
        pub name: String,
    }
    pub struct File(std::fs::File);
    impl File {
        pub fn open(p: &str, f: usize) -> Result<Self> {
            let mut o = std::fs::OpenOptions::new();
            o.read(f & 1 != 0).write(f & 2 != 0).create_new(f & 32 != 0);
            io(o.open(p)).map(Self)
        }
        pub fn read(&mut self, b: &mut [u8]) -> Result<usize> {
            MAX_BUFFER.fetch_max(b.len(), Ordering::SeqCst);
            io(self.0.read(b))
        }
        pub fn write(&mut self, b: &[u8]) -> Result<usize> {
            if ZERO.load(Ordering::SeqCst) > 0 {
                return Ok(0);
            }
            let before = WRITTEN.load(Ordering::SeqCst);
            if before >= FAIL_AFTER.load(Ordering::SeqCst) {
                return Err(Error(28));
            }
            if before >= CANCEL_AT.load(Ordering::SeqCst) {
                if let Some(s) = CANCEL_STATE.lock().unwrap().as_ref() {
                    s.lock().cancel = true;
                }
            }
            let n = b.len().min(SHORT.load(Ordering::SeqCst));
            let n = io(self.0.write(&b[..n]))?;
            WRITTEN.fetch_add(n, Ordering::SeqCst);
            Ok(n)
        }
    }
    pub fn stat(p: &str) -> Result<Stat> {
        io(std::fs::metadata(p)).map(|m| Stat {
            kind: if m.is_dir() { 2 } else { 1 },
            size: m.len(),
            mtime: m.modified().unwrap().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64,
        })
    }
    pub fn is_dir(p: &str) -> bool {
        Path::new(p).is_dir()
    }
    pub fn exists(p: &str) -> bool {
        Path::new(p).exists()
    }
    pub fn mkdir(p: &str) -> Result<()> {
        io(std::fs::create_dir(p))
    }
    pub fn read_dir(p: &str) -> Result<Vec<Entry>> {
        io(std::fs::read_dir(p))?
            .map(|e| io(e).map(|e| Entry { name: e.file_name().to_string_lossy().into() }))
            .collect()
    }
    pub fn remove(p: &str) -> Result<()> {
        if is_dir(p) {
            io(std::fs::remove_dir(p))
        } else {
            io(std::fs::remove_file(p))
        }
    }
    pub fn remove_all(p: &str) -> Result<()> {
        if FAIL_CLEANUP.load(Ordering::SeqCst) > 0 {
            return Err(Error(5));
        }
        if is_dir(p) {
            io(std::fs::remove_dir_all(p))
        } else {
            remove(p)
        }
    }
    pub fn rename(a: &str, b: &str) -> Result<()> {
        if CROSS.load(Ordering::SeqCst) > 0 && !a.contains("/other/") && b.contains("/other/") {
            return Err(Error(22));
        }
        io(std::fs::rename(a, b))
    }
    pub fn try_sync() -> Result<()> {
        let call = SYNC_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
        if FAIL_SYNC.load(Ordering::SeqCst) > 0 || call == FAIL_SYNC_AT.load(Ordering::SeqCst) {
            Err(Error(5))
        } else {
            Ok(())
        }
    }
    pub fn join(a: &str, b: &str) -> String {
        Path::new(a).join(b).to_string_lossy().into()
    }
    pub fn resolve(_: &str, p: &str) -> String {
        p.into()
    }
    pub fn file_name(p: &str) -> &str {
        Path::new(p).file_name().unwrap().to_str().unwrap()
    }
    pub fn parent(p: &str) -> &str {
        Path::new(p).parent().unwrap().to_str().unwrap()
    }
    pub fn unique_name(dir: &str, stem: &str, ext: &str) -> String {
        for i in 0..1000 {
            let name = if i == 0 { format!("{stem}{ext}") } else { format!("{stem} {i}{ext}") };
            let p = join(dir, &name);
            if !exists(&p) {
                return p;
            }
        }
        panic!("names exhausted")
    }
}
fn split_ext(name: &str, dir: bool) -> (String, String) {
    match name.rfind('.') {
        Some(i) if i > 0 && !dir => (name[..i].into(), name[i..].into()),
        _ => (name.into(), String::new()),
    }
}
#[path = "../src/transfer.rs"]
mod transfer;
struct Fixture(String);
impl Fixture {
    fn new() -> Self {
        SHORT.store(usize::MAX, Ordering::SeqCst);
        FAIL_AFTER.store(usize::MAX, Ordering::SeqCst);
        WRITTEN.store(0, Ordering::SeqCst);
        MAX_BUFFER.store(0, Ordering::SeqCst);
        FAIL_SYNC.store(0, Ordering::SeqCst);
        SYNC_CALLS.store(0, Ordering::SeqCst);
        FAIL_SYNC_AT.store(usize::MAX, Ordering::SeqCst);
        FAIL_CLEANUP.store(0, Ordering::SeqCst);
        ZERO.store(0, Ordering::SeqCst);
        CROSS.store(0, Ordering::SeqCst);
        CANCEL_AT.store(usize::MAX, Ordering::SeqCst);
        *CANCEL_STATE.lock().unwrap() = None;
        let path = std::env::temp_dir().join(format!("waveos-transfer-{}-{}", std::process::id(), time::uptime_ms()));
        std::fs::create_dir(&path).unwrap();
        for n in ["src", "dst", "other"] {
            std::fs::create_dir(path.join(n)).unwrap();
        }
        Self(path.to_string_lossy().into())
    }
    fn p(&self, s: &str) -> String {
        fs::join(&self.0, s)
    }
    fn run(
        &self,
        source: &str,
        dest: &str,
        moving: bool,
        choice: Option<transfer::Choice>,
    ) -> Arc<sync::Mutex<transfer::State>> {
        let s = Arc::new(sync::Mutex::new(transfer::State {
            running: true,
            total: 1,
            remembered: choice,
            ..Default::default()
        }));
        *CANCEL_STATE.lock().unwrap() = Some(s.clone());
        transfer::run(&s, vec![(self.p(source), self.p(dest))], moving);
        s
    }
    fn clean(&self) {
        assert!(std::fs::read_dir(self.p("dst")).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".waveos-transfer")));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
#[test]
fn streaming_and_partial_writes() {
    let f = Fixture::new();
    let data = vec![73; 2_000_000];
    std::fs::write(f.p("src/a"), &data).unwrap();
    SHORT.store(997, Ordering::SeqCst);
    let s = f.run("src/a", "dst", false, None);
    assert_eq!(s.lock().completed, 1);
    assert_eq!(std::fs::read(f.p("dst/a")).unwrap(), data);
    assert_eq!(MAX_BUFFER.load(Ordering::SeqCst), 65536);
    f.clean();
}
#[test]
fn all_conflict_choices() {
    let f = Fixture::new();
    std::fs::write(f.p("src/a.txt"), b"new").unwrap();
    std::fs::write(f.p("dst/a.txt"), b"old").unwrap();
    assert_eq!(f.run("src/a.txt", "dst", false, Some(transfer::Choice::Skip)).lock().skipped, 1);
    assert_eq!(std::fs::read(f.p("dst/a.txt")).unwrap(), b"old");
    f.run("src/a.txt", "dst", false, Some(transfer::Choice::KeepBoth));
    assert_eq!(std::fs::read(f.p("dst/a copy.txt")).unwrap(), b"new");
    f.run("src/a.txt", "dst", false, Some(transfer::Choice::Replace));
    assert_eq!(std::fs::read(f.p("dst/a.txt")).unwrap(), b"new");
    f.clean();
}
#[test]
fn full_disk_and_zero_write_preserve_destination() {
    for zero in [false, true] {
        let f = Fixture::new();
        std::fs::write(f.p("src/a"), vec![2; 200000]).unwrap();
        std::fs::write(f.p("dst/a"), b"old").unwrap();
        if zero {
            ZERO.store(1, Ordering::SeqCst);
        } else {
            FAIL_AFTER.store(65536, Ordering::SeqCst);
        }
        let s = f.run("src/a", "dst", false, Some(transfer::Choice::Replace));
        assert_eq!(s.lock().completed, 0);
        assert_eq!(s.lock().errors.len(), 1);
        assert_eq!(std::fs::read(f.p("dst/a")).unwrap(), b"old");
        assert!(fs::exists(&f.p("src/a")));
        f.clean();
    }
}
#[test]
fn cancel_mid_copy_cleans_only_staging() {
    let f = Fixture::new();
    std::fs::write(f.p("src/a"), vec![2; 500000]).unwrap();
    std::fs::write(f.p("dst/a"), b"old").unwrap();
    CANCEL_AT.store(65536, Ordering::SeqCst);
    let s = f.run("src/a", "dst", false, Some(transfer::Choice::Replace));
    assert!(s.lock().cancel);
    assert_eq!(std::fs::read(f.p("dst/a")).unwrap(), b"old");
    assert_eq!(std::fs::metadata(f.p("src/a")).unwrap().len(), 500000);
    f.clean();
}
#[test]
fn sync_failure_prevents_publication_and_source_deletion() {
    let f = Fixture::new();
    std::fs::write(f.p("src/a"), b"new").unwrap();
    std::fs::write(f.p("other/a"), b"old").unwrap();
    CROSS.store(1, Ordering::SeqCst);
    FAIL_SYNC.store(1, Ordering::SeqCst);
    let s = f.run("src/a", "other", true, Some(transfer::Choice::Replace));
    assert_eq!(s.lock().errors.len(), 1);
    assert_eq!(std::fs::read(f.p("src/a")).unwrap(), b"new");
    assert_eq!(std::fs::read(f.p("other/a")).unwrap(), b"old");
}
#[test]
fn cross_volume_directory_move() {
    let f = Fixture::new();
    std::fs::create_dir(f.p("src/folder")).unwrap();
    std::fs::write(f.p("src/folder/a"), b"new").unwrap();
    CROSS.store(1, Ordering::SeqCst);
    let s = f.run("src/folder", "other", true, None);
    assert_eq!(s.lock().completed, 1, "{:?}", s.lock().errors);
    assert!(!fs::exists(&f.p("src/folder")));
    assert_eq!(std::fs::read(f.p("other/folder/a")).unwrap(), b"new");
}
#[test]
fn one_worker_and_conflict_cancellation() {
    let f = Fixture::new();
    std::fs::write(f.p("src/a"), b"a").unwrap();
    std::fs::write(f.p("dst/a"), b"b").unwrap();
    let shared = transfer::Manager::new();
    let jobs = vec![(f.p("src/a"), f.p("dst"))];
    assert!(shared.lock().start(jobs.clone(), false));
    while shared.lock().state.lock().conflict.is_none() {
        time::sleep_ms(1);
    }
    assert!(!shared.lock().start(jobs, false));
    shared.lock().state.lock().cancel = true;
    while !shared.lock().reap() {
        time::sleep_ms(1);
    }
    assert!(!shared.lock().busy());
    assert_eq!(std::fs::read(f.p("dst/a")).unwrap(), b"b");
}

#[test]
fn apply_to_remaining_conflicts_and_folder_replace_guard() {
    let f = Fixture::new();
    for name in ["a", "b"] {
        std::fs::write(f.p(&format!("src/{name}")), b"new").unwrap();
        std::fs::write(f.p(&format!("dst/{name}")), b"old").unwrap();
    }
    let state = Arc::new(sync::Mutex::new(transfer::State { running: true, total: 2, ..Default::default() }));
    let jobs = vec![(f.p("src/a"), f.p("dst")), (f.p("src/b"), f.p("dst"))];
    let s = state.clone();
    let worker = std::thread::spawn(move || transfer::run(&s, jobs, false));
    while state.lock().conflict.is_none() {
        time::sleep_ms(1);
    }
    {
        let mut s = state.lock();
        assert!(s.conflict.as_ref().unwrap().replace);
        assert_eq!(s.total, 2);
        s.apply_all = true;
        s.decision = Some(transfer::Choice::Replace);
    }
    worker.join().unwrap();
    assert_eq!(state.lock().completed, 2);
    for name in ["a", "b"] {
        assert_eq!(std::fs::read(f.p(&format!("dst/{name}"))).unwrap(), b"new");
    }
    std::fs::create_dir(f.p("src/folder")).unwrap();
    std::fs::create_dir(f.p("dst/folder")).unwrap();
    std::fs::write(f.p("src/folder/x"), b"copied").unwrap();
    let s = state.clone();
    let jobs = vec![(f.p("src/folder"), f.p("dst"))];
    let worker = std::thread::spawn(move || transfer::run(&s, jobs, false));
    while state.lock().conflict.is_none() {
        time::sleep_ms(1);
    }
    {
        let mut s = state.lock();
        let c = s.conflict.as_ref().unwrap();
        assert!(!c.replace);
        assert_eq!(c.name, "folder");
        s.decision = Some(transfer::Choice::KeepBoth);
    }
    worker.join().unwrap();
    assert_eq!(std::fs::read(f.p("dst/folder copy/x")).unwrap(), b"copied");
    assert!(std::fs::read_dir(f.p("dst/folder")).unwrap().next().is_none());
}

#[test]
fn cancellation_keeps_previously_completed_items() {
    let f = Fixture::new();
    std::fs::write(f.p("src/a"), b"complete").unwrap();
    std::fs::write(f.p("src/b"), vec![2; 500000]).unwrap();
    let state = Arc::new(sync::Mutex::new(transfer::State { running: true, total: 2, ..Default::default() }));
    *CANCEL_STATE.lock().unwrap() = Some(state.clone());
    CANCEL_AT.store(65536, Ordering::SeqCst);
    transfer::run(&state, vec![(f.p("src/a"), f.p("dst")), (f.p("src/b"), f.p("dst"))], false);
    assert_eq!(state.lock().completed, 1);
    assert_eq!(std::fs::read(f.p("dst/a")).unwrap(), b"complete");
    assert!(!fs::exists(&f.p("dst/b")));
    assert!(fs::exists(&f.p("src/b")));
    f.clean();
}

#[test]
fn root_and_descendant_destinations_are_rejected_before_io() {
    let f = Fixture::new();
    let s = Arc::new(sync::Mutex::new(transfer::State::default()));
    transfer::run(&s, vec![(String::from("/"), f.p("dst")), (f.p("src"), f.p("src/nested"))], false);
    assert_eq!(s.lock().errors.len(), 2);
    assert!(std::fs::read_dir(f.p("dst")).unwrap().next().is_none());
}

#[test]
fn post_publish_sync_failure_retains_cross_volume_source() {
    let f = Fixture::new();
    std::fs::write(f.p("src/a"), b"new complete contents").unwrap();
    std::fs::write(f.p("other/a"), b"old complete contents").unwrap();
    CROSS.store(1, Ordering::SeqCst);
    FAIL_SYNC_AT.store(2, Ordering::SeqCst);
    let s = f.run("src/a", "other", true, Some(transfer::Choice::Replace));
    assert_eq!(s.lock().completed, 0);
    assert!(s.lock().errors[0].contains("source retained"));
    assert_eq!(std::fs::read(f.p("src/a")).unwrap(), b"new complete contents");
    assert_eq!(std::fs::read(f.p("other/a")).unwrap(), b"new complete contents");
    assert!(s.lock().moved.is_empty());
}

#[test]
fn cancellation_reports_cleanup_failure_and_preserves_unowned_data() {
    let f = Fixture::new();
    let original = vec![5u8; 256 * 1024];
    std::fs::write(f.p("src/a"), &original).unwrap();
    std::fs::write(f.p("dst/a"), b"keep destination").unwrap();
    std::fs::create_dir(f.p("dst/.waveos-transfer-unrelated")).unwrap();
    std::fs::write(f.p("dst/.waveos-transfer-unrelated/keep"), b"keep unrelated").unwrap();
    CANCEL_AT.store(64 * 1024, Ordering::SeqCst);
    FAIL_CLEANUP.store(1, Ordering::SeqCst);
    let s = f.run("src/a", "dst", false, Some(transfer::Choice::Replace));
    assert!(s.lock().cancel);
    assert_eq!(s.lock().completed, 0);
    assert!(s.lock().errors.iter().any(|e| e.contains("Temporary item retained at")));
    assert_eq!(std::fs::read(f.p("src/a")).unwrap(), original);
    assert_eq!(std::fs::read(f.p("dst/a")).unwrap(), b"keep destination");
    assert_eq!(std::fs::read(f.p("dst/.waveos-transfer-unrelated/keep")).unwrap(), b"keep unrelated");
}
