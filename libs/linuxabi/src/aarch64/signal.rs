//! arm64 signal frames, exactly as Linux lays them out
//! (`arch/arm64/kernel/signal.c`).
//!
//! On the stack, from the top: a frame record `{fp, lr}` holding the
//! interrupted x29 and x30 (16-byte aligned; x29 points at it while the
//! handler runs, so backtraces go through the signal), then the
//! `rt_sigframe`: a `siginfo` (128 bytes) and a `ucontext` whose
//! `sigcontext` holds x0-x30, sp, pc and pstate, followed by a 4096-byte
//! area of records: the FP/SIMD registers (`fpsimd_context`, always), the
//! fault's syndrome (`esr_context`, for faults), and a zero terminator.
//! Without SVE everything fits in the area, so no `extra_context` is ever
//! written; on `rt_sigreturn` one is refused (the frame is bad), as are
//! records Linux doesn't know.
//!
//! The handler starts with x0 = the signal, x1 = &siginfo and x2 =
//! &ucontext (with `SA_SIGINFO`), sp = the frame, x29 = the record, and
//! x30 = the restorer: the program's own if it set `SA_RESTORER` (C
//! libraries don't on arm64), otherwise the vDSO's `__kernel_rt_sigreturn`.

use crate::{get32, get64, put};

pub const SIGINFO_SIZE: usize = 128;
/// Offsets in the `rt_sigframe`: the `ucontext` follows the `siginfo`.
pub const UC: usize = SIGINFO_SIZE;
/// In the `ucontext`.
pub const UC_STACK: usize = 16;
pub const UC_SIGMASK: usize = 40;
pub const UC_MCONTEXT: usize = 176;
pub const UCONTEXT_SIZE: usize = 4560;
/// In the `sigcontext`.
pub const SC_FAULT_ADDRESS: usize = 0;
pub const SC_REGS: usize = 8;
pub const SC_SP: usize = 256;
pub const SC_PC: usize = 264;
pub const SC_PSTATE: usize = 272;
pub const SC_RESERVED: usize = 288;
pub const RESERVED_SIZE: usize = 4096;
/// The whole `rt_sigframe` (a multiple of 16).
pub const FRAME_SIZE: usize = UC + UCONTEXT_SIZE;
/// The frame record above it.
pub const RECORD_SIZE: usize = 16;

/// Records in the reserved area: `{u32 magic, u32 size}` then the data.
pub const FPSIMD_MAGIC: u32 = 0x4650_8001;
pub const FPSIMD_SIZE: usize = 528;
pub const ESR_MAGIC: u32 = 0x4553_5201;
pub const ESR_SIZE: usize = 16;
pub const EXTRA_MAGIC: u32 = 0x4558_5401;
pub const SVE_MAGIC: u32 = 0x5356_4501;

/// The stack a signal needs at least (`AT_MINSIGSTKSZ`): the frame, the
/// record and the alignment Linux allows for.
pub const MIN_STACK: u64 = (FRAME_SIZE + RECORD_SIZE + 16) as u64;

/// `stack_t` flags.
pub const SS_ONSTACK: u32 = 1;
pub const SS_DISABLE: u32 = 2;

/// PSTATE bits: NZCV, and what must be clear for EL0 AArch64 (the mode,
/// nRW, DAIF).
pub const PSTATE_NZCV: u64 = 0xF000_0000;
const PSTATE_EL0_MUST_BE_ZERO: u64 = 0x3DF;

/// A thread's general registers as the signal frame holds them.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Regs {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

/// The FP/SIMD registers: v0-v31, FPSR and FPCR.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct FpSimd {
    pub v: [u128; 32],
    pub fpsr: u32,
    pub fpcr: u32,
}

/// Size of `struct user_fpsimd_state`: v0-v31, then FPSR and FPCR (the
/// layout the kernel saves a thread's registers in).
pub const USER_FPSIMD_SIZE: usize = 528;

impl FpSimd {
    /// From a `user_fpsimd_state`.
    pub fn from_user_state(b: &[u8]) -> FpSimd {
        let mut v = [0u128; 32];
        for (i, r) in v.iter_mut().enumerate() {
            *r = u128::from_le_bytes(b[i * 16..i * 16 + 16].try_into().unwrap());
        }
        FpSimd { v, fpsr: get32(b, 512), fpcr: get32(b, 516) }
    }

    /// As a `user_fpsimd_state`.
    pub fn to_user_state(&self) -> [u8; USER_FPSIMD_SIZE] {
        let mut b = [0u8; USER_FPSIMD_SIZE];
        for (i, r) in self.v.iter().enumerate() {
            put(&mut b, i * 16, &r.to_le_bytes());
        }
        put(&mut b, 512, &self.fpsr.to_le_bytes());
        put(&mut b, 516, &self.fpcr.to_le_bytes());
        b
    }
}

/// `stack_t`: the alternate signal stack.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Stack {
    pub sp: u64,
    pub flags: u32,
    pub size: u64,
}

/// What goes into a frame.
pub struct Delivery<'a> {
    pub regs: Regs,
    pub fpsimd: FpSimd,
    /// The mask to put back on `rt_sigreturn`.
    pub mask: u64,
    pub stack: Stack,
    pub siginfo: &'a [u8; SIGINFO_SIZE],
    /// `sigcontext.fault_address` (the faulting address for memory faults).
    pub fault_address: u64,
    /// The exception syndrome of a fault (an `esr_context` record).
    pub esr: Option<u64>,
}

/// Where a frame goes below `sp_top` (the interrupted stack pointer, or
/// the top of the alternate stack).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The `rt_sigframe` (the handler's sp).
    pub frame: u64,
    /// The frame record (the handler's x29).
    pub record: u64,
}

pub fn place(sp_top: u64) -> Placement {
    let record = (sp_top - RECORD_SIZE as u64) & !15;
    let frame = (record & !15) - FRAME_SIZE as u64;
    Placement { frame, record }
}

/// The `rt_sigframe` for `d`.
pub fn write_frame(d: &Delivery, out: &mut [u8; FRAME_SIZE]) {
    out.fill(0);
    put(out, 0, d.siginfo);
    // ucontext: uc_flags and uc_link stay 0.
    put(out, UC + UC_STACK, &d.stack.sp.to_le_bytes());
    put(out, UC + UC_STACK + 8, &d.stack.flags.to_le_bytes());
    put(out, UC + UC_STACK + 16, &d.stack.size.to_le_bytes());
    put(out, UC + UC_SIGMASK, &d.mask.to_le_bytes());
    let sc = UC + UC_MCONTEXT;
    put(out, sc + SC_FAULT_ADDRESS, &d.fault_address.to_le_bytes());
    for (i, x) in d.regs.x.iter().enumerate() {
        put(out, sc + SC_REGS + i * 8, &x.to_le_bytes());
    }
    put(out, sc + SC_SP, &d.regs.sp.to_le_bytes());
    put(out, sc + SC_PC, &d.regs.pc.to_le_bytes());
    put(out, sc + SC_PSTATE, &d.regs.pstate.to_le_bytes());
    // The records.
    let mut at = sc + SC_RESERVED;
    put(out, at, &FPSIMD_MAGIC.to_le_bytes());
    put(out, at + 4, &(FPSIMD_SIZE as u32).to_le_bytes());
    put(out, at + 8, &d.fpsimd.fpsr.to_le_bytes());
    put(out, at + 12, &d.fpsimd.fpcr.to_le_bytes());
    for (i, r) in d.fpsimd.v.iter().enumerate() {
        put(out, at + 16 + i * 16, &r.to_le_bytes());
    }
    at += FPSIMD_SIZE;
    if let Some(esr) = d.esr {
        put(out, at, &ESR_MAGIC.to_le_bytes());
        put(out, at + 4, &(ESR_SIZE as u32).to_le_bytes());
        put(out, at + 8, &esr.to_le_bytes());
        at += ESR_SIZE;
    }
    // The terminator: magic and size 0 (already zero).
    let _ = at;
}

/// The frame record: the interrupted frame pointer and link register.
pub fn record(regs: &Regs) -> [u8; RECORD_SIZE] {
    let mut b = [0u8; RECORD_SIZE];
    put(&mut b, 0, &regs.x[29].to_le_bytes());
    put(&mut b, 8, &regs.x[30].to_le_bytes());
    b
}

/// The registers the handler starts with.
pub fn handler_regs(regs: &Regs, sig: u32, at: Placement, handler: u64, restorer: u64, siginfo: bool) -> Regs {
    let mut r = *regs;
    r.x[0] = sig as u64;
    if siginfo {
        r.x[1] = at.frame;
        r.x[2] = at.frame + UC as u64;
    }
    r.sp = at.frame;
    r.x[29] = at.record;
    r.x[30] = restorer;
    r.pc = handler;
    // The condition flags stay; BTYPE and the rest start clear.
    r.pstate &= PSTATE_NZCV;
    r
}

/// What `rt_sigreturn` puts back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Restored {
    pub regs: Regs,
    pub fpsimd: FpSimd,
    pub mask: u64,
    pub stack: Stack,
}

/// Why a frame was refused (the process then gets `SIGSEGV`, as on Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BadFrame {
    /// PSTATE isn't EL0 AArch64 with interrupts unmasked.
    Pstate,
    /// A record's size runs past the area, or isn't one Linux accepts.
    Record,
    /// No FP/SIMD record.
    NoFpSimd,
    /// An `extra_context` or SVE record (never written by this kernel).
    Unsupported,
}

/// Reads the `rt_sigframe` a handler returns with (`rt_sigreturn`), as
/// Linux's `restore_sigframe` and `parse_user_sigframe` check it.
pub fn read_frame(b: &[u8; FRAME_SIZE]) -> Result<Restored, BadFrame> {
    let sc = UC + UC_MCONTEXT;
    let mut regs = Regs::default();
    for (i, x) in regs.x.iter_mut().enumerate() {
        *x = get64(b, sc + SC_REGS + i * 8);
    }
    regs.sp = get64(b, sc + SC_SP);
    regs.pc = get64(b, sc + SC_PC);
    regs.pstate = get64(b, sc + SC_PSTATE);
    if regs.pstate & PSTATE_EL0_MUST_BE_ZERO != 0 {
        return Err(BadFrame::Pstate);
    }
    let mut fpsimd = None;
    let (start, limit) = (sc + SC_RESERVED, sc + SC_RESERVED + RESERVED_SIZE);
    let mut at = start;
    loop {
        if limit - at < 8 {
            return Err(BadFrame::Record);
        }
        let (magic, size) = (get32(b, at), get32(b, at + 4) as usize);
        match magic {
            0 if size == 0 => break,
            0 => return Err(BadFrame::Record),
            FPSIMD_MAGIC => {
                if fpsimd.is_some() || size != FPSIMD_SIZE || limit - at < size {
                    return Err(BadFrame::Record);
                }
                let mut v = [0u128; 32];
                for (i, r) in v.iter_mut().enumerate() {
                    let o = at + 16 + i * 16;
                    *r = u128::from_le_bytes(b[o..o + 16].try_into().unwrap());
                }
                fpsimd = Some(FpSimd { v, fpsr: get32(b, at + 8), fpcr: get32(b, at + 12) });
            }
            ESR_MAGIC => {} // ignored, as on Linux
            EXTRA_MAGIC | SVE_MAGIC => return Err(BadFrame::Unsupported),
            _ => return Err(BadFrame::Record),
        }
        if size < 8 || limit - at < size {
            return Err(BadFrame::Record);
        }
        at += size;
    }
    let fpsimd = fpsimd.ok_or(BadFrame::NoFpSimd)?;
    let stack =
        Stack { sp: get64(b, UC + UC_STACK), flags: get32(b, UC + UC_STACK + 8), size: get64(b, UC + UC_STACK + 16) };
    Ok(Restored { regs, fpsimd, mask: get64(b, UC + UC_SIGMASK), stack })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> (Regs, FpSimd) {
        let mut regs = Regs::default();
        for (i, x) in regs.x.iter_mut().enumerate() {
            *x = 0x1000 + i as u64;
        }
        regs.sp = 0x7fff_f000;
        regs.pc = 0x40_1234;
        regs.pstate = 0x6000_0000;
        let mut fp = FpSimd::default();
        for (i, v) in fp.v.iter_mut().enumerate() {
            *v = (i as u128) << 64 | 0xabcd;
        }
        fp.fpsr = 0x10;
        fp.fpcr = 0x0300_0000;
        (regs, fp)
    }

    #[test]
    fn a_frame_reads_back_as_written() {
        let (regs, fpsimd) = sample();
        let info = [7u8; SIGINFO_SIZE];
        let stack = Stack { sp: 0x5000_0000, flags: SS_DISABLE, size: 0 };
        let d = Delivery {
            regs,
            fpsimd,
            mask: 0x8001,
            stack,
            siginfo: &info,
            fault_address: 0xdead,
            esr: Some(0x9200_0047),
        };
        let mut f = [0u8; FRAME_SIZE];
        write_frame(&d, &mut f);
        assert_eq!(&f[..SIGINFO_SIZE], &info[..]);
        let sc = UC + UC_MCONTEXT;
        assert_eq!(get64(&f, sc + SC_FAULT_ADDRESS), 0xdead);
        // FP/SIMD first, then the syndrome, then the terminator.
        assert_eq!(get32(&f, sc + SC_RESERVED), FPSIMD_MAGIC);
        assert_eq!(get32(&f, sc + SC_RESERVED + FPSIMD_SIZE), ESR_MAGIC);
        assert_eq!(get64(&f, sc + SC_RESERVED + FPSIMD_SIZE + 8), 0x9200_0047);
        assert_eq!(get64(&f, sc + SC_RESERVED + FPSIMD_SIZE + ESR_SIZE), 0);
        let r = read_frame(&f).unwrap();
        assert_eq!(r, Restored { regs, fpsimd, mask: 0x8001, stack });
    }

    #[test]
    fn the_handler_starts_as_linux_starts_it() {
        let (regs, _) = sample();
        let at = place(0x7fff_f008);
        // The record 16-byte aligned below the top, the frame below it.
        assert_eq!(at.record, 0x7fff_eff0);
        assert_eq!(at.frame, 0x7fff_eff0 - FRAME_SIZE as u64);
        assert_eq!(at.frame % 16, 0);
        let h = handler_regs(&regs, 10, at, 0x40_0000, 0x7f00_1000, true);
        assert_eq!((h.x[0], h.x[1], h.x[2]), (10, at.frame, at.frame + UC as u64));
        assert_eq!((h.sp, h.x[29], h.x[30], h.pc), (at.frame, at.record, 0x7f00_1000, 0x40_0000));
        assert_eq!(h.x[3], regs.x[3]);
        // Without SA_SIGINFO, x1 and x2 are left alone.
        let h = handler_regs(&regs, 10, at, 0x40_0000, 0x7f00_1000, false);
        assert_eq!((h.x[1], h.x[2]), (regs.x[1], regs.x[2]));
        assert_eq!(record(&regs), {
            let mut b = [0u8; 16];
            b[..8].copy_from_slice(&regs.x[29].to_le_bytes());
            b[8..].copy_from_slice(&regs.x[30].to_le_bytes());
            b
        });
    }

    #[test]
    fn bad_frames_are_refused() {
        let (regs, fpsimd) = sample();
        let info = [0u8; SIGINFO_SIZE];
        let d =
            Delivery { regs, fpsimd, mask: 0, stack: Stack::default(), siginfo: &info, fault_address: 0, esr: None };
        let mut good = [0u8; FRAME_SIZE];
        write_frame(&d, &mut good);
        let sc = UC + UC_MCONTEXT;
        // EL1 or AArch32 or masked interrupts.
        for bad_pstate in [0x5u64, 0x10, 0x80] {
            let mut f = good;
            put(&mut f, sc + SC_PSTATE, &bad_pstate.to_le_bytes());
            assert_eq!(read_frame(&f), Err(BadFrame::Pstate));
        }
        // No FP/SIMD record.
        let mut f = good;
        put(&mut f, sc + SC_RESERVED, &[0u8; 8]);
        assert_eq!(read_frame(&f), Err(BadFrame::NoFpSimd));
        // A wrong size.
        let mut f = good;
        put(&mut f, sc + SC_RESERVED + 4, &512u32.to_le_bytes());
        assert_eq!(read_frame(&f), Err(BadFrame::Record));
        // An unknown record, and an extra_context.
        let mut f = good;
        put(&mut f, sc + SC_RESERVED + FPSIMD_SIZE, &0x1234_5678u32.to_le_bytes());
        put(&mut f, sc + SC_RESERVED + FPSIMD_SIZE + 4, &16u32.to_le_bytes());
        assert_eq!(read_frame(&f), Err(BadFrame::Record));
        put(&mut f, sc + SC_RESERVED + FPSIMD_SIZE, &EXTRA_MAGIC.to_le_bytes());
        assert_eq!(read_frame(&f), Err(BadFrame::Unsupported));
        // A record that runs past the area.
        let mut f = good;
        put(&mut f, sc + SC_RESERVED + FPSIMD_SIZE, &ESR_MAGIC.to_le_bytes());
        put(&mut f, sc + SC_RESERVED + FPSIMD_SIZE + 4, &8192u32.to_le_bytes());
        assert_eq!(read_frame(&f), Err(BadFrame::Record));
    }

    #[test]
    fn fp_state_converts_both_ways() {
        let (_, fp) = sample();
        let raw = fp.to_user_state();
        assert_eq!(get32(&raw, 512), 0x10);
        assert_eq!(FpSimd::from_user_state(&raw), fp);
    }
}
