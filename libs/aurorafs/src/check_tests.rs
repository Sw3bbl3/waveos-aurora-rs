//! `check` finds every kind of inconsistency, all at once, and `repair`
//! fixes the ones it safely can. Each test breaks a healthy volume on
//! purpose, through the volume's own internals.

use super::tests::MemDisk;
use super::*;
use std::format;

fn now() -> u64 {
    5
}

/// A volume with /a (a folder holding f1 and f2), /b (a file) and /c (empty).
fn healthy() -> Volume<MemDisk> {
    let mut v = Volume::format(MemDisk::new(2048), "Check", [2; 16], now).unwrap();
    v.write_file("/a/f1", &[1u8; 20_000]).unwrap();
    v.write_file("/a/f2", &[2u8; 9_000]).unwrap();
    v.write_file("/b", b"bee").unwrap();
    v.create(ROOT, "c", Kind::Dir).unwrap();
    v.commit().unwrap();
    v.fsck().unwrap();
    v
}

/// Adds a raw entry to `dir` (no checks).
fn add_entry(v: &mut Volume<MemDisk>, dir: u64, name: &str, ino: u64, kind: Kind) {
    v.op(|v| {
        let mut e = v.readdir(dir)?;
        e.push(DirEntry { name: name.into(), ino, kind });
        v.write_dir(dir, &e)
    })
    .unwrap();
}

/// An allocated inode no folder names, with `links` links.
fn loose_inode(v: &mut Volume<MemDisk>, links: u32) -> u64 {
    v.op(|v| {
        let ino = v.alloc_inode()?;
        let mut inode = Inode::new(Kind::File as u16, links, 1, None);
        v.grow(&mut inode, 2)?;
        inode.size = 5000;
        v.write_inode(ino, &inode)?;
        Ok(ino)
    })
    .unwrap()
}

/// What a report should hold.
type Expect<'a> = dyn Fn(&Problem) -> bool + 'a;

fn has(r: &Report, f: impl Fn(&Problem) -> bool) -> bool {
    r.problems.iter().any(f)
}

fn repaired_clean(v: &mut Volume<MemDisk>) {
    let (_, left) = v.repair().unwrap();
    assert!(left.is_clean(), "left after repair:\n{}", left.summary(50));
    v.fsck().unwrap();
    // And it stays so on the disk.
    let disk = v.disk().clone();
    Volume::mount(disk, now).unwrap().fsck().unwrap();
}

#[test]
fn every_problem_is_reported_at_once_and_repaired() {
    let mut v = healthy();
    let a = v.resolve("/a").unwrap();
    let c = v.resolve("/c").unwrap();
    let f1 = v.resolve("/a/f1").unwrap();
    let b = v.resolve("/b").unwrap();
    // 1. An inode with a link but no entry; 2. one with none (an orphan).
    let lost = loose_inode(&mut v, 1);
    let orphan = loose_inode(&mut v, 0);
    // 3. An entry naming a free inode.
    add_entry(&mut v, c, "dangling", 900, Kind::File);
    // 4. A second entry for f1 (a hard link AuroraFS doesn't have).
    add_entry(&mut v, c, "second-name", f1, Kind::File);
    // 5. The same name twice.
    add_entry(&mut v, c, "dangling", b, Kind::File);
    // 6. A folder inside itself.
    add_entry(&mut v, a, "loop", a, Kind::Dir);
    // 7. An entry of the wrong kind.
    add_entry(&mut v, c, "kind", b, Kind::Dir);
    // 8. A wrong link count.
    v.op(|v| {
        let mut i = v.read_inode(b)?;
        i.links = 7;
        v.write_inode(b, &i)
    })
    .unwrap();
    // 9. Blocks marked used that nothing uses, and 10. a used one marked free.
    let leak = v.sb.blocks - 10;
    let f1_block = v.read_inode(f1).unwrap().extents[0].start;
    v.op(|v| {
        v.set_map_bit(false, leak, true);
        v.set_map_bit(false, leak + 1, true);
        v.set_map_bit(false, f1_block + 1, false);
        Ok(())
    })
    .unwrap();
    // 11. Free counts that disagree with the bitmaps.
    v.op(|v| {
        v.sb.free_blocks += 3;
        v.sb.free_inodes -= 1;
        v.dirty_super();
        Ok(())
    })
    .unwrap();
    v.commit().unwrap();
    let r = v.check();
    let want: &[(&str, &Expect<'_>)] = &[
        ("unreachable", &|p| matches!(p, Problem::Unreachable { ino, links: 1, .. } if *ino == lost)),
        ("orphan", &|p| matches!(p, Problem::Unreachable { ino, links: 0, .. } if *ino == orphan)),
        ("dangling", &|p| matches!(p, Problem::BadEntry { fault: EntryFault::Dangling, ino: 900, .. })),
        ("second name", &|p| matches!(p, Problem::BadEntry { fault: EntryFault::ReachedTwice, ino, .. } if *ino == f1)),
        ("duplicate", &|p| matches!(p, Problem::BadEntry { fault: EntryFault::Duplicate, .. })),
        ("cycle", &|p| matches!(p, Problem::BadEntry { fault: EntryFault::ReachedTwice, ino, .. } if *ino == a)),
        ("kind", &|p| matches!(p, Problem::BadEntry { fault: EntryFault::WrongKind(Kind::File), .. })),
        ("links", &|p| matches!(p, Problem::LinkCount { found: 7, want: 1, .. })),
        ("leak", &|p| matches!(p, Problem::Leaked { start, count: 2 } if *start == leak)),
        ("used but free", &|p| matches!(p, Problem::UsedButFree { block, .. } if *block == f1_block + 1)),
        ("free blocks", &|p| matches!(p, Problem::FreeBlocks { .. })),
        ("free inodes", &|p| matches!(p, Problem::FreeInodes { .. })),
    ];
    for (what, f) in want {
        assert!(has(&r, f), "{what} not reported; the report:\n{}", r.summary(50));
    }
    assert!(!r.only_orphans());
    repaired_clean(&mut v);
    // What had a link but no entry is back, under /lost+found; the orphan is gone.
    let back = v.resolve(&format!("/{LOST_AND_FOUND}/#{lost}")).unwrap();
    assert_eq!(back, lost);
    assert_eq!(v.stat(lost).unwrap().size, 5000);
    assert!(v.stat(orphan).is_err());
    // Nothing healthy was touched.
    let mut buf = vec![0u8; 20_000];
    v.read(f1, 0, &mut buf).unwrap();
    assert!(buf.iter().all(|&x| x == 1));
    assert_eq!(v.lookup(a, "f2").map(|_| ()), Ok(()));
}

#[test]
fn a_folder_whose_contents_stop_parsing_is_cut_there() {
    let mut v = healthy();
    let a = v.resolve("/a").unwrap();
    // Garbage after the entries, as a folder written but not yet cut to
    // its new size would leave (the "dir 85: Corrupt" of a power cut).
    v.op(|v| {
        let size = v.read_inode(a)?.size;
        v.write_inner(a, size, &[0xEE; 37])?;
        Ok(())
    })
    .unwrap();
    let r = v.check();
    assert!(has(&r, |p| matches!(p, Problem::BadDir { dir, .. } if *dir == a)), "{}", r.summary(10));
    repaired_clean(&mut v);
    assert!(v.resolve("/a/f1").is_ok() && v.resolve("/a/f2").is_ok());
}

#[test]
fn a_detached_cycle_of_folders_goes_to_lost_and_found() {
    let mut v = healthy();
    let x = v.mkdir_all("/x/y").unwrap();
    let xi = v.resolve("/x").unwrap();
    v.write_file("/x/y/data", b"keep me").unwrap();
    // /x/y gets an entry back to /x, then /x is cut from the root: a loop
    // nothing reaches.
    add_entry(&mut v, x, "up", xi, Kind::Dir);
    v.op(|v| {
        let mut e = v.readdir(ROOT)?;
        e.retain(|e| e.name != "x");
        v.write_dir(ROOT, &e)
    })
    .unwrap();
    let r = v.check();
    assert!(has(&r, |p| matches!(p, Problem::Unreachable { .. })), "{}", r.summary(10));
    assert!(has(&r, |p| matches!(p, Problem::BadEntry { fault: EntryFault::ReachedTwice, .. })), "{}", r.summary(10));
    repaired_clean(&mut v);
    let lf = v.resolve(&format!("/{LOST_AND_FOUND}")).unwrap();
    let found: Vec<String> = v.readdir(lf).unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(found.len(), 1, "{found:?}");
    let data = v.resolve(&format!("/{LOST_AND_FOUND}/{}/y/data", found[0])).unwrap();
    let mut buf = [0u8; 7];
    v.read(data, 0, &mut buf).unwrap();
    assert_eq!(&buf, b"keep me");
}

#[test]
fn overlapping_extents_are_reported_and_left_alone() {
    let mut v = healthy();
    let f1 = v.resolve("/a/f1").unwrap();
    let b = v.resolve("/b").unwrap();
    let shared = v.read_inode(f1).unwrap().extents[0];
    v.op(|v| {
        let mut i = v.read_inode(b)?;
        let mine = i.extents[0];
        i.extents[0] = Extent { start: shared.start, len: 1 };
        v.write_inode(b, &i)?;
        v.free_extent(mine);
        Ok(())
    })
    .unwrap();
    v.commit().unwrap();
    let r = v.check();
    assert!(has(&r, |p| matches!(p, Problem::SharedBlock { block, .. } if *block == shared.start)), "{}", r.summary(10));
    let (_, left) = v.repair().unwrap();
    assert!(has(&left, |p| matches!(p, Problem::SharedBlock { .. })), "a shared block can't be repaired safely");
    // Both files still have their data.
    assert!(v.stat(f1).is_ok() && v.stat(b).is_ok());
}

#[test]
fn bad_inodes_are_dropped_and_their_blocks_freed() {
    let mut v = healthy();
    let b = v.resolve("/b").unwrap();
    // An extent far outside the volume.
    v.op(|v| {
        let (blk, off) = v.inode_location(b)?;
        let mut img = zero_block();
        v.meta_read(blk, &mut img)?;
        put_u64(&mut img[..], off + 40, 1 << 40);
        v.meta_write(blk, &img);
        Ok(())
    })
    .unwrap();
    let r = v.check();
    assert!(has(&r, |p| matches!(p, Problem::BadInode { ino, .. } if *ino == b)), "{}", r.summary(10));
    assert!(has(&r, |p| matches!(p, Problem::BadEntry { fault: EntryFault::BadInode, .. })), "{}", r.summary(10));
    repaired_clean(&mut v);
    assert_eq!(v.resolve("/b"), Err(Error::NotFound));
}

#[test]
fn a_clean_unmount_is_remembered_and_cleared_by_the_next_change() {
    let mut v = healthy();
    v.unmount().unwrap();
    let mut v = Volume::mount(v.into_disk(), now).unwrap();
    assert!(v.was_clean());
    // Mounting cleared it: a crash now would leave it not clean.
    let crashed = v.disk().after_power_cut();
    assert!(!Volume::mount(crashed, now).unwrap().was_clean());
    v.write_file("/new", b"x").unwrap();
    v.commit().unwrap();
    assert!(!Volume::mount(v.into_disk(), now).unwrap().was_clean());
}

#[test]
fn an_open_file_keeps_its_inode_until_closed_and_unmount_is_then_clean() {
    let mut v = healthy();
    let b = v.resolve("/b").unwrap();
    v.open_inode(b).unwrap();
    v.unlink(ROOT, "b").unwrap();
    let n = v.create(ROOT, "new", Kind::Dir).unwrap();
    assert_ne!(n, b);
    assert_eq!(v.truncate(n, 0), Err(Error::IsDir));
    v.write(b, 3, b" still here").unwrap();
    v.fsck().unwrap();
    // Open at unmount: left not clean, and the next mount's check frees it.
    v.unmount().unwrap();
    let mut again = Volume::mount(v.disk().clone(), now).unwrap();
    assert!(!again.was_clean());
    let r = again.check();
    assert!(r.only_orphans() && r.problems.len() == 1, "{}", r.summary(10));
    repaired_clean(&mut again);
    // Closed: freed, and the unmount is clean.
    v.close_inode(b).unwrap();
    assert!(v.stat(b).is_err());
    v.unmount().unwrap();
    assert!(Volume::mount(v.into_disk(), now).unwrap().was_clean());
}
