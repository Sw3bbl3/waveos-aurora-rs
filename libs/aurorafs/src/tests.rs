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
fn times_can_be_set_and_survive_a_remount() {
    let mut v = fresh(4096);
    let f = v.write_file("/Documents/old.txt", b"from another volume").unwrap();
    v.set_times(f, 42, 4_242).unwrap();
    v.commit().unwrap();
    let mut v = Volume::mount(v.into_disk(), now).unwrap();
    let ino = v.resolve("/Documents/old.txt").unwrap();
    let m = v.stat(ino).unwrap();
    assert_eq!((m.ctime, m.mtime, m.size), (42, 4_242, 19));
    v.fsck().unwrap();
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
fn staged_replacement_keeps_complete_old_or_new_file_after_power_cut() {
    // Same protocol as Files: sync private payload, rename, then sync.
    for phase in 0..3 {
        let mut v = fresh(4096);
        v.write_file("/existing.txt", b"old complete contents").unwrap();
        v.write_file("/.transfer/payload", b"new complete contents").unwrap();
        v.commit().unwrap();
        if phase > 0 {
            let staging = v.resolve("/.transfer").unwrap();
            v.rename(staging, "payload", ROOT, "existing.txt").unwrap();
        }
        if phase == 2 {
            v.commit().unwrap();
        }
        let mut recovered = Volume::mount(v.disk().after_power_cut(), now).unwrap();
        recovered.fsck().unwrap();
        let file = recovered.resolve("/existing.txt").unwrap();
        let mut bytes = [0; 64];
        let n = recovered.read(file, 0, &mut bytes).unwrap();
        let expected: &[u8] = if phase == 2 { b"new complete contents" } else { b"old complete contents" };
        assert_eq!(&bytes[..n], expected, "power cut phase {phase}");
    }
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

/// Rewrites the first extent of `ino`'s inode on disk.
fn corrupt_extent(v: &mut Volume<MemDisk>, ino: u64, start: u64, len: u32) {
    let (blk, off) = v.inode_location(ino).unwrap();
    let mut b = v.disk.blocks[blk as usize];
    put_u64(&mut b, off + 40, start);
    put_u32(&mut b, off + 48, len);
    v.disk.blocks[blk as usize] = b;
}

#[test]
fn extents_outside_the_data_area_are_corrupt() {
    let mut v = fresh(4096);
    let f = v.create(ROOT, "f", Kind::File).unwrap();
    v.write(f, 0, &[1; 5000]).unwrap();
    v.commit().unwrap();
    // A four-billion-block extent used to send fsck (and freeing) through
    // every block number it covered.
    let data_start = v.sb.data_start;
    corrupt_extent(&mut v, f, data_start, u32::MAX);
    assert_eq!(v.stat(f).err(), Some(Error::Corrupt));
    assert!(v.fsck().is_err());
    corrupt_extent(&mut v, f, 0, 1); // the superblock
    assert_eq!(v.read(f, 0, &mut [0; 16]).err(), Some(Error::Corrupt));
    corrupt_extent(&mut v, f, u64::MAX, 2);
    assert_eq!(v.unlink(ROOT, "f").err(), Some(Error::Corrupt));
}

#[test]
fn superblocks_with_impossible_geometry_are_refused() {
    let v = fresh(4096);
    let good = v.sb.clone();
    let disk = v.into_disk();
    let tweaks: [fn(&mut Superblock); 6] = [
        |sb| sb.bbitmap_blocks = 0,               // a bitmap too small for the volume
        |sb| sb.ibitmap_blocks = 0,               // or for the inodes
        |sb| sb.itable_blocks = 1,                // an inode table too small
        |sb| sb.journal_start = sb.bbitmap_start, // overlapping regions
        |sb| sb.data_start = sb.blocks - 1,       // no data area
        |sb| sb.journal_blocks = u64::MAX,        // arithmetic that overflows
    ];
    for (i, tweak) in tweaks.iter().enumerate() {
        let mut sb = good.clone();
        tweak(&mut sb);
        let mut d = disk.clone();
        // A valid checksum, so only the geometry checks stand in the way.
        d.blocks[0] = *sb.encode();
        let last = d.blocks.len() - 1;
        d.blocks[last] = *sb.encode();
        assert!(Volume::mount(d, now).is_err(), "tweak {i} mounted");
    }
    assert!(Volume::mount(disk, now).is_ok());
}

/// Volumes come from USB sticks and other machines, and the kernel mounts
/// them: corrupt metadata is an error, never a panic or a hang.
#[test]
fn corrupt_volumes_never_panic() {
    let mut v = fresh(1024);
    v.mkdir_all("/Documents/Sub").unwrap();
    v.write_file("/Documents/a.txt", &[5; 10_000]).unwrap();
    v.write_file("/Documents/Sub/b.txt", b"hello").unwrap();
    for i in 0..20 {
        v.write_file(&alloc::format!("/Documents/f{i}"), b"x").unwrap();
    }
    v.commit().unwrap();
    let image: StdVec<u8> = v.into_disk().blocks.iter().flatten().copied().collect();
    let used = aurora_mutate::used_blocks(&image, BLOCK);
    aurora_mutate::check_with(
        "aurorafs",
        &[image],
        150,
        |m, seed| m.mutate_within(seed, &used),
        |bytes| {
            let mut disk = MemDisk::new(bytes.len() / BLOCK);
            for (block, data) in disk.blocks.iter_mut().zip(bytes.chunks(BLOCK)) {
                block.copy_from_slice(data);
            }
            let Ok(mut v) = Volume::mount(disk, now) else { return };
            let mut dirs = vec![ROOT];
            while let Some(dir) = dirs.pop() {
                for e in v.readdir(dir).unwrap_or_default().into_iter().take(64) {
                    let _ = v.stat(e.ino);
                    match e.kind {
                        Kind::Dir if dirs.len() < 16 => dirs.push(e.ino),
                        _ => {
                            let _ = v.read(e.ino, 0, &mut [0; 4096]);
                        }
                    }
                }
            }
            let _ = v.write_file("/Documents/new.txt", &[3; 6000]);
            let _ = v.rename(ROOT, "Documents", ROOT, "Docs");
            let _ = v.unlink(ROOT, "Docs");
            let _ = v.commit();
            let _ = v.fsck();
        },
    );
}

#[test]
fn growing_truncate_is_bounded_and_zero_filled() {
    let mut v = fresh(4096);
    let f = v.create(ROOT, "f", Kind::File).unwrap();
    v.write(f, 0, b"abc").unwrap();
    // Far more than the volume holds: refused up front, without trying to
    // allocate a buffer the size of the gap (which a program could ask for).
    assert_eq!(v.truncate(f, 1 << 39), Err(Error::NoSpace));
    assert_eq!(v.truncate(f, MAX_FILE_SIZE + 1), Err(Error::TooBig));
    assert_eq!(v.stat(f).unwrap().size, 3);
    v.truncate(f, 200_000).unwrap();
    let mut buf = vec![9u8; 200_000];
    assert_eq!(v.read(f, 0, &mut buf).unwrap(), 200_000);
    assert_eq!(&buf[..3], b"abc");
    assert!(buf[3..].iter().all(|&b| b == 0));
    v.fsck().unwrap();
}
