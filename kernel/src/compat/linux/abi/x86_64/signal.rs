//! x86-64 signal frames: on the user stack below the red zone, the SIMD
//! state (XSAVE's, with the markers Linux puts around it, or FXSAVE's),
//! then the return address of the program's restorer (`SA_RESTORER`), a
//! `ucontext` with the registers (`sigcontext`) and mask, and the
//! `siginfo`. `rt_sigreturn` restores what the frame holds; a call cut
//! short is made again by stepping back over the 2-byte `syscall`.

use crate::arch::fpu::FpuState;
use crate::arch::syscall::{user_rflags, SyscallFrame};
use crate::arch::trap::TrapFrame;
use crate::compat::linux::signal::{self, Action, Info, Thread, SIGSEGV, SS_DISABLE, SS_ONSTACK};
use crate::proc::{self, Process};
use crate::syscall::user;
use aurora_abi::err::*;

/// Markers of XSAVE data in a signal frame's SIMD state (Linux's).
const FP_XSTATE_MAGIC1: u32 = 0x4650_5853;
const FP_XSTATE_MAGIC2: u32 = 0x4650_5845;

/// A thread's registers, in the order of Linux's `sigcontext`: r8–r15, rdi,
/// rsi, rbp, rbx, rdx, rax, rcx, rsp, rip, rflags; and the trap's error code
/// and number.
#[derive(Clone, Copy, Default)]
pub struct Regs {
    g: [u64; 18],
    err: u64,
    trapno: u64,
}

const R_R11: usize = 3;
const R_RDI: usize = 8;
const R_RSI: usize = 9;
const R_RDX: usize = 12;
const R_RAX: usize = 13;
const R_RCX: usize = 14;
const R_RSP: usize = 15;
const R_RIP: usize = 16;
const R_RFLAGS: usize = 17;

impl Regs {
    pub fn from_syscall(f: &SyscallFrame) -> Regs {
        // `syscall` keeps rip in rcx and rflags in r11, unless the frame
        // holds the real ones (`full`, after `rt_sigreturn`).
        let (rcx, r11) = if f.full != 0 { (f.rcx, f.r11) } else { (f.rip, f.rflags) };
        Regs {
            g: [
                f.r8, f.r9, f.r10, r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, rcx,
                f.rsp, f.rip, f.rflags,
            ],
            ..Default::default()
        }
    }

    pub fn write_syscall(&self, f: &mut SyscallFrame) {
        let g = &self.g;
        [f.r8, f.r9, f.r10] = [g[0], g[1], g[2]];
        [f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax] =
            [g[4], g[5], g[6], g[7], g[8], g[9], g[10], g[11], g[12], g[13]];
        f.rsp = g[R_RSP];
        f.rip = g[R_RIP];
        f.rflags = user_rflags(g[R_RFLAGS]);
        f.rcx = g[R_RCX];
        f.r11 = g[R_R11];
    }

    pub fn from_trap(f: &TrapFrame) -> Regs {
        Regs {
            g: [
                f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx,
                f.rsp, f.rip, f.rflags,
            ],
            err: f.error,
            trapno: f.vector,
        }
    }

    pub fn write_trap(&self, f: &mut TrapFrame) {
        let g = &self.g;
        [f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15] = [g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7]];
        [f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx] = [g[8], g[9], g[10], g[11], g[12], g[13], g[14]];
        f.rsp = g[R_RSP];
        f.rip = g[R_RIP];
        f.rflags = user_rflags(g[R_RFLAGS]);
    }
}

/// Sets the frame up to make system call `nr` again.
pub fn restart(f: &mut SyscallFrame, nr: u64, _args: &[u64; 6]) {
    f.rip -= 2; // the `syscall` instruction
    f.rax = nr;
}

/// Bytes from the frame's start to the `ucontext` and the `siginfo`.
const UC: u64 = 8;
const INFO: u64 = UC + 304;
const FRAME: u64 = INFO + 128;
/// `ucontext` offsets: the stack, the registers (`sigcontext`), the mask.
const UC_STACK: u64 = 16;
const UC_MCONTEXT: u64 = 40;
const UC_SIGMASK: u64 = 296;

/// Builds the signal frame on the user stack and points `r` at the handler.
pub fn enter_handler(
    _p: &Process,
    r: &mut Regs,
    sig: u32,
    info: Info,
    a: &Action,
    old_mask: u64,
    t: &Thread,
) -> Result<(), isize> {
    let rsp = r.g[R_RSP];
    let on_alt = t.alt_size != 0 && (t.alt_sp..t.alt_sp + t.alt_size).contains(&rsp);
    let use_alt = a.flags & signal::SA_ONSTACK != 0 && t.alt_size != 0 && !on_alt;
    let mut sp = if use_alt { t.alt_sp + t.alt_size } else { rsp - 128 };
    // The SIMD state (XSAVE's, with the markers Linux puts around it, or
    // FXSAVE's), then the frame; the handler starts as if called.
    let size = crate::arch::fpu::size() as u64;
    sp = (sp - size - 4) & !63;
    let fpstate = sp;
    sp = ((sp - FRAME) & !15) - 8;

    let mut fpu = FpuState::new();
    fpu.save();
    user::put_bytes(fpstate, fpu.as_bytes())?;
    let xfeatures = crate::arch::fpu::xfeatures();
    if xfeatures != 0 {
        // struct _fpx_sw_bytes, in the FXSAVE area's spare bytes.
        user::put(fpstate + 464, &[FP_XSTATE_MAGIC1, size as u32 + 4])?;
        user::put(fpstate + 472, &xfeatures)?;
        user::put(fpstate + 480, &(size as u32))?;
        user::put(fpstate + size, &FP_XSTATE_MAGIC2)?;
    }

    let mut uc = [0u8; 304];
    let mut w = |off: u64, v: u64| uc[off as usize..off as usize + 8].copy_from_slice(&v.to_le_bytes());
    w(UC_STACK, t.alt_sp);
    w(
        UC_STACK + 8,
        if t.alt_size == 0 {
            SS_DISABLE
        } else if on_alt {
            SS_ONSTACK
        } else {
            0
        } as u64,
    );
    w(UC_STACK + 16, t.alt_size);
    for (i, v) in r.g.iter().enumerate() {
        w(UC_MCONTEXT + i as u64 * 8, *v);
    }
    // cs, gs, fs, ss
    w(UC_MCONTEXT + 144, crate::arch::gdt::USER_CS as u64 | (crate::arch::gdt::USER_SS as u64) << 48);
    w(UC_MCONTEXT + 152, r.err);
    w(UC_MCONTEXT + 160, r.trapno);
    w(UC_MCONTEXT + 168, old_mask); // oldmask
    w(UC_MCONTEXT + 176, info.addr); // cr2
    w(UC_MCONTEXT + 184, fpstate);
    w(UC_SIGMASK, old_mask);

    let si = signal::siginfo_bytes(sig, &info);

    user::put_bytes(sp, &a.restorer.to_le_bytes())?;
    user::put_bytes(sp + UC, &uc)?;
    user::put_bytes(sp + INFO, &si)?;

    r.g[R_RIP] = a.handler;
    r.g[R_RSP] = sp;
    r.g[R_RDI] = sig as u64;
    r.g[R_RSI] = sp + INFO;
    r.g[R_RDX] = sp + UC;
    r.g[R_RAX] = 0;
    r.g[R_RFLAGS] &= !0x500; // TF, DF
    Ok(())
}

/// `rt_sigreturn`: puts back what the signal frame at the stack holds.
pub fn sigreturn(p: &Process, f: &mut SyscallFrame) -> Result<u64, isize> {
    let uc = f.rsp; // the handler's `ret` popped the return address
    let mut raw = [0u8; 304];
    user::get_bytes(uc, &mut raw)?;
    let r = |off: u64| u64::from_le_bytes(raw[off as usize..off as usize + 8].try_into().unwrap());
    let m = |i: u64| r(UC_MCONTEXT + i * 8);
    f.r8 = m(0);
    f.r9 = m(1);
    f.r10 = m(2);
    // rcx and r11 too: the thread may have been interrupted anywhere, so
    // this call leaves with every register as the frame holds it.
    f.r11 = m(3);
    f.rcx = m(14);
    f.full = 1;
    f.r12 = m(4);
    f.r13 = m(5);
    f.r14 = m(6);
    f.r15 = m(7);
    f.rdi = m(8);
    f.rsi = m(9);
    f.rbp = m(10);
    f.rbx = m(11);
    f.rdx = m(12);
    f.rax = m(13);
    f.rsp = m(15);
    f.rip = m(16);
    f.rflags = user_rflags(m(17));
    if f.rip >= aurora_abi::layout::USER_END {
        proc::terminate_by_signal(p, SIGSEGV);
        return Err(EFAULT);
    }
    let fpstate = r(UC_MCONTEXT + 184);
    if fpstate != 0 {
        // XSAVE data when the frame says so, else the FXSAVE part.
        let magic: u32 = user::get(fpstate + 464)?;
        let len = if magic == FP_XSTATE_MAGIC1 { crate::arch::fpu::size() } else { 512 };
        let mut bytes = alloc::vec![0u8; len];
        user::get_bytes(fpstate, &mut bytes)?;
        FpuState::from_bytes(&bytes).restore();
    }
    signal::set_blocked(p, r(UC_SIGMASK));
    Ok(f.rax)
}
