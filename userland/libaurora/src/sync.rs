//! Thread synchronization built on futexes: the fast path is a single atomic
//! operation, and only contended threads enter the kernel.

use crate::abi::nr;
use crate::sys::call;
use core::cell::UnsafeCell;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicU32, Ordering};

/// Sleeps while `*word == expected` (spurious wakeups are possible).
pub fn futex_wait(word: &AtomicU32, expected: u32, timeout_ms: Option<u64>) -> bool {
    call(nr::FUTEX_WAIT, &[word.as_ptr() as u64, expected as u64, timeout_ms.unwrap_or(u64::MAX)]).is_ok()
}

pub fn futex_wake(word: &AtomicU32, count: u32) -> u32 {
    call(nr::FUTEX_WAKE, &[word.as_ptr() as u64, count as u64]).unwrap_or(0) as u32
}

/// A lock word: 0 = unlocked, 1 = locked, 2 = locked with waiters.
pub struct RawMutex(AtomicU32);

impl RawMutex {
    pub const fn new() -> Self {
        Self(AtomicU32::new(0))
    }

    pub fn lock(&self) {
        if self.0.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok() {
            return;
        }
        // Mark contended, then sleep until we take it from the unlocked state.
        while self.0.swap(2, Ordering::Acquire) != 0 {
            futex_wait(&self.0, 2, None);
        }
    }

    pub fn try_lock(&self) -> bool {
        self.0.compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed).is_ok()
    }

    pub fn unlock(&self) {
        if self.0.swap(0, Ordering::Release) == 2 {
            futex_wake(&self.0, 1);
        }
    }
}

impl Default for RawMutex {
    fn default() -> Self {
        Self::new()
    }
}

pub struct Mutex<T> {
    raw: RawMutex,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    lock: &'a Mutex<T>,
}

impl<T> Mutex<T> {
    pub const fn new(value: T) -> Self {
        Self { raw: RawMutex::new(), value: UnsafeCell::new(value) }
    }

    pub fn lock(&self) -> MutexGuard<'_, T> {
        self.raw.lock();
        MutexGuard { lock: self }
    }

    pub fn try_lock(&self) -> Option<MutexGuard<'_, T>> {
        self.raw.try_lock().then_some(MutexGuard { lock: self })
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
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
        self.lock.raw.unlock();
    }
}

/// A condition variable: a sequence number that waiters sleep on.
pub struct Condvar {
    seq: AtomicU32,
}

impl Condvar {
    pub const fn new() -> Self {
        Self { seq: AtomicU32::new(0) }
    }

    /// Atomically releases `guard`, sleeps until notified (or `timeout_ms`), and relocks.
    pub fn wait_timeout<'a, T>(&self, guard: MutexGuard<'a, T>, timeout_ms: Option<u64>) -> MutexGuard<'a, T> {
        let seq = self.seq.load(Ordering::Acquire);
        let lock = guard.lock;
        drop(guard);
        futex_wait(&self.seq, seq, timeout_ms);
        lock.lock()
    }

    pub fn wait<'a, T>(&self, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
        self.wait_timeout(guard, None)
    }

    pub fn notify_one(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        futex_wake(&self.seq, 1);
    }

    pub fn notify_all(&self) {
        self.seq.fetch_add(1, Ordering::Release);
        futex_wake(&self.seq, u32::MAX);
    }
}

impl Default for Condvar {
    fn default() -> Self {
        Self::new()
    }
}
