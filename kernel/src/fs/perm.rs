//! File permissions: who may read, write, run, list, create in, remove from
//! and rename in what.
//!
//! The VFS asks [`caller`] for the credentials of the calling process and
//! checks Unix permission bits against the owner of each object: search
//! (`x`) on every folder walked through, read or write on what is opened,
//! write and search on the folder an entry is made in, removed from or
//! renamed in (with the sticky bit's restriction), read on a folder
//! listed. Kernel tasks — and kernel code acting for the system inside a
//! system call ([`system`]) — are not checked.
//!
//! Filesystems without owners get them from where they are mounted:
//! the system image (`/System`) belongs to the system and is read-only
//! for everyone; the EFI partition (`/Boot`) to the system and the admin
//! group; removable volumes to the logged-in account.
//!
//! On the home volume, `/Security`, `/Users` and `/Applications` are the
//! system's: nobody else may create, remove or rename them.

use super::*;
use crate::users::{self, Cred};
use alloc::collections::BTreeSet;

pub const R: u16 = 4;
pub const W: u16 = 2;
pub const X: u16 = 1;
const STICKY: u16 = 0o1000;

/// Top-level names on the home volume only the system may change.
const PROTECTED: [&str; 3] = ["Security", "Users", "Applications"];
/// The first account's personal folders, private on an upgraded volume.
const PRIVATE: [&str; 6] = ["Desktop", "Documents", "Downloads", "Pictures", "Music", "Movies"];

/// Tasks running kernel code for the system inside a system call.
static SYSTEM_TASKS: IrqMutex<BTreeSet<u64>> = IrqMutex::new(BTreeSet::new());

/// Runs `f` with the calling task's file access unchecked: kernel code
/// that keeps the system's own files (preferences, the account database)
/// while serving a system call. Never wrap a path the caller chose.
pub fn system<T>(f: impl FnOnce() -> T) -> T {
    let me = crate::sched::current_id();
    let fresh = SYSTEM_TASKS.lock().insert(me);
    let out = f();
    if fresh {
        SYSTEM_TASKS.lock().remove(&me);
    }
    out
}

/// The credentials file access is checked against: None for the system.
pub fn caller() -> Option<Cred> {
    let p = crate::proc::current()?;
    if SYSTEM_TASKS.lock().contains(&crate::sched::current_id()) {
        return None;
    }
    let c = *p.cred.lock();
    (!c.is_root()).then_some(c)
}

/// The permission bits `cred` gets on something owned by `o`: the owner's,
/// the group's, or everyone else's.
pub fn bits(cred: &Cred, o: &Owner) -> u16 {
    if cred.uid == o.uid {
        (o.mode >> 6) & 7
    } else if cred.in_group(o.gid) {
        (o.mode >> 3) & 7
    } else {
        o.mode & 7
    }
}

pub fn allowed(cred: &Cred, o: &Owner, want: u16) -> bool {
    bits(cred, o) & want == want
}

fn need(cred: Option<&Cred>, o: &Owner, want: u16) -> FsResult<()> {
    match cred {
        Some(c) if !allowed(c, o, want) => Err(EACCES),
        _ => Ok(()),
    }
}

/// The owner of things on a filesystem that has none, by its mount point.
fn mount_owner(mount: &str) -> Owner {
    match mount {
        "/System" => Owner { uid: users::ROOT_UID, gid: users::SYSTEM_GID, mode: 0o755 },
        "/Boot" => Owner { uid: users::ROOT_UID, gid: users::ADMIN_GID, mode: 0o775 },
        _ => {
            let s = users::session();
            Owner { uid: s.uid, gid: s.gid, mode: 0o755 }
        }
    }
}

/// Effective owner of `ino` on `fs` (mounted at `mount`), and whether the
/// inode carries it itself.
pub(super) fn owner_of(fs: &Arc<dyn Filesystem>, mount: &str, ino: Ino) -> (Owner, bool) {
    fs.owner(ino).unwrap_or_else(|| (mount_owner(mount), false))
}

/// Effective owner of the file or folder at `path`. Files under `/Linux`
/// from before owners existed count as executable (Linux programs need
/// the bit, and there was no way to keep it).
pub fn stat_owner(path: &str) -> FsResult<(Owner, bool)> {
    let path = resolve("/", path);
    let (fs, ino, mount) = lookup_full(&path)?;
    let (mut o, explicit) = owner_of(&fs, &mount, ino);
    if !explicit && path.starts_with("/Linux/") && fs.metadata(ino)?.kind == Kind::File {
        o.mode |= 0o111;
    }
    Ok((o, explicit))
}

/// What a new entry in folder `dir` belongs to: the creating process, or,
/// for the system, the folder's own owner (so a screenshot the desktop
/// saves in someone's Pictures is theirs). None: the volume's default.
pub(super) fn new_owner(
    cred: Option<&Cred>,
    fs: &Arc<dyn Filesystem>,
    mount: &str,
    dir: Ino,
    kind: Kind,
    mode: Option<u16>,
) -> Option<Owner> {
    let base = match kind {
        Kind::Dir => 0o755,
        Kind::File => 0o644,
    };
    match cred {
        Some(c) => Some(Owner { uid: c.uid, gid: c.gid, mode: mode.unwrap_or(base | 0o022) & !c.umask & 0o7777 }),
        None => {
            let (o, explicit) = owner_of(fs, mount, dir);
            if !explicit && mode.is_none() {
                return None;
            }
            Some(Owner { uid: o.uid, gid: o.gid, mode: mode.unwrap_or(base) & 0o7777 })
        }
    }
}

/// Checks search on every folder `walk` goes through (called per step).
pub(super) fn search(cred: Option<&Cred>, fs: &Arc<dyn Filesystem>, mount: &str, dir: Ino) -> FsResult<()> {
    if cred.is_none() {
        return Ok(());
    }
    need(cred, &owner_of(fs, mount, dir).0, X)
}

/// May the caller open what's at `ino` this way?
pub(super) fn check_open(fs: &Arc<dyn Filesystem>, mount: &str, ino: Ino, read: bool, write: bool) -> FsResult<()> {
    let cred = caller();
    let Some(c) = cred.as_ref() else { return Ok(()) };
    let o = owner_of(fs, mount, ino).0;
    need(Some(c), &o, if read { R } else { 0 } | if write { W } else { 0 })
}

/// May the caller list folder `ino`?
pub(super) fn check_list(fs: &Arc<dyn Filesystem>, mount: &str, ino: Ino) -> FsResult<()> {
    need(caller().as_ref(), &owner_of(fs, mount, ino).0, R)
}

/// Is `name` in folder `dir` a top-level name only the system may change?
fn protected(mount: &str, fs: &Arc<dyn Filesystem>, dir: Ino, name: &str) -> bool {
    mount == "/" && dir == fs.root() && PROTECTED.contains(&name)
}

/// May the caller make `name` in folder `dir`?
pub(super) fn check_create(fs: &Arc<dyn Filesystem>, mount: &str, dir: Ino, name: &str) -> FsResult<()> {
    let cred = caller();
    if cred.is_some() && protected(mount, fs, dir, name) {
        return Err(EPERM);
    }
    need(cred.as_ref(), &owner_of(fs, mount, dir).0, W | X)
}

/// May the caller remove (or rename away) `name` from folder `dir`? In a
/// sticky folder only the entry's or the folder's owner may.
pub(super) fn check_remove(fs: &Arc<dyn Filesystem>, mount: &str, dir: Ino, name: &str) -> FsResult<()> {
    let cred = caller();
    let Some(c) = cred.as_ref() else { return Ok(()) };
    if protected(mount, fs, dir, name) {
        return Err(EPERM);
    }
    let d = owner_of(fs, mount, dir).0;
    need(Some(c), &d, W | X)?;
    if d.mode & STICKY != 0 && c.uid != d.uid {
        let ino = fs.lookup(dir, name)?;
        if owner_of(fs, mount, ino).0.uid != c.uid {
            return Err(EACCES);
        }
    }
    Ok(())
}

/// Linux programs need `x` on the file (and search on the way to it).
pub fn check_exec(path: &str) -> FsResult<()> {
    let Some(c) = caller() else { return Ok(()) };
    let (o, _) = stat_owner(path)?;
    if allowed(&c, &o, X) {
        Ok(())
    } else {
        Err(EACCES)
    }
}

/// May the logged-in account do `want` with `path`? (For what the window
/// server shows or does on its behalf.)
pub fn session_allows(path: &str, want: u16) -> bool {
    let s = users::session();
    s.is_root() || stat_owner(path).is_ok_and(|(o, _)| allowed(&s, &o, want))
}

/// `access()`: may the caller do `want` (R, W, X bits) with `path`?
pub fn access(path: &str, want: u16) -> FsResult<()> {
    let (o, _) = stat_owner(path)?;
    let (_, ro) = stat(path)?;
    if want & W != 0 && ro {
        return Err(EROFS);
    }
    need(caller().as_ref(), &o, want)
}

/// `chmod`: the owner (or the system) changes the permission bits.
pub fn chmod(path: &str, mode: u16) -> FsResult<()> {
    let (fs, ino, mount) = lookup_full(&resolve("/", path))?;
    if fs.read_only() {
        return Err(EROFS);
    }
    let (o, _) = owner_of(&fs, &mount, ino);
    if let Some(c) = caller() {
        if c.uid != o.uid && !c.is_admin() {
            return Err(EPERM);
        }
    }
    fs.set_owner(ino, Owner { mode: mode & 0o7777, ..o })
}

/// `chown`: changing the owner takes the administrator capability; the
/// owner may hand the group to one they are in. `u32::MAX` = unchanged.
pub fn chown(path: &str, uid: u32, gid: u32) -> FsResult<()> {
    let (fs, ino, mount) = lookup_full(&resolve("/", path))?;
    if fs.read_only() {
        return Err(EROFS);
    }
    let (o, _) = owner_of(&fs, &mount, ino);
    let new = Owner {
        uid: if uid == u32::MAX { o.uid } else { uid },
        gid: if gid == u32::MAX { o.gid } else { gid },
        mode: o.mode,
    };
    if let Some(c) = caller() {
        let own_group_change = new.uid == o.uid && c.uid == o.uid && c.in_group(new.gid);
        if new != o && !own_group_change && !c.is_admin() {
            return Err(EPERM);
        }
    }
    // A new owner loses the set-id bits, as on Unix.
    let mode = if new.uid != o.uid || new.gid != o.gid { new.mode & !0o6000 } else { new.mode };
    fs.set_owner(ino, Owner { mode, ..new })
}

/// Sets owner, group and mode outright (the system's own files).
pub fn set_owner_path(path: &str, owner: Owner) -> FsResult<()> {
    let (fs, ino) = lookup(path)?;
    fs.set_owner(ino, owner)
}

/// True if `path` and its folder belong to the system and nobody else may
/// write them (what the account database must be to be trusted).
pub fn system_owned(path: &str) -> bool {
    let check = |p: &str| match lookup_full(&resolve("/", p)) {
        // A filesystem without owners can't keep anything private; the
        // in-memory one (no disk) can.
        Ok((fs, ino, mount)) => {
            let (o, explicit) = owner_of(&fs, &mount, ino);
            explicit && o.uid == users::ROOT_UID && o.mode & 0o022 == 0
        }
        Err(_) => false,
    };
    check(path) && check(&parent_path(&resolve("/", path)))
}

/// Makes a folder only the system can use (`rwx------ system`).
pub fn ensure_system_dir(path: &str) -> FsResult<()> {
    match mkdir(path) {
        Ok(()) | Err(EEXIST) => {}
        Err(e) => return Err(e),
    }
    set_owner_path(path, Owner::system(0o700))
}

/// Makes `/Users/<name>` for account `uid`, with the usual folders inside.
pub fn make_home(home: &str, uid: u32) {
    system(|| {
        if !is_dir("/Users") {
            let _ = mkdir("/Users");
            let _ = set_owner_path("/Users", Owner::system(0o755));
        }
        let _ = mkdir(home);
        let _ = set_owner_path(home, Owner { uid, gid: uid, mode: 0o755 });
        for d in PRIVATE.iter().take(4) {
            let p = join(home, d);
            let _ = mkdir(&p);
            let _ = set_owner_path(&p, Owner { uid, gid: uid, mode: 0o700 });
        }
    });
}

/// First boot with owners: gives the home volume owners (an older volume
/// is the first account's), makes the first account's personal folders
/// private and creates the system's folders.
pub fn prepare_root_volume() {
    system(|| {
        let Ok((fs, root)) = lookup("/") else { return };
        match fs.enable_owners(users::FIRST_UID, users::FIRST_UID) {
            Ok(true) => log!("vfs", "the home volume now has owners (uid {} by default)", users::FIRST_UID),
            Ok(false) => {}
            Err(e) => log!("vfs", "could not turn on owners: {}", aurora_abi::err::name(e)),
        }
        if fs.owner(root).is_none() {
            return;
        }
        if !is_dir(super::super::users::db::DIR) {
            for name in PRIVATE {
                let p = join("/", name);
                if let Ok((o, false)) = stat_owner(&p) {
                    let _ = set_owner_path(&p, Owner { mode: 0o700, ..o });
                }
            }
            let _ = ensure_system_dir(super::super::users::db::DIR);
        }
        if !is_dir("/Applications") {
            let _ = mkdir("/Applications");
        }
        if let Ok((o, explicit)) = stat_owner("/Applications") {
            let want = Owner { uid: users::ROOT_UID, gid: users::ADMIN_GID, mode: 0o775 };
            if !explicit || o != want {
                let _ = set_owner_path("/Applications", want);
            }
        }
    });
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    let alice = Cred::for_user(1000, true);
    let bob = Cred::for_user(1001, false);
    let mine = Owner { uid: 1000, gid: 1000, mode: 0o640 };
    assert!(allowed(&alice, &mine, R | W) && !allowed(&alice, &mine, X));
    assert!(!allowed(&bob, &mine, R));
    let shared = Owner { uid: 1000, gid: users::USERS_GID, mode: 0o640 };
    assert!(allowed(&bob, &shared, R) && !allowed(&bob, &shared, W));
    let apps = Owner { uid: 0, gid: users::ADMIN_GID, mode: 0o775 };
    assert!(allowed(&alice, &apps, W) && !allowed(&bob, &apps, W) && allowed(&bob, &apps, R | X));

    // On an in-memory volume mounted for the test: Bob is refused what's
    // private to Alice; the system is refused nothing.
    let fs: Arc<dyn Filesystem> = Arc::new(super::ramfs::RamFs::new());
    let root = fs.root();
    let home = fs.create_owned(root, "alice", Kind::Dir, Some(Owner { uid: 1000, gid: 1000, mode: 0o700 })).unwrap();
    let file = fs.create_owned(home, "a.txt", Kind::File, Some(mine)).unwrap();
    assert_eq!(fs.owner(file), Some((mine, true)));
    assert!(search(Some(&bob), &fs, "/t", home).is_err());
    assert!(search(Some(&alice), &fs, "/t", home).is_ok());
    assert!(search(None, &fs, "/t", home).is_ok());
    let tmp = fs.create_owned(root, "tmp", Kind::Dir, Some(Owner { uid: 0, gid: 0, mode: 0o1777 })).unwrap();
    let bobs = fs.create_owned(tmp, "b", Kind::File, Some(Owner { uid: 1001, gid: 1001, mode: 0o644 })).unwrap();
    let _ = bobs;
    // Sticky: Alice can write in the folder but not remove Bob's file.
    let d = owner_of(&fs, "/t", tmp).0;
    assert!(allowed(&alice, &d, W | X));
    assert!(d.mode & STICKY != 0);
    // Owners made for a creator follow its umask.
    let o = new_owner(Some(&bob), &fs, "/t", tmp, Kind::File, Some(0o666)).unwrap();
    assert_eq!((o.uid, o.mode), (1001, 0o644));
    // The system's files in someone's folder become theirs.
    let o = new_owner(None, &fs, "/t", home, Kind::File, None).unwrap();
    assert_eq!((o.uid, o.mode), (1000, 0o644));
    assert_eq!(mount_owner("/System"), Owner { uid: 0, gid: 0, mode: 0o755 });
}
