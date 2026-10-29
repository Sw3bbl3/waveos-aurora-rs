//! A write-back cache of 4 KiB blocks over a block device.
//!
//! Writes stay in memory until `flush` (or eviction); `flush` writes dirty
//! blocks in ascending order and then flushes the device, which is the
//! durability point filesystems build their ordering guarantees on.
//!
//! A miss reads ahead: the block and the ones after it that aren't cached,
//! up to [`READ_AHEAD`], in one request, since files are mostly read in
//! order and each request costs far more than its bytes. The block used
//! longest ago is the one evicted.

use crate::drivers::block::BlockDevice;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use aurora_abi::err::*;

pub const BLOCK: usize = 4096;
/// Blocks a miss reads at most (128 KiB).
const READ_AHEAD: u64 = 32;
type Block = [u8; BLOCK];

struct Entry {
    data: Box<Block>,
    dirty: bool,
    used: u64,
}

pub struct BlockCache {
    dev: Arc<dyn BlockDevice>,
    entries: BTreeMap<u64, Entry>,
    /// Blocks by when they were last used (`Entry::used` → block).
    by_use: BTreeMap<u64, u64>,
    capacity: usize,
    clock: u64,
    per_block: u64,
    pub hits: u64,
    pub misses: u64,
}

impl BlockCache {
    pub fn new(dev: Arc<dyn BlockDevice>, capacity: usize) -> Result<BlockCache, isize> {
        let ss = dev.sector_size() as usize;
        if !BLOCK.is_multiple_of(ss) {
            return Err(EINVAL);
        }
        Ok(BlockCache {
            per_block: (BLOCK / ss) as u64,
            dev,
            entries: BTreeMap::new(),
            by_use: BTreeMap::new(),
            capacity,
            clock: 0,
            hits: 0,
            misses: 0,
        })
    }

    pub fn blocks(&self) -> u64 {
        self.dev.sectors() / self.per_block
    }

    /// Makes room for one more block: writes back and drops the one used longest ago.
    fn evict(&mut self) -> Result<(), isize> {
        while self.entries.len() >= self.capacity {
            let Some((_, victim)) = self.by_use.pop_first() else { break };
            let e = self.entries.remove(&victim).unwrap();
            if e.dirty {
                self.dev.write(victim * self.per_block, &e.data[..])?;
            }
        }
        Ok(())
    }

    /// Marks `block` (cached) as just used.
    fn touch(&mut self, block: u64) -> &mut Entry {
        self.clock += 1;
        let e = self.entries.get_mut(&block).unwrap();
        self.by_use.remove(&e.used);
        e.used = self.clock;
        self.by_use.insert(self.clock, block);
        e
    }

    fn insert(&mut self, block: u64, data: Box<Block>, dirty: bool) -> Result<(), isize> {
        self.evict()?;
        self.clock += 1;
        self.entries.insert(block, Entry { data, dirty, used: self.clock });
        self.by_use.insert(self.clock, block);
        Ok(())
    }

    pub fn read(&mut self, block: u64, buf: &mut Block) -> Result<(), isize> {
        if self.entries.contains_key(&block) {
            buf.copy_from_slice(&self.touch(block).data[..]);
            self.hits += 1;
            return Ok(());
        }
        self.misses += 1;
        // This block and the uncached ones after it, in one request.
        let limit = READ_AHEAD.min(self.blocks().saturating_sub(block)).max(1);
        let run = (1..limit).take_while(|i| !self.entries.contains_key(&(block + i))).count() as u64 + 1;
        let mut data = vec![0u8; run as usize * BLOCK];
        self.dev.read(block * self.per_block, &mut data)?;
        buf.copy_from_slice(&data[..BLOCK]);
        // The wanted block last, so it's the most recently used.
        for i in (0..run).rev() {
            let at = i as usize * BLOCK;
            let mut b = Box::new([0u8; BLOCK]);
            b.copy_from_slice(&data[at..at + BLOCK]);
            self.insert(block + i, b, false)?;
        }
        Ok(())
    }

    /// Reads `buf.len() / BLOCK` consecutive blocks from `block` without
    /// caching them (a file's data, read in bulk): cached blocks (which may
    /// be newer than the disk) are copied, the others come from the device
    /// straight into `buf`, a run per request.
    pub fn read_run(&mut self, block: u64, buf: &mut [u8]) -> Result<(), isize> {
        let count = (buf.len() / BLOCK) as u64;
        if block + count > self.blocks() {
            return Err(EINVAL);
        }
        let mut i = 0;
        while i < count {
            if let Some(e) = self.entries.get(&(block + i)) {
                let at = i as usize * BLOCK;
                buf[at..at + BLOCK].copy_from_slice(&e.data[..]);
                self.hits += 1;
                i += 1;
                continue;
            }
            let run = (i..count).take_while(|j| !self.entries.contains_key(&(block + j))).count() as u64;
            let at = i as usize * BLOCK;
            self.dev.read((block + i) * self.per_block, &mut buf[at..at + run as usize * BLOCK])?;
            self.misses += run;
            i += run;
        }
        Ok(())
    }

    pub fn write(&mut self, block: u64, buf: &Block) -> Result<(), isize> {
        if block >= self.blocks() {
            return Err(EINVAL);
        }
        if self.entries.contains_key(&block) {
            let e = self.touch(block);
            e.data.copy_from_slice(buf);
            e.dirty = true;
            return Ok(());
        }
        let mut data = Box::new([0u8; BLOCK]);
        data.copy_from_slice(buf);
        self.insert(block, data, true)
    }

    /// Writes back every dirty block (merging runs of adjacent blocks), then flushes the device.
    pub fn flush(&mut self) -> Result<(), isize> {
        let dirty: alloc::vec::Vec<u64> = self.entries.iter().filter(|(_, e)| e.dirty).map(|(&b, _)| b).collect();
        let mut i = 0;
        while i < dirty.len() {
            let mut j = i + 1;
            while j < dirty.len() && dirty[j] == dirty[j - 1] + 1 && j - i < 16 {
                j += 1;
            }
            let mut run = vec![0u8; (j - i) * BLOCK];
            for (k, b) in dirty[i..j].iter().enumerate() {
                run[k * BLOCK..(k + 1) * BLOCK].copy_from_slice(&self.entries[b].data[..]);
            }
            self.dev.write(dirty[i] * self.per_block, &run)?;
            for b in &dirty[i..j] {
                self.entries.get_mut(b).unwrap().dirty = false;
            }
            i = j;
        }
        self.dev.flush()
    }
}
