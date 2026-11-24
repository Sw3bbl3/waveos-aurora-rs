//! Processor architecture support.
//!
//! Everything that differs between processors lives in `arch/<arch>/`, and
//! the rest of the kernel reaches it only through the modules re-exported
//! here, which every architecture provides under the same names and with
//! the same signatures (docs/developer/porting.md lists them):
//!
//! | Module | What |
//! |---|---|
//! | `cpu` | identification, cycle counter, halt, spin hint, thread pointer, frame pointer |
//! | `interrupts` | enable, disable, `without_interrupts`, wait for an interrupt |
//! | `irq` | device interrupts: allocate a vector, route and mask a GSI, IPIs |
//! | `percpu` | per-CPU data, `cpu_id`, CPU count |
//! | `timer` | each CPU's deadline timer |
//! | `switch`, `fpu` | kernel context switch; user FP/SIMD state |
//! | `syscall`, `trap` | user entry and exit, the system call and trap frames |
//! | `uaccess` | the user-copy routine and its fault fixup |
//! | `mmu` | page-table root, TLB maintenance |
//! | `smp` | starting and stopping the other CPUs |
//! | `protect` | hardening features, kernel image permissions |
//! | `rng` | hardware random numbers |
//! | `cache` | cache maintenance and store ordering for devices |
//! | `io` | port I/O (x86) or its memory-mapped equivalent |
//!
//! Modules that exist on one architecture only (`x86_64::apic`, `gdt`,
//! `idt`, `pat`) are reached through `arch::x86_64::…` by code that is
//! itself x86-only.

#[cfg(target_arch = "x86_64")]
pub mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use self::x86_64::*;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use self::aarch64::*;
