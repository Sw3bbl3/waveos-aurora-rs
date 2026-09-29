//! Virtual filesystem: a mount table over pluggable filesystems.
//!
//! Mounts (longest prefix wins):
//!   `/`        user files — WaveFS on disk (M3), or RamFS when no disk is found
//!   `/System`  the read-only system image (TarFS over the boot initrd)
//!   `/Boot`    the EFI System Partition (FAT32, M3)
//!
//! Filesystems implement [`Filesystem`] with interior locking (`sync::Mutex`,
//! which may be held across disk I/O). Errors are `aurora_abi::err` numbers.

pub mod ramfs;
pub mod tarfs;

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

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub ino: Ino,
    pub kind: Kind,
}

pub trait Filesystem: Send + Sync {
    /// Short description, e.g. "RamFS" or "WaveFS on AHCI".
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
    fn read_only(&self) -> bool {
        false
    }
    /// (total bytes, free bytes), if meaningful.
    fn space(&self) -> Option<(u64, u64)> {
        None
    }
}

struct Mount {
    path: String,
    fs: Arc<dyn Filesystem>,
}

static MOUNTS: IrqMutex<Vec<Mount>> = IrqMutex::new(Vec::new());

pub fn mount(path: &str, fs: Arc<dyn Filesystem>) {
    log!("vfs", "mounted {} at {}", fs.describe(), path);
    let mut m = MOUNTS.lock();
    m.retain(|x| x.path != path);
    m.push(Mount { path: String::from(path), fs });
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
    Ok((m.fs.clone(), rest))
}

fn walk(fs: &Arc<dyn Filesystem>, rest: &str) -> FsResult<Ino> {
    let mut ino = fs.root();
    for c in components(rest) {
        if c.len() > aurora_abi::NAME_MAX {
            return Err(ENAMETOOLONG);
        }
        ino = fs.lookup(ino, c)?;
    }
    Ok(ino)
}

/// Resolves an absolute path to (filesystem, inode).
pub fn lookup(path: &str) -> FsResult<(Arc<dyn Filesystem>, Ino)> {
    let path = resolve("/", path);
    let (fs, rest) = mount_for(&path)?;
    let ino = walk(&fs, &rest)?;
    Ok((fs, ino))
}

/// Resolves the parent directory of `path` and returns (fs, dir inode, final name).
fn parent_of(path: &str) -> FsResult<(Arc<dyn Filesystem>, Ino, String)> {
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
    let (fs, dino) = lookup(dir)?;
    if fs.metadata(dino)?.kind != Kind::Dir {
        return Err(ENOTDIR);
    }
    Ok((fs, dino, String::from(name)))
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
    let (fs, ino) = lookup(&path)?;
    if fs.metadata(ino)?.kind != Kind::Dir {
        return Err(ENOTDIR);
    }
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
    out.sort_by(|a, b| (a.kind != Kind::Dir, a.name.to_lowercase()).cmp(&(b.kind != Kind::Dir, b.name.to_lowercase())));
    Ok(out)
}

pub fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    }
}

pub fn mkdir(path: &str) -> FsResult<()> {
    let (fs, dir, name) = parent_of(path)?;
    if fs.read_only() {
        return Err(EROFS);
    }
    if fs.lookup(dir, &name).is_ok() {
        return Err(EEXIST);
    }
    fs.create(dir, &name, Kind::Dir).map(|_| ())
}

pub fn unlink(path: &str) -> FsResult<()> {
    let (fs, dir, name) = parent_of(path)?;
    if fs.read_only() {
        return Err(EROFS);
    }
    fs.unlink(dir, &name)
}

pub fn rename(from: &str, to: &str) -> FsResult<()> {
    let (fs_a, dir_a, name_a) = parent_of(from)?;
    let (fs_b, dir_b, name_b) = parent_of(to)?;
    if !Arc::ptr_eq(&fs_a, &fs_b) {
        return Err(EINVAL); // cross-volume moves are copy + delete in user space
    }
    if fs_a.read_only() {
        return Err(EROFS);
    }
    // Refuse to move a directory into itself.
    let from_n = resolve("/", from);
    let to_n = resolve("/", to);
    if to_n.starts_with(&from_n) && to_n.as_bytes().get(from_n.len()) == Some(&b'/') {
        return Err(EINVAL);
    }
    fs_a.rename(dir_a, &name_a, dir_b, &name_b)
}

pub fn sync_all() {
    let all: Vec<Arc<dyn Filesystem>> = MOUNTS.lock().iter().map(|m| m.fs.clone()).collect();
    for fs in all {
        if let Err(e) = fs.sync() {
            log!("vfs", "sync of {} failed: {}", fs.describe(), aurora_abi::err::name(e));
        }
    }
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
    use aurora_abi::open::*;
    let writable = flags & (WRITE | APPEND | TRUNCATE) != 0;
    let (fs, ino) = match lookup(path) {
        Ok((fs, ino)) => {
            if flags & CREATE != 0 && flags & EXCLUSIVE != 0 {
                return Err(EEXIST);
            }
            (fs, ino)
        }
        Err(ENOENT) if flags & CREATE != 0 => {
            let (fs, dir, name) = parent_of(path)?;
            if fs.read_only() {
                return Err(EROFS);
            }
            let ino = fs.create(dir, &name, Kind::File)?;
            (fs, ino)
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
    if flags & TRUNCATE != 0 {
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

    pub fn truncate(&mut self, size: u64) -> FsResult<()> {
        if !self.writable {
            return Err(EBADF);
        }
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
        done += f.write(&data[done..])?;
    }
    Ok(())
}

pub fn is_dir(path: &str) -> bool {
    stat(path).is_ok_and(|(m, _)| m.kind == Kind::Dir)
}

/// Mounts the boot-time filesystems. Called once during kernel init.
pub fn init(initrd: &'static [u8]) {
    let root = Arc::new(ramfs::RamFs::new());
    mount("/", root);
    ramfs::seed();
    match tarfs::TarFs::parse(initrd) {
        Some(t) => mount("/System", Arc::new(t)),
        None => log!("vfs", "no system image (initrd) — /System is unavailable"),
    }
}
