//! Builds a bootable raw disk image: a GPT with a single FAT32 EFI System
//! Partition containing the contents of the ESP directory. The result can be
//! written straight to a USB stick for booting on real UEFI hardware.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const SECTOR: u64 = 512;
const ENTRIES: u64 = 128;
const ENTRY_SIZE: u64 = 128;
const ENTRY_SECTORS: u64 = ENTRIES * ENTRY_SIZE / SECTOR; // 32
const PART_START: u64 = 2048; // 1 MiB alignment

/// EFI System Partition type GUID C12A7328-F81F-11D2-BA4B-00A0C93EC93B, on-disk byte order.
const ESP_TYPE: [u8; 16] =
    [0x28, 0x73, 0x2A, 0xC1, 0x1F, 0xF8, 0xD2, 0x11, 0xBA, 0x4B, 0x00, 0xA0, 0xC9, 0x3E, 0xC9, 0x3B];

pub fn create(esp: &Path, out: &Path, size: u64) -> io::Result<()> {
    let total = size / SECTOR;
    let part_end = total - 1 - ENTRY_SECTORS - 1; // last usable LBA
    let mut file = OpenOptions::new().read(true).write(true).create(true).truncate(true).open(out)?;
    file.set_len(size)?;

    write_gpt(&mut file, total, part_end)?;

    let part =
        Partition { file: &mut file, start: PART_START * SECTOR, len: (part_end - PART_START + 1) * SECTOR, pos: 0 };
    let mut part = part;
    fatfs::format_volume(
        &mut part,
        fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32).volume_label(*b"AURORA     "),
    )?;
    part.pos = 0;
    let fs = fatfs::FileSystem::new(part, fatfs::FsOptions::new())?;
    copy_dir(esp, &fs.root_dir())?;
    fs.unmount()?;
    Ok(())
}

fn copy_dir<T: fatfs::ReadWriteSeek>(src: &Path, dst: &fatfs::Dir<T>) -> io::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if entry.file_type()?.is_dir() {
            let sub = dst.create_dir(&name)?;
            copy_dir(&entry.path(), &sub)?;
        } else {
            let mut f = dst.create_file(&name)?;
            f.truncate()?;
            f.write_all(&fs::read(entry.path())?)?;
        }
    }
    Ok(())
}

fn write_gpt(file: &mut File, total: u64, part_end: u64) -> io::Result<()> {
    // Protective MBR.
    let mut mbr = [0u8; 512];
    let e = &mut mbr[446..462];
    e[4] = 0xEE;
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&((total - 1).min(u32::MAX as u64) as u32).to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xAA;
    write_at(file, 0, &mbr)?;

    // Partition entry array (one ESP).
    let mut entries = vec![0u8; (ENTRIES * ENTRY_SIZE) as usize];
    entries[0..16].copy_from_slice(&ESP_TYPE);
    entries[16..32].copy_from_slice(&pseudo_guid(1));
    entries[32..40].copy_from_slice(&PART_START.to_le_bytes());
    entries[40..48].copy_from_slice(&part_end.to_le_bytes());
    for (i, c) in "EFI System".encode_utf16().enumerate() {
        entries[56 + i * 2..58 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    let entries_crc = crc32(&entries);
    let disk_guid = pseudo_guid(0);

    let header = |my_lba: u64, alt_lba: u64, entries_lba: u64| {
        let mut h = [0u8; 512];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&my_lba.to_le_bytes());
        h[32..40].copy_from_slice(&alt_lba.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + ENTRY_SECTORS).to_le_bytes());
        h[48..56].copy_from_slice(&part_end.to_le_bytes());
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
    let mut x = t ^ salt.wrapping_mul(0x9E37_79B9_7F4A_7C15);
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
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// A window onto the partition's byte range within the image file.
struct Partition<'a> {
    file: &'a mut File,
    start: u64,
    len: u64,
    pos: u64,
}

impl Read for Partition<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = (buf.len() as u64).min(self.len - self.pos) as usize;
        self.file.seek(SeekFrom::Start(self.start + self.pos))?;
        let n = self.file.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Write for Partition<'_> {
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

impl Seek for Partition<'_> {
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
