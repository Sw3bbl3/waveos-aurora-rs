//! A write-back cache of 4 KiB blocks over a block device.
//!
//! Writes stay in memory until `flush` (or eviction); `flush` writes dirty
//! blocks in ascending order and then flushes the device, which is the
//! durability point filesystems build their ordering guarantees on.

use crate::drivers::block::BlockDevice;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec;
use aurora_abi::err::*;

pub const BLOCK: usize = 4096;
type Block = [u8; BLOCK];

struct Entry {
    data: Box<Block>,
    dirty: bool,
    used: u64,
}

pub struct BlockCache {
    dev: Arc<dyn BlockDevice>,
    entries: BTreeMap<u64, Entry>,
    capacity: usize,
    clock: u64,
    per_block: u64,
    pub hits: u64,
    pub misses: u64,
}

impl BlockCache {
    pub fn new(dev: Arc<dyn BlockDevice>, capacity: usize) -> Result<BlockCache, isize> {
        let ss = dev.sector_size() as usize;
        if BLOCK % ss != 0 {
            return Err(EINVAL);
        }
        Ok(BlockCache {
            per_block: (BLOCK / ss) as u64,
            dev,
            entries: BTreeMap::new(),
            capacity,
            clock: 0,
            hits: 0,
            misses: 0,
        })
    }

    pub fn blocks(&self) -> u64 {
        self.dev.sectors() / self.per_block
    }

    fn evict(&mut self) -> Result<(), isize> {
        while self.entries.len() >= self.capacity {
            let (&victim, _) = self.entries.iter().min_by_key(|(_, e)| e.used).unwrap();
            let e = self.entries.remove(&victim).unwrap();
            if e.dirty {
                self.dev.write(victim * self.per_block, &e.data[..])?;
            }
        }
        Ok(())
    }

    pub fn read(&mut self, block: u64, buf: &mut Block) -> Result<(), isize> {
        self.clock += 1;
        if let Some(e) = self.entries.get_mut(&block) {
            e.used = self.clock;
            buf.copy_from_slice(&e.data[..]);
            self.hits += 1;
            return Ok(());
        }
        self.misses += 1;
        let mut data = Box::new([0u8; BLOCK]);
        self.dev.read(block * self.per_block, &mut data[..])?;
        buf.copy_from_slice(&data[..]);
        self.evict()?;
        self.entries.insert(block, Entry { data, dirty: false, used: self.clock });
        Ok(())
    }

    pub fn write(&mut self, block: u64, buf: &Block) -> Result<(), isize> {
        if block >= self.blocks() {
            return Err(EINVAL);
        }
        self.clock += 1;
        if let Some(e) = self.entries.get_mut(&block) {
            e.data.copy_from_slice(buf);
            e.dirty = true;
            e.used = self.clock;
            return Ok(());
        }
        self.evict()?;
        let mut data = Box::new([0u8; BLOCK]);
        data.copy_from_slice(buf);
        self.entries.insert(block, Entry { data, dirty: true, used: self.clock });
        Ok(())
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
