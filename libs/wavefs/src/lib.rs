//! WaveFS — the WaveOS Aurora filesystem.
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
//! All metadata changes (superblock, bitmaps, inodes, directory contents) are
//! collected in a transaction and reach their home location only after the
//! transaction has been written to the journal and flushed. File data is
//! written in place and flushed *before* the commit record, so metadata never
//! points at unwritten data. Blocks freed in a transaction are not reused
//! until it commits. After a crash, `mount` replays the last committed
//! transaction, so the on-disk structure is always consistent.
//!
//! This crate is `no_std`; the kernel and the host tools (`xtask mkfs`) share it.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod crc;
#[cfg(test)]
mod tests;

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub use crc::crc32;

pub const BLOCK: usize = 4096;
pub const MAGIC: [u8; 8] = *b"WAVEFS01";
pub const VERSION: u32 = 1;
pub const ROOT: u64 = 1;
pub const NAME_MAX: usize = 255;
/// GPT partition type for WaveFS volumes: 57415645-4653-4175-726F-72612D465331 (on-disk byte order).
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

pub type Block = [u8; BLOCK];

/// Storage underneath a volume, in 4 KiB blocks.
pub trait Disk {
    fn blocks(&self) -> u64;
    fn read(&mut self, block: u64, buf: &mut Block) -> Result<()>;
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
}

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
        };
        if sb.data_start >= sb.blocks || sb.itable_start + sb.itable_blocks > sb.data_start {
            return Err(Error::Corrupt);
        }
        Ok(sb)
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
}

impl Inode {
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
        vol.write_inode(
            ROOT,
            &Inode {
                kind: Kind::Dir as u16,
                links: 2,
                size: 0,
                ctime: t,
                mtime: t,
                extents: Vec::new(),
                extent_block: 0,
            },
        )?;
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
            disk,
            sb,
        };
        vol.replay()?;
        // The superblock may have been replayed: reload it.
        let mut b = zero_block();
        vol.disk.read(0, &mut b)?;
        vol.sb = Superblock::decode(&b)?;
        vol.bmap = vol.load_region(vol.sb.bbitmap_start, vol.sb.bbitmap_blocks)?;
        vol.imap = vol.load_region(vol.sb.ibitmap_start, vol.sb.ibitmap_blocks)?;
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

    fn meta_write(&mut self, block: u64, data: &Block) {
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
    pub fn commit(&mut self) -> Result<()> {
        self.stage_bitmaps();
        if self.txn.is_empty() {
            self.pending_free.clear();
            return self.disk.flush();
        }
        self.sb.journal_seq += 1;
        self.dirty_super();
        let blocks: Vec<u64> = self.txn.keys().copied().collect();
        if blocks.len() + 2 > self.sb.journal_blocks as usize || blocks.len() > DESC_CAPACITY {
            return Err(Error::TooBig);
        }
        // 1. File data (written in place) must be durable before the commit record.
        self.disk.flush()?;
        // 2. Journal: descriptor, images, commit record.
        let mut desc = zero_block();
        desc[0..8].copy_from_slice(&DESC_MAGIC);
        put_u64(&mut desc[..], 8, self.sb.journal_seq);
        put_u32(&mut desc[..], 16, blocks.len() as u32);
        for (i, b) in blocks.iter().enumerate() {
            put_u64(&mut desc[..], 24 + i * 8, *b);
        }
        let mut crc_input: Vec<u8> = Vec::with_capacity((blocks.len() + 1) * BLOCK);
        crc_input.extend_from_slice(&desc[..]);
        let j = self.sb.journal_start;
        self.disk.write(j, &desc)?;
        for (i, b) in blocks.iter().enumerate() {
            let img = &self.txn[b];
            crc_input.extend_from_slice(&img[..]);
            self.disk.write(j + 1 + i as u64, img)?;
        }
        let mut cmt = zero_block();
        cmt[0..8].copy_from_slice(&COMMIT_MAGIC);
        put_u64(&mut cmt[..], 8, self.sb.journal_seq);
        put_u32(&mut cmt[..], 16, blocks.len() as u32);
        put_u32(&mut cmt[..], 20, crc32(&crc_input));
        self.disk.write(j + 1 + blocks.len() as u64, &cmt)?;
        self.disk.flush()?;
        // 3. Checkpoint: copy images home. (Durable by the next commit's first
        //    flush; until then, a crash is repaired by replaying the journal.)
        self.checkpoint()
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
        let mut desc = zero_block();
        desc[0..8].copy_from_slice(&DESC_MAGIC);
        put_u64(&mut desc[..], 8, self.sb.journal_seq);
        put_u32(&mut desc[..], 16, blocks.len() as u32);
        for (i, b) in blocks.iter().enumerate() {
            put_u64(&mut desc[..], 24 + i * 8, *b);
        }
        let mut crc_input = Vec::new();
        crc_input.extend_from_slice(&desc[..]);
        let j = self.sb.journal_start;
        self.disk.write(j, &desc)?;
        for (i, b) in blocks.iter().enumerate() {
            crc_input.extend_from_slice(&self.txn[b][..]);
            let img = self.txn[b].clone();
            self.disk.write(j + 1 + i as u64, &img)?;
        }
        let mut cmt = zero_block();
        cmt[0..8].copy_from_slice(&COMMIT_MAGIC);
        put_u64(&mut cmt[..], 8, self.sb.journal_seq);
        put_u32(&mut cmt[..], 16, blocks.len() as u32);
        put_u32(&mut cmt[..], 20, crc32(&crc_input));
        self.disk.write(j + 1 + blocks.len() as u64, &cmt)?;
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
        let mut crc_input = Vec::with_capacity((count + 1) * BLOCK);
        crc_input.extend_from_slice(&desc[..]);
        for i in 0..count {
            let mut img = zero_block();
            self.disk.read(j + 1 + i as u64, &mut img)?;
            crc_input.extend_from_slice(&img[..]);
            images.push(img);
        }
        if crc32(&crc_input) != get_u32(&cmt[..], 20) {
            return Ok(false);
        }
        for (i, img) in images.iter().enumerate() {
            let home = get_u64(&desc[..], 24 + i * 8);
            if home < self.disk.blocks() {
                self.disk.write(home, img)?;
            }
        }
        // Invalidate the journal so the transaction is not replayed again.
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

    fn mark_blocks(&mut self, start: u64, len: u64, used: bool) {
        for b in start..start + len {
            set_bit(&mut self.bmap, b, used);
            self.dirty_bmap[(b / (BLOCK as u64 * 8)) as usize] = true;
        }
        if used {
            self.sb.free_blocks -= len;
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
            self.txn.remove(&b);
        }
        self.mark_blocks(e.start, e.len as u64, false);
        self.pending_free.push(e);
    }

    fn alloc_inode(&mut self) -> Result<u64> {
        for i in 1..self.sb.inodes {
            if !bit(&self.imap, i) {
                set_bit(&mut self.imap, i, true);
                self.dirty_imap[(i / (BLOCK as u64 * 8)) as usize] = true;
                self.sb.free_inodes -= 1;
                self.dirty_super();
                return Ok(i);
            }
        }
        Err(Error::NoSpace)
    }

    fn free_inode(&mut self, ino: u64) {
        set_bit(&mut self.imap, ino, false);
        self.dirty_imap[(ino / (BLOCK as u64 * 8)) as usize] = true;
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
        if !bit(&self.imap, ino) {
            return Err(Error::NotFound);
        }
        let (blk, off) = self.inode_location(ino)?;
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
        let inode = Inode {
            kind: get_u16(r, 0),
            links: get_u32(r, 4),
            size: get_u64(r, 8),
            ctime: get_u64(r, 16),
            mtime: get_u64(r, 24),
            extents,
            extent_block,
        };
        inode.kind()?;
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
        Ok(Meta { kind: i.kind()?, size: i.size, mtime: i.mtime, ctime: i.ctime, links: i.links })
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

    pub fn write(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<usize> {
        let mut inode = self.read_inode(ino)?;
        let dir = inode.kind()? == Kind::Dir;
        let end = offset.checked_add(data.len() as u64).ok_or(Error::TooBig)?;
        if end > (1u64 << 40) {
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
        self.maybe_commit()?;
        Ok(data.len())
    }

    pub fn truncate(&mut self, ino: u64, size: u64) -> Result<()> {
        let mut inode = self.read_inode(ino)?;
        if size > inode.size {
            let zeros = vec![0u8; (size - inode.size) as usize];
            return self.write(ino, inode.size, &zeros).map(|_| ());
        }
        let keep = size.div_ceil(BLOCK as u64);
        self.shrink(&mut inode, keep);
        // Clear the tail of the last kept block so later growth reads zeros.
        if size % BLOCK as u64 != 0 {
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
        self.write_inode(ino, &inode)?;
        self.maybe_commit()
    }

    /// Commits early when a transaction grows large.
    fn maybe_commit(&mut self) -> Result<()> {
        let limit = ((self.sb.journal_blocks as usize).saturating_sub(64)).min(DESC_CAPACITY - 64);
        if self.txn.len() + self.dirty_bmap.len() + self.dirty_imap.len() > limit {
            self.commit()?;
        }
        Ok(())
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
        self.write(dir, 0, &bytes)?;
        self.truncate(dir, bytes.len() as u64)
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
        Self::check_name(name)?;
        let mut entries = self.readdir(dir)?;
        if entries.iter().any(|e| e.name == name) {
            return Err(Error::Exists);
        }
        let ino = self.alloc_inode()?;
        let t = (self.now)();
        let links = if kind == Kind::Dir { 2 } else { 1 };
        self.write_inode(
            ino,
            &Inode { kind: kind as u16, links, size: 0, ctime: t, mtime: t, extents: Vec::new(), extent_block: 0 },
        )?;
        entries.push(DirEntry { name: String::from(name), ino, kind });
        self.write_dir(dir, &entries)?;
        Ok(ino)
    }

    fn release(&mut self, ino: u64) -> Result<()> {
        let mut inode = self.read_inode(ino)?;
        self.shrink(&mut inode, 0);
        let empty = Inode { kind: 0, links: 0, size: 0, ctime: 0, mtime: 0, extents: Vec::new(), extent_block: 0 };
        self.write_inode(ino, &empty)?;
        self.free_inode(ino);
        Ok(())
    }

    /// Removes a file or an empty directory.
    pub fn unlink(&mut self, dir: u64, name: &str) -> Result<()> {
        let mut entries = self.readdir(dir)?;
        let pos = entries.iter().position(|e| e.name == name).ok_or(Error::NotFound)?;
        let e = entries[pos].clone();
        if e.kind == Kind::Dir && !self.readdir(e.ino)?.is_empty() {
            return Err(Error::NotEmpty);
        }
        entries.remove(pos);
        self.write_dir(dir, &entries)?;
        self.release(e.ino)?;
        self.maybe_commit()
    }

    pub fn rename(&mut self, from_dir: u64, from: &str, to_dir: u64, to: &str) -> Result<()> {
        Self::check_name(to)?;
        let mut src = self.readdir(from_dir)?;
        let pos = src.iter().position(|e| e.name == from).ok_or(Error::NotFound)?;
        let mut entry = src[pos].clone();
        if from_dir == to_dir && from == to {
            return Ok(());
        }
        let mut dst = if from_dir == to_dir { src.clone() } else { self.readdir(to_dir)? };
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
        self.maybe_commit()
    }

    // --------------------------------------------------------- checking

    /// Verifies structural consistency: every allocated block is referenced
    /// exactly once, every live inode is reachable, directory entries are valid.
    pub fn fsck(&mut self) -> core::result::Result<(), String> {
        use alloc::format;
        let mut owner: BTreeMap<u64, u64> = BTreeMap::new();
        let mut seen: BTreeMap<u64, bool> = BTreeMap::new();
        let mut stack = vec![ROOT];
        while let Some(ino) = stack.pop() {
            if seen.insert(ino, true).is_some() {
                return Err(format!("inode {ino} reachable twice"));
            }
            let inode = self.read_inode(ino).map_err(|e| format!("inode {ino}: {e:?}"))?;
            if inode.allocated_blocks() < inode.size.div_ceil(BLOCK as u64) {
                return Err(format!("inode {ino}: size {} exceeds its blocks", inode.size));
            }
            let mut blocks: Vec<u64> = Vec::new();
            for e in &inode.extents {
                for b in e.start..e.start + e.len as u64 {
                    blocks.push(b);
                }
            }
            if inode.extent_block != 0 {
                blocks.push(inode.extent_block);
            }
            for b in blocks {
                if b < self.sb.data_start || b >= self.sb.blocks - 1 {
                    return Err(format!("inode {ino}: block {b} outside the data area"));
                }
                if let Some(o) = owner.insert(b, ino) {
                    return Err(format!("block {b} owned by inodes {o} and {ino}"));
                }
                if !bit(&self.bmap, b) {
                    return Err(format!("block {b} used by inode {ino} but free in the bitmap"));
                }
            }
            if inode.kind() == Ok(Kind::Dir) {
                for e in self.readdir(ino).map_err(|e| format!("dir {ino}: {e:?}"))? {
                    let child = self.read_inode(e.ino).map_err(|x| format!("entry {} -> {}: {x:?}", e.name, e.ino))?;
                    if child.kind() != Ok(e.kind) {
                        return Err(format!("entry {} has the wrong kind", e.name));
                    }
                    stack.push(e.ino);
                }
            }
        }
        for b in self.sb.data_start..self.sb.blocks - 1 {
            if bit(&self.bmap, b) && !owner.contains_key(&b) {
                return Err(format!("block {b} marked used but unreferenced (leak)"));
            }
        }
        for i in 2..self.sb.inodes {
            if bit(&self.imap, i) && !seen.contains_key(&i) {
                return Err(format!("inode {i} allocated but unreachable"));
            }
        }
        let free = (self.sb.data_start..self.sb.blocks - 1).filter(|&b| !bit(&self.bmap, b)).count() as u64;
        if free != self.sb.free_blocks {
            return Err(format!("free block count {} != bitmap {}", self.sb.free_blocks, free));
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
