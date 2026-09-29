//! WaveFS volumes mounted into the VFS.

use super::cache::BlockCache;
use super::*;
use crate::drivers::block::BlockDevice;
use crate::sync::Mutex;
use ::wavefs::{Disk, Error, Volume};

/// Adapts the kernel block cache to the WaveFS `Disk` trait.
pub struct CachedDisk(pub BlockCache);

fn io<T>(r: Result<T, isize>) -> ::wavefs::Result<T> {
    r.map_err(|_| Error::Io)
}

impl Disk for CachedDisk {
    fn blocks(&self) -> u64 {
        self.0.blocks()
    }
    fn read(&mut self, block: u64, buf: &mut ::wavefs::Block) -> ::wavefs::Result<()> {
        io(self.0.read(block, buf))
    }
    fn write(&mut self, block: u64, buf: &::wavefs::Block) -> ::wavefs::Result<()> {
        io(self.0.write(block, buf))
    }
    fn flush(&mut self) -> ::wavefs::Result<()> {
        io(self.0.flush())
    }
}

fn errno(e: Error) -> isize {
    match e {
        Error::NotFound => ENOENT,
        Error::Exists => EEXIST,
        Error::NotDir => ENOTDIR,
        Error::IsDir => EISDIR,
        Error::NotEmpty => ENOTEMPTY,
        Error::NoSpace => ENOSPC,
        Error::Io | Error::Corrupt => EIO,
        Error::NameTooLong => ENAMETOOLONG,
        Error::Invalid | Error::TooBig => EINVAL,
    }
}

fn kind(k: ::wavefs::Kind) -> Kind {
    match k {
        ::wavefs::Kind::File => Kind::File,
        ::wavefs::Kind::Dir => Kind::Dir,
    }
}

pub struct WaveFs {
    vol: Mutex<Volume<CachedDisk>>,
    device: String,
}

impl WaveFs {
    pub fn mount(dev: Arc<dyn BlockDevice>) -> Result<WaveFs, isize> {
        let device = dev.describe();
        let cache = BlockCache::new(dev, 4096)?;
        let vol = Volume::mount(CachedDisk(cache), crate::time::wall_seconds).map_err(errno)?;
        Ok(WaveFs { vol: Mutex::new(vol), device })
    }

    /// Formats `dev` (used by tests).
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))]
    pub fn format(dev: Arc<dyn BlockDevice>, label: &str) -> Result<WaveFs, isize> {
        let device = dev.describe();
        let cache = BlockCache::new(dev, 1024)?;
        let uuid = (crate::time::wall_seconds() as u128 * 0x9E37_79B9_7F4A_7C15).to_le_bytes();
        let vol = Volume::format(CachedDisk(cache), label, uuid, crate::time::wall_seconds).map_err(errno)?;
        Ok(WaveFs { vol: Mutex::new(vol), device })
    }

    pub fn label(&self) -> String {
        self.vol.lock().label()
    }
}

impl Filesystem for WaveFs {
    fn describe(&self) -> String {
        let label = self.label();
        alloc::format!("WaveFS \"{}\" on {}", label, self.device)
    }
    fn root(&self) -> Ino {
        ::wavefs::ROOT
    }
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino> {
        self.vol.lock().lookup(dir, name).map_err(errno)
    }
    fn metadata(&self, ino: Ino) -> FsResult<Metadata> {
        let m = self.vol.lock().stat(ino).map_err(errno)?;
        Ok(Metadata { kind: kind(m.kind), size: m.size, mtime: m.mtime, ctime: m.ctime })
    }
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        self.vol.lock().read(ino, offset, buf).map_err(errno)
    }
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        let mut v = self.vol.lock();
        if v.stat(ino).map_err(errno)?.kind == ::wavefs::Kind::Dir {
            return Err(EISDIR);
        }
        v.write(ino, offset, data).map_err(errno)
    }
    fn truncate(&self, ino: Ino, size: u64) -> FsResult<()> {
        self.vol.lock().truncate(ino, size).map_err(errno)
    }
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>> {
        let entries = self.vol.lock().readdir(dir).map_err(errno)?;
        Ok(entries.into_iter().map(|e| DirEntry { name: e.name, ino: e.ino, kind: kind(e.kind) }).collect())
    }
    fn create(&self, dir: Ino, name: &str, k: Kind) -> FsResult<Ino> {
        let k = if k == Kind::Dir { ::wavefs::Kind::Dir } else { ::wavefs::Kind::File };
        self.vol.lock().create(dir, name, k).map_err(errno)
    }
    fn unlink(&self, dir: Ino, name: &str) -> FsResult<()> {
        self.vol.lock().unlink(dir, name).map_err(errno)
    }
    fn rename(&self, from_dir: Ino, from: &str, to_dir: Ino, to: &str) -> FsResult<()> {
        self.vol.lock().rename(from_dir, from, to_dir, to).map_err(errno)
    }
    fn sync(&self) -> FsResult<()> {
        self.vol.lock().commit().map_err(errno)
    }
    fn space(&self) -> Option<(u64, u64)> {
        Some(self.vol.lock().space())
    }
    fn check(&self) -> Result<(), String> {
        self.vol.lock().fsck()
    }
}
