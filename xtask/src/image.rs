//! Builds bootable raw disk images:
//!
//! ```text
//! LBA 0        protective MBR
//! LBA 1..33    GPT header + partition entries
//! 1 MiB        partition 1: EFI System Partition (FAT32) — bootloader, kernel, system image
//!  +64 MiB     partition 2: "Aurora HD" (AuroraFS) — the user's files
//! last 1 MiB   unpartitioned scratch area (used by the kernel's driver self-tests)
//! end-33..end  backup GPT
//! ```
//!
//! `refresh_esp` rewrites only the ESP, so files on the AuroraFS partition
//! survive rebuilds.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const SECTOR: u64 = 512;
const ENTRIES: u64 = 128;
const ENTRY_SIZE: u64 = 128;
const ENTRY_SECTORS: u64 = ENTRIES * ENTRY_SIZE / SECTOR; // 32
const ESP_START: u64 = 2048; // 1 MiB
const ESP_SECTORS: u64 = 64 * 1024 * 1024 / SECTOR;
pub const SCRATCH_SECTORS: u64 = 2048;

/// EFI System Partition C12A7328-F81F-11D2-BA4B-00A0C93EC93B, on-disk byte order.
const ESP_TYPE: [u8; 16] =
    [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];

struct Layout {
    total: u64,
    home_start: u64,
    home_end: u64,
}

fn layout(size: u64) -> Layout {
    let total = size / SECTOR;
    let last_usable = total - 1 - ENTRY_SECTORS - 1;
    let home_start = ESP_START + ESP_SECTORS;
    let home_end = last_usable - SCRATCH_SECTORS; // inclusive
    Layout { total, home_start, home_end }
}

/// Creates a fresh image: GPT, FAT32 ESP with `esp`'s contents, and a AuroraFS
/// home volume seeded from `home` (a directory tree), with a Linux system
/// root (`cargo xtask linux-root`), if given, on it as `/Linux`.
pub fn create_with(esp: &Path, home: &Path, linux: Option<&Path>, out: &Path, size: u64) -> io::Result<()> {
    let l = layout(size);
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out)?;
    file.set_len(size)?;
    write_gpt(&mut file, &l)?;
    format_esp(&mut file, ESP_START, ESP_SECTORS, esp)?;
    mkfs_home(&mut file, l.home_start, l.home_end - l.home_start + 1, home, linux)?;
    Ok(())
}

/// What [`add_linux`] did to an existing image.
pub enum LinuxOnDisk {
    /// The Linux root went onto the disk.
    Added,
    /// It was there already; WaveOS's settings files were rewritten.
    Updated,
    /// It was there, from a different package list: the files new to it
    /// were added (and the settings rewritten).
    Upgraded,
    /// The home volume has `free` bytes, and the Linux root needs `need`.
    NoRoom { need: u64, free: u64 },
}

/// Read a persisted guest preference before starting QEMU.
pub fn preference(img: &Path, key: &str) -> Option<String> {
    let mut file = OpenOptions::new().read(true).write(true).open(img).ok()?;
    let mut entry = [0u8; ENTRY_SIZE as usize];
    file.seek(SeekFrom::Start(2 * SECTOR + ENTRY_SIZE)).ok()?;
    file.read_exact(&mut entry).ok()?;
    if entry[0..16] != aurorafs::PARTITION_TYPE {
        return None;
    }
    let start = u64::from_le_bytes(entry[32..40].try_into().ok()?);
    let end = u64::from_le_bytes(entry[40..48].try_into().ok()?);
    let bytes = end.checked_sub(start)?.checked_add(1)?.checked_mul(SECTOR)?;
    let region = Region { file: &mut file, start: start * SECTOR, len: bytes, pos: 0 };
    let mut volume = aurorafs::Volume::mount(WaveDisk { region, blocks: bytes / 4096 }, now).ok()?;
    let data = read_file(&mut volume, "/Settings/aurora.conf");
    core::str::from_utf8(&data).ok()?.lines().find_map(|line| {
        let (k, v) = line.split_once('=')?;
        (k.trim() == key).then(|| v.trim().to_owned())
    })
}

/// Mounts the home volume of a copy of `img` (replaying its journal, as a
/// start would) and checks it with `fsck`; `img` itself is not changed.
pub fn check_home_copy(img: &Path) -> Result<(), String> {
    let copy = img.with_extension("check.img");
    fs::copy(img, &copy).map_err(|e| e.to_string())?;
    let result = (|| {
        let mut file = OpenOptions::new().read(true).write(true).open(&copy).map_err(|e| e.to_string())?;
        let mut entry = [0u8; ENTRY_SIZE as usize];
        file.seek(SeekFrom::Start(2 * SECTOR + ENTRY_SIZE)).map_err(|e| e.to_string())?;
        file.read_exact(&mut entry).map_err(|e| e.to_string())?;
        let start = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let end = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let bytes = (end - start + 1) * SECTOR;
        let region = Region { file: &mut file, start: start * SECTOR, len: bytes, pos: 0 };
        let mut vol = aurorafs::Volume::mount(WaveDisk { region, blocks: bytes / 4096 }, now)
            .map_err(|e| format!("mount: {e:?}"))?;
        vol.fsck()
    })();
    let _ = fs::remove_file(&copy);
    result
}

/// Checks the home volume of disk image `img` (`cargo xtask fsck IMG`):
/// replays its journal in memory, runs `fsck`, and lists the top folders.
/// (The journal replay writes: run it on a copy to keep the image as it was.)
pub fn check_home(img: &Path) -> io::Result<String> {
    let mut file = OpenOptions::new().read(true).write(true).open(img)?;
    let mut entry = [0u8; ENTRY_SIZE as usize];
    file.seek(SeekFrom::Start(2 * SECTOR + ENTRY_SIZE))?;
    file.read_exact(&mut entry)?;
    if entry[0..16] != aurorafs::PARTITION_TYPE {
        return Err(io::Error::other("the disk has no AuroraFS volume"));
    }
    let start = u64::from_le_bytes(entry[32..40].try_into().unwrap());
    let end = u64::from_le_bytes(entry[40..48].try_into().unwrap());
    let err = |e: aurorafs::Error| io::Error::other(format!("AuroraFS: {e:?}"));
    let mut vol = home_volume(&mut file, (start, end)).map_err(err)?;
    let mut out = String::new();
    vol.fsck().map_err(io::Error::other)?;
    let (total, free) = vol.space();
    out.push_str(&format!("fsck: clean; {} of {} MiB free\n", free >> 20, total >> 20));
    for dir in ["/", "/Security"] {
        let ino = vol.resolve(dir).map_err(err)?;
        for e in vol.readdir(ino).map_err(err)? {
            let size = vol.stat(e.ino).map(|m| m.size).unwrap_or(0);
            out.push_str(&format!("{dir}{}{}: {:?}, {size} bytes\n", if dir == "/" { "" } else { "/" }, e.name, e.kind));
        }
    }
    Ok(out)
}

/// Put the Linux root on the home volume as `/Linux`, preserving saved files.
/// Existing installations receive updated settings and missing packages.
pub fn add_linux(img: &Path, linux: &Path, settings: &[(String, PathBuf)]) -> io::Result<LinuxOnDisk> {
    let mut file = OpenOptions::new().read(true).write(true).open(img)?;
    let mut entry = [0u8; ENTRY_SIZE as usize];
    file.seek(SeekFrom::Start(2 * SECTOR + ENTRY_SIZE))?;
    file.read_exact(&mut entry)?;
    if entry[0..16] != aurorafs::PARTITION_TYPE {
        return Err(io::Error::other("the disk has no AuroraFS volume"));
    }
    let start = u64::from_le_bytes(entry[32..40].try_into().unwrap());
    let end = u64::from_le_bytes(entry[40..48].try_into().unwrap());
    let bytes = (end - start + 1) * SECTOR;
    let region = Region { file: &mut file, start: start * SECTOR, len: bytes, pos: 0 };
    let err = |e: aurorafs::Error| io::Error::other(format!("AuroraFS: {e:?}"));
    let mut vol = aurorafs::Volume::mount(WaveDisk { region, blocks: bytes / 4096 }, now).map_err(err)?;
    let done = if vol.resolve("/Linux/usr/lib/firefox-esr/firefox-esr").is_ok() {
        let mut done = LinuxOnDisk::Updated;
        let packages = fs::read(linux.join("packages.txt")).unwrap_or_default();
        if read_file(&mut vol, "/Linux/packages.txt") != packages {
            let need = missing_blocks(&mut vol, linux, "/Linux") * 4096 * 9 / 8;
            let (_, free) = vol.space();
            if free < need {
                return Ok(LinuxOnDisk::NoRoom { need, free });
            }
            seed_missing(&mut vol, linux, "/Linux").map_err(err)?;
            // What was made from the old package set is out of date: take it again.
            for (_, dir) in super::debian::GENERATED {
                seed(&mut vol, &linux.join(dir), &format!("/Linux/{dir}")).map_err(err)?;
            }
            vol.write_file("/Linux/packages.txt", &packages).map_err(err)?;
            done = LinuxOnDisk::Upgraded;
        }
        // The table of links describes the root as xtask makes it now.
        vol.write_file("/Linux/.links", &fs::read(linux.join(".links"))?).map_err(err)?;
        for (path, from) in settings {
            let path = format!("/Linux{path}");
            vol.mkdir_all(&path[..path.rfind('/').unwrap()]).map_err(err)?;
            vol.write_file(&path, &fs::read(from)?).map_err(err)?;
        }
        done
    } else {
        // Whole blocks per file, and an eighth more for the volume's own records.
        let need = tree_blocks(linux) * 4096 * 9 / 8;
        let (_, free) = vol.space();
        if free < need {
            return Ok(LinuxOnDisk::NoRoom { need, free });
        }
        vol.mkdir_all("/Linux").map_err(err)?;
        seed(&mut vol, linux, "/Linux").map_err(err)?;
        LinuxOnDisk::Added
    };
    vol.commit().map_err(err)?;
    vol.fsck().map_err(io::Error::other)?;
    Ok(done)
}

/// Makes `img` `size` bytes, keeping everything on it: a new image gets the
/// same ESP and a bigger home volume holding a copy of every file and folder
/// (with their times), and takes the old one's place once the copy is
/// checked against it. Nothing changes if any step fails.
pub fn grow(img: &Path, size: u64) -> io::Result<()> {
    let mut old = OpenOptions::new().read(true).write(true).open(img)?;
    let esp = partition(&mut old, 0)?;
    let home = partition(&mut old, 1)?;
    let l = layout(size);
    if esp != (ESP_START, ESP_START + ESP_SECTORS - 1) || home.0 != l.home_start || size <= old.metadata()?.len() {
        return Err(io::Error::other("the disk isn't laid out the way WaveOS makes it"));
    }
    let tmp = img.with_extension("img.growing");
    let copied = (|| {
        let mut new = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&tmp)?;
        new.set_len(size)?;
        write_gpt(&mut new, &l)?;
        // The ESP as it is, boot options included.
        let mut buf = vec![0u8; 1 << 20];
        for at in (0..ESP_SECTORS * SECTOR).step_by(buf.len()) {
            old.seek(SeekFrom::Start(ESP_START * SECTOR + at))?;
            old.read_exact(&mut buf)?;
            write_at(&mut new, ESP_START * SECTOR + at, &buf)?;
        }
        let err = |e: aurorafs::Error| io::Error::other(format!("AuroraFS: {e:?}"));
        {
            let mut from = home_volume(&mut old, home).map_err(err)?;
            let bytes = (l.home_end - l.home_start + 1) * SECTOR;
            let disk = WaveDisk {
                region: Region { file: &mut new, start: l.home_start * SECTOR, len: bytes, pos: 0 },
                blocks: bytes / 4096,
            };
            let mut to = aurorafs::Volume::format(disk, &from.label(), pseudo_guid(3), now).map_err(err)?;
            // Owners as they were: the same default owner (a volume from
            // before owners gets the first account, as the kernel would
            // give it), and each inode's own.
            if from.features() & aurorafs::FEATURE_OWNERS != 0 {
                let (uid, gid) = from.default_owner();
                to.enable_owners(uid, gid).map_err(err)?;
            }
            copy_tree(&mut from, &mut to, aurorafs::ROOT, "").map_err(err)?;
            let root = from.stat(aurorafs::ROOT).map_err(err)?;
            if from.has_own_owner(aurorafs::ROOT).map_err(err)? {
                to.set_owner(aurorafs::ROOT, root.owner).map_err(err)?;
            }
            to.set_times(aurorafs::ROOT, root.ctime, root.mtime).map_err(err)?;
            to.commit().map_err(err)?;
            to.fsck().map_err(io::Error::other)?;
            if !same_tree(&mut from, &mut to, aurorafs::ROOT, aurorafs::ROOT).map_err(err)? {
                return Err(io::Error::other("the copy differs from the disk"));
            }
        }
        new.sync_all()
    })();
    match copied {
        Ok(()) => fs::rename(&tmp, img),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// The first and last sector of partition `index` in the GPT.
fn partition(file: &mut File, index: u64) -> io::Result<(u64, u64)> {
    let mut entry = [0u8; ENTRY_SIZE as usize];
    file.seek(SeekFrom::Start(2 * SECTOR + index * ENTRY_SIZE))?;
    file.read_exact(&mut entry)?;
    Ok((u64::from_le_bytes(entry[32..40].try_into().unwrap()), u64::from_le_bytes(entry[40..48].try_into().unwrap())))
}

fn home_volume(file: &mut File, (start, end): (u64, u64)) -> aurorafs::Result<aurorafs::Volume<WaveDisk<'_>>> {
    let bytes = (end - start + 1) * SECTOR;
    let region = Region { file, start: start * SECTOR, len: bytes, pos: 0 };
    aurorafs::Volume::mount(WaveDisk { region, blocks: bytes / 4096 }, now)
}

/// Copies the folder `dir` of `from` to `path` on `to`, times included.
fn copy_tree<A: aurorafs::Disk, B: aurorafs::Disk>(
    from: &mut aurorafs::Volume<A>,
    to: &mut aurorafs::Volume<B>,
    dir: u64,
    path: &str,
) -> aurorafs::Result<()> {
    let parent = to.resolve(path)?;
    let mut buf = vec![0u8; 1 << 20];
    for e in from.readdir(dir)? {
        let meta = from.stat(e.ino)?;
        let owner = from.has_own_owner(e.ino)?.then_some(meta.owner);
        let ino = to.create_owned(parent, &e.name, e.kind, owner)?;
        if e.kind == aurorafs::Kind::Dir {
            copy_tree(from, to, e.ino, &format!("{path}/{}", e.name))?;
        } else {
            let mut at = 0;
            while at < meta.size {
                let n = from.read(e.ino, at, &mut buf)?;
                if n == 0 {
                    return Err(aurorafs::Error::Io);
                }
                to.write(ino, at, &buf[..n])?;
                at += n as u64;
            }
        }
        // After the contents, which change a folder's times.
        to.set_times(ino, meta.ctime, meta.mtime)?;
    }
    Ok(())
}

/// Whether folders `a` and `b` hold the same names, kinds, times and contents.
fn same_tree<A: aurorafs::Disk, B: aurorafs::Disk>(
    from: &mut aurorafs::Volume<A>,
    to: &mut aurorafs::Volume<B>,
    a: u64,
    b: u64,
) -> aurorafs::Result<bool> {
    let mut left = from.readdir(a)?;
    let mut right = to.readdir(b)?;
    left.sort_by(|x, y| x.name.cmp(&y.name));
    right.sort_by(|x, y| x.name.cmp(&y.name));
    if left.len() != right.len() {
        return Ok(false);
    }
    let (mut x, mut y) = (vec![0u8; 1 << 20], vec![0u8; 1 << 20]);
    for (l, r) in left.iter().zip(&right) {
        let (ml, mr) = (from.stat(l.ino)?, to.stat(r.ino)?);
        if l.name != r.name
            || ml.kind != mr.kind
            || ml.size != mr.size
            || (ml.ctime, ml.mtime) != (mr.ctime, mr.mtime)
            || own_owner(from, l.ino)? != own_owner(to, r.ino)?
        {
            return Ok(false);
        }
        if ml.kind == aurorafs::Kind::Dir {
            if !same_tree(from, to, l.ino, r.ino)? {
                return Ok(false);
            }
            continue;
        }
        let mut at = 0;
        while at < ml.size {
            let n = from.read(l.ino, at, &mut x)?;
            if n == 0 || to.read(r.ino, at, &mut y[..n])? != n || x[..n] != y[..n] {
                return Ok(false);
            }
            at += n as u64;
        }
    }
    Ok(true)
}

/// The owner an inode carries itself (None: the volume's default).
fn own_owner<D: aurorafs::Disk>(vol: &mut aurorafs::Volume<D>, ino: u64) -> aurorafs::Result<Option<aurorafs::Owner>> {
    Ok(if vol.has_own_owner(ino)? { Some(vol.owner(ino)?) } else { None })
}

/// A file's contents on the volume (empty if it isn't there).
fn read_file<D: aurorafs::Disk>(vol: &mut aurorafs::Volume<D>, path: &str) -> Vec<u8> {
    let Ok(ino) = vol.resolve(path) else { return Vec::new() };
    let size = vol.stat(ino).map(|m| m.size).unwrap_or(0);
    let mut data = vec![0u8; size as usize];
    let got = vol.read(ino, 0, &mut data).unwrap_or(0);
    data.truncate(got);
    data
}

/// [`tree_blocks`] for the files under `src` that aren't at `prefix` yet.
fn missing_blocks<D: aurorafs::Disk>(vol: &mut aurorafs::Volume<D>, src: &Path, prefix: &str) -> u64 {
    let Ok(entries) = fs::read_dir(src) else { return 0 };
    let mut blocks = 0;
    for entry in entries.flatten() {
        let path = format!("{prefix}/{}", entry.file_name().to_string_lossy());
        match entry.metadata() {
            Ok(m) if m.is_dir() => blocks += 1 + missing_blocks(vol, &entry.path(), &path),
            Ok(m) if vol.resolve(&path).is_err() => blocks += 1 + m.len().div_ceil(4096),
            _ => {}
        }
    }
    blocks
}

/// [`seed`], leaving the files already at `prefix` as they are.
fn seed_missing<D: aurorafs::Disk>(vol: &mut aurorafs::Volume<D>, src: &Path, prefix: &str) -> aurorafs::Result<()> {
    let Ok(entries) = fs::read_dir(src) else { return Ok(()) };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{prefix}/{name}");
        if entry.path().is_dir() {
            vol.mkdir_all(&path)?;
            seed_missing(vol, &entry.path(), &path)?;
        } else if vol.resolve(&path).is_err() {
            let ino = vol.write_file(&path, &fs::read(entry.path()).map_err(|_| aurorafs::Error::Io)?)?;
            keep_exec_bit(vol, ino, &entry.path())?;
        }
    }
    Ok(())
}

/// Files the host marks executable (programs in the Linux tree) stay
/// executable: `rwxr-xr-x`, owned by the first account. Others keep the
/// volume's default owner and `rw-r--r--`.
fn keep_exec_bit<D: aurorafs::Disk>(vol: &mut aurorafs::Volume<D>, ino: u64, host: &Path) -> aurorafs::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let executable = fs::metadata(host).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
        if executable {
            let (uid, gid) = vol.default_owner();
            vol.set_owner(ino, aurorafs::Owner { uid, gid, mode: 0o755 })?;
        }
    }
    #[cfg(not(unix))]
    let _ = (vol, ino, host);
    Ok(())
}

/// 4 KiB blocks the files under `dir` take, one more for each file and folder.
fn tree_blocks(dir: &Path) -> u64 {
    fs::read_dir(dir)
        .map(|d| {
            d.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => 1 + tree_blocks(&e.path()),
                    Ok(m) => 1 + m.len().div_ceil(4096),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Creates a FAT16 EFI boot image for an optical disc's El Torito entry.
/// UEFI firmware mounts this image as the boot volume.
pub fn create_efi_boot_image(esp: &Path, out: &Path) -> io::Result<()> {
    // El Torito stores the load size in a 16-bit count of 512-byte sectors.
    // Keep the image below that limit for firmware that cannot handle size 0.
    const ISO_ESP_SECTORS: u64 = 32 * 1024 * 1024 / SECTOR - 4;
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out)?;
    file.set_len(ISO_ESP_SECTORS * SECTOR)?;
    format_esp_with_type(&mut file, 0, ISO_ESP_SECTORS, esp, fatfs::FatType::Fat16)?;
    file.sync_all()
}

/// The file on an EFI partition that names the processor it boots
/// (`x86_64` or `aarch64`, see `arch.rs`).
pub const ARCH_FILE: &str = "aurora/arch";

/// A disk for one architecture offered an EFI partition for another.
pub struct ArchMismatch {
    pub disk: String,
    pub new: String,
}

impl std::fmt::Display for ArchMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let (disk, new) = (&self.disk, &self.new);
        write!(
            f,
            "it is a disk for {disk} and this build is for {new}, so its EFI partition was left as it was \
             (an {new} system on it would not start). Run with `--arch {disk}` to use it, or give {new} a disk \
             of its own with `--disk-image`"
        )
    }
}

/// The architecture an EFI partition folder is for: its [`ARCH_FILE`], or
/// else the removable-media boot file it holds.
pub fn esp_dir_arch(esp: &Path) -> Option<String> {
    if let Ok(text) = fs::read_to_string(esp.join(ARCH_FILE)) {
        return Some(text.trim().to_string());
    }
    crate::arch::ALL.iter().find(|a| esp.join("EFI/BOOT").join(a.boot_file()).exists()).map(|a| a.name().to_string())
}

/// The architecture of the EFI partition at sector `start`, by the same
/// signs as [`esp_dir_arch`] (`None`: an empty or foreign partition).
fn fat_arch(file: &mut File, start: u64, sectors: u64) -> Option<String> {
    let part = Region { file, start: start * SECTOR, len: sectors * SECTOR, pos: 0 };
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new()).ok()?;
    let root = fs.root_dir();
    if let Ok(mut f) = root.open_file(ARCH_FILE) {
        let mut text = String::new();
        if f.read_to_string(&mut text).is_ok() && !text.trim().is_empty() {
            return Some(text.trim().to_string());
        }
    }
    crate::arch::ALL
        .iter()
        .find(|a| root.open_file(&format!("EFI/BOOT/{}", a.boot_file())).is_ok())
        .map(|a| a.name().to_string())
}

/// The architecture of the disk `img` (see [`fat_arch`]).
pub fn disk_arch(img: &Path) -> io::Result<Option<String>> {
    let mut file = File::open(img)?;
    let (start, end) = partition(&mut file, 0)?;
    Ok(fat_arch(&mut file, start, end - start + 1))
}

/// Replaces the ESP contents of an existing image, keeping the home volume.
/// Returns false if `img` doesn't have the expected layout, and refuses
/// (`InvalidInput`, see [`ArchMismatch`]) an ESP for another architecture.
pub fn refresh_esp(esp: &Path, img: &Path) -> io::Result<bool> {
    let mut file = OpenOptions::new().read(true).write(true).open(img)?;
    let mut hdr = [0u8; 512];
    file.seek(SeekFrom::Start(SECTOR))?;
    file.read_exact(&mut hdr)?;
    if &hdr[0..8] != b"EFI PART" {
        return Ok(false);
    }
    let mut entry = [0u8; 128];
    file.seek(SeekFrom::Start(2 * SECTOR))?;
    file.read_exact(&mut entry)?;
    let start = u64::from_le_bytes(entry[32..40].try_into().unwrap());
    let end = u64::from_le_bytes(entry[40..48].try_into().unwrap());
    if entry[0..16] != ESP_TYPE || start != ESP_START {
        return Ok(false);
    }
    // Never an x86 disk refreshed with ARM binaries, or the reverse: its
    // files would stay, but it would no longer start.
    if let (Some(disk), Some(new)) = (fat_arch(&mut file, start, end - start + 1), esp_dir_arch(esp)) {
        if disk != new {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, ArchMismatch { disk, new }.to_string()));
        }
    }
    // Options written inside WaveOS (Settings → Display) survive the refresh;
    // a system update it was running is set aside (only the four slot
    // lines go), so the new build starts. The home volume, and the slots on
    // it, are never touched: only the EFI partition's sectors are written.
    let boot_conf = read_boot_conf(&mut file, start, end - start + 1).map(|c| {
        String::from_utf8_lossy(&c)
            .lines()
            .filter(|l| {
                let key = l.split_once('=').map(|(k, _)| k.trim());
                !matches!(key, Some("system" | "system_tries" | "system_previous" | "system_failed"))
            })
            .map(|l| format!("{l}\n"))
            .collect::<String>()
            .into_bytes()
    });
    format_esp(&mut file, start, end - start + 1, esp)?;
    if let Some(conf) = boot_conf {
        let part = Region { file: &mut file, start: start * SECTOR, len: (end - start + 1) * SECTOR, pos: 0 };
        let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
        fs.root_dir().create_file("aurora/boot.conf")?.write_all(&conf)?;
        fs.unmount()?;
    }
    Ok(true)
}

/// Sets `key=value` in `\aurora\boot.conf` on a fresh image's ESP.
pub fn set_boot_option(img: &Path, key: &str, value: &str) -> io::Result<()> {
    let mut file = OpenOptions::new().read(true).write(true).open(img)?;
    let old = read_boot_conf(&mut file, ESP_START, ESP_SECTORS).unwrap_or_default();
    let mut text: String = String::from_utf8_lossy(&old)
        .lines()
        .filter(|l| l.split_once('=').is_none_or(|(k, _)| k.trim() != key))
        .map(|l| format!("{l}\n"))
        .collect();
    text.push_str(&format!("{key}={value}\n"));
    let part = Region { file: &mut file, start: ESP_START * SECTOR, len: ESP_SECTORS * SECTOR, pos: 0 };
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
    let mut f = fs.root_dir().create_file("aurora/boot.conf")?;
    f.truncate()?;
    f.write_all(text.as_bytes())?;
    drop(f);
    fs.unmount()?;
    Ok(())
}

fn read_boot_conf(file: &mut File, start: u64, sectors: u64) -> Option<Vec<u8>> {
    let part = Region { file, start: start * SECTOR, len: sectors * SECTOR, pos: 0 };
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new()).ok()?;
    let mut data = Vec::new();
    fs.root_dir().open_file("aurora/boot.conf").ok()?.read_to_end(&mut data).ok()?;
    Some(data)
}

/// A 64 MiB "USB stick" for the tests: an MBR with one FAT32 partition
/// labelled AURORA-USB, holding `Hello.txt`.
pub fn create_usb_stick(out: &Path) -> io::Result<()> {
    create_fat_medium(out, 64 * 1024 * 1024, 64 * 1024 * 1024, *b"AURORA-USB ", b"Hello from a USB stick!\n")
}

/// The tests' second SD card: 4 GiB (so high capacity, block-addressed),
/// a 64 MiB FAT32 partition labelled AURORA-SD with `Hello.txt`, and the
/// rest unpartitioned (the data-integrity tests' scratch space).
pub fn create_sd_card(out: &Path) -> io::Result<()> {
    create_fat_medium(out, 4 << 30, 65 * 1024 * 1024, *b"AURORA-SD  ", b"Hello from an SD card!\n")
}

/// A sparse medium of `size` bytes with an MBR and one FAT32 partition
/// from 1 MiB to `end`, holding `Hello.txt`.
fn create_fat_medium(out: &Path, size: u64, end: u64, label: [u8; 11], hello: &[u8]) -> io::Result<()> {
    const START: u64 = 2048;
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out)?;
    file.set_len(size)?;
    let sectors = end / SECTOR - START;
    let mut mbr = [0u8; 512];
    let e = &mut mbr[446..462];
    e[4] = 0x0C; // FAT32 (LBA)
    e[8..12].copy_from_slice(&(START as u32).to_le_bytes());
    e[12..16].copy_from_slice(&(sectors as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    file.write_all(&mbr)?;
    let mut part = Region { file: &mut file, start: START * SECTOR, len: sectors * SECTOR, pos: 0 };
    fatfs::format_volume(
        &mut part,
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(label),
    )?;
    part.pos = 0;
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
    fs.root_dir().create_file("Hello.txt")?.write_all(hello)?;
    fs.unmount()?;
    Ok(())
}

/// Whether QEMU takes an image of `bytes` as an SD card or eMMC user area:
/// up to 2 GiB a power of two, above it a multiple of 512 KiB.
pub fn sd_size_ok(bytes: u64) -> bool {
    if bytes <= 2 << 30 {
        bytes.is_power_of_two()
    } else {
        bytes.is_multiple_of(512 * 1024)
    }
}

/// Copies the EFI System Partition of the GPT disk image that starts
/// `offset` bytes into `img` to `out`, as a whole-disk FAT volume. UEFI
/// firmware boots from it like from any disk; QEMU's has no SD or eMMC
/// driver, so this is how it starts a system that lives on a card.
pub fn esp_stub(img: &Path, offset: u64, out: &Path) -> io::Result<()> {
    let mut file = File::open(img)?;
    let mut entry = [0u8; ENTRY_SIZE as usize];
    file.seek(SeekFrom::Start(offset + 2 * SECTOR))?;
    file.read_exact(&mut entry)?;
    if entry[0..16] != ESP_TYPE {
        return Err(io::Error::other("the first partition is not an EFI System Partition"));
    }
    let start = u64::from_le_bytes(entry[32..40].try_into().unwrap());
    let end = u64::from_le_bytes(entry[40..48].try_into().unwrap());
    file.seek(SeekFrom::Start(offset + start * SECTOR))?;
    let mut stub = OpenOptions::new().write(true).create(true).truncate(true).open(out)?;
    io::copy(&mut (&mut file).take((end - start + 1) * SECTOR), &mut stub)?;
    Ok(())
}

/// eMMC images for QEMU: two boot partitions and an RPMB partition come
/// before the user area. Each boot partition is filled with its own byte
/// (0xB1, 0xB2) and RPMB with 0xEE, so the tests can tell they were never written.
pub const EMMC_BOOT_BYTES: u64 = 1024 * 1024;
pub const EMMC_RPMB_BYTES: u64 = 128 * 1024;

/// `img` (the user area) behind eMMC boot and RPMB partitions, in `out`.
pub fn create_emmc(img: &Path, out: &Path) -> io::Result<()> {
    let mut f = OpenOptions::new().write(true).create(true).truncate(true).open(out)?;
    f.write_all(&vec![0xB1; EMMC_BOOT_BYTES as usize])?;
    f.write_all(&vec![0xB2; EMMC_BOOT_BYTES as usize])?;
    f.write_all(&vec![0xEE; EMMC_RPMB_BYTES as usize])?;
    io::copy(&mut File::open(img)?, &mut f)?;
    Ok(())
}

/// Whether an eMMC image's boot and RPMB partitions are as [`create_emmc`] left them.
pub fn emmc_hidden_intact(out: &Path) -> io::Result<bool> {
    let mut f = File::open(out)?;
    let mut buf = vec![0u8; (2 * EMMC_BOOT_BYTES + EMMC_RPMB_BYTES) as usize];
    f.read_exact(&mut buf)?;
    let (b1, rest) = buf.split_at(EMMC_BOOT_BYTES as usize);
    let (b2, rpmb) = rest.split_at(EMMC_BOOT_BYTES as usize);
    Ok(b1.iter().all(|&b| b == 0xB1) && b2.iter().all(|&b| b == 0xB2) && rpmb.iter().all(|&b| b == 0xEE))
}

fn format_esp(file: &mut File, start: u64, sectors: u64, esp: &Path) -> io::Result<()> {
    format_esp_with_type(file, start, sectors, esp, fatfs::FatType::Fat32)
}

fn format_esp_with_type(
    file: &mut File,
    start: u64,
    sectors: u64,
    esp: &Path,
    fat_type: fatfs::FatType,
) -> io::Result<()> {
    // The system image goes on as it is; when the partition is too small
    // for it (64 MiB holds about 62 MiB of system), compressed instead
    // (`system.tar.zlib`, which the bootloader inflates).
    let attempt = |file: &mut File, pack: bool| -> io::Result<()> {
        let mut part = Region { file, start: start * SECTOR, len: sectors * SECTOR, pos: 0 };
        fatfs::format_volume(
            &mut part,
            fatfs::FormatVolumeOptions::new().fat_type(fat_type).volume_label(*b"AURORA-BOOT"),
        )?;
        part.pos = 0;
        let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
        copy_to_fat(esp, &fs.root_dir(), pack)?;
        fs.unmount()?;
        Ok(())
    };
    match attempt(file, false) {
        Err(e) if e.kind() == io::ErrorKind::StorageFull || e.to_string().contains("No space") => {
            println!("note: the system image doesn't fit the EFI partition as it is; writing it compressed");
            attempt(file, true)
        }
        other => other,
    }
}

fn copy_to_fat<T: fatfs::ReadWriteSeek>(src: &Path, dst: &fatfs::Dir<T>, pack: bool) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            let sub = dst.create_dir(&name)?;
            copy_to_fat(&entry.path(), &sub, pack)?;
        } else if pack && name == "system.tar" {
            let packed = miniz_oxide::deflate::compress_to_vec_zlib(&fs::read(entry.path())?, 6);
            let mut f = dst.create_file("system.tar.zlib")?;
            f.truncate()?;
            f.write_all(&packed)?;
        } else {
            let mut f = dst.create_file(&name)?;
            f.truncate()?;
            f.write_all(&fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------ AuroraFS

/// A partition of the image file seen as 4 KiB AuroraFS blocks.
struct WaveDisk<'a> {
    region: Region<'a>,
    blocks: u64,
}

impl aurorafs::Disk for WaveDisk<'_> {
    fn blocks(&self) -> u64 {
        self.blocks
    }
    fn read(&mut self, block: u64, buf: &mut aurorafs::Block) -> aurorafs::Result<()> {
        self.region.seek(SeekFrom::Start(block * 4096)).map_err(|_| aurorafs::Error::Io)?;
        self.region.read_exact(buf).map_err(|_| aurorafs::Error::Io)
    }
    fn write(&mut self, block: u64, buf: &aurorafs::Block) -> aurorafs::Result<()> {
        self.region.seek(SeekFrom::Start(block * 4096)).map_err(|_| aurorafs::Error::Io)?;
        self.region.write_all(buf).map_err(|_| aurorafs::Error::Io)
    }
    fn flush(&mut self) -> aurorafs::Result<()> {
        self.region.flush().map_err(|_| aurorafs::Error::Io)
    }
}

/// Local time in seconds since 2000-01-01 (the WaveOS epoch). QEMU runs the
/// guest RTC on local time, so file times use the host's UTC offset too.
fn now() -> u64 {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let offset = std::process::Command::new("date")
        .arg("+%z")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|z| {
            let z = z.trim();
            let sign = if z.starts_with('-') { -1 } else { 1 };
            let digits = z.trim_start_matches(['+', '-']);
            let h: i64 = digits.get(0..2)?.parse().ok()?;
            let m: i64 = digits.get(2..4)?.parse().ok()?;
            Some(sign * (h * 3600 + m * 60))
        })
        .unwrap_or(0);
    (unix as i64 + offset).max(946_684_800) as u64 - 946_684_800
}

fn mkfs_home(file: &mut File, start: u64, sectors: u64, home: &Path, linux: Option<&Path>) -> io::Result<()> {
    let bytes = sectors * SECTOR;
    let region = Region { file, start: start * SECTOR, len: bytes, pos: 0 };
    let disk = WaveDisk { region, blocks: bytes / 4096 };
    let err = |e: aurorafs::Error| io::Error::other(format!("AuroraFS: {e:?}"));
    let mut vol = aurorafs::Volume::format(disk, "Aurora HD", pseudo_guid(3), now).map_err(err)?;
    seed(&mut vol, home, "").map_err(err)?;
    if let Some(dir) = linux {
        vol.mkdir_all("/Linux").map_err(err)?;
        seed(&mut vol, dir, "/Linux").map_err(err)?;
    }
    vol.commit().map_err(err)?;
    vol.fsck().map_err(io::Error::other)?;
    Ok(())
}

fn seed<D: aurorafs::Disk>(vol: &mut aurorafs::Volume<D>, src: &Path, prefix: &str) -> aurorafs::Result<()> {
    let Ok(entries) = fs::read_dir(src) else { return Ok(()) };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        // Hidden files stay out, except the Linux tree's table of links.
        if name.starts_with('.') && name != ".links" {
            continue;
        }
        let path = format!("{prefix}/{name}");
        if entry.path().is_dir() {
            vol.mkdir_all(&path)?;
            seed(vol, &entry.path(), &path)?;
        } else {
            let ino = vol.write_file(&path, &fs::read(entry.path()).map_err(|_| aurorafs::Error::Io)?)?;
            keep_exec_bit(vol, ino, &entry.path())?;
        }
    }
    Ok(())
}

// --------------------------------------------------------------- GPT

fn write_gpt(file: &mut File, l: &Layout) -> io::Result<()> {
    let total = l.total;
    let mut mbr = [0u8; 512];
    let e = &mut mbr[446..462];
    e[4] = 0xEE;
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&((total - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    write_at(file, 0, &mbr)?;

    let mut entries = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    let parts: [(&[u8; 16], u64, u64, &str); 2] = [
        (&ESP_TYPE, ESP_START, ESP_START + ESP_SECTORS - 1, "EFI System"),
        (&aurorafs::PARTITION_TYPE, l.home_start, l.home_end, "Aurora HD"),
    ];
    for (i, (ty, first, last, name)) in parts.iter().enumerate() {
        let e = &mut entries[i * ENTRY_SIZE as usize..(i + 1) * ENTRY_SIZE as usize];
        e[0..16].copy_from_slice(*ty);
        e[16..32].copy_from_slice(&pseudo_guid(10 + i as u64));
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        for (j, c) in name.encode_utf16().enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&c.to_le_bytes());
        }
    }
    let entries_crc = crc32(&entries);
    let disk_guid = pseudo_guid(0);
    let last_usable = total - 1 - ENTRY_SECTORS - 1;

    let header = |my_lba: u64, alt_lba: u64, entries_lba: u64| {
        let mut h = [0u8; 512];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my_lba.to_le_bytes());
        h[32..40].copy_from_slice(&alt_lba.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + ENTRY_SECTORS).to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&disk_guid);
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
        let crc = crc32(&h[0..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };

    let backup_entries_lba = total - 1 - ENTRY_SECTORS;
    write_at(file, SECTOR, &header(1, total - 1, 2))?;
    write_at(file, 2 * SECTOR, &entries)?;
    write_at(file, backup_entries_lba * SECTOR, &entries)?;
    write_at(file, (total - 1) * SECTOR, &header(total - 1, 1, backup_entries_lba))?;
    Ok(())
}

fn write_at(file: &mut File, offset: u64, data: &[u8]) -> io::Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)
}

/// A random-enough version-4 GUID derived from the clock (no RNG dependency).
fn pseudo_guid(salt: u64) -> [u8; 16] {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos() as u64;
    let mut x = t ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut g = [0u8; 16];
    for b in g.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    g
}

fn crc32(data: &[u8]) -> u32 {
    aurorafs::crc32(data)
}

/// A window onto a byte range of the image file.
struct Region<'a> {
    file: &'a mut File,
    start: u64,
    len: u64,
    pos: u64,
}

impl Read for Region<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.file.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for Region<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        if n == 0 && !buf.is_empty() {
            return Err(io::Error::new(io::ErrorKind::WriteZero, "partition full"));
        }
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.file.write(&buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Seek for Region<'_> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let new = match pos {
            SeekFrom::Start(p) => p as i64,
            SeekFrom::Current(d) => self.pos as i64 + d,
            SeekFrom::End(d) => self.len as i64 + d,
        };
        if new < 0 || new as u64 > self.len {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek out of partition"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_volume(file: &mut File) -> aurorafs::Volume<WaveDisk<'_>> {
        let mut entry = [0u8; ENTRY_SIZE as usize];
        file.seek(SeekFrom::Start(2 * SECTOR + ENTRY_SIZE)).unwrap();
        file.read_exact(&mut entry).unwrap();
        let start = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        let end = u64::from_le_bytes(entry[40..48].try_into().unwrap());
        let bytes = (end - start + 1) * SECTOR;
        let region = Region { file, start: start * SECTOR, len: bytes, pos: 0 };
        aurorafs::Volume::mount(WaveDisk { region, blocks: bytes / 4096 }, now).unwrap()
    }

    fn esp_file(file: &mut File, path: &str) -> Option<Vec<u8>> {
        let part = Region { file, start: ESP_START * SECTOR, len: ESP_SECTORS * SECTOR, pos: 0 };
        let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new()).ok()?;
        let mut data = Vec::new();
        fs.root_dir().open_file(path).ok()?.read_to_end(&mut data).ok()?;
        Some(data)
    }

    /// What `cargo xtask run` does to an existing disk: the EFI partition
    /// gets the new build and boot.conf loses only its system-update lines;
    /// files on the home volume, installed system update slots among them,
    /// are byte for byte as they were.
    #[test]
    fn refresh_keeps_the_home_volume_and_drops_only_slot_lines() {
        let dir = std::env::temp_dir().join(format!("wv-refresh-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let (old, new, home) = (dir.join("esp-old"), dir.join("esp-new"), dir.join("home"));
        for (esp, kernel) in [(&old, b"old kernel"), (&new, b"new kernel")] {
            fs::create_dir_all(esp.join("aurora")).unwrap();
            fs::create_dir_all(esp.join("EFI/BOOT")).unwrap();
            fs::write(esp.join("aurora/kernel.elf"), kernel).unwrap();
            fs::write(esp.join("aurora/system.tar"), vec![7u8; 4096]).unwrap();
            fs::write(esp.join("EFI/BOOT/BOOTX64.EFI"), b"MZ loader").unwrap();
        }
        fs::create_dir_all(home.join("Documents")).unwrap();
        fs::write(home.join("Documents/letter.txt"), b"keep me").unwrap();
        let img = dir.join("disk.img");
        create_with(&old, &home, None, &img, 160 * 1024 * 1024).unwrap();
        {
            let mut file = OpenOptions::new().read(true).write(true).open(&img).unwrap();
            let mut vol = home_volume(&mut file);
            vol.mkdir_all("/.system/a").unwrap();
            vol.write_file("/.system/a/update.txt", b"gina-system-update 1").unwrap();
            vol.commit().unwrap();
        }
        for (k, v) in [
            ("resolution", "1920x1080"),
            ("system", "a"),
            ("system_tries", "2"),
            ("system_previous", "esp"),
            ("system_failed", "b"),
        ] {
            set_boot_option(&img, k, v).unwrap();
        }
        assert!(refresh_esp(&new, &img).unwrap());
        let mut file = OpenOptions::new().read(true).write(true).open(&img).unwrap();
        assert_eq!(esp_file(&mut file, "aurora/kernel.elf").unwrap(), b"new kernel");
        let conf = String::from_utf8(esp_file(&mut file, "aurora/boot.conf").unwrap()).unwrap();
        assert!(conf.contains("resolution=1920x1080"), "{conf}");
        assert!(!conf.lines().any(|l| l.starts_with("system")), "{conf}");
        let mut vol = home_volume(&mut file);
        assert_eq!(read_file(&mut vol, "/Documents/letter.txt"), b"keep me");
        assert_eq!(read_file(&mut vol, "/.system/a/update.txt"), b"gina-system-update 1");
        assert!(vol.fsck().is_ok());
        drop(vol);
        fs::remove_dir_all(&dir).unwrap();
    }

    /// An x86 disk is never refreshed with an aarch64 build, nor the
    /// reverse; a disk from before `aurora/arch` is known by its boot file.
    #[test]
    fn refresh_refuses_the_other_architecture() {
        let dir = std::env::temp_dir().join(format!("wv-arch-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let esp = |name: &str, boot: &str, arch: Option<&str>| {
            let esp = dir.join(name);
            fs::create_dir_all(esp.join("aurora")).unwrap();
            fs::create_dir_all(esp.join("EFI/BOOT")).unwrap();
            fs::write(esp.join("aurora/kernel.elf"), name).unwrap();
            fs::write(esp.join("EFI/BOOT").join(boot), b"MZ loader").unwrap();
            if let Some(a) = arch {
                fs::write(esp.join(ARCH_FILE), format!("{a}\n")).unwrap();
            }
            esp
        };
        // An x86 disk made before the architecture was recorded.
        let old_x86 = esp("old-x86", "BOOTX64.EFI", None);
        let x86 = esp("x86", "BOOTX64.EFI", Some("x86_64"));
        let arm = esp("arm", "BOOTAA64.EFI", Some("aarch64"));
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let img = dir.join("disk.img");
        create_with(&old_x86, &home, None, &img, 160 * 1024 * 1024).unwrap();
        assert_eq!(disk_arch(&img).unwrap().as_deref(), Some("x86_64"));
        let before = fs::read(&img).unwrap();
        let e = refresh_esp(&arm, &img).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput);
        assert!(e.to_string().contains("--arch x86_64"), "{e}");
        assert!(fs::read(&img).unwrap() == before, "the refused disk changed");
        assert!(refresh_esp(&x86, &img).unwrap());
        assert_eq!(disk_arch(&img).unwrap().as_deref(), Some("x86_64"));
        // And an aarch64 disk refuses x86.
        create_with(&arm, &home, None, &img, 160 * 1024 * 1024).unwrap();
        assert_eq!(disk_arch(&img).unwrap().as_deref(), Some("aarch64"));
        assert!(refresh_esp(&x86, &img).is_err());
        assert!(refresh_esp(&arm, &img).unwrap());
        fs::remove_dir_all(&dir).unwrap();
    }
}
