//! The vDSO: a small shared object (built from `kernel/vdso/vdso.c`) that
//! every Linux program gets mapped, with `clock_gettime`, `gettimeofday`
//! and `time` reading the time-stamp counter directly instead of making a
//! system call. Programs find it through `AT_SYSINFO_EHDR`.
//!
//! One page below the image holds the time page ([`Vvar`]): where the TSC
//! started, its rate and the offsets to the kernel's clocks. Every process
//! maps the same frames (read-only). The kernel's own clock uses the same
//! rate (`time::tsc_mult`), so both always agree.

use crate::mm::vmm::{AddressSpace, Prot};
use crate::mm::{frame, phys_to_virt, PAGE_SIZE};
use alloc::vec::Vec;
use core::sync::atomic::{fence, AtomicU32, Ordering};
use spin::Once;

/// The time page; `kernel/vdso/vdso.c` reads the same layout.
#[repr(C)]
struct Vvar {
    /// Odd while the kernel changes the page.
    seq: AtomicU32,
    /// The TSC is the clock (otherwise the vDSO asks the kernel).
    ok: u32,
    tsc_base: u64,
    /// Nanoseconds per tick, times 2^32.
    mult: u64,
    offset_ns: i64,
    /// CLOCK_REALTIME minus CLOCK_MONOTONIC.
    real_offset_ns: i64,
}

#[cfg(vdso)]
static IMAGE: &[u8] = include_bytes!(env!("WAVEOS_VDSO"));
#[cfg(not(vdso))]
static IMAGE: &[u8] = &[];

/// The time page's frame, then the image's.
static FRAMES: Once<Option<Vec<u64>>> = Once::new();

fn frames() -> Option<&'static [u64]> {
    FRAMES
        .call_once(|| {
            if IMAGE.is_empty() {
                return None;
            }
            let pages = 1 + IMAGE.len().div_ceil(PAGE_SIZE as usize);
            let mut frames = Vec::with_capacity(pages);
            for _ in 0..pages {
                let f = frame::alloc()?;
                unsafe { core::ptr::write_bytes(phys_to_virt(f) as *mut u8, 0, PAGE_SIZE as usize) };
                frames.push(f);
            }
            for (i, chunk) in IMAGE.chunks(PAGE_SIZE as usize).enumerate() {
                let to = phys_to_virt(frames[1 + i]) as *mut u8;
                unsafe { core::ptr::copy_nonoverlapping(chunk.as_ptr(), to, chunk.len()) };
            }
            Some(frames)
        })
        .as_deref()
}

/// Writes the clock's current parameters into the time page (at start, and
/// whenever they change: after sleep).
pub fn publish() {
    let Some(f) = frames() else { return };
    let v = unsafe { &mut *(phys_to_virt(f[0]) as *mut Vvar) };
    let (ok, tsc_base, mult, offset_ns) = crate::time::tsc_params();
    let real_offset_ns = super::misc::realtime_ns() as i64 - crate::time::now_ns() as i64;
    let seq = v.seq.load(Ordering::Relaxed);
    v.seq.store(seq.wrapping_add(1) | 1, Ordering::Relaxed);
    fence(Ordering::SeqCst);
    v.ok = ok as u32;
    v.tsc_base = tsc_base;
    v.mult = mult;
    v.offset_ns = offset_ns;
    v.real_offset_ns = real_offset_ns;
    fence(Ordering::SeqCst);
    v.seq.store((seq | 1).wrapping_add(1), Ordering::Release);
}

/// Maps the time page and the vDSO into `aspace` just below `below`;
/// returns the image's address (`AT_SYSINFO_EHDR`), or 0 without one.
pub fn map(aspace: &mut AddressSpace, below: u64) -> u64 {
    static PUBLISHED: Once<()> = Once::new();
    let Some(f) = frames() else { return 0 };
    PUBLISHED.call_once(publish);
    // A guard page between it and the stack.
    let base = below - (f.len() as u64 + 1) * PAGE_SIZE;
    if aspace.map_shared(base, &f[..1], Prot::ReadOnly).is_err()
        || aspace.map_shared(base + PAGE_SIZE, &f[1..], Prot::ReadExec).is_err()
    {
        return 0;
    }
    base + PAGE_SIZE
}
