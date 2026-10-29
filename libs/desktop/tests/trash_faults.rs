//! The actual kernel Trash implementation against a fault-injecting host filesystem.
extern crate alloc;
pub use aurora_abi::err::*;
use std::{
    io::{Read, Write},
    path::PathBuf,
    sync::{Mutex, OnceLock},
};
pub type FsResult<T> = Result<T, isize>;
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
}
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    pub mtime: u64,
}
pub struct Entry {
    pub name: String,
    pub kind: Kind,
}
struct State {
    root: PathBuf,
    cross: bool,
    syncs: usize,
    fail_sync: usize,
    fail_write: bool,
    fail_delete: bool,
}
static STATE: OnceLock<Mutex<State>> = OnceLock::new();
fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.get().unwrap().lock().unwrap()
}
fn host(p: &str) -> PathBuf {
    state().root.join(p.trim_start_matches('/'))
}
fn err(e: std::io::Error) -> isize {
    match e.kind() {
        std::io::ErrorKind::NotFound => ENOENT,
        std::io::ErrorKind::AlreadyExists => EEXIST,
        std::io::ErrorKind::PermissionDenied => EACCES,
        _ => EIO,
    }
}
pub fn stat(p: &str) -> FsResult<(Meta, ())> {
    let m = std::fs::metadata(host(p)).map_err(err)?;
    Ok((Meta { kind: if m.is_dir() { Kind::Dir } else { Kind::File }, size: m.len(), mtime: 0 }, ()))
}
pub fn is_dir(p: &str) -> bool {
    stat(p).is_ok_and(|(m, _)| m.kind == Kind::Dir)
}
pub fn join(p: &str, n: &str) -> String {
    format!("{}/{n}", p.trim_end_matches('/'))
}
pub fn parent_path(p: &str) -> String {
    p.rsplit_once('/').map(|(p, _)| if p.is_empty() { "/" } else { p }).unwrap_or("/").into()
}
pub fn resolve(_: &str, p: &str) -> String {
    p.into()
}
pub fn read_all(p: &str) -> FsResult<Vec<u8>> {
    std::fs::read(host(p)).map_err(err)
}
pub fn mkdir(p: &str) -> FsResult<()> {
    std::fs::create_dir(host(p)).map_err(err)
}
pub fn readdir(p: &str) -> FsResult<Vec<Entry>> {
    std::fs::read_dir(host(p))
        .map_err(err)?
        .map(|e| {
            let e = e.map_err(err)?;
            Ok(Entry {
                name: e.file_name().to_string_lossy().into(),
                kind: if e.file_type().map_err(err)?.is_dir() { Kind::Dir } else { Kind::File },
            })
        })
        .collect()
}
pub fn rename(a: &str, b: &str) -> FsResult<()> {
    if state().cross && a.starts_with("/Volumes/") != b.starts_with("/Volumes/") {
        return Err(EINVAL);
    }
    std::fs::rename(host(a), host(b)).map_err(err)
}
pub fn unlink(p: &str) -> FsResult<()> {
    if state().fail_delete && p.starts_with("/Volumes/") {
        return Err(EACCES);
    }
    if is_dir(p) {
        std::fs::remove_dir(host(p)).map_err(err)
    } else {
        std::fs::remove_file(host(p)).map_err(err)
    }
}
pub fn remove_tree(p: &str) -> FsResult<()> {
    if is_dir(p) {
        for e in readdir(p)? {
            remove_tree(&join(p, &e.name))?;
        }
    }
    unlink(p)
}
pub struct Volume;
impl Volume {
    pub fn sync(&self) -> FsResult<()> {
        let mut s = state();
        s.syncs += 1;
        if s.syncs == s.fail_sync {
            Err(EIO)
        } else {
            Ok(())
        }
    }
}
pub fn lookup(_: &str) -> FsResult<(Volume, ())> {
    Ok((Volume, ()))
}
pub struct File(std::fs::File);
impl File {
    pub fn read(&mut self, b: &mut [u8]) -> FsResult<usize> {
        self.0.read(b).map_err(err)
    }
    pub fn write(&mut self, b: &[u8]) -> FsResult<usize> {
        if state().fail_write {
            return Err(ENOSPC);
        }
        self.0.write(b).map_err(err)
    }
}
pub fn open(p: &str, flags: usize) -> FsResult<File> {
    use aurora_abi::open::*;
    let mut o = std::fs::OpenOptions::new();
    o.read(flags & READ != 0).write(flags & WRITE != 0).create_new(flags & EXCLUSIVE != 0);
    o.open(host(p)).map(File).map_err(err)
}
pub fn write_atomic(p: &str, b: &[u8]) -> FsResult<aurora_desktop::save::Published> {
    struct Disk;
    impl aurora_desktop::save::Storage for Disk {
        fn create_new(&mut self, p: &str, b: &[u8]) -> FsResult<()> {
            let mut f = open(p, aurora_abi::open::WRITE | aurora_abi::open::CREATE | aurora_abi::open::EXCLUSIVE)?;
            let r = f.write(b).map(|_| ());
            if r.is_err() {
                let _ = unlink(p);
            }
            r
        }
        fn sync(&mut self, _: &str) -> FsResult<()> {
            Volume.sync()
        }
        fn rename(&mut self, a: &str, b: &str) -> FsResult<()> {
            rename(a, b)
        }
        fn remove(&mut self, p: &str) -> FsResult<()> {
            unlink(p)
        }
    }
    aurora_desktop::save::write(&mut Disk, p, b, 1)
}
pub mod sync {
    pub struct Mutex<T>(std::sync::Mutex<T>);
    impl<T> Mutex<T> {
        pub const fn new(v: T) -> Self {
            Self(std::sync::Mutex::new(v))
        }
        pub fn lock(&self) -> std::sync::MutexGuard<'_, T> {
            self.0.lock().unwrap()
        }
    }
}
#[path = "../../../kernel/src/fs/trash.rs"]
mod trash;
fn reset() {
    let root = std::env::temp_dir().join(format!("wave-trash-faults-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Volumes/USB")).unwrap();
    std::fs::create_dir_all(root.join("Trash")).unwrap();
    std::fs::create_dir_all(root.join("Documents")).unwrap();
    *STATE
        .get_or_init(|| {
            Mutex::new(State {
                root: root.clone(),
                cross: true,
                syncs: 0,
                fail_sync: 0,
                fail_write: false,
                fail_delete: false,
            })
        })
        .lock()
        .unwrap() =
        State { root: root.clone(), cross: true, syncs: 0, fail_sync: 0, fail_write: false, fail_delete: false };
}
fn put(p: &str) {
    std::fs::write(host(p), b"irreplaceable work").unwrap();
}
#[test]
fn production_trash_fault_matrix() {
    reset();
    put("/Volumes/USB/note");
    let name = trash::move_to_trash("/Volumes/USB/note").unwrap();
    assert!(!host("/Volumes/USB/note").exists());
    assert_eq!(trash::put_back(&name).unwrap(), "/Volumes/USB/note");
    assert_eq!(read_all("/Volumes/USB/note").unwrap(), b"irreplaceable work");
    // Every index/copy/publication flush failure retains the original or a recoverable copy plus metadata.
    for fail in 1..=5 {
        reset();
        put("/Volumes/USB/note");
        state().fail_sync = fail;
        assert!(trash::move_to_trash("/Volumes/USB/note").is_err());
        assert!(host("/Volumes/USB/note").exists() || host("/Trash/note").exists());
        if host("/Trash/note").exists() {
            assert!(read_all("/Trash/.trashinfo")
                .unwrap()
                .windows(b"/Volumes/USB/note\n".len())
                .any(|w| w == b"/Volumes/USB/note\n"));
        }
    }
    for fail in 1..=5 {
        reset();
        put("/Volumes/USB/note");
        let name = trash::move_to_trash("/Volumes/USB/note").unwrap();
        {
            let mut s = state();
            s.syncs = 0;
            s.fail_sync = fail;
        }
        assert!(trash::put_back(&name).is_err());
        assert!(host("/Volumes/USB/note").exists() || host("/Trash/note").exists());
        let recover = if host("/Volumes/USB/note").exists() { "/Volumes/USB/note" } else { "/Trash/note" };
        assert_eq!(read_all(recover).unwrap(), b"irreplaceable work");
    }
    reset();
    put("/Volumes/USB/note");
    state().fail_write = true;
    assert!(trash::move_to_trash("/Volumes/USB/note").is_err());
    assert!(host("/Volumes/USB/note").exists());
    reset();
    put("/Volumes/USB/note");
    state().fail_delete = true;
    assert!(trash::move_to_trash("/Volumes/USB/note").is_err());
    assert!(host("/Volumes/USB/note").exists());
    assert!(host("/Trash/note").exists());
    reset();
    put("/Documents/.hidden");
    let n = trash::move_to_trash("/Documents/.hidden").unwrap();
    assert_eq!(n, "Recovered .hidden");
    assert_eq!(trash::count(), 1);
    put("/Documents/.hidden");
    let restored = trash::put_back(&n).unwrap();
    assert_ne!(restored, "/Documents/.hidden");
    assert!(host(&restored).exists());
    reset();
    put("/Documents/a");
    trash::move_to_trash("/Documents/a").unwrap();
    trash::empty().unwrap();
    assert_eq!(trash::count(), 0);
    let root = state().root.clone();
    std::fs::remove_dir_all(root).unwrap();
}
