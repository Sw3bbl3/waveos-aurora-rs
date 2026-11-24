//! Exception entry: the vector table (VBAR_EL1), the [`TrapFrame`] every
//! exception saves, and the handler that sorts them out.
//!
//! Every exception — a system call (`svc`), a fault, an interrupt — saves
//! all general registers, the interrupted stack pointer, ELR, SPSR, ESR and
//! FAR in a [`TrapFrame`] on the kernel stack (SP_EL1: for exceptions from
//! EL0 the CPU switches to it by itself; it is the running task's kernel
//! stack, as the scheduler left it) and calls [`trap`], which may change
//! the frame: a system call's result, a signal handler's entry. The same
//! frame is the system call frame (`syscall::SyscallFrame`).

use core::arch::global_asm;

/// Registers at the time of the exception, lowest address first.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct TrapFrame {
    /// x0-x30 (x29 the frame pointer, x30 the link register).
    pub x: [u64; 31],
    /// The interrupted stack pointer (SP_EL0 for user code).
    pub sp: u64,
    /// Where execution resumes (ELR_EL1).
    pub pc: u64,
    /// The interrupted PSTATE (SPSR_EL1).
    pub pstate: u64,
    /// Exception syndrome and fault address (ESR_EL1, FAR_EL1).
    pub esr: u64,
    pub far: u64,
    /// Which vector: see the `KIND_` constants.
    pub kind: u64,
    pub _pad: u64,
}

/// Frame size: a multiple of 16, so SP stays aligned.
const FRAME: usize = core::mem::size_of::<TrapFrame>();
const _: () = assert!(FRAME == 304);

/// Vector kinds: from EL1 (kernel) 0-3, from EL0 (user) 4-7, and the
/// vectors that should never be taken (EL1 on SP_EL0, AArch32 EL0) 8.
pub const KIND_SYNC: u64 = 0;
pub const KIND_IRQ: u64 = 1;
pub const KIND_FIQ: u64 = 2;
pub const KIND_SERROR: u64 = 3;
pub const KIND_USER: u64 = 4;
pub const KIND_INVALID: u64 = 8;

impl TrapFrame {
    pub fn user_mode(&self) -> bool {
        self.pstate & 0xF == 0 // EL0t
    }

    /// The system call number (x8).
    #[inline]
    pub fn number(&self) -> u64 {
        self.x[8]
    }

    /// The six arguments (x0-x5).
    #[inline]
    pub fn args(&self) -> [u64; 6] {
        [self.x[0], self.x[1], self.x[2], self.x[3], self.x[4], self.x[5]]
    }

    /// Sets the value the call returns (x0).
    #[inline]
    pub fn set_return(&mut self, v: u64) {
        self.x[0] = v;
    }

    /// The value the call returns (x0, once it has run).
    #[inline]
    pub fn ret(&self) -> u64 {
        self.x[0]
    }

    /// Where execution resumes.
    #[inline]
    pub fn pc(&self) -> u64 {
        self.pc
    }

    /// The interrupted stack pointer.
    #[inline]
    pub fn sp(&self) -> u64 {
        self.sp
    }

    #[inline]
    pub fn set_sp(&mut self, sp: u64) {
        self.sp = sp;
    }

    /// A frame that starts a program at `pc` with stack `sp` (EL0t,
    /// interrupts unmasked).
    pub fn user_start(pc: u64, sp: u64) -> TrapFrame {
        TrapFrame { pc, sp, ..Default::default() }
    }
}

macro_rules! vectors {
    ($($kind:literal)*) => {
        global_asm!(
            ".section .text.vectors, \"ax\"",
            ".balign 2048",
            ".global aurora_vectors",
            "aurora_vectors:",
            $(
                ".balign 128",
                "sub sp, sp, #{frame}",
                "stp x0, x1, [sp, #0]",
                concat!("mov x0, #", $kind),
                "b aurora_trap_common",
            )*
            ".text",
            "aurora_trap_common:",
            "stp x2, x3, [sp, #16]",
            "stp x4, x5, [sp, #32]",
            "stp x6, x7, [sp, #48]",
            "stp x8, x9, [sp, #64]",
            "stp x10, x11, [sp, #80]",
            "stp x12, x13, [sp, #96]",
            "stp x14, x15, [sp, #112]",
            "stp x16, x17, [sp, #128]",
            "stp x18, x19, [sp, #144]",
            "stp x20, x21, [sp, #160]",
            "stp x22, x23, [sp, #176]",
            "stp x24, x25, [sp, #192]",
            "stp x26, x27, [sp, #208]",
            "stp x28, x29, [sp, #224]",
            "str x30, [sp, #240]",
            // The interrupted SP: the user's, or the kernel's before the frame.
            "cmp x0, #4",
            "b.lo 1f",
            "mrs x1, sp_el0",
            "b 2f",
            "1:",
            "add x1, sp, #{frame}",
            "2:",
            "mrs x2, elr_el1",
            "stp x1, x2, [sp, #248]",
            "mrs x3, spsr_el1",
            "mrs x1, esr_el1",
            "stp x3, x1, [sp, #264]",
            "mrs x2, far_el1",
            "stp x2, x0, [sp, #280]",
            // A frame record, so backtraces walk from the handler into the
            // interrupted code.
            "add x29, sp, #224",
            "mov x0, sp",
            "bl {trap}",
            "ldp x1, x2, [sp, #248]",
            "ldr x3, [sp, #264]",
            // Back to EL0 (by the saved PSTATE, which a handler may have
            // rewritten, as `execve` does): the user stack pointer too.
            "tst x3, #0xf",
            "b.ne 3f",
            "msr sp_el0, x1",
            "3:",
            "msr elr_el1, x2",
            "msr spsr_el1, x3",
            "ldp x2, x3, [sp, #16]",
            "ldp x4, x5, [sp, #32]",
            "ldp x6, x7, [sp, #48]",
            "ldp x8, x9, [sp, #64]",
            "ldp x10, x11, [sp, #80]",
            "ldp x12, x13, [sp, #96]",
            "ldp x14, x15, [sp, #112]",
            "ldp x16, x17, [sp, #128]",
            "ldp x18, x19, [sp, #144]",
            "ldp x20, x21, [sp, #160]",
            "ldp x22, x23, [sp, #176]",
            "ldp x24, x25, [sp, #192]",
            "ldp x26, x27, [sp, #208]",
            "ldp x28, x29, [sp, #224]",
            "ldr x30, [sp, #240]",
            "ldp x0, x1, [sp, #0]",
            "add sp, sp, #{frame}",
            "eret",
            frame = const FRAME,
            trap = sym trap,
        );
    };
}

// EL1 with SP_EL0 (never used), EL1 with SP_EL1 (the kernel), EL0 AArch64
// (user programs), EL0 AArch32 (not supported): sync, IRQ, FIQ, SError each.
vectors!(8 8 8 8 0 1 2 3 4 5 6 7 8 8 8 8);

extern "C" {
    static aurora_vectors: u8;
}

/// Points VBAR_EL1 at the vector table (every CPU).
pub fn init() {
    // SAFETY: the table is 2 KiB aligned, in the kernel image, and handles
    // every entry.
    unsafe {
        let base = &aurora_vectors as *const u8 as u64;
        super::regs::write!("vbar_el1", base);
    }
    super::regs::isb();
}

/// Exception classes (ESR_EL1.EC).
const EC_UNKNOWN: u64 = 0x00;
const EC_FP: u64 = 0x07;
const EC_SVC64: u64 = 0x15;
const EC_SYSREG: u64 = 0x18;
const EC_SVE: u64 = 0x19;
const EC_IABT_LOWER: u64 = 0x20;
const EC_IABT_SAME: u64 = 0x21;
const EC_PC_ALIGN: u64 = 0x22;
const EC_DABT_LOWER: u64 = 0x24;
const EC_DABT_SAME: u64 = 0x25;
const EC_SP_ALIGN: u64 = 0x26;
const EC_FP64: u64 = 0x2C;
const EC_BRK64: u64 = 0x3C;

/// Signal numbers and `si_code` values for faults (Linux's).
const SIGILL: u32 = 4;
const SIGTRAP: u32 = 5;
const SIGBUS: u32 = 7;
const SIGFPE: u32 = 8;
const SIGSEGV: u32 = 11;
const SEGV_MAPERR: i32 = 1;
const SEGV_ACCERR: i32 = 2;
const ILL_ILLOPC: i32 = 1;
const FPE_FLTINV: i32 = 7;
const BUS_ADRALN: i32 = 1;
const TRAP_BRKPT: i32 = 1;

fn ec_name(ec: u64) -> &'static str {
    match ec {
        EC_UNKNOWN => "Undefined Instruction",
        EC_FP | EC_SVE => "FP/SIMD Access Trap",
        EC_SVC64 => "System Call",
        EC_SYSREG => "System Register Access",
        EC_IABT_LOWER | EC_IABT_SAME => "Instruction Abort",
        EC_PC_ALIGN => "PC Alignment Fault",
        EC_DABT_LOWER | EC_DABT_SAME => "Data Abort",
        EC_SP_ALIGN => "SP Alignment Fault",
        EC_FP64 => "Floating-point Exception",
        EC_BRK64 => "Breakpoint",
        _ => "Exception",
    }
}

/// Return addresses up the frame-pointer chain of the interrupted code.
struct Callers(u64);

impl core::fmt::Display for Callers {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let mut fp = self.0;
        for _ in 0..32 {
            if fp < bootinfo::PHYS_OFFSET || !fp.is_multiple_of(8) {
                break;
            }
            // SAFETY: a kernel stack address (checked above), 8-aligned.
            let (next, ret) = unsafe { (*(fp as *const u64), *((fp + 8) as *const u64)) };
            write!(f, " {ret:#x}")?;
            if next <= fp {
                break;
            }
            fp = next;
        }
        Ok(())
    }
}

fn fatal(name: &str, f: &TrapFrame, detail: core::fmt::Arguments) -> ! {
    if f.user_mode() {
        crate::proc::crash_current(format_args!(
            "{} at {:#x}{}{}",
            name,
            f.pc,
            if detail.as_str() == Some("") { "" } else { " — " },
            detail
        ));
    }
    panic!(
        "CPU exception: {}\n{}\nPC={:#x} SP={:#x} PSTATE={:#x} ESR={:#x} FAR={:#x}\nLR={:#x} FP={:#x}\ncalled from:{}",
        name,
        detail,
        f.pc,
        f.sp,
        f.pstate,
        f.esr,
        f.far,
        f.x[30],
        f.x[29],
        Callers(f.x[29])
    );
}

/// Every exception comes here (from the vectors above).
extern "C" fn trap(f: &mut TrapFrame) {
    match f.kind {
        KIND_SYNC | KIND_USER => sync(f),
        k if k == KIND_IRQ || k == KIND_USER + KIND_IRQ => super::irq::interrupt(),
        k if k == KIND_FIQ || k == KIND_USER + KIND_FIQ => fatal("FIQ", f, format_args!("")),
        k if k == KIND_SERROR || k == KIND_USER + KIND_SERROR => fatal("SError", f, format_args!("")),
        _ => fatal("Unexpected Exception Vector", f, format_args!("")),
    }
    if f.user_mode() {
        // As on the way out of a system call, with interrupts on: writing a
        // signal frame may have to make a stack page.
        super::interrupts::enable();
        crate::proc::check_killed();
        crate::compat::linux::signal::on_trap_return(f);
        super::interrupts::disable();
    }
}

fn sync(f: &mut TrapFrame) {
    let ec = f.esr >> 26;
    match ec {
        EC_SVC64 if f.user_mode() => {
            // `svc` leaves ELR past itself already. Interrupts on during the
            // call, as on x86 after `syscall`.
            super::interrupts::enable();
            crate::syscall::dispatch(f);
            super::interrupts::disable();
        }
        EC_DABT_LOWER | EC_DABT_SAME | EC_IABT_LOWER | EC_IABT_SAME => page_fault(f, ec),
        EC_BRK64 if !f.user_mode() => {
            log!("int", "breakpoint at {:#x}", f.pc);
            f.pc += 4;
        }
        // A Linux program reading an ID register (`mrs`): emulated as Linux does.
        EC_SYSREG | EC_UNKNOWN
            if f.user_mode()
                && crate::proc::current().is_some_and(|p| p.linux.is_some())
                && crate::compat::linux::abi::aarch64::emulate_mrs(f) => {}
        _ => {
            let (sig, code, addr) = match ec {
                EC_BRK64 => (SIGTRAP, TRAP_BRKPT, f.pc),
                EC_PC_ALIGN | EC_SP_ALIGN => (SIGBUS, BUS_ADRALN, f.far),
                EC_FP64 => (SIGFPE, FPE_FLTINV, f.pc),
                _ => (SIGILL, ILL_ILLOPC, f.pc),
            };
            if !user_fault(f, sig, code, addr) {
                fatal(ec_name(ec), f, format_args!("ESR {:#x}", f.esr));
            }
        }
    }
}

/// A fault in a Linux program with a handler for `sig`: true once the
/// frame enters the handler. Otherwise the caller reports the crash.
fn user_fault(f: &mut TrapFrame, sig: u32, code: i32, addr: u64) -> bool {
    if !f.user_mode() {
        return false;
    }
    let Some(p) = crate::proc::current().filter(|p| p.linux.is_some()) else { return false };
    let info = crate::compat::linux::signal::Info { code, addr, ..Default::default() };
    super::interrupts::enable();
    let entered = crate::compat::linux::signal::force(&p, sig, info, f);
    super::interrupts::disable();
    entered
}

fn page_fault(f: &mut TrapFrame, ec: u64) {
    let addr = f.far;
    let iss = f.esr & 0x1FF_FFFF;
    let fetch = ec == EC_IABT_LOWER || ec == EC_IABT_SAME;
    // WnR (data aborts; cache maintenance reports as a write: CM).
    let write = !fetch && iss & (1 << 6) != 0 && iss & (1 << 8) == 0;
    // Fault status: 0b0001xx translation (nothing mapped), 0b0010xx access
    // flag, 0b0011xx permission.
    let status = iss & 0x3F;
    let protection = status & 0x3C == 0x0C;
    // The access flag and dirtiness are kept in software (`mmu::pte`):
    // set them and go again, at once, whatever the interrupt state.
    // (Not a kernel access to user memory with PAN on: that one is refused.)
    let pan_refused = !f.user_mode() && f.pstate & (1 << 22) != 0 && addr < aurora_abi::layout::USER_END;
    if !pan_refused && (status & 0x3C == 0x08 || (protection && write)) {
        let root = if addr >= aurora_abi::layout::USER_END { super::mmu::kernel_root() } else { super::mmu::root() };
        if super::mmu::fix_soft_fault(root, addr, write) {
            return;
        }
    }
    let fixable = !protection || (write && !fetch);
    let interrupts_on = f.pstate & (1 << 7) == 0;
    if fixable
        && addr < aurora_abi::layout::USER_END
        && interrupts_on
        && (f.user_mode() || crate::syscall::user::fixup(f.pc, addr).is_some())
    {
        super::interrupts::enable();
        let resolved = crate::proc::resolve_fault(addr, write);
        let killed = !resolved && f.user_mode() && crate::proc::current_killed();
        super::interrupts::disable();
        if resolved || killed {
            return;
        }
    }
    if !f.user_mode() {
        if let Some(resume) = crate::syscall::user::fixup(f.pc, addr) {
            f.pc = resume;
            return;
        }
        let walk = crate::mm::vmm::entries_for(addr);
        fatal(
            if fetch { "Instruction Abort" } else { "Data Abort" },
            f,
            format_args!(
                "address {:#x}, {}{}, status {:#x}, page tables {:x?}",
                addr,
                if write {
                    "write"
                } else if fetch {
                    "fetch"
                } else {
                    "read"
                },
                if protection { " (permission)" } else { "" },
                status,
                walk
            ),
        );
    }
    super::interrupts::enable();
    let mapped = protection || crate::proc::is_mapped(addr);
    super::interrupts::disable();
    let si_code = if mapped { SEGV_ACCERR } else { SEGV_MAPERR };
    if !user_fault(f, SIGSEGV, si_code, addr) {
        fatal("Page Fault", f, format_args!("address {:#x}, status {:#x}", addr, status));
    }
}
