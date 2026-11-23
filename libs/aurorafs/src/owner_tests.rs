//! Host-side tests for owners, groups and modes (feature `FEATURE_OWNERS`):
//! persistence, journaling, and volumes from before the feature.

use super::tests::MemDisk;
use super::*;

fn now() -> u64 {
    1_000
}

fn fresh(blocks: usize) -> Volume<MemDisk> {
    Volume::format(MemDisk::new(blocks), "Test", [7; 16], now).unwrap()
}

const ALICE: Owner = Owner { uid: 1000, gid: 1000, mode: 0o700 };
const BOB: Owner = Owner { uid: 1001, gid: 1001, mode: 0o640 };

/// What an older kernel's next commit leaves: a superblock without the
/// extension (its encoder writes only the first 188 bytes).
fn as_older_kernel(v: &mut Volume<MemDisk>) {
    v.sb.features = 0;
    v.sb.default_uid = 0;
    v.sb.default_gid = 0;
    v.dirty_super();
    v.commit().unwrap();
}

/// Changes inode `ino`'s raw bytes (through the journal, as a kernel would).
fn poke_inode(v: &mut Volume<MemDisk>, ino: u64, f: impl FnOnce(&mut [u8])) {
    let (blk, off) = v.inode_location(ino).unwrap();
    let mut b = zero_block();
    v.meta_read(blk, &mut b).unwrap();
    f(&mut b[off..off + INODE_SIZE]);
    v.meta_write(blk, &b);
    v.commit().unwrap();
}

#[test]
fn new_volumes_have_owners_and_a_default_owner() {
    let mut v = fresh(4096);
    assert_eq!(v.features() & FEATURE_OWNERS, FEATURE_OWNERS);
    assert_eq!(v.default_owner(), (FIRST_UID, FIRST_GID));
    // Files made without an owner belong to the default owner.
    let f = v.write_file("/Documents/a.txt", b"a").unwrap();
    assert_eq!(v.owner(f).unwrap(), Owner { uid: FIRST_UID, gid: FIRST_GID, mode: 0o644 });
    assert!(!v.has_own_owner(f).unwrap());
    let d = v.resolve("/Documents").unwrap();
    assert_eq!(v.owner(d).unwrap().mode, 0o755);
    v.fsck().unwrap();
}

#[test]
fn owners_survive_a_remount_and_renames() {
    let mut v = fresh(4096);
    let home = v.mkdir_all("/Users").unwrap();
    let b = v.create_owned(home, "bob", Kind::Dir, Some(BOB)).unwrap();
    let f = v.create_owned(b, "secret.txt", Kind::File, Some(Owner { mode: 0o600, ..BOB })).unwrap();
    v.write(f, 0, b"mine").unwrap();
    v.set_owner(home, Owner { uid: 0, gid: 0, mode: 0o1755 }).unwrap();
    v.rename(b, "secret.txt", ROOT, "moved.txt").unwrap();
    v.commit().unwrap();
    let mut v = Volume::mount(v.into_disk(), now).unwrap();
    let bob = v.resolve("/Users/bob").unwrap();
    assert_eq!(v.owner(bob).unwrap(), BOB);
    let f = v.resolve("/moved.txt").unwrap();
    assert_eq!(v.owner(f).unwrap(), Owner { mode: 0o600, ..BOB });
    let users = v.resolve("/Users").unwrap();
    assert_eq!(v.owner(users).unwrap(), Owner { uid: 0, gid: 0, mode: 0o1755 });
    // Mode bits beyond 0o7777 are never stored.
    v.set_owner(f, Owner { mode: 0o177_777, ..ALICE }).unwrap();
    assert_eq!(v.owner(f).unwrap().mode, 0o7777);
    v.fsck().unwrap();
}

#[test]
fn volumes_from_before_owners_mount_and_take_a_default_owner() {
    let mut v = fresh(4096);
    v.write_file("/Documents/old.txt", b"written by an older kernel").unwrap();
    as_older_kernel(&mut v);
    let mut v = Volume::mount(v.into_disk().after_power_cut(), now).unwrap();
    assert_eq!(v.features(), 0);
    let f = v.resolve("/Documents/old.txt").unwrap();
    assert_eq!(v.owner(f).unwrap().uid, 0);
    // The upgrade a kernel makes on first mount: one superblock change.
    v.enable_owners(1000, 1000).unwrap();
    assert_eq!(v.owner(f).unwrap(), Owner { uid: 1000, gid: 1000, mode: 0o644 });
    let mut v = Volume::mount(v.into_disk().after_power_cut(), now).unwrap();
    assert_eq!(v.features(), FEATURE_OWNERS);
    assert_eq!(v.default_owner(), (1000, 1000));
    v.fsck().unwrap();
}

#[test]
fn an_older_kernel_rewriting_inodes_falls_back_to_the_default_owner() {
    let mut v = fresh(4096);
    let f = v.write_file("/note.txt", b"x").unwrap();
    v.set_owner(f, BOB).unwrap();
    v.commit().unwrap();
    // An older kernel writes the inode with bytes 240..256 zeroed and the
    // superblock without its extension.
    poke_inode(&mut v, f, |r| r[240..256].fill(0));
    as_older_kernel(&mut v);
    let mut v = Volume::mount(v.into_disk().after_power_cut(), now).unwrap();
    assert_eq!(v.features(), 0);
    v.fsck().unwrap();
    v.enable_owners(1000, 1000).unwrap();
    assert_eq!(v.owner(f).unwrap(), Owner { uid: 1000, gid: 1000, mode: 0o644 });
}

#[test]
fn owner_changes_are_journaled() {
    let mut v = fresh(4096);
    let f = v.write_file("/a.txt", b"a").unwrap();
    v.set_owner(f, ALICE).unwrap();
    v.commit().unwrap();
    // Changed but not committed: a power cut keeps the old owner.
    v.set_owner(f, BOB).unwrap();
    let mut lost = Volume::mount(v.disk().after_power_cut(), now).unwrap();
    assert_eq!(lost.owner(f).unwrap(), ALICE);
    lost.fsck().unwrap();
    // Committed to the journal only: replay brings the new owner.
    v.commit_journal_only().unwrap();
    let mut kept = Volume::mount(v.disk().after_power_cut(), now).unwrap();
    assert_eq!(kept.owner(f).unwrap(), BOB);
    kept.fsck().unwrap();
}

#[test]
fn fsck_refuses_unknown_inode_flags() {
    let mut v = fresh(4096);
    let f = v.write_file("/a.txt", b"a").unwrap();
    poke_inode(&mut v, f, |r| r[250] = 0x80);
    let mut v = Volume::mount(v.into_disk().after_power_cut(), now).unwrap();
    assert!(v.fsck().unwrap_err().contains("unknown flags"));
}

#[test]
fn a_damaged_extension_reads_as_absent() {
    let mut v = fresh(4096);
    v.commit().unwrap();
    let mut b = *v.sb.encode();
    assert_eq!(Superblock::decode(&b).unwrap().features, FEATURE_OWNERS);
    b[190] ^= 0xFF;
    let sb = Superblock::decode(&b).unwrap();
    assert_eq!((sb.features, sb.default_uid), (0, 0));
}
