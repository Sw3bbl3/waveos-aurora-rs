//! SIMD versions of the hottest pixel loops, with exact scalar results:
//! SSE2 on x86-64, NEON on aarch64, nothing elsewhere (each function then
//! handles no pixels and the scalar code does them all).
//!
//! The x86 loops are inline assembly so the same code serves every build:
//! apps and the host (where SSE2 is part of the target) and the kernel,
//! which is built soft-float. The kernel's compiler never touches SIMD
//! registers, and interrupt and system-call paths don't save them, so there
//! these loops run only where [`set_gate`]'s check allows: in a kernel task
//! that owns saved SIMD state (the compositor). The aarch64 loops are NEON
//! intrinsics, compiled wherever the target has NEON (apps, the host and an
//! aarch64 kernel built with it, which saves the vector registers anyway);
//! a build without NEON has no SIMD paths at all. Each loop stops at the
//! first group of four pixels it can't handle exactly (transparency, or a
//! shade of 0 or 255) and leaves that group to the scalar code. Both
//! versions compute the same 16-bit sums and keep the high byte, so their
//! pixels are identical to each other and to the scalar code's (see the
//! tests below, which run whichever this computer has).

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

/// True when the SIMD paths may run in the current context.
#[inline]
pub fn enabled() -> bool {
    if cfg!(target_feature = "sse2") || cfg!(target_feature = "neon") {
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

/// `dst[i] = mix(a[i], b[i], t)` (see [`crate::canvas::mix`]) with the alpha
/// byte set, four pixels at a time, for the first `n / 4 * 4` pixels; `t` is
/// 0..=256. `dst` may be `a`. Returns how many pixels it wrote.
///
/// # Safety
/// SSE2 must be usable (see [`enabled`]); all three hold `n` pixels.
#[cfg(target_arch = "x86_64")]
pub unsafe fn mix_rows(dst: *mut u32, a: *const u32, b: *const u32, n: usize, t: u32) -> usize {
    let t = t.min(256) as u16;
    let u = 256 - t;
    let weights: [u16; 16] = [u, u, u, u, u, u, u, u, t, t, t, t, t, t, t, t];
    let groups = n / 4;
    let mut done: usize = 0;
    sse!(
        "movdqu xmm6, [{w}]",
        "movdqu xmm5, [{w} + 16]",
        "pcmpeqd xmm7, xmm7",
        "pslld xmm7, 24",
        "pxor xmm4, xmm4",
        "2:",
        "cmp {done}, {groups}",
        "jae 3f",
        "movdqu xmm0, [{a}]",
        "movdqu xmm2, [{b}]",
        "movdqa xmm1, xmm0",
        "punpcklbw xmm0, xmm4",
        "punpckhbw xmm1, xmm4",
        "movdqa xmm3, xmm2",
        "punpcklbw xmm2, xmm4",
        "punpckhbw xmm3, xmm4",
        "pmullw xmm0, xmm6",
        "pmullw xmm1, xmm6",
        "pmullw xmm2, xmm5",
        "pmullw xmm3, xmm5",
        "paddw xmm0, xmm2",
        "paddw xmm1, xmm3",
        "psrlw xmm0, 8",
        "psrlw xmm1, 8",
        "packuswb xmm0, xmm1",
        "por xmm0, xmm7",
        "movdqu [{d}], xmm0",
        "add {a}, 16",
        "add {b}, 16",
        "add {d}, 16",
        "inc {done}",
        "jmp 2b",
        "3:",
        w = in(reg) weights.as_ptr(),
        groups = in(reg) groups,
        a = inout(reg) a => _,
        b = inout(reg) b => _,
        d = inout(reg) dst => _,
        done = inout(reg) done
    );
    done * 4
}

/// NEON: the same three loops, lane for lane what the SSE2 ones compute.
#[cfg(all(target_arch = "aarch64", target_feature = "neon"))]
mod neon {
    use core::arch::aarch64::*;

    /// Whether all four pixels in `p` are opaque.
    #[inline(always)]
    unsafe fn opaque(p: uint32x4_t) -> bool {
        let alpha = vdupq_n_u32(0xFF00_0000);
        vminvq_u32(vceqq_u32(vandq_u32(p, alpha), alpha)) == u32::MAX
    }

    /// `(x >> 8)` of each 16-bit lane of two halves, packed back to bytes.
    #[inline(always)]
    unsafe fn high_bytes(lo: uint16x8_t, hi: uint16x8_t) -> uint32x4_t {
        vreinterpretq_u32_u8(vcombine_u8(vshrn_n_u16::<8>(lo), vshrn_n_u16::<8>(hi)))
    }

    pub unsafe fn blend_row(row: &mut [u32], c: u32, a: u32) -> usize {
        let term = |shift: u32| (((c >> shift) & 255) * a + 128) as u16;
        let src_terms: [u16; 8] = [term(0), term(8), term(16), 0, term(0), term(8), term(16), 0];
        let src = vld1q_u16(src_terms.as_ptr());
        let inv = vdup_n_u8((255 - a) as u8);
        let inv16 = vdupq_n_u8((255 - a) as u8);
        let alpha = vdupq_n_u32(0xFF00_0000);
        let groups = row.len() / 4;
        let p = row.as_mut_ptr();
        let mut done = 0;
        while done < groups {
            let at = p.add(done * 4);
            let px = vld1q_u32(at);
            if !opaque(px) {
                break;
            }
            let bytes = vreinterpretq_u8_u32(px);
            let lo = vaddq_u16(vmull_u8(vget_low_u8(bytes), inv), src);
            let hi = vaddq_u16(vmull_high_u8(bytes, inv16), src);
            vst1q_u32(at, vorrq_u32(high_bytes(lo, hi), alpha));
            done += 1;
        }
        done * 4
    }

    pub unsafe fn shade_row(row: &mut [u32], shade: &[u32]) -> usize {
        let alpha = vdupq_n_u32(0xFF00_0000);
        let full = vdupq_n_u32(255);
        let zero = vdupq_n_u32(0);
        let round = vdupq_n_u16(128);
        let groups = row.len() / 4;
        let (p, s) = (row.as_mut_ptr(), shade.as_ptr());
        let mut done = 0;
        while done < groups {
            let at = p.add(done * 4);
            let px = vld1q_u32(at);
            let sh = vld1q_u32(s.add(done * 4));
            // Every pixel opaque, every shade neither 0 nor 255.
            let edge = vorrq_u32(vceqq_u32(sh, zero), vceqq_u32(sh, full));
            if !opaque(px) || vmaxvq_u32(edge) != 0 {
                break;
            }
            // 255 - shade in both halves of each pixel's word, then in all
            // four channel lanes of its pixel.
            let inv = vsubq_u32(full, sh);
            let inv = vorrq_u32(inv, vshlq_n_u32::<16>(inv));
            let (inv_lo, inv_hi) =
                (vreinterpretq_u16_u32(vzip1q_u32(inv, inv)), vreinterpretq_u16_u32(vzip2q_u32(inv, inv)));
            let bytes = vreinterpretq_u8_u32(px);
            let lo = vaddq_u16(vmulq_u16(vmovl_u8(vget_low_u8(bytes)), inv_lo), round);
            let hi = vaddq_u16(vmulq_u16(vmovl_high_u8(bytes), inv_hi), round);
            vst1q_u32(at, vorrq_u32(high_bytes(lo, hi), alpha));
            done += 1;
        }
        done * 4
    }

    pub unsafe fn mix_rows(dst: *mut u32, a: *const u32, b: *const u32, n: usize, t: u32) -> usize {
        let t = t.min(256) as u16;
        let (wa, wb) = (vdupq_n_u16(256 - t), vdupq_n_u16(t));
        let alpha = vdupq_n_u32(0xFF00_0000);
        let groups = n / 4;
        for g in 0..groups {
            let x = vreinterpretq_u8_u32(vld1q_u32(a.add(g * 4)));
            let y = vreinterpretq_u8_u32(vld1q_u32(b.add(g * 4)));
            let lo = vmlaq_u16(vmulq_u16(vmovl_u8(vget_low_u8(x)), wa), vmovl_u8(vget_low_u8(y)), wb);
            let hi = vmlaq_u16(vmulq_u16(vmovl_high_u8(x), wa), vmovl_high_u8(y), wb);
            vst1q_u32(dst.add(g * 4), vorrq_u32(high_bytes(lo, hi), alpha));
        }
        groups * 4
    }
}

/// Blends `c` at opacity `a` (1..=254) over opaque pixels, four at a time
/// (see the x86 version).
///
/// # Safety
/// SIMD must be usable (see [`enabled`]).
#[cfg(target_arch = "aarch64")]
pub unsafe fn blend_row(row: &mut [u32], c: u32, a: u32) -> usize {
    #[cfg(target_feature = "neon")]
    return neon::blend_row(row, c, a);
    #[cfg(not(target_feature = "neon"))]
    return {
        let _ = (row, c, a);
        0
    };
}

/// Darkens opaque pixels by `shade[i]` (1..=254), four at a time (see the
/// x86 version).
///
/// # Safety
/// SIMD must be usable (see [`enabled`]); `shade` is at least as long as `row`.
#[cfg(target_arch = "aarch64")]
pub unsafe fn shade_row(row: &mut [u32], shade: &[u32]) -> usize {
    debug_assert!(shade.len() >= row.len());
    #[cfg(target_feature = "neon")]
    return neon::shade_row(row, shade);
    #[cfg(not(target_feature = "neon"))]
    return {
        let _ = (row, shade);
        0
    };
}

/// `dst[i] = mix(a[i], b[i], t)` with the alpha byte set, four pixels at a
/// time (see the x86 version).
///
/// # Safety
/// SIMD must be usable (see [`enabled`]); all three hold `n` pixels.
#[cfg(target_arch = "aarch64")]
pub unsafe fn mix_rows(dst: *mut u32, a: *const u32, b: *const u32, n: usize, t: u32) -> usize {
    #[cfg(target_feature = "neon")]
    return neon::mix_rows(dst, a, b, n, t);
    #[cfg(not(target_feature = "neon"))]
    return {
        let _ = (dst, a, b, n, t);
        0
    };
}

/// Without SIMD: no pixels handled (the callers' scalar code does them).
///
/// # Safety
/// None; `unsafe` like the SIMD versions.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub unsafe fn blend_row(_: &mut [u32], _: u32, _: u32) -> usize {
    0
}

/// See [`blend_row`].
///
/// # Safety
/// None.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub unsafe fn shade_row(_: &mut [u32], _: &[u32]) -> usize {
    0
}

/// See [`blend_row`].
///
/// # Safety
/// None.
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub unsafe fn mix_rows(_: *mut u32, _: *const u32, _: *const u32, _: usize, _: u32) -> usize {
    0
}

/// The SIMD loops against the scalar code they stand in for, pixel for
/// pixel: SSE2 on an x86 computer, NEON on an Arm one (the CI runs one,
/// a Mac with Apple silicon the other).
#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::canvas::{blend, mix};
    use alloc::vec::Vec;

    /// A deterministic pixel source: xorshift.
    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0 as u32
        }
        /// Mostly opaque pixels, some not, to exercise where the loops stop.
        fn pixel(&mut self) -> u32 {
            let p = self.next();
            match self.next() % 16 {
                0 => p & 0x00FF_FFFF,
                1 => p & 0x7FFF_FFFF,
                _ => p | 0xFF00_0000,
            }
        }
    }

    fn row(rng: &mut Rng, n: usize) -> Vec<u32> {
        (0..n).map(|_| rng.pixel()).collect()
    }

    #[test]
    fn simd_is_on_where_the_target_has_it() {
        if cfg!(any(target_feature = "sse2", target_feature = "neon")) {
            assert!(enabled());
        }
    }

    #[test]
    fn blend_row_matches_scalar() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for round in 0..4000 {
            let n = 4 + rng.next() as usize % 61;
            let original = row(&mut rng, n);
            let c = rng.next() & 0x00FF_FFFF;
            let a = 1 + rng.next() % 254;
            let mut simd = original.clone();
            let done = unsafe { blend_row(&mut simd, c, a) };
            assert!(done.is_multiple_of(4) && done <= n);
            if cfg!(any(target_feature = "sse2", target_feature = "neon")) {
                // It handles every group up to the first with transparency.
                let first_clear = original.as_chunks::<4>().0.iter().position(|g| g.iter().any(|p| p >> 24 != 255));
                assert_eq!(done, first_clear.unwrap_or(n / 4) * 4, "round {round}");
            }
            for i in 0..n {
                let want = if i < done { blend(original[i], c, a) } else { original[i] };
                assert_eq!(simd[i], want, "round {round}, pixel {i}: {:#010x} under {c:#08x} at {a}", original[i]);
            }
        }
    }

    #[test]
    fn shade_row_matches_scalar() {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D);
        for round in 0..4000 {
            let n = 4 + rng.next() as usize % 61;
            let original = row(&mut rng, n);
            let shade: Vec<u32> = (0..n)
                .map(|_| match rng.next() % 40 {
                    0 => 0,
                    1 => 255,
                    _ => 1 + rng.next() % 254,
                })
                .collect();
            let mut simd = original.clone();
            let done = unsafe { shade_row(&mut simd, &shade) };
            assert!(done.is_multiple_of(4) && done <= n);
            if cfg!(any(target_feature = "sse2", target_feature = "neon")) {
                let stop = (0..n / 4).position(|g| {
                    (g * 4..g * 4 + 4).any(|i| original[i] >> 24 != 255 || shade[i] == 0 || shade[i] == 255)
                });
                assert_eq!(done, stop.unwrap_or(n / 4) * 4, "round {round}");
            }
            for i in 0..n {
                let want = if i < done { blend(original[i], 0, shade[i]) } else { original[i] };
                assert_eq!(simd[i], want, "round {round}, pixel {i}: {:#010x} shaded {}", original[i], shade[i]);
            }
        }
    }

    #[test]
    fn mix_rows_matches_scalar() {
        let mut rng = Rng(0xD1B5_4A32_D192_ED03);
        for round in 0..4000 {
            let n = rng.next() as usize % 67;
            let (a, b) = (row(&mut rng, n), row(&mut rng, n));
            let t = match round % 50 {
                0 => 0,
                1 => 256,
                _ => rng.next() % 257,
            };
            let mut dst = alloc::vec![0u32; n];
            let done = unsafe { mix_rows(dst.as_mut_ptr(), a.as_ptr(), b.as_ptr(), n, t) };
            if cfg!(any(target_feature = "sse2", target_feature = "neon")) {
                assert_eq!(done, n / 4 * 4);
            }
            for i in 0..done {
                assert_eq!(dst[i], mix(a[i], b[i], t as i32) | 0xFF00_0000, "round {round}, pixel {i}, t {t}");
            }
            // In place (`dst` is `a`), as `mix_over` uses it.
            let mut inplace = a.clone();
            let p = inplace.as_mut_ptr();
            let done = unsafe { mix_rows(p, p, b.as_ptr(), n, t) };
            for i in 0..done {
                assert_eq!(inplace[i], mix(a[i], b[i], t as i32) | 0xFF00_0000, "in place, round {round}, pixel {i}");
            }
        }
    }
}
