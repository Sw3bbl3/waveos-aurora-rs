//! Minimal reproducers of the consistency bugs found by the stress, crash
//! and race harnesses (`stress.rs`, `concurrent.rs`), kept as regression
//! tests. Each one failed on the code before the fix, with the message
//! quoted in its comment. They use only the long-standing API, so they also
//! run against a copy of that older code.

use super::*;
use std::format;
use std::vec::Vec;

fn now() -> u64 {
    1
}

/// A RAM disk that records its writes and flushes, to rebuild the disk as
/// a power cut at any point would have left it.
#[derive(Clone, Default)]
struct Rec {
    data: Vec<u8>,
    /// (block, data) for writes; None for a flush.
    log: Vec<Option<(u64, Vec<u8>)>>,
}

impl Disk for Rec {
    fn blocks(&self) -> u64 {
        (self.data.len() / BLOCK) as u64
    }
    fn read(&mut self, b: u64, buf: &mut Block) -> Result<()> {
        let o = b as usize * BLOCK;
        buf.copy_from_slice(self.data.get(o..o + BLOCK).ok_or(Error::Io)?);
        Ok(())
    }
    fn write(&mut self, b: u64, buf: &Block) -> Result<()> {
        let o = b as usize * BLOCK;
        self.data.get_mut(o..o + BLOCK).ok_or(Error::Io)?.copy_from_slice(buf);
        self.log.push(Some((b, buf.to_vec())));
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        self.log.push(None);
        Ok(())
    }
}

fn rec(blocks: usize) -> Rec {
    Rec { data: vec![0; blocks * BLOCK], log: Vec::new() }
}

/// The disk `start` after replaying the first `n` logged writes (in
/// order; what a disk that stopped there kept).
fn cut(start: &[u8], log: &[Option<(u64, Vec<u8>)>], n: usize) -> Rec {
    let mut data = start.to_vec();
    for (b, d) in log.iter().flatten().take(n) {
        data[*b as usize * BLOCK..(*b as usize + 1) * BLOCK].copy_from_slice(d);
    }
    Rec { data, log: Vec::new() }
}

/// Runs `setup`, commits, then `act`, and checks the volume a power cut
/// after every one of `act`'s writes would leave.
fn every_cut(blocks: usize, setup: impl Fn(&mut Volume<Rec>), act: impl Fn(&mut Volume<Rec>)) {
    let mut v = Volume::format(rec(blocks), "Cut", [1; 16], now).unwrap();
    setup(&mut v);
    v.commit().unwrap();
    let start = v.disk().data.clone();
    v.disk().log.clear();
    act(&mut v);
    let log = v.disk().log.clone();
    let writes = log.iter().flatten().count();
    for n in 0..=writes {
        let mut r = Volume::mount(cut(&start, &log, n), now).unwrap();
        if let Err(e) = r.fsck() {
            panic!("power cut after write {n} of {writes}: {e}");
        }
    }
}

/// Fills the disk with one file until a write fails for space.
fn fill(v: &mut Volume<Rec>) {
    let f = v.create(ROOT, "filler", Kind::File).unwrap();
    let mut off = 0;
    let chunk = vec![7u8; BLOCK];
    while v.write(f, off, &chunk).is_ok() {
        off += BLOCK as u64;
    }
}

/// Creates files in `dir` until its contents are exactly one block.
fn fill_dir_block(v: &mut Volume<Rec>, dir: u64) {
    // Entries are 10 bytes plus the name: 4096 = 30 * 120 + 496 = ...
    let mut used = 0usize;
    let mut i = 0;
    while used < BLOCK {
        let left = BLOCK - used;
        let len = if left - 10 <= 255 { left - 10 } else { 100.min(left - 10 - 11) };
        let name = format!("{i:0>width$}", width = len);
        v.create(dir, &name, Kind::File).unwrap();
        used += 10 + len;
        i += 1;
    }
    assert_eq!(v.stat(dir).unwrap().size, BLOCK as u64);
}

/// Making a file on a full disk, when its folder needs another block:
/// before the fix the new inode stayed allocated with no entry naming it
/// ("inode 4 allocated but unreachable"; the native fuzzer's failure).
#[test]
fn create_on_a_full_disk_leaks_no_inode() {
    let mut v = Volume::format(rec(512), "Full", [1; 16], now).unwrap();
    let d = v.create(ROOT, "d", Kind::Dir).unwrap();
    fill_dir_block(&mut v, d);
    fill(&mut v);
    assert_eq!(v.create(d, "one-more", Kind::File), Err(Error::NoSpace));
    v.fsck().unwrap();
    v.commit().unwrap();
    Volume::mount(v.into_disk(), now).unwrap().fsck().unwrap();
}

/// Moving a file into a folder that needs another block, on a full disk:
/// before the fix the source folder lost the entry and the destination
/// never got it ("inode 3 allocated but unreachable").
#[test]
fn rename_on_a_full_disk_loses_nothing() {
    let mut v = Volume::format(rec(512), "Full", [1; 16], now).unwrap();
    let a = v.create(ROOT, "a", Kind::Dir).unwrap();
    v.create(a, "staying", Kind::File).unwrap();
    let f = v.create(a, "moving", Kind::File).unwrap();
    let b = v.create(ROOT, "b", Kind::Dir).unwrap();
    fill_dir_block(&mut v, b);
    fill(&mut v);
    assert_eq!(v.rename(a, "moving", b, "moved"), Err(Error::NoSpace));
    assert_eq!(v.lookup(a, "moving"), Ok(f));
    v.fsck().unwrap();
}

/// A power cut inside `unlink` on a volume with a small journal: the
/// transaction used to be committed after the folder was rewritten and
/// before the inode was freed ("inode 2 allocated but unreachable").
#[test]
fn power_cut_inside_unlink() {
    every_cut(
        1024,
        |v| {
            v.write_file("/a", b"gone soon").unwrap();
        },
        |v| {
            v.unlink(ROOT, "a").unwrap();
            v.commit().unwrap();
        },
    );
}

/// A power cut while a folder shrinks: its new contents were committed
/// before its new size, leaving the old size over a stale tail
/// ("dir 1: Corrupt"; the "dir 85: Corrupt" of the install test).
#[test]
fn power_cut_while_a_folder_shrinks() {
    every_cut(
        1024,
        |v| {
            for i in 0..40 {
                v.write_file(&format!("/d/entry-with-a-long-name-{i}"), b"x").unwrap();
            }
        },
        |v| {
            let d = v.resolve("/d").unwrap();
            for i in (0..40).step_by(3) {
                v.unlink(d, &format!("entry-with-a-long-name-{i}")).unwrap();
            }
            v.rename(d, "entry-with-a-long-name-1", d, "short").unwrap();
            v.commit().unwrap();
        },
    );
}

/// A power cut inside a rename that replaces a file.
#[test]
fn power_cut_inside_rename_over_a_file() {
    every_cut(
        1024,
        |v| {
            v.write_file("/a/new", b"new contents").unwrap();
            v.write_file("/b/old", b"old contents").unwrap();
        },
        |v| {
            let (a, b) = (v.resolve("/a").unwrap(), v.resolve("/b").unwrap());
            v.rename(a, "new", b, "old").unwrap();
            v.commit().unwrap();
        },
    );
}

/// A folder moved into its own subtree: the VFS refuses it by path, but
/// a racing rename can make the path it checked stale. Before the fix the
/// library did it, cutting the subtree off ("inode 2 allocated but
/// unreachable").
#[test]
fn folder_cannot_move_into_itself() {
    let mut v = Volume::format(rec(1024), "Cycle", [1; 16], now).unwrap();
    let a = v.create(ROOT, "a", Kind::Dir).unwrap();
    let b = v.create(a, "b", Kind::Dir).unwrap();
    assert_eq!(v.rename(ROOT, "a", b, "a"), Err(Error::Invalid));
    assert_eq!(v.rename(ROOT, "a", a, "a"), Err(Error::Invalid));
    v.fsck().unwrap();
}

/// The journal checksum: a power cut that lands the next transaction's
/// superblock image in the journal over the last one's, before the next
/// descriptor, used to pass the last commit record's CRC-32 (a superblock
/// embeds its own CRC-32, and CRC-32 over data plus its CRC-32 is the same
/// for all data). Replay then put the new superblock over the old bitmaps
/// ("free block count 879 != bitmap 940").
#[test]
fn torn_journal_with_a_new_superblock_image_is_not_replayed() {
    // (Big enough for a full-size journal: one commit for the whole file.)
    let mut v = Volume::format(rec(16384), "Crc", [1; 16], now).unwrap();
    v.commit().unwrap();
    let start = v.disk().data.clone();
    v.disk().log.clear();
    v.write_file("/f", &[5u8; 60_000]).unwrap();
    v.commit().unwrap();
    let log = v.disk().log.clone();
    // The last commit: flush, descriptor, superblock image (block 0 sorts
    // first), the other images, commit record, flush, checkpoint.
    let flushes: Vec<usize> = log.iter().enumerate().filter(|(_, e)| e.is_none()).map(|(i, _)| i).collect();
    let journal_start = flushes[flushes.len() - 2] + 1;
    let mut data = start.clone();
    for (i, e) in log.iter().enumerate() {
        let Some((b, d)) = e else { continue };
        // Everything before the commit, and of the journal writes only the
        // superblock image (the second one): the descriptor never landed.
        if i < journal_start || i == journal_start + 1 {
            data[*b as usize * BLOCK..(*b as usize + 1) * BLOCK].copy_from_slice(d);
        }
    }
    let mut r = Volume::mount(Rec { data, log: Vec::new() }, now).unwrap();
    r.fsck().unwrap();
}
