//! arm64 Linux programs: `svc #0` with the number in x8, arguments in
//! x0-x5, the result in x0 (`arch::syscall::SyscallFrame` reads them).
//!
//! The generic call table (`aurora_linuxabi::aarch64::TABLE`) maps each
//! number onto the shared call of the same name; `clone` swaps its last two
//! arguments ([`own`]). Structures that differ (`stat`, `epoll_event`, the
//! signal frame) are this module's; threads' TLS is TPIDR_EL0, which
//! `clone` sets with `CLONE_SETTLS` through `sched`'s thread pointer.
//!
//! Also here: the auxiliary vector's `AT_HWCAP`/`AT_HWCAP2`/
//! `AT_MINSIGSTKSZ`, and the emulation of EL0 reads of the ID registers
//! ([`emulate_mrs`]), both from `aurora_linuxabi::aarch64::cpu`.

pub mod signal;

use super::Call;
use crate::arch::syscall::SyscallFrame;
use crate::arch::trap::TrapFrame;
use crate::proc::Process;
use aurora_linuxabi::aarch64::{self as abi, cpu, TABLE};
use aurora_linuxabi::Map;

pub use aurora_linuxabi::aarch64::{EPOLL_DATA_AT, EPOLL_EVENT_SIZE, MACHINE, MINSIGSTKSZ, O_DIRECTORY, O_NOFOLLOW};

/// What call `n` is.
#[inline]
pub fn lookup(n: u64) -> Call {
    match TABLE.get(n).map(|e| e.map) {
        Some(Map::Same(x)) => Call::Shared(x as u64),
        Some(Map::Own) => Call::Own,
        Some(Map::No) | None => Call::None,
    }
}

/// The layer's number for call `n` (`u64::MAX` for none), for what the
/// shared code decides by number (restarting, tracing).
#[inline]
pub fn canonical(n: u64) -> u64 {
    match lookup(n) {
        Call::Shared(x) => x,
        _ if n == abi::table::CLONE => super::nr::CLONE,
        _ => u64::MAX,
    }
}

/// Call `n`'s name, for logs.
pub fn name(n: u64) -> &'static str {
    TABLE.get(n).map_or("", |e| e.name)
}

/// The calls arm64 handles itself.
pub fn own(n: u64, f: &mut SyscallFrame, p: &alloc::sync::Arc<Process>) -> Result<u64, isize> {
    let a = f.args();
    match n {
        abi::table::CLONE => {
            let [flags, stack, ptid, ctid, tls] = abi::clone_args(a);
            super::super::process::clone(p, f, flags, stack, ptid, ctid, tls)
        }
        _ => Err(aurora_abi::err::ENOSYS),
    }
}

/// `struct stat` at `ptr`.
pub fn put_stat(ptr: u64, s: &aurora_linuxabi::Stat) -> Result<u64, isize> {
    crate::syscall::user::put(ptr, &abi::encode_stat(s)).map(|_| 0)
}

/// The ID registers features are decided by, as the boot CPU read them
/// at start-up (`arch::aarch64::cpu::id_regs`).
fn id_regs() -> Option<cpu::IdRegs> {
    Some(crate::arch::aarch64::cpu::id_regs())
}

/// What the kernel does for programs beyond the processor's features.
fn kernel() -> cpu::Kernel {
    cpu::Kernel { pointer_auth: false, cpuid: id_regs().is_some(), event_stream: false }
}

fn hwcaps() -> (u64, u64) {
    match id_regs() {
        Some(id) => cpu::hwcaps(&id, kernel()),
        None => (crate::arch::cpu::linux_hwcap(), 0),
    }
}

/// `AT_HWCAP`.
pub fn hwcap() -> u64 {
    hwcaps().0
}

/// `AT_HWCAP2` and `AT_MINSIGSTKSZ`, after the common entries.
pub fn extra_auxv(_vdso: u64) -> alloc::vec::Vec<(u64, u64)> {
    alloc::vec![(abi::AT_HWCAP2, hwcaps().1), (abi::AT_MINSIGSTKSZ, abi::signal::MIN_STACK)]
}

/// The `Features` line of `/proc/cpuinfo`, from the same bits.
pub fn features() -> alloc::string::String {
    let (h, h2) = hwcaps();
    let mut names = alloc::vec::Vec::new();
    cpu::features(h, h2, &mut |n| names.push(n));
    names.join(" ")
}

/// An EL0 `mrs` of an ID register (a trapped system register access, or
/// an undefined instruction): emulated as Linux does, true if it was one.
/// For `arch::trap` to call before turning the exception into `SIGILL`.
pub fn emulate_mrs(f: &mut TrapFrame) -> bool {
    let decoded = cpu::decode_esr(f.esr).or_else(|| {
        // EC 0: an undefined instruction; it may be an `mrs`.
        (f.esr >> 26 == 0).then(|| crate::syscall::user::get::<u32>(f.pc).ok()).flatten().and_then(cpu::decode_insn)
    });
    let (Some((reg, rt)), Some(id)) = (decoded, id_regs()) else { return false };
    let Some(value) = cpu::emulate(reg, &id, kernel()) else { return false };
    if rt != 31 {
        f.x[rt] = value; // (x31 is the zero register here)
    }
    f.pc += 4;
    true
}
