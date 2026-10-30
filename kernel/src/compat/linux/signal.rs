//! Linux signals: actions, masks, sending, and delivery on the way back to
//! user mode.
//!
//! A signal sent to a process (or to one of its threads) whose action is the
//! default either ends the process at once or is dropped, as Linux does. One
//! with a handler, or one that is blocked, waits in a pending set — the
//! process's or the thread's. Threads blocked in a system call notice it
//! (`proc::interrupted`) and return `EINTR`; on the way back to user mode a
//! thread that doesn't block the signal builds a signal frame on its stack
//! (the return address of the program's restorer, a `ucontext` with the
//! registers, mask and SIMD state, and the `siginfo`) and enters the
//! handler. `rt_sigreturn` restores what the frame holds. A call cut short
//! by a signal whose handler has `SA_RESTART` (or by one that turned out to
//! need no handler) starts again.
//!
//! Signals reach a thread when it returns from a system call, or from an
//! interrupt (the timer, or the reschedule IPI a sender raises on the CPU
//! the thread runs on), so a thread busy in user code takes them too. A
//! fault in a thread (a bad memory access, an invalid instruction, a
//! division by zero, `int3`) becomes a signal for a handler the program set
//! (`force`); without one the process crashes, reported as before.

use super::nr;
use crate::arch::fpu::FpuState;
use crate::arch::syscall::{user_rflags, SyscallFrame};
use crate::arch::trap::TrapFrame;
use crate::proc::{self, Process};
use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use crate::syscall::user;
use alloc::collections::BTreeMap;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

pub const SIGKILL: u32 = 9;
pub const SIGSEGV: u32 = 11;
pub const SIGPIPE: u32 = 13;
pub const SIGCHLD: u32 = 17;
pub const SIGSTOP: u32 = 19;

const SIG_DFL: u64 = 0;
const SIG_IGN: u64 = 1;
const SA_NOCLDSTOP: u64 = 1;
const SA_SIGINFO: u64 = 4;
const SA_ONSTACK: u64 = 0x0800_0000;
const SA_RESTART: u64 = 0x1000_0000;
const SA_NODEFER: u64 = 0x4000_0000;
const SA_RESETHAND: u64 = 0x8000_0000;
const SS_ONSTACK: u32 = 1;
const SS_DISABLE: u32 = 2;
/// `si_code` values.
pub const SI_USER: i32 = 0;
pub const SI_TKILL: i32 = -6;
const CLD_EXITED: i32 = 1;
const CLD_KILLED: i32 = 2;

/// Signals that can be neither caught, blocked nor ignored.
const UNBLOCKABLE: u64 = bit(SIGKILL) | bit(SIGSTOP);

const fn bit(sig: u32) -> u64 {
    1 << (sig - 1)
}

/// What a signal does when nobody handles it.
fn ends_process(sig: u32) -> bool {
    // CHLD, CONT, URG and WINCH are ignored; STOP, TSTP, TTIN and TTOU would
    // stop the process, which WaveOS doesn't do yet (they are ignored too).
    !matches!(sig, 17..=23 | 28)
}

/// An action set with `rt_sigaction`.
#[derive(Clone, Copy, Default)]
pub struct Action {
    handler: u64,
    flags: u64,
    restorer: u64,
    mask: u64,
}

/// What a pending signal carries into its handler's `siginfo`.
#[derive(Clone, Copy, Default)]
pub struct Info {
    pub code: i32,
    pub pid: u32,
    pub status: i32,
    pub addr: u64,
}

#[derive(Clone, Copy, Default)]
struct Thread {
    blocked: u64,
    pending: u64,
    /// `sigaltstack`: base and size (0: none).
    alt_sp: u64,
    alt_size: u64,
    /// The mask to put back after a handler that ends `rt_sigsuspend`.
    suspended: Option<u64>,
}

struct State {
    actions: [Action; 64],
    /// Sent to the process: any thread that doesn't block it takes it.
    pending: u64,
    info: [Info; 64],
    thread_info: BTreeMap<(TaskId, u32), Info>,
    threads: BTreeMap<TaskId, Thread>,
    /// The mask of threads not seen yet (a forked child's first thread).
    initial_mask: u64,
}

impl State {
    fn thread(&mut self, task: TaskId) -> &mut Thread {
        let mask = self.initial_mask;
        self.threads.entry(task).or_insert(Thread { blocked: mask, ..Default::default() })
    }
}

/// A Linux process's signal state.
pub struct Signals {
    state: IrqMutex<State>,
    /// Something is pending somewhere (a quick check before taking the lock).
    any: AtomicBool,
    /// Threads with a mask to put back on the way out (`suspended`).
    saved_masks: AtomicU32,
}

impl Signals {
    pub fn new() -> Signals {
        Signals {
            state: IrqMutex::new(State {
                actions: [Action::default(); 64],
                pending: 0,
                info: [Info::default(); 64],
                thread_info: BTreeMap::new(),
                threads: BTreeMap::new(),
                initial_mask: 0,
            }),
            any: AtomicBool::new(false),
            saved_masks: AtomicU32::new(0),
        }
    }

    /// The state a child made by `fork` starts with: the same actions, and the
    /// calling thread's mask and alternate stack; nothing pending.
    pub fn for_child(&self) -> Signals {
        let child = Signals::new();
        let me = sched::current_id();
        let mut s = self.state.lock();
        let t = *s.thread(me);
        let mut c = child.state.lock();
        c.actions = s.actions;
        c.initial_mask = t.blocked;
        drop(c);
        child
    }

    /// After `exec`: handlers go back to the default (ignored signals stay
    /// ignored); the mask and pending signals stay.
    pub fn exec_reset(&self) {
        let me = sched::current_id();
        let mut s = self.state.lock();
        for a in s.actions.iter_mut() {
            if a.handler != SIG_IGN {
                *a = Action::default();
            }
        }
        let t = *s.thread(me);
        s.threads.clear();
        s.threads.insert(me, Thread { blocked: t.blocked, pending: t.pending, ..Default::default() });
    }

    /// A new thread (`clone`) starts with its creator's mask.
    pub fn thread_started(&self, creator: TaskId, task: TaskId) {
        let mut s = self.state.lock();
        let blocked = s.thread(creator).blocked;
        s.threads.insert(task, Thread { blocked, ..Default::default() });
    }

    pub fn thread_exited(&self, task: TaskId) {
        let mut s = self.state.lock();
        s.threads.remove(&task);
        s.thread_info.retain(|&(t, _), _| t != task);
    }

    /// True if thread `task` has a signal it doesn't block.
    pub fn deliverable(&self, task: TaskId) -> bool {
        if !self.any.load(Ordering::Acquire) {
            return false;
        }
        // (Called while the caller sleeps: no allocation here.)
        let s = self.state.lock();
        let (blocked, mine) = s.threads.get(&task).map_or((s.initial_mask, 0), |t| (t.blocked, t.pending));
        (s.pending | mine) & !blocked != 0
    }

    fn refresh_any(&self, s: &State) {
        let any = s.pending != 0 || s.threads.values().any(|t| t.pending != 0);
        self.any.store(any, Ordering::Release);
    }

    /// The parent's side of a child's end: `SIGCHLD`.
    pub fn child_exited(&self, parent: &Process, child: u32, code: i64) {
        let info = if code < 0 {
            Info { code: CLD_KILLED, pid: child, status: (-code) as i32, addr: 0 }
        } else {
            Info { code: CLD_EXITED, pid: child, status: code as i32, addr: 0 }
        };
        send(parent, SIGCHLD, info, None);
    }
}

/// Sends `sig` to process `p`, or to one of its threads.
pub fn send(p: &Process, sig: u32, info: Info, thread: Option<TaskId>) {
    if sig == 0 || sig > 64 || p.exit_code().is_some() {
        return;
    }
    let Some(l) = &p.linux else {
        // WaveOS programs have no handlers: the usual ending signals end them.
        if matches!(sig, 1 | 2 | 3 | 6 | 9 | 15) {
            let _ = proc::kill(p.pid);
        }
        return;
    };
    if sig == SIGKILL {
        proc::terminate_by_signal(p, sig);
        return;
    }
    let mut s = l.signals.state.lock();
    let action = s.actions[sig as usize - 1];
    if action.handler == SIG_IGN || (action.handler == SIG_DFL && !ends_process(sig)) {
        return;
    }
    if action.handler == SIG_DFL {
        // Ends the process now, unless every thread it could go to blocks it.
        let blocked = match thread {
            Some(t) => s.thread(t).blocked & bit(sig) != 0,
            None => !s.threads.is_empty() && s.threads.values().all(|t| t.blocked & bit(sig) != 0),
        };
        if !blocked {
            drop(s);
            proc::terminate_by_signal(p, sig);
            return;
        }
    }
    match thread {
        Some(t) => {
            s.thread(t).pending |= bit(sig);
            s.thread_info.insert((t, sig), info);
        }
        None => {
            s.pending |= bit(sig);
            s.info[sig as usize - 1] = info;
        }
    }
    l.signals.any.store(true, Ordering::Release);
    drop(s);
    // Sleeping threads wake up; running ones are interrupted, so the
    // signal arrives even in a thread busy in user code.
    match thread {
        Some(t) => {
            sched::wake(t);
            sched::interrupt(t);
        }
        None => {
            sched::wake_process(p.pid);
            sched::interrupt_process(p.pid);
        }
    }
}

/// Takes the lowest signal thread `task` may receive now.
fn take(s: &mut State, task: TaskId) -> Option<(u32, Info)> {
    let pending = s.pending;
    let t = s.thread(task);
    let ready = (t.pending | pending) & !t.blocked;
    if ready == 0 {
        return None;
    }
    let sig = ready.trailing_zeros() + 1;
    let info = if t.pending & bit(sig) != 0 {
        t.pending &= !bit(sig);
        s.thread_info.remove(&(task, sig)).unwrap_or_default()
    } else {
        s.pending &= !bit(sig);
        s.info[sig as usize - 1]
    };
    Some((sig, info))
}

/// System calls that are never started again after a handler (Linux returns
/// `EINTR` from them whatever `SA_RESTART` says).
fn restartable(nr: u64) -> bool {
    !matches!(
        nr,
        nr::POLL
            | nr::PPOLL
            | nr::SELECT
            | nr::PSELECT6
            | nr::EPOLL_WAIT
            | nr::EPOLL_PWAIT
            | nr::EPOLL_PWAIT2
            | nr::NANOSLEEP
            | nr::CLOCK_NANOSLEEP
            | nr::RT_SIGSUSPEND
            | nr::PAUSE
            | nr::RT_SIGTIMEDWAIT
            | nr::RT_SIGRETURN
    )
}

const EINTR_RET: u64 = (-(EINTR as i64)) as u64;
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
    fn from_syscall(f: &SyscallFrame) -> Regs {
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

    fn write_syscall(&self, f: &mut SyscallFrame) {
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

    fn from_trap(f: &TrapFrame) -> Regs {
        Regs {
            g: [
                f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15, f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx,
                f.rsp, f.rip, f.rflags,
            ],
            err: f.error,
            trapno: f.vector,
        }
    }

    fn write_trap(&self, f: &mut TrapFrame) {
        let g = &self.g;
        [f.r8, f.r9, f.r10, f.r11, f.r12, f.r13, f.r14, f.r15] = [g[0], g[1], g[2], g[3], g[4], g[5], g[6], g[7]];
        [f.rdi, f.rsi, f.rbp, f.rbx, f.rdx, f.rax, f.rcx] = [g[8], g[9], g[10], g[11], g[12], g[13], g[14]];
        f.rsp = g[R_RSP];
        f.rip = g[R_RIP];
        f.rflags = user_rflags(g[R_RFLAGS]);
    }
}

/// What to do about the signals pending for thread `me`.
enum Next {
    Nothing,
    /// The process ended (a signal's default action).
    Ended,
    /// Enter this handler (the thread's mask is already updated).
    Handler {
        sig: u32,
        info: Info,
        action: Action,
        old_mask: u64,
        thread: Thread,
    },
}

/// Takes the next signal thread `me` should act on.
fn next(p: &Process, l: &super::Linux, me: TaskId) -> Next {
    loop {
        let mut s = l.signals.state.lock();
        let Some((sig, info)) = take(&mut s, me) else {
            if let Some(mask) = s.thread(me).suspended.take() {
                s.thread(me).blocked = mask;
                l.signals.saved_masks.fetch_sub(1, Ordering::AcqRel);
            }
            l.signals.refresh_any(&s);
            return Next::Nothing;
        };
        let action = s.actions[sig as usize - 1];
        l.signals.refresh_any(&s);
        if action.handler == SIG_IGN || (action.handler == SIG_DFL && !ends_process(sig)) {
            continue;
        }
        if action.handler == SIG_DFL {
            drop(s);
            proc::terminate_by_signal(p, sig);
            return Next::Ended;
        }
        let (old_mask, thread) = handler_mask(&mut s, l, me, sig, &action);
        return Next::Handler { sig, info, action, old_mask, thread };
    }
}

/// Blocks what a handler for `sig` blocks while it runs; returns the mask
/// to put back afterwards, and the thread's state.
fn handler_mask(s: &mut State, l: &super::Linux, me: TaskId, sig: u32, action: &Action) -> (u64, Thread) {
    let t = *s.thread(me);
    let old_mask = t.suspended.unwrap_or(t.blocked);
    if t.suspended.is_some() {
        l.signals.saved_masks.fetch_sub(1, Ordering::AcqRel);
    }
    let th = s.thread(me);
    th.suspended = None;
    th.blocked |= action.mask & !UNBLOCKABLE;
    if action.flags & SA_NODEFER == 0 {
        th.blocked |= bit(sig);
    }
    if action.flags & SA_RESETHAND != 0 {
        s.actions[sig as usize - 1] = Action::default();
    }
    (old_mask, t)
}

/// On the way back to user mode from an interrupt or a fault: enters the
/// handler of a pending signal.
pub fn on_trap_return(f: &mut TrapFrame) {
    let pid = sched::current_pid();
    if pid == 0 {
        return;
    }
    let Some(p) = proc::get(pid) else { return };
    let Some(l) = &p.linux else { return };
    if !l.signals.any.load(Ordering::Acquire) && l.signals.saved_masks.load(Ordering::Acquire) == 0 {
        return;
    }
    if let Next::Handler { sig, info, action, old_mask, thread } = next(&p, l, sched::current_id()) {
        let mut regs = Regs::from_trap(f);
        if enter_handler(&mut regs, sig, info, &action, old_mask, &thread).is_err() {
            proc::terminate_by_signal(&p, SIGSEGV);
            return;
        }
        regs.write_trap(f);
    }
}

/// A fault in the running thread: `sig` goes straight to its handler, if
/// the program set one and the thread doesn't block it (true: the frame now
/// enters the handler). Otherwise false, and the process crashes.
pub fn force(p: &Process, sig: u32, info: Info, f: &mut TrapFrame) -> bool {
    let Some(l) = &p.linux else { return false };
    let me = sched::current_id();
    let mut s = l.signals.state.lock();
    let action = s.actions[sig as usize - 1];
    if action.handler == SIG_DFL || action.handler == SIG_IGN || s.thread(me).blocked & bit(sig) != 0 {
        return false;
    }
    let (old_mask, thread) = handler_mask(&mut s, l, me, sig, &action);
    l.signals.refresh_any(&s);
    drop(s);
    let mut regs = Regs::from_trap(f);
    if enter_handler(&mut regs, sig, info, &action, old_mask, &thread).is_err() {
        return false;
    }
    regs.write_trap(f);
    true
}

/// On the way back to user mode from system call `nr` (its result is in
/// `f.rax`): restarts it, or enters the handler of a pending signal.
pub fn on_return(f: &mut SyscallFrame, nr: u64, p: &alloc::sync::Arc<Process>) {
    let Some(l) = &p.linux else { return };
    let me = sched::current_id();
    let interrupted = f.rax == EINTR_RET && nr != nr::RT_SIGRETURN;
    // Nearly always nothing to do: say so without the lock, which every
    // thread of the process would otherwise take on every system call.
    if !interrupted && !l.signals.any.load(Ordering::Acquire) && l.signals.saved_masks.load(Ordering::Acquire) == 0 {
        return;
    }
    match next(p, l, me) {
        Next::Nothing => {
            // Woken for a signal another thread took: carry on as if nothing happened.
            if interrupted && restartable(nr) {
                restart(f, nr);
            }
        }
        Next::Ended => {}
        Next::Handler { sig, info, action, old_mask, thread } => {
            if interrupted && action.flags & SA_RESTART != 0 && restartable(nr) {
                restart(f, nr);
            }
            let mut regs = Regs::from_syscall(f);
            if enter_handler(&mut regs, sig, info, &action, old_mask, &thread).is_err() {
                // No room for the frame: the process ends as if it had crashed.
                proc::terminate_by_signal(p, SIGSEGV);
                return;
            }
            regs.write_syscall(f);
        }
    }
}

/// Sets the frame up to make system call `nr` again.
fn restart(f: &mut SyscallFrame, nr: u64) {
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

/// Builds the signal frame on the user stack and points `f` at the handler.
fn enter_handler(r: &mut Regs, sig: u32, info: Info, a: &Action, old_mask: u64, t: &Thread) -> Result<(), isize> {
    let rsp = r.g[R_RSP];
    let on_alt = t.alt_size != 0 && (t.alt_sp..t.alt_sp + t.alt_size).contains(&rsp);
    let use_alt = a.flags & SA_ONSTACK != 0 && t.alt_size != 0 && !on_alt;
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

    let mut si = [0u8; 128];
    si[0..4].copy_from_slice(&sig.to_le_bytes());
    si[8..12].copy_from_slice(&info.code.to_le_bytes());
    if matches!(sig, 4 | 5 | 7 | 8 | SIGSEGV) && info.pid == 0 {
        // A fault's address (`si_addr`).
        si[16..24].copy_from_slice(&info.addr.to_le_bytes());
    } else {
        si[16..20].copy_from_slice(&info.pid.to_le_bytes());
        si[20..24].copy_from_slice(&(super::UID as u32).to_le_bytes());
        si[24..28].copy_from_slice(&info.status.to_le_bytes());
    }

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
    let _ = a.flags & SA_SIGINFO; // the arguments are passed either way
    Ok(())
}

/// `rt_sigreturn`: puts back what the signal frame at the stack holds.
pub fn sigreturn(p: &Process, f: &mut SyscallFrame) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
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
    let mask = r(UC_SIGMASK) & !UNBLOCKABLE;
    let mut s = l.signals.state.lock();
    s.thread(sched::current_id()).blocked = mask;
    l.signals.refresh_any(&s);
    Ok(f.rax)
}

/// `rt_sigaction`.
pub fn sigaction(p: &Process, sig: u64, act: u64, old: u64) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    if sig == 0 || sig > 64 {
        return Err(EINVAL);
    }
    let new = if act != 0 {
        let [handler, flags, restorer, mask]: [u64; 4] = user::get(act)?;
        if bit(sig as u32) & UNBLOCKABLE != 0 {
            return Err(EINVAL);
        }
        Some(Action { handler, flags, restorer, mask })
    } else {
        None
    };
    let mut s = l.signals.state.lock();
    let prev = s.actions[sig as usize - 1];
    if let Some(a) = new {
        s.actions[sig as usize - 1] = a;
        // Pending signals that are now ignored go away.
        if a.handler == SIG_IGN || (a.handler == SIG_DFL && !ends_process(sig as u32)) {
            s.pending &= !bit(sig as u32);
            for t in s.threads.values_mut() {
                t.pending &= !bit(sig as u32);
            }
            l.signals.refresh_any(&s);
        }
    }
    drop(s);
    if old != 0 {
        user::put(old, &[prev.handler, prev.flags, prev.restorer, prev.mask])?;
    }
    let _ = SA_NOCLDSTOP;
    Ok(0)
}

/// `rt_sigprocmask`.
pub fn sigprocmask(p: &Process, how: u64, set: u64, old: u64) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let new: Option<u64> = if set != 0 { Some(user::get(set)?) } else { None };
    let mut s = l.signals.state.lock();
    let t = s.thread(sched::current_id());
    let prev = t.blocked;
    if let Some(m) = new {
        t.blocked = match how {
            0 => prev | m,
            1 => prev & !m,
            2 => m,
            _ => return Err(EINVAL),
        } & !UNBLOCKABLE;
    }
    drop(s);
    if old != 0 {
        user::put(old, &prev)?;
    }
    Ok(0)
}

/// `sigaltstack`.
pub fn sigaltstack(p: &Process, new: u64, old: u64) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let me = sched::current_id();
    let cur = *l.signals.state.lock().thread(me);
    if old != 0 {
        let flags = if cur.alt_size == 0 { SS_DISABLE } else { 0 } as u64;
        user::put(old, &[cur.alt_sp, flags, cur.alt_size])?;
    }
    if new != 0 {
        let [sp, flags, size]: [u64; 3] = user::get(new)?;
        let (sp, size) = if flags as u32 & SS_DISABLE != 0 {
            (0, 0)
        } else if size < 2048 {
            return Err(ENOMEM);
        } else {
            (sp, size)
        };
        let mut s = l.signals.state.lock();
        let t = s.thread(me);
        t.alt_sp = sp;
        t.alt_size = size;
    }
    Ok(0)
}

/// `rt_sigpending`.
pub fn sigpending(p: &Process, set: u64) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let mut s = l.signals.state.lock();
    let pending = s.pending | s.thread(sched::current_id()).pending;
    drop(s);
    user::put(set, &pending).map(|_| 0)
}

/// `rt_sigsuspend` (with a mask) and `pause` (without): sleeps until a
/// signal comes; the handler runs before the call returns `EINTR`.
pub fn suspend(p: &Process, mask_ptr: Option<u64>) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let me = sched::current_id();
    if let Some(ptr) = mask_ptr {
        let mask: u64 = user::get(ptr)?;
        let mut s = l.signals.state.lock();
        let t = s.thread(me);
        if t.suspended.is_none() {
            l.signals.saved_masks.fetch_add(1, Ordering::AcqRel);
        }
        t.suspended = Some(t.blocked);
        t.blocked = mask & !UNBLOCKABLE;
    }
    while !proc::interrupted() {
        sched::wait_until(1000, proc::interrupted);
    }
    Err(EINTR)
}

/// `ppoll`/`pselect6`: waits with the mask at `mask_ptr`; the caller's own
/// mask comes back on the way out (after a handler, if one runs).
pub fn wait_with_mask(p: &Process, mask_ptr: u64) -> Result<(), isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let mask: u64 = user::get(mask_ptr)?;
    let mut s = l.signals.state.lock();
    let t = s.thread(sched::current_id());
    if t.suspended.is_none() {
        t.suspended = Some(t.blocked);
        l.signals.saved_masks.fetch_add(1, Ordering::AcqRel);
    }
    t.blocked = mask & !UNBLOCKABLE;
    Ok(())
}

/// `rt_sigtimedwait`: takes a signal in `set` without running its handler.
pub fn sigtimedwait(p: &Process, set_ptr: u64, info_ptr: u64, timeout: u64) -> Result<u64, isize> {
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let set: u64 = user::get(set_ptr)?;
    let deadline = if timeout != 0 {
        let [sec, nsec]: [i64; 2] = user::get(timeout)?;
        Some(crate::time::uptime_ms() + sec.max(0) as u64 * 1000 + nsec.max(0) as u64 / 1_000_000)
    } else {
        None
    };
    let me = sched::current_id();
    loop {
        {
            let mut s = l.signals.state.lock();
            let pending = s.pending;
            let t = s.thread(me);
            let ready = (t.pending | pending) & set;
            if ready != 0 {
                let sig = ready.trailing_zeros() + 1;
                let info = if t.pending & bit(sig) != 0 {
                    t.pending &= !bit(sig);
                    s.thread_info.remove(&(me, sig)).unwrap_or_default()
                } else {
                    s.pending &= !bit(sig);
                    s.info[sig as usize - 1]
                };
                l.signals.refresh_any(&s);
                drop(s);
                if info_ptr != 0 {
                    let mut si = [0u8; 128];
                    si[0..4].copy_from_slice(&sig.to_le_bytes());
                    si[8..12].copy_from_slice(&info.code.to_le_bytes());
                    si[16..20].copy_from_slice(&info.pid.to_le_bytes());
                    user::put_bytes(info_ptr, &si)?;
                }
                return Ok(sig as u64);
            }
        }
        let now = crate::time::uptime_ms();
        if deadline.is_some_and(|d| now >= d) {
            return Err(EAGAIN);
        }
        if proc::interrupted() {
            return Err(EINTR);
        }
        let wait = deadline.map_or(1000, |d| d - now);
        sched::wait_until(wait, || {
            let s = l.signals.state.lock();
            let t = s.threads.get(&me).copied().unwrap_or_default();
            (s.pending | t.pending) & set != 0 || proc::interrupted()
        });
    }
}

/// The task behind Linux thread id `tid` of process `p`.
pub fn task_of(p: &Process, tid: u64) -> Option<TaskId> {
    if tid == p.pid as u64 {
        Some(p.main_task.load(Ordering::Acquire))
    } else {
        let task = tid.checked_sub(0x10_0000)?;
        (sched::task_pid(task) == Some(p.pid)).then_some(task)
    }
}

/// `kill`: to a process (`pid` > 0), the caller's group (0), every process
/// it may signal (-1), or group `-pid`.
pub fn kill(p: &Process, pid: i64, sig: u64) -> Result<u64, isize> {
    if sig > 64 {
        return Err(EINVAL);
    }
    let info = Info { code: SI_USER, pid: p.pid, ..Default::default() };
    let targets = match pid {
        0 => proc::group(p.pgid.load(Ordering::Relaxed)),
        -1 => proc::all().into_iter().filter(|q| q.pid != p.pid && q.linux.is_some()).collect(),
        n if n < 0 => proc::group((-n) as u32),
        n => proc::get(n as u32).filter(|q| q.exit_code().is_none()).into_iter().collect(),
    };
    if targets.is_empty() {
        return Err(ESRCH);
    }
    for q in &targets {
        send(q, sig as u32, info, None);
    }
    Ok(0)
}

/// `tgkill` and `tkill`: to one thread.
pub fn tgkill(p: &Process, tgid: Option<u64>, tid: u64, sig: u64) -> Result<u64, isize> {
    if sig > 64 {
        return Err(EINVAL);
    }
    let target = match tgid {
        Some(g) => proc::get(g as u32).ok_or(ESRCH)?,
        None => proc::all()
            .into_iter()
            .find(|q| q.linux.is_some() && task_of(q, tid).is_some() && q.exit_code().is_none())
            .ok_or(ESRCH)?,
    };
    let task = task_of(&target, tid).ok_or(ESRCH)?;
    send(&target, sig as u32, Info { code: SI_TKILL, pid: p.pid, ..Default::default() }, Some(task));
    Ok(0)
}
