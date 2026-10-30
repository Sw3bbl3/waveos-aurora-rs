//! Memory for Linux programs: `mmap`, `munmap`, `mprotect`, `brk`,
//! `mremap`, `madvise`.
//!
//! Anonymous memory gets its pages when they are first touched (see
//! `AddressSpace::map_lazy`): programs reserve gigabytes they never fill
//! (WebKit's JavaScript engine, allocators, thread stacks). A `PROT_NONE`
//! mapping only reserves its addresses. New ranges come from the address
//! space's mmap region, which never hands the same addresses out twice.

use crate::mm::vmm::{AddressSpace, Prot};
use crate::mm::PAGE_SIZE;
use crate::proc::{Handle, Process};
use alloc::string::String;
use aurora_abi::err::*;
use aurora_abi::layout;
use core::fmt::Write;
use core::sync::atomic::Ordering;

const PROT_WRITE: u64 = 2;
const PROT_EXEC: u64 = 4;
const MAP_SHARED: u64 = 0x01;
const MAP_FIXED: u64 = 0x10;
const MAP_ANONYMOUS: u64 = 0x20;
const MAP_FIXED_NOREPLACE: u64 = 0x100000;
const MREMAP_MAYMOVE: u64 = 1;
const MADV_DONTNEED: u64 = 4;
/// The heap (`brk`) may grow this far past the program.
const BRK_LIMIT: u64 = 1 << 30;
/// Largest single mapping.
const MAX_MAP: u64 = 1 << 36;

#[derive(Clone)]
pub struct Memory {
    /// Where the heap starts (just past the program) and where it ends now.
    brk_start: u64,
    brk: u64,
}

impl Memory {
    pub fn new(brk: u64) -> Memory {
        Memory { brk_start: brk, brk }
    }

    /// The heap's range.
    pub fn heap(&self) -> (u64, u64) {
        (self.brk_start, self.brk)
    }
}

/// `/proc/self/maps`: the program's mappings (glibc finds the main
/// thread's stack here).
pub fn maps(p: &Process) -> String {
    let heap = p.linux.as_ref().map_or((0, 0), |l| l.mm.lock().heap());
    let regions = p.mem().as_ref().map(|a| a.regions()).unwrap_or_default();
    let mut out = String::new();
    for (start, end, w, x) in regions {
        let name = if end == layout::STACK_TOP {
            "[stack]"
        } else if start < heap.1 && end > heap.0 {
            "[heap]"
        } else {
            ""
        };
        let _ = writeln!(
            out,
            "{start:08x}-{end:08x} r{}{}p 00000000 00:00 0 {name}",
            if w { 'w' } else { '-' },
            if x { 'x' } else { '-' }
        );
    }
    out
}

/// A Linux protection as a page protection (`None`: no access).
pub fn prot(p: u64) -> Option<Prot> {
    match (p & 7, p & PROT_WRITE != 0, p & PROT_EXEC != 0) {
        (0, _, _) => None,
        (_, true, true) => Some(Prot::ReadWriteExec),
        (_, true, false) => Some(Prot::ReadWrite),
        (_, false, true) => Some(Prot::ReadExec),
        _ => Some(Prot::ReadOnly),
    }
}

fn pages(len: u64) -> u64 {
    len.div_ceil(PAGE_SIZE)
}

fn user_range(addr: u64, len: u64) -> bool {
    addr >= PAGE_SIZE && addr.checked_add(len).is_some_and(|e| e <= layout::USER_END)
}

/// Picks free addresses for `n` pages (near `hint` if it is free).
fn place(aspace: &mut AddressSpace, hint: u64, n: u64) -> Result<u64, isize> {
    if hint != 0 && hint.is_multiple_of(PAGE_SIZE) && user_range(hint, n * PAGE_SIZE) && aspace.is_free(hint, n) {
        return Ok(hint);
    }
    loop {
        let va = aspace.reserve(n).ok_or(ENOMEM)?;
        if aspace.is_free(va, n) {
            return Ok(va);
        }
    }
}

fn update_resident(p: &Process, aspace: &AddressSpace) {
    p.resident_kib.store(aspace.resident / 1024, Ordering::Relaxed);
}

pub fn mmap(p: &Process, addr: u64, len: u64, prot_bits: u64, flags: u64, fd: i64, offset: u64) -> Result<u64, isize> {
    if len == 0 || len > MAX_MAP || !offset.is_multiple_of(PAGE_SIZE) {
        return Err(EINVAL);
    }
    let n = pages(len);
    // Shared memory: the same frames in every process that maps it.
    let shared = if flags & MAP_SHARED != 0 {
        if flags & MAP_ANONYMOUS != 0 {
            Some(super::shm::ShmFile::new(n * PAGE_SIZE)?)
        } else {
            super::fs::shm_of(p, fd as u64)
        }
    } else {
        None
    };
    if let Some(shm) = shared {
        let frames = shm.frames(offset / PAGE_SIZE, n);
        let mut guard = p.mem();
        let aspace = guard.as_mut().ok_or(ESRCH)?;
        let va = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
            if !addr.is_multiple_of(PAGE_SIZE) || !user_range(addr, n * PAGE_SIZE) {
                return Err(EINVAL);
            }
            if flags & MAP_FIXED_NOREPLACE != 0 && !aspace.is_free(addr, n) {
                return Err(EEXIST);
            }
            aspace.unmap(addr, n);
            addr
        } else {
            place(aspace, addr & !(PAGE_SIZE - 1), n)?
        };
        // Pages past the object's end stay unmapped (Linux: SIGBUS there).
        aspace.map_shared(va, &frames, prot(prot_bits).unwrap_or(Prot::ReadOnly))?;
        if prot(prot_bits).is_none() {
            aspace.protect(va, frames.len() as u64, None)?;
        }
        update_resident(p, aspace);
        return Ok(va);
    }
    // A file's contents are read now, before taking the address-space lock.
    let file_data = if let (true, Some(shm)) = (flags & MAP_ANONYMOUS == 0, super::fs::shm_of(p, fd as u64)) {
        // A private copy of shared memory.
        let mut data = alloc::vec![0u8; len as usize];
        let got = shm.read_at(offset, &mut data);
        data.truncate(got);
        Some(data)
    } else {
        None
    };
    // A disk file's pages come from the cache every mapping of it shares
    // (read first if need be). Shared mappings of disk files are private
    // for now: what is written to them stays in the process.
    let cached = if flags & MAP_ANONYMOUS == 0 && file_data.is_none() {
        let h = p.fd(fd as usize)?;
        let Handle::File(f) = &*h else { return Err(ENODEV) };
        let (fs, ino) = {
            let f = f.lock();
            (f.fs.clone(), f.ino)
        };
        Some(crate::fs::mapcache::pages(&fs, ino, offset, n)?)
    } else {
        None
    };
    let mut guard = p.mem();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    let va = if flags & (MAP_FIXED | MAP_FIXED_NOREPLACE) != 0 {
        if !addr.is_multiple_of(PAGE_SIZE) || !user_range(addr, n * PAGE_SIZE) {
            return Err(EINVAL);
        }
        if flags & MAP_FIXED_NOREPLACE != 0 && !aspace.is_free(addr, n) {
            return Err(EEXIST);
        }
        aspace.unmap(addr, n);
        addr
    } else {
        place(aspace, addr & !(PAGE_SIZE - 1), n)?
    };
    let target = prot(prot_bits);
    if let Some(pages) = cached {
        // No access: the range stays reserved, unmapped.
        if let Some(pr) = target {
            if let Err(e) = pages.map(aspace, va, pr) {
                aspace.unmap(va, n);
                return Err(e);
            }
        }
        update_resident(p, aspace);
        return Ok(va);
    }
    // Contents go in through a writable mapping; the protection comes after.
    // Anonymous memory comes when touched.
    let initial = if file_data.is_some() { Some(Prot::ReadWrite) } else { target };
    if let Some(pr) = initial {
        let mapped = if file_data.is_some() { aspace.map_anon(va, n, pr) } else { aspace.map_lazy(va, n, pr) };
        if let Err(e) = mapped {
            aspace.unmap(va, n);
            return Err(e);
        }
    }
    if let Some(data) = file_data {
        aspace.write_bytes(va, &data)?;
        if target != Some(Prot::ReadWrite) {
            aspace.protect(va, n, target)?;
        }
    }
    update_resident(p, aspace);
    Ok(va)
}

pub fn munmap(p: &Process, addr: u64, len: u64) -> Result<u64, isize> {
    if !addr.is_multiple_of(PAGE_SIZE) || len == 0 || !user_range(addr, len) {
        return Err(EINVAL);
    }
    let mut guard = p.mem();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    aspace.unmap(addr, pages(len));
    update_resident(p, aspace);
    Ok(0)
}

pub fn mprotect(p: &Process, addr: u64, len: u64, prot_bits: u64) -> Result<u64, isize> {
    if !addr.is_multiple_of(PAGE_SIZE) || !user_range(addr, len.max(1)) {
        return Err(EINVAL);
    }
    if len == 0 {
        return Ok(0);
    }
    let mut guard = p.mem();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    aspace.protect(addr, pages(len), prot(prot_bits))?;
    update_resident(p, aspace);
    Ok(0)
}

pub fn brk(p: &Process, want: u64) -> Result<u64, isize> {
    let Some(linux) = &p.linux else { return Err(ENOSYS) };
    let mut m = linux.mm.lock();
    if want < m.brk_start || want > m.brk_start + BRK_LIMIT {
        return Ok(m.brk);
    }
    let old_end = m.brk.next_multiple_of(PAGE_SIZE);
    let new_end = want.next_multiple_of(PAGE_SIZE);
    let mut guard = p.mem();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    if new_end > old_end {
        let n = (new_end - old_end) / PAGE_SIZE;
        if !aspace.is_free(old_end, n) || aspace.map_lazy(old_end, n, Prot::ReadWrite).is_err() {
            return Ok(m.brk);
        }
    } else if new_end < old_end {
        aspace.unmap(new_end, (old_end - new_end) / PAGE_SIZE);
    }
    update_resident(p, aspace);
    m.brk = want;
    Ok(want)
}

pub fn mremap(p: &Process, addr: u64, old_len: u64, new_len: u64, flags: u64) -> Result<u64, isize> {
    if !addr.is_multiple_of(PAGE_SIZE) || new_len == 0 || new_len > MAX_MAP || !user_range(addr, old_len.max(1)) {
        return Err(EINVAL);
    }
    let (old_n, new_n) = (pages(old_len), pages(new_len));
    let mut guard = p.mem();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    if new_n <= old_n {
        aspace.unmap(addr + new_n * PAGE_SIZE, old_n - new_n);
        update_resident(p, aspace);
        return Ok(addr);
    }
    // Grow in place if the pages after it are free.
    let tail = addr + old_n * PAGE_SIZE;
    if aspace.is_free(tail, new_n - old_n) && user_range(tail, (new_n - old_n) * PAGE_SIZE) {
        aspace.map_lazy(tail, new_n - old_n, Prot::ReadWrite)?;
        update_resident(p, aspace);
        return Ok(addr);
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return Err(ENOMEM);
    }
    // Move: the pages themselves go to the new place (no copying), and the
    // rest is new memory.
    let to = place(aspace, 0, new_n)?;
    aspace.move_pages(addr, to, old_n)?;
    aspace.map_lazy(to + old_n * PAGE_SIZE, new_n - old_n, Prot::ReadWrite)?;
    update_resident(p, aspace);
    Ok(to)
}

pub fn madvise(p: &Process, addr: u64, len: u64, advice: u64) -> Result<u64, isize> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(EINVAL);
    }
    if advice == MADV_DONTNEED && len > 0 && user_range(addr, len) {
        // The pages read as zero afterwards (anonymous memory lets them go).
        let mut guard = p.mem();
        let aspace = guard.as_mut().ok_or(ESRCH)?;
        aspace.discard(addr, pages(len));
        update_resident(p, aspace);
    }
    Ok(0)
}
