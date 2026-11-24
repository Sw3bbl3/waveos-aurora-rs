//! Kernel thread context switch.
//!
//! Only callee-saved registers need saving (x19-x28, the frame pointer x29
//! and the link register x30): every switch happens inside an ordinary
//! function call (`context_switch`), so the compiler has already spilled
//! anything caller-saved. The kernel is built soft-float (it never touches
//! v0-v31), so there is no FP/SIMD state to keep here. Switches always run
//! with interrupts disabled.

use core::arch::global_asm;

global_asm!(
    ".global aurora_context_switch",
    "aurora_context_switch:",
    "sub sp, sp, #96",
    "stp x19, x20, [sp, #0]",
    "stp x21, x22, [sp, #16]",
    "stp x23, x24, [sp, #32]",
    "stp x25, x26, [sp, #48]",
    "stp x27, x28, [sp, #64]",
    "stp x29, x30, [sp, #80]",
    "mov x9, sp",
    "str x9, [x0]",
    "mov sp, x1",
    "ldp x19, x20, [sp, #0]",
    "ldp x21, x22, [sp, #16]",
    "ldp x23, x24, [sp, #32]",
    "ldp x25, x26, [sp, #48]",
    "ldp x27, x28, [sp, #64]",
    "ldp x29, x30, [sp, #80]",
    "add sp, sp, #96",
    "ret",
    "",
    ".global aurora_task_trampoline",
    "aurora_task_trampoline:",
    // Interrupts stay off: task_entry first completes the switch, then enables them.
    "mov x0, x19",
    "mov x1, x20",
    "bl {entry}",
    "brk #0",
    entry = sym crate::sched::task_entry,
);

/// Where a switched-out task's saved frame pointer and return address sit,
/// above its saved stack pointer (for stack dumps).
pub const SAVED_FRAME_POINTER: u64 = 80;
pub const SAVED_RETURN: u64 = 88;

extern "C" {
    fn aurora_context_switch(save_sp: *mut u64, load_sp: u64);
    fn aurora_task_trampoline();
}

/// Saves the current context into `*save_sp` and resumes the one at `load_sp`.
///
/// # Safety
/// Must be called with interrupts disabled and `load_sp` pointing at a stack
/// prepared by [`init_stack`] or saved by a previous switch.
pub unsafe fn switch(save_sp: *mut u64, load_sp: u64) {
    aurora_context_switch(save_sp, load_sp);
}

/// Prepares a fresh stack so the first switch into it "returns" into the
/// trampoline, which calls `task_entry(f, arg)`.
pub fn init_stack(stack_top: u64, f: u64, arg: u64) -> u64 {
    let top = stack_top & !0xF;
    let mut frame = [0u64; 12];
    frame[0] = f; // x19 -> x0
    frame[1] = arg; // x20 -> x1
    frame[11] = aurora_task_trampoline as *const () as u64; // x30
    let sp = top - 8 * frame.len() as u64;
    // SAFETY: the caller hands over the stack below `stack_top`.
    unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), sp as *mut u64, frame.len()) };
    sp
}
