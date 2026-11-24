//! Starting and stopping the other CPUs.
//!
//! Each CPU the MADT lists is started with PSCI `CPU_ON` at
//! [`aurora_ap_entry`]'s physical address, with the MMU off, at the boot
//! CPU's exception level (EL1). It reads a start block (physical, passed
//! as the context argument in x0): the boot CPU's MAIR, TCR and SCTLR, the
//! kernel's root for TTBR1, a small identity map for TTBR0 (1 GiB blocks
//! over the kernel image, so the instructions after the MMU comes on are
//! still mapped), its stack, its index (TPIDR_EL1) and [`ap_main`]'s
//! address. The block and the identity map are cleaned to the point of
//! coherency first: the new CPU reads them with its caches off.

use super::percpu::{self, MAX_CPUS};
use super::regs::read;
use core::arch::global_asm;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

const AP_STACK: usize = 64 * 1024;

/// The CPUs the MADT lists (MPIDRs), the boot CPU among them.
static MPIDRS: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static LISTED: AtomicUsize = AtomicUsize::new(0);
static AP_READY: AtomicBool = AtomicBool::new(false);

global_asm!(
    ".global aurora_ap_entry",
    "aurora_ap_entry:",
    "msr daifset, #0xf",
    "ldp x1, x2, [x0, #0]",
    "ldp x3, x4, [x0, #16]",
    "ldp x5, x6, [x0, #32]",
    "ldp x7, x8, [x0, #48]",
    "msr mair_el1, x1",
    "msr tcr_el1, x2",
    "msr ttbr1_el1, x3",
    "msr ttbr0_el1, x4",
    "msr tpidr_el1, x8",
    "tlbi vmalle1",
    "dsb nsh",
    "isb",
    "msr sctlr_el1, x5",
    "isb",
    "ic iallu",
    "dsb nsh",
    "isb",
    "mov sp, x6",
    "mov x0, x8",
    "mov x29, xzr",
    "mov x30, xzr",
    "br x7",
);

extern "C" {
    fn aurora_ap_entry();
}

/// Records the CPUs from the MADT: (MPIDR, GICR, GICC).
pub fn set_cpus(cpus: &[(u64, u64, u64)]) {
    for (i, c) in cpus.iter().take(MAX_CPUS).enumerate() {
        MPIDRS[i].store(c.0, Ordering::Relaxed);
    }
    LISTED.store(cpus.len().min(MAX_CPUS), Ordering::Relaxed);
}

/// Cleans `[p, p + len)` (kernel virtual) to the point of coherency.
fn clean(p: u64, len: u64) {
    let mut a = p & !63;
    while a < p + len {
        super::cache::flush_line(a as *const u8);
        a += 64;
    }
    super::regs::dsb_sy();
}

/// A zeroed page (physical address).
fn page() -> Option<u64> {
    let f = crate::mm::frame::alloc()?;
    // SAFETY: a frame just allocated, in the physical window.
    unsafe { core::ptr::write_bytes(crate::mm::phys_to_virt(f) as *mut u8, 0, 4096) };
    Some(f)
}

/// An identity map of the 2 GiB around physical address `pa` in 1 GiB
/// blocks (normal memory, executable at EL1): TTBR0 while a CPU turns its
/// MMU on.
fn identity_map(pa: u64) -> Option<u64> {
    let (l0, l1) = (page()?, page()?);
    let gib = pa & !((1 << 30) - 1);
    let t0 = crate::mm::phys_to_virt(l0) as *mut u64;
    let t1 = crate::mm::phys_to_virt(l1) as *mut u64;
    // SAFETY: both tables were just allocated; indices are below 512.
    unsafe {
        *t0.add(((gib >> 39) & 0x1FF) as usize) = l1 | 0b11;
        for g in [gib, gib + (1 << 30)] {
            if (g >> 39) & 0x1FF == (gib >> 39) & 0x1FF {
                use super::mmu::pte;
                *t1.add(((g >> 30) & 0x1FF) as usize) = g | 0b01 | pte::ACCESSED | pte::SH_INNER | pte::NO_EXECUTE;
            }
        }
    }
    clean(crate::mm::phys_to_virt(l0), 4096);
    clean(crate::mm::phys_to_virt(l1), 4096);
    Some(l0)
}

/// Starts every other CPU the MADT lists. Returns how many are up.
pub fn start_aps(_cpu_ids: &[u8], _trampoline: u64) -> usize {
    let listed = LISTED.load(Ordering::Relaxed);
    let me = super::cpu::mpidr();
    if listed <= 1 || !super::gic::ready() {
        return 1;
    }
    let entry = aurora_ap_entry as *const () as u64;
    let Some(entry_pa) = super::mmu::virt_to_phys(entry) else { return 1 };
    let Some(idmap) = identity_map(entry_pa) else { return 1 };
    let Some(block) = page() else { return 1 };
    let bv = crate::mm::phys_to_virt(block) as *mut u64;
    let mut up = 1;
    for slot in MPIDRS.iter().take(listed) {
        let mpidr = slot.load(Ordering::Relaxed);
        if mpidr == me {
            continue;
        }
        let cpu = up;
        let stack = alloc::vec![0u8; AP_STACK].leak();
        let top = (stack.as_ptr() as u64 + AP_STACK as u64) & !0xF;
        let fields = [
            read!("mair_el1"),
            read!("tcr_el1"),
            super::mmu::kernel_root(),
            idmap,
            read!("sctlr_el1"),
            top,
            ap_main as *const () as u64,
            cpu as u64,
        ];
        // SAFETY: the block page is ours; 8 words.
        unsafe { core::ptr::copy_nonoverlapping(fields.as_ptr(), bv, fields.len()) };
        clean(bv as u64, 64);
        AP_READY.store(false, Ordering::Release);
        let r = super::psci::call(super::psci::CPU_ON, mpidr, entry_pa, block) as i64;
        if r != 0 {
            log!("smp", "CPU with MPIDR {:#x} did not start (PSCI error {})", mpidr, r);
            continue;
        }
        let deadline = crate::time::now_ns() + 500_000_000;
        while !AP_READY.load(Ordering::Acquire) && crate::time::now_ns() < deadline {
            core::hint::spin_loop();
        }
        if AP_READY.load(Ordering::Acquire) {
            up += 1;
            percpu::set_count(up);
        } else {
            log!("smp", "CPU with MPIDR {:#x} did not come up", mpidr);
        }
    }
    log!("smp", "{} CPU(s) online", up);
    up
}

/// First Rust code on another CPU, on its own stack, its MMU on (TTBR0 the
/// identity map) and TPIDR_EL1 set.
extern "C" fn ap_main(cpu: u64) -> ! {
    let cpu = cpu as usize;
    // SAFETY: the kernel's root shares the kernel half; nothing runs from
    // the identity map any more.
    unsafe { super::mmu::load_root(crate::mm::vmm::kernel_pml4()) };
    super::trap::init();
    super::fpu::init();
    super::cpu::init_el0();
    super::protect::init();
    percpu::init(cpu);
    super::gic::init_cpu(cpu);
    super::timer::start_local();
    crate::timer::reprogram();
    crate::power::cpu_started(cpu);
    crate::sched::init_ap(cpu);
    log!("smp", "CPU {} up (MPIDR {:#x})", cpu, super::cpu::mpidr());
    AP_READY.store(true, Ordering::Release);
    crate::sched::idle();
}

/// Stops every other CPU (a kernel panic).
pub fn halt_others() {
    if super::gic::ready() && percpu::count() > 1 {
        super::gic::broadcast_sgi(super::gic::SGI_HALT);
    }
}

/// No real-mode trampoline page on Arm.
#[allow(dead_code)]
pub fn trampoline() -> u64 {
    0
}
