//! SIMD state for user programs.
//!
//! The kernel is built soft-float and never touches x87/SSE/AVX registers,
//! so only user tasks (and the few SIMD kernel tasks) carry SIMD state. It
//! is saved and restored eagerly on every switch: with XSAVE, which covers
//! x87, SSE and AVX, on processors that have it (Linux programs use AVX
//! when the processor offers it: WebKit's JavaScript compiler, glibc's
//! string functions), or else with FXSAVE (x87 and SSE).

use core::arch::asm;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// Bytes of state kept: XSAVE's for x87, SSE and AVX take 832.
pub const AREA: usize = 1024;
/// The state components saved: x87, SSE and AVX (XCR0 bits 0-2).
const COMPONENTS: u64 = 0b111;
/// XSAVE is in use (on every CPU, set up by [`init`]).
static XSAVE: AtomicBool = AtomicBool::new(false);
/// The components enabled in XCR0.
static XCR0: AtomicU64 = AtomicU64::new(0);
/// Bytes XSAVE writes for them (512 with FXSAVE).
static SIZE: AtomicU64 = AtomicU64::new(512);

/// An XSAVE (or FXSAVE) area.
#[repr(C, align(64))]
pub struct FpuState([u8; AREA]);

impl FpuState {
    /// The power-on state: all exceptions masked, round to nearest.
    pub fn new() -> FpuState {
        let mut s = FpuState([0; AREA]);
        s.0[0..2].copy_from_slice(&0x037Fu16.to_le_bytes()); // FCW
        s.0[24..28].copy_from_slice(&0x1F80u32.to_le_bytes()); // MXCSR
        s.0[28..32].copy_from_slice(&0xFFFFu32.to_le_bytes()); // MXCSR_MASK
                                                               // XSAVE header: x87 and SSE come from the area (their control words
                                                               // above); AVX starts in its initial state (zero).
        s.0[512..520].copy_from_slice(&0b11u64.to_le_bytes());
        s
    }

    /// The bytes XSAVE (or FXSAVE) uses: what a Linux signal frame carries.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..size()]
    }

    /// A state from bytes a program supplied (a Linux signal frame), made
    /// safe to load: MXCSR bits this CPU doesn't have cleared, and the
    /// XSAVE header limited to the enabled components (anything else in
    /// either would fault).
    pub fn from_bytes(bytes: &[u8]) -> FpuState {
        let mut s = FpuState::new();
        let n = bytes.len().min(size());
        s.0[..n].copy_from_slice(&bytes[..n]);
        let mut probe = FpuState::new();
        probe.save();
        let mask = match u32::from_le_bytes(probe.0[28..32].try_into().unwrap()) {
            0 => 0xFFBF,
            m => m,
        };
        let mxcsr = u32::from_le_bytes(s.0[24..28].try_into().unwrap()) & mask;
        s.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        if XSAVE.load(Ordering::Relaxed) {
            // Only x87 and SSE when the frame had no XSAVE data (512 bytes).
            let present = if n > 512 { u64::from_le_bytes(s.0[512..520].try_into().unwrap()) } else { 0b11 };
            s.0[512..576].fill(0);
            s.0[512..520].copy_from_slice(&(present & XCR0.load(Ordering::Relaxed)).to_le_bytes());
        }
        s
    }

    #[inline]
    pub fn save(&mut self) {
        if XSAVE.load(Ordering::Relaxed) {
            unsafe {
                asm!("xsave64 [{}]", in(reg) self.0.as_mut_ptr(), in("eax") u32::MAX, in("edx") u32::MAX,
                     options(nostack))
            };
        } else {
            unsafe { asm!("fxsave64 [{}]", in(reg) self.0.as_mut_ptr(), options(nostack)) };
        }
    }

    #[inline]
    pub fn restore(&self) {
        if XSAVE.load(Ordering::Relaxed) {
            unsafe {
                asm!("xrstor64 [{}]", in(reg) self.0.as_ptr(), in("eax") u32::MAX, in("edx") u32::MAX,
                     options(nostack))
            };
        } else {
            unsafe { asm!("fxrstor64 [{}]", in(reg) self.0.as_ptr(), options(nostack)) };
        }
    }
}

/// Bytes of state saved: 512 (FXSAVE) or XSAVE's size for the enabled components.
pub fn size() -> usize {
    SIZE.load(Ordering::Relaxed) as usize
}

/// The components enabled (XCR0), 0 without XSAVE: for Linux signal frames.
pub fn xfeatures() -> u64 {
    if XSAVE.load(Ordering::Relaxed) {
        XCR0.load(Ordering::Relaxed)
    } else {
        0
    }
}

/// Enables x87/SSE for ring 3 on this CPU.
pub fn init() {
    unsafe {
        Cr0::update(|f| {
            f.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            f.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR);
            // The kernel can't write through read-only pages either (shared
            // pages of mapped files; see `syscall::user`). The firmware sets
            // this on the boot CPU; the others start without it.
            f.insert(Cr0Flags::WRITE_PROTECT);
        });
        Cr4::update(|f| f.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE));
        asm!("fninit", options(nomem, nostack));
    }
    // XSAVE, and with it AVX, where the processor has it.
    let has_xsave = core::arch::x86_64::__cpuid(1).ecx & (1 << 26) != 0;
    if !has_xsave {
        return;
    }
    let supported = core::arch::x86_64::__cpuid_count(0xD, 0).eax as u64;
    let xcr0 = supported & COMPONENTS;
    unsafe {
        Cr4::update(|f| f.insert(Cr4Flags::OSXSAVE));
        asm!("xsetbv", in("ecx") 0u32, in("eax") xcr0 as u32, in("edx") (xcr0 >> 32) as u32, options(nomem, nostack));
    }
    // With XCR0 set, EBX is the size XSAVE writes for those components.
    let size = core::arch::x86_64::__cpuid_count(0xD, 0).ebx as u64;
    if size as usize > AREA {
        // Too big to keep: x87 and SSE only, as with FXSAVE (no AVX).
        unsafe { asm!("xsetbv", in("ecx") 0u32, in("eax") 0b11u32, in("edx") 0u32, options(nomem, nostack)) };
        return;
    }
    XCR0.store(xcr0, Ordering::Relaxed);
    SIZE.store(size, Ordering::Relaxed);
    XSAVE.store(true, Ordering::Relaxed);
}
