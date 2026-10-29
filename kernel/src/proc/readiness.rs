//! Waiting for "something changed" across many objects (Linux `poll`,
//! `select` and `epoll`, and blocking reads and writes of event descriptors
//! and local sockets). Each object has a key (its address, from [`key`]):
//! pipes, event and timer descriptors, local sockets and the network stack
//! call [`notify`] with theirs when they change state, and a waiter sleeps
//! in [`wait`] on the keys of what it watches, so it wakes only for those.
//! (Before, every change woke every waiter in the system to look again:
//! with Firefox's dozens of event loops, most of the kernel's time.)

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// The key of a waiter that wants to hear of every change (what it
/// watches has no key of its own, e.g. an epoll set inside another).
pub const ANY: usize = 0;
/// The key of all network sockets: the network stack says only that
/// something changed.
pub const NET: usize = 1;

static GEN: AtomicU64 = AtomicU64::new(0);
static WAITERS: IrqMutex<Vec<(TaskId, Vec<usize>)>> = IrqMutex::new(Vec::new());

/// The key of the object at `p`.
pub fn key<T: ?Sized>(p: *const T) -> usize {
    p as *const () as usize
}

/// The current generation (read before checking, then pass it to [`wait`]).
pub fn generation() -> u64 {
    GEN.load(Ordering::Acquire)
}

/// The object with key `key` may have become readable or writable: wake
/// whoever waits on it.
pub fn notify(key: usize) {
    GEN.fetch_add(1, Ordering::AcqRel);
    let mut woken = Vec::new();
    WAITERS.lock().retain(|(t, keys)| {
        let hit = keys.iter().any(|&k| k == key || k == ANY);
        if hit {
            woken.push(*t);
        }
        !hit
    });
    for t in woken {
        sched::wake(t);
    }
}

/// Sleeps up to `ms` while the generation is still `gen` (any change since
/// the caller looked: it looks again), until one of `keys` is notified, or
/// until the caller is interrupted.
pub fn wait(gen: u64, keys: &[usize], ms: u64) {
    let me = sched::current_id();
    WAITERS.lock().push((me, keys.to_vec()));
    sched::wait_until(ms, || GEN.load(Ordering::Acquire) != gen || super::interrupted());
    WAITERS.lock().retain(|(t, _)| *t != me);
}
