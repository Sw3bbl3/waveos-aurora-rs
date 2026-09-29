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
