//! Shared memory for Linux programs: `memfd_create`, files in `/dev/shm`
//! (`shm_open`), and shared anonymous mappings. An object is a list of
//! frames; mapping it with `MAP_SHARED` puts the same frames in each
//! address space (they are reference-counted in `mm::vmm`, so they last
//! while anything maps them). Reading and writing the descriptor goes
//! straight to the frames.

use crate::mm::vmm;
use crate::mm::{phys_to_virt, PAGE_SIZE};
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

/// Largest object (Firefox's shared buffers are far smaller).
const MAX_SIZE: u64 = 4 << 30;

pub struct ShmFile {
    inner: Mutex<Inner>,
}

struct Inner {
    frames: Vec<u64>,
    size: u64,
}

impl ShmFile {
    pub fn new(size: u64) -> Result<Arc<ShmFile>, isize> {
        let f = Arc::new(ShmFile { inner: Mutex::new(Inner { frames: Vec::new(), size: 0 }) });
        f.resize(size)?;
        Ok(f)
    }

    pub fn size(&self) -> u64 {
        self.inner.lock().size
    }

    /// `ftruncate`: grows with zeroes, or shrinks.
    pub fn resize(&self, size: u64) -> Result<(), isize> {
        if size > MAX_SIZE {
            return Err(EFBIG);
        }
        let mut i = self.inner.lock();
        let want = size.div_ceil(PAGE_SIZE) as usize;
        while i.frames.len() < want {
            let f = vmm::shm_alloc().ok_or(ENOMEM)?;
            i.frames.push(f);
        }
        for f in i.frames.drain(want..) {
            vmm::shm_release(f);
        }
        // The part of the last page past the end reads as zero again.
        if size < i.size && !size.is_multiple_of(PAGE_SIZE) {
            let f = i.frames[want - 1];
            let off = (size % PAGE_SIZE) as usize;
            unsafe { core::ptr::write_bytes((phys_to_virt(f) as *mut u8).add(off), 0, PAGE_SIZE as usize - off) };
        }
        i.size = size;
        Ok(())
    }

    /// The frames for `pages` pages from page `first` (as many as exist).
    pub fn frames(&self, first: u64, pages: u64) -> Vec<u64> {
        let i = self.inner.lock();
        i.frames.iter().skip(first as usize).take(pages as usize).copied().collect()
    }

    pub fn read_at(&self, off: u64, out: &mut [u8]) -> usize {
        let i = self.inner.lock();
        let n = (i.size.saturating_sub(off) as usize).min(out.len());
        let mut done = 0;
        while done < n {
            let at = off + done as u64;
            let f = i.frames[(at / PAGE_SIZE) as usize];
            let in_page = (at % PAGE_SIZE) as usize;
            let k = (PAGE_SIZE as usize - in_page).min(n - done);
            unsafe {
                core::ptr::copy_nonoverlapping((phys_to_virt(f) as *const u8).add(in_page), out[done..].as_mut_ptr(), k)
            };
            done += k;
        }
        n
    }

    pub fn write_at(&self, off: u64, data: &[u8]) -> Result<usize, isize> {
        let end = off.checked_add(data.len() as u64).ok_or(EINVAL)?;
        if end > self.size() {
            self.resize(end)?;
        }
        let i = self.inner.lock();
        let mut done = 0;
        while done < data.len() {
            let at = off + done as u64;
            let f = i.frames[(at / PAGE_SIZE) as usize];
            let in_page = (at % PAGE_SIZE) as usize;
            let k = (PAGE_SIZE as usize - in_page).min(data.len() - done);
            unsafe {
                core::ptr::copy_nonoverlapping(data[done..].as_ptr(), (phys_to_virt(f) as *mut u8).add(in_page), k)
            };
            done += k;
        }
        Ok(data.len())
    }
}

impl Drop for ShmFile {
    fn drop(&mut self) {
        for f in self.inner.lock().frames.drain(..) {
            vmm::shm_release(f);
        }
    }
}

/// Objects in `/dev/shm`, by name.
static NAMES: Mutex<BTreeMap<String, Arc<ShmFile>>> = Mutex::new(BTreeMap::new());

/// Opens `/dev/shm/name`, making it with `create`.
pub fn open(name: &str, create: bool, exclusive: bool, truncate: bool) -> Result<Arc<ShmFile>, isize> {
    if name.is_empty() || name.contains('/') {
        return Err(EINVAL);
    }
    let mut names = NAMES.lock();
    let f = match names.get(name) {
        Some(_) if create && exclusive => return Err(EEXIST),
        Some(f) => f.clone(),
        None if create => {
            let f = ShmFile::new(0)?;
            names.insert(String::from(name), f.clone());
            f
        }
        None => return Err(ENOENT),
    };
    drop(names);
    if truncate {
        f.resize(0)?;
    }
    Ok(f)
}

pub fn unlink(name: &str) -> Result<(), isize> {
    NAMES.lock().remove(name).map(|_| ()).ok_or(ENOENT)
}

pub fn names() -> Vec<String> {
    NAMES.lock().keys().cloned().collect()
}

pub fn lookup(name: &str) -> Option<Arc<ShmFile>> {
    NAMES.lock().get(name).cloned()
}
