//! Device interrupts: handler slots ("vectors") for drivers, wired to the
//! GIC (`gic`).
//!
//! The interface is the x86 one: [`alloc`] hands out a small number that
//! names a handler slot; [`route_gsi`] connects a wired interrupt (on Arm a
//! GIC SPI: GSI = INTID) to it, [`msi_message`] says what a device writes to
//! raise it (a GICv2m doorbell and an SPI). Without a GIC to drive no slots
//! are handed out: drivers fall back to polling, as they do on x86 when a
//! device has no interrupt.

use super::gic;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

/// The numbers handed out start here, as on x86 (where they are IDT vectors).
pub const FIRST: u8 = 0x50;
pub const COUNT: usize = 64;

static HANDLER: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static ARG: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static HITS: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
static NAMES: Mutex<Vec<(u8, String)>> = Mutex::new(Vec::new());
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Reserves a slot that runs `handler(arg)`; `None` when all are taken,
/// or while no interrupt controller can deliver it (the driver then polls).
pub fn alloc(name: &str, handler: fn(usize), arg: usize) -> Option<u8> {
    if !gic::ready() {
        return None;
    }
    let i = NEXT.fetch_add(1, Ordering::Relaxed);
    if i >= COUNT {
        return None;
    }
    ARG[i].store(arg, Ordering::Relaxed);
    HANDLER[i].store(handler as usize, Ordering::Release);
    let vector = FIRST + i as u8;
    NAMES.lock().push((vector, String::from(name)));
    Some(vector)
}

/// Total interrupts delivered through allocated slots.
pub fn total() -> u64 {
    HITS.iter().map(|h| h.load(Ordering::Relaxed)).sum()
}

/// (slot, owner, interrupts so far) for every allocated slot.
pub fn list() -> Vec<(u8, String, u64)> {
    NAMES.lock().iter().map(|(v, n)| (*v, n.clone(), HITS[(*v - FIRST) as usize].load(Ordering::Relaxed))).collect()
}

/// Runs the handler of slot `vector` (from the GIC's interrupt handler).
pub fn dispatch(vector: u8) {
    let i = vector.wrapping_sub(FIRST) as usize;
    if i >= COUNT {
        return;
    }
    HITS[i].fetch_add(1, Ordering::Relaxed);
    let h = HANDLER[i].load(Ordering::Acquire);
    if h != 0 {
        // SAFETY: only `alloc` stores here, and it stores a `fn(usize)`.
        let f: fn(usize) = unsafe { core::mem::transmute(h) };
        f(ARG[i].load(Ordering::Relaxed));
    }
}

/// An IRQ exception (from `trap`).
pub fn interrupt() {
    gic::handle();
}

/// Routes wired interrupt `gsi` (an SPI) to slot `vector`. GIC SPIs are
/// active-high or rising-edge; firmware describes them so.
pub fn route_gsi(gsi: u32, vector: u8, level: bool, _active_low: bool) -> bool {
    gic::route(gsi, vector, !level)
}

/// Masks a wired interrupt.
pub fn mask_gsi(gsi: u32) {
    gic::mask(gsi)
}

/// Whether devices can raise MSIs (a GICv2m frame).
pub fn msi_available() -> bool {
    gic::msi_available()
}

/// The MSI message (address, data) that raises `vector`.
pub fn msi_message(vector: u8) -> (u64, u32) {
    gic::msi(vector).unwrap_or((0, 0))
}

/// Inter-processor interrupts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ipi {
    /// Look at the run queue (`sched::on_reschedule_ipi`).
    Reschedule,
    /// Service a TLB shootdown (`vmm::service_shootdown`).
    TlbShootdown,
}

/// Sends `ipi` to CPU `cpu` (an SGI).
pub fn send_ipi(cpu: usize, ipi: Ipi) {
    if !gic::ready() {
        return;
    }
    let sgi = match ipi {
        Ipi::Reschedule => gic::SGI_RESCHEDULE,
        Ipi::TlbShootdown => gic::SGI_TLB,
    };
    gic::send_sgi(super::percpu::CPUS[cpu].lapic_id.load(Ordering::Relaxed) as u64, sgi);
}
