//! Waiting for "something changed" across many objects (Linux `poll`,
//! `select` and `epoll`, and blocking reads and writes of event descriptors
//! and local sockets). Each object has a key (its address, from [`key`]):
//! pipes, event and timer descriptors, local sockets and the network stack
//! call [`notify`] with theirs when they change state, and a waiter sleeps
//! in [`Watch::wait`] on the keys of what it watches, so it wakes only for those.
//! (Before, every change woke every waiter in the system to look again:
//! with Firefox's dozens of event loops, most of the kernel's time.)

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

/// The key of a waiter that wants to hear of every change (what it
/// watches has no key of its own, e.g. an epoll set inside another).
pub const ANY: usize = 0;
/// The key of all network sockets: the network stack says only that
/// something changed.
pub const NET: usize = 1;

struct Entry {
    task: TaskId,
    keys: Vec<usize>,
    notified: Arc<AtomicBool>,
}
static WAITERS: IrqMutex<Vec<Entry>> = IrqMutex::new(Vec::new());

/// The key of the object at `p`.
pub fn key<T: ?Sized>(p: *const T) -> usize {
    p as *const () as usize
}

/// The object with key `key` may have become readable or writable: wake
/// whoever waits on it.
pub fn notify(key: usize) {
    let mut woken = Vec::new();
    for entry in WAITERS.lock().iter() {
        let hit = entry.keys.iter().any(|&k| k == key || k == ANY);
        if hit && !entry.notified.swap(true, Ordering::AcqRel) {
            woken.push(entry.task);
        }
    }
    for t in woken {
        sched::wake(t);
    }
}

/// Register BEFORE checking an object's readiness. A matching notification
/// between the check and sleep is retained; unrelated activity cannot cancel
/// the sleep. The former global generation made slow epoll scans busy-loop.
pub struct Watch {
    notified: Arc<AtomicBool>,
}

impl Watch {
    pub fn new(keys: &[usize]) -> Self {
        let notified = Arc::new(AtomicBool::new(false));
        let mut keys = keys.to_vec();
        keys.sort_unstable();
        keys.dedup();
        WAITERS.lock().push(Entry { task: sched::current_id(), keys, notified: notified.clone() });
        Self { notified }
    }

    pub fn wait(self, ms: u64) {
        sched::wait_until(ms, || self.notified.load(Ordering::Acquire) || super::interrupted());
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        WAITERS.lock().retain(|entry| !Arc::ptr_eq(&entry.notified, &self.notified));
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    let watched = 0x5741_2000;
    let other = watched + 1;
    let watch = Watch::new(&[watched, watched]);
    notify(other);
    assert!(!watch.notified.load(Ordering::Acquire), "unrelated activity must not cancel a wait");
    let start = crate::time::uptime_ms();
    watch.wait(15);
    assert!(crate::time::uptime_ms() - start >= 14, "an unrelated notification caused a busy rescan");

    let watch = Watch::new(&[watched]);
    notify(watched); // notification before sleeping must not be lost
    assert!(watch.notified.load(Ordering::Acquire));
    let start = crate::time::uptime_ms();
    watch.wait(100);
    assert!(crate::time::uptime_ms() - start < 50);

    let watch = Watch::new(&[ANY]);
    notify(other);
    assert!(watch.notified.load(Ordering::Acquire));
    drop(watch);
}
