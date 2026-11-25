//! CPU identification, the cycle counter and small CPU operations.

use alloc::string::String;
use alloc::vec::Vec;
pub use core::arch::x86_64::CpuidResult;

/// CPUID leaf `leaf` (subleaf 0). x86-only: arch-neutral code asks the
/// questions below instead.
#[inline]
pub fn cpuid(leaf: u32) -> CpuidResult {
    core::arch::x86_64::__cpuid(leaf)
}

/// CPUID leaf `leaf`, subleaf `sub`.
#[inline]
pub fn cpuid_count(leaf: u32, sub: u32) -> CpuidResult {
    core::arch::x86_64::__cpuid_count(leaf, sub)
}

/// The processor brand string, e.g. "QEMU Virtual CPU version 2.5+".
pub fn brand() -> String {
    let max_ext = cpuid(0x8000_0000).eax;
    if max_ext < 0x8000_0004 {
        return vendor();
    }
    let mut bytes = [0u8; 48];
    for (i, leaf) in (0x8000_0002u32..=0x8000_0004).enumerate() {
        let r = cpuid(leaf);
        for (j, reg) in [r.eax, r.ebx, r.ecx, r.edx].iter().enumerate() {
            bytes[i * 16 + j * 4..i * 16 + j * 4 + 4].copy_from_slice(&reg.to_le_bytes());
        }
    }
    let s = core::str::from_utf8(&bytes).unwrap_or("").trim_matches(char::from(0)).trim();
    String::from(s)
}

pub fn vendor() -> String {
    let r = cpuid(0);
    let mut b = [0u8; 12];
    b[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    b[4..8].copy_from_slice(&r.edx.to_le_bytes());
    b[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    String::from(core::str::from_utf8(&b).unwrap_or("unknown"))
}

/// Family, model and stepping, for `cpuinfo`.
pub fn model() -> String {
    let eax = cpuid(1).eax;
    let family = (eax >> 8) & 0xF;
    let model = (eax >> 4) & 0xF | ((eax >> 16) & 0xF) << 4;
    alloc::format!("family {:#x}, model {:#x}, stepping {}", family, model, eax & 0xF)
}

/// Notable features this CPU has, for `cpuinfo`.
pub fn feature_names() -> Vec<&'static str> {
    let leaf1 = cpuid(1);
    let leaf7 = cpuid(7);
    let ext = cpuid(0x8000_0007);
    [
        (leaf1.ecx & (1 << 20) != 0, "sse4.2"),
        (leaf1.ecx & (1 << 28) != 0, "avx"),
        (leaf7.ebx & (1 << 5) != 0, "avx2"),
        (leaf1.ecx & (1 << 25) != 0, "aes"),
        (leaf1.ecx & (1 << 30) != 0, "rdrand"),
        (leaf1.ecx & (1 << 21) != 0, "x2apic"),
        (leaf1.ecx & (1 << 24) != 0, "tsc-deadline"),
        (ext.edx & (1 << 8) != 0, "invariant-tsc"),
        (leaf7.ebx & (1 << 7) != 0, "smep"),
        (leaf7.ebx & (1 << 20) != 0, "smap"),
    ]
    .iter()
    .filter(|f| f.0)
    .map(|f| f.1)
    .collect()
}

/// XCR0, the register state enabled for XSAVE (0 without OSXSAVE).
pub fn xcr0() -> u64 {
    if cpuid(1).ecx & (1 << 27) == 0 {
        return 0;
    }
    let (lo, hi): (u32, u32);
    // SAFETY: OSXSAVE is set, so XGETBV is allowed.
    unsafe { core::arch::asm!("xgetbv", in("ecx") 0u32, out("eax") lo, out("edx") hi, options(nomem, nostack)) };
    (hi as u64) << 32 | lo as u64
}

/// What Linux programs get as AT_HWCAP (CPUID.01H:EDX, as Linux gives).
pub fn linux_hwcap() -> u64 {
    cpuid(1).edx as u64
}

/// Running under a hypervisor (CPUID.01H:ECX[31]).
pub fn hypervisor() -> bool {
    cpuid(1).ecx & (1 << 31) != 0
}

/// The cycle counter runs at a constant rate in every state (invariant TSC).
pub fn invariant_counter() -> bool {
    cpuid(0x8000_0000).eax >= 0x8000_0007 && cpuid(0x8000_0007).edx & (1 << 8) != 0
}

/// The cycle counter (the TSC). Not ordered against earlier loads.
#[inline]
pub fn cycles() -> u64 {
    // SAFETY: `rdtsc` has no side effects; CR4.TSD is never set.
    unsafe { core::arch::x86_64::_rdtsc() }
}

/// The cycle counter, read only once earlier loads are done (`lfence; rdtsc`).
#[cfg(feature = "ktest")] // the vDSO clock test reads it as the vDSO does
#[inline]
pub fn cycles_ordered() -> u64 {
    // SAFETY: as `cycles`; `lfence` only orders.
    unsafe {
        core::arch::x86_64::_mm_lfence();
        core::arch::x86_64::_rdtsc()
    }
}

/// Waits for the next interrupt (`hlt`). With interrupts off, forever.
#[inline]
pub fn halt() {
    x86_64::instructions::hlt();
}

pub fn halt_forever() -> ! {
    x86_64::instructions::interrupts::disable();
    loop {
        x86_64::instructions::hlt();
    }
}

/// The caller's frame pointer (RBP): the kernel keeps frame pointers, and a
/// frame record is (previous frame pointer, return address).
#[inline(always)]
pub fn frame_pointer() -> u64 {
    let rbp: u64;
    // SAFETY: reads a register.
    unsafe { core::arch::asm!("mov {}, rbp", out(reg) rbp, options(nomem, nostack, preserves_flags)) };
    rbp
}

/// The thread pointer as the running program left it, where programs set
/// it themselves: never on x86 (FS base changes only through `arch_prctl`).
#[inline(always)]
pub fn user_thread_pointer() -> Option<u64> {
    None
}

/// Sets the user thread pointer (FS base) of the calling CPU.
pub fn set_thread_pointer(base: u64) {
    // A non-canonical base would fault the wrmsr itself.
    if base < 0x8000_0000_0000 {
        // SAFETY: IA32_FS_BASE only affects FS-relative accesses, which the
        // kernel never makes; the value is canonical.
        unsafe { x86_64::registers::model_specific::Msr::new(0xC000_0100).write(base) };
    }
}

/// A VMware backdoor call (`in` from `port` with `magic` in EAX, the
/// command in ECX, its argument in EBX); returns (eax, ebx, ecx, edx).
///
/// The hypervisor writes guest registers back on this `in` (eax..edx, and
/// esi/edi for some commands). `rbx` is reserved by LLVM, so it is saved in
/// `r8` — a register the backdoor never touches — and everything the backdoor
/// may modify is declared clobbered.
pub fn vmware_backdoor(magic: u32, port: u16, cmd: u32, arg: u32) -> (u32, u32, u32, u32) {
    let (a, c, d): (u32, u32, u32);
    let mut b: u64 = arg as u64;
    // SAFETY: without a VMware-compatible hypervisor the `in` reads an
    // unclaimed port (all ones); registers are declared as above.
    unsafe {
        core::arch::asm!(
            "xchg rbx, r8",
            "in eax, dx",
            "xchg rbx, r8",
            inout("r8") b,
            inout("eax") magic => a,
            inout("ecx") cmd => c,
            inout("edx") port as u32 => d,
            lateout("rsi") _,
            lateout("rdi") _,
            options(nostack)
        );
    }
    (a, b as u32, c, d)
}

/// MONITOR/MWAIT usable with interrupts as break events.
pub fn mwait_supported() -> bool {
    cpuid(1).ecx & (1 << 3) != 0 && cpuid(0).eax >= 5 && cpuid(5).ecx & 0b11 == 0b11
}

/// MWAIT sub-state counts per C-state (CPUID leaf 5 EDX).
pub fn mwait_substates() -> u32 {
    cpuid(5).edx
}

/// Arms the monitor on the cache line of `line`, then, unless `cancel()`
/// is true by then, enables interrupts and waits in `mwait` with C-state
/// `hint` until a write to the line or an interrupt. Interrupts must be off
/// on entry; they are on when this returns.
pub fn monitor_mwait(line: *const u8, hint: u32, cancel: impl Fn() -> bool) {
    // SAFETY: `monitor` only arms address monitoring (rax = address, ecx =
    // extensions, edx = hints); the caller checked MONITOR/MWAIT support.
    unsafe { core::arch::asm!("monitor", in("rax") line, in("ecx") 0, in("edx") 0, options(nostack)) };
    if cancel() {
        crate::arch::interrupts::enable();
    } else {
        // `sti` delays interrupts by one instruction, so none slips
        // in between it and `mwait` (ECX bit 0: interrupts wake it).
        unsafe { core::arch::asm!("sti; mwait", in("eax") hint, in("ecx") 1, options(nostack)) };
    }
}
