//! Pages of mapped files (Linux `mmap`), read once and shared.
//!
//! A program's libraries are mapped by every process that runs it (Firefox
//! maps its 170 MB `libxul.so` in each of its processes, twice). Without
//! this, each mapping read the file again and held a copy. Now a file's
//! pages are read on first use into counted frames (`mm::vmm::shm_alloc`)
//! that every mapping of it shares, read-only and marked copy-on-write: a
//! process that makes one writable (`mprotect`) gets a copy of its own.
//!
//! Writing to a file, truncating or deleting it drops it from the cache
//! ([`forget`]; mappings made before keep what they had, as private
//! mappings do). The cache keeps up to a quarter of memory; past that, the
//! files used longest ago are dropped (their pages live on while mapped).

use super::{Filesystem, Ino};
use crate::mm::vmm::{self, AddressSpace, Prot};
use crate::mm::PAGE_SIZE;
use crate::sync::Mutex;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicU64, Ordering};

/// A file in the cache. Its pages are read under its own lock, so a long
/// read holds up only those who want the same file.
struct Cached {
    fs: usize,
    ino: Ino,
    size: u64,
    /// One per page of the file, read when first wanted (0: not yet).
    frames: Mutex<Vec<u64>>,
    /// When it was last used (a counter, for dropping the oldest).
    used: AtomicU64,
}

impl Drop for Cached {
    /// The cache's hold on the pages goes (mappings keep theirs).
    fn drop(&mut self) {
        let mut n = 0;
        for &f in self.frames.lock().iter().filter(|&&f| f != 0) {
            vmm::shm_release(f);
            n += 1;
        }
        PAGES.fetch_sub(n, Ordering::Relaxed);
    }
}

static FILES: Mutex<Vec<Arc<Cached>>> = Mutex::new(Vec::new());
/// Pages held by cached files.
static PAGES: AtomicU64 = AtomicU64::new(0);
static CLOCK: AtomicU64 = AtomicU64::new(0);

/// What a read is done in: 256 KiB at a time.
const CHUNK: usize = 64 * PAGE_SIZE as usize;

/// Pages of a file, held for a mapping: [`Pages::map`] puts them in an
/// address space, and dropping lets go of what wasn't mapped.
pub struct Pages {
    frames: Vec<u64>,
    /// Pages wanted past the end of the file (zero-filled when mapped).
    beyond: u64,
}

/// `count` pages of the file from page-aligned `offset`, read if they
/// aren't cached yet. May wait for the disk: call it before taking an
/// address space's lock.
pub fn pages(fs: &Arc<dyn Filesystem>, ino: Ino, offset: u64, count: u64) -> Result<Pages, isize> {
    let size = fs.metadata(ino)?.size;
    let first = offset / PAGE_SIZE;
    let in_file = size.div_ceil(PAGE_SIZE).saturating_sub(first).min(count);
    let file = entry(fs, ino, size);
    let frames = read(&file, fs, first, in_file)?;
    trim(&file);
    Ok(Pages { frames, beyond: count - in_file })
}

impl Pages {
    /// Maps the pages at `va` with `prot`: shared read-only, and copies
    /// where `prot` allows writing.
    pub fn map(self, aspace: &mut AddressSpace, va: u64, prot: Prot) -> Result<(), isize> {
        aspace.map_cached(va, &self.frames, prot)?;
        if self.beyond > 0 {
            aspace.map_anon(va + self.frames.len() as u64 * PAGE_SIZE, self.beyond, prot)?;
        }
        Ok(())
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // The hold taken for the mapping (which took its own for each page).
        for &f in &self.frames {
            vmm::shm_release(f);
        }
    }
}

/// The cache entry for the file, made if there's none (or the size changed).
fn entry(fs: &Arc<dyn Filesystem>, ino: Ino, size: u64) -> Arc<Cached> {
    let id = fs_id(fs);
    let mut files = FILES.lock();
    let file = match files.iter().position(|c| c.fs == id && c.ino == ino) {
        Some(i) if files[i].size == size => files[i].clone(),
        stale => {
            if let Some(i) = stale {
                files.swap_remove(i);
            }
            let pages = size.div_ceil(PAGE_SIZE) as usize;
            let file =
                Arc::new(Cached { fs: id, ino, size, frames: Mutex::new(vec![0; pages]), used: AtomicU64::new(0) });
            files.push(file.clone());
            file
        }
    };
    file.used.store(CLOCK.fetch_add(1, Ordering::Relaxed), Ordering::Relaxed);
    file
}

/// Frames for pages `first..first + count`, read where missing, each held
/// once more for the caller.
fn read(file: &Cached, fs: &Arc<dyn Filesystem>, first: u64, count: u64) -> Result<Vec<u64>, isize> {
    let mut frames = file.frames.lock();
    let end = first + count;
    let mut page = first;
    let mut buf = vec![0u8; CHUNK];
    while page < end {
        if frames[page as usize] != 0 {
            page += 1;
            continue;
        }
        let run = (page..end).take_while(|&p| frames[p as usize] == 0).count().min(CHUNK / PAGE_SIZE as usize) as u64;
        let want = (run * PAGE_SIZE).min(file.size - page * PAGE_SIZE) as usize;
        let got = fs.read(file.ino, page * PAGE_SIZE, &mut buf[..want])?;
        for i in 0..run {
            let start = ((i * PAGE_SIZE) as usize).min(got);
            let f = vmm::shm_alloc_with(&buf[start..got.min(start + PAGE_SIZE as usize)]).ok_or(ENOMEM)?;
            frames[(page + i) as usize] = f;
            PAGES.fetch_add(1, Ordering::Relaxed);
        }
        page += run;
    }
    let out: Vec<u64> = frames[first as usize..end as usize].to_vec();
    for &f in &out {
        vmm::shm_hold(f);
    }
    Ok(out)
}

/// Drops the files used longest ago (never `keep`) while the cache holds
/// more than a quarter of memory.
fn trim(keep: &Arc<Cached>) {
    let limit = crate::mm::frame::counts().0 / 4;
    let mut files = FILES.lock();
    while PAGES.load(Ordering::Relaxed) > limit {
        let oldest = (0..files.len())
            .filter(|&i| !Arc::ptr_eq(&files[i], keep))
            .min_by_key(|&i| files[i].used.load(Ordering::Relaxed));
        let Some(oldest) = oldest else { return };
        // Freed when the last user of the entry lets go of it.
        files.swap_remove(oldest);
    }
}

fn fs_id(fs: &Arc<dyn Filesystem>) -> usize {
    Arc::as_ptr(fs) as *const () as usize
}

/// The file changed (written, truncated or deleted): later mappings read it afresh.
pub fn forget(fs: &Arc<dyn Filesystem>, ino: Ino) {
    let id = fs_id(fs);
    let mut files = FILES.lock();
    if let Some(i) = files.iter().position(|c| c.fs == id && c.ino == ino) {
        files.swap_remove(i);
    }
}
