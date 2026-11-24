//! Device interrupts: handler slots ("vectors") for drivers, wired to the
//! GIC.
//!
//! The interface is the x86 one: [`alloc`] hands out a small number that
//! names a handler slot; [`route_gsi`] connects a wired interrupt (on Arm a
//! GIC SPI: GSI = INTID) to it, [`msi_message`] says what a device writes to
//! raise it. Until the GIC driver is in (milestone 2) no slots are handed
//! out: drivers fall back to polling, as they do on x86 when a device has
//! no interrupt.

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;

/// The numbers handed out start here, as on x86 (where they are IDT vectors).
pub const FIRST: u8 = 0x50;
pub const COUNT: usize = 64;

static HANDLER: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static ARG: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static HITS: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
static NAMES: Mutex<Vec<(u8, String)>> = Mutex::new(Vec::new());
static NEXT: AtomicUsize = AtomicUsize::new(0);
/// An interrupt controller driver is up (slots can be routed).
static READY: AtomicBool = AtomicBool::new(false);

/// Reserves a slot that runs `handler(arg)`; `None` when all are taken,
/// or while no interrupt controller can deliver it (the driver then polls).
pub fn alloc(name: &str, handler: fn(usize), arg: usize) -> Option<u8> {
    if !READY.load(Ordering::Acquire) {
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
#[allow(dead_code)] // called once the GIC routes interrupts
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

/// An IRQ exception (from `trap`). Nothing unmasks interrupts at the
/// controller yet, so one arriving is a bug worth stopping for.
pub fn interrupt() {
    panic!("IRQ taken, but no interrupt controller driver is running yet");
}

/// Routes wired interrupt `gsi` to slot `vector`; false while there is no
/// interrupt controller driver.
pub fn route_gsi(_gsi: u32, _vector: u8, _level: bool, _active_low: bool) -> bool {
    false
}

/// Masks a wired interrupt.
pub fn mask_gsi(_gsi: u32) {}

/// The MSI message (address, data) that raises `vector`: none yet (the
/// device then gets no MSI and its driver polls).
pub fn msi_message(_vector: u8) -> (u64, u32) {
    (0, 0)
}

/// Inter-processor interrupts.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ipi {
    /// Look at the run queue (`sched::on_reschedule_ipi`).
    Reschedule,
    /// Service a TLB shootdown (`vmm::service_shootdown`).
    TlbShootdown,
}

/// Sends `ipi` to CPU `cpu`: nothing to send to while only the boot CPU runs.
pub fn send_ipi(_cpu: usize, _ipi: Ipi) {}
