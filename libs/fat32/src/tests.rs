//! Cross-checks against the independent `fatfs` crate: images it creates are
//! read correctly, and everything this driver writes reads back through fatfs.

use super::*;
use std::io::{Cursor, Read, Write};
use std::vec::Vec as StdVec;

struct MemDisk(StdVec<u8>);

impl SectorDisk for MemDisk {
    fn sectors(&self) -> u64 {
        (self.0.len() / SECTOR) as u64
    }
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<()> {
        let o = lba as usize * SECTOR;
        buf.copy_from_slice(self.0.get(o..o + SECTOR).ok_or(Error::Io)?);
        Ok(())
    }
    fn write(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<()> {
        let o = lba as usize * SECTOR;
        self.0.get_mut(o..o + SECTOR).ok_or(Error::Io)?.copy_from_slice(buf);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fn now() -> u64 {
    // 2026-09-29 12:00:00 in seconds since 2000
    842_400_000
}

fn fresh_image() -> StdVec<u8> {
    let mut img = Cursor::new(vec![0u8; 40 * 1024 * 1024]);
    fatfs::format_volume(&mut img, fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat32)).unwrap();
    img.into_inner()
}

#[test]
fn reads_what_fatfs_wrote() {
    let mut img = Cursor::new(fresh_image());
    {
        let fs = fatfs::FileSystem::new(&mut img, fatfs::FsOptions::new()).unwrap();
        let root = fs.root_dir();
        root.create_dir("EFI").unwrap();
        let boot = root.create_dir("EFI/BOOT").unwrap();
        boot.create_file("BOOTX64.EFI").unwrap().write_all(&[0xAB; 70_000]).unwrap();
        root.create_file("A long file name with spaces.txt").unwrap().write_all(b"hello from fatfs").unwrap();
    }
    let mut fs = Fat32::mount(MemDisk(img.into_inner()), now).unwrap();
    let names: StdVec<String> = fs.readdir(ROOT).unwrap().into_iter().map(|e| e.0).collect();
    assert!(names.contains(&String::from("EFI")));
    assert!(names.contains(&String::from("A long file name with spaces.txt")));
    let efi = fs.lookup(ROOT, "efi").unwrap(); // case-insensitive
    let boot = fs.lookup(efi, "BOOT").unwrap();
    let f = fs.lookup(boot, "BOOTX64.EFI").unwrap();
    assert_eq!(fs.stat(f).unwrap().size, 70_000);
    let mut buf = vec![0u8; 70_000];
    assert_eq!(fs.read(f, 0, &mut buf).unwrap(), 70_000);
    assert!(buf.iter().all(|&b| b == 0xAB));
    let t = fs.lookup(ROOT, "A long file name with spaces.txt").unwrap();
    let mut buf = [0u8; 16];
    fs.read(t, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"hello from fatfs");
}

#[test]
fn fatfs_reads_what_we_wrote() {
    let mut fs = Fat32::mount(MemDisk(fresh_image()), now).unwrap();
    let (_, free_before) = fs.space();
    let dir = fs.create(ROOT, "Aurora Notes", true).unwrap();
    let f = fs.create(dir, "Shopping list for the weekend.txt", false).unwrap();
    fs.write(f, 0, b"milk, eggs, flour").unwrap();
    fs.write(f, 17, &[b'!'; 5000]).unwrap(); // spans clusters
    let short = fs.create(ROOT, "README.TXT", false).unwrap();
    fs.write(short, 0, b"short name").unwrap();
    let doomed = fs.create(ROOT, "delete me.bin", false).unwrap();
    fs.write(doomed, 0, &[1u8; 20_000]).unwrap();
    fs.unlink(ROOT, "delete me.bin").unwrap();
    fs.rename(ROOT, "README.TXT", dir, "Read me.txt").unwrap();
    // Many entries force the directory to grow past one cluster.
    for i in 0..40 {
        let g = fs.create(dir, &std::format!("generated file number {i}.dat"), false).unwrap();
        fs.write(g, 0, &[i as u8; 10]).unwrap();
    }
    fs.truncate(f, 17).unwrap();
    let img = fs.into_disk().0;

    let mut cur = Cursor::new(img);
    {
        let ffs = fatfs::FileSystem::new(&mut cur, fatfs::FsOptions::new()).unwrap();
        let root = ffs.root_dir();
        let mut s = std::string::String::new();
        root.open_file("Aurora Notes/Shopping list for the weekend.txt").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "milk, eggs, flour");
        let mut s = std::string::String::new();
        root.open_file("Aurora Notes/Read me.txt").unwrap().read_to_string(&mut s).unwrap();
        assert_eq!(s, "short name");
        assert!(root.open_file("README.TXT").is_err());
        assert!(root.open_file("delete me.bin").is_err());
        let mut buf = StdVec::new();
        root.open_file("Aurora Notes/generated file number 39.dat").unwrap().read_to_end(&mut buf).unwrap();
        assert_eq!(buf, [39u8; 10]);
        let count = root.open_dir("Aurora Notes").unwrap().iter().filter(|e| e.is_ok()).count();
        assert_eq!(count, 2 + 2 + 40); // . .. + 2 files + 40 generated
    }
    let fs = Fat32::mount(MemDisk(cur.into_inner()), now).unwrap();
    let (_, free_after) = fs.space();
    assert!(free_after < free_before);
}

#[test]
fn rejects_non_fat32() {
    let mut img = Cursor::new(vec![0u8; 4 * 1024 * 1024]);
    fatfs::format_volume(&mut img, fatfs::FormatVolumeOptions::new().fat_type(fatfs::FatType::Fat16)).unwrap();
    assert!(matches!(Fat32::mount(MemDisk(img.into_inner()), now), Err(Error::Unsupported)));
}

#[test]
fn time_round_trip() {
    let (d, t) = to_fat_time(now());
    assert_eq!(from_fat_time(d, t), now());
}

fn walk(fs: &mut Fat32<MemDisk>, dir: u64, depth: usize) {
    if depth > 8 {
        return;
    }
    for (name, ino, is_dir) in fs.readdir(dir).unwrap_or_default().into_iter().take(64) {
        let _ = fs.stat(ino);
        let _ = fs.lookup(dir, &name);
        if is_dir {
            walk(fs, ino, depth + 1);
        } else {
            let _ = fs.read(ino, 0, &mut [0; 8192]);
        }
    }
}

/// USB sticks are FAT32 and the kernel mounts them: a corrupt one is an
/// error, never a panic.
#[test]
fn corrupt_volumes_never_panic() {
    let mut img = Cursor::new(fresh_image());
    {
        let fs = fatfs::FileSystem::new(&mut img, fatfs::FsOptions::new()).unwrap();
        let root = fs.root_dir();
        let docs = root.create_dir("Documents").unwrap();
        docs.create_file("A long file name.txt").unwrap().write_all(&[7; 9000]).unwrap();
        docs.create_dir("Sub dir").unwrap().create_file("x").unwrap().write_all(b"x").unwrap();
        root.create_file("README.TXT").unwrap().write_all(b"hello").unwrap();
    }
    let image = img.into_inner();
    let used = aurora_mutate::used_blocks(&image, SECTOR);
    aurora_mutate::check_with(
        "fat32",
        &[image],
        60,
        |m, seed| m.mutate_within(seed, &used),
        |bytes| {
            let Ok(mut fs) = Fat32::mount(MemDisk(bytes.to_vec()), now) else { return };
            let _ = fs.space();
            walk(&mut fs, ROOT, 0);
            if let Ok(f) = fs.create(ROOT, "new file.txt", false) {
                let _ = fs.write(f, 0, &[1; 5000]);
            }
            if let Ok(docs) = fs.lookup(ROOT, "Documents") {
                let _ = fs.rename(docs, "A long file name.txt", ROOT, "moved.txt");
                let _ = fs.unlink(docs, "Sub dir");
                let _ = fs.truncate(docs, 0);
            }
            let _ = fs.flush();
        },
    );
}

#[test]
fn growing_truncate_is_bounded_and_zero_filled() {
    let mut fs = Fat32::mount(MemDisk(fresh_image()), now).unwrap();
    let f = fs.create(ROOT, "F.BIN", false).unwrap();
    fs.write(f, 0, b"abc").unwrap();
    // Bigger than the volume: refused before anything is written or allocated.
    assert_eq!(fs.truncate(f, u32::MAX as u64), Err(Error::NoSpace));
    assert_eq!(fs.truncate(f, 1 << 40), Err(Error::NoSpace));
    assert_eq!(fs.stat(f).unwrap().size, 3);
    fs.truncate(f, 150_000).unwrap();
    let mut buf = vec![9u8; 150_000];
    assert_eq!(fs.read(f, 0, &mut buf).unwrap(), 150_000);
    assert_eq!(&buf[..3], b"abc");
    assert!(buf[3..].iter().all(|&b| b == 0));
}
