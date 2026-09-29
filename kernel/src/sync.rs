//! Kernel synchronization primitives.
//!
//! On a single core with a preemptive scheduler, a task that is preempted
//! while holding a plain spinlock would deadlock any other task that tries to
//! take it. `IrqMutex` disables interrupts for as long as the guard lives, so a
//! holder can never be preempted and interrupt handlers can share state safely.

use core::ops::{Deref, DerefMut};
use x86_64::instructions::interrupts;

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

/// A mutex for task context that may be held across blocking operations
/// (disk I/O, sleeping). Contenders yield to the scheduler instead of
/// spinning with interrupts off. Never take it from an interrupt handler.
pub struct Mutex<T> {
    locked: core::sync::atomic::AtomicBool,
    value: core::cell::UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Mutex<T> {}
unsafe impl<T: Send> Send for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self { locked: core::sync::atomic::AtomicBool::new(false), value: core::cell::UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        use core::sync::atomic::Ordering;
        while self.locked.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            crate::sched::yield_now();
        }
        MutexGuard { lock: self }
    }

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
        self.lock.locked.store(false, core::sync::atomic::Ordering::Release);
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

    pub fn wake_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        for t in waiters {
            crate::sched::wake(t);
        }
    }
}
