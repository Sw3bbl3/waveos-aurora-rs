//! Model-checking stress and crash-consistency tests.
//!
//! Random sequences of operations run against a volume on a disk that
//! records every write and flush, and against a reference model of what the
//! tree should hold. After every operation `fsck` must find nothing and the
//! volume must match the model; an operation that fails (a full disk) must
//! leave everything as it was.
//!
//! Then the recorded writes are replayed to simulate a power cut at every
//! write boundary: what was flushed survives, and of what was written since,
//! any subset may have reached the disk, in any order, an old write of a
//! block may win over a newer one, and a 4 KiB block may be torn at its
//! 512-byte sectors (what a disk may legally do between flushes). Each image
//! is mounted (replaying the journal), checked with `fsck`, and must match
//! the model as it was after one of the operations between the last `sync`
//! and the cut: operations are atomic, and what was synced is durable. File
//! data is written in place (ordered mode), so each data block may hold any
//! version the file had in that window.
//!
//! A failing sequence is shrunk to a minimal one and printed. Longer runs:
//! `AURORAFS_STRESS_SEEDS=n` sequences (default 40),
//! `AURORAFS_STRESS_OPS=n` operations each (default 60),
//! `AURORAFS_STRESS_SEED=s` the first seed,
//! `AURORAFS_STRESS_CUTS=n` power cuts checked per sequence (default 200).
//! Run them in release mode: `cargo test --release -p aurorafs stress`.

use super::*;
use std::collections::{BTreeMap as Map, BTreeSet};
use std::format;
use std::string::{String, ToString};
use std::vec::Vec;

// ------------------------------------------------------------------ rng

#[derive(Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64) -> Rng {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    pub(crate) fn next(&mut self) -> u64 {
        // splitmix64
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub(crate) fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    pub(crate) fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    pub(crate) fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

pub(crate) fn env(name: &str, default: u64) -> u64 {
    std::env::var(name).ok().and_then(|v| v.parse().ok()).unwrap_or(default)
}

// --------------------------------------------------------- recording disk

#[derive(Clone)]
pub(crate) enum Ev {
    Write(u64, Box<Block>),
    Flush,
    /// Operation `n` (counting from 1) returned.
    OpDone(usize),
    /// Everything up to and including operation `n` was synced.
    Synced(usize),
}

/// A RAM disk that records what is written and flushed.
#[derive(Clone)]
pub(crate) struct RecDisk {
    data: Vec<u8>,
    pub(crate) trace: Vec<Ev>,
    record: bool,
}

impl RecDisk {
    pub(crate) fn new(blocks: usize) -> RecDisk {
        RecDisk { data: vec![0; blocks * BLOCK], trace: Vec::new(), record: false }
    }
    pub(crate) fn from_image(data: Vec<u8>) -> RecDisk {
        RecDisk { data, trace: Vec::new(), record: false }
    }
    pub(crate) fn image(&self) -> Vec<u8> {
        self.data.clone()
    }
    pub(crate) fn start_recording(&mut self) {
        self.trace.clear();
        self.record = true;
    }
    pub(crate) fn mark(&mut self, ev: Ev) {
        if self.record {
            self.trace.push(ev);
        }
    }
}

impl Disk for RecDisk {
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
        if self.record {
            self.trace.push(Ev::Write(b, Box::new(*buf)));
        }
        Ok(())
    }
    fn flush(&mut self) -> Result<()> {
        if self.record {
            self.trace.push(Ev::Flush);
        }
        Ok(())
    }
}

fn now() -> u64 {
    1_000
}

// ----------------------------------------------------------------- model

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) kind: Kind,
    /// Which file this is (it keeps it across renames).
    pub(crate) id: u64,
    pub(crate) data: Vec<u8>,
    /// Bytes from here on were allocated without being written (`allocate`).
    pub(crate) unknown_from: Option<u64>,
    pub(crate) owner: Option<Owner>,
}

/// Open file descriptors a sequence may hold at once.
pub(crate) const FDS: usize = 4;

/// What the tree should hold, by path ("" is the root).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Model {
    pub(crate) nodes: Map<String, Node>,
    next_id: u64,
    /// Open descriptors: the file (by id) each one has.
    pub(crate) fds: [Option<u64>; FDS],
    /// Files removed while open, by id: still there for their descriptors.
    pub(crate) orphans: Map<u64, Node>,
}

fn parent(path: &str) -> &str {
    &path[..path.rfind('/').unwrap_or(0)]
}

fn name_of(path: &str) -> &str {
    &path[path.rfind('/').map_or(0, |i| i + 1)..]
}

impl Model {
    fn is_dir(&self, path: &str) -> bool {
        path.is_empty() || self.nodes.get(path).is_some_and(|n| n.kind == Kind::Dir)
    }
    fn children(&self, dir: &str) -> impl Iterator<Item = &String> {
        let prefix = format!("{dir}/");
        self.nodes.keys().filter(move |k| k.starts_with(&prefix) && !k[prefix.len()..].contains('/'))
    }
    fn files(&self) -> Vec<String> {
        self.nodes.iter().filter(|(_, n)| n.kind == Kind::File).map(|(k, _)| k.clone()).collect()
    }
    fn dirs(&self) -> Vec<String> {
        let mut d: Vec<String> = vec![String::new()];
        d.extend(self.nodes.iter().filter(|(_, n)| n.kind == Kind::Dir).map(|(k, _)| k.clone()));
        d
    }
    fn by_id(&mut self, id: u64) -> Option<&mut Node> {
        if self.orphans.contains_key(&id) {
            return self.orphans.get_mut(&id);
        }
        self.nodes.values_mut().find(|n| n.id == id)
    }
    /// Removes the entry at `path`; an open file lives on as an orphan.
    fn drop_entry(&mut self, path: &str) {
        if let Some(n) = self.nodes.remove(path) {
            if self.fds.contains(&Some(n.id)) {
                self.orphans.insert(n.id, n);
            }
        }
    }
}

fn write_node(n: &mut Node, off: u64, data: &[u8]) -> core::result::Result<(), Error> {
    if n.kind == Kind::Dir {
        return Err(Error::IsDir);
    }
    let end = off as usize + data.len();
    if n.data.len() < end {
        n.data.resize(end, 0);
    }
    n.data[off as usize..end].copy_from_slice(data);
    if let Some(u) = n.unknown_from {
        // What was written is known again; only a gap could still be stale.
        if off <= u && end as u64 >= n.data.len() as u64 {
            n.unknown_from = None;
        }
    }
    Ok(())
}

fn truncate_node(n: &mut Node, size: u64) -> core::result::Result<(), Error> {
    if n.kind == Kind::Dir {
        return Err(Error::IsDir);
    }
    n.data.resize(size as usize, 0);
    if n.unknown_from.is_some_and(|u| u >= size) {
        n.unknown_from = None;
    }
    Ok(())
}

// ------------------------------------------------------------ operations

#[derive(Clone, Debug)]
pub(crate) enum Op {
    Create(String, Kind),
    /// (path, offset, length, pattern): writes `pattern_bytes(length, pattern)`.
    Write(String, u64, usize, u8),
    Truncate(String, u64),
    Rename(String, String),
    Unlink(String),
    Chown(String, Owner),
    Times(String, u64, u64),
    Allocate(String, u64),
    /// `vol.commit()`: everything before it is durable.
    Sync,
    /// Opens the file or folder at the path as descriptor `slot`.
    Open(String, usize),
    Close(usize),
    /// Writes through descriptor `slot` (offset, length, pattern).
    FdWrite(usize, u64, usize, u8),
    FdTruncate(usize, u64),
}

/// What an operation should do to the model: `Ok` with the model after it,
/// or the error the volume must return (and leave everything unchanged).
fn expect(m: &Model, op: &Op) -> core::result::Result<Model, Error> {
    let mut m = m.clone();
    let exists = |m: &Model, p: &str| p.is_empty() || m.nodes.contains_key(p);
    match op {
        Op::Create(p, kind) => {
            if !exists(&m, parent(p)) {
                return Err(Error::NotFound);
            }
            if !m.is_dir(parent(p)) {
                return Err(Error::NotDir);
            }
            if m.nodes.contains_key(p) {
                return Err(Error::Exists);
            }
            m.next_id += 1;
            let id = m.next_id;
            m.nodes.insert(p.clone(), Node { kind: *kind, id, data: Vec::new(), unknown_from: None, owner: None });
        }
        Op::Write(p, off, len, pat) => {
            write_node(m.nodes.get_mut(p).ok_or(Error::NotFound)?, *off, &pattern_bytes(*len, *pat))?;
        }
        Op::Truncate(p, size) => {
            truncate_node(m.nodes.get_mut(p).ok_or(Error::NotFound)?, *size)?;
        }
        Op::Rename(a, b) => {
            if !m.nodes.contains_key(a) {
                return Err(Error::NotFound);
            }
            if !exists(&m, parent(b)) {
                return Err(Error::NotFound);
            }
            if !m.is_dir(parent(b)) {
                return Err(Error::NotDir);
            }
            if a == b {
                return Ok(m);
            }
            if b.starts_with(&format!("{a}/")) {
                return Err(Error::Invalid);
            }
            if let Some(old) = m.nodes.get(b) {
                if old.kind == Kind::Dir || m.nodes[a].kind == Kind::Dir {
                    return Err(Error::Exists);
                }
                m.drop_entry(b);
            }
            let prefix = format!("{a}/");
            let moved: Vec<String> =
                m.nodes.keys().filter(|k| *k == a || k.starts_with(&prefix)).cloned().collect();
            for k in moved {
                let n = m.nodes.remove(&k).unwrap();
                m.nodes.insert(format!("{b}{}", &k[a.len()..]), n);
            }
        }
        Op::Unlink(p) => {
            let n = m.nodes.get(p).ok_or(Error::NotFound)?;
            if n.kind == Kind::Dir && m.children(p).next().is_some() {
                return Err(Error::NotEmpty);
            }
            m.drop_entry(p);
        }
        Op::Chown(p, o) => {
            m.nodes.get_mut(p).ok_or(Error::NotFound)?.owner = Some(*o);
        }
        Op::Times(p, _, _) => {
            m.nodes.get(p).ok_or(Error::NotFound)?;
        }
        Op::Allocate(p, size) => {
            let n = m.nodes.get_mut(p).ok_or(Error::NotFound)?;
            if n.kind == Kind::Dir {
                return Err(Error::IsDir);
            }
            if *size > n.data.len() as u64 {
                let old = n.data.len() as u64;
                n.unknown_from = Some(n.unknown_from.map_or(old, |u| u.min(old)));
                n.data.resize(*size as usize, 0);
            }
        }
        Op::Sync => {}
        Op::Open(p, slot) => {
            if m.fds[*slot].is_some() {
                return Err(Error::Invalid);
            }
            m.fds[*slot] = Some(m.nodes.get(p).ok_or(Error::NotFound)?.id);
        }
        Op::Close(slot) => {
            if let Some(id) = m.fds[*slot].take() {
                if !m.fds.contains(&Some(id)) {
                    m.orphans.remove(&id);
                }
            }
        }
        Op::FdWrite(slot, off, len, pat) => {
            let id = m.fds[*slot].ok_or(Error::Invalid)?;
            write_node(m.by_id(id).unwrap(), *off, &pattern_bytes(*len, *pat))?;
        }
        Op::FdTruncate(slot, size) => {
            let id = m.fds[*slot].ok_or(Error::Invalid)?;
            truncate_node(m.by_id(id).unwrap(), *size)?;
        }
    }
    Ok(m)
}

/// A volume and the descriptors open on it (their inodes).
pub(crate) struct Live<D: Disk> {
    pub(crate) v: Volume<D>,
    pub(crate) fds: [Option<u64>; FDS],
}

/// Runs `op` on the volume.
fn apply<D: Disk>(live: &mut Live<D>, op: &Op) -> Result<()> {
    let fds = &mut live.fds;
    let v = &mut live.v;
    let at = |v: &mut Volume<D>, p: &str| v.resolve(p);
    let split = |v: &mut Volume<D>, p: &str| -> Result<(u64, String)> {
        Ok((v.resolve(parent(p))?, name_of(p).to_string()))
    };
    match op {
        Op::Create(p, kind) => {
            let (d, n) = split(v, p)?;
            v.create(d, &n, *kind).map(|_| ())
        }
        Op::Write(p, off, len, pat) => {
            let i = at(v, p)?;
            v.write(i, *off, &pattern_bytes(*len, *pat)).map(|_| ())
        }
        Op::Truncate(p, size) => {
            let i = at(v, p)?;
            v.truncate(i, *size)
        }
        Op::Rename(a, b) => {
            let (da, na) = split(v, a)?;
            let (db, nb) = split(v, b)?;
            v.rename(da, &na, db, &nb)
        }
        Op::Unlink(p) => {
            let (d, n) = split(v, p)?;
            v.unlink(d, &n)
        }
        Op::Chown(p, o) => {
            let i = at(v, p)?;
            v.set_owner(i, *o)
        }
        Op::Times(p, c, m) => {
            let i = at(v, p)?;
            v.set_times(i, *c, *m)
        }
        Op::Allocate(p, size) => {
            let i = at(v, p)?;
            v.allocate(i, *size)
        }
        Op::Sync => v.commit(),
        Op::Open(p, slot) => {
            if fds[*slot].is_some() {
                return Err(Error::Invalid);
            }
            let i = at(v, p)?;
            v.open_inode(i)?;
            fds[*slot] = Some(i);
            Ok(())
        }
        Op::Close(slot) => match fds[*slot].take() {
            Some(i) => v.close_inode(i),
            None => Ok(()),
        },
        Op::FdWrite(slot, off, len, pat) => {
            let i = fds[*slot].ok_or(Error::Invalid)?;
            v.write(i, *off, &pattern_bytes(*len, *pat)).map(|_| ())
        }
        Op::FdTruncate(slot, size) => {
            let i = fds[*slot].ok_or(Error::Invalid)?;
            v.truncate(i, *size)
        }
    }
}

/// The bytes an `Op::Write` writes.
pub(crate) fn pattern_bytes(len: usize, pat: u8) -> Vec<u8> {
    (0..len).map(|i| pat.wrapping_add((i / 7) as u8).wrapping_mul(pat | 1)).collect()
}

const NAMES: &[&str] = &["a", "b", "c", "d", "long-name-for-a-bigger-entry", "e"];

/// A random operation that makes sense for the model (mostly).
fn gen(r: &mut Rng, m: &Model, disk_blocks: u64) -> Op {
    let dirs = m.dirs();
    let files = m.files();
    let all: Vec<String> = m.nodes.keys().cloned().collect();
    let new_path = |r: &mut Rng| {
        let d = r.pick(&dirs).clone();
        format!("{d}/{}", r.pick(NAMES))
    };
    let some = |r: &mut Rng, list: &Vec<String>| if list.is_empty() { None } else { Some(r.pick(list).clone()) };
    loop {
        match r.below(100) {
            0..=17 => return Op::Create(new_path(r), if r.chance(30) { Kind::Dir } else { Kind::File }),
            18..=37 => {
                if let Some(f) = some(r, &files) {
                    let size = m.nodes[&f].data.len() as u64;
                    let off = match r.below(4) {
                        0 => 0,
                        1 => size,
                        2 => r.below(size + 1),
                        _ => r.below(size + 3 * BLOCK as u64),
                    };
                    let len = match r.below(10) {
                        0..=5 => r.below(3 * BLOCK as u64) as usize + 1,
                        6..=8 => r.below(20 * BLOCK as u64) as usize + 1,
                        // Big: towards a full disk.
                        _ => (r.below(disk_blocks / 3) + 1) as usize * BLOCK,
                    };
                    return Op::Write(f, off, len, r.next() as u8);
                }
            }
            38..=45 => {
                if let Some(f) = some(r, &files) {
                    let size = m.nodes[&f].data.len() as u64;
                    let to = match r.below(3) {
                        0 => 0,
                        1 => r.below(size + 1),
                        _ => size + r.below(8 * BLOCK as u64),
                    };
                    return Op::Truncate(f, to);
                }
            }
            46..=63 => {
                if let Some(a) = some(r, &all) {
                    // Over an existing file, into another folder, or anywhere.
                    let b = match r.below(3) {
                        0 if !all.is_empty() => r.pick(&all).clone(),
                        _ => new_path(r),
                    };
                    return Op::Rename(a, b);
                }
            }
            64..=77 => {
                if let Some(a) = some(r, &all) {
                    return Op::Unlink(a);
                }
            }
            78..=81 => {
                if let Some(a) = some(r, &all) {
                    let o = Owner { uid: r.below(3) as u32 + 999, gid: r.below(3) as u32 + 999, mode: r.below(0o7777) as u16 };
                    return Op::Chown(a, o);
                }
            }
            82..=83 => {
                if let Some(a) = some(r, &all) {
                    return Op::Times(a, r.below(10_000), r.below(10_000));
                }
            }
            84..=86 => {
                if let Some(f) = some(r, &files) {
                    let size = m.nodes[&f].data.len() as u64;
                    return Op::Allocate(f, size + r.below(disk_blocks / 4) * BLOCK as u64);
                }
            }
            87..=89 => {
                let free: Vec<usize> = (0..FDS).filter(|&s| m.fds[s].is_none()).collect();
                if let (Some(a), false) = (some(r, &all), free.is_empty()) {
                    return Op::Open(a, *r.pick(&free));
                }
            }
            90..=95 => {
                let open: Vec<usize> = (0..FDS).filter(|&s| m.fds[s].is_some()).collect();
                if !open.is_empty() {
                    let slot = *r.pick(&open);
                    return match r.below(4) {
                        0 => Op::Close(slot),
                        1 => Op::FdTruncate(slot, r.below(6 * BLOCK as u64)),
                        _ => Op::FdWrite(slot, r.below(4 * BLOCK as u64), r.below(3 * BLOCK as u64) as usize + 1, r.next() as u8),
                    };
                }
            }
            _ => return Op::Sync,
        }
    }
}

// ------------------------------------------------------------- checking

/// What a volume holds, by path: (kind, size, owner if its own, data).
type Listing = Map<String, (Kind, u64, Option<Owner>, Vec<u8>)>;

fn list<D: Disk>(v: &mut Volume<D>) -> core::result::Result<Listing, String> {
    let mut out = Map::new();
    let mut stack = vec![(ROOT, String::new())];
    while let Some((dir, path)) = stack.pop() {
        let entries = v.readdir(dir).map_err(|e| format!("readdir {path:?}: {e:?}"))?;
        for e in entries {
            let p = format!("{path}/{}", e.name);
            let m = v.stat(e.ino).map_err(|x| format!("stat {p}: {x:?}"))?;
            let own = if v.has_own_owner(e.ino).map_err(|x| format!("{p}: {x:?}"))? { Some(m.owner) } else { None };
            let mut data = Vec::new();
            if e.kind == Kind::File {
                data = vec![0; m.size as usize];
                v.read(e.ino, 0, &mut data).map_err(|x| format!("read {p}: {x:?}"))?;
            } else {
                stack.push((e.ino, p.clone()));
            }
            if out.insert(p.clone(), (e.kind, m.size, own, data)).is_some() {
                return Err(format!("{p} listed twice"));
            }
        }
    }
    Ok(out)
}

/// The volume holds exactly what the model says (live: every byte).
fn same_as_model(l: &Listing, m: &Model) -> core::result::Result<(), String> {
    let lk: BTreeSet<&String> = l.keys().collect();
    let mk: BTreeSet<&String> = m.nodes.keys().collect();
    if lk != mk {
        let extra: Vec<_> = lk.difference(&mk).collect();
        let missing: Vec<_> = mk.difference(&lk).collect();
        return Err(format!("tree differs: extra {extra:?}, missing {missing:?}"));
    }
    for (p, n) in &m.nodes {
        let (kind, size, owner, data) = &l[p];
        if *kind != n.kind {
            return Err(format!("{p}: kind {kind:?}, model {:?}", n.kind));
        }
        if n.owner.is_some() && *owner != n.owner {
            return Err(format!("{p}: owner {owner:?}, model {:?}", n.owner));
        }
        if n.kind == Kind::File {
            if *size != n.data.len() as u64 {
                return Err(format!("{p}: size {size}, model {}", n.data.len()));
            }
            let known = n.unknown_from.unwrap_or(u64::MAX).min(*size) as usize;
            if data[..known] != n.data[..known] {
                let at = (0..known).find(|&i| data[i] != n.data[i]).unwrap();
                return Err(format!("{p}: data differs at byte {at}"));
            }
        }
    }
    Ok(())
}

/// The volume after a power cut matches snapshot `s` in structure, and its
/// file data blocks each hold what one of `later` (snapshots from `s` on)
/// had there.
fn matches_after_cut(l: &Listing, s: &Model, later: &[Model]) -> core::result::Result<(), String> {
    let lk: BTreeSet<&String> = l.keys().collect();
    let mk: BTreeSet<&String> = s.nodes.keys().collect();
    if lk != mk {
        let extra: Vec<_> = lk.difference(&mk).collect();
        let missing: Vec<_> = mk.difference(&lk).collect();
        return Err(format!("tree: extra {extra:?}, missing {missing:?}"));
    }
    for (p, n) in &s.nodes {
        let (kind, size, owner, data) = &l[p];
        if *kind != n.kind || (n.owner.is_some() && *owner != n.owner) {
            return Err(format!("{p}: kind/owner"));
        }
        if n.kind != Kind::File {
            continue;
        }
        if *size != n.data.len() as u64 {
            return Err(format!("{p}: size {size} != {}", n.data.len()));
        }
        // Versions of this file (by id) from the snapshot on.
        // (Including after it was removed while open: writes through a
        // descriptor land in the same blocks.)
        let versions: Vec<&Node> = later
            .iter()
            .flat_map(|m| m.nodes.values().chain(m.orphans.values()).filter(|x| x.id == n.id && x.kind == Kind::File))
            .collect();
        let unknown = versions.iter().filter_map(|v| v.unknown_from).min().unwrap_or(u64::MAX);
        // A data write the cut tore leaves some sectors old and some new.
        for (si, chunk) in data.chunks(512).enumerate() {
            let start = si * 512;
            if (start as u64) >= unknown {
                break;
            }
            let ok = versions.iter().any(|v| {
                (0..chunk.len()).all(|i| chunk[i] == v.data.get(start + i).copied().unwrap_or(0))
            });
            if !ok {
                return Err(format!("{p}: sector {si} holds data no version of the file had"));
            }
        }
    }
    Ok(())
}

// ------------------------------------------------------------ the runner

pub(crate) const DISK_BLOCKS: usize = 1024;

#[derive(Debug)]
pub(crate) struct Failure {
    pub(crate) what: String,
    /// The operation it happened at (1-based), or the power cut's description.
    pub(crate) at: String,
}

/// A fresh volume on a recording disk, its image, and the model.
fn setup() -> (Volume<RecDisk>, Model) {
    let v = Volume::format(RecDisk::new(DISK_BLOCKS), "Stress", [3; 16], now).unwrap();
    let mut disk = v.into_disk();
    disk.start_recording();
    // Mount once, as a boot would, so the trace starts from a mounted volume.
    let v = Volume::mount(disk, now).unwrap();
    (v, Model::default())
}

/// Runs `ops`, checking after each one. Returns the trace, the starting
/// image and the model after every operation (`snaps[0]`: before any).
#[allow(clippy::type_complexity)]
pub(crate) fn run_live(ops: &[Op]) -> core::result::Result<(Vec<u8>, Vec<Ev>, Vec<Model>), Failure> {
    let (v, mut m) = setup();
    let mut live = Live { v, fds: [None; FDS] };
    live.v.commit().unwrap();
    let start = live.v.disk().image();
    live.v.disk().start_recording();
    let mut snaps = vec![m.clone()];
    for (i, op) in ops.iter().enumerate() {
        let at = format!("op {} {op:?}", i + 1);
        let want = expect(&m, op);
        let got = apply(&mut live, op);
        let v = &mut live.v;
        match (&want, got) {
            (Ok(next), Ok(())) => m = next.clone(),
            (Err(e), Err(g)) if *e == g => {}
            // The model can't foresee a full disk (or a too-big file).
            (_, Err(Error::NoSpace | Error::TooBig)) => {}
            (w, g) => {
                return Err(Failure { what: format!("returned {g:?}, model expected {:?}", w.as_ref().map(|_| ())), at })
            }
        }
        if let Err(e) = v.fsck() {
            return Err(Failure { what: format!("fsck: {e}"), at });
        }
        let l = list(v).map_err(|e| Failure { what: e, at: at.clone() })?;
        same_as_model(&l, &m).map_err(|e| Failure { what: e, at: at.clone() })?;
        // What each descriptor reads is its file, removed or not.
        for slot in 0..FDS {
            let (Some(ino), Some(id)) = (live.fds[slot], m.fds[slot]) else { continue };
            let want = m.by_id(id).unwrap().clone();
            let st = live.v.stat(ino).map_err(|e| Failure { what: format!("fd {slot}: stat {e:?}"), at: at.clone() })?;
            if st.kind != want.kind || (want.kind == Kind::File && st.size != want.data.len() as u64) {
                return Err(Failure { what: format!("fd {slot}: {:?} of {} bytes, model {:?} of {}", st.kind, st.size, want.kind, want.data.len()), at });
            }
            if want.kind == Kind::File {
                let mut data = vec![0; st.size as usize];
                live.v.read(ino, 0, &mut data).map_err(|e| Failure { what: format!("fd {slot}: read {e:?}"), at: at.clone() })?;
                let known = want.unknown_from.unwrap_or(u64::MAX).min(st.size) as usize;
                if data[..known] != want.data[..known] {
                    return Err(Failure { what: format!("fd {slot}: data differs"), at });
                }
            }
        }
        let v = &mut live.v;
        snaps.push(m.clone());
        v.disk().mark(Ev::OpDone(i + 1));
        if matches!(op, Op::Sync) {
            v.disk().mark(Ev::Synced(i + 1));
        }
    }
    let trace = core::mem::take(&mut live.v.disk().trace);
    Ok((start, trace, snaps))
}

/// How a power cut treats the writes since the last flush.
#[derive(Clone, Copy, Debug)]
enum Cut {
    /// All of them reached the disk, in order (QEMU killed).
    All,
    /// None did.
    None,
    /// A random subset, each block's surviving version chosen at random,
    /// some torn at sector boundaries.
    Random(u64),
}

/// Simulates power cuts across the trace; checks each recovered volume.
pub(crate) fn check_cuts(start: &[u8], trace: &[Ev], snaps: &[Model], cuts: u64, seed: u64) -> core::result::Result<usize, Failure> {
    let mut r = Rng::new(seed);
    let writes: Vec<usize> = trace.iter().enumerate().filter(|(_, e)| matches!(e, Ev::Write(..))).map(|(i, _)| i).collect();
    // Which write boundaries to cut at: all of them if few, else a sample
    // (always including those just before and after each flush).
    let mut points: BTreeSet<usize> = BTreeSet::new();
    if writes.len() as u64 <= cuts {
        points.extend(writes.iter().copied());
    } else {
        for (i, e) in trace.iter().enumerate() {
            if matches!(e, Ev::Flush) && points.len() < cuts as usize / 2 {
                points.insert(i);
            }
        }
        while (points.len() as u64) < cuts {
            points.insert(writes[r.below(writes.len() as u64) as usize]);
        }
    }
    points.insert(trace.len());
    let mut durable = start.to_vec();
    let mut pending: Vec<(u64, &Block)> = Vec::new();
    let (mut done, mut synced) = (0usize, 0usize);
    let mut checked = 0;
    for (i, ev) in trace.iter().enumerate().chain(core::iter::once((trace.len(), &Ev::Flush))) {
        if points.contains(&i) {
            // The power goes before event `i`.
            let modes = [Cut::All, Cut::None, Cut::Random(r.next()), Cut::Random(r.next())];
            for mode in modes {
                let img = cut_image(&durable, &pending, mode);
                let at = format!("power cut before trace event {i} of {} ({mode:?}), {done} ops done, synced at {synced}", trace.len());
                check_recovered(img, snaps, synced, done).map_err(|what| Failure { what, at })?;
                checked += 1;
            }
        }
        if i == trace.len() {
            break;
        }
        match ev {
            Ev::Write(b, data) => pending.push((*b, data)),
            Ev::Flush => {
                for (b, d) in pending.drain(..) {
                    durable[b as usize * BLOCK..(b as usize + 1) * BLOCK].copy_from_slice(&d[..]);
                }
            }
            Ev::OpDone(n) => done = *n,
            Ev::Synced(n) => synced = *n,
        }
    }
    Ok(checked)
}

fn cut_image(durable: &[u8], pending: &[(u64, &Block)], mode: Cut) -> Vec<u8> {
    let mut img = durable.to_vec();
    let put = |img: &mut Vec<u8>, b: u64, d: &Block, lo: usize, hi: usize| {
        let o = b as usize * BLOCK;
        img[o + lo..o + hi].copy_from_slice(&d[lo..hi]);
    };
    match mode {
        Cut::None => {}
        Cut::All => {
            for (b, d) in pending {
                put(&mut img, *b, d, 0, BLOCK);
            }
        }
        Cut::Random(seed) => {
            let mut r = Rng::new(seed);
            // Per block, which of its pending versions (if any) survives.
            let mut by_block: Map<u64, Vec<&Block>> = Map::new();
            for (b, d) in pending {
                by_block.entry(*b).or_default().push(d);
            }
            for (b, versions) in by_block {
                if r.chance(50) {
                    continue;
                }
                let d = versions[r.below(versions.len() as u64) as usize];
                if r.chance(15) {
                    // Torn: some sectors new, the rest old.
                    for s in 0..BLOCK / 512 {
                        if r.chance(50) {
                            put(&mut img, b, d, s * 512, (s + 1) * 512);
                        }
                    }
                } else {
                    put(&mut img, b, d, 0, BLOCK);
                }
            }
        }
    }
    img
}

/// Mounts a power-cut image and checks it against the snapshots that may
/// legally be what survived: from `synced` (durable) to `done + 1` (the
/// operation that was running).
fn check_recovered(img: Vec<u8>, snaps: &[Model], synced: usize, done: usize) -> core::result::Result<(), String> {
    let mut v = Volume::mount(RecDisk::from_image(img), now).map_err(|e| format!("mount: {e:?}"))?;
    // As the kernel does at mount: files that were open when removed are
    // left over (and only those); repair frees them.
    let report = v.check();
    if !report.only_orphans() {
        return Err(format!("fsck: {}", report.summary(10).trim_end().replace('\n', "; ")));
    }
    if !report.is_clean() {
        let (_, after) = v.repair().map_err(|e| format!("repair: {e:?}"))?;
        if !after.is_clean() {
            return Err(format!("after repair: {}", after.summary(10)));
        }
    }
    v.fsck().map_err(|e| format!("fsck: {e}"))?;
    let l = list(&mut v)?;
    let hi = (done + 1).min(snaps.len() - 1);
    let mut why = Vec::new();
    for s in synced..=hi {
        match matches_after_cut(&l, &snaps[s], &snaps[s..=hi]) {
            Ok(()) => {
                // And the volume goes on working.
                let mut r = Rng::new(s as u64);
                let mut m = snaps[s].clone();
                m.fds = [None; FDS];
                m.orphans.clear();
                let mut live = Live { v, fds: [None; FDS] };
                for _ in 0..4 {
                    let op = gen(&mut r, &m, DISK_BLOCKS as u64);
                    if apply(&mut live, &op).is_ok() {
                        if let Ok(n) = expect(&m, &op) {
                            m = n;
                        }
                    }
                }
                live.v.commit().map_err(|e| format!("commit after recovery: {e:?}"))?;
                live.v.fsck().map_err(|e| format!("fsck after more work on the recovered volume: {e}"))?;
                return Ok(());
            }
            Err(e) => why.push(format!("after op {s}: {e}")),
        }
    }
    Err(format!("the recovered volume matches no state between op {synced} and op {hi}: {}", why.join("; ")))
}

/// The whole check for one sequence.
pub(crate) fn check_sequence(ops: &[Op], cuts: u64, seed: u64) -> core::result::Result<usize, Failure> {
    let (start, trace, snaps) = run_live(ops)?;
    check_cuts(&start, &trace, &snaps, cuts, seed)
}

/// Shrinks a failing sequence: drops chunks, then single operations, then
/// simplifies writes, while it still fails the same way.
pub(crate) fn shrink(ops: Vec<Op>, cuts: u64, seed: u64, same: &dyn Fn(&Failure) -> bool) -> (Vec<Op>, Failure) {
    let fails = |o: &[Op]| match check_sequence(o, cuts, seed) {
        Err(f) if same(&f) => Some(f),
        _ => None,
    };
    let mut best = ops;
    let mut failure = fails(&best).expect("the sequence fails");
    let mut chunk = best.len() / 2;
    while chunk >= 1 {
        let mut i = 0;
        while i < best.len() {
            let mut cand = best.clone();
            cand.drain(i..(i + chunk).min(cand.len()));
            if let Some(f) = fails(&cand) {
                best = cand;
                failure = f;
            } else {
                i += chunk;
            }
        }
        chunk /= 2;
    }
    // Smaller writes.
    for i in 0..best.len() {
        if let Op::Write(p, o, n, pat) = &best[i] {
            for len in [1usize, BLOCK, n / 2] {
                if len >= *n {
                    continue;
                }
                let mut cand = best.clone();
                cand[i] = Op::Write(p.clone(), *o, len, *pat);
                if let Some(f) = fails(&cand) {
                    best = cand;
                    failure = f;
                    break;
                }
            }
        }
    }
    (best, failure)
}

pub(crate) fn random_ops(seed: u64, count: usize) -> Vec<Op> {
    let mut r = Rng::new(seed);
    let mut m = Model::default();
    let mut ops = Vec::new();
    for _ in 0..count {
        let op = gen(&mut r, &m, DISK_BLOCKS as u64);
        if let Ok(n) = expect(&m, &op) {
            // (Writes may still fail for space; the model only guides generation.)
            m = n;
        }
        ops.push(op);
    }
    ops
}

#[test]
fn stress_random_sequences_and_power_cuts() {
    let seeds = env("AURORAFS_STRESS_SEEDS", 40);
    let first = env("AURORAFS_STRESS_SEED", 1);
    let count = env("AURORAFS_STRESS_OPS", 60) as usize;
    let cuts = env("AURORAFS_STRESS_CUTS", 200);
    let mut total = 0;
    for seed in first..first + seeds {
        let ops = random_ops(seed, count);
        match check_sequence(&ops, cuts, seed) {
            Ok(n) => total += n,
            Err(f) => {
                let prefix: String = f.what.chars().take(24).collect();
                let (min, f2) = shrink(ops, cuts, seed, &|g: &Failure| g.what.starts_with(&prefix));
                panic!(
                    "seed {seed}: {} at {}\nshrunk to {} ops ({} at {}):\n{min:#?}",
                    f.what,
                    f.at,
                    min.len(),
                    f2.what,
                    f2.at
                );
            }
        }
    }
    std::println!("{seeds} sequences of {count} operations, {total} power cuts checked");
}
