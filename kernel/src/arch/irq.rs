//! Dynamically allocated interrupt vectors, for MSI and MSI-X devices.
//!
//! Vectors `FIRST..FIRST + COUNT` each have a tiny stub that calls
//! [`dispatch`], which runs the handler registered for that vector with its
//! argument and signals end-of-interrupt. Handlers run with interrupts off on
//! whichever CPU the message targets, so they must be short: acknowledge the
//! device and wake the task waiting for it.

use super::apic;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use spin::Mutex;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame};

pub const FIRST: u8 = 0x50;
pub const COUNT: usize = 64;

static HANDLER: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static ARG: [AtomicUsize; COUNT] = [const { AtomicUsize::new(0) }; COUNT];
static HITS: [AtomicU64; COUNT] = [const { AtomicU64::new(0) }; COUNT];
static NAMES: Mutex<Vec<(u8, String)>> = Mutex::new(Vec::new());
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Reserves a vector that runs `handler(arg)`; `None` when all are taken.
pub fn alloc(name: &str, handler: fn(usize), arg: usize) -> Option<u8> {
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

/// Total interrupts delivered through allocated vectors.
pub fn total() -> u64 {
    HITS.iter().map(|h| h.load(Ordering::Relaxed)).sum()
}

/// (vector, owner, interrupts so far) for every allocated vector.
pub fn list() -> Vec<(u8, String, u64)> {
    NAMES.lock().iter().map(|(v, n)| (*v, n.clone(), HITS[(*v - FIRST) as usize].load(Ordering::Relaxed))).collect()
}

fn dispatch(i: usize) {
    HITS[i].fetch_add(1, Ordering::Relaxed);
    let h = HANDLER[i].load(Ordering::Acquire);
    if h != 0 {
        let f: fn(usize) = unsafe { core::mem::transmute(h) };
        f(ARG[i].load(Ordering::Relaxed));
    }
    apic::eoi();
}

extern "x86-interrupt" fn stub<const N: usize>(_frame: InterruptStackFrame) {
    dispatch(N);
}

macro_rules! install_stubs {
    ($idt:ident; $($n:literal)*) => {
        $( $idt[FIRST + $n].set_handler_fn(stub::<$n>); )*
    };
}

/// Points every dynamic vector at its stub.
pub fn install(idt: &mut InterruptDescriptorTable) {
    install_stubs!(idt;
        0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31
        32 33 34 35 36 37 38 39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 57 58 59 60 61 62 63);
}
