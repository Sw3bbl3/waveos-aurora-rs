//! AuroraFS volumes mounted into the VFS.

use super::cache::BlockCache;
use super::*;
use crate::drivers::block::BlockDevice;
use crate::sync::Mutex;
use ::aurorafs::{Disk, Error, Volume};

/// Adapts the kernel block cache to the AuroraFS `Disk` trait.
pub struct CachedDisk(pub BlockCache);

fn io<T>(r: Result<T, isize>) -> ::aurorafs::Result<T> {
    r.map_err(|_| Error::Io)
}

impl Disk for CachedDisk {
    fn blocks(&self) -> u64 {
        self.0.blocks()
    }
    fn read(&mut self, block: u64, buf: &mut ::aurorafs::Block) -> ::aurorafs::Result<()> {
        io(self.0.read(block, buf))
    }
    fn read_run(&mut self, block: u64, buf: &mut [u8]) -> ::aurorafs::Result<()> {
        io(self.0.read_run(block, buf))
    }
    fn write(&mut self, block: u64, buf: &::aurorafs::Block) -> ::aurorafs::Result<()> {
        io(self.0.write(block, buf))
    }
    fn flush(&mut self) -> ::aurorafs::Result<()> {
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

fn kind(k: ::aurorafs::Kind) -> Kind {
    match k {
        ::aurorafs::Kind::File => Kind::File,
        ::aurorafs::Kind::Dir => Kind::Dir,
    }
}

pub struct AuroraFs {
    vol: Mutex<Volume<CachedDisk>>,
    device: String,
    /// The swap file's inode (0: none), which nothing else may touch.
    swap: core::sync::atomic::AtomicU64,
}

impl AuroraFs {
    pub fn mount(dev: Arc<dyn BlockDevice>) -> Result<AuroraFs, isize> {
        let device = super::short_device(dev.as_ref());
        let cache = BlockCache::new(dev, 4096)?;
        let vol = Volume::mount(CachedDisk(cache), crate::time::wall_seconds).map_err(errno)?;
        Ok(AuroraFs { vol: Mutex::new(vol), device, swap: core::sync::atomic::AtomicU64::new(0) })
    }

    /// Formats `dev` (used by tests).
    #[cfg_attr(not(feature = "ktest"), allow(dead_code))]
    pub fn format(dev: Arc<dyn BlockDevice>, label: &str) -> Result<AuroraFs, isize> {
        let device = super::short_device(dev.as_ref());
        let cache = BlockCache::new(dev, 1024)?;
        let uuid = (crate::time::wall_seconds() as u128 * 0x9E37_79B9_7F4A_7C15).to_le_bytes();
        let vol = Volume::format(CachedDisk(cache), label, uuid, crate::time::wall_seconds).map_err(errno)?;
        Ok(AuroraFs { vol: Mutex::new(vol), device, swap: core::sync::atomic::AtomicU64::new(0) })
    }

    pub fn label(&self) -> String {
        self.vol.lock().label()
    }

    /// Refuses access to the swap file (`EBUSY`).
    fn guard(&self, ino: Ino) -> FsResult<()> {
        if ino != 0 && self.swap.load(core::sync::atomic::Ordering::Acquire) == ino {
            return Err(EBUSY);
        }
        Ok(())
    }

    /// [`AuroraFs::guard`] for the entry `name` in `dir`.
    fn guard_entry(&self, v: &mut Volume<CachedDisk>, dir: Ino, name: &str) -> FsResult<()> {
        match v.lookup(dir, name) {
            Ok(ino) => self.guard(ino),
            Err(_) => Ok(()),
        }
    }
}

impl Filesystem for AuroraFs {
    fn describe(&self) -> String {
        let label = self.label();
        alloc::format!("AuroraFS “{}” · {}", label, self.device)
    }
    fn root(&self) -> Ino {
        ::aurorafs::ROOT
    }
    fn shrink(&self) {
        if let Some(mut vol) = self.vol.try_lock() {
            vol.disk().0.shrink(256);
        }
    }
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino> {
        self.vol.lock().lookup(dir, name).map_err(errno)
    }
    fn metadata(&self, ino: Ino) -> FsResult<Metadata> {
        let m = self.vol.lock().stat(ino).map_err(errno)?;
        Ok(Metadata { kind: kind(m.kind), size: m.size, mtime: m.mtime, ctime: m.ctime })
    }
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        self.guard(ino)?;
        self.vol.lock().read(ino, offset, buf).map_err(errno)
    }
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        self.guard(ino)?;
        let mut v = self.vol.lock();
        if v.stat(ino).map_err(errno)?.kind == ::aurorafs::Kind::Dir {
            return Err(EISDIR);
        }
        v.write(ino, offset, data).map_err(errno)
    }
    fn truncate(&self, ino: Ino, size: u64) -> FsResult<()> {
        self.guard(ino)?;
        self.vol.lock().truncate(ino, size).map_err(errno)
    }
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>> {
        let entries = self.vol.lock().readdir(dir).map_err(errno)?;
        Ok(entries.into_iter().map(|e| DirEntry { name: e.name, ino: e.ino, kind: kind(e.kind) }).collect())
    }
    fn create(&self, dir: Ino, name: &str, k: Kind) -> FsResult<Ino> {
        self.create_owned(dir, name, k, None)
    }
    fn create_owned(&self, dir: Ino, name: &str, k: Kind, owner: Option<Owner>) -> FsResult<Ino> {
        let k = if k == Kind::Dir { ::aurorafs::Kind::Dir } else { ::aurorafs::Kind::File };
        let owner = owner.map(|o| ::aurorafs::Owner { uid: o.uid, gid: o.gid, mode: o.mode });
        self.vol.lock().create_owned(dir, name, k, owner).map_err(errno)
    }
    fn owner(&self, ino: Ino) -> Option<(Owner, bool)> {
        let mut v = self.vol.lock();
        let o = v.owner(ino).ok()?;
        let explicit = v.has_own_owner(ino).ok()?;
        // Before owners are turned on, everything is the first account's.
        let (uid, gid) = if explicit || v.features() & ::aurorafs::FEATURE_OWNERS != 0 {
            (o.uid, o.gid)
        } else {
            (::aurorafs::FIRST_UID, ::aurorafs::FIRST_GID)
        };
        Some((Owner { uid, gid, mode: o.mode }, explicit))
    }
    fn set_owner(&self, ino: Ino, o: Owner) -> FsResult<()> {
        let owner = ::aurorafs::Owner { uid: o.uid, gid: o.gid, mode: o.mode };
        self.vol.lock().set_owner(ino, owner).map_err(errno)
    }
    fn enable_owners(&self, uid: u32, gid: u32) -> FsResult<bool> {
        let mut v = self.vol.lock();
        if v.features() & ::aurorafs::FEATURE_OWNERS != 0 {
            return Ok(false);
        }
        v.enable_owners(uid, gid).map_err(errno)?;
        Ok(true)
    }
    fn unlink(&self, dir: Ino, name: &str) -> FsResult<()> {
        let mut v = self.vol.lock();
        self.guard_entry(&mut v, dir, name)?;
        v.unlink(dir, name).map_err(errno)
    }
    fn rename(&self, from_dir: Ino, from: &str, to_dir: Ino, to: &str) -> FsResult<()> {
        let mut v = self.vol.lock();
        self.guard_entry(&mut v, from_dir, from)?;
        self.guard_entry(&mut v, to_dir, to)?;
        v.rename(from_dir, from, to_dir, to).map_err(errno)
    }
    fn swap_file(&self, ino: Ino, size: u64) -> FsResult<SwapExtents> {
        use core::sync::atomic::Ordering;
        let mut v = self.vol.lock();
        let current = self.swap.load(Ordering::Acquire);
        if current != 0 && current != ino {
            return Err(EBUSY);
        }
        v.allocate(ino, size).map_err(errno)?;
        let extents = v.extents(ino).map_err(errno)?;
        // Nothing of the file may be left in the cache to be written later.
        let cache = &mut v.disk().0;
        cache.flush()?;
        for &(start, len) in &extents {
            cache.disown(start, len);
        }
        let (dev, per_block) = cache.device();
        let mut runs = Vec::new();
        let mut page = 0;
        for (start, len) in extents {
            runs.push((page, start * per_block, len));
            page += len;
        }
        self.swap.store(ino, Ordering::Release);
        Ok(SwapExtents { dev, runs })
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
