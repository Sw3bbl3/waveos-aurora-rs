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
