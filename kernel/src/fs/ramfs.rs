//! RamFS — an in-memory filesystem. Used as the root when no WaveFS disk is
//! present, and by tests.

use super::*;
use crate::sync::Mutex;
use alloc::collections::BTreeMap;

struct Node {
    kind: Kind,
    data: Vec<u8>,
    children: BTreeMap<String, Ino>,
    mtime: u64,
    ctime: u64,
}

struct Inner {
    nodes: BTreeMap<Ino, Node>,
    next: Ino,
}

pub struct RamFs {
    inner: Mutex<Inner>,
}

impl RamFs {
    pub fn new() -> Self {
        let now = crate::time::wall_seconds();
        let mut nodes = BTreeMap::new();
        nodes.insert(1, Node { kind: Kind::Dir, data: Vec::new(), children: BTreeMap::new(), mtime: now, ctime: now });
        Self { inner: Mutex::new(Inner { nodes, next: 2 }) }
    }
}

impl Filesystem for RamFs {
    fn describe(&self) -> String {
        String::from("RamFS (not persistent)")
    }
    fn root(&self) -> Ino {
        1
    }
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino> {
        let g = self.inner.lock();
        let d = g.nodes.get(&dir).ok_or(ENOENT)?;
        if d.kind != Kind::Dir {
            return Err(ENOTDIR);
        }
        d.children.get(name).copied().ok_or(ENOENT)
    }
    fn metadata(&self, ino: Ino) -> FsResult<Metadata> {
        let g = self.inner.lock();
        let n = g.nodes.get(&ino).ok_or(ENOENT)?;
        Ok(Metadata { kind: n.kind, size: n.data.len() as u64, mtime: n.mtime, ctime: n.ctime })
    }
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        let g = self.inner.lock();
        let n = g.nodes.get(&ino).ok_or(ENOENT)?;
        let start = (offset as usize).min(n.data.len());
        let len = buf.len().min(n.data.len() - start);
        buf[..len].copy_from_slice(&n.data[start..start + len]);
        Ok(len)
    }
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        let mut g = self.inner.lock();
        let n = g.nodes.get_mut(&ino).ok_or(ENOENT)?;
        let end = (offset as usize).checked_add(data.len()).ok_or(EINVAL)?;
        if end > 64 << 20 {
            return Err(ENOSPC);
        }
        if n.data.len() < end {
            n.data.resize(end, 0);
        }
        n.data[offset as usize..end].copy_from_slice(data);
        n.mtime = crate::time::wall_seconds();
        Ok(data.len())
    }
    fn truncate(&self, ino: Ino, size: u64) -> FsResult<()> {
        let mut g = self.inner.lock();
        let n = g.nodes.get_mut(&ino).ok_or(ENOENT)?;
        n.data.resize(size.min(64 << 20) as usize, 0);
        n.mtime = crate::time::wall_seconds();
        Ok(())
    }
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>> {
        let g = self.inner.lock();
        let d = g.nodes.get(&dir).ok_or(ENOENT)?;
        Ok(d.children
            .iter()
            .map(|(name, &ino)| DirEntry { name: name.clone(), ino, kind: g.nodes[&ino].kind })
            .collect())
    }
    fn create(&self, dir: Ino, name: &str, kind: Kind) -> FsResult<Ino> {
        let mut g = self.inner.lock();
        let ino = g.next;
        let d = g.nodes.get_mut(&dir).ok_or(ENOENT)?;
        if d.kind != Kind::Dir {
            return Err(ENOTDIR);
        }
        if d.children.contains_key(name) {
            return Err(EEXIST);
        }
        d.children.insert(String::from(name), ino);
        let now = crate::time::wall_seconds();
        g.nodes.insert(ino, Node { kind, data: Vec::new(), children: BTreeMap::new(), mtime: now, ctime: now });
        g.next += 1;
        Ok(ino)
    }
    fn unlink(&self, dir: Ino, name: &str) -> FsResult<()> {
        let mut g = self.inner.lock();
        let ino = *g.nodes.get(&dir).ok_or(ENOENT)?.children.get(name).ok_or(ENOENT)?;
        if !g.nodes[&ino].children.is_empty() {
            return Err(ENOTEMPTY);
        }
        g.nodes.get_mut(&dir).unwrap().children.remove(name);
        g.nodes.remove(&ino);
        Ok(())
    }
    fn rename(&self, from_dir: Ino, from: &str, to_dir: Ino, to: &str) -> FsResult<()> {
        let mut g = self.inner.lock();
        let ino = *g.nodes.get(&from_dir).ok_or(ENOENT)?.children.get(from).ok_or(ENOENT)?;
        let target = g.nodes.get(&to_dir).ok_or(ENOENT)?;
        if target.kind != Kind::Dir {
            return Err(ENOTDIR);
        }
        if let Some(&existing) = target.children.get(to) {
            if existing == ino {
                return Ok(());
            }
            if g.nodes[&existing].kind == Kind::Dir {
                return Err(EEXIST);
            }
            g.nodes.remove(&existing);
        }
        g.nodes.get_mut(&from_dir).unwrap().children.remove(from);
        g.nodes.get_mut(&to_dir).unwrap().children.insert(String::from(to), ino);
        Ok(())
    }
}

/// Default folders and sample documents (shared with the disk image builder).
pub const SEED_DIRS: &[&str] = &["/Desktop", "/Documents", "/Downloads", "/Pictures", "/Settings"];

pub const SEED_FILES: &[(&str, &str)] = &[
    ("/Desktop/Read Me.txt", include_str!("../../../assets/home/Desktop/Read Me.txt")),
    ("/Documents/Roadmap.txt", include_str!("../../../assets/home/Documents/Roadmap.txt")),
    ("/Documents/Ideas.txt", include_str!("../../../assets/home/Documents/Ideas.txt")),
];

/// Populates the root volume with the default layout if it is empty.
pub fn seed() {
    if super::readdir("/").is_ok_and(|e| e.iter().any(|e| e.name == "Documents")) {
        return;
    }
    for d in SEED_DIRS {
        let _ = super::mkdir(d);
    }
    for (path, text) in SEED_FILES {
        let _ = super::write_all(path, text.as_bytes());
    }
}
