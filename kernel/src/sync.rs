//! Kernel synchronization primitives.
//!
//! On a single core with a preemptive scheduler, a task that is preempted
//! while holding a plain spinlock would deadlock any other task that tries to
//! take it. `IrqMutex` disables interrupts for as long as the guard lives, so a
//! holder can never be preempted and interrupt handlers can share state safely.

use core::ops::{Deref, DerefMut};
use crate::arch::interrupts;

pub struct IrqMutex<T> {
    inner: spin::Mutex<T>,
}

pub struct IrqMutexGuard<'a, T> {
    guard: Option<spin::MutexGuard<'a, T>>,
    reenable: bool,
}

impl<T> IrqMutex<T> {
    pub const fn new(value: T) -> Self {
        Self { inner: spin::Mutex::new(value) }
    }

    pub fn lock(&self) -> IrqMutexGuard<'_, T> {
        let reenable = interrupts::are_enabled();
        interrupts::disable();
        IrqMutexGuard { guard: Some(self.inner.lock()), reenable }
    }

    /// The lock if it is free right now (the panic handler, which must not wait).
    pub fn try_lock(&self) -> Option<IrqMutexGuard<'_, T>> {
        let reenable = interrupts::are_enabled();
        interrupts::disable();
        match self.inner.try_lock() {
            Some(g) => Some(IrqMutexGuard { guard: Some(g), reenable }),
            None => {
                if reenable {
                    interrupts::enable();
                }
                None
            }
        }
    }
}

impl<T> Deref for IrqMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_ref().unwrap()
    }
}

impl<T> DerefMut for IrqMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard.as_mut().unwrap()
    }
}

impl<T> Drop for IrqMutexGuard<'_, T> {
    fn drop(&mut self) {
        self.guard.take();
        if self.reenable {
            interrupts::enable();
        }
    }
}

/// Sleeps contenders took waiting for a [`Mutex`] held a long time (tests).
#[cfg(feature = "ktest")]
pub static LONG_WAITS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// A mutex for task context that may be held across blocking operations
/// (disk I/O, sleeping). Contenders yield to the scheduler instead of
/// spinning with interrupts off. Never take it from an interrupt handler.
pub struct Mutex<T> {
    locked: core::sync::atomic::AtomicBool,
    /// A task sleeping for the lock, for the holder to wake when it lets go
    /// (0: none; others waiting meanwhile look again every millisecond).
    sleeper: core::sync::atomic::AtomicU64,
    value: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: core::sync::atomic::AtomicBool::new(false),
            sleeper: core::sync::atomic::AtomicU64::new(0),
            value: core::cell::UnsafeCell::new(value),
        }
    }

    /// Waits for the lock: yielding at first; once it has been held for a
    /// couple of milliseconds (its holder is doing I/O, say), sleeping until
    /// the holder lets go (one waiter is woken then; any others look again
    /// every millisecond). Yielding alone could starve the holder for good
    /// when it runs in an idle task (the boot code does, and the idle task
    /// runs only when nothing else is ready): contenders that only ever
    /// yield are always ready.
    pub fn lock(&self) -> MutexGuard<'_, T> {
        use core::sync::atomic::Ordering;
        let mut since = 0;
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            let now = crate::time::now_ns();
            if since == 0 {
                since = now;
            }
            if now.saturating_sub(since) < 2_000_000 {
                crate::sched::yield_now();
            } else {
                #[cfg(feature = "ktest")]
                LONG_WAITS.fetch_add(1, Ordering::Relaxed);
                let me = crate::sched::current_id();
                let woken = me != 0 && self.sleeper.compare_exchange(0, me, Ordering::SeqCst, Ordering::Relaxed).is_ok();
                // (Marked asleep before it looks: a release in between is seen.)
                crate::sched::wait_until(1, || !self.locked.load(Ordering::SeqCst));
                if woken {
                    let _ = self.sleeper.compare_exchange(me, 0, Ordering::SeqCst, Ordering::Relaxed);
                }
            }
        }
        MutexGuard { lock: self }
    }

    /// The lock, if nobody holds it right now.
    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        use core::sync::atomic::Ordering;
        self.locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .ok()
            .map(|_| MutexGuard { lock: self })
    }
}

impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        use core::sync::atomic::Ordering;
        self.lock.locked.store(false, Ordering::SeqCst);
        let sleeper = self.lock.sleeper.swap(0, Ordering::SeqCst);
        if sleeper != 0 {
            crate::sched::wake(sleeper);
        }
    }
}

/// Tasks waiting for an event signalled from an interrupt handler (a disk
/// completion, a USB transfer). The waiter sleeps; [`WaitQueue::wake_all`]
/// (safe in interrupt context) makes it runnable to re-check its condition.
pub struct WaitQueue {
    waiters: IrqMutex<alloc::vec::Vec<crate::sched::TaskId>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self { waiters: IrqMutex::new(alloc::vec::Vec::new()) }
    }

    /// Waits until `done()` holds, for at most `timeout_ms`; returns whether
    /// it did. Polls briefly first — fast devices often finish within
    /// microseconds, cheaper than a round trip through the scheduler.
    pub fn wait(&self, timeout_ms: u64, done: impl Fn() -> bool) -> bool {
        for _ in 0..200 {
            if done() {
                return true;
            }
            core::hint::spin_loop();
        }
        let me = crate::sched::current_id();
        let deadline = crate::time::uptime_ms() + timeout_ms;
        loop {
            if done() {
                return true;
            }
            let now = crate::time::uptime_ms();
            if now > deadline {
                return done();
            }
            self.waiters.lock().push(me);
            // Sleeps unless `done()` holds once the task is marked sleeping, so
            // an interrupt between the check and the sleep is not lost. The
            // idle task (early boot) cannot sleep and simply polls.
            crate::sched::wait_until((deadline - now).max(1), &done);
            self.waiters.lock().retain(|&t| t != me);
        }
    }

    /// Wakes every waiter. Each stays on the list until it leaves it
    /// itself (after its sleep): a waiter is on the list a moment before it
    /// is asleep, and a wakeup in that moment (for an earlier event) must not
    /// take it off, or the wakeup for its own event finds the list empty and
    /// it sleeps until its timeout (virtio-blk reads stalled for exactly
    /// 5 s, now and then, this way).
    pub fn wake_all(&self) {
        let waiters = self.waiters.lock();
        for &t in waiters.iter() {
            crate::sched::wake(t);
        }
    }
}
