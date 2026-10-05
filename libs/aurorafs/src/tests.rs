//! Host-side tests: format, file operations, consistency, crash recovery.

use super::*;
use std::vec::Vec as StdVec;

#[derive(Clone)]
struct MemDisk {
    blocks: StdVec<Block>,
    /// Blocks written since the last flush (lost on a simulated power cut).
    unflushed: StdVec<(u64, Block)>,
    durable: StdVec<Block>,
}

impl MemDisk {
    fn new(n: usize) -> Self {
        MemDisk { blocks: vec![[0; BLOCK]; n], unflushed: StdVec::new(), durable: vec![[0; BLOCK]; n] }
    }
    /// The disk as it would look after a power cut: only flushed writes survive.
    fn after_power_cut(&self) -> MemDisk {
        MemDisk { blocks: self.durable.clone(), unflushed: StdVec::new(), durable: self.durable.clone() }
    }
}

impl Disk for MemDisk {
    fn blocks(&self) -> u64 {
        self.blocks.len() as u64
    }
    fn read(&mut self, b: u64, buf: &mut Block) -> Result<()> {
        buf.copy_from_slice(self.blocks.get(b as usize).ok_or(Error::Io)?);
        Ok(())
    }
    fn write(&mut self, b: u64, buf: &Block) -> Result<()> {
        *self.blocks.get_mut(b as usize).ok_or(Error::Io)? = *buf;
        self.unflushed.push((b, *buf));
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        for (b, data) in self.unflushed.drain(..) {
            self.durable[b as usize] = data;
        }
        Ok(())
    }
}

fn now() -> u64 {
    1_000
}

fn fresh(blocks: usize) -> Volume<MemDisk> {
    Volume::format(MemDisk::new(blocks), "Test", [7; 16], now).unwrap()
}

#[test]
fn format_and_mount() {
    let mut v = fresh(4096);
    assert_eq!(v.label(), "Test");
    assert!(v.readdir(ROOT).unwrap().is_empty());
    v.fsck().unwrap();
    let disk = v.into_disk();
    let mut v = Volume::mount(disk, now).unwrap();
    v.fsck().unwrap();
    assert!(v.space().1 > 0);
}

#[test]
fn files_and_directories() {
    let mut v = fresh(4096);
    let docs = v.create(ROOT, "Documents", Kind::Dir).unwrap();
    let f = v.create(docs, "hello.txt", Kind::File).unwrap();
    assert_eq!(v.write(f, 0, b"hello world").unwrap(), 11);
    let mut buf = [0u8; 32];
    assert_eq!(v.read(f, 0, &mut buf).unwrap(), 11);
    assert_eq!(&buf[..11], b"hello world");
    // Sparse write past the end zero-fills the gap.
    v.write(f, 20, b"!").unwrap();
    assert_eq!(v.stat(f).unwrap().size, 21);
    let mut buf = [9u8; 21];
    v.read(f, 0, &mut buf).unwrap();
    assert_eq!(&buf[11..20], &[0; 9]);
    assert_eq!(v.create(docs, "hello.txt", Kind::File), Err(Error::Exists));
    assert_eq!(v.unlink(ROOT, "Documents"), Err(Error::NotEmpty));
    v.rename(docs, "hello.txt", ROOT, "moved.txt").unwrap();
    assert_eq!(v.lookup(ROOT, "moved.txt").unwrap(), f);
    assert_eq!(v.lookup(docs, "hello.txt"), Err(Error::NotFound));
    v.unlink(ROOT, "Documents").unwrap();
    v.fsck().unwrap();
    v.commit().unwrap();
    let mut v = Volume::mount(v.into_disk(), now).unwrap();
    let mut buf = [0u8; 5];
    let moved = v.resolve("/moved.txt").unwrap();
    v.read(moved, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"hello");
    v.fsck().unwrap();
}

#[test]
fn large_and_fragmented_files() {
    let mut v = fresh(8192);
    let data: StdVec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    let big = v.write_file("/big.bin", &data).unwrap();
    // Interleave many small files to fragment free space, then grow the big file.
    for i in 0..200 {
        v.write_file(&std::format!("/small/{i}.txt"), &[i as u8; 5000]).unwrap();
    }
    let small = v.resolve("/small").unwrap();
    for i in (0..200).step_by(2) {
        v.unlink(small, &std::format!("{i}.txt")).unwrap();
    }
    v.commit().unwrap();
    v.write(big, data.len() as u64, &data[..400_000]).unwrap();
    let mut back = vec![0u8; data.len() + 400_000];
    assert_eq!(v.read(big, 0, &mut back).unwrap(), back.len());
    assert_eq!(&back[..data.len()], &data[..]);
    assert_eq!(&back[data.len()..], &data[..400_000]);
    v.truncate(big, 10_000).unwrap();
    assert_eq!(v.stat(big).unwrap().size, 10_000);
    v.fsck().unwrap();
    v.commit().unwrap();
    let mut v = Volume::mount(v.into_disk(), now).unwrap();
    v.fsck().unwrap();
}

#[test]
fn full_disk_reports_no_space() {
    let mut v = fresh(256);
    let f = v.create(ROOT, "fill", Kind::File).unwrap();
    let chunk = [1u8; 64 * 1024];
    let mut off = 0;
    let err = loop {
        match v.write(f, off, &chunk) {
            Ok(n) => off += n as u64,
            Err(e) => break e,
        }
    };
    assert_eq!(err, Error::NoSpace);
    let r = v.unlink(ROOT, "fill");
    assert!(r.is_ok(), "unlink after ENOSPC: {r:?} fsck: {:?}", v.fsck());
    v.commit().unwrap();
    v.fsck().unwrap();
}

#[test]
fn replays_committed_journal_after_crash() {
    let mut v = fresh(4096);
    v.write_file("/Documents/keep.txt", b"committed before the crash").unwrap();
    v.commit().unwrap();
    v.write_file("/Documents/journaled.txt", b"in the journal only").unwrap();
    v.commit_journal_only().unwrap();
    // Power cut: metadata never reached its home location.
    let crashed = v.disk().after_power_cut();
    let mut v = Volume::mount(crashed, now).unwrap();
    v.fsck().unwrap();
    let ino = v.resolve("/Documents/journaled.txt").unwrap();
    let mut buf = [0u8; 19];
    v.read(ino, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"in the journal only");
    assert!(v.resolve("/Documents/keep.txt").is_ok());
}

#[test]
fn uncommitted_changes_vanish_cleanly() {
    let mut v = fresh(4096);
    v.write_file("/a.txt", b"safe").unwrap();
    v.commit().unwrap();
    v.write_file("/b.txt", b"never committed").unwrap();
    v.unlink(ROOT, "a.txt").unwrap();
    let crashed = v.disk().after_power_cut();
    let mut v = Volume::mount(crashed, now).unwrap();
    v.fsck().unwrap();
    assert!(v.resolve("/a.txt").is_ok());
    assert_eq!(v.resolve("/b.txt"), Err(Error::NotFound));
}

#[test]
fn torn_journal_is_ignored() {
    let mut v = fresh(4096);
    v.write_file("/x.txt", b"x").unwrap();
    v.commit().unwrap();
    v.write_file("/y.txt", b"y").unwrap();
    v.commit_journal_only().unwrap();
    // Corrupt one journaled image: the checksum must reject the transaction.
    let mut disk = v.disk().after_power_cut();
    let j = 2;
    let mut b = [0u8; BLOCK];
    disk.read(j, &mut b).unwrap();
    b[100] ^= 0xFF;
    disk.write(j, &b).unwrap();
    disk.flush().unwrap();
    let mut v = Volume::mount(disk, now).unwrap();
    v.fsck().unwrap();
    assert!(v.resolve("/x.txt").is_ok());
    assert_eq!(v.resolve("/y.txt"), Err(Error::NotFound));
}

#[test]
fn freed_blocks_not_reused_before_commit() {
    let mut v = fresh(4096);
    let a = v.write_file("/a.bin", &[0xAA; 8192]).unwrap();
    v.commit().unwrap();
    let before = v.read_inode(a).unwrap().extents.clone();
    v.unlink(ROOT, "a.bin").unwrap();
    let b = v.write_file("/b.bin", &[0xBB; 8192]).unwrap();
    let after = v.read_inode(b).unwrap().extents.clone();
    for e in &after {
        for o in &before {
            assert!(e.start + e.len as u64 <= o.start || o.start + o.len as u64 <= e.start, "block reused in same txn");
        }
    }
    v.commit().unwrap();
    v.fsck().unwrap();
}

#[test]
fn backup_superblock() {
    let mut v = fresh(1024);
    v.write_file("/f", b"data").unwrap();
    v.commit().unwrap();
    let mut disk = v.into_disk();
    disk.write(0, &[0u8; BLOCK]).unwrap();
    // The backup has format-time counts but a valid layout; mount must succeed.
    assert!(Volume::mount(disk, now).is_ok());
}
