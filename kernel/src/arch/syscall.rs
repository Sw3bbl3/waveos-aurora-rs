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
#[repr(C)]
pub struct SyscallFrame {
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
}

global_asm!(
    ".global aurora_syscall_entry",
    "aurora_syscall_entry:",
    "swapgs",
    "mov gs:[8], rsp",
    "mov rsp, gs:[0]",
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
    "mov rdi, rsp",
    "sti",
    "call {dispatch}",
    "cli",
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
    dispatch = sym crate::syscall::dispatch,
    user_ss = const super::gdt::USER_SS as u64,
    user_cs = const super::gdt::USER_CS as u64,
);

extern "sysv64" {
    fn aurora_syscall_entry();
    fn aurora_enter_user(entry: u64, stack: u64, arg: u64) -> !;
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
