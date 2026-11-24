//! Racing threads through the kernel's VFS, with reproducible interleavings.
//!
//! The kernel's AuroraFS adapter (`kernel/src/fs/aurorafs.rs`) holds one
//! lock over the volume for each call, and the VFS (`kernel/src/fs/mod.rs`)
//! makes several calls for one system call: it resolves a path one lookup
//! at a time, checks, then acts. Other threads run in between. The fuzzers
//! do exactly that from 3-4 threads in one small tree: racing renames,
//! removals and creations, descriptors kept open across removals.
//!
//! Here real threads run those same call sequences, and every call takes
//! the volume through a scheduler that lets one thread at a time in, chosen
//! by a seeded random number generator once every live thread is waiting
//! for it. So an interleaving that breaks the volume is found again from
//! its seed. `fsck` runs every few calls and at the end.
//!
//! Longer runs: `AURORAFS_RACE_SEEDS` (default 60), `AURORAFS_RACE_OPS`
//! operations per thread (default 120), `AURORAFS_RACE_THREADS` (4).

use super::stress::{env, pattern_bytes, Rng};
use super::*;
use std::format;
use std::string::{String, ToString};
use std::sync::{Arc, Condvar, Mutex};
use std::vec::Vec;

/// A RAM disk (no recording: this is about the tree, not power cuts).
struct Ram(Vec<u8>);

impl Disk for Ram {
    fn blocks(&self) -> u64 {
        (self.0.len() / BLOCK) as u64
    }
    fn read(&mut self, b: u64, buf: &mut Block) -> Result<()> {
        let o = b as usize * BLOCK;
        buf.copy_from_slice(self.0.get(o..o + BLOCK).ok_or(Error::Io)?);
        Ok(())
    }
    fn write(&mut self, b: u64, buf: &Block) -> Result<()> {
        let o = b as usize * BLOCK;
        self.0.get_mut(o..o + BLOCK).ok_or(Error::Io)?.copy_from_slice(buf);
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        Ok(())
    }
}

fn now() -> u64 {
    7
}

struct State {
    vol: Volume<Ram>,
    /// Threads waiting for the volume.
    waiting: Vec<usize>,
    /// Threads still running.
    alive: usize,
    /// Whose turn it is.
    turn: Option<usize>,
    rng: Rng,
    calls: u64,
    /// The first inconsistency `fsck` found, with the call it followed.
    broken: Option<String>,
    /// Pin open files (what the kernel does now); off: as it did before.
    pin: bool,
}

/// The volume behind a deterministic scheduler.
struct Shared {
    s: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    /// One volume call by thread `me`, as the kernel adapter makes it:
    /// under the lock, and atomic.
    fn call<T>(&self, me: usize, what: &str, f: impl FnOnce(&mut Volume<Ram>) -> T) -> T {
        let mut s = self.s.lock().unwrap();
        s.waiting.push(me);
        loop {
            if s.turn.is_none() && s.waiting.len() == s.alive {
                // Everyone is waiting: pick who goes next.
                let n = s.waiting.len() as u64;
                let i = s.rng.below(n) as usize;
                s.waiting.sort();
                s.turn = Some(s.waiting[i]);
                self.cv.notify_all();
            }
            if s.turn == Some(me) {
                break;
            }
            s = self.cv.wait(s).unwrap();
        }
        s.waiting.retain(|&t| t != me);
        let out = f(&mut s.vol);
        s.calls += 1;
        if s.calls.is_multiple_of(7) && s.broken.is_none() {
            if let Err(e) = s.vol.fsck() {
                let (open, orphans) = (s.vol.open.clone(), s.vol.orphans.clone());
                s.broken = Some(format!("after call {} ({what}, thread {me}): {e} (open {open:?}, orphans {orphans:?})", s.calls));
            }
        }
        s.turn = None;
        self.cv.notify_all();
        out
    }

    fn done(&self) {
        let mut s = self.s.lock().unwrap();
        s.alive -= 1;
        self.cv.notify_all();
    }

    fn pin(&self) -> bool {
        self.s.lock().unwrap().pin
    }
}

type R<T> = core::result::Result<T, Error>;

/// The VFS, as `kernel/src/fs/mod.rs` does it, one volume call at a time.
struct Vfs<'a> {
    sh: &'a Shared,
    me: usize,
}

/// An open descriptor (`OpenFile`).
struct Fd {
    ino: u64,
    writable: bool,
}

fn split(path: &str) -> (&str, &str) {
    let i = path.rfind('/').unwrap();
    (if i == 0 { "/" } else { &path[..i] }, &path[i + 1..])
}

impl Vfs<'_> {
    fn call<T>(&self, what: &str, f: impl FnOnce(&mut Volume<Ram>) -> T) -> T {
        self.sh.call(self.me, what, f)
    }
    /// `walk`: a lookup per component.
    fn lookup(&self, path: &str) -> R<u64> {
        let mut ino = ROOT;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            ino = self.call("lookup", |v| v.lookup(ino, c))?;
        }
        Ok(ino)
    }
    /// `parent_full`: the folder, checked to be one.
    fn parent(&self, path: &str) -> R<(u64, String)> {
        let (dir, name) = split(path);
        let d = self.lookup(dir)?;
        if self.call("stat", |v| v.stat(d))?.kind != Kind::Dir {
            return Err(Error::NotDir);
        }
        Ok((d, name.to_string()))
    }
    fn open(&self, path: &str, create: bool, truncate: bool) -> R<Fd> {
        let ino = match self.lookup(path) {
            Ok(i) => i,
            Err(Error::NotFound) if create => {
                let (d, name) = self.parent(path)?;
                self.call("create", |v| v.create(d, &name, Kind::File))?
            }
            Err(e) => return Err(e),
        };
        let writable = create || truncate;
        if self.sh.pin() {
            self.call("open", |v| v.open_inode(ino))?;
        }
        let md = self.call("stat", |v| v.stat(ino));
        let md = match md {
            Ok(m) => m,
            Err(e) => {
                self.close(Fd { ino, writable });
                return Err(e);
            }
        };
        if writable && md.kind == Kind::Dir {
            self.close(Fd { ino, writable });
            return Err(Error::IsDir);
        }
        if truncate {
            let _ = self.call("truncate", |v| v.truncate(ino, 0));
        }
        Ok(Fd { ino, writable })
    }
    fn close(&self, fd: Fd) {
        if self.sh.pin() {
            let _ = self.call("close", |v| v.close_inode(fd.ino));
        }
    }
    /// `OpenFile::write`: the adapter checks the kind, then writes.
    fn write(&self, fd: &Fd, off: u64, data: &[u8]) -> R<usize> {
        if !fd.writable {
            return Err(Error::Invalid);
        }
        self.call("write", |v| {
            if v.stat(fd.ino)?.kind == Kind::Dir {
                return Err(Error::IsDir);
            }
            v.write(fd.ino, off, data)
        })
    }
    fn truncate(&self, fd: &Fd, size: u64) -> R<()> {
        if !fd.writable {
            return Err(Error::Invalid);
        }
        self.call("ftruncate", |v| v.truncate(fd.ino, size))
    }
    fn mkdir(&self, path: &str) -> R<()> {
        if self.lookup(path).is_ok() {
            return Err(Error::Exists);
        }
        let (d, name) = self.parent(path)?;
        if self.call("lookup", |v| v.lookup(d, &name)).is_ok() {
            return Err(Error::Exists);
        }
        self.call("mkdir", |v| v.create(d, &name, Kind::Dir)).map(|_| ())
    }
    fn unlink(&self, path: &str) -> R<()> {
        let (d, name) = self.parent(path)?;
        let _ = self.call("lookup", |v| v.lookup(d, &name));
        self.call("unlink", |v| v.unlink(d, &name))
    }
    fn rename(&self, from: &str, to: &str) -> R<()> {
        let (da, na) = self.parent(from)?;
        let (db, nb) = self.parent(to)?;
        // The VFS's own check, by path (racy: the library checks again).
        if to.starts_with(from) && to.as_bytes().get(from.len()) == Some(&b'/') {
            return Err(Error::Invalid);
        }
        let _ = self.call("lookup", |v| v.lookup(db, &nb));
        self.call("rename", |v| v.rename(da, &na, db, &nb))
    }
    /// `remove_tree` (what `rm -rf` amounts to).
    fn remove_tree(&self, path: &str) -> R<()> {
        let ino = self.lookup(path)?;
        if self.call("stat", |v| v.stat(ino))?.kind == Kind::Dir {
            let entries = self.call("readdir", |v| v.readdir(ino))?;
            for e in entries {
                let _ = self.remove_tree(&format!("{path}/{}", e.name));
            }
        }
        self.unlink(path)
    }
}

const PATHS: &[&str] = &["/t/a", "/t/b", "/t/a/x", "/t/a/y", "/t/b/x", "/t/a/x/z", "/t/b/x/z", "/t/c", "/t/a/c"];

/// One thread's work: random VFS calls on a small shared tree.
fn worker(sh: &Shared, me: usize, seed: u64, ops: usize) {
    let fs = Vfs { sh, me };
    let mut r = Rng::new(seed.wrapping_mul(31).wrapping_add(me as u64));
    let mut fds: Vec<Fd> = Vec::new();
    for _ in 0..ops {
        let p = *r.pick(PATHS);
        let q = *r.pick(PATHS);
        let _ = match r.below(16) {
            0..=1 => fs.mkdir(p),
            2..=3 => fs.open(p, true, r.chance(50)).map(|fd| {
                let _ = fs.write(&fd, r.below(3 * BLOCK as u64), &pattern_bytes(r.below(9000) as usize + 1, r.next() as u8));
                if r.chance(60) {
                    fds.push(fd);
                } else {
                    fs.close(fd);
                }
            }),
            4..=6 => fs.rename(p, q),
            7..=8 => fs.unlink(p),
            9 => fs.remove_tree(p),
            10..=12 => {
                if fds.is_empty() {
                    Ok(())
                } else {
                    let i = r.below(fds.len() as u64) as usize;
                    if r.chance(50) {
                        fs.write(&fds[i], r.below(2 * BLOCK as u64), &pattern_bytes(r.below(5000) as usize + 1, r.next() as u8)).map(|_| ())
                    } else {
                        fs.truncate(&fds[i], r.below(3 * BLOCK as u64))
                    }
                }
            }
            13 => {
                if !fds.is_empty() {
                    let i = r.below(fds.len() as u64) as usize;
                    fs.close(fds.swap_remove(i));
                }
                Ok(())
            }
            14 => fs.open(p, false, false).map(|fd| fds.push(fd)),
            _ => fs.mkdir("/t").or(Ok(())),
        };
    }
    for fd in fds {
        fs.close(fd);
    }
}

/// Runs `threads` workers from `seed`; returns the first inconsistency.
fn race(seed: u64, threads: usize, ops: usize, pin: bool) -> core::result::Result<u64, String> {
    let mut vol = Volume::format(Ram(vec![0; 2048 * BLOCK]), "Race", [5; 16], now).unwrap();
    let t = vol.create(ROOT, "t", Kind::Dir).unwrap();
    for d in ["a", "b"] {
        vol.create(t, d, Kind::Dir).unwrap();
    }
    let sh = Arc::new(Shared {
        s: Mutex::new(State {
            vol,
            waiting: Vec::new(),
            alive: threads,
            turn: None,
            rng: Rng::new(seed),
            calls: 0,
            broken: None,
            pin,
        }),
        cv: Condvar::new(),
    });
    let handles: Vec<_> = (0..threads)
        .map(|me| {
            let sh = sh.clone();
            std::thread::spawn(move || {
                worker(&sh, me, seed, ops);
                sh.done();
            })
        })
        .collect();
    for h in handles {
        h.join().map_err(|_| String::from("a worker panicked"))?;
    }
    let mut s = sh.s.lock().unwrap();
    if let Some(b) = s.broken.take() {
        return Err(b);
    }
    s.vol.fsck().map_err(|e| format!("at the end: {e}"))?;
    if s.vol.orphans() != 0 {
        return Err(format!("{} orphans left with every descriptor closed", s.vol.orphans()));
    }
    s.vol.commit().unwrap();
    // And what is on the disk mounts clean.
    let disk = Ram(s.vol.disk().0.clone());
    let mut again = Volume::mount(disk, now).map_err(|e| format!("mount: {e:?}"))?;
    again.fsck().map_err(|e| format!("after a remount: {e}"))?;
    Ok(s.calls)
}

#[test]
fn racing_threads_keep_the_volume_consistent() {
    let seeds = env("AURORAFS_RACE_SEEDS", 60);
    let ops = env("AURORAFS_RACE_OPS", 120) as usize;
    let threads = env("AURORAFS_RACE_THREADS", 4) as usize;
    let first = env("AURORAFS_RACE_SEED", 1);
    // The seeds of the fuzz rounds in the failure logs, then a range.
    let logged = [0x6141_bd9d_f9a5_b45d, 0x79a5_28c7_ec20_2727, 0xd651_ed71_8519_91ce, 0x3881_d99d_95c2_982d];
    let mut calls = 0;
    for seed in logged.into_iter().chain(first..first + seeds) {
        match race(seed, threads, ops, true) {
            Ok(n) => calls += n,
            Err(e) => panic!("seed {seed:#x}: {e}"),
        }
    }
    std::println!("{} seeds, {threads} threads of {ops} operations: {calls} volume calls", seeds + 4);
}

/// Without pinning open files, a descriptor outlives its file's inode: the
/// number goes to the next file or folder made, and the descriptor writes
/// into that. The volume itself stays consistent (writes and truncates
/// refuse folders), which is what this checks; the files' contents don't.
#[test]
fn racing_threads_without_pinning_stay_consistent() {
    for seed in 1..=20 {
        if let Err(e) = race(seed, 4, 80, false) {
            panic!("seed {seed}: {e}");
        }
    }
}
