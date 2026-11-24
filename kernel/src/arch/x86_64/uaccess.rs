//! The user-copy routine: the only code that touches user memory, and the
//! fault fixup that turns a page fault inside it into an error return
//! (`syscall::user` checks ranges and calls it).
//!
//! With SMAP (`protect`) the routine's SMAP variant alone opens user access
//! (`stac`/`clac`).

use core::arch::global_asm;

global_asm!(
    ".global aurora_user_copy",
    "aurora_user_copy:",
    "mov rcx, rdx",
    ".global aurora_user_copy_insn",
    "aurora_user_copy_insn:",
    "rep movsb",
    "xor eax, eax",
    "ret",
    ".global aurora_user_copy_fault",
    "aurora_user_copy_fault:",
    "mov eax, 1",
    "ret",
    // The same with SMAP on: user pages are reachable only between `stac`
    // and `clac`. A fault returns through the trap, whose `iretq` restores
    // RFLAGS.AC, so the fault exit closes access again too.
    ".global aurora_user_copy_smap",
    "aurora_user_copy_smap:",
    "mov rcx, rdx",
    "stac",
    ".global aurora_user_copy_smap_insn",
    "aurora_user_copy_smap_insn:",
    "rep movsb",
    "clac",
    "xor eax, eax",
    "ret",
    ".global aurora_user_copy_smap_fault",
    "aurora_user_copy_smap_fault:",
    "clac",
    "mov eax, 1",
    "ret",
);

extern "sysv64" {
    fn aurora_user_copy(dst: *mut u8, src: *const u8, len: usize) -> u32;
    static aurora_user_copy_insn: u8;
    static aurora_user_copy_fault: u8;
    fn aurora_user_copy_smap(dst: *mut u8, src: *const u8, len: usize) -> u32;
    static aurora_user_copy_smap_insn: u8;
    static aurora_user_copy_smap_fault: u8;
}

/// Copies `len` bytes; returns 0, or 1 if a page fault cut it short.
///
/// # Safety
/// The kernel side of the copy must be valid for `len` bytes; the user side
/// must lie below `USER_END` (checked by the caller), and may fault.
pub unsafe fn copy(dst: *mut u8, src: *const u8, len: usize) -> u32 {
    if super::protect::smap() {
        aurora_user_copy_smap(dst, src, len)
    } else {
        aurora_user_copy(dst, src, len)
    }
}

/// Called by the page fault handler for a kernel-mode fault at `pc` on
/// user address `addr`: where to resume if the fault happened inside the
/// copy routine (or a self-test probe).
pub fn fixup(pc: u64, addr: u64) -> Option<u64> {
    if addr >= aurora_abi::layout::USER_END {
        return None;
    }
    let (plain, smap) =
        unsafe { (&aurora_user_copy_insn as *const u8 as u64, &aurora_user_copy_smap_insn as *const u8 as u64) };
    if pc == plain {
        Some(unsafe { &aurora_user_copy_fault as *const u8 as u64 })
    } else if pc == smap {
        Some(unsafe { &aurora_user_copy_smap_fault as *const u8 as u64 })
    } else {
        probe::fixup(pc, addr)
    }
}

/// Self-test probes: a kernel read of a user address *without* opening
/// access, and a kernel call into a user page, each recovering from its
/// fault — they show SMAP and SMEP at work (`tests::cpu_protections`).
pub mod probe {
    #[cfg(feature = "ktest")]
    use core::sync::atomic::{AtomicU64, Ordering};

    #[cfg(feature = "ktest")]
    core::arch::global_asm!(
        "aurora_probe_read:",
        "aurora_probe_read_insn:",
        "mov al, byte ptr [rdi]",
        "xor eax, eax",
        "ret",
        "aurora_probe_read_fault:",
        "mov eax, 1",
        "ret",
        "aurora_probe_exec:",
        "call rdi",
        "xor eax, eax",
        "ret",
        // The call pushed its return address before the fetch faulted.
        "aurora_probe_exec_fault:",
        "add rsp, 8",
        "mov eax, 1",
        "ret",
    );

    #[cfg(feature = "ktest")]
    extern "sysv64" {
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
    pub fn fixup(rip: u64, addr: u64) -> Option<u64> {
        if rip == unsafe { &aurora_probe_read_insn as *const u8 as u64 } {
            return Some(unsafe { &aurora_probe_read_fault as *const u8 as u64 });
        }
        let target = EXEC_TARGET.load(Ordering::Relaxed);
        (target != 0 && rip == target && addr == target)
            .then_some(unsafe { &aurora_probe_exec_fault as *const u8 as u64 })
    }

    #[cfg(not(feature = "ktest"))]
    pub fn fixup(_rip: u64, _addr: u64) -> Option<u64> {
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
