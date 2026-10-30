//! `syscall`/`sysret` entry.
//!
//! On `syscall` the CPU loads RIP from LSTAR, saves the user RIP in RCX and
//! RFLAGS in R11, and masks RFLAGS with FMASK (we clear IF, so no interrupt can
//! arrive before we are on the kernel stack). It does *not* switch stacks: the
//! entry stub `swapgs`es to reach this CPU's `percpu::SyscallArea`, stashes
//! the user RSP there, loads the task's kernel stack (which the scheduler
//! keeps current), and swaps GS straight back — so kernel code never depends
//! on GS.

use super::gdt::{KERNEL_CS, KERNEL_SS};
use core::arch::global_asm;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;
use x86_64::structures::gdt::SegmentSelector;
use x86_64::{PrivilegeLevel, VirtAddr};

/// Register state saved on entry; the dispatcher writes the result into `rax`.
/// It holds every general register (`rcx` and `r11` carry `rip` and
/// `rflags`), so a new Linux thread, a forked child or a signal handler can
/// start from a copy of it. The four words after `rsp` are zero on entry.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct SyscallFrame {
    pub rbx: u64,
    pub rbp: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rax: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub r10: u64,
    pub r8: u64,
    pub r9: u64,
    pub rip: u64,
    pub rflags: u64,
    pub rsp: u64,
    /// Set (to 1) to leave with `iretq`, putting back `rcx` and `r11` from
    /// the next two fields instead of letting `sysret` use them: after
    /// `rt_sigreturn` to a thread interrupted at any instruction.
    pub full: u64,
    pub rcx: u64,
    pub r11: u64,
    pub pad: u64,
}

global_asm!(
    ".global aurora_syscall_entry",
    "aurora_syscall_entry:",
    "swapgs",
    "mov gs:[8], rsp",
    "mov rsp, gs:[0]",
    "push 0",
    "push 0",
    "push 0",
    "push 0",
    "push qword ptr gs:[8]",
    "swapgs",
    "push r11",
    "push rcx",
    "push r9",
    "push r8",
    "push r10",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rax",
    "push r15",
    "push r14",
    "push r13",
    "push r12",
    "push rbp",
    "push rbx",
    "mov rdi, rsp",
    "sti",
    "call {dispatch}",
    "cli",
    "cmp qword ptr [rsp + 128], 0",
    "jne 2f",
    "pop rbx",
    "pop rbp",
    "pop r12",
    "pop r13",
    "pop r14",
    "pop r15",
    "pop rax",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "pop rcx",
    "pop r11",
    "pop rsp",
    "sysretq",
    // Every register back, with `iretq`: the frame's top five words become
    // the interrupt frame (rip, cs, rflags, rsp, ss) once rcx and r11 are
    // loaded from theirs.
    "2:",
    "mov rcx, [rsp + 136]",
    "mov r11, [rsp + 144]",
    "mov rax, [rsp + 120]",
    "mov [rsp + 144], rax",
    "mov rax, [rsp + 112]",
    "mov [rsp + 136], rax",
    "mov rax, [rsp + 104]",
    "mov [rsp + 120], rax",
    "mov qword ptr [rsp + 128], {user_cs}",
    "mov qword ptr [rsp + 152], {user_ss}",
    "pop rbx",
    "pop rbp",
    "pop r12",
    "pop r13",
    "pop r14",
    "pop r15",
    "pop rax",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "add rsp, 16",
    "iretq",
    "",
    // Enters ring 3 for the first time: rdi = entry, rsi = user stack, rdx = argument.
    ".global aurora_enter_user",
    "aurora_enter_user:",
    "cli",
    "push {user_ss}",
    "push rsi",
    "push 0x202",
    "push {user_cs}",
    "push rdi",
    "mov rdi, rdx",
    "xor eax, eax",
    "xor ebx, ebx",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor esi, esi",
    "xor ebp, ebp",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "iretq",
    "",
    // Enters ring 3 with every register from a `SyscallFrame` at rdi.
    ".global aurora_enter_user_frame",
    "aurora_enter_user_frame:",
    "cli",
    "push {user_ss}",
    "push qword ptr [rdi + 120]",
    "push qword ptr [rdi + 112]",
    "push {user_cs}",
    "push qword ptr [rdi + 104]",
    "mov rbx, [rdi]",
    "mov rbp, [rdi + 8]",
    "mov r12, [rdi + 16]",
    "mov r13, [rdi + 24]",
    "mov r14, [rdi + 32]",
    "mov r15, [rdi + 40]",
    "mov rax, [rdi + 48]",
    "mov rsi, [rdi + 64]",
    "mov rdx, [rdi + 72]",
    "mov r10, [rdi + 80]",
    "mov r8, [rdi + 88]",
    "mov r9, [rdi + 96]",
    "xor ecx, ecx",
    "xor r11d, r11d",
    "mov rdi, [rdi + 56]",
    "iretq",
    dispatch = sym crate::syscall::dispatch,
    user_ss = const super::gdt::USER_SS as u64,
    user_cs = const super::gdt::USER_CS as u64,
);

extern "sysv64" {
    fn aurora_syscall_entry();
    fn aurora_enter_user(entry: u64, stack: u64, arg: u64) -> !;
    fn aurora_enter_user_frame(frame: *const SyscallFrame) -> !;
}

/// Enables `syscall` on the calling CPU (every CPU runs this).
pub fn init() {
    unsafe {
        Efer::update(|f| f.insert(EferFlags::SYSTEM_CALL_EXTENSIONS));
        LStar::write(VirtAddr::new(aurora_syscall_entry as *const () as u64));
        // Clear IF, TF, DF, AC on entry.
        SFMask::write(RFlags::INTERRUPT_FLAG | RFlags::TRAP_FLAG | RFlags::DIRECTION_FLAG | RFlags::ALIGNMENT_CHECK);
        Star::write(
            SegmentSelector::new(4, PrivilegeLevel::Ring3), // user CS 0x23
            SegmentSelector::new(3, PrivilegeLevel::Ring3), // user SS 0x1B
            SegmentSelector::new(KERNEL_CS / 8, PrivilegeLevel::Ring0),
            SegmentSelector::new(KERNEL_SS / 8, PrivilegeLevel::Ring0),
        )
        .expect("GDT layout incompatible with sysret");
    }
}

/// Records the kernel stack for the task about to run on this CPU (syscalls and ring-3 interrupts).
pub fn set_kernel_stack(top: u64) {
    super::percpu::this().syscall.kernel_rsp.store(top, core::sync::atomic::Ordering::Relaxed);
    super::gdt::set_kernel_stack(top);
}

/// Drops to ring 3 at `entry` with stack `stack` and `rdi = arg`. Never returns.
pub fn enter_user(entry: u64, stack: u64, arg: u64) -> ! {
    unsafe { aurora_enter_user(entry, stack, arg) }
}

/// User-mode flags a program may set: the arithmetic flags, direction and
/// trap; interrupts are always on and IOPL stays 0.
pub fn user_rflags(rflags: u64) -> u64 {
    (rflags & 0xDD5) | 0x202
}

/// Drops to ring 3 with every register taken from `frame` (a new thread or a
/// forked child). The frame is copied first, so it may live anywhere.
pub fn enter_user_frame(frame: &SyscallFrame) -> ! {
    let mut f = *frame;
    f.rflags = user_rflags(f.rflags);
    unsafe { aurora_enter_user_frame(&f) }
}
