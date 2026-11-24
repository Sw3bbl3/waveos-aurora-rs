//! Checking a volume (`fsck`) and repairing what can safely be repaired.
//!
//! [`Volume::check`] walks the whole tree from the root and compares what it
//! finds with the bitmaps and the superblock. It reports every problem, not
//! just the first: entries naming free or unreadable inodes, folders whose
//! contents don't parse, names that are invalid or repeated, an inode
//! reached twice (a cycle, or a second name), wrong link counts, inodes
//! allocated but in no folder, blocks claimed twice, blocks in use but free
//! in the bitmap or marked used with nothing using them, and free counts
//! that disagree with the bitmaps.
//!
//! [`Volume::repair`] fixes what it can without guessing: it drops bad
//! entries, cuts a folder's contents at the first record that doesn't
//! parse, frees inodes left by files removed while open (no links), puts
//! other unreachable files and folders back under `/lost+found` (nothing is
//! thrown away), corrects link counts, the bitmaps and the free counts. A
//! block claimed by two files is reported and left alone.

use super::*;
use alloc::format;
use alloc::string::ToString;
use core::fmt;

/// What is wrong with a directory entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryFault {
    /// It names an inode that is free (or out of range).
    Dangling,
    /// It names an inode that can't be read (bad extents, kind or size).
    BadInode,
    /// Its name is empty, ".", "..", or holds '/' or NUL.
    BadName,
    /// Another entry in the folder has the same name.
    Duplicate,
    /// Its inode was already reached by another entry (a second name, or
    /// a folder inside itself).
    ReachedTwice,
    /// It says file where the inode is a folder, or the other way round.
    WrongKind(Kind),
}

/// One inconsistency [`Volume::check`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// The root folder is missing or unreadable (not repairable).
    BadRoot(String),
    /// Inode `ino` can't be read: bad extents or kind.
    BadInode { ino: u64, why: String },
    /// Inode `ino` claims a size beyond its blocks.
    BadSize { ino: u64, size: u64, blocks: u64 },
    /// Inode flags this version doesn't know, or an impossible owner.
    BadFlags { ino: u64, flags: u16 },
    /// Folder `dir`'s contents stop parsing at byte `valid` of `size`.
    BadDir { dir: u64, valid: u64, size: u64 },
    /// Entry `index` (`name` → `ino`) of folder `dir`.
    BadEntry { dir: u64, index: usize, name: String, ino: u64, fault: EntryFault },
    /// Allocated but in no folder. `links == 0`: a file removed while open
    /// whose last close never came (a crash): expected, and freed by repair.
    Unreachable { ino: u64, kind: Kind, links: u32 },
    /// The link count isn't what the tree says.
    LinkCount { ino: u64, found: u32, want: u32 },
    /// Block `block` is claimed again, by inode `ino` (not repairable).
    SharedBlock { block: u64, ino: u64 },
    /// Block `block` is used by inode `ino` but free in the bitmap.
    UsedButFree { block: u64, ino: u64 },
    /// `count` blocks from `start` are marked used but nothing uses them.
    Leaked { start: u64, count: u64 },
    /// `count` blocks of the metadata area are marked free.
    ReservedFree { count: u64 },
    /// The superblock's free block count.
    FreeBlocks { recorded: u64, counted: u64 },
    /// The superblock's free inode count.
    FreeInodes { recorded: u64, counted: u64 },
}

impl Problem {
    /// An orphan: an inode a crash left behind with no links. Freeing it
    /// is routine, not a repair anyone needs to hear about.
    pub fn is_orphan(&self) -> bool {
        matches!(self, Problem::Unreachable { links: 0, .. })
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Problem::BadRoot(why) => write!(f, "the root folder: {why}"),
            Problem::BadInode { ino, why } => write!(f, "inode {ino}: {why}"),
            Problem::BadSize { ino, size, blocks } => write!(f, "inode {ino}: size {size} exceeds its {blocks} blocks"),
            Problem::BadFlags { ino, flags } => write!(f, "inode {ino}: unknown flags or owner ({flags:#x})"),
            Problem::BadDir { dir, valid, size } => {
                write!(f, "dir {dir}: Corrupt (contents stop parsing at byte {valid} of {size})")
            }
            Problem::BadEntry { dir, name, ino, fault, .. } => {
                let what = match fault {
                    EntryFault::Dangling => "names a free inode".to_string(),
                    EntryFault::BadInode => "names an unreadable inode".to_string(),
                    EntryFault::BadName => "has an invalid name".to_string(),
                    EntryFault::Duplicate => "repeats a name".to_string(),
                    EntryFault::ReachedTwice => "reaches its inode a second time".to_string(),
                    EntryFault::WrongKind(k) => format!("has the wrong kind (the inode is a {k:?})"),
                };
                write!(f, "dir {dir}: entry {name:?} -> {ino} {what}")
            }
            Problem::Unreachable { ino, links: 0, .. } => write!(f, "inode {ino} allocated but unreachable (an orphan)"),
            Problem::Unreachable { ino, .. } => write!(f, "inode {ino} allocated but unreachable"),
            Problem::LinkCount { ino, found, want } => write!(f, "inode {ino}: {found} links, should be {want}"),
            Problem::SharedBlock { block, ino } => write!(f, "block {block} claimed again by inode {ino}"),
            Problem::UsedButFree { block, ino } => write!(f, "block {block} used by inode {ino} but free in the bitmap"),
            Problem::Leaked { start, count } => write!(f, "{count} blocks from {start} marked used but unreferenced (leak)"),
            Problem::ReservedFree { count } => write!(f, "{count} metadata blocks marked free"),
            Problem::FreeBlocks { recorded, counted } => write!(f, "free block count {recorded} != bitmap {counted}"),
            Problem::FreeInodes { recorded, counted } => write!(f, "free inode count {recorded} != bitmap {counted}"),
        }
    }
}

/// What [`Volume::check`] found.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub problems: Vec<Problem>,
    /// Problems beyond the first [`Report::LIMIT`] (counted, not kept).
    pub more: usize,
    pub files: u64,
    pub folders: u64,
    /// Inodes with a block another inode claims too (repair never frees them).
    conflicted: BTreeSet<u64>,
}

impl Report {
    pub const LIMIT: usize = 1000;

    pub fn is_clean(&self) -> bool {
        self.problems.is_empty()
    }

    /// Everything wrong is an orphan (see [`Problem::is_orphan`]).
    pub fn only_orphans(&self) -> bool {
        self.more == 0 && self.problems.iter().all(Problem::is_orphan)
    }

    fn add(&mut self, p: Problem) {
        if self.problems.len() < Self::LIMIT {
            self.problems.push(p);
        } else {
            self.more += 1;
        }
    }

    /// One problem per line (at most `max`, then how many more).
    pub fn summary(&self, max: usize) -> String {
        let mut out = String::new();
        for p in self.problems.iter().take(max) {
            out.push_str(&format!("{p}\n"));
        }
        let rest = self.problems.len().saturating_sub(max) + self.more;
        if rest > 0 {
            out.push_str(&format!("... and {rest} more\n"));
        }
        out
    }
}

/// An inode as stored, before any validation.
struct RawInode {
    kind: u16,
    links: u32,
    size: u64,
    extents: Vec<Extent>,
    extent_block: u64,
    flags: u16,
    owner: Option<Owner>,
}

/// A directory record as stored.
struct RawEntry {
    ino: u64,
    kind: u8,
    name: Vec<u8>,
}

/// Parses a folder's contents up to the first record that doesn't parse;
/// also returns how many bytes parsed.
fn parse_dir(bytes: &[u8]) -> (Vec<RawEntry>, usize) {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 10 <= bytes.len() {
        let kind = bytes[i + 8];
        let n = bytes[i + 9] as usize;
        if !(kind == Kind::File as u8 || kind == Kind::Dir as u8) || i + 10 + n > bytes.len() {
            break;
        }
        out.push(RawEntry { ino: get_u64(bytes, i), kind, name: bytes[i + 10..i + 10 + n].to_vec() });
        i += 10 + n;
    }
    (out, i)
}

fn kind_of(k: u16) -> Option<Kind> {
    match k {
        1 => Some(Kind::File),
        2 => Some(Kind::Dir),
        _ => None,
    }
}

/// A bitmap over `n` items.
struct Bits(Vec<u64>);

impl Bits {
    fn new(n: u64) -> Bits {
        Bits(vec![0; n.div_ceil(64) as usize])
    }
    fn get(&self, i: u64) -> bool {
        self.0[(i / 64) as usize] & (1 << (i % 64)) != 0
    }
    fn set(&mut self, i: u64) {
        self.0[(i / 64) as usize] |= 1 << (i % 64);
    }
}

/// The walk's state.
struct Walk {
    report: Report,
    /// Blocks some inode has claimed.
    refs: Bits,
    /// Inodes reached from a folder.
    reached: Bits,
    /// Inodes found unreadable (reported once).
    bad: BTreeSet<u64>,
}

impl<D: Disk> Volume<D> {
    /// An inode as stored; Err says why it can't be used.
    fn read_raw(&mut self, ino: u64) -> core::result::Result<RawInode, String> {
        let io = |e: Error| format!("{e:?}");
        let (blk, off) = self.inode_location(ino).map_err(io)?;
        let mut b = zero_block();
        self.meta_read(blk, &mut b).map_err(io)?;
        let r = &b[off..off + INODE_SIZE];
        let count = get_u32(r, 32) as usize;
        let extent_block = get_u64(r, 232);
        let mut extents = Vec::new();
        for i in 0..count.min(DIRECT_EXTENTS) {
            extents.push(Extent { start: get_u64(r, 40 + i * 16), len: get_u32(r, 48 + i * 16) });
        }
        let (data_start, blocks) = (self.sb.data_start, self.sb.blocks);
        let in_data = move |start: u64, len: u64| start >= data_start && start.checked_add(len).is_some_and(|e| e < blocks);
        if count > DIRECT_EXTENTS && count <= MAX_EXTENTS && in_data(extent_block, 1) {
            let mut eb = zero_block();
            self.meta_read(extent_block, &mut eb).map_err(io)?;
            for i in 0..count - DIRECT_EXTENTS {
                extents.push(Extent { start: get_u64(&eb[..], i * 16), len: get_u32(&eb[..], i * 16 + 8) });
            }
        }
        let flags = get_u16(r, 250);
        let owner = (flags & IFLAG_OWNED != 0).then(|| Owner {
            uid: get_u32(r, 240),
            gid: get_u32(r, 244),
            mode: get_u16(r, 248) & 0o7777,
        });
        let raw = RawInode { kind: get_u16(r, 0), links: get_u32(r, 4), size: get_u64(r, 8), extents, extent_block, flags, owner };
        // Validity, the way `read_inode` sees it (minus the size, which
        // repair can fix on its own).
        let why = if kind_of(raw.kind).is_none() {
            Some(format!("unknown kind {}", raw.kind))
        } else if count > MAX_EXTENTS {
            Some(format!("{count} extents"))
        } else if (count > DIRECT_EXTENTS || extent_block != 0) && !in_data(extent_block, 1) {
            Some(format!("extent block {extent_block} outside the data area"))
        } else {
            raw.extents
                .iter()
                .find(|e| !in_data(e.start, e.len as u64))
                .map(|e| format!("extent {}+{} outside the data area", e.start, e.len))
        };
        match why {
            Some(why) => Err(why),
            None => Ok(raw),
        }
    }

    /// Claims an inode's blocks for the walk.
    fn claim(&mut self, w: &mut Walk, ino: u64, raw: &RawInode) {
        let mut blocks: Vec<(u64, u64)> = raw.extents.iter().map(|e| (e.start, e.len as u64)).collect();
        if raw.extent_block != 0 {
            blocks.push((raw.extent_block, 1));
        }
        for (start, len) in blocks {
            for b in start..start + len {
                if w.refs.get(b) {
                    w.report.add(Problem::SharedBlock { block: b, ino });
                    w.report.conflicted.insert(ino);
                    continue;
                }
                w.refs.set(b);
                if !bit(&self.bmap, b) {
                    w.report.add(Problem::UsedButFree { block: b, ino });
                }
            }
        }
        let allocated: u64 = raw.extents.iter().map(|e| e.len as u64).sum();
        if raw.size.div_ceil(BLOCK as u64) > allocated {
            w.report.add(Problem::BadSize { ino, size: raw.size, blocks: allocated });
        }
        if raw.flags & !IFLAG_OWNED != 0 || raw.owner.is_some_and(|o| o.uid == u32::MAX || o.gid == u32::MAX) {
            w.report.add(Problem::BadFlags { ino, flags: raw.flags });
        }
    }

    /// Reads inode `ino` for the walk: None (and a report, once) if it
    /// can't be read.
    fn walk_inode(&mut self, w: &mut Walk, ino: u64) -> Option<RawInode> {
        match self.read_raw(ino) {
            Ok(raw) => Some(raw),
            Err(why) => {
                if w.bad.insert(ino) {
                    w.report.add(Problem::BadInode { ino, why });
                }
                None
            }
        }
    }

    /// Walks the tree below folder `top` (already reached and claimed).
    /// `tops`: unreachable inodes being collected; one found inside
    /// another's tree isn't a top after all.
    fn walk_from(&mut self, w: &mut Walk, top: u64, mut tops: Option<&mut BTreeSet<u64>>) {
        let mut stack = vec![top];
        while let Some(dir) = stack.pop() {
            w.report.folders += 1;
            let Some(raw) = self.walk_inode(w, dir) else { continue };
            let size = raw.size.min(raw.extents.iter().map(|e| e.len as u64).sum::<u64>() * BLOCK as u64);
            let mut bytes = vec![0u8; size as usize];
            if self.read_extents(&raw, &mut bytes).is_err() {
                w.report.add(Problem::BadInode { ino: dir, why: String::from("its contents can't be read") });
                continue;
            }
            let (entries, valid) = parse_dir(&bytes);
            if valid as u64 != raw.size {
                w.report.add(Problem::BadDir { dir, valid: valid as u64, size: raw.size });
            }
            let mut names = BTreeSet::new();
            for (index, e) in entries.into_iter().enumerate() {
                let name = String::from_utf8_lossy(&e.name).into_owned();
                let fault = |fault| Problem::BadEntry { dir, index, name: name.clone(), ino: e.ino, fault };
                let bad_name = name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0');
                if bad_name {
                    w.report.add(fault(EntryFault::BadName));
                    continue;
                }
                if !names.insert(e.name.clone()) {
                    w.report.add(fault(EntryFault::Duplicate));
                    continue;
                }
                if e.ino <= ROOT || e.ino >= self.sb.inodes || !bit(&self.imap, e.ino) {
                    w.report.add(fault(if e.ino == ROOT { EntryFault::ReachedTwice } else { EntryFault::Dangling }));
                    continue;
                }
                let Some(child) = self.walk_inode(w, e.ino) else {
                    w.report.add(fault(EntryFault::BadInode));
                    continue;
                };
                let kind = kind_of(child.kind).unwrap();
                if kind as u8 != e.kind {
                    w.report.add(fault(EntryFault::WrongKind(kind)));
                }
                if w.reached.get(e.ino) {
                    // A top met inside another's tree hangs here after all.
                    let was_top = e.ino != top && tops.as_deref_mut().is_some_and(|t| t.remove(&e.ino));
                    if !was_top {
                        w.report.add(fault(EntryFault::ReachedTwice));
                    }
                    continue;
                }
                w.reached.set(e.ino);
                self.claim(w, e.ino, &child);
                let want = if kind == Kind::Dir { 2 } else { 1 };
                if child.links != want {
                    w.report.add(Problem::LinkCount { ino: e.ino, found: child.links, want });
                }
                if kind == Kind::Dir {
                    stack.push(e.ino);
                } else {
                    w.report.files += 1;
                }
            }
        }
    }

    /// Reads the blocks an inode's extents name, into `buf`.
    fn read_extents(&mut self, raw: &RawInode, buf: &mut [u8]) -> Result<()> {
        let mut blk = zero_block();
        let mut done = 0;
        let mut n = 0u64;
        while done < buf.len() {
            let mut left = n;
            let pb = raw.extents.iter().find_map(|e| {
                if left < e.len as u64 {
                    Some(e.start + left)
                } else {
                    left -= e.len as u64;
                    None
                }
            });
            let take = (buf.len() - done).min(BLOCK);
            match pb {
                Some(pb) => {
                    self.meta_read(pb, &mut blk)?;
                    buf[done..done + take].copy_from_slice(&blk[..take]);
                }
                None => buf[done..done + take].fill(0),
            }
            done += take;
            n += 1;
        }
        Ok(())
    }

    /// Checks the whole volume and reports every problem found (see the
    /// module documentation). Files removed while still open on this mount
    /// are expected, not problems.
    pub fn check(&mut self) -> Report {
        let mut w = Walk {
            report: Report::default(),
            refs: Bits::new(self.sb.blocks),
            reached: Bits::new(self.sb.inodes),
            bad: BTreeSet::new(),
        };
        if !bit(&self.imap, ROOT) {
            w.report.add(Problem::BadRoot(String::from("free in the inode bitmap")));
            return w.report;
        }
        match self.walk_inode(&mut w, ROOT) {
            Some(raw) if raw.kind == Kind::Dir as u16 => {
                w.reached.set(ROOT);
                self.claim(&mut w, ROOT, &raw);
                if raw.links != 2 {
                    w.report.add(Problem::LinkCount { ino: ROOT, found: raw.links, want: 2 });
                }
                self.walk_from(&mut w, ROOT, None);
            }
            _ => {
                w.report.add(Problem::BadRoot(String::from("not a readable folder")));
                return w.report;
            }
        }
        // Allocated inodes no folder reached: the tops of what hangs loose.
        let loose: Vec<u64> = (ROOT + 1..self.sb.inodes).filter(|&i| bit(&self.imap, i) && !w.reached.get(i)).collect();
        let mut tops: BTreeSet<u64> = BTreeSet::new();
        for &ino in &loose {
            if w.reached.get(ino) {
                continue; // inside an earlier top's tree
            }
            let Some(raw) = self.walk_inode(&mut w, ino) else { continue };
            w.reached.set(ino);
            self.claim(&mut w, ino, &raw);
            tops.insert(ino);
            if raw.kind == Kind::Dir as u16 {
                self.walk_from(&mut w, ino, Some(&mut tops));
            } else {
                w.report.files += 1;
            }
        }
        for ino in tops {
            if self.orphans.contains(&ino) {
                continue; // removed while open on this mount: expected
            }
            let Ok(raw) = self.read_raw(ino) else { continue };
            let kind = kind_of(raw.kind).unwrap_or(Kind::File);
            w.report.add(Problem::Unreachable { ino, kind, links: raw.links });
        }
        // The bitmaps against what was found.
        let reserved = (0..self.sb.data_start).chain([self.sb.blocks - 1]).filter(|&b| !bit(&self.bmap, b)).count();
        if reserved > 0 {
            w.report.add(Problem::ReservedFree { count: reserved as u64 });
        }
        let mut b = self.sb.data_start;
        while b < self.sb.blocks - 1 {
            if bit(&self.bmap, b) && !w.refs.get(b) {
                let start = b;
                while b < self.sb.blocks - 1 && bit(&self.bmap, b) && !w.refs.get(b) {
                    b += 1;
                }
                w.report.add(Problem::Leaked { start, count: b - start });
            } else {
                b += 1;
            }
        }
        let free = (self.sb.data_start..self.sb.blocks - 1).filter(|&b| !bit(&self.bmap, b)).count() as u64;
        if free != self.sb.free_blocks {
            w.report.add(Problem::FreeBlocks { recorded: self.sb.free_blocks, counted: free });
        }
        let free_inodes = (0..self.sb.inodes).filter(|&i| !bit(&self.imap, i)).count() as u64;
        if free_inodes != self.sb.free_inodes {
            w.report.add(Problem::FreeInodes { recorded: self.sb.free_inodes, counted: free_inodes });
        }
        w.report
    }

    /// [`Volume::check`], as one string of every problem (or `Ok`).
    pub fn fsck(&mut self) -> core::result::Result<(), String> {
        let r = self.check();
        if r.is_clean() {
            Ok(())
        } else {
            let s = r.summary(20);
            Err(String::from(s.trim_end()).replace('\n', "; "))
        }
    }

    /// Repairs what [`Volume::check`] finds, where that is safe (see the
    /// module documentation), and commits. Returns what it found first and
    /// what is still wrong after (problems it can't repair, such as a block
    /// claimed by two files).
    pub fn repair(&mut self) -> Result<(Report, Report)> {
        self.commit()?;
        let first = self.check();
        let mut last = first.clone();
        for _ in 0..4 {
            if last.is_clean() || !last.problems.iter().any(repairable) {
                break;
            }
            self.repair_once(&last)?;
            self.commit()?;
            last = self.check();
        }
        Ok((first, last))
    }

    fn repair_once(&mut self, r: &Report) -> Result<()> {
        let conflicted = &r.conflicted;
        // Folders first: bad entries go, contents are cut where they stop
        // parsing, kinds are corrected.
        let mut dirs: BTreeMap<u64, Vec<&Problem>> = BTreeMap::new();
        for p in &r.problems {
            match p {
                Problem::BadDir { dir, .. } | Problem::BadEntry { dir, .. } => dirs.entry(*dir).or_default().push(p),
                _ => {}
            }
        }
        for (dir, ps) in dirs {
            self.op(|v| {
                let raw = v.read_raw(dir).map_err(|_| Error::Corrupt)?;
                let size = raw.size.min(raw.extents.iter().map(|e| e.len as u64).sum::<u64>() * BLOCK as u64);
                let mut bytes = vec![0u8; size as usize];
                v.read_extents(&raw, &mut bytes)?;
                let (entries, _) = parse_dir(&bytes);
                let mut keep = Vec::new();
                for (i, e) in entries.into_iter().enumerate() {
                    let mut kind = if e.kind == Kind::Dir as u8 { Kind::Dir } else { Kind::File };
                    let mut drop = false;
                    for p in &ps {
                        if let Problem::BadEntry { index, fault, .. } = p {
                            if *index == i {
                                match fault {
                                    EntryFault::WrongKind(k) => kind = *k,
                                    _ => drop = true,
                                }
                            }
                        }
                    }
                    if !drop {
                        keep.push(DirEntry { name: String::from_utf8_lossy(&e.name).into_owned(), ino: e.ino, kind });
                    }
                }
                // The size may claim more than the blocks hold: fix it first.
                if raw.size != size {
                    let mut inode = v.read_inode_unchecked(dir)?;
                    inode.size = size;
                    v.write_inode(dir, &inode)?;
                }
                v.write_dir(dir, &keep)
            })?;
        }
        for p in &r.problems {
            match *p {
                Problem::BadInode { ino, .. } if ino != ROOT && bit(&self.imap, ino) => {
                    // Its blocks can't be trusted: the next pass frees those
                    // nothing else uses (as leaks).
                    self.op(|v| {
                        v.write_inode(ino, &Inode::new(0, 0, 0, None))?;
                        v.free_inode(ino);
                        Ok(())
                    })?;
                }
                Problem::BadSize { ino, blocks, .. } => self.op(|v| {
                    let mut inode = v.read_inode_unchecked(ino)?;
                    inode.size = blocks * BLOCK as u64;
                    v.write_inode(ino, &inode)
                })?,
                Problem::BadFlags { ino, .. } => self.op(|v| {
                    let mut inode = v.read_inode_unchecked(ino)?;
                    if inode.owner.is_some_and(|o| o.uid == u32::MAX || o.gid == u32::MAX) {
                        inode.owner = None;
                    }
                    // (Writing it back keeps only the flags this version knows.)
                    v.write_inode(ino, &inode)
                })?,
                Problem::LinkCount { ino, want, .. } => self.op(|v| {
                    let mut inode = v.read_inode_unchecked(ino)?;
                    inode.links = want;
                    v.write_inode(ino, &inode)
                })?,
                Problem::Unreachable { ino, links: 0, .. } if !conflicted.contains(&ino) => {
                    self.op(|v| {
                        let mut inode = v.read_inode_unchecked(ino)?;
                        v.shrink(&mut inode, 0);
                        v.write_inode(ino, &Inode::new(0, 0, 0, None))?;
                        v.free_inode(ino);
                        Ok(())
                    })?
                }
                Problem::Unreachable { ino, kind, .. } => self.op(|v| v.reattach(ino, kind))?,
                Problem::UsedButFree { block, .. } => self.op(|v| {
                    v.set_map_bit(false, block, true);
                    Ok(())
                })?,
                Problem::Leaked { start, count } => self.op(|v| {
                    // Not reusable until this commits, like any freed block.
                    v.free_extent(Extent { start, len: count as u32 });
                    Ok(())
                })?,
                Problem::ReservedFree { .. } => self.op(|v| {
                    for b in (0..v.sb.data_start).chain([v.sb.blocks - 1]) {
                        if !bit(&v.bmap, b) {
                            v.set_map_bit(false, b, true);
                        }
                    }
                    Ok(())
                })?,
                _ => {}
            }
        }
        // Last, the free counts, from the bitmaps as they now are.
        self.op(|v| {
            v.sb.free_blocks = (v.sb.data_start..v.sb.blocks - 1).filter(|&b| !bit(&v.bmap, b)).count() as u64;
            v.sb.free_inodes = (0..v.sb.inodes).filter(|&i| !bit(&v.imap, i)).count() as u64;
            v.dirty_super();
            Ok(())
        })
    }

    /// Puts unreachable `ino` back, as `/lost+found/#<ino>`.
    fn reattach(&mut self, ino: u64, kind: Kind) -> Result<()> {
        let lf = match self.lookup(ROOT, LOST_AND_FOUND) {
            Ok(i) if self.stat(i)?.kind == Kind::Dir => i,
            _ => {
                let mut name = String::from(LOST_AND_FOUND);
                while self.lookup(ROOT, &name).is_ok() {
                    name.push('_');
                }
                self.create_owned(ROOT, &name, Kind::Dir, None)?
            }
        };
        let mut entries = self.readdir(lf)?;
        let mut name = format!("#{ino}");
        while entries.iter().any(|e| e.name == name) {
            name.push('_');
        }
        let mut inode = self.read_inode_unchecked(ino)?;
        inode.links = if kind == Kind::Dir { 2 } else { 1 };
        self.write_inode(ino, &inode)?;
        entries.push(DirEntry { name, ino, kind });
        self.write_dir(lf, &entries)
    }

    /// An inode with its size clamped to its blocks (repair reads inodes
    /// `read_inode` would refuse for that).
    fn read_inode_unchecked(&mut self, ino: u64) -> Result<Inode> {
        let raw = self.read_raw(ino).map_err(|_| Error::Corrupt)?;
        let blocks: u64 = raw.extents.iter().map(|e| e.len as u64).sum();
        let (blk, off) = self.inode_location(ino)?;
        let mut b = zero_block();
        self.meta_read(blk, &mut b)?;
        let r = &b[off..off + INODE_SIZE];
        Ok(Inode {
            kind: raw.kind,
            links: raw.links,
            size: raw.size.min(blocks * BLOCK as u64),
            ctime: get_u64(r, 16),
            mtime: get_u64(r, 24),
            extents: raw.extents,
            extent_block: raw.extent_block,
            owner: raw.owner,
        })
    }
}

fn repairable(p: &Problem) -> bool {
    !matches!(p, Problem::BadRoot(_) | Problem::SharedBlock { .. })
}
