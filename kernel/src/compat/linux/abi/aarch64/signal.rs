//! arm64 signal frames (`aurora_linuxabi::aarch64::signal` lays them out):
//! the frame record and `rt_sigframe` on the user stack, x30 = the
//! restorer — the vDSO's `__kernel_rt_sigreturn` unless the program set
//! `SA_RESTORER` — and `rt_sigreturn` checking and restoring every
//! register, the FP/SIMD state, the mask and the alternate stack. A call
//! cut short is made again by stepping back over the 4-byte `svc` with
//! its first argument back in x0 (Linux's `orig_x0`; x8 is never changed).

use crate::arch::fpu::FpuState;
use crate::arch::syscall::{user_pstate, SyscallFrame};
use crate::arch::trap::TrapFrame;
use crate::compat::linux::signal::{self, Action, Info, Thread, SIGSEGV, SS_DISABLE, SS_ONSTACK};
use crate::proc::{self, Process};
use crate::syscall::user;
use aurora_abi::err::*;
use aurora_linuxabi::aarch64::signal as frame;

const SA_RESTORER: u64 = 0x0400_0000;

/// A thread's registers, and the syndrome of the exception that stopped
/// it (for a fault's `esr_context`).
#[derive(Clone, Copy, Default)]
pub struct Regs {
    r: frame::Regs,
    esr: u64,
}

impl Regs {
    pub fn from_trap(f: &TrapFrame) -> Regs {
        Regs { r: frame::Regs { x: f.x, sp: f.sp, pc: f.pc, pstate: f.pstate }, esr: f.esr }
    }

    pub fn write_trap(&self, f: &mut TrapFrame) {
        f.x = self.r.x;
        f.sp = self.r.sp;
        f.pc = self.r.pc;
        f.pstate = user_pstate(self.r.pstate);
    }

    /// (A system call's frame is the trap frame on arm64.)
    pub fn from_syscall(f: &SyscallFrame) -> Regs {
        Regs::from_trap(f)
    }

    pub fn write_syscall(&self, f: &mut SyscallFrame) {
        self.write_trap(f)
    }
}

/// Sets the frame up to make system call `nr` again: `svc` again, with
/// the first argument the result replaced.
pub fn restart(f: &mut SyscallFrame, _nr: u64, args: &[u64; 6]) {
    f.pc -= 4;
    f.x[0] = args[0];
}

/// Builds the frame on the user stack and points `r` at the handler.
pub fn enter_handler(
    p: &Process,
    r: &mut Regs,
    sig: u32,
    info: Info,
    a: &Action,
    old_mask: u64,
    t: &Thread,
) -> Result<(), isize> {
    let sp = r.r.sp;
    let on_alt = t.alt_size != 0 && (t.alt_sp..t.alt_sp + t.alt_size).contains(&sp);
    let use_alt = a.flags & signal::SA_ONSTACK != 0 && t.alt_size != 0 && !on_alt;
    let at = frame::place(if use_alt { t.alt_sp + t.alt_size } else { sp });
    if use_alt && at.frame < t.alt_sp {
        return Err(EFAULT); // the alternate stack is too small
    }
    let restorer = if a.flags & SA_RESTORER != 0 { a.restorer } else { sigtramp(p) };
    if restorer == 0 {
        return Err(EFAULT);
    }
    // A fault (rather than a signal someone sent) carries its syndrome, and
    // memory faults their address.
    let fault = signal::is_fault(sig, &info);
    let mut fpu = FpuState::new();
    fpu.save();
    let siginfo = signal::siginfo_bytes(sig, &info);
    let stack = frame::Stack {
        sp: t.alt_sp,
        flags: if t.alt_size == 0 {
            SS_DISABLE
        } else if on_alt {
            SS_ONSTACK
        } else {
            0
        },
        size: t.alt_size,
    };
    let d = frame::Delivery {
        regs: r.r,
        fpsimd: frame::FpSimd::from_user_state(fpu.as_bytes()),
        mask: old_mask,
        stack,
        siginfo: &siginfo,
        fault_address: if fault && matches!(sig, 7 | SIGSEGV) { info.addr } else { 0 },
        esr: fault.then_some(r.esr),
    };
    let mut buf = alloc::vec![0u8; frame::FRAME_SIZE];
    frame::write_frame(&d, (&mut buf[..]).try_into().unwrap());
    user::put_bytes(at.frame, &buf)?;
    user::put_bytes(at.record, &frame::record(&r.r))?;
    r.r = frame::handler_regs(&r.r, sig, at, a.handler, restorer, a.flags & signal::SA_SIGINFO != 0);
    Ok(())
}

/// The vDSO's `__kernel_rt_sigreturn` in `p` (0: no vDSO).
fn sigtramp(p: &Process) -> u64 {
    let base = p.linux.as_ref().map_or(0, |l| l.vdso());
    match (base, crate::compat::linux::vdso::symbol("__kernel_rt_sigreturn")) {
        (0, _) | (_, None) => 0,
        (b, Some(off)) => b + off,
    }
}

/// `rt_sigreturn`: puts back what the frame at sp holds. A frame Linux
/// would refuse ends the process with `SIGSEGV`.
pub fn sigreturn(p: &Process, f: &mut SyscallFrame) -> Result<u64, isize> {
    let bad = |p: &Process| {
        proc::terminate_by_signal(p, SIGSEGV);
        Err(EFAULT)
    };
    if f.sp & 15 != 0 {
        return bad(p);
    }
    let mut buf = alloc::vec![0u8; frame::FRAME_SIZE];
    if user::get_bytes(f.sp, &mut buf).is_err() {
        return bad(p);
    }
    let Ok(restored) = frame::read_frame((&buf[..]).try_into().unwrap()) else { return bad(p) };
    if restored.regs.pc >= aurora_abi::layout::USER_END {
        return bad(p);
    }
    let s = restored.stack;
    if signal::restore_altstack(p, s.sp, s.flags, s.size).is_err() {
        return bad(p);
    }
    Regs { r: restored.regs, esr: 0 }.write_syscall(f);
    FpuState::from_bytes(&restored.fpsimd.to_user_state()).restore();
    signal::set_blocked(p, restored.mask);
    Ok(f.x[0])
}
