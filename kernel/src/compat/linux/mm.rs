//! Memory for Linux programs: `mmap`, `munmap`, `mprotect`, `brk`,
//! `mremap`, `madvise`.
//!
//! Pages are allocated when mapped; a `PROT_NONE` mapping only reserves its
//! addresses (allocators and JITs reserve large ranges this way and make
//! parts accessible with `mprotect`). New ranges come from the address
//! space's mmap region, which never hands the same addresses out twice.

use crate::mm::vmm::{AddressSpace, Prot};
use crate::mm::PAGE_SIZE;
use crate::proc::{Handle, Process};
use aurora_abi::err::*;
use aurora_abi::layout;
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

pub struct Memory {
    /// Where the heap starts (just past the program) and where it ends now.
    brk_start: u64,
    brk: u64,
}

impl Memory {
    pub fn new(brk: u64) -> Memory {
        Memory { brk_start: brk, brk }
    }
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
    // A file's contents are read now, before taking the address-space lock.
    let file_data = if flags & MAP_ANONYMOUS == 0 {
        let h = p.fd(fd as usize)?;
        let Handle::File(f) = &*h else { return Err(ENODEV) };
        let f = f.lock();
        let size = f.fs.metadata(f.ino)?.size;
        let want = len.min(size.saturating_sub(offset)) as usize;
        let mut data = alloc::vec![0u8; want];
        let got = if want > 0 { f.fs.read(f.ino, offset, &mut data)? } else { 0 };
        data.truncate(got);
        let _ = flags & MAP_SHARED; // shared file mappings are private copies for now
        Some(data)
    } else {
        None
    };
    let mut guard = p.aspace.lock();
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
    // Contents go in through a writable mapping; the protection comes after.
    let initial = if file_data.is_some() { Some(Prot::ReadWrite) } else { target };
    if let Some(pr) = initial {
        if let Err(e) = aspace.map_anon(va, n, pr) {
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
    let mut guard = p.aspace.lock();
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
    let mut guard = p.aspace.lock();
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
    let mut guard = p.aspace.lock();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    if new_end > old_end {
        let n = (new_end - old_end) / PAGE_SIZE;
        if !aspace.is_free(old_end, n) || aspace.map_anon(old_end, n, Prot::ReadWrite).is_err() {
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
    let mut guard = p.aspace.lock();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    if new_n <= old_n {
        aspace.unmap(addr + new_n * PAGE_SIZE, old_n - new_n);
        update_resident(p, aspace);
        return Ok(addr);
    }
    // Grow in place if the pages after it are free.
    let tail = addr + old_n * PAGE_SIZE;
    if aspace.is_free(tail, new_n - old_n) && user_range(tail, (new_n - old_n) * PAGE_SIZE) {
        aspace.map_anon(tail, new_n - old_n, Prot::ReadWrite)?;
        update_resident(p, aspace);
        return Ok(addr);
    }
    if flags & MREMAP_MAYMOVE == 0 {
        return Err(ENOMEM);
    }
    let to = place(aspace, 0, new_n)?;
    aspace.map_anon(to, new_n, Prot::ReadWrite)?;
    let mut page = alloc::vec![0u8; PAGE_SIZE as usize];
    for i in 0..old_n {
        // Pages never touched (PROT_NONE) stay zero.
        if aspace.read_bytes(addr + i * PAGE_SIZE, &mut page).is_ok() {
            aspace.write_bytes(to + i * PAGE_SIZE, &page)?;
        }
    }
    aspace.unmap(addr, old_n);
    update_resident(p, aspace);
    Ok(to)
}

pub fn madvise(p: &Process, addr: u64, len: u64, advice: u64) -> Result<u64, isize> {
    if !addr.is_multiple_of(PAGE_SIZE) {
        return Err(EINVAL);
    }
    if advice == MADV_DONTNEED && len > 0 && user_range(addr, len) {
        // The pages read as zero afterwards.
        let zero = alloc::vec![0u8; PAGE_SIZE as usize];
        let guard = p.aspace.lock();
        let aspace = guard.as_ref().ok_or(ESRCH)?;
        for i in 0..pages(len) {
            let _ = aspace.write_bytes(addr + i * PAGE_SIZE, &zero);
        }
    }
    Ok(0)
}
