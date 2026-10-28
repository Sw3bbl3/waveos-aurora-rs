//! FAT32 volumes (the EFI System Partition) mounted into the VFS.

use super::*;
use crate::drivers::block::BlockDevice;
use crate::sync::Mutex;
use ::fat32::{Error, Fat32, SectorDisk, SECTOR};

/// 512-byte sector access on any block device (emulated on 4 KiB-sector
/// disks), reading ahead in 32 KiB windows: the FAT library asks for one
/// sector at a time, and mounting alone reads the whole allocation table.
pub struct Sectors {
    dev: Arc<dyn BlockDevice>,
    /// A window of the disk: its first 512-byte sector and contents.
    window: Option<(u64, Vec<u8>)>,
}

/// Read-ahead window, in 512-byte sectors.
const WINDOW: u64 = 64;

impl Sectors {
    pub fn new(dev: Arc<dyn BlockDevice>) -> Sectors {
        Sectors { dev, window: None }
    }

    /// Native sectors per 512-byte sector group.
    fn per(&self) -> u64 {
        (self.dev.sector_size() as u64 / SECTOR as u64).max(1)
    }

    fn cached(&self, lba: u64) -> Option<&[u8]> {
        let (start, data) = self.window.as_ref()?;
        let o = lba.checked_sub(*start)? as usize * SECTOR;
        data.get(o..o + SECTOR)
    }

    /// Reads the window holding `lba` (aligned, clipped to the disk's end).
    fn fill(&mut self, lba: u64) -> ::fat32::Result<()> {
        let per = self.per();
        let window = WINDOW.max(per);
        let start = lba / window * window;
        let count = window.min(self.sectors() - start);
        let mut data = alloc::vec![0u8; (count * SECTOR as u64) as usize];
        self.dev.read(start / per, &mut data).map_err(|_| Error::Io)?;
        self.window = Some((start, data));
        Ok(())
    }
}

impl SectorDisk for Sectors {
    fn sectors(&self) -> u64 {
        self.dev.sectors() * self.per()
    }
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> ::fat32::Result<()> {
        if self.cached(lba).is_none() {
            self.fill(lba)?;
        }
        buf.copy_from_slice(self.cached(lba).ok_or(Error::Io)?);
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8; SECTOR]) -> ::fat32::Result<()> {
        let per = self.per();
        if per == 1 {
            self.dev.write(lba, buf).map_err(|_| Error::Io)?;
        } else {
            let mut tmp = alloc::vec![0u8; self.dev.sector_size() as usize];
            self.dev.read(lba / per, &mut tmp).map_err(|_| Error::Io)?;
            let o = (lba % per) as usize * SECTOR;
            tmp[o..o + SECTOR].copy_from_slice(buf);
            self.dev.write(lba / per, &tmp).map_err(|_| Error::Io)?;
        }
        // Keep the window current.
        if let Some((start, data)) = self.window.as_mut() {
            if let Some(o) = lba.checked_sub(*start).map(|d| d as usize * SECTOR) {
                if let Some(slot) = data.get_mut(o..o + SECTOR) {
                    slot.copy_from_slice(buf);
                }
            }
        }
        Ok(())
    }
    fn flush(&mut self) -> ::fat32::Result<()> {
        self.dev.flush().map_err(|_| Error::Io)
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
        Error::Unsupported | Error::Invalid => EINVAL,
        Error::NameTooLong => ENAMETOOLONG,
    }
}

pub struct FatFs {
    fs: Mutex<Fat32<Sectors>>,
    device: String,
}

impl FatFs {
    pub fn mount(dev: Arc<dyn BlockDevice>) -> Result<FatFs, isize> {
        let device = super::short_device(dev.as_ref());
        let fs = Fat32::mount(Sectors::new(dev), crate::time::wall_seconds).map_err(errno)?;
        Ok(FatFs { fs: Mutex::new(fs), device })
    }
}

impl Filesystem for FatFs {
    fn describe(&self) -> String {
        alloc::format!("FAT32 · {}", self.device)
    }
    fn root(&self) -> Ino {
        ::fat32::ROOT
    }
    fn lookup(&self, dir: Ino, name: &str) -> FsResult<Ino> {
        self.fs.lock().lookup(dir, name).map_err(errno)
    }
    fn metadata(&self, ino: Ino) -> FsResult<Metadata> {
        let m = self.fs.lock().stat(ino).map_err(errno)?;
        Ok(Metadata {
            kind: if m.is_dir { Kind::Dir } else { Kind::File },
            size: m.size,
            mtime: m.mtime,
            ctime: m.mtime,
        })
    }
    fn read(&self, ino: Ino, offset: u64, buf: &mut [u8]) -> FsResult<usize> {
        self.fs.lock().read(ino, offset, buf).map_err(errno)
    }
    fn write(&self, ino: Ino, offset: u64, data: &[u8]) -> FsResult<usize> {
        self.fs.lock().write(ino, offset, data).map_err(errno)
    }
    fn truncate(&self, ino: Ino, size: u64) -> FsResult<()> {
        self.fs.lock().truncate(ino, size).map_err(errno)
    }
    fn readdir(&self, dir: Ino) -> FsResult<Vec<DirEntry>> {
        let entries = self.fs.lock().readdir(dir).map_err(errno)?;
        Ok(entries
            .into_iter()
            .map(|(name, ino, is_dir)| DirEntry { name, ino, kind: if is_dir { Kind::Dir } else { Kind::File } })
            .collect())
    }
    fn create(&self, dir: Ino, name: &str, kind: Kind) -> FsResult<Ino> {
        self.fs.lock().create(dir, name, kind == Kind::Dir).map_err(errno)
    }
    fn unlink(&self, dir: Ino, name: &str) -> FsResult<()> {
        self.fs.lock().unlink(dir, name).map_err(errno)
    }
    fn rename(&self, from_dir: Ino, from: &str, to_dir: Ino, to: &str) -> FsResult<()> {
        self.fs.lock().rename(from_dir, from, to_dir, to).map_err(errno)
    }
    fn sync(&self) -> FsResult<()> {
        self.fs.lock().flush().map_err(errno)
    }
    fn space(&self) -> Option<(u64, u64)> {
        Some(self.fs.lock().space())
    }
}
