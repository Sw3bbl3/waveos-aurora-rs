//! System calls: `svc #0` from EL0 enters through the exception vectors
//! (`trap`), which save every register in the frame below and call
//! `syscall::dispatch`; `eret` goes back.
//!
//! Convention (docs/developer/porting.md): the number in x8, arguments in
//! x0-x5, the result in x0 (a negative error number on failure); every
//! other register is preserved. Linux uses the same registers on arm64.

pub use super::trap::TrapFrame as SyscallFrame;
use core::arch::global_asm;

global_asm!(
    // Enters EL0 for the first time: x0 = entry, x1 = user stack, x2 = argument.
    ".global aurora_enter_user",
    "aurora_enter_user:",
    "msr daifset, #0xf",
    "msr elr_el1, x0",
    "msr sp_el0, x1",
    "msr spsr_el1, xzr", // EL0t, interrupts unmasked
    "mov x0, x2",
    "mov x1, xzr",
    "mov x2, xzr",
    "mov x3, xzr",
    "mov x4, xzr",
    "mov x5, xzr",
    "mov x6, xzr",
    "mov x7, xzr",
    "mov x8, xzr",
    "mov x9, xzr",
    "mov x10, xzr",
    "mov x11, xzr",
    "mov x12, xzr",
    "mov x13, xzr",
    "mov x14, xzr",
    "mov x15, xzr",
    "mov x16, xzr",
    "mov x17, xzr",
    "mov x18, xzr",
    "mov x19, xzr",
    "mov x20, xzr",
    "mov x21, xzr",
    "mov x22, xzr",
    "mov x23, xzr",
    "mov x24, xzr",
    "mov x25, xzr",
    "mov x26, xzr",
    "mov x27, xzr",
    "mov x28, xzr",
    "mov x29, xzr",
    "mov x30, xzr",
    "eret",
    "",
    // Enters EL0 with every register from the frame at x0 (whose pstate
    // the caller made safe).
    ".global aurora_enter_user_frame",
    "aurora_enter_user_frame:",
    "msr daifset, #0xf",
    "ldp x1, x2, [x0, #248]",
    "ldr x3, [x0, #264]",
    "msr sp_el0, x1",
    "msr elr_el1, x2",
    "msr spsr_el1, x3",
    "ldp x2, x3, [x0, #16]",
    "ldp x4, x5, [x0, #32]",
    "ldp x6, x7, [x0, #48]",
    "ldp x8, x9, [x0, #64]",
    "ldp x10, x11, [x0, #80]",
    "ldp x12, x13, [x0, #96]",
    "ldp x14, x15, [x0, #112]",
    "ldp x16, x17, [x0, #128]",
    "ldp x18, x19, [x0, #144]",
    "ldp x20, x21, [x0, #160]",
    "ldp x22, x23, [x0, #176]",
    "ldp x24, x25, [x0, #192]",
    "ldp x26, x27, [x0, #208]",
    "ldp x28, x29, [x0, #224]",
    "ldr x30, [x0, #240]",
    "ldp x0, x1, [x0, #0]",
    "eret",
);

extern "C" {
    fn aurora_enter_user(entry: u64, stack: u64, arg: u64) -> !;
    fn aurora_enter_user_frame(frame: *const SyscallFrame) -> !;
}

/// Nothing to set up: `svc` goes through the exception vectors.
pub fn init() {}

/// The kernel stack for exceptions from EL0 is SP_EL1, which is the
/// running task's own stack whenever it is in user mode: nothing to record.
pub fn set_kernel_stack(_top: u64) {}

/// Drops to EL0 at `entry` with stack `stack` and `x0 = arg`. Never returns.
pub fn enter_user(entry: u64, stack: u64, arg: u64) -> ! {
    // SAFETY: the caller's address space maps `entry` and `stack` for the
    // program; the kernel stack stays as it is (SP_EL1) for its traps.
    unsafe { aurora_enter_user(entry, stack, arg) }
}

/// PSTATE bits a program may set: the condition flags (NZCV); always EL0t
/// with interrupts unmasked.
pub fn user_pstate(pstate: u64) -> u64 {
    pstate & 0xF000_0000
}

/// Drops to EL0 with every register taken from `frame` (a new thread or a
/// forked child). The frame is copied first, so it may live anywhere.
pub fn enter_user_frame(frame: &SyscallFrame) -> ! {
    let mut f = *frame;
    f.pstate = user_pstate(f.pstate);
    // SAFETY: as `enter_user`; the PSTATE is EL0t.
    unsafe { aurora_enter_user_frame(&f) }
}
