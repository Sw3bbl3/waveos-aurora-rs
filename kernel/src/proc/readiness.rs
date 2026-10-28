//! Waiting for "something changed" across many objects (Linux `poll`,
//! `select` and `epoll`): pipes, event and timer descriptors and the network
//! stack call [`notify`] when they change state, and waiters sleep in
//! [`wait`] until the generation moves on.

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

static GEN: AtomicU64 = AtomicU64::new(0);
static WAITERS: IrqMutex<Vec<TaskId>> = IrqMutex::new(Vec::new());

/// The current generation (read before checking, then pass it to [`wait`]).
pub fn generation() -> u64 {
    GEN.load(Ordering::Acquire)
}

/// Something may have become readable or writable: wake every waiter.
pub fn notify() {
    GEN.fetch_add(1, Ordering::AcqRel);
    let waiters = core::mem::take(&mut *WAITERS.lock());
    for t in waiters {
        sched::wake(t);
    }
}

/// Sleeps up to `ms` while the generation is still `gen` (or until the
/// caller is interrupted).
pub fn wait(gen: u64, ms: u64) {
    let me = sched::current_id();
    WAITERS.lock().push(me);
    sched::wait_until(ms, || GEN.load(Ordering::Acquire) != gen || super::interrupted());
    WAITERS.lock().retain(|&t| t != me);
}
