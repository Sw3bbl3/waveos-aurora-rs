//! The vDSO: a small shared object (built from `kernel/vdso/vdso.c`) that
//! every Linux program gets mapped, with `clock_gettime`, `gettimeofday`
//! and `time` reading the time-stamp counter directly instead of making a
//! system call. Programs find it through `AT_SYSINFO_EHDR`.
//!
//! One page below the image holds the time page ([`Vvar`]): where the TSC
//! started, its rate and the offsets to the kernel's clocks. Every process
//! maps the same frames (read-only). The kernel's own clocks use the very
//! same values (`time::tsc_params`, `time::real_offset_ns`) in the same
//! formula, so both always agree.

use crate::mm::vmm::{AddressSpace, Prot};
use crate::mm::{phys_to_virt, PAGE_SIZE};
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
            // Counted shared frames (held forever by the vDSO itself), so a
            // `fork` child keeps them mapped: plain shared frames stay with
            // the parent, and the child crashed in its first clock_gettime
            // (found by `linuxfuzz`).
            for _ in 0..pages {
                frames.push(crate::mm::vmm::shm_alloc()?);
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
/// whenever they change: after sleep, when the clock is set or its zone
/// learnt). They are the very values the kernel's clocks use — the vDSO
/// computes `time::now_ns` and `time::realtime_ns` with the same formula —
/// so both agree by construction; nothing here reads the clock.
pub fn publish() {
    static WRITER: spin::Mutex<()> = spin::Mutex::new(());
    let Some(f) = frames() else { return };
    let _writer = WRITER.lock();
    let v = unsafe { &mut *(phys_to_virt(f[0]) as *mut Vvar) };
    let (ok, tsc_base, mult, offset_ns) = crate::time::tsc_params();
    let real_offset_ns = crate::time::real_offset_ns();
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

/// The time as the vDSO computes it from the time page (`now()` in
/// `kernel/vdso/vdso.c`), real time if `real`; `None` if it would ask the
/// kernel. For the self-tests.
#[cfg(feature = "ktest")]
pub fn read_ns(real: bool) -> Option<u64> {
    let f = frames()?;
    let v = unsafe { &*(phys_to_virt(f[0]) as *const Vvar) };
    loop {
        let seq = v.seq.load(Ordering::Acquire);
        if seq & 1 != 0 {
            core::hint::spin_loop();
            continue;
        }
        let (ok, base, mult, offset, real_offset) = unsafe {
            (
                core::ptr::read_volatile(&v.ok),
                core::ptr::read_volatile(&v.tsc_base),
                core::ptr::read_volatile(&v.mult),
                core::ptr::read_volatile(&v.offset_ns),
                core::ptr::read_volatile(&v.real_offset_ns),
            )
        };
        if ok == 0 {
            return None;
        }
        let d = unsafe {
            core::arch::x86_64::_mm_lfence();
            core::arch::x86_64::_rdtsc()
        }
        .wrapping_sub(base);
        let mut t = ((d as u128 * mult as u128) >> 32) as i64 + offset;
        t = t.max(0);
        if real {
            t += real_offset;
        }
        fence(Ordering::Acquire);
        if v.seq.load(Ordering::Relaxed) == seq {
            return Some(t as u64);
        }
    }
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
