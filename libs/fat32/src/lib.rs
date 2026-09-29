//! A FAT32 driver with long-file-name support (`no_std` + `alloc`).
//!
//! Used by the kernel to mount the EFI System Partition at `/Boot`. Nodes are
//! addressed by stable inode numbers the driver assigns, since FAT itself has
//! no inodes: a node remembers where its directory entry lives.

#![no_std]

extern crate alloc;
#[cfg(test)]
extern crate std;

#[cfg(test)]
mod tests;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const SECTOR: usize = 512;
const EOC: u32 = 0x0FFF_FFF8;
const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0F;
const ATTR_VOLUME: u8 = 0x08;
pub const ROOT: u64 = 1;

pub trait SectorDisk {
    fn sectors(&self) -> u64;
    fn read(&mut self, lba: u64, buf: &mut [u8; SECTOR]) -> Result<()>;
    fn write(&mut self, lba: u64, buf: &[u8; SECTOR]) -> Result<()>;
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
    Unsupported,
    Corrupt,
    Invalid,
    NameTooLong,
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug)]
pub struct Meta {
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
    pub read_only: bool,
}

#[derive(Clone, Debug)]
struct Node {
    /// First cluster of the directory holding this node's entry (0 for the root itself).
    parent: u32,
    /// Byte offset of the short entry within the parent directory's data.
    offset: u64,
    first: u32,
    size: u32,
    is_dir: bool,
    attr: u8,
    mtime: u64,
}

struct RawEntry {
    name: String,
    /// Offset of the first slot used (LFN entries come before the short entry).
    start: u64,
    offset: u64,
    first: u32,
    size: u32,
    attr: u8,
    mtime: u64,
}

pub struct Fat32<D: SectorDisk> {
    disk: D,
    spc: u64,
    fat_start: u64,
    fat_sectors: u64,
    num_fats: u64,
    data_start: u64,
    root_cluster: u32,
    clusters: u32,
    free_hint: u32,
    free_count: u32,
    nodes: BTreeMap<u64, Node>,
    by_location: BTreeMap<(u32, u64), u64>,
    next_ino: u64,
    now: fn() -> u64,
}

fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}

/// Converts seconds since 2000-01-01 to FAT (date, time).
fn to_fat_time(secs: u64) -> (u16, u16) {
    let days = (secs / 86400) as i64 + 10957;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    let rem = secs % 86400;
    let date = (((y - 1980).clamp(0, 127) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time = (((rem / 3600) as u16) << 11) | (((rem / 60 % 60) as u16) << 5) | ((rem % 60 / 2) as u16);
    (date, time)
}

fn from_fat_time(date: u16, time: u16) -> u64 {
    let y = (date >> 9) as i64 + 1980;
    let m = ((date >> 5) & 0xF).clamp(1, 12) as i64;
    let d = (date & 0x1F).max(1) as i64;
    let (yy, mm) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let doy = (153 * mm + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 730425;
    let secs = (time >> 11) as u64 * 3600 + ((time >> 5) & 0x3F) as u64 * 60 + (time & 0x1F) as u64 * 2;
    days.max(0) as u64 * 86400 + secs
}

fn lfn_checksum(short: &[u8]) -> u8 {
    short.iter().fold(0u8, |sum, &c| (sum >> 1 | sum << 7).wrapping_add(c))
}

fn short_name_string(e: &[u8]) -> String {
    let base: String = e[0..8].iter().map(|&c| c as char).collect::<String>().trim_end().into();
    let ext: String = e[8..11].iter().map(|&c| c as char).collect::<String>().trim_end().into();
    let base = if e[12] & 0x08 != 0 { base.to_lowercase() } else { base };
    let ext = if e[12] & 0x10 != 0 { ext.to_lowercase() } else { ext };
    if ext.is_empty() {
        base
    } else {
        alloc::format!("{base}.{ext}")
    }
}

fn eq_ignore_case(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.chars().zip(b.chars()).all(|(x, y)| x.to_lowercase().eq(y.to_lowercase()))
}

impl<D: SectorDisk> Fat32<D> {
    pub fn mount(mut disk: D, now: fn() -> u64) -> Result<Fat32<D>> {
        let mut b = [0u8; SECTOR];
        disk.read(0, &mut b)?;
        if b[510] != 0x55 || b[511] != 0xAA || u16_at(&b, 11) as usize != SECTOR {
            return Err(Error::Unsupported);
        }
        let spc = b[13] as u64;
        let reserved = u16_at(&b, 14) as u64;
        let num_fats = b[16] as u64;
        let root_entries = u16_at(&b, 17);
        let fat16_size = u16_at(&b, 22);
        let total = if u16_at(&b, 19) != 0 { u16_at(&b, 19) as u64 } else { u32_at(&b, 32) as u64 };
        let fat_sectors = u32_at(&b, 36) as u64;
        if root_entries != 0 || fat16_size != 0 || spc == 0 || !spc.is_power_of_two() || num_fats == 0 {
            return Err(Error::Unsupported); // FAT12/16 or garbage
        }
        let data_start = reserved + num_fats * fat_sectors;
        if total > disk.sectors() || data_start >= total {
            return Err(Error::Corrupt);
        }
        let clusters = ((total - data_start) / spc) as u32;
        let mut fs = Fat32 {
            disk,
            spc,
            fat_start: reserved,
            fat_sectors,
            num_fats,
            data_start,
            root_cluster: u32_at(&b, 44),
            clusters,
            free_hint: 2,
            free_count: 0,
            nodes: BTreeMap::new(),
            by_location: BTreeMap::new(),
            next_ino: 2,
            now,
        };
        let root = Node {
            parent: 0,
            offset: 0,
            first: fs.root_cluster,
            size: 0,
            is_dir: true,
            attr: ATTR_DIRECTORY,
            mtime: 0,
        };
        fs.nodes.insert(ROOT, root);
        fs.free_count = fs.count_free()?;
        Ok(fs)
    }

    pub fn into_disk(self) -> D {
        self.disk
    }

    pub fn space(&self) -> (u64, u64) {
        let cs = self.spc * SECTOR as u64;
        (self.clusters as u64 * cs, self.free_count as u64 * cs)
    }

    pub fn flush(&mut self) -> Result<()> {
        self.disk.flush()
    }

    // ------------------------------------------------------------- FAT

    fn fat_get(&mut self, c: u32) -> Result<u32> {
        let off = c as u64 * 4;
        let mut b = [0u8; SECTOR];
        self.disk.read(self.fat_start + off / SECTOR as u64, &mut b)?;
        Ok(u32_at(&b, (off % SECTOR as u64) as usize) & 0x0FFF_FFFF)
    }

    fn fat_set(&mut self, c: u32, v: u32) -> Result<()> {
        let off = c as u64 * 4;
        for f in 0..self.num_fats {
            let lba = self.fat_start + f * self.fat_sectors + off / SECTOR as u64;
            let mut b = [0u8; SECTOR];
            self.disk.read(lba, &mut b)?;
            let o = (off % SECTOR as u64) as usize;
            let old = u32_at(&b, o);
            b[o..o + 4].copy_from_slice(&((old & 0xF000_0000) | (v & 0x0FFF_FFFF)).to_le_bytes());
            self.disk.write(lba, &b)?;
        }
        Ok(())
    }

    fn count_free(&mut self) -> Result<u32> {
        let mut free = 0;
        let mut b = [0u8; SECTOR];
        let entries_per_sector = (SECTOR / 4) as u32;
        for s in 0..(self.clusters + 2).div_ceil(entries_per_sector) {
            self.disk.read(self.fat_start + s as u64, &mut b)?;
            for i in 0..entries_per_sector {
                let c = s * entries_per_sector + i;
                if (2..self.clusters + 2).contains(&c) && u32_at(&b, i as usize * 4) & 0x0FFF_FFFF == 0 {
                    free += 1;
                }
            }
        }
        Ok(free)
    }

    fn chain(&mut self, first: u32) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        let mut c = first;
        while (2..EOC).contains(&c) {
            if c >= self.clusters + 2 || out.len() > self.clusters as usize {
                return Err(Error::Corrupt);
            }
            out.push(c);
            c = self.fat_get(c)?;
        }
        Ok(out)
    }

    fn alloc_cluster(&mut self, prev: Option<u32>) -> Result<u32> {
        let n = self.clusters;
        for i in 0..n {
            let c = 2 + (self.free_hint - 2 + i) % n;
            if self.fat_get(c)? == 0 {
                self.fat_set(c, 0x0FFF_FFFF)?;
                if let Some(p) = prev {
                    self.fat_set(p, c)?;
                }
                self.free_hint = c;
                self.free_count = self.free_count.saturating_sub(1);
                // Zero it (directories rely on this; files get overwritten).
                let zero = [0u8; SECTOR];
                for s in 0..self.spc {
                    self.disk.write(self.cluster_lba(c) + s, &zero)?;
                }
                return Ok(c);
            }
        }
        Err(Error::NoSpace)
    }

    fn free_chain(&mut self, first: u32) -> Result<()> {
        for c in self.chain(first)? {
            self.fat_set(c, 0)?;
            self.free_count += 1;
        }
        Ok(())
    }

    fn cluster_lba(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.spc
    }

    fn cluster_bytes(&self) -> usize {
        self.spc as usize * SECTOR
    }

    fn read_chain_data(&mut self, first: u32) -> Result<(Vec<u32>, Vec<u8>)> {
        let chain = self.chain(first)?;
        let mut data = vec![0u8; chain.len() * self.cluster_bytes()];
        let mut b = [0u8; SECTOR];
        for (i, &c) in chain.iter().enumerate() {
            for s in 0..self.spc {
                self.disk.read(self.cluster_lba(c) + s, &mut b)?;
                let o = i * self.cluster_bytes() + s as usize * SECTOR;
                data[o..o + SECTOR].copy_from_slice(&b);
            }
        }
        Ok((chain, data))
    }

    /// Writes the byte range `[from, to)` of a directory's data back to disk.
    fn write_dir_range(&mut self, chain: &[u32], data: &[u8], from: usize, to: usize) -> Result<()> {
        let first_sector = from / SECTOR;
        let last_sector = to.div_ceil(SECTOR);
        for s in first_sector..last_sector {
            let c = chain[s * SECTOR / self.cluster_bytes()];
            let within = (s * SECTOR % self.cluster_bytes()) / SECTOR;
            let mut b = [0u8; SECTOR];
            b.copy_from_slice(&data[s * SECTOR..(s + 1) * SECTOR]);
            self.disk.write(self.cluster_lba(c) + within as u64, &b)?;
        }
        Ok(())
    }

    // ------------------------------------------------------ directories

    fn dir_cluster(&self, ino: u64) -> Result<u32> {
        let n = self.nodes.get(&ino).ok_or(Error::NotFound)?;
        if !n.is_dir {
            return Err(Error::NotDir);
        }
        Ok(n.first)
    }

    fn parse_dir(&mut self, first: u32) -> Result<(Vec<u32>, Vec<u8>, Vec<RawEntry>)> {
        let (chain, data) = self.read_chain_data(first)?;
        let mut entries = Vec::new();
        let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
        let mut lfn_start = 0u64;
        for i in 0..data.len() / 32 {
            let e = &data[i * 32..(i + 1) * 32];
            if e[0] == 0 {
                break;
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if e[11] == ATTR_LFN {
                if e[0] & 0x40 != 0 {
                    lfn.clear();
                    lfn_start = (i * 32) as u64;
                }
                let mut part = [0u16; 13];
                for (k, off) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                    part[k] = u16_at(e, *off);
                }
                lfn.push((e[0] & 0x1F, part));
                continue;
            }
            if e[11] & ATTR_VOLUME != 0 || e[0] == b'.' {
                lfn.clear();
                continue;
            }
            let short = short_name_string(e);
            let name = if !lfn.is_empty() {
                lfn.sort_by_key(|(ord, _)| *ord);
                let units: Vec<u16> =
                    lfn.iter().flat_map(|(_, p)| p.iter().copied()).take_while(|&u| u != 0 && u != 0xFFFF).collect();
                String::from_utf16(&units).unwrap_or(short)
            } else {
                short
            };
            let start = if lfn.is_empty() { (i * 32) as u64 } else { lfn_start };
            lfn.clear();
            entries.push(RawEntry {
                name,
                start,
                offset: (i * 32) as u64,
                first: (u16_at(e, 20) as u32) << 16 | u16_at(e, 26) as u32,
                size: u32_at(e, 28),
                attr: e[11],
                mtime: from_fat_time(u16_at(e, 24), u16_at(e, 22)),
            });
        }
        Ok((chain, data, entries))
    }

    fn node_for(&mut self, dir_first: u32, e: &RawEntry) -> u64 {
        let key = (dir_first, e.offset);
        let node = Node {
            parent: dir_first,
            offset: e.offset,
            first: e.first,
            size: e.size,
            is_dir: e.attr & ATTR_DIRECTORY != 0,
            attr: e.attr,
            mtime: e.mtime,
        };
        if let Some(&ino) = self.by_location.get(&key) {
            self.nodes.insert(ino, node);
            return ino;
        }
        let ino = self.next_ino;
        self.next_ino += 1;
        self.nodes.insert(ino, node);
        self.by_location.insert(key, ino);
        ino
    }

    pub fn readdir(&mut self, dir: u64) -> Result<Vec<(String, u64, bool)>> {
        let first = self.dir_cluster(dir)?;
        let (_, _, entries) = self.parse_dir(first)?;
        let mut out = Vec::new();
        for e in &entries {
            let ino = self.node_for(first, e);
            out.push((e.name.clone(), ino, e.attr & ATTR_DIRECTORY != 0));
        }
        Ok(out)
    }

    pub fn lookup(&mut self, dir: u64, name: &str) -> Result<u64> {
        let first = self.dir_cluster(dir)?;
        let (_, _, entries) = self.parse_dir(first)?;
        let e = entries.iter().find(|e| eq_ignore_case(&e.name, name)).ok_or(Error::NotFound)?;
        Ok(self.node_for(first, e))
    }

    pub fn stat(&mut self, ino: u64) -> Result<Meta> {
        let n = self.nodes.get(&ino).ok_or(Error::NotFound)?;
        Ok(Meta { is_dir: n.is_dir, size: n.size as u64, mtime: n.mtime, read_only: n.attr & ATTR_READ_ONLY != 0 })
    }

    /// Rewrites a node's short directory entry from its in-memory state.
    fn store_entry(&mut self, ino: u64) -> Result<()> {
        let n = self.nodes.get(&ino).ok_or(Error::NotFound)?.clone();
        if ino == ROOT {
            return Ok(());
        }
        let (chain, mut data, _) = self.parse_dir(n.parent)?;
        let o = n.offset as usize;
        let e = &mut data[o..o + 32];
        e[20..22].copy_from_slice(&((n.first >> 16) as u16).to_le_bytes());
        e[26..28].copy_from_slice(&(n.first as u16).to_le_bytes());
        e[28..32].copy_from_slice(&(if n.is_dir { 0 } else { n.size }).to_le_bytes());
        let (date, time) = to_fat_time(n.mtime);
        e[22..24].copy_from_slice(&time.to_le_bytes());
        e[24..26].copy_from_slice(&date.to_le_bytes());
        self.write_dir_range(&chain, &data, o, o + 32)
    }

    /// A unique 8.3 alias for `name`: the name itself when it is a valid
    /// upper-case 8.3 name, otherwise `BASE~N.EXT`.
    fn short_alias(name: &str, existing: &[[u8; 11]]) -> [u8; 11] {
        let valid = |b: &u8| b.is_ascii_uppercase() || b.is_ascii_digit() || b"!#$%&'()-@^_`{}~".contains(b);
        let (base, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
            _ => (name, ""),
        };
        let mut out = [b' '; 11];
        if (1..=8).contains(&base.len())
            && ext.len() <= 3
            && base.bytes().all(|b| valid(&b))
            && ext.bytes().all(|b| valid(&b))
        {
            out[..base.len()].copy_from_slice(base.as_bytes());
            out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
            if !existing.contains(&out) {
                return out;
            }
        }
        let clean = |s: &str, max: usize| -> Vec<u8> { s.to_uppercase().bytes().filter(valid).take(max).collect() };
        let mut stem = clean(base, 6);
        if stem.is_empty() {
            stem = b"FILE".to_vec();
        }
        let ext = clean(ext, 3);
        out[8..8 + ext.len()].copy_from_slice(&ext);
        for n in 1..1_000_000u32 {
            let tail = alloc::format!("~{n}");
            let keep = stem.len().min(8 - tail.len());
            let mut t = out;
            t[..keep].copy_from_slice(&stem[..keep]);
            t[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
            if !existing.contains(&t) {
                return t;
            }
        }
        out
    }

    /// Inserts entries (LFN + short) for `name` into directory `first`.
    fn insert_entry(&mut self, first: u32, name: &str, attr: u8, cluster: u32, size: u32) -> Result<u64> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|'])
        {
            return Err(Error::Invalid);
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        if units.len() > 255 {
            return Err(Error::NameTooLong);
        }
        let (mut chain, mut data, entries) = self.parse_dir(first)?;
        if entries.iter().any(|e| eq_ignore_case(&e.name, name)) {
            return Err(Error::Exists);
        }
        let existing: Vec<[u8; 11]> =
            (0..data.len() / 32).map(|i| data[i * 32..i * 32 + 11].try_into().unwrap()).collect();
        let short = Self::short_alias(name, &existing);
        let lfn_count = units.len().div_ceil(13);
        let slots = lfn_count + 1;
        // Find `slots` consecutive free entries, extending the directory if needed.
        let find = |data: &[u8]| -> Option<usize> {
            let mut run = 0;
            for i in 0..data.len() / 32 {
                let b0 = data[i * 32];
                if b0 == 0 || b0 == 0xE5 {
                    run += 1;
                    if run == slots {
                        return Some(i + 1 - slots);
                    }
                } else {
                    run = 0;
                }
            }
            None
        };
        let start = loop {
            if let Some(s) = find(&data) {
                break s;
            }
            let c = self.alloc_cluster(chain.last().copied())?;
            chain.push(c);
            data.extend(core::iter::repeat_n(0u8, self.cluster_bytes()));
        };
        let checksum = lfn_checksum(&short);
        for k in 0..lfn_count {
            let ord = (lfn_count - k) as u8;
            let slot = start + k;
            let e = &mut data[slot * 32..slot * 32 + 32];
            e.fill(0);
            e[0] = ord | if k == 0 { 0x40 } else { 0 };
            e[11] = ATTR_LFN;
            e[13] = checksum;
            let chunk = (ord as usize - 1) * 13;
            for (j, off) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                let u = match units.get(chunk + j) {
                    Some(&u) => u,
                    None if chunk + j == units.len() => 0,
                    None => 0xFFFF,
                };
                e[*off..*off + 2].copy_from_slice(&u.to_le_bytes());
            }
        }
        let slot = start + lfn_count;
        let (date, time) = to_fat_time((self.now)());
        let e = &mut data[slot * 32..slot * 32 + 32];
        e.fill(0);
        e[0..11].copy_from_slice(&short);
        e[11] = attr;
        e[14..16].copy_from_slice(&time.to_le_bytes());
        e[16..18].copy_from_slice(&date.to_le_bytes());
        e[18..20].copy_from_slice(&date.to_le_bytes());
        e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        e[22..24].copy_from_slice(&time.to_le_bytes());
        e[24..26].copy_from_slice(&date.to_le_bytes());
        e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        e[28..32].copy_from_slice(&size.to_le_bytes());
        self.write_dir_range(&chain, &data, start * 32, (slot + 1) * 32)?;
        let raw = RawEntry {
            name: String::from(name),
            start: (start * 32) as u64,
            offset: (slot * 32) as u64,
            first: cluster,
            size,
            attr,
            mtime: (self.now)(),
        };
        Ok(self.node_for(first, &raw))
    }

    /// Marks an entry (and its LFN slots) deleted; returns the raw entry.
    fn remove_entry(&mut self, first: u32, name: &str) -> Result<RawEntry> {
        let (chain, mut data, entries) = self.parse_dir(first)?;
        let e = entries.into_iter().find(|e| eq_ignore_case(&e.name, name)).ok_or(Error::NotFound)?;
        for o in (e.start..=e.offset).step_by(32) {
            data[o as usize] = 0xE5;
        }
        self.write_dir_range(&chain, &data, e.start as usize, e.offset as usize + 32)?;
        if let Some(ino) = self.by_location.remove(&(first, e.offset)) {
            self.nodes.remove(&ino);
        }
        Ok(e)
    }

    pub fn create(&mut self, dir: u64, name: &str, is_dir: bool) -> Result<u64> {
        let first = self.dir_cluster(dir)?;
        if !is_dir {
            return self.insert_entry(first, name, ATTR_ARCHIVE, 0, 0);
        }
        let c = self.alloc_cluster(None)?;
        let ino = match self.insert_entry(first, name, ATTR_DIRECTORY, c, 0) {
            Ok(i) => i,
            Err(e) => {
                self.free_chain(c)?;
                return Err(e);
            }
        };
        // "." and ".." entries.
        let mut sector = [0u8; SECTOR];
        let (date, time) = to_fat_time((self.now)());
        let parent = if first == self.root_cluster { 0 } else { first };
        for (i, (nm, cl)) in [(b".          ", c), (b"..         ", parent)].iter().enumerate() {
            let e = &mut sector[i * 32..i * 32 + 32];
            e[0..11].copy_from_slice(*nm);
            e[11] = ATTR_DIRECTORY;
            e[20..22].copy_from_slice(&((cl >> 16) as u16).to_le_bytes());
            e[22..24].copy_from_slice(&time.to_le_bytes());
            e[24..26].copy_from_slice(&date.to_le_bytes());
            e[26..28].copy_from_slice(&(*cl as u16).to_le_bytes());
        }
        let lba = self.cluster_lba(c);
        self.disk.write(lba, &sector)?;
        Ok(ino)
    }

    pub fn unlink(&mut self, dir: u64, name: &str) -> Result<()> {
        let first = self.dir_cluster(dir)?;
        let ino = self.lookup(dir, name)?;
        let n = self.nodes[&ino].clone();
        if n.is_dir && !self.readdir(ino)?.is_empty() {
            return Err(Error::NotEmpty);
        }
        self.remove_entry(first, name)?;
        if n.first >= 2 {
            self.free_chain(n.first)?;
        }
        Ok(())
    }

    pub fn rename(&mut self, from_dir: u64, from: &str, to_dir: u64, to: &str) -> Result<()> {
        let src = self.dir_cluster(from_dir)?;
        let dst = self.dir_cluster(to_dir)?;
        let ino = self.lookup(from_dir, from)?;
        let n = self.nodes[&ino].clone();
        if let Ok(existing) = self.lookup(to_dir, to) {
            if existing == ino {
                if from == to {
                    return Ok(());
                }
            } else {
                if self.nodes[&existing].is_dir || n.is_dir {
                    return Err(Error::Exists);
                }
                self.unlink(to_dir, to)?;
            }
        }
        self.remove_entry(src, from)?;
        let new = self.insert_entry(dst, to, n.attr, n.first, n.size)?;
        if n.is_dir && src != dst {
            // Point ".." at the new parent.
            let mut b = [0u8; SECTOR];
            let lba = self.cluster_lba(n.first);
            self.disk.read(lba, &mut b)?;
            let parent = if dst == self.root_cluster { 0 } else { dst };
            b[32 + 20..32 + 22].copy_from_slice(&((parent >> 16) as u16).to_le_bytes());
            b[32 + 26..32 + 28].copy_from_slice(&(parent as u16).to_le_bytes());
            self.disk.write(lba, &b)?;
        }
        let _ = new;
        Ok(())
    }

    // ------------------------------------------------------------ files

    pub fn read(&mut self, ino: u64, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let n = self.nodes.get(&ino).ok_or(Error::NotFound)?.clone();
        if n.is_dir {
            return Err(Error::IsDir);
        }
        if offset >= n.size as u64 {
            return Ok(0);
        }
        let len = buf.len().min((n.size as u64 - offset) as usize);
        let cb = self.cluster_bytes() as u64;
        let chain = self.chain(n.first)?;
        let mut done = 0;
        let mut sector = [0u8; SECTOR];
        while done < len {
            let pos = offset + done as u64;
            let c = *chain.get((pos / cb) as usize).ok_or(Error::Corrupt)?;
            let within = pos % cb;
            let lba = self.cluster_lba(c) + within / SECTOR as u64;
            self.disk.read(lba, &mut sector)?;
            let so = (within % SECTOR as u64) as usize;
            let k = (SECTOR - so).min(len - done);
            buf[done..done + k].copy_from_slice(&sector[so..so + k]);
            done += k;
        }
        Ok(len)
    }

    pub fn write(&mut self, ino: u64, offset: u64, data: &[u8]) -> Result<usize> {
        let mut n = self.nodes.get(&ino).ok_or(Error::NotFound)?.clone();
        if n.is_dir {
            return Err(Error::IsDir);
        }
        let end = offset.checked_add(data.len() as u64).ok_or(Error::Invalid)?;
        if end > u32::MAX as u64 {
            return Err(Error::NoSpace);
        }
        let cb = self.cluster_bytes() as u64;
        let mut chain = if n.first >= 2 { self.chain(n.first)? } else { Vec::new() };
        while (chain.len() as u64) * cb < end {
            let c = self.alloc_cluster(chain.last().copied())?;
            if chain.is_empty() {
                n.first = c;
            }
            chain.push(c);
        }
        // Zero-fill a gap between the old size and `offset` (fresh clusters are already zero).
        let mut sector = [0u8; SECTOR];
        let mut pos = (n.size as u64).min(offset);
        while pos < end {
            let c = chain[(pos / cb) as usize];
            let within = pos % cb;
            let lba = self.cluster_lba(c) + within / SECTOR as u64;
            let so = (within % SECTOR as u64) as usize;
            let k = ((SECTOR - so) as u64).min(end - pos) as usize;
            if so != 0 || k != SECTOR {
                self.disk.read(lba, &mut sector)?;
            }
            for i in 0..k {
                let p = pos + i as u64;
                sector[so + i] = if p >= offset { data[(p - offset) as usize] } else { 0 };
            }
            self.disk.write(lba, &sector)?;
            pos += k as u64;
        }
        n.size = n.size.max(end as u32);
        n.mtime = (self.now)();
        self.nodes.insert(ino, n);
        self.store_entry(ino)?;
        Ok(data.len())
    }

    pub fn truncate(&mut self, ino: u64, size: u64) -> Result<()> {
        let mut n = self.nodes.get(&ino).ok_or(Error::NotFound)?.clone();
        if n.is_dir {
            return Err(Error::IsDir);
        }
        if size > n.size as u64 {
            let zeros = vec![0u8; (size - n.size as u64) as usize];
            return self.write(ino, n.size as u64, &zeros).map(|_| ());
        }
        let cb = self.cluster_bytes() as u64;
        let keep = size.div_ceil(cb) as usize;
        if n.first >= 2 {
            let chain = self.chain(n.first)?;
            if keep == 0 {
                self.free_chain(n.first)?;
                n.first = 0;
            } else if keep < chain.len() {
                self.free_chain(chain[keep])?;
                self.fat_set(chain[keep - 1], 0x0FFF_FFFF)?;
            }
        }
        n.size = size as u32;
        n.mtime = (self.now)();
        self.nodes.insert(ino, n);
        self.store_entry(ino)
    }
}
