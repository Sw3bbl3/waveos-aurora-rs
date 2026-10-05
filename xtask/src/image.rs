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
use std::path::Path;

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
/// home volume seeded from `home` (a directory tree).
pub fn create(esp: &Path, home: &Path, out: &Path, size: u64) -> io::Result<()> {
    let l = layout(size);
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out)?;
    file.set_len(size)?;
    write_gpt(&mut file, &l)?;
    format_esp(&mut file, ESP_START, ESP_SECTORS, esp)?;
    mkfs_home(&mut file, l.home_start, l.home_end - l.home_start + 1, home)?;
    Ok(())
}

/// Replaces the ESP contents of an existing image, keeping the home volume.
/// Returns false if `img` doesn't have the expected layout.
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
    // Options written inside WaveOS (Settings → Display) survive the refresh.
    let boot_conf = read_boot_conf(&mut file, start, end - start + 1);
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
    const SIZE: u64 = 64 * 1024 * 1024;
    const START: u64 = 2048;
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out)?;
    file.set_len(SIZE)?;
    let sectors = SIZE / SECTOR - START;
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
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(*b"AURORA-USB "),
    )?;
    part.pos = 0;
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
    fs.root_dir().create_file("Hello.txt")?.write_all(b"Hello from a USB stick!\n")?;
    fs.unmount()?;
    Ok(())
}

fn format_esp(file: &mut File, start: u64, sectors: u64, esp: &Path) -> io::Result<()> {
    let mut part = Region { file, start: start * SECTOR, len: sectors * SECTOR, pos: 0 };
    fatfs::format_volume(
        &mut part,
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(*b"AURORA-BOOT"),
    )?;
    part.pos = 0;
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
    copy_to_fat(esp, &fs.root_dir())?;
    fs.unmount()?;
    Ok(())
}

fn copy_to_fat<T: fatfs::ReadWriteSeek>(src: &Path, dst: &fatfs::Dir<T>) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            let sub = dst.create_dir(&name)?;
            copy_to_fat(&entry.path(), &sub)?;
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

fn mkfs_home(file: &mut File, start: u64, sectors: u64, home: &Path) -> io::Result<()> {
    let bytes = sectors * SECTOR;
    let region = Region { file, start: start * SECTOR, len: bytes, pos: 0 };
    let disk = WaveDisk { region, blocks: bytes / 4096 };
    let err = |e: aurorafs::Error| io::Error::other(format!("AuroraFS: {e:?}"));
    let mut vol = aurorafs::Volume::format(disk, "Aurora HD", pseudo_guid(3), now).map_err(err)?;
    seed(&mut vol, home, "").map_err(err)?;
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
        if name.starts_with('.') {
            continue;
        }
        let path = format!("{prefix}/{name}");
        if entry.path().is_dir() {
            vol.mkdir_all(&path)?;
            seed(vol, &entry.path(), &path)?;
        } else {
            vol.write_file(&path, &fs::read(entry.path()).map_err(|_| aurorafs::Error::Io)?)?;
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
