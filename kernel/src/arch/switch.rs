//! Kernel thread context switch.
//!
//! Only callee-saved registers need saving: every switch happens inside an
//! ordinary function call (`context_switch`), so the compiler has already
//! spilled anything caller-saved. The kernel is built soft-float, so there is
//! no FPU/SSE state to preserve. Switches always run with interrupts disabled.

use core::arch::global_asm;

global_asm!(
    ".global aurora_context_switch",
    "aurora_context_switch:",
    "push rbp",
    "push rbx",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "mov [rdi], rsp",
    "mov rsp, rsi",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbx",
    "pop rbp",
    "ret",
    "",
    ".global aurora_task_trampoline",
    "aurora_task_trampoline:",
    "sti",
    "mov rdi, r12",
    "call {entry}",
    "ud2",
    entry = sym crate::sched::task_entry,
);

extern "sysv64" {
    fn aurora_context_switch(save_rsp: *mut u64, load_rsp: u64);
    fn aurora_task_trampoline();
}

/// Saves the current context into `*save_rsp` and resumes the one at `load_rsp`.
///
/// # Safety
/// Must be called with interrupts disabled and `load_rsp` pointing at a stack
/// prepared by [`init_stack`] or saved by a previous switch.
pub unsafe fn switch(save_rsp: *mut u64, load_rsp: u64) {
    aurora_context_switch(save_rsp, load_rsp);
}

/// Prepares a fresh stack so the first switch into it "returns" into the
/// trampoline, which enables interrupts and calls `task_entry(arg)`.
pub fn init_stack(stack_top: u64, arg: u64) -> u64 {
    let top = stack_top & !0xF;
    let frame: [u64; 7] = [
        0,   // r15
        0,   // r14
        0,   // r13
        arg, // r12 -> rdi
        0,   // rbx
        0,   // rbp
        aurora_task_trampoline as *const () as u64,
    ];
    let rsp = top - 8 * frame.len() as u64;
    unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), rsp as *mut u64, frame.len()) };
    rsp
}
