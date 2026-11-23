//! The swap file: pages that don't fit in the compressed store go to
//! `/Security/swapfile` on the system volume.
//!
//! **When.** Nothing happens at boot. The first time memory runs short,
//! `kswapd` starts [`activate`] on a task of its own (the compressed store
//! goes on meanwhile). It uses the file left by an earlier boot, or makes
//! one: half the size of memory, at most 2 GiB and at most an eighth of
//! the volume's free space; under 4 MiB there is no swap file. A volume
//! that is read-only, full, or not a disk (the in-memory root) has none.
//!
//! **How.** The filesystem allocates the file's blocks without writing
//! them, says where they are, and refuses every access to the file from
//! then on (`Filesystem::swap_file`); pages are then read and written
//! straight to those sectors, never through the block cache, so a page
//! can be swapped in or out while the filesystem is busy.
//!
//! **Privacy.** Every page is encrypted (ChaCha20, `random::chacha_block`)
//! with a key made at boot and kept only in memory, and a nonce of its
//! own for each write, so what is on the disk can't be read by another
//! process (the kernel never gives a freed page's data to anyone) or after
//! a reboot (the key is gone).
//!
//! **Errors.** A write that fails leaves the page in memory; after a few,
//! the file is used no more for writing (pages already there are still
//! read). A page that can't be read back is lost: its process gets a
//! fault there (Linux: `SIGBUS`).

use super::PAGE_SIZE;
use crate::drivers::block::BlockDevice;
use crate::sync::IrqMutex;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const PATH: &str = "/Security/swapfile";
const MIN_BYTES: u64 = 4 << 20;
const MAX_BYTES: u64 = 2 << 30;
/// Write errors after which the file takes no more pages.
const MAX_ERRORS: u32 = 4;

struct File {
    dev: Arc<dyn BlockDevice>,
    /// (first page, first sector, pages).
    runs: Vec<(u64, u64, u64)>,
    sectors_per_page: u64,
    /// One bit per page: in use.
    used: Vec<u64>,
    /// Where the next search for a free page starts (so pages taken one
    /// after another lie one after another).
    next: u32,
    key: [u32; 8],
    errors: u32,
}

impl File {
    fn sector(&self, page: u32) -> u64 {
        let p = page as u64;
        let i = self.runs.partition_point(|r| r.0 <= p) - 1;
        let (first, sector, _) = self.runs[i];
        sector + (p - first) * self.sectors_per_page
    }
}

static FILE: IrqMutex<Option<File>> = IrqMutex::new(None);
static PAGES: AtomicU64 = AtomicU64::new(0);
static FREE: AtomicU64 = AtomicU64::new(0);
static NONCE: AtomicU64 = AtomicU64::new(1);
static STARTED: AtomicBool = AtomicBool::new(false);
static READS: AtomicU64 = AtomicU64::new(0);
static WRITES: AtomicU64 = AtomicU64::new(0);
/// Why there is no swap file, for reports (0: there is one or none was tried).
static WHY_NOT: AtomicU64 = AtomicU64::new(0);

/// Pages the swap file holds (0 without one).
pub fn pages() -> u64 {
    PAGES.load(Ordering::Relaxed)
}

/// Pages of it free for writing.
pub fn free_pages() -> u64 {
    FREE.load(Ordering::Relaxed)
}

/// (reads, writes) of pages since boot.
pub fn io_counts() -> (u64, u64) {
    (READS.load(Ordering::Relaxed), WRITES.load(Ordering::Relaxed))
}

/// The error that kept the swap file from being used, if any.
pub fn failure() -> Option<isize> {
    match WHY_NOT.load(Ordering::Relaxed) {
        0 => None,
        e => Some(e as isize),
    }
}

/// Starts [`activate`] on a task of its own, once per boot.
pub fn start() {
    if !STARTED.swap(true, Ordering::AcqRel) {
        crate::sched::spawn("swapon", || {
            let _ = activate();
        });
    }
}

/// The size the swap file should have: half of memory, between 4 MiB and
/// 2 GiB, and no more than an eighth of the volume's free space.
fn wanted_bytes(free_space: u64) -> u64 {
    let ram = super::frame::counts().0 * PAGE_SIZE;
    let want = (ram / 2).clamp(MIN_BYTES, MAX_BYTES).min(free_space / 8);
    want & !(PAGE_SIZE - 1)
}

/// Finds or makes the swap file and starts using it; returns its pages.
pub fn activate() -> Result<u64, isize> {
    use crate::fs::{self, Kind, Owner};
    let result = (|| {
        if pages() > 0 {
            return Ok(pages());
        }
        if !fs::root_persistent() {
            return Err(fs::ENODEV);
        }
        let (root, _) = fs::lookup("/")?;
        if root.read_only() {
            return Err(fs::EROFS);
        }
        let (dir_fs, dir) = match fs::lookup("/Security") {
            Ok(found) => found,
            Err(_) => {
                fs::perm::system(|| fs::mkdir("/Security"))?;
                fs::lookup("/Security")?
            }
        };
        let (_, free_space) = dir_fs.space().ok_or(fs::EOPNOTSUPP)?;
        let existing = dir_fs.lookup(dir, "swapfile").ok();
        let have = existing.and_then(|ino| dir_fs.metadata(ino).ok()).map_or(0, |m| m.size);
        // One left by an earlier boot is used as it is (free space counts it as used).
        let size = if have >= MIN_BYTES { have.min(MAX_BYTES) } else { wanted_bytes(free_space + have) };
        if size < MIN_BYTES {
            return Err(fs::ENOSPC);
        }
        let ino = match existing {
            Some(ino) => ino,
            None => dir_fs.create_owned(dir, "swapfile", Kind::File, Some(Owner { uid: 0, gid: 0, mode: 0o600 }))?,
        };
        let extents = match dir_fs.swap_file(ino, size) {
            Ok(e) => e,
            Err(e) => {
                if existing.is_none() {
                    let _ = dir_fs.unlink(dir, "swapfile");
                }
                return Err(e);
            }
        };
        let sector = extents.dev.sector_size() as u64;
        if !PAGE_SIZE.is_multiple_of(sector) {
            return Err(fs::EINVAL);
        }
        let total: u64 = extents.runs.iter().map(|r| r.2).sum::<u64>().min(size / PAGE_SIZE);
        let mut key = [0u8; 32];
        crate::random::fill(&mut key);
        let mut k = [0u32; 8];
        for (w, b) in k.iter_mut().zip(key.as_chunks::<4>().0) {
            *w = u32::from_le_bytes(*b);
        }
        let file = File {
            dev: extents.dev,
            runs: extents.runs,
            sectors_per_page: PAGE_SIZE / sector,
            used: alloc::vec![0; total.div_ceil(64) as usize],
            next: 0,
            key: k,
            errors: 0,
        };
        *FILE.lock() = Some(file);
        FREE.store(total, Ordering::Relaxed);
        PAGES.store(total, Ordering::Release);
        log!("swap", "swap file {} ready: {} MiB", PATH, (total * PAGE_SIZE) >> 20);
        Ok(total)
    })();
    match result {
        Ok(_) => WHY_NOT.store(0, Ordering::Relaxed),
        Err(e) => {
            WHY_NOT.store(e.unsigned_abs() as u64, Ordering::Relaxed);
            log!("swap", "no swap file: {}", aurora_abi::err::name(e));
        }
    }
    result
}

/// XORs `buf` with the keystream for `nonce`: one ChaCha20 block per 64
/// bytes, the block counter being the nonce and the block's index.
fn crypt(key: &[u32; 8], nonce: u64, buf: &mut [u8]) {
    for (i, chunk) in buf.chunks_mut(64).enumerate() {
        let ks = crate::random::chacha_block(key, nonce << 6 | i as u64);
        for (j, b) in chunk.iter_mut().enumerate() {
            *b ^= (ks[j / 4] >> (8 * (j % 4))) as u8;
        }
    }
}

/// Pages one request moves at most.
pub const RUN: usize = 32;

/// The buffer requests of several pages go through (pages are encrypted
/// into it, never in place). A static one: memory is short when it's used.
static IO: crate::sync::Mutex<[u8; RUN * PAGE_SIZE as usize]> = crate::sync::Mutex::new([0; RUN * PAGE_SIZE as usize]);

impl File {
    fn is_used(&self, page: u32) -> bool {
        self.used[page as usize / 64] & (1 << (page % 64)) != 0
    }

    /// Whether pages `at..at + n` lie one after another on the disk.
    fn contiguous(&self, at: u32, n: u32) -> bool {
        let run = |p: u64| self.runs.partition_point(|r| r.0 <= p);
        run(at as u64) == run(at as u64 + n as u64 - 1)
    }

    /// `n` free pages in a row on the disk, from where the last ones were
    /// taken (so pages swapped out together lie together, for read-ahead).
    fn find(&self, n: u32) -> Option<u32> {
        let total = PAGES.load(Ordering::Relaxed) as u32;
        let mut at = self.next;
        let mut looked = 0;
        while looked <= total {
            if at + n > total {
                looked += total - at;
                at = 0;
                continue;
            }
            match (0..n).find(|&k| self.is_used(at + k)) {
                None if self.contiguous(at, n) => return Some(at),
                None => {
                    at += 1;
                    looked += 1;
                }
                Some(k) => {
                    at += k + 1;
                    looked += k + 1;
                }
            }
        }
        None
    }

    /// Notes a failed write.
    fn error(&mut self) {
        self.errors += 1;
        if self.errors == MAX_ERRORS {
            log!("swap", "the swap file failed {} writes: it takes no more pages", MAX_ERRORS);
        }
    }
}

/// Writes a page to a free page of the file; returns where, and the
/// nonce it was encrypted with. None if the file is full, missing or failing.
pub fn write(page: &[u8]) -> Option<(u32, u64)> {
    write_run(&[page])
}

/// Writes `pages` (at most [`RUN`]) to pages of the file one after another
/// on the disk, in one request: page `i` goes to the returned page plus
/// `i`, encrypted with the returned nonce plus `i`. None (and nothing
/// written) if there is no such room, or on a disk error.
pub fn write_run(pages: &[&[u8]]) -> Option<(u32, u64)> {
    let n = pages.len() as u32;
    if n == 0 || n as usize > RUN {
        return None;
    }
    let (at, sector, dev, key) = {
        let mut g = FILE.lock();
        let f = g.as_mut()?;
        if f.errors >= MAX_ERRORS || FREE.load(Ordering::Relaxed) < n as u64 {
            return None;
        }
        let at = f.find(n)?;
        for p in at..at + n {
            f.used[p as usize / 64] |= 1 << (p % 64);
        }
        f.next = (at + n) % PAGES.load(Ordering::Relaxed) as u32;
        FREE.fetch_sub(n as u64, Ordering::Relaxed);
        (at, f.sector(at), f.dev.clone(), f.key)
    };
    let nonce = NONCE.fetch_add(n as u64, Ordering::Relaxed);
    let ok = {
        let mut io = IO.lock();
        for (i, page) in pages.iter().enumerate() {
            let chunk = &mut io[i * PAGE_SIZE as usize..(i + 1) * PAGE_SIZE as usize];
            chunk.copy_from_slice(page);
            crypt(&key, nonce + i as u64, chunk);
        }
        dev.write(sector, &io[..n as usize * PAGE_SIZE as usize]).is_ok()
    };
    if !ok {
        if let Some(f) = FILE.lock().as_mut() {
            f.error();
        }
        for p in at..at + n {
            free(p);
        }
        return None;
    }
    WRITES.fetch_add(n as u64, Ordering::Relaxed);
    Some((at, nonce))
}

/// Reads page `at` (written with `nonce`) into `page`; false on a disk error.
pub fn read(at: u32, nonce: u64, page: &mut [u8]) -> bool {
    let Some((sector, dev, key)) = FILE.lock().as_ref().map(|f| (f.sector(at), f.dev.clone(), f.key)) else {
        return false;
    };
    if dev.read(sector, page).is_err() {
        return false;
    }
    crypt(&key, nonce, page);
    READS.fetch_add(1, Ordering::Relaxed);
    true
}

/// Reads pages `at..at + n` (written with `nonces`) into `pages` in one
/// request (read-ahead); false if they don't lie together on the disk, or
/// on a disk error.
pub fn read_run(at: u32, nonces: &[u64], pages: &mut [&mut [u8]]) -> bool {
    let n = nonces.len().min(pages.len()).min(RUN) as u32;
    let found = FILE.lock().as_ref().and_then(|f| f.contiguous(at, n).then(|| (f.sector(at), f.dev.clone(), f.key)));
    let Some((sector, dev, key)) = found else { return false };
    let mut io = IO.lock();
    if dev.read(sector, &mut io[..n as usize * PAGE_SIZE as usize]).is_err() {
        return false;
    }
    for i in 0..n as usize {
        pages[i].copy_from_slice(&io[i * PAGE_SIZE as usize..(i + 1) * PAGE_SIZE as usize]);
        crypt(&key, nonces[i], pages[i]);
    }
    READS.fetch_add(n as u64, Ordering::Relaxed);
    true
}

/// Gives page `at` back (its contents are left: encrypted, they are noise).
pub fn free(at: u32) {
    let mut g = FILE.lock();
    let Some(f) = g.as_mut() else { return };
    let bit = 1u64 << (at % 64);
    if let Some(w) = f.used.get_mut(at as usize / 64) {
        if *w & bit != 0 {
            *w &= !bit;
            FREE.fetch_add(1, Ordering::Relaxed);
        }
    }
}
