//! Entry for the exceptions a program can cause, and for the timer and
//! reschedule interrupts: every general register is saved in a [`TrapFrame`]
//! that the handler may change, so a fault in a Linux program can become a
//! signal handled by the program, and a signal can interrupt a thread busy
//! in user code (at the next tick or reschedule IPI).
//!
//! Each vector has a stub that pushes an error code (0 where the CPU pushes
//! none) and the vector number, then jumps to the common entry, which saves
//! the registers and calls [`super::idt::trap`]. The CPU aligns the stack to
//! 16 bytes before pushing its own frame, so the 22 words pushed in all
//! leave it aligned for the call.

use core::arch::global_asm;

/// Registers at the time of the trap, lowest address first.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct TrapFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl TrapFrame {
    pub fn user_mode(&self) -> bool {
        self.cs & 3 == 3
    }
}

macro_rules! stubs {
    ($($name:literal $vector:literal $err:literal),* $(,)?) => {
        global_asm!(
            $(
                concat!(".global ", $name),
                concat!($name, ":"),
                concat!(".if ", $err, " == 0"),
                "push 0",
                ".endif",
                concat!("push ", $vector),
                "jmp aurora_trap_common",
            )*
            "aurora_trap_common:",
            "push rax",
            "push rbx",
            "push rcx",
            "push rdx",
            "push rsi",
            "push rdi",
            "push rbp",
            "push r8",
            "push r9",
            "push r10",
            "push r11",
            "push r12",
            "push r13",
            "push r14",
            "push r15",
            "cld",
            "mov rdi, rsp",
            "call {trap}",
            "pop r15",
            "pop r14",
            "pop r13",
            "pop r12",
            "pop r11",
            "pop r10",
            "pop r9",
            "pop r8",
            "pop rbp",
            "pop rdi",
            "pop rsi",
            "pop rdx",
            "pop rcx",
            "pop rbx",
            "pop rax",
            "add rsp, 16",
            "iretq",
            trap = sym super::idt::trap,
        );
    };
}

stubs!(
    "aurora_trap_de" 0 0,
    "aurora_trap_bp" 3 0,
    "aurora_trap_of" 4 0,
    "aurora_trap_br" 5 0,
    "aurora_trap_ud" 6 0,
    "aurora_trap_gp" 13 1,
    "aurora_trap_pf" 14 1,
    "aurora_trap_mf" 16 0,
    "aurora_trap_ac" 17 1,
    "aurora_trap_xm" 19 0,
    "aurora_trap_timer" 32 0,
    "aurora_trap_resched" 0xF0 0,
);

extern "C" {
    fn aurora_trap_de();
    fn aurora_trap_bp();
    fn aurora_trap_of();
    fn aurora_trap_br();
    fn aurora_trap_ud();
    fn aurora_trap_gp();
    fn aurora_trap_pf();
    fn aurora_trap_mf();
    fn aurora_trap_ac();
    fn aurora_trap_xm();
    fn aurora_trap_timer();
    fn aurora_trap_resched();
}

/// The stub for each vector that has one.
pub fn stub(vector: u8) -> u64 {
    let f: unsafe extern "C" fn() = match vector {
        0 => aurora_trap_de,
        3 => aurora_trap_bp,
        4 => aurora_trap_of,
        5 => aurora_trap_br,
        6 => aurora_trap_ud,
        13 => aurora_trap_gp,
        14 => aurora_trap_pf,
        16 => aurora_trap_mf,
        17 => aurora_trap_ac,
        19 => aurora_trap_xm,
        32 => aurora_trap_timer,
        0xF0 => aurora_trap_resched,
        _ => unreachable!("no trap stub for vector {vector}"),
    };
    f as usize as u64
}
