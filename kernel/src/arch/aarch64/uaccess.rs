//! The user-copy routine: the only code that touches user memory, and the
//! fault fixup that turns a fault inside it into an error return.
//!
//! A byte loop between two labels: a fault with the PC inside resumes at
//! the routine's fault exit, which returns 1. With PAN on (`protect`) the
//! PAN variant clears PSTATE.PAN around the loop; an exception taken inside
//! sets it again on entry and restores the routine's on return, so the
//! fault exit sets it once more. (`msr pan, #imm` is written as `.inst`:
//! the kernel's target is ARMv8.0.)

use core::arch::global_asm;

global_asm!(
    ".global aurora_user_copy",
    "aurora_user_copy:",
    "cbz x2, 2f",
    "aurora_user_copy_start:",
    "1:",
    "ldrb w3, [x1], #1",
    "strb w3, [x0], #1",
    "subs x2, x2, #1",
    "b.ne 1b",
    "aurora_user_copy_end:",
    "2:",
    "mov w0, #0",
    "ret",
    "aurora_user_copy_fault:",
    "mov w0, #1",
    "ret",
    "",
    ".global aurora_user_copy_pan",
    "aurora_user_copy_pan:",
    ".inst 0xd500409f", // msr pan, #0
    "cbz x2, 2f",
    "aurora_user_copy_pan_start:",
    "1:",
    "ldrb w3, [x1], #1",
    "strb w3, [x0], #1",
    "subs x2, x2, #1",
    "b.ne 1b",
    "aurora_user_copy_pan_end:",
    "2:",
    ".inst 0xd500419f", // msr pan, #1
    "mov w0, #0",
    "ret",
    "aurora_user_copy_pan_fault:",
    ".inst 0xd500419f",
    "mov w0, #1",
    "ret",
);

extern "C" {
    fn aurora_user_copy(dst: *mut u8, src: *const u8, len: usize) -> u32;
    fn aurora_user_copy_pan(dst: *mut u8, src: *const u8, len: usize) -> u32;
    static aurora_user_copy_start: u8;
    static aurora_user_copy_end: u8;
    static aurora_user_copy_fault: u8;
    static aurora_user_copy_pan_start: u8;
    static aurora_user_copy_pan_end: u8;
    static aurora_user_copy_pan_fault: u8;
}

fn addr(s: &u8) -> u64 {
    s as *const u8 as u64
}

/// Copies `len` bytes; returns 0, or 1 if a fault cut it short.
///
/// # Safety
/// The kernel side of the copy must be valid for `len` bytes; the user side
/// must lie below `USER_END` (checked by the caller), and may fault.
pub unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) -> u32 {
    if super::protect::smap() {
        aurora_user_copy_pan(dst, src, len)
    } else {
        aurora_user_copy(dst, src, len)
    }
}

/// Where to resume after a kernel fault at `pc` on user address `addr`, if
/// it happened inside a copy loop (or a self-test probe).
pub fn fixup(pc: u64, fault_addr: u64) -> Option<u64> {
    if fault_addr >= aurora_abi::layout::USER_END {
        return None;
    }
    // SAFETY: only the symbols' addresses are taken.
    let ranges = unsafe {
        [
            (addr(&aurora_user_copy_start), addr(&aurora_user_copy_end), addr(&aurora_user_copy_fault)),
            (addr(&aurora_user_copy_pan_start), addr(&aurora_user_copy_pan_end), addr(&aurora_user_copy_pan_fault)),
        ]
    };
    ranges.iter().find(|r| (r.0..r.1).contains(&pc)).map(|r| r.2).or_else(|| probe::fixup(pc, fault_addr))
}

/// Self-test probes: a kernel read of a user address *without* opening
/// access, and a kernel call into a user page, each recovering from its
/// fault — they show PAN and PXN at work.
pub mod probe {
    #[cfg(feature = "ktest")]
    use core::sync::atomic::{AtomicU64, Ordering};

    #[cfg(feature = "ktest")]
    core::arch::global_asm!(
        "aurora_probe_read:",
        "aurora_probe_read_insn:",
        "ldrb w1, [x0]",
        "mov w0, #0",
        "ret",
        "aurora_probe_read_fault:",
        "mov w0, #1",
        "ret",
        "aurora_probe_exec:",
        "stp x29, x30, [sp, #-16]!",
        "blr x0",
        "mov w0, #0",
        "ldp x29, x30, [sp], #16",
        "ret",
        "aurora_probe_exec_fault:",
        "mov w0, #1",
        "ldp x29, x30, [sp], #16",
        "ret",
    );

    #[cfg(feature = "ktest")]
    extern "C" {
        fn aurora_probe_read(addr: u64) -> u32;
        static aurora_probe_read_insn: u8;
        static aurora_probe_read_fault: u8;
        fn aurora_probe_exec(addr: u64) -> u32;
        static aurora_probe_exec_fault: u8;
    }

    /// The user address [`exec_faults`] is calling (0: none).
    #[cfg(feature = "ktest")]
    static EXEC_TARGET: AtomicU64 = AtomicU64::new(0);

    #[cfg(feature = "ktest")]
    pub fn fixup(pc: u64, addr: u64) -> Option<u64> {
        if pc == unsafe { &aurora_probe_read_insn as *const u8 as u64 } {
            return Some(unsafe { &aurora_probe_read_fault as *const u8 as u64 });
        }
        let target = EXEC_TARGET.load(Ordering::Relaxed);
        (target != 0 && pc == target && addr == target).then_some(unsafe { &aurora_probe_exec_fault as *const u8 as u64 })
    }

    #[cfg(not(feature = "ktest"))]
    pub fn fixup(_pc: u64, _addr: u64) -> Option<u64> {
        None
    }

    /// Reads a byte at `addr` the way a careless kernel would: true if it faulted.
    #[cfg(feature = "ktest")]
    pub fn read_faults(addr: u64) -> bool {
        unsafe { aurora_probe_read(addr) != 0 }
    }

    /// Calls code at user address `addr` from the kernel: true if it faulted.
    /// (The code must be a `ret` if it runs.)
    #[cfg(feature = "ktest")]
    pub fn exec_faults(addr: u64) -> bool {
        EXEC_TARGET.store(addr, Ordering::Relaxed);
        let r = unsafe { aurora_probe_exec(addr) != 0 };
        EXEC_TARGET.store(0, Ordering::Relaxed);
        r
    }
}
