//! The first instructions of a program: everything that depends on the
//! state the kernel starts a program in lives here (see
//! docs/developer/porting.md, "User entry", and `aurora_abi`).
//!
//! - x86-64: the kernel enters `_start` with the argument block in `rdi`;
//!   [`crate::entry!`] defines `_start` itself, as a Rust function.
//! - aarch64: the kernel enters `_start` at EL0 with the argument block in
//!   `x0`, the stack in SP_EL0 and every other register zero. `_start`
//!   below makes no assumption it doesn't have to: it aligns the stack to
//!   16 bytes (AAPCS64; an EL0 access through a misaligned SP faults when
//!   the kernel enables SP alignment checking), ends the frame-pointer
//!   chain (x29 and x30 zero) for stack traces, and calls the program's
//!   `__aurora_start` (defined by [`crate::entry!`]) with `x0` untouched.
//!   Native programs use no thread pointer, so TPIDR_EL0 is left alone.

#[cfg(target_arch = "aarch64")]
core::arch::global_asm!(
    ".pushsection .text._start, \"ax\", %progbits",
    ".global _start",
    ".type _start, %function",
    "_start:",
    "mov x9, sp",
    "and x9, x9, #~15",
    "mov sp, x9",
    "mov x29, xzr",
    "mov x30, xzr",
    "bl __aurora_start",
    // `__aurora_start` never returns; stop here if it ever did.
    "brk #0",
    ".size _start, . - _start",
    ".popsection",
);
