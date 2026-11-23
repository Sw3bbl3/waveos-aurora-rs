//! Virtual filesystem: a mount table over pluggable filesystems.
//!
//! Mounts (longest prefix wins):
//!   `/`        user files — AuroraFS on disk (M3), or RamFS when no disk is found
//!   `/System`  the read-only system image (TarFS over the boot initrd)
//!   `/Boot`    the EFI System Partition (FAT32, M3)
//!
//! Filesystems implement [`Filesystem`] with interior locking (`sync::Mutex`,
//! which may be held across disk I/O). Errors are `aurora_abi::err` numbers.

pub mod aurorafs;
pub mod cache;
pub mod fat;
pub mod index;
pub mod mapcache;
pub mod perm;
pub mod ramfs;
pub mod tarfs;
pub mod trash;

use crate::sync::IrqMutex;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec::Vec;
pub use aurora_abi::err::*;

pub type Ino = u64;
pub type FsResult<T> = Result<T, isize>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
}

#[derive(Clone, Copy, Debug)]
pub struct Metadata {
    pub kind: Kind,
    pub size: u64,
    pub mtime: u64,
    pub ctime: u64,
}

/// Who a file belongs to: owner, group and Unix permission bits (`0o7777`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
    pub mode: u16,
}

impl Owner {
    /// The system's, with `mode`.
    pub const fn system(mode: u16) -> Owner {
        Owner { uid: 0, gid: 0, mode }
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: Ino,
    pub kind: Kind,
}

pub trait Filesystem: Send + Sync {
    /// Short description, e.g. "RamFS" or "AuroraFS on AHCI".
    fn describe(&self) -> String;
    fn root(&self) -> Ino;
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino>;
    fn metadata(&self, ino: Ino) -> FsResult<Metadata>;
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize>;
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize>;
    fn truncate(&self, ino: Ino, size: u64) -> FsResult<()>;
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>>;
    fn create(&self, dir: Ino, name: &str, kind: Kind) -> FsResult<Ino>;
    /// Removes a file or an empty directory.
    fn unlink(&self, dir: Ino, name: &str) -> FsResult<()>;
    fn rename(&self, from_dir: Ino, from: &str, to_dir: Ino, to: &str) -> FsResult<()>;
    fn sync(&self) -> FsResult<()> {
        Ok(())
    }
    /// Memory is short: give back what a cache holds (without waiting
    /// for a lock someone holds).
    fn shrink(&self) {}
    fn read_only(&self) -> bool {
        false
    }
    /// (total bytes, free bytes), if meaningful.
    fn space(&self) -> Option<(u64, u64)> {
        None
    }
    /// The owner, group and mode of `ino`, and whether the inode carries
    /// them itself (false: the volume's default). None: this filesystem
    /// has no owners (the VFS decides by where it is mounted).
    fn owner(&self, _ino: Ino) -> Option<(Owner, bool)> {
        None
    }
    fn set_owner(&self, _ino: Ino, _owner: Owner) -> FsResult<()> {
        Err(EPERM)
    }
    /// Creates an entry with an owner (None: the volume's default).
    fn create_owned(&self, dir: Ino, name: &str, kind: Kind, _owner: Option<Owner>) -> FsResult<Ino> {
        self.create(dir, name, kind)
    }
    /// Turns on owners for a volume from before them (the inodes without
    /// one belong to `uid`/`gid`). True if it did.
    fn enable_owners(&self, _uid: u32, _gid: u32) -> FsResult<bool> {
        Ok(false)
    }
    /// Makes file `ino` the swap file, of at least `size` bytes (blocks
    /// allocated, not written), and says where its blocks are. From then
    /// on, until the next boot, the filesystem refuses to read, write,
    /// truncate, rename or remove it: the swap file writes its blocks
    /// directly (`mm::swapfile`).
    fn swap_file(&self, _ino: Ino, _size: u64) -> FsResult<SwapExtents> {
        Err(EOPNOTSUPP)
    }
    /// Verifies on-disk consistency (filesystems that support it).
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))]
    fn check(&self) -> Result<(), String> {
        Ok(())
    }
}

/// Where a swap file's blocks are: the device, then runs of (first page
/// of the file, first sector, pages).
pub struct SwapExtents {
    pub dev: Arc<dyn crate::drivers::block::BlockDevice>,
    pub runs: Vec<(u64, u64, u64)>,
}

struct Mount {
    path: String,
    fs: Arc<dyn Filesystem>,
}

static MOUNTS: IrqMutex<Vec<Mount>> = IrqMutex::new(Vec::new());
/// `/` is a AuroraFS volume (files persist), not the in-memory fallback.
static PERSISTENT_ROOT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn root_persistent() -> bool {
    PERSISTENT_ROOT.load(core::sync::atomic::Ordering::Acquire)
}

/// Block devices a mounted filesystem lives on (so removable-media code
/// never mounts one twice).
static IN_USE: IrqMutex<Vec<String>> = IrqMutex::new(Vec::new());

pub fn note_device(name: &str) {
    IN_USE.lock().push(String::from(name));
}

pub fn device_in_use(name: &str) -> bool {
    IN_USE.lock().iter().any(|n| n == name)
}

/// A device and its partitions went away.
pub fn forget_device(name: &str) {
    let part_prefix = alloc::format!("{name}p");
    IN_USE.lock().retain(|n| n != name && !n.starts_with(part_prefix.as_str()));
}

/// Mounts an EFI System Partition found later (on a USB disk) at `/Boot`
/// when boot found none. Returns whether it did.
pub fn adopt_boot(dev: Arc<dyn crate::drivers::block::BlockDevice>) -> bool {
    if MOUNTS.lock().iter().any(|m| m.path == "/Boot") {
        return false;
    }
    let name = dev.name();
    match fat::FatFs::mount(dev) {
        Ok(fs) => {
            mount("/Boot", Arc::new(fs));
            note_device(&name);
            true
        }
        Err(_) => false,
    }
}

/// Makes a AuroraFS volume found later (on a USB disk) the home volume, when
/// boot found none. Returns whether it did.
pub fn adopt_home(dev: Arc<dyn crate::drivers::block::BlockDevice>) -> bool {
    if root_persistent() {
        return false;
    }
    match aurorafs::AuroraFs::mount(dev.clone()) {
        Ok(fs) => {
            mount("/", Arc::new(fs));
            note_device(&dev.name());
            PERSISTENT_ROOT.store(true, core::sync::atomic::Ordering::Release);
            log!("vfs", "home volume on {} ({})", dev.name(), dev.describe());
            true
        }
        Err(e) => {
            log!("vfs", "{}: AuroraFS mount failed: {}", dev.name(), aurora_abi::err::name(e));
            false
        }
    }
}

pub fn mount(path: &str, fs: Arc<dyn Filesystem>) {
    log!("vfs", "mounted {} at {}", fs.describe(), path);
    crate::telemetry::mount(path, &fs.describe());
    let mut m = MOUNTS.lock();
    m.retain(|x| x.path != path);
    m.push(Mount { path: String::from(path), fs });
}

/// (total, free) bytes of the filesystem holding `path`, if it reports them.
pub fn space_of(path: &str) -> Option<(u64, u64)> {
    crate::sandbox::check_path(&resolve("/", path), crate::sandbox::Access::Stat).ok()?;
    let (fs, _) = mount_for(&resolve("/", path)).ok()?;
    fs.space()
}

/// Detaches the filesystem at `path` after writing out what it has cached.
pub fn unmount(path: &str) -> FsResult<()> {
    let fs = MOUNTS.lock().iter().find(|m| m.path == path).map(|m| m.fs.clone()).ok_or(ENOENT)?;
    let synced = fs.sync();
    MOUNTS.lock().retain(|m| m.path != path);
    log!("vfs", "unmounted {}", path);
    synced
}

/// A mounted filesystem: (mount point, description, (total, free) bytes).
pub type MountInfo = (String, String, Option<(u64, u64)>);

/// Every mounted filesystem.
pub fn mounts() -> Vec<MountInfo> {
    let list: Vec<(String, Arc<dyn Filesystem>)> =
        MOUNTS.lock().iter().map(|m| (m.path.clone(), m.fs.clone())).collect();
    list.into_iter().map(|(p, fs)| (p, fs.describe(), fs.space())).collect()
}

/// Normalises `path` relative to `cwd` (handles `.`, `..`, repeated slashes).
pub fn resolve(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> =
        if path.starts_with('/') { Vec::new() } else { cwd.split('/').filter(|s| !s.is_empty()).collect() };
    for p in path.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path.split('/').filter(|s| !s.is_empty())
}

/// Picks the mount for `path` (already normalised) and returns the path remainder.
fn mount_for(path: &str) -> FsResult<(Arc<dyn Filesystem>, String)> {
    mount_full(path).map(|(fs, rest, _)| (fs, rest))
}

/// [`mount_for`], with the mount point.
fn mount_full(path: &str) -> FsResult<(Arc<dyn Filesystem>, String, String)> {
    let mounts = MOUNTS.lock();
    let mut best: Option<&Mount> = None;
    for m in mounts.iter() {
        let hit = m.path == "/"
            || path == m.path
            || (path.starts_with(m.path.as_str()) && path.as_bytes().get(m.path.len()) == Some(&b'/'));
        if hit && best.is_none_or(|b| m.path.len() > b.path.len()) {
            best = Some(m);
        }
    }
    let m = best.ok_or(ENOENT)?;
    let rest = if m.path == "/" { path.to_string() } else { path[m.path.len()..].to_string() };
    Ok((m.fs.clone(), rest, m.path.clone()))
}

/// Walks `rest` on `fs`, checking search permission on each folder for `cred`.
fn walk(fs: &Arc<dyn Filesystem>, rest: &str, mount: &str, cred: Option<&crate::users::Cred>) -> FsResult<Ino> {
    let mut ino = fs.root();
    for c in components(rest) {
        if c.len() > aurora_abi::NAME_MAX {
            return Err(ENAMETOOLONG);
        }
        perm::search(cred, fs, mount, ino)?;
        ino = fs.lookup(ino, c)?;
    }
    Ok(ino)
}

/// Resolves an absolute path to (filesystem, inode).
pub fn lookup(path: &str) -> FsResult<(Arc<dyn Filesystem>, Ino)> {
    lookup_full(&resolve("/", path)).map(|(fs, ino, _)| (fs, ino))
}

/// [`lookup`] of a normalised path, with the mount point. A sandboxed
/// caller must be allowed to see it (see `sandbox`).
fn lookup_full(path: &str) -> FsResult<(Arc<dyn Filesystem>, Ino, String)> {
    crate::sandbox::check_path(path, crate::sandbox::Access::Stat)?;
    let (fs, rest, mount) = mount_full(path)?;
    let ino = walk(&fs, &rest, &mount, perm::caller().as_ref())?;
    Ok((fs, ino, mount))
}

/// Resolves the parent directory of `path` and returns (fs, dir inode,
/// final name, mount point).
fn parent_full(path: &str) -> FsResult<(Arc<dyn Filesystem>, Ino, String, String)> {
    let path = resolve("/", path);
    let (dir, name) = match path.rfind('/') {
        Some(0) => ("/", &path[1..]),
        Some(i) => (&path[..i], &path[i + 1..]),
        None => return Err(EINVAL),
    };
    if name.is_empty() {
        return Err(EINVAL);
    }
    if name.len() > aurora_abi::NAME_MAX {
        return Err(ENAMETOOLONG);
    }
    // A mount point itself cannot be created/removed/renamed.
    if MOUNTS.lock().iter().any(|m| m.path == path) {
        return Err(EPERM);
    }
    // Making, removing or renaming an entry changes it.
    crate::sandbox::check_path(&path, crate::sandbox::Access::Write)?;
    let (fs, dino, mount) = lookup_full(&resolve("/", dir))?;
    if fs.metadata(dino)?.kind != Kind::Dir {
        return Err(ENOTDIR);
    }
    Ok((fs, dino, String::from(name), mount))
}

pub fn stat(path: &str) -> FsResult<(Metadata, bool)> {
    let (fs, ino) = lookup(path)?;
    Ok((fs.metadata(ino)?, fs.read_only()))
}

pub struct Entry {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub mtime: u64,
}

/// Lists a directory, including mount points that live directly inside it.
pub fn readdir(path: &str) -> FsResult<Vec<Entry>> {
    let path = resolve("/", path);
    crate::sandbox::check_path(&path, crate::sandbox::Access::Read)?;
    let (fs, ino, mount) = lookup_full(&path)?;
    if fs.metadata(ino)?.kind != Kind::Dir {
        return Err(ENOTDIR);
    }
    perm::check_list(&fs, &mount, ino)?;
    let mut out = Vec::new();
    for e in fs.readdir(ino)? {
        let md = fs.metadata(e.ino)?;
        out.push(Entry { name: e.name, kind: e.kind, size: md.size, mtime: md.mtime });
    }
    let mount_points: Vec<String> = MOUNTS
        .lock()
        .iter()
        .filter(|m| m.path != "/" && parent_path(&m.path) == path)
        .map(|m| String::from(&m.path[m.path.rfind('/').unwrap() + 1..]))
        .collect();
    for name in mount_points {
        if !out.iter().any(|e| e.name == name) {
            out.push(Entry { name, kind: Kind::Dir, size: 0, mtime: 0 });
        }
    }
    out.sort_by_cached_key(|e| (e.kind != Kind::Dir, e.name.to_lowercase()));
    Ok(out)
}

pub fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    }
}

pub fn mkdir(path: &str) -> FsResult<()> {
    // `/` and mount points have no parent to look in, but exist all the
    // same (`mkdir -p` starts at `/` and expects EEXIST there).
    if stat(path).is_ok() {
        return Err(EEXIST);
    }
    let (fs, dir, name, mount) = parent_full(path)?;
    if fs.read_only() {
        return Err(EROFS);
    }
    if fs.lookup(dir, &name).is_ok() {
        return Err(EEXIST);
    }
    perm::check_create(&fs, &mount, dir, &name)?;
    let owner = perm::new_owner(perm::caller().as_ref(), &fs, &mount, dir, Kind::Dir, None);
    fs.create_owned(dir, &name, Kind::Dir, owner)?;
    index::added(&resolve("/", path), true);
    Ok(())
}

pub fn unlink(path: &str) -> FsResult<()> {
    let (fs, dir, name, mount) = parent_full(path)?;
    if fs.read_only() {
        return Err(EROFS);
    }
    perm::check_remove(&fs, &mount, dir, &name)?;
    if let Ok(ino) = fs.lookup(dir, &name) {
        mapcache::forget(&fs, ino);
    }
    fs.unlink(dir, &name)?;
    index::removed(&resolve("/", path));
    Ok(())
}

/// Deletes a file, or a directory and everything in it.
pub fn remove_tree(path: &str) -> FsResult<()> {
    if is_dir(path) {
        for e in readdir(path)? {
            remove_tree(&join(path, &e.name))?;
        }
    }
    unlink(path)
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        alloc::format!("{dir}{name}")
    } else {
        alloc::format!("{dir}/{name}")
    }
}

pub fn rename(from: &str, to: &str) -> FsResult<()> {
    let (fs_a, dir_a, name_a, mount) = parent_full(from)?;
    let (fs_b, dir_b, name_b, _) = parent_full(to)?;
    if !Arc::ptr_eq(&fs_a, &fs_b) {
        return Err(EINVAL); // cross-volume moves are copy + delete in user space
    }
    if fs_a.read_only() {
        return Err(EROFS);
    }
    perm::check_remove(&fs_a, &mount, dir_a, &name_a)?;
    perm::check_create(&fs_b, &mount, dir_b, &name_b)?;
    if fs_b.lookup(dir_b, &name_b).is_ok() {
        perm::check_remove(&fs_b, &mount, dir_b, &name_b)?;
    }
    // Refuse to move a directory into itself.
    let from_n = resolve("/", from);
    let to_n = resolve("/", to);
    if to_n.starts_with(&from_n) && to_n.as_bytes().get(from_n.len()) == Some(&b'/') {
        return Err(EINVAL);
    }
    // A file this replaces is gone.
    if let Ok(ino) = fs_b.lookup(dir_b, &name_b) {
        mapcache::forget(&fs_b, ino);
    }
    fs_a.rename(dir_a, &name_a, dir_b, &name_b)?;
    index::renamed(&from_n, &to_n);
    Ok(())
}

pub fn sync_all() {
    let _ = try_sync_all();
}

/// Memory is short: every mounted filesystem shrinks its cache.
pub fn shrink_caches() {
    let all: Vec<Arc<dyn Filesystem>> = MOUNTS.lock().iter().map(|m| m.fs.clone()).collect();
    for fs in all {
        fs.shrink();
    }
}

pub fn try_sync_all() -> FsResult<()> {
    let all: Vec<Arc<dyn Filesystem>> = MOUNTS.lock().iter().map(|m| m.fs.clone()).collect();
    let mut error = None;
    for fs in all {
        if let Err(e) = fs.sync() {
            log!("vfs", "sync of {} failed: {}", fs.describe(), aurora_abi::err::name(e));
            error = Some(e);
        }
    }
    error.map_or(Ok(()), Err)
}

/// An open file (or directory) handle.
pub struct OpenFile {
    pub fs: Arc<dyn Filesystem>,
    pub ino: Ino,
    pub pos: u64,
    pub readable: bool,
    pub writable: bool,
    pub append: bool,
}

pub fn open(path: &str, flags: usize) -> FsResult<OpenFile> {
    open_mode(path, flags, None)
}

/// [`open`], giving a file it creates permission bits `mode` (before the
/// creator's umask; None: `rw-r--r--`).
pub fn open_mode(path: &str, flags: usize, mode: Option<u16>) -> FsResult<OpenFile> {
    use aurora_abi::open::*;
    let writable = flags & (WRITE | APPEND | TRUNCATE) != 0;
    let mut created = false;
    let access = if writable { crate::sandbox::Access::Write } else { crate::sandbox::Access::Read };
    crate::sandbox::check_path(&resolve("/", path), access)?;
    let (fs, ino, mount) = match lookup_full(&resolve("/", path)) {
        Ok(found) => {
            if flags & CREATE != 0 && flags & EXCLUSIVE != 0 {
                return Err(EEXIST);
            }
            found
        }
        Err(ENOENT) if flags & CREATE != 0 => {
            let (fs, dir, name, mount) = parent_full(path)?;
            if fs.read_only() {
                return Err(EROFS);
            }
            perm::check_create(&fs, &mount, dir, &name)?;
            let owner = perm::new_owner(perm::caller().as_ref(), &fs, &mount, dir, Kind::File, mode);
            let ino = fs.create_owned(dir, &name, Kind::File, owner)?;
            index::added(&resolve("/", path), false);
            created = true;
            (fs, ino, mount)
        }
        Err(e) => return Err(e),
    };
    let md = fs.metadata(ino)?;
    if writable && fs.read_only() {
        return Err(EROFS);
    }
    if writable && md.kind == Kind::Dir {
        return Err(EISDIR);
    }
    // The creator may write what it just made, whatever its mode says.
    if !created {
        perm::check_open(&fs, &mount, ino, flags & READ != 0 || !writable, writable)?;
    }
    if flags & TRUNCATE != 0 {
        mapcache::forget(&fs, ino);
        fs.truncate(ino, 0)?;
    }
    Ok(OpenFile { fs, ino, pos: 0, readable: flags & READ != 0 || !writable, writable, append: flags & APPEND != 0 })
}

impl OpenFile {
    pub fn read(&mut self, buf: &mut [u8]) -> FsResult<usize> {
        if !self.readable {
            return Err(EBADF);
        }
        if self.fs.metadata(self.ino)?.kind == Kind::Dir {
            return Err(EISDIR);
        }
        let n = self.fs.read(self.ino, self.pos, buf)?;
        self.pos += n as u64;
        Ok(n)
    }

    pub fn write(&mut self, data: &[u8]) -> FsResult<usize> {
        if !self.writable {
            return Err(EBADF);
        }
        if self.append {
            self.pos = self.fs.metadata(self.ino)?.size;
        }
        mapcache::forget(&self.fs, self.ino);
        let n = self.fs.write(self.ino, self.pos, data)?;
        self.pos += n as u64;
        Ok(n)
    }

    pub fn seek(&mut self, offset: i64, whence: usize) -> FsResult<u64> {
        let base = match whence {
            aurora_abi::seek::SET => 0,
            aurora_abi::seek::CUR => self.pos as i64,
            aurora_abi::seek::END => self.fs.metadata(self.ino)?.size as i64,
            _ => return Err(EINVAL),
        };
        let new = base.checked_add(offset).ok_or(EINVAL)?;
        if new < 0 {
            return Err(EINVAL);
        }
        self.pos = new as u64;
        Ok(self.pos)
    }

    pub fn size(&self) -> FsResult<u64> {
        Ok(self.fs.metadata(self.ino)?.size)
    }

    pub fn truncate(&mut self, size: u64) -> FsResult<()> {
        if !self.writable {
            return Err(EBADF);
        }
        mapcache::forget(&self.fs, self.ino);
        self.fs.truncate(self.ino, size)
    }
}

// ----------------------------------------------------------------- helpers

pub fn read_all(path: &str) -> FsResult<Vec<u8>> {
    let mut f = open(path, aurora_abi::open::READ)?;
    let size = f.fs.metadata(f.ino)?.size as usize;
    let mut buf = alloc::vec![0u8; size];
    let mut done = 0;
    while done < size {
        let n = f.read(&mut buf[done..])?;
        if n == 0 {
            break;
        }
        done += n;
    }
    buf.truncate(done);
    Ok(buf)
}

pub fn write_all(path: &str, data: &[u8]) -> FsResult<()> {
    use aurora_abi::open::*;
    let mut f = open(path, WRITE | CREATE | TRUNCATE)?;
    let mut done = 0;
    while done < data.len() {
        let n = f.write(&data[done..])?;
        if n == 0 {
            return Err(EIO);
        }
        done += n;
    }
    Ok(())
}

/// Safe same-volume replacement. Publication can succeed while final sync fails.
pub fn write_atomic(path: &str, data: &[u8]) -> FsResult<aurora_desktop::save::Published> {
    use core::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    struct Adapter;
    impl aurora_desktop::save::Storage for Adapter {
        fn create_new(&mut self, p: &str, data: &[u8]) -> FsResult<()> {
            use aurora_abi::open::*;
            let mut f = open(p, WRITE | CREATE | EXCLUSIVE)?;
            let result = (|| {
                let mut at = 0;
                while at < data.len() {
                    let n = f.write(&data[at..])?;
                    if n == 0 {
                        return Err(EIO);
                    }
                    at += n;
                }
                Ok(())
            })();
            drop(f);
            if result.is_err() {
                let _ = unlink(p);
            }
            result
        }
        fn sync(&mut self, p: &str) -> FsResult<()> {
            lookup(p)?.0.sync()
        }
        fn rename(&mut self, a: &str, b: &str) -> FsResult<()> {
            rename(a, b)
        }
        fn remove(&mut self, p: &str) -> FsResult<()> {
            unlink(p)
        }
    }
    aurora_desktop::save::write(&mut Adapter, path, data, NEXT.fetch_add(1, Ordering::Relaxed))
}

pub fn is_dir(path: &str) -> bool {
    stat(path).is_ok_and(|(m, _)| m.kind == Kind::Dir)
}

/// A compact description of the controller behind a (partition) device,
/// e.g. "AHCI SATA" or "NVMe".
pub fn short_device(dev: &dyn crate::drivers::block::BlockDevice) -> String {
    let d = dev.describe();
    let d = d.rsplit(" on ").next().unwrap_or(&d);
    String::from(d.split(" — ").next().unwrap_or(d))
}

/// Mounts the boot-time filesystems. Called once during kernel init.
pub fn init(initrd: &'static [u8]) {
    mount("/", Arc::new(ramfs::RamFs::new()));
    match tarfs::TarFs::parse(initrd) {
        Some(t) => mount("/System", Arc::new(t)),
        None => log!("vfs", "no system image (initrd) — /System is unavailable"),
    }
}

/// Mounts volumes found on disks: the first AuroraFS partition becomes `/`
/// (otherwise the RamFS root stays, seeded with default folders). Starts the
/// background flusher.
pub fn mount_disks() {
    let mut root_found = false;
    let mut boot_found = false;
    for dev in crate::drivers::block::devices() {
        let Some(part) = crate::drivers::block::partition_info(&dev) else { continue };
        if part.type_guid == crate::drivers::block::gpt::ESP_TYPE && !boot_found {
            match fat::FatFs::mount(dev.clone()) {
                Ok(fs) => {
                    mount("/Boot", Arc::new(fs));
                    note_device(&dev.name());
                    boot_found = true;
                }
                Err(e) => log!("vfs", "{}: FAT32 mount failed: {}", dev.name(), aurora_abi::err::name(e)),
            }
            continue;
        }
        if part.type_guid == ::aurorafs::PARTITION_TYPE && !root_found {
            match aurorafs::AuroraFs::mount(dev.clone()) {
                Ok(fs) => {
                    mount("/", Arc::new(fs));
                    note_device(&dev.name());
                    root_found = true;
                    PERSISTENT_ROOT.store(true, core::sync::atomic::Ordering::Release);
                }
                Err(e) => log!("vfs", "{}: AuroraFS mount failed: {}", dev.name(), aurora_abi::err::name(e)),
            }
        }
    }
    if !root_found {
        log!("vfs", "no AuroraFS volume found — files are kept in memory only");
    }
    ramfs::seed();
    crate::sched::spawn("flusher", flusher);
    crate::sched::spawn("indexer", index::build);
}

/// Commits filesystem changes to disk every few seconds.
fn flusher() {
    loop {
        crate::sched::sleep_ms(5000);
        sync_all();
    }
}
