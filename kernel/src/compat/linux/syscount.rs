//! System call counts per program (`WAVEOS_SYSCOUNT=1` in its
//! environment): how many of each call it made and how long they took
//! (waits included), written to the system log when the program ends or
//! `exec`s another. Cheaper than tracing every call (`WAVEOS_STRACE=1`), so
//! the program runs at nearly its usual speed: for finding what a program
//! asks of the kernel per frame (Mesa: futexes, `mmap` churn, ioctls).

use crate::sync::Mutex;
use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

/// Calls numbered beyond this are counted together in the last slot.
const SLOTS: usize = 512;

#[derive(Clone, Copy, Default)]
struct Slot {
    count: u64,
    ns: u64,
    max_ns: u64,
}

struct Table {
    name: String,
    since_ns: u64,
    slots: [Slot; SLOTS],
}

pub struct Counts {
    on: AtomicBool,
    table: Mutex<Option<Box<Table>>>,
}

/// True if the environment asks for system calls to be counted.
pub fn wanted(env: &[String]) -> bool {
    env.iter().any(|e| e == "WAVEOS_SYSCOUNT=1")
}

impl Default for Counts {
    fn default() -> Counts {
        Counts::new(false)
    }
}

impl Counts {
    pub fn new(on: bool) -> Counts {
        Counts { on: AtomicBool::new(on), table: Mutex::new(None) }
    }

    pub fn on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// A new program: what the old one counted is reported.
    pub fn reset(&self, on: bool) {
        self.report();
        self.on.store(on, Ordering::Relaxed);
    }

    /// Call `nr` of program `name` took `ns`.
    pub fn record(&self, name: impl FnOnce() -> String, nr: u64, ns: u64) {
        let mut t = self.table.lock();
        let t = t.get_or_insert_with(|| {
            Box::new(Table { name: name(), since_ns: crate::time::now_ns(), slots: [Slot::default(); SLOTS] })
        });
        let s = &mut t.slots[(nr as usize).min(SLOTS - 1)];
        s.count += 1;
        s.ns += ns;
        s.max_ns = s.max_ns.max(ns);
    }

    /// Writes the counts so far to the system log (most time first) and
    /// starts again.
    pub fn report(&self) {
        let Some(t) = self.table.lock().take() else { return };
        let ms = (crate::time::now_ns() - t.since_ns) / 1_000_000;
        let total: u64 = t.slots.iter().map(|s| s.count).sum();
        log!("syscount", "{}: {total} system calls in {ms} ms; most time (waits included):", t.name);
        let mut order: Vec<usize> = (0..SLOTS).filter(|&i| t.slots[i].count > 0).collect();
        order.sort_by_key(|&i| core::cmp::Reverse(t.slots[i].ns));
        for i in order.into_iter().take(16) {
            let s = &t.slots[i];
            log!(
                "syscount",
                "  call {i:>3}: {:>7} calls, {:>6} ms, {:>5} us each, max {} us",
                s.count,
                s.ns / 1_000_000,
                s.ns / s.count / 1000,
                s.max_ns / 1000
            );
        }
    }
}

impl Drop for Counts {
    fn drop(&mut self) {
        self.report();
    }
}
