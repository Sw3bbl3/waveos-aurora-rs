//! AuroraFS — the WaveOS Aurora filesystem.
//!
//! A small extent-based filesystem with a physical write-ahead journal for
//! metadata (ordered mode, like ext4's default):
//!
//! ```text
//! block 0            superblock (backup copy in the last block)
//! journal            descriptor · block images · commit record
//! block bitmap       1 bit per block
//! inode bitmap       1 bit per inode
//! inode table        256-byte inodes, 16 per block
//! data               file contents and directories
//! ```
//!
//! **Owners** (feature [`FEATURE_OWNERS`]). Each inode can carry an owner, a
//! group and Unix permission bits (bytes 240–251, flagged by bit 0 of the
//! inode flags). Inodes without them — every inode on a volume made before
//! owners existed — belong to the volume's *default owner*, kept with the
//! feature bits in a checksummed extension of the superblock (bytes
//! 188–255). The extension is a compatible feature: an older kernel ignores
//! it, still mounts the volume, and the inodes it rewrites simply fall back
//! to the default owner.
//!
//! All metadata changes (superblock, bitmaps, inodes, directory contents) are
//! collected in a transaction and reach their home location only after the
//! transaction has been written to the journal and flushed. File data is
//! written in place and flushed *before* the commit record, so metadata never
//! points at unwritten data. Blocks freed in a transaction are not reused
//! until it commits. After a crash, `mount` replays the last committed
//! transaction, so the on-disk structure is always consistent.
//!
//! **Operations are atomic.** Each public call that changes the volume runs
//! as one operation: a transaction is committed only between operations,
//! never in the middle of one, and an operation that fails part-way (a full
//! disk, a read error) puts back every change it made. After a power cut the
//! volume holds the tree as it was after some operation, and everything up
//! to the last [`Volume::commit`] (sync) is there. `docs/developer/aurorafs.md`
//! has the whole list of guarantees.
//!
//! **Orphans.** A file removed while open ([`Volume::open_inode`]) keeps its
//! inode, with no links, until it is closed. After a crash such inodes are
//! freed by [`Volume::repair`], which the kernel runs at mount when the
//! volume was not cleanly unmounted ([`Volume::was_clean`]).
//!
//! This crate is `no_std`; the kernel and the host tools (`xtask mkfs`) share it.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod check;
mod crc;
#[cfg(test)]
mod check_tests;
#[cfg(test)]
mod concurrent;
#[cfg(test)]
mod owner_tests;
#[cfg(test)]
mod regressions;
#[cfg(test)]
mod stress;
#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub use check::{EntryFault, Problem, Report};
pub use crc::crc32;
use crc::{Crc32, Crc32c};

pub const BLOCK: usize = 4096;
pub const MAGIC: [u8; 8] = *b"WAVEFS01";
pub const VERSION: u32 = 1;
pub const ROOT: u64 = 1;
/// Superblock feature: inodes may carry an owner, a group and a mode.
pub const FEATURE_OWNERS: u32 = 1;
/// Feature bits this version understands.
const KNOWN_FEATURES: u32 = FEATURE_OWNERS;
/// The default owner of a volume made by `format` (the first account).
pub const FIRST_UID: u32 = 1000;
pub const FIRST_GID: u32 = 1000;
/// Inode flag: bytes 240–251 hold an owner, a group and a mode.
const IFLAG_OWNED: u16 = 1;
pub const NAME_MAX: usize = 255;
/// The largest file, in bytes.
pub const MAX_FILE_SIZE: u64 = 1 << 40;
/// How much a growing `truncate` zero-fills per write, so the gap never has
/// to fit in memory.
const ZERO_FILL: usize = 64 * 1024;
/// GPT partition type for AuroraFS volumes: 57415645-4653-4175-726F-72612D465331 (on-disk byte order).
pub const PARTITION_TYPE: [u8; 16] =
    [0x45, 0x56, 0x41, 0x57, 0x53, 0x46, 0x75, 0x41, 0x72, 0x6F, 0x72, 0x61, 0x2D, 0x46, 0x53, 0x31];

const INODE_SIZE: usize = 256;
const INODES_PER_BLOCK: u64 = (BLOCK / INODE_SIZE) as u64;
const DIRECT_EXTENTS: usize = 12;
const EXTENTS_PER_BLOCK: usize = BLOCK / 16;
const MAX_EXTENTS: usize = DIRECT_EXTENTS + EXTENTS_PER_BLOCK;
const DESC_MAGIC: [u8; 8] = *b"WJRNDESC";
const COMMIT_MAGIC: [u8; 8] = *b"WJRNCMIT";
const DESC_CAPACITY: usize = (BLOCK - 24) / 8;
/// Commit record, byte 28: bytes 24..28 hold a CRC-32C of the transaction
/// (see `write_journal`).
const JOURNAL_CRC32C: u32 = 0x4332_3343; // "C32C"
/// Superblock extension, byte 200: the volume was cleanly unmounted.
const CLEAN_MAGIC: u32 = 0x4E41_454C; // "LEAN"
/// The folder `repair` puts unreachable files and folders back into.
pub const LOST_AND_FOUND: &str = "lost+found";

pub type Block = [u8; BLOCK];

/// Storage underneath a volume, in 4 KiB blocks.
pub trait Disk {
    fn blocks(&self) -> u64;
    fn read(&mut self, block: u64, buf: &mut Block) -> Result<()>;
    /// Reads `buf.len() / BLOCK` consecutive blocks from `block`. File data
    /// is read this way, a run at a time; a disk can do it in one request.
    fn read_run(&mut self, block: u64, buf: &mut [u8]) -> Result<()> {
        let mut blk = zero_block();
        for (i, chunk) in buf.chunks_mut(BLOCK).enumerate() {
            self.read(block + i as u64, &mut blk)?;
            chunk.copy_from_slice(&blk[..chunk.len()]);
        }
        Ok(())
    }
    fn write(&mut self, block: u64, buf: &Block) -> Result<()>;
    /// Makes all previous writes durable.
    fn flush(&mut self) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exists,
    NotDir,
    IsDir,
    NotEmpty,
    NoSpace,
    Io,
    Corrupt,
    NameTooLong,
    Invalid,
    TooBig,
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File = 1,
    Dir = 2,
}

#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub links: u32,
    pub owner: Owner,
}

/// Who a file belongs to and what others may do with it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits (`0o7777`: set-id, sticky, owner, group, others).
    pub mode: u16,
}

impl Owner {
    /// The usual mode of something new: `rwxr-xr-x` folders, `rw-r--r--` files.
    pub fn default_mode(kind: Kind) -> u16 {
        match kind {
            Kind::Dir => 0o755,
            Kind::File => 0o644,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    pub name: String,
    pub ino: u64,
    pub kind: Kind,
}

// ------------------------------------------------------------ encoding

fn get_u16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn get_u32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn get_u64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}
fn put_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes())
}
fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes())
}
fn put_u64(b: &mut [u8], o: usize, v: u64) {
    b[o..o + 8].copy_from_slice(&v.to_le_bytes())
}

fn zero_block() -> Box<Block> {
    Box::new([0u8; BLOCK])
}

#[derive(Clone, Debug)]
struct Superblock {
    blocks: u64,
    inodes: u64,
    journal_start: u64,
    journal_blocks: u64,
    bbitmap_start: u64,
    bbitmap_blocks: u64,
    ibitmap_start: u64,
    ibitmap_blocks: u64,
    itable_start: u64,
    itable_blocks: u64,
    data_start: u64,
    free_blocks: u64,
    free_inodes: u64,
    uuid: [u8; 16],
    label: [u8; 32],
    mount_count: u64,
    journal_seq: u64,
    /// `FEATURE_*` bits (in the checksummed extension, bytes 188–255).
    features: u32,
    /// Owner of inodes that carry none.
    default_uid: u32,
    default_gid: u32,
    /// Cleanly unmounted (extension byte 200). Only `unmount` writes it;
    /// every other commit clears it, so an older kernel (which writes zero
    /// there) leaves the volume marked as not clean: the safe direction.
    clean: bool,
}

/// The superblock extension: bytes 188..252 with a CRC32 at 252. A volume
/// written before it existed (or by a kernel that doesn't know it) has a
/// zero or mismatching checksum there, and the extension reads as empty.
const EXT_START: usize = 188;
const EXT_CRC: usize = 252;

impl Superblock {
    fn encode(&self) -> Box<Block> {
        let mut b = zero_block();
        b[0..8].copy_from_slice(&MAGIC);
        put_u32(&mut b[..], 8, VERSION);
        put_u32(&mut b[..], 12, BLOCK as u32);
        let fields = [
            self.blocks,
            self.inodes,
            self.journal_start,
            self.journal_blocks,
            self.bbitmap_start,
            self.bbitmap_blocks,
            self.ibitmap_start,
            self.ibitmap_blocks,
            self.itable_start,
            self.itable_blocks,
            self.data_start,
            self.free_blocks,
            self.free_inodes,
        ];
        for (i, f) in fields.iter().enumerate() {
            put_u64(&mut b[..], 16 + i * 8, *f);
        }
        b[120..136].copy_from_slice(&self.uuid);
        b[136..168].copy_from_slice(&self.label);
        put_u64(&mut b[..], 168, self.mount_count);
        put_u64(&mut b[..], 176, self.journal_seq);
        let crc = crc32(&b[..184]);
        put_u32(&mut b[..], 184, crc);
        if self.features != 0 || self.clean {
            put_u32(&mut b[..], EXT_START, self.features);
            put_u32(&mut b[..], EXT_START + 4, self.default_uid);
            put_u32(&mut b[..], EXT_START + 8, self.default_gid);
            if self.clean {
                put_u32(&mut b[..], EXT_START + 12, CLEAN_MAGIC);
            }
            let crc = crc32(&b[EXT_START..EXT_CRC]);
            put_u32(&mut b[..], EXT_CRC, crc);
        }
        b
    }

    fn decode(b: &Block) -> Result<Superblock> {
        if b[0..8] != MAGIC || get_u32(b, 8) != VERSION || get_u32(b, 12) != BLOCK as u32 {
            return Err(Error::Corrupt);
        }
        if crc32(&b[..184]) != get_u32(b, 184) {
            return Err(Error::Corrupt);
        }
        let f = |i: usize| get_u64(b, 16 + i * 8);
        let sb = Superblock {
            blocks: f(0),
            inodes: f(1),
            journal_start: f(2),
            journal_blocks: f(3),
            bbitmap_start: f(4),
            bbitmap_blocks: f(5),
            ibitmap_start: f(6),
            ibitmap_blocks: f(7),
            itable_start: f(8),
            itable_blocks: f(9),
            data_start: f(10),
            free_blocks: f(11),
            free_inodes: f(12),
            uuid: b[120..136].try_into().unwrap(),
            label: b[136..168].try_into().unwrap(),
            mount_count: get_u64(b, 168),
            journal_seq: get_u64(b, 176),
            features: 0,
            default_uid: 0,
            default_gid: 0,
            clean: false,
        };
        let mut sb = sb;
        let ext_crc = get_u32(b, EXT_CRC);
        if ext_crc != 0 && crc32(&b[EXT_START..EXT_CRC]) == ext_crc {
            // Unknown feature bits are ignored: every feature so far is
            // compatible (an older reader may mount the volume).
            sb.features = get_u32(b, EXT_START) & KNOWN_FEATURES;
            sb.default_uid = get_u32(b, EXT_START + 4);
            sb.default_gid = get_u32(b, EXT_START + 8);
            sb.clean = get_u32(b, EXT_START + 12) == CLEAN_MAGIC;
        }
        if !sb.geometry_ok() {
            return Err(Error::Corrupt);
        }
        Ok(sb)
    }

    /// The regions `layout` creates: in order, without overlap, inside the
    /// volume, and with bitmaps and an inode table big enough for what they
    /// describe. A volume from another machine is untrusted input, and the
    /// checksum only proves the superblock was written whole.
    fn geometry_ok(&self) -> bool {
        let bits = (BLOCK * 8) as u64;
        let end = |start: u64, len: u64| start.checked_add(len);
        let regions = [
            (self.journal_start, self.journal_blocks),
            (self.bbitmap_start, self.bbitmap_blocks),
            (self.ibitmap_start, self.ibitmap_blocks),
            (self.itable_start, self.itable_blocks),
        ];
        let mut next = 1;
        for (start, len) in regions {
            match end(start, len) {
                Some(e) if start >= next => next = e,
                _ => return false,
            }
        }
        self.blocks >= 64
            && next <= self.data_start
            // The last block holds the backup superblock.
            && self.data_start < self.blocks - 1
            && self.journal_blocks >= 2
            && self.inodes > ROOT
            && self.bbitmap_blocks.checked_mul(bits).is_some_and(|n| n >= self.blocks)
            && self.ibitmap_blocks.checked_mul(bits).is_some_and(|n| n >= self.inodes)
            && self.itable_blocks.checked_mul(INODES_PER_BLOCK).is_some_and(|n| n >= self.inodes)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Extent {
    start: u64,
    len: u32,
}

#[derive(Clone, Debug)]
struct Inode {
    kind: u16,
    links: u32,
    size: u64,
    ctime: u64,
    mtime: u64,
    extents: Vec<Extent>,
    /// Block holding extents beyond the first 12 (0 = none).
    extent_block: u64,
    /// Its own owner, group and mode (None: the volume's default owner).
    owner: Option<Owner>,
}

impl Inode {
    fn new(kind: u16, links: u32, t: u64, owner: Option<Owner>) -> Inode {
        Inode { kind, links, size: 0, ctime: t, mtime: t, extents: Vec::new(), extent_block: 0, owner }
    }

    fn kind(&self) -> Result<Kind> {
        match self.kind {
            1 => Ok(Kind::File),
            2 => Ok(Kind::Dir),
            _ => Err(Error::Corrupt),
        }
    }

    fn allocated_blocks(&self) -> u64 {
        self.extents.iter().map(|e| e.len as u64).sum()
    }

    /// Physical block for file block `n`.
    fn map(&self, mut n: u64) -> Option<u64> {
        for e in &self.extents {
            if n < e.len as u64 {
                return Some(e.start + n);
            }
            n -= e.len as u64;
        }
        None
    }
}

// ------------------------------------------------------------ the volume

pub struct Volume<D: Disk> {
    disk: D,
    sb: Superblock,
    /// Logical block bitmap (what gets written to disk).
    bmap: Vec<u8>,
    imap: Vec<u8>,
    dirty_bmap: Vec<bool>,
    dirty_imap: Vec<bool>,
    /// Metadata block images waiting for the next commit (also read-through).
    txn: BTreeMap<u64, Box<Block>>,
    /// Extents freed in the running transaction; not reusable until it commits.
    pending_free: Vec<Extent>,
    alloc_hint: u64,
    now: fn() -> u64,
    /// The running operation's undo log (see [`Volume::op`]).
    undo: Option<Undo>,
    /// Inodes held open (`open_inode`), and how many times.
    open: BTreeMap<u64, u32>,
    /// Inodes removed while open: no links, freed at their last close.
    orphans: BTreeSet<u64>,
    /// The superblock said "cleanly unmounted" when this mount began.
    was_clean: bool,
}

/// What the running operation changed, so that it can be put back if the
/// operation fails part-way.
struct Undo {
    /// Each block's pending image before the operation first changed it
    /// (None: it had none).
    txn: BTreeMap<u64, Option<Box<Block>>>,
    /// Bitmap bytes before they changed: (inode bitmap?, byte, old value).
    bits: Vec<(bool, usize, u8)>,
    dirty_bmap: Vec<bool>,
    dirty_imap: Vec<bool>,
    sb: Superblock,
    pending_free: usize,
    alloc_hint: u64,
    orphans: BTreeSet<u64>,
}

fn bit(map: &[u8], i: u64) -> bool {
    map[(i / 8) as usize] & (1 << (i % 8)) != 0
}
fn set_bit(map: &mut [u8], i: u64, v: bool) {
    let b = &mut map[(i / 8) as usize];
    if v {
        *b |= 1 << (i % 8);
    } else {
        *b &= !(1 << (i % 8));
    }
}

fn layout(blocks: u64) -> Result<Superblock> {
    if blocks < 64 {
        return Err(Error::NoSpace);
    }
    let bits = (BLOCK * 8) as u64;
    let inodes = (blocks / 4).max(64).next_multiple_of(INODES_PER_BLOCK);
    let journal_blocks = (blocks / 16).clamp(32, 1024);
    let bbitmap_blocks = blocks.div_ceil(bits);
    let ibitmap_blocks = inodes.div_ceil(bits);
    let itable_blocks = inodes / INODES_PER_BLOCK;
    let journal_start = 1;
    let bbitmap_start = journal_start + journal_blocks;
    let ibitmap_start = bbitmap_start + bbitmap_blocks;
    let itable_start = ibitmap_start + ibitmap_blocks;
    let data_start = itable_start + itable_blocks;
    if data_start + 8 >= blocks {
        return Err(Error::NoSpace);
    }
    Ok(Superblock {
        blocks,
        inodes,
        journal_start,
        journal_blocks,
        bbitmap_start,
        bbitmap_blocks,
        ibitmap_start,
        ibitmap_blocks,
        itable_start,
        itable_blocks,
        data_start,
        free_blocks: 0,
        free_inodes: 0,
        uuid: [0; 16],
        label: [0; 32],
        mount_count: 0,
        journal_seq: 0,
        features: FEATURE_OWNERS,
        default_uid: FIRST_UID,
        default_gid: FIRST_GID,
        clean: false,
    })
}

impl<D: Disk> Volume<D> {
    // ----------------------------------------------------- format / mount

    /// Creates an empty filesystem on `disk`.
    pub fn format(mut disk: D, label: &str, uuid: [u8; 16], now: fn() -> u64) -> Result<Volume<D>> {
        let mut sb = layout(disk.blocks())?;
        sb.uuid = uuid;
        let n = label.len().min(32);
        sb.label[..n].copy_from_slice(&label.as_bytes()[..n]);
        let zero = zero_block();
        for b in 0..sb.data_start {
            disk.write(b, &zero)?;
        }
        let mut vol = Volume {
            bmap: vec![0; (sb.bbitmap_blocks as usize) * BLOCK],
            imap: vec![0; (sb.ibitmap_blocks as usize) * BLOCK],
            dirty_bmap: vec![true; sb.bbitmap_blocks as usize],
            dirty_imap: vec![true; sb.ibitmap_blocks as usize],
            txn: BTreeMap::new(),
            pending_free: Vec::new(),
            alloc_hint: sb.data_start,
            now,
            undo: None,
            open: BTreeMap::new(),
            orphans: BTreeSet::new(),
            was_clean: true,
            disk,
            sb,
        };
        // Metadata region and the backup superblock are permanently in use.
        for b in 0..vol.sb.data_start {
            set_bit(&mut vol.bmap, b, true);
        }
        set_bit(&mut vol.bmap, vol.sb.blocks - 1, true);
        // Bits past the end of the device are "used" so they are never allocated.
        for b in vol.sb.blocks..(vol.sb.bbitmap_blocks * BLOCK as u64 * 8) {
            set_bit(&mut vol.bmap, b, true);
        }
        for i in vol.sb.inodes..(vol.sb.ibitmap_blocks * BLOCK as u64 * 8) {
            set_bit(&mut vol.imap, i, true);
        }
        vol.sb.free_blocks = vol.sb.blocks - vol.sb.data_start - 1;
        // Inode 0 is reserved; inode 1 is the root directory.
        set_bit(&mut vol.imap, 0, true);
        set_bit(&mut vol.imap, ROOT, true);
        vol.sb.free_inodes = vol.sb.inodes - 2;
        let t = now();
        vol.write_inode(ROOT, &Inode::new(Kind::Dir as u16, 2, t, None))?;
        // Bitmaps can be larger than the journal: write them in place.
        for i in 0..vol.sb.bbitmap_blocks as usize {
            let mut img = zero_block();
            img.copy_from_slice(&vol.bmap[i * BLOCK..(i + 1) * BLOCK]);
            vol.disk.write(vol.sb.bbitmap_start + i as u64, &img)?;
            vol.dirty_bmap[i] = false;
        }
        for i in 0..vol.sb.ibitmap_blocks as usize {
            let mut img = zero_block();
            img.copy_from_slice(&vol.imap[i * BLOCK..(i + 1) * BLOCK]);
            vol.disk.write(vol.sb.ibitmap_start + i as u64, &img)?;
            vol.dirty_imap[i] = false;
        }
        vol.commit()?;
        let backup = vol.sb.encode();
        vol.disk.write(vol.sb.blocks - 1, &backup)?;
        vol.disk.flush()?;
        Ok(vol)
    }

    /// Opens an existing filesystem, replaying the journal if needed.
    pub fn mount(mut disk: D, now: fn() -> u64) -> Result<Volume<D>> {
        let mut b = zero_block();
        disk.read(0, &mut b)?;
        let sb = match Superblock::decode(&b) {
            Ok(sb) => sb,
            Err(_) => {
                let last = disk.blocks().checked_sub(1).ok_or(Error::Corrupt)?;
                disk.read(last, &mut b)?;
                Superblock::decode(&b)?
            }
        };
        if sb.blocks > disk.blocks() {
            return Err(Error::Corrupt);
        }
        let mut vol = Volume {
            bmap: Vec::new(),
            imap: Vec::new(),
            dirty_bmap: vec![false; sb.bbitmap_blocks as usize],
            dirty_imap: vec![false; sb.ibitmap_blocks as usize],
            txn: BTreeMap::new(),
            pending_free: Vec::new(),
            alloc_hint: sb.data_start,
            now,
            undo: None,
            open: BTreeMap::new(),
            orphans: BTreeSet::new(),
            was_clean: false,
            disk,
            sb,
        };
        vol.replay()?;
        // The superblock may have been replayed: reload it.
        let mut b = zero_block();
        vol.disk.read(0, &mut b)?;
        vol.sb = Superblock::decode(&b)?;
        if vol.sb.blocks > vol.disk.blocks() {
            return Err(Error::Corrupt);
        }
        vol.dirty_bmap = vec![false; vol.sb.bbitmap_blocks as usize];
        vol.dirty_imap = vec![false; vol.sb.ibitmap_blocks as usize];
        vol.bmap = vol.load_region(vol.sb.bbitmap_start, vol.sb.bbitmap_blocks)?;
        vol.imap = vol.load_region(vol.sb.ibitmap_start, vol.sb.ibitmap_blocks)?;
        vol.was_clean = vol.sb.clean;
        // Mounted: not clean until `unmount` says so.
        vol.sb.clean = false;
        vol.sb.mount_count += 1;
        vol.dirty_super();
        vol.commit()?;
        Ok(vol)
    }

    fn load_region(&mut self, start: u64, count: u64) -> Result<Vec<u8>> {
        let mut out = vec![0u8; count as usize * BLOCK];
        let mut b = zero_block();
        for i in 0..count {
            self.disk.read(start + i, &mut b)?;
            out[i as usize * BLOCK..(i as usize + 1) * BLOCK].copy_from_slice(&b[..]);
        }
        Ok(out)
    }

    pub fn into_disk(self) -> D {
        self.disk
    }

    /// The volume had been cleanly unmounted ([`Volume::unmount`]) when it
    /// was mounted. If not, the kernel checks it ([`Volume::check`]) and
    /// repairs what it safely can ([`Volume::repair`]).
    pub fn was_clean(&self) -> bool {
        self.was_clean
    }

    /// Commits everything and marks the volume cleanly unmounted, so that
    /// the next mount needs no check. Nothing may be open: files removed
    /// while open are freed by the next mount's repair instead, and the
    /// volume is then left marked as not clean. Changes made after this
    /// clear the mark again with their commit.
    pub fn unmount(&mut self) -> Result<()> {
        self.commit()?;
        if !self.orphans.is_empty() {
            return Ok(());
        }
        self.sb.clean = true;
        self.dirty_super();
        let r = self.commit();
        self.sb.clean = false;
        r
    }

    pub fn disk(&mut self) -> &mut D {
        &mut self.disk
    }

    pub fn label(&self) -> String {
        let end = self.sb.label.iter().position(|&b| b == 0).unwrap_or(32);
        String::from_utf8_lossy(&self.sb.label[..end]).into_owned()
    }

    /// (total bytes, free bytes)
    pub fn space(&self) -> (u64, u64) {
        (self.sb.blocks * BLOCK as u64, self.sb.free_blocks * BLOCK as u64)
    }

    pub fn has_pending(&self) -> bool {
        !self.txn.is_empty() || self.dirty_bmap.iter().any(|&d| d) || self.dirty_imap.iter().any(|&d| d)
    }

    // ------------------------------------------------------- journaling

    fn meta_read(&mut self, block: u64, buf: &mut Block) -> Result<()> {
        match self.txn.get(&block) {
            Some(img) => {
                buf.copy_from_slice(&img[..]);
                Ok(())
            }
            None => self.disk.read(block, buf),
        }
    }

    /// Notes `block`'s pending image before the running operation changes it.
    fn note_txn(&mut self, block: u64) {
        if let Some(u) = self.undo.as_mut() {
            if let alloc::collections::btree_map::Entry::Vacant(e) = u.txn.entry(block) {
                e.insert(self.txn.get(&block).cloned());
            }
        }
    }

    fn meta_write(&mut self, block: u64, data: &Block) {
        self.note_txn(block);
        match self.txn.get_mut(&block) {
            Some(img) => img.copy_from_slice(data),
            None => {
                let mut img = zero_block();
                img.copy_from_slice(data);
                self.txn.insert(block, img);
            }
        }
    }

    fn dirty_super(&mut self) {
        let img = self.sb.encode();
        self.meta_write(0, &img);
    }

    /// Writes all pending metadata atomically: journal, then home locations.
    /// When it returns, every operation before it is durable.
    pub fn commit(&mut self) -> Result<()> {
        // Never in the middle of an operation: that is what keeps them atomic.
        debug_assert!(self.undo.is_none(), "commit inside an operation");
        self.stage_bitmaps();
        if self.txn.is_empty() {
            self.pending_free.clear();
            return self.disk.flush();
        }
        // (Operations keep the transaction within the journal; only
        // `enable_owners`-style callers outside one could overflow it.)
        if self.txn_blocks() > self.journal_capacity() {
            return Err(Error::TooBig);
        }
        self.sb.journal_seq += 1;
        self.dirty_super();
        let blocks: Vec<u64> = self.txn.keys().copied().collect();
        // 1. File data (written in place) must be durable before the commit record.
        self.disk.flush()?;
        // 2. Journal: descriptor, images, commit record.
        self.write_journal(&blocks)?;
        self.disk.flush()?;
        // 3. Checkpoint: copy images home. (Durable by the next commit's first
        //    flush; until then, a crash is repaired by replaying the journal.)
        self.checkpoint()
    }

    /// Writes the transaction (blocks `blocks` of it) to the journal:
    /// descriptor, images, commit record. The commit record carries two
    /// checksums of descriptor and images: CRC-32, which kernels before
    /// 0.8.1 check, and CRC-32C (marked by `JOURNAL_CRC32C` at byte 28),
    /// which this version also requires when the mark is there. CRC-32 alone
    /// cannot tell one superblock image from another: a superblock ends its
    /// checked part with its own CRC-32, and CRC-32 over data followed by
    /// its CRC-32 is the same for all data, so a new superblock that a power
    /// cut left in the slot of an old transaction's passed for the old one.
    fn write_journal(&mut self, blocks: &[u64]) -> Result<()> {
        let mut desc = zero_block();
        desc[0..8].copy_from_slice(&DESC_MAGIC);
        put_u64(&mut desc[..], 8, self.sb.journal_seq);
        put_u32(&mut desc[..], 16, blocks.len() as u32);
        for (i, b) in blocks.iter().enumerate() {
            put_u64(&mut desc[..], 24 + i * 8, *b);
        }
        let (mut c32, mut c32c) = (Crc32::new(), Crc32c::new());
        c32.update(&desc[..]);
        c32c.update(&desc[..]);
        let j = self.sb.journal_start;
        self.disk.write(j, &desc)?;
        for (i, b) in blocks.iter().enumerate() {
            let img = &self.txn[b];
            c32.update(&img[..]);
            c32c.update(&img[..]);
            self.disk.write(j + 1 + i as u64, img)?;
        }
        let mut cmt = zero_block();
        cmt[0..8].copy_from_slice(&COMMIT_MAGIC);
        put_u64(&mut cmt[..], 8, self.sb.journal_seq);
        put_u32(&mut cmt[..], 16, blocks.len() as u32);
        put_u32(&mut cmt[..], 20, c32.finish());
        put_u32(&mut cmt[..], 24, c32c.finish());
        put_u32(&mut cmt[..], 28, JOURNAL_CRC32C);
        self.disk.write(j + 1 + blocks.len() as u64, &cmt)
    }

    fn checkpoint(&mut self) -> Result<()> {
        let txn = core::mem::take(&mut self.txn);
        for (b, img) in &txn {
            self.disk.write(*b, img)?;
        }
        self.pending_free.clear();
        Ok(())
    }

    /// Test hook: commit to the journal but "crash" before the checkpoint.
    #[cfg(test)]
    fn commit_journal_only(&mut self) -> Result<()> {
        self.stage_bitmaps();
        self.sb.journal_seq += 1;
        self.dirty_super();
        let blocks: Vec<u64> = self.txn.keys().copied().collect();
        self.write_journal(&blocks)?;
        self.disk.flush()
    }

    fn stage_bitmaps(&mut self) {
        for i in 0..self.dirty_bmap.len() {
            if core::mem::replace(&mut self.dirty_bmap[i], false) {
                let mut img = zero_block();
                img.copy_from_slice(&self.bmap[i * BLOCK..(i + 1) * BLOCK]);
                self.meta_write(self.sb.bbitmap_start + i as u64, &img);
            }
        }
        for i in 0..self.dirty_imap.len() {
            if core::mem::replace(&mut self.dirty_imap[i], false) {
                let mut img = zero_block();
                img.copy_from_slice(&self.imap[i * BLOCK..(i + 1) * BLOCK]);
                self.meta_write(self.sb.ibitmap_start + i as u64, &img);
            }
        }
    }

    /// Replays a committed transaction left in the journal (crash recovery).
    fn replay(&mut self) -> Result<bool> {
        let j = self.sb.journal_start;
        let mut desc = zero_block();
        self.disk.read(j, &mut desc)?;
        if desc[0..8] != DESC_MAGIC {
            return Ok(false);
        }
        let seq = get_u64(&desc[..], 8);
        let count = get_u32(&desc[..], 16) as usize;
        if count > DESC_CAPACITY || count as u64 + 2 > self.sb.journal_blocks {
            return Ok(false);
        }
        let mut cmt = zero_block();
        self.disk.read(j + 1 + count as u64, &mut cmt)?;
        if cmt[0..8] != COMMIT_MAGIC || get_u64(&cmt[..], 8) != seq || get_u32(&cmt[..], 16) as usize != count {
            return Ok(false); // incomplete transaction: discard
        }
        let mut images = Vec::with_capacity(count);
        let (mut c32, mut c32c) = (Crc32::new(), Crc32c::new());
        c32.update(&desc[..]);
        c32c.update(&desc[..]);
        for i in 0..count {
            let mut img = zero_block();
            self.disk.read(j + 1 + i as u64, &mut img)?;
            c32.update(&img[..]);
            c32c.update(&img[..]);
            images.push(img);
        }
        if c32.finish() != get_u32(&cmt[..], 20) {
            return Ok(false);
        }
        // Written by this version: the stronger checksum must match too (a
        // journal from an older kernel has only CRC-32).
        if get_u32(&cmt[..], 28) == JOURNAL_CRC32C && c32c.finish() != get_u32(&cmt[..], 24) {
            return Ok(false);
        }
        // A home location must be inside the volume and outside the journal
        // (only a corrupt descriptor that still matched its checksum could
        // say otherwise; such a transaction is not replayed at all).
        let journal = self.sb.journal_start..self.sb.journal_start + self.sb.journal_blocks;
        let homes: Vec<u64> = (0..count).map(|i| get_u64(&desc[..], 24 + i * 8)).collect();
        if homes.iter().any(|&h| h >= self.sb.blocks.min(self.disk.blocks()) || journal.contains(&h)) {
            return Ok(false);
        }
        for (home, img) in homes.iter().zip(&images) {
            self.disk.write(*home, img)?;
        }
        // Invalidate the journal so the transaction is not replayed again.
        // (Replaying it twice would do no harm: the images are whole blocks.)
        self.disk.flush()?;
        self.disk.write(j, &zero_block())?;
        self.disk.flush()?;
        Ok(true)
    }

    // ------------------------------------------------------- allocation

    fn is_pending_free(&self, b: u64) -> bool {
        self.pending_free.iter().any(|e| b >= e.start && b < e.start + e.len as u64)
    }

    fn block_free(&self, b: u64) -> bool {
        !bit(&self.bmap, b) && !self.is_pending_free(b)
    }

    /// Sets bit `i` of the block bitmap (or the inode bitmap), noting the
    /// old byte for the running operation's undo log.
    fn set_map_bit(&mut self, inode_map: bool, i: u64, v: bool) {
        let byte = (i / 8) as usize;
        let map = if inode_map { &mut self.imap } else { &mut self.bmap };
        if let Some(u) = self.undo.as_mut() {
            u.bits.push((inode_map, byte, map[byte]));
        }
        set_bit(map, i, v);
        let dirty = if inode_map { &mut self.dirty_imap } else { &mut self.dirty_bmap };
        dirty[(i / (BLOCK as u64 * 8)) as usize] = true;
    }

    fn mark_blocks(&mut self, start: u64, len: u64, used: bool) {
        for b in start..start + len {
            self.set_map_bit(false, b, used);
        }
        if used {
            self.sb.free_blocks = self.sb.free_blocks.saturating_sub(len);
        } else {
            self.sb.free_blocks += len;
        }
        self.dirty_super();
    }

    /// Allocates up to `want` blocks as one contiguous run near `hint`.
    fn alloc_run(&mut self, want: u64, hint: u64) -> Result<Extent> {
        let (lo, hi) = (self.sb.data_start, self.sb.blocks - 1);
        let hint = hint.clamp(lo, hi - 1);
        let mut best: Option<Extent> = None;
        for (from, to) in [(hint, hi), (lo, hint)] {
            let mut b = from;
            while b < to {
                if !self.block_free(b) {
                    b += 1;
                    continue;
                }
                let start = b;
                while b < to && b - start < want.min(u32::MAX as u64) && self.block_free(b) {
                    b += 1;
                }
                let len = b - start;
                if best.is_none_or(|x| (x.len as u64) < len) {
                    best = Some(Extent { start, len: len as u32 });
                }
                if len >= want {
                    break;
                }
            }
            if best.is_some_and(|x| x.len as u64 >= want) {
                break;
            }
        }
        let e = best.ok_or(Error::NoSpace)?;
        self.mark_blocks(e.start, e.len as u64, true);
        self.alloc_hint = e.start + e.len as u64;
        Ok(e)
    }

    fn free_extent(&mut self, e: Extent) {
        if e.len == 0 {
            return;
        }
        // Revoke journaled images of these blocks (e.g. directory contents): once
        // free they may hold file data, which a replay must never overwrite.
        for b in e.start..e.start + e.len as u64 {
            if self.txn.contains_key(&b) {
                self.note_txn(b);
                self.txn.remove(&b);
            }
        }
        self.mark_blocks(e.start, e.len as u64, false);
        self.pending_free.push(e);
    }

    fn alloc_inode(&mut self) -> Result<u64> {
        for i in 1..self.sb.inodes {
            if !bit(&self.imap, i) {
                self.set_map_bit(true, i, true);
                self.sb.free_inodes = self.sb.free_inodes.saturating_sub(1);
                self.dirty_super();
                return Ok(i);
            }
        }
        Err(Error::NoSpace)
    }

    fn free_inode(&mut self, ino: u64) {
        self.set_map_bit(true, ino, false);
        self.sb.free_inodes += 1;
        self.dirty_super();
    }

    // ---------------------------------------------------------- inodes

    fn inode_location(&self, ino: u64) -> Result<(u64, usize)> {
        if ino == 0 || ino >= self.sb.inodes {
            return Err(Error::NotFound);
        }
        Ok((self.sb.itable_start + ino / INODES_PER_BLOCK, (ino % INODES_PER_BLOCK) as usize * INODE_SIZE))
    }

    fn read_inode(&mut self, ino: u64) -> Result<Inode> {
        // Range first: a corrupt directory entry can name any number.
        let (blk, off) = self.inode_location(ino)?;
        if !bit(&self.imap, ino) {
            return Err(Error::NotFound);
        }
        let mut b = zero_block();
        self.meta_read(blk, &mut b)?;
        let r = &b[off..off + INODE_SIZE];
        let count = get_u32(r, 32) as usize;
        if count > MAX_EXTENTS {
            return Err(Error::Corrupt);
        }
        let mut extents = Vec::with_capacity(count);
        for i in 0..count.min(DIRECT_EXTENTS) {
            extents.push(Extent { start: get_u64(r, 40 + i * 16), len: get_u32(r, 48 + i * 16) });
        }
        let extent_block = get_u64(r, 232);
        if count > DIRECT_EXTENTS {
            let mut eb = zero_block();
            self.meta_read(extent_block, &mut eb)?;
            for i in 0..count - DIRECT_EXTENTS {
                extents.push(Extent { start: get_u64(&eb[..], i * 16), len: get_u32(&eb[..], i * 16 + 8) });
            }
        }
        // Every block an inode names must lie in the data area. Checking here
        // keeps a corrupt extent from sending readers, `free` or fsck through
        // billions of blocks.
        let in_data = |start: u64, len: u64| {
            start >= self.sb.data_start && start.checked_add(len).is_some_and(|end| end < self.sb.blocks)
        };
        if !extents.iter().all(|e| in_data(e.start, e.len as u64))
            || (extent_block != 0 && !in_data(extent_block, 1))
            || (count > DIRECT_EXTENTS && extent_block == 0)
        {
            return Err(Error::Corrupt);
        }
        let inode = Inode {
            kind: get_u16(r, 0),
            links: get_u32(r, 4),
            size: get_u64(r, 8),
            ctime: get_u64(r, 16),
            mtime: get_u64(r, 24),
            extents,
            extent_block,
            owner: (get_u16(r, 250) & IFLAG_OWNED != 0).then(|| Owner {
                uid: get_u32(r, 240),
                gid: get_u32(r, 244),
                mode: get_u16(r, 248) & 0o7777,
            }),
        };
        inode.kind()?;
        // Nor may its size claim more than those blocks hold (callers size
        // buffers by it).
        if inode.size.div_ceil(BLOCK as u64) > inode.allocated_blocks() {
            return Err(Error::Corrupt);
        }
        Ok(inode)
    }

    fn write_inode(&mut self, ino: u64, inode: &Inode) -> Result<()> {
        if inode.extents.len() > MAX_EXTENTS {
            return Err(Error::TooBig);
        }
        let (blk, off) = self.inode_location(ino)?;
        let mut b = zero_block();
        self.meta_read(blk, &mut b)?;
        let r = &mut b[off..off + INODE_SIZE];
        r.fill(0);
        put_u16(r, 0, inode.kind);
        put_u32(r, 4, inode.links);
        put_u64(r, 8, inode.size);
        put_u64(r, 16, inode.ctime);
        put_u64(r, 24, inode.mtime);
        put_u32(r, 32, inode.extents.len() as u32);
        for (i, e) in inode.extents.iter().take(DIRECT_EXTENTS).enumerate() {
            put_u64(r, 40 + i * 16, e.start);
            put_u32(r, 48 + i * 16, e.len);
        }
        put_u64(r, 232, inode.extent_block);
        if let Some(o) = inode.owner {
            put_u32(r, 240, o.uid);
            put_u32(r, 244, o.gid);
            put_u16(r, 248, o.mode & 0o7777);
            put_u16(r, 250, IFLAG_OWNED);
        }
        self.meta_write(blk, &b);
        if inode.extents.len() > DIRECT_EXTENTS {
            let mut eb = zero_block();
            for (i, e) in inode.extents[DIRECT_EXTENTS..].iter().enumerate() {
                put_u64(&mut eb[..], i * 16, e.start);
                put_u32(&mut eb[..], i * 16 + 8, e.len);
            }
            self.meta_write(inode.extent_block, &eb);
        }
        Ok(())
    }

    /// Grows `inode` to at least `blocks` allocated blocks.
    fn grow(&mut self, inode: &mut Inode, blocks: u64) -> Result<()> {
        let mut have = inode.allocated_blocks();
        while have < blocks {
            let hint = inode.extents.last().map(|e| e.start + e.len as u64).unwrap_or(self.alloc_hint);
            let e = self.alloc_run(blocks - have, hint)?;
            match inode.extents.last_mut() {
                Some(last)
                    if last.start + last.len as u64 == e.start
                        && (last.len as u64 + e.len as u64) < u32::MAX as u64 =>
                {
                    last.len += e.len
                }
                _ => {
                    if inode.extents.len() == MAX_EXTENTS {
                        self.free_extent(e);
                        return Err(Error::TooBig);
                    }
                    inode.extents.push(e);
                }
            }
            if inode.extents.len() > DIRECT_EXTENTS && inode.extent_block == 0 {
                inode.extent_block = self.alloc_run(1, self.alloc_hint)?.start;
            }
            have += e.len as u64;
        }
        Ok(())
    }

    /// Frees blocks beyond the first `keep` blocks.
    fn shrink(&mut self, inode: &mut Inode, keep: u64) {
        let mut kept = 0u64;
        let mut out = Vec::new();
        for e in core::mem::take(&mut inode.extents) {
            if kept >= keep {
                self.free_extent(e);
            } else if kept + e.len as u64 <= keep {
                kept += e.len as u64;
                out.push(e);
            } else {
                let k = (keep - kept) as u32;
                out.push(Extent { start: e.start, len: k });
                self.free_extent(Extent { start: e.start + k as u64, len: e.len - k });
                kept = keep;
            }
        }
        inode.extents = out;
        if inode.extents.len() <= DIRECT_EXTENTS && inode.extent_block != 0 {
            self.free_extent(Extent { start: inode.extent_block, len: 1 });
            inode.extent_block = 0;
        }
    }

    fn data_read(&mut self, dir: bool, block: u64, buf: &mut Block) -> Result<()> {
        if dir {
            self.meta_read(block, buf)
        } else {
            self.disk.read(block, buf)
        }
    }

    fn data_write(&mut self, dir: bool, block: u64, buf: &Block) -> Result<()> {
        if dir {
            self.meta_write(block, buf);
            Ok(())
        } else {
            self.disk.write(block, buf)
        }
    }

    // ------------------------------------------------------ public API

    pub fn stat(&mut self, ino: u64) -> Result<Meta> {
        let i = self.read_inode(ino)?;
        let kind = i.kind()?;
        Ok(Meta { kind, size: i.size, mtime: i.mtime, ctime: i.ctime, links: i.links, owner: self.owner_of(&i, kind) })
    }

    fn owner_of(&self, i: &Inode, kind: Kind) -> Owner {
        i.owner.unwrap_or(Owner { uid: self.sb.default_uid, gid: self.sb.default_gid, mode: Owner::default_mode(kind) })
    }

    /// The feature bits of this volume (`FEATURE_*`).
    pub fn features(&self) -> u32 {
        self.sb.features
    }

    /// The owner of inodes that carry none: `(uid, gid)`.
    pub fn default_owner(&self) -> (u32, u32) {
        (self.sb.default_uid, self.sb.default_gid)
    }

    /// Turns on owners for a volume made before they existed: every file
    /// without one belongs to `uid`/`gid` from now on. Nothing else changes.
    pub fn enable_owners(&mut self, uid: u32, gid: u32) -> Result<()> {
        self.sb.features |= FEATURE_OWNERS;
        self.sb.default_uid = uid;
        self.sb.default_gid = gid;
        self.dirty_super();
        self.commit()
    }

    /// The owner, group and mode of a file or folder.
    pub fn owner(&mut self, ino: u64) -> Result<Owner> {
        Ok(self.stat(ino)?.owner)
    }

    /// True if the inode has its own owner (rather than the volume's default).
    pub fn has_own_owner(&mut self, ino: u64) -> Result<bool> {
        Ok(self.read_inode(ino)?.owner.is_some())
    }


    /// Gives a file or folder an owner, a group and a mode.
    pub fn set_owner(&mut self, ino: u64, owner: Owner) -> Result<()> {
        self.op(|v| {
            let mut inode = v.read_inode(ino)?;
            inode.owner = Some(Owner { mode: owner.mode & 0o7777, ..owner });
            inode.ctime = (v.now)();
            v.write_inode(ino, &inode)
        })
    }

    pub fn read(&mut self, ino: u64, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let inode = self.read_inode(ino)?;
        let dir = inode.kind()? == Kind::Dir;
        if offset >= inode.size {
            return Ok(0);
        }
        let len = (buf.len() as u64).min(inode.size - offset) as usize;
        let mut done = 0;
        let mut blk = zero_block();
        while done < len {
            let pos = offset + done as u64;
            let (fb, off) = (pos / BLOCK as u64, (pos % BLOCK as u64) as usize);
            // Whole blocks of file data go straight into `buf`, as many
            // as lie one after another on the disk.
            if !dir && off == 0 && len - done >= BLOCK {
                if let Some(pb) = inode.map(fb) {
                    let most = ((len - done) / BLOCK) as u64;
                    let run = (1..most).take_while(|&i| inode.map(fb + i) == Some(pb + i)).count() + 1;
                    self.disk.read_run(pb, &mut buf[done..done + run * BLOCK])?;
                    done += run * BLOCK;
                    continue;
                }
            }
            let n = (BLOCK - off).min(len - done);
            match inode.map(fb) {
                Some(pb) => {
                    self.data_read(dir, pb, &mut blk)?;
                    buf[done..done + n].copy_from_slice(&blk[off..off + n]);
                }
                None => buf[done..done + n].fill(0),
            }
            done += n;
        }
        Ok(len)
    }

    /// Writes file data (a folder's contents are the volume's own business:
    /// `IsDir`).
    pub fn write(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<usize> {
        if self.read_inode(ino)?.kind()? == Kind::Dir {
            return Err(Error::IsDir);
        }
        self.op(|v| v.write_inner(ino, offset, data))
    }

    fn write_inner(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<usize> {
        let mut inode = self.read_inode(ino)?;
        let dir = inode.kind()? == Kind::Dir;
        let end = offset.checked_add(data.len() as u64).ok_or(Error::TooBig)?;
        if end > MAX_FILE_SIZE {
            return Err(Error::TooBig);
        }
        let old_size = inode.size;
        let had = inode.allocated_blocks();
        if let Err(e) = self.grow(&mut inode, end.div_ceil(BLOCK as u64)) {
            // Give back whatever part of the growth succeeded.
            self.shrink(&mut inode, had);
            return Err(e);
        }
        let mut blk = zero_block();
        // Covers any gap between the old end of file and `offset` (zero-filled) plus the new data.
        let mut pos = old_size.min(offset);
        while pos < end {
            let fb = pos / BLOCK as u64;
            let (bstart, bend) = (fb * BLOCK as u64, (fb + 1) * BLOCK as u64);
            let cend = bend.min(end);
            let pb = inode.map(fb).ok_or(Error::Corrupt)?;
            if !(pos == bstart && cend == bend) {
                if bstart < old_size {
                    self.data_read(dir, pb, &mut blk)?;
                } else {
                    blk.fill(0);
                }
            }
            let gap_end = cend.min(offset.max(pos));
            if gap_end > pos {
                blk[(pos - bstart) as usize..(gap_end - bstart) as usize].fill(0);
            }
            let ds = pos.max(offset);
            if cend > ds {
                blk[(ds - bstart) as usize..(cend - bstart) as usize]
                    .copy_from_slice(&data[(ds - offset) as usize..(cend - offset) as usize]);
            }
            self.data_write(dir, pb, &blk)?;
            pos = cend;
        }
        inode.size = inode.size.max(end);
        inode.mtime = (self.now)();
        self.write_inode(ino, &inode)?;
        Ok(data.len())
    }

    /// Sets a file's size: what it loses is freed, what it gains reads as
    /// zeros. Folders: `IsDir`.
    pub fn truncate(&mut self, ino: u64, size: u64) -> Result<()> {
        if self.read_inode(ino)?.kind()? == Kind::Dir {
            return Err(Error::IsDir);
        }
        self.op(|v| v.truncate_inner(ino, size))
    }

    fn truncate_inner(&mut self, ino: u64, size: u64) -> Result<()> {
        let mut inode = self.read_inode(ino)?;
        if size > inode.size {
            if size > MAX_FILE_SIZE {
                return Err(Error::TooBig);
            }
            // Fail before writing anything if the blocks can't fit.
            if size.div_ceil(BLOCK as u64).saturating_sub(inode.allocated_blocks()) > self.sb.free_blocks {
                return Err(Error::NoSpace);
            }
            let zeros = vec![0u8; ZERO_FILL];
            let mut at = inode.size;
            while at < size {
                let n = (size - at).min(ZERO_FILL as u64) as usize;
                self.write_inner(ino, at, &zeros[..n])?;
                at += n as u64;
            }
            return Ok(());
        }
        let keep = size.div_ceil(BLOCK as u64);
        self.shrink(&mut inode, keep);
        // Clear the tail of the last kept block so later growth reads zeros.
        if !size.is_multiple_of(BLOCK as u64) {
            if let Some(pb) = inode.map(keep - 1) {
                let dir = inode.kind()? == Kind::Dir;
                let mut blk = zero_block();
                self.data_read(dir, pb, &mut blk)?;
                blk[(size % BLOCK as u64) as usize..].fill(0);
                self.data_write(dir, pb, &blk)?;
            }
        }
        inode.size = size;
        inode.mtime = (self.now)();
        self.write_inode(ino, &inode)
    }

    /// Gives file `ino` blocks for `size` bytes without writing them (a
    /// swap file, whose every block is written before it is read), and
    /// commits. What the new blocks held before is not cleared: the caller
    /// must never let it be read. Fails before allocating anything if the
    /// blocks don't fit.
    pub fn allocate(&mut self, ino: u64, size: u64) -> Result<()> {
        self.op(|v| {
            let mut inode = v.read_inode(ino)?;
            if inode.kind()? != Kind::File {
                return Err(Error::IsDir);
            }
            if size > MAX_FILE_SIZE {
                return Err(Error::TooBig);
            }
            let blocks = size.div_ceil(BLOCK as u64);
            if blocks.saturating_sub(inode.allocated_blocks()) > v.sb.free_blocks {
                return Err(Error::NoSpace);
            }
            v.grow(&mut inode, blocks)?;
            inode.size = inode.size.max(size);
            inode.mtime = (v.now)();
            v.write_inode(ino, &inode)
        })?;
        self.commit()
    }

    /// Where file `ino`'s blocks are on the disk, in file order: runs of
    /// (first block, blocks).
    pub fn extents(&mut self, ino: u64) -> Result<Vec<(u64, u64)>> {
        let inode = self.read_inode(ino)?;
        Ok(inode.extents.iter().map(|e| (e.start, e.len as u64)).collect())
    }

    /// Sets a file's or folder's times, as when copying it from another volume.
    pub fn set_times(&mut self, ino: u64, ctime: u64, mtime: u64) -> Result<()> {
        self.op(|v| {
            let mut inode = v.read_inode(ino)?;
            inode.ctime = ctime;
            inode.mtime = mtime;
            v.write_inode(ino, &inode)
        })
    }

    // ------------------------------------------------------ operations

    /// The most blocks one transaction may hold (the journal also needs a
    /// descriptor and a commit record).
    fn journal_capacity(&self) -> usize {
        (self.sb.journal_blocks as usize).saturating_sub(2).min(DESC_CAPACITY)
    }

    /// Blocks the running transaction would commit: its images, the
    /// bitmap blocks still to be staged, and the superblock.
    fn txn_blocks(&self) -> usize {
        let staged = |dirty: &[bool], start: u64| {
            dirty.iter().enumerate().filter(|&(i, &d)| d && !self.txn.contains_key(&(start + i as u64))).count()
        };
        self.txn.len()
            + staged(&self.dirty_bmap, self.sb.bbitmap_start)
            + staged(&self.dirty_imap, self.sb.ibitmap_start)
            + usize::from(!self.txn.contains_key(&0))
    }

    /// Runs `f` as one operation. A transaction is never committed inside
    /// one; if `f` fails, every change it made to the metadata is put back
    /// (file data it wrote in place stays, as with any failed write), so a
    /// failing call leaves the volume as it found it. After `f`, the
    /// transaction is committed if it is within 64 blocks of filling the
    /// journal (or past half of a small one).
    ///
    /// An operation too big for what is left of the journal is undone and,
    /// if a transaction was already running, run again after committing it.
    /// One too big for the whole journal fails with `TooBig`.
    fn op<T>(&mut self, mut f: impl FnMut(&mut Self) -> Result<T>) -> Result<T> {
        if self.undo.is_some() {
            // Part of an operation already running.
            return f(self);
        }
        for attempt in 0..2 {
            self.undo = Some(Undo {
                txn: BTreeMap::new(),
                bits: Vec::new(),
                dirty_bmap: self.dirty_bmap.clone(),
                dirty_imap: self.dirty_imap.clone(),
                sb: self.sb.clone(),
                pending_free: self.pending_free.len(),
                alloc_hint: self.alloc_hint,
                orphans: self.orphans.clone(),
            });
            let started_empty = self.txn_blocks() <= 1;
            let r = f(self).and_then(|v| if self.txn_blocks() > self.journal_capacity() { Err(Error::TooBig) } else { Ok(v) });
            let undo = self.undo.take().unwrap();
            match r {
                Ok(v) => {
                    let capacity = self.journal_capacity();
                    if self.txn_blocks() > capacity.saturating_sub(64).max(capacity / 2) {
                        // The operation is done (in memory) either way; a
                        // commit that fails here is tried again by the next
                        // one, and reported by `sync`.
                        let _ = self.commit();
                    }
                    return Ok(v);
                }
                Err(e) => {
                    self.rollback(undo);
                    if e == Error::TooBig && attempt == 0 && !started_empty {
                        self.commit()?;
                        continue;
                    }
                    return Err(e);
                }
            }
        }
        Err(Error::TooBig)
    }

    fn rollback(&mut self, u: Undo) {
        for (b, img) in u.txn {
            match img {
                Some(img) => {
                    self.txn.insert(b, img);
                }
                None => {
                    self.txn.remove(&b);
                }
            }
        }
        for (inode_map, byte, old) in u.bits.into_iter().rev() {
            if inode_map {
                self.imap[byte] = old;
            } else {
                self.bmap[byte] = old;
            }
        }
        self.dirty_bmap = u.dirty_bmap;
        self.dirty_imap = u.dirty_imap;
        self.sb = u.sb;
        self.pending_free.truncate(u.pending_free);
        self.alloc_hint = u.alloc_hint;
        self.orphans = u.orphans;
    }

    // ------------------------------------------------------ directories

    pub fn readdir(&mut self, dir: u64) -> Result<Vec<DirEntry>> {
        let inode = self.read_inode(dir)?;
        if inode.kind()? != Kind::Dir {
            return Err(Error::NotDir);
        }
        let mut bytes = vec![0u8; inode.size as usize];
        self.read(dir, 0, &mut bytes)?;
        let mut out = Vec::new();
        let mut i = 0;
        while i + 10 <= bytes.len() {
            let ino = get_u64(&bytes, i);
            let kind = match bytes[i + 8] {
                1 => Kind::File,
                2 => Kind::Dir,
                _ => return Err(Error::Corrupt),
            };
            let n = bytes[i + 9] as usize;
            if i + 10 + n > bytes.len() {
                return Err(Error::Corrupt);
            }
            let name = String::from_utf8_lossy(&bytes[i + 10..i + 10 + n]).into_owned();
            out.push(DirEntry { name, ino, kind });
            i += 10 + n;
        }
        Ok(out)
    }

    fn write_dir(&mut self, dir: u64, entries: &[DirEntry]) -> Result<()> {
        let mut bytes = Vec::new();
        for e in entries {
            bytes.extend_from_slice(&e.ino.to_le_bytes());
            bytes.push(e.kind as u8);
            bytes.push(e.name.len() as u8);
            bytes.extend_from_slice(e.name.as_bytes());
        }
        self.write_inner(dir, 0, &bytes)?;
        self.truncate_inner(dir, bytes.len() as u64)
    }

    fn check_name(name: &str) -> Result<()> {
        if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
            return Err(Error::Invalid);
        }
        if name.len() > NAME_MAX {
            return Err(Error::NameTooLong);
        }
        Ok(())
    }

    pub fn lookup(&mut self, dir: u64, name: &str) -> Result<u64> {
        self.readdir(dir)?.into_iter().find(|e| e.name == name).map(|e| e.ino).ok_or(Error::NotFound)
    }

    pub fn create(&mut self, dir: u64, name: &str, kind: Kind) -> Result<u64> {
        self.create_owned(dir, name, kind, None)
    }

    /// Creates a file or folder with an owner (None: the volume's default).
    pub fn create_owned(&mut self, dir: u64, name: &str, kind: Kind, owner: Option<Owner>) -> Result<u64> {
        Self::check_name(name)?;
        self.op(|v| {
            let mut entries = v.live_dir(dir)?;
            if entries.iter().any(|e| e.name == name) {
                return Err(Error::Exists);
            }
            let ino = v.alloc_inode()?;
            let t = (v.now)();
            let links = if kind == Kind::Dir { 2 } else { 1 };
            let owner = owner.map(|o| Owner { mode: o.mode & 0o7777, ..o });
            v.write_inode(ino, &Inode::new(kind as u16, links, t, owner))?;
            entries.push(DirEntry { name: String::from(name), ino, kind });
            v.write_dir(dir, &entries)?;
            Ok(ino)
        })
    }

    /// The entries of folder `dir`, which must still have a name: a folder
    /// removed while open takes no new entries (`NotFound`, as on Linux),
    /// or they would be lost with it at its last close.
    fn live_dir(&mut self, dir: u64) -> Result<Vec<DirEntry>> {
        if self.orphans.contains(&dir) {
            return Err(Error::NotFound);
        }
        self.readdir(dir)
    }

    /// The last name of `ino` is gone: frees it, or, while it is open,
    /// keeps it as an orphan with no links until its last close.
    fn release(&mut self, ino: u64) -> Result<()> {
        let mut inode = self.read_inode(ino)?;
        if self.open.contains_key(&ino) {
            inode.links = 0;
            inode.ctime = (self.now)();
            self.write_inode(ino, &inode)?;
            self.orphans.insert(ino);
            return Ok(());
        }
        self.shrink(&mut inode, 0);
        let empty = Inode::new(0, 0, 0, None);
        self.write_inode(ino, &empty)?;
        self.free_inode(ino);
        self.orphans.remove(&ino);
        Ok(())
    }

    /// Notes that `ino` is open (a file descriptor): if its last name goes,
    /// its inode and data stay, readable and writable through it, until
    /// [`Volume::close_inode`]. Without this, a removed file's inode number
    /// could be given to a new file or folder while someone still wrote to
    /// (or truncated) it by number.
    pub fn open_inode(&mut self, ino: u64) -> Result<()> {
        self.read_inode(ino)?;
        *self.open.entry(ino).or_insert(0) += 1;
        Ok(())
    }

    /// Looks `name` up in `dir` and holds what it names open
    /// ([`Volume::open_inode`]), at once. Pinning an inode number found by
    /// an earlier lookup is not the same: the file may have been removed in
    /// between and its inode given to a new one.
    pub fn open_at(&mut self, dir: u64, name: &str) -> Result<u64> {
        let ino = self.lookup(dir, name)?;
        self.open_inode(ino)?;
        Ok(ino)
    }

    /// One holder of `ino` let go; an orphan is freed at its last close.
    pub fn close_inode(&mut self, ino: u64) -> Result<()> {
        let Some(n) = self.open.get_mut(&ino) else { return Ok(()) };
        *n -= 1;
        if *n > 0 {
            return Ok(());
        }
        self.open.remove(&ino);
        if self.orphans.contains(&ino) {
            self.op(|v| v.release(ino))?;
        }
        Ok(())
    }

    /// Inodes removed while open and not yet closed.
    pub fn orphans(&self) -> usize {
        self.orphans.len()
    }

    /// Removes a file or an empty directory.
    pub fn unlink(&mut self, dir: u64, name: &str) -> Result<()> {
        self.op(|v| {
            let mut entries = v.readdir(dir)?;
            let pos = entries.iter().position(|e| e.name == name).ok_or(Error::NotFound)?;
            let e = entries[pos].clone();
            if e.kind == Kind::Dir && !v.readdir(e.ino)?.is_empty() {
                return Err(Error::NotEmpty);
            }
            entries.remove(pos);
            v.write_dir(dir, &entries)?;
            v.release(e.ino)
        })
    }

    /// Folder `dir` is `target` or has it somewhere inside.
    fn contains(&mut self, dir: u64, target: u64) -> Result<bool> {
        let mut stack = vec![dir];
        let mut seen = BTreeSet::new();
        while let Some(d) = stack.pop() {
            if d == target {
                return Ok(true);
            }
            if !seen.insert(d) {
                continue; // (only a corrupt volume has cycles)
            }
            stack.extend(self.readdir(d)?.into_iter().filter(|e| e.kind == Kind::Dir).map(|e| e.ino));
        }
        Ok(false)
    }

    /// Moves (and renames) an entry; a file at the destination is replaced.
    /// A folder can't be moved into itself or anything inside it
    /// (`Invalid`), whatever paths the caller resolved before: the check is
    /// made here, in the same operation as the move.
    pub fn rename(&mut self, from_dir: u64, from: &str, to_dir: u64, to: &str) -> Result<()> {
        Self::check_name(to)?;
        self.op(|v| v.rename_inner(from_dir, from, to_dir, to))
    }

    fn rename_inner(&mut self, from_dir: u64, from: &str, to_dir: u64, to: &str) -> Result<()> {
        let mut src = self.readdir(from_dir)?;
        let pos = src.iter().position(|e| e.name == from).ok_or(Error::NotFound)?;
        let mut entry = src[pos].clone();
        if from_dir == to_dir && from == to {
            return Ok(());
        }
        let mut dst = if from_dir == to_dir { src.clone() } else { self.live_dir(to_dir)? };
        if entry.kind == Kind::Dir && from_dir != to_dir && self.contains(entry.ino, to_dir)? {
            return Err(Error::Invalid);
        }
        let mut replaced = None;
        if let Some(i) = dst.iter().position(|e| e.name == to) {
            let old = dst[i].clone();
            if old.kind == Kind::Dir || entry.kind == Kind::Dir {
                return Err(Error::Exists);
            }
            dst.remove(i);
            replaced = Some(old.ino);
        }
        entry.name = String::from(to);
        if from_dir == to_dir {
            dst.retain(|e| e.name != from);
            dst.push(entry);
            self.write_dir(to_dir, &dst)?;
        } else {
            src.remove(pos);
            self.write_dir(from_dir, &src)?;
            dst.push(entry);
            self.write_dir(to_dir, &dst)?;
        }
        if let Some(ino) = replaced {
            self.release(ino)?;
        }
        Ok(())
    }
}

// ------------------------------------------------------------ paths

impl<D: Disk> Volume<D> {
    /// Resolves an absolute path ("/a/b") to an inode.
    pub fn resolve(&mut self, path: &str) -> Result<u64> {
        let mut ino = ROOT;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            ino = self.lookup(ino, c)?;
        }
        Ok(ino)
    }

    /// Creates every directory along `path`.
    pub fn mkdir_all(&mut self, path: &str) -> Result<u64> {
        let mut ino = ROOT;
        for c in path.split('/').filter(|c| !c.is_empty()) {
            ino = match self.lookup(ino, c) {
                Ok(i) => i,
                Err(Error::NotFound) => self.create(ino, c, Kind::Dir)?,
                Err(e) => return Err(e),
            };
        }
        Ok(ino)
    }

    /// Creates (or replaces) a file at `path` with `data`.
    pub fn write_file(&mut self, path: &str, data: &[u8]) -> Result<u64> {
        let (dir, name) = match path.rfind('/') {
            Some(i) => (&path[..i], &path[i + 1..]),
            None => ("", path),
        };
        let d = self.mkdir_all(dir)?;
        let ino = match self.lookup(d, name) {
            Ok(i) => i,
            Err(Error::NotFound) => self.create(d, name, Kind::File)?,
            Err(e) => return Err(e),
        };
        self.truncate(ino, 0)?;
        self.write(ino, 0, data)?;
        Ok(ino)
    }
}
