//! The user-copy routine: the only code that touches user memory, and the
//! fault fixup that turns a fault inside it into an error return.
//!
//! A byte loop between two labels: a fault with the PC inside resumes at
//! the routine's fault exit, which returns 1.

use core::arch::global_asm;

global_asm!(
    ".global aurora_user_copy",
    "aurora_user_copy:",
    "cbz x2, 2f",
    ".global aurora_user_copy_start",
    "aurora_user_copy_start:",
    "1:",
    "ldrb w3, [x1], #1",
    "strb w3, [x0], #1",
    "subs x2, x2, #1",
    "b.ne 1b",
    ".global aurora_user_copy_end",
    "aurora_user_copy_end:",
    "2:",
    "mov w0, #0",
    "ret",
    ".global aurora_user_copy_fault",
    "aurora_user_copy_fault:",
    "mov w0, #1",
    "ret",
);

extern "C" {
    fn aurora_user_copy(dst: *mut u8, src: *const u8, len: usize) -> u32;
    static aurora_user_copy_start: u8;
    static aurora_user_copy_end: u8;
    static aurora_user_copy_fault: u8;
}

/// Copies `len` bytes; returns 0, or 1 if a fault cut it short.
///
/// # Safety
/// The kernel side of the copy must be valid for `len` bytes; the user side
/// must lie below `USER_END` (checked by the caller), and may fault.
pub unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) -> u32 {
    aurora_user_copy(dst, src, len)
}

/// Where to resume after a kernel fault at `pc` on user address `addr`, if
/// it happened inside the copy loop.
pub fn fixup(pc: u64, addr: u64) -> Option<u64> {
    if addr >= aurora_abi::layout::USER_END {
        return None;
    }
    // SAFETY: only the symbols' addresses are taken.
    let (start, end, fault) = unsafe {
        (
            &aurora_user_copy_start as *const u8 as u64,
            &aurora_user_copy_end as *const u8 as u64,
            &aurora_user_copy_fault as *const u8 as u64,
        )
    };
    (start..end).contains(&pc).then_some(fault)
}
