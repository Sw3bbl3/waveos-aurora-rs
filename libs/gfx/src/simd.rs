//! SSE2 versions of the hottest pixel loops, with exact scalar results.
//!
//! The loops are inline assembly so the same code serves every build: apps
//! and the host (where SSE2 is part of the target) and the kernel, which is
//! built soft-float. The kernel's compiler never touches SIMD registers, and
//! interrupt and system-call paths don't save them, so there these loops run
//! only where [`set_gate`]'s check allows: in a kernel task that owns saved
//! SIMD state (the compositor). Each loop stops at the first group of four
//! pixels it can't handle exactly (transparency, or a shade of 0 or 255) and
//! leaves that group to the scalar code.

use core::sync::atomic::{AtomicPtr, Ordering};

fn never() -> bool {
    false
}

static GATE: AtomicPtr<()> = AtomicPtr::new(never as fn() -> bool as *mut ());

/// Installs the check that says whether SIMD registers may be used right
/// now (kernel builds only; elsewhere SSE2 is always available).
pub fn set_gate(gate: fn() -> bool) {
    GATE.store(gate as *mut (), Ordering::Release);
}

/// True when the SSE2 paths may run in the current context.
#[inline]
pub fn enabled() -> bool {
    if cfg!(target_feature = "sse2") {
        return true;
    }
    let gate: fn() -> bool = unsafe { core::mem::transmute(GATE.load(Ordering::Acquire)) };
    gate()
}

/// Runs `asm!` naming the SIMD registers as clobbered where the compiler
/// itself uses them; the soft-float kernel has no such register class (and
/// never holds values in them).
#[cfg(target_arch = "x86_64")]
macro_rules! sse {
    ($($body:tt)*) => {
        #[cfg(target_feature = "sse2")]
        core::arch::asm!($($body)*,
            out("xmm0") _, out("xmm1") _, out("xmm2") _, out("xmm3") _, out("xmm4") _,
            out("xmm5") _, out("xmm6") _, out("xmm7") _, out("xmm8") _, out("xmm9") _,
            options(nostack));
        #[cfg(not(target_feature = "sse2"))]
        core::arch::asm!($($body)*, options(nostack));
    };
}

/// Blends `c` at opacity `a` (1..=254) over opaque pixels, four at a time,
/// from the start of `row`. Returns how many pixels it handled (a multiple
/// of four); it stops at a group containing a pixel with transparency.
///
/// # Safety
/// SSE2 must be usable (see [`enabled`]).
#[cfg(target_arch = "x86_64")]
pub unsafe fn blend_row(row: &mut [u32], c: u32, a: u32) -> usize {
    let term = |shift: u32| (((c >> shift) & 255) * a + 128) as u16;
    // Per channel of two pixels (B, G, R, A): src * a + 128, and 255 - a.
    let src: [u16; 8] = [term(0), term(8), term(16), 0, term(0), term(8), term(16), 0];
    let inv = [(255 - a) as u16; 8];
    let groups = row.len() / 4;
    let mut done: usize = 0;
    sse!(
        "movdqu xmm5, [{src}]",
        "movdqu xmm6, [{inv}]",
        "pcmpeqd xmm7, xmm7",
        "pslld xmm7, 24",
        "pxor xmm4, xmm4",
        "2:",
        "cmp {done}, {groups}",
        "jae 3f",
        "movdqu xmm0, [{p}]",
        "movdqa xmm1, xmm0",
        "pand xmm1, xmm7",
        "pcmpeqd xmm1, xmm7",
        "pmovmskb {m:e}, xmm1",
        "cmp {m:e}, 0xFFFF",
        "jne 3f",
        "movdqa xmm1, xmm0",
        "punpcklbw xmm0, xmm4",
        "punpckhbw xmm1, xmm4",
        "pmullw xmm0, xmm6",
        "pmullw xmm1, xmm6",
        "paddw xmm0, xmm5",
        "paddw xmm1, xmm5",
        "psrlw xmm0, 8",
        "psrlw xmm1, 8",
        "packuswb xmm0, xmm1",
        "por xmm0, xmm7",
        "movdqu [{p}], xmm0",
        "add {p}, 16",
        "inc {done}",
        "jmp 2b",
        "3:",
        src = in(reg) src.as_ptr(),
        inv = in(reg) inv.as_ptr(),
        groups = in(reg) groups,
        p = inout(reg) row.as_mut_ptr() => _,
        done = inout(reg) done,
        m = out(reg) _
    );
    done * 4
}

/// Darkens opaque pixels by `shade[i]` (1..=254), four at a time, from the
/// start of `row` (a drop shadow row). Returns how many pixels it handled; it
/// stops at a group with transparency or a shade of 0 or 255.
///
/// # Safety
/// SSE2 must be usable (see [`enabled`]); `shade` is at least as long as `row`.
#[cfg(target_arch = "x86_64")]
pub unsafe fn shade_row(row: &mut [u32], shade: &[u32]) -> usize {
    debug_assert!(shade.len() >= row.len());
    let consts: [u32; 8] = [255, 255, 255, 255, 0x0080_0080, 0x0080_0080, 0x0080_0080, 0x0080_0080];
    let groups = row.len() / 4;
    let mut done: usize = 0;
    sse!(
        "movdqu xmm8, [{k}]",       // 255 in each dword
        "movdqu xmm9, [{k} + 16]",  // 128 in each word
        "pcmpeqd xmm7, xmm7",
        "pslld xmm7, 24",           // 0xFF000000
        "pxor xmm4, xmm4",
        "2:",
        "cmp {done}, {groups}",
        "jae 3f",
        "movdqu xmm0, [{p}]",
        "movdqu xmm2, [{s}]",
        // ok = opaque && shade != 0 && shade < 255
        "movdqa xmm1, xmm0",
        "pand xmm1, xmm7",
        "pcmpeqd xmm1, xmm7",
        "movdqa xmm3, xmm2",
        "pcmpeqd xmm3, xmm4",
        "pandn xmm3, xmm1",
        "movdqa xmm1, xmm2",
        "pcmpeqd xmm1, xmm8",
        "pandn xmm1, xmm3",
        "pmovmskb {m:e}, xmm1",
        "cmp {m:e}, 0xFFFF",
        "jne 3f",
        // inv = 255 - shade, in every channel lane of its pixel
        "movdqa xmm1, xmm8",
        "psubd xmm1, xmm2",
        "movdqa xmm3, xmm1",
        "pslld xmm3, 16",
        "por xmm1, xmm3",
        "movdqa xmm3, xmm1",
        "punpckldq xmm1, xmm1",
        "punpckhdq xmm3, xmm3",
        "movdqa xmm2, xmm0",
        "punpcklbw xmm0, xmm4",
        "punpckhbw xmm2, xmm4",
        "pmullw xmm0, xmm1",
        "pmullw xmm2, xmm3",
        "paddw xmm0, xmm9",
        "paddw xmm2, xmm9",
        "psrlw xmm0, 8",
        "psrlw xmm2, 8",
        "packuswb xmm0, xmm2",
        "por xmm0, xmm7",
        "movdqu [{p}], xmm0",
        "add {p}, 16",
        "add {s}, 16",
        "inc {done}",
        "jmp 2b",
        "3:",
        k = in(reg) consts.as_ptr(),
        groups = in(reg) groups,
        p = inout(reg) row.as_mut_ptr() => _,
        s = inout(reg) shade.as_ptr() => _,
        done = inout(reg) done,
        m = out(reg) _
    );
    done * 4
}
