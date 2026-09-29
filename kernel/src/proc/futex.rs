//! Futexes: user-space locks that sleep in the kernel only when contended.
//!
//! Waiters are keyed by (address space, virtual address), so two processes
//! using the same address never see each other's wakeups.

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aurora_abi::err::*;

type Key = (u64, u64);

static WAITERS: IrqMutex<BTreeMap<Key, Vec<TaskId>>> = IrqMutex::new(BTreeMap::new());

/// Validates a 4-byte aligned, writable user word of the current process.
fn key(addr: u64) -> Result<Key, isize> {
    if addr % 4 != 0 {
        return Err(EINVAL);
    }
    let p = super::current().ok_or(EPERM)?;
    let ok = p.aspace.lock().as_ref().is_some_and(|a| a.check(addr, 4, true));
    if !ok {
        return Err(EFAULT);
    }
    Ok((p.cr3, addr))
}

fn queued(k: Key, task: TaskId) -> bool {
    WAITERS.lock().get(&k).is_some_and(|v| v.contains(&task))
}

fn dequeue(k: Key, task: TaskId) {
    let mut w = WAITERS.lock();
    if let Some(v) = w.get_mut(&k) {
        v.retain(|&t| t != task);
        if v.is_empty() {
            w.remove(&k);
        }
    }
}

/// Sleeps while `*addr == expected`, until woken, interrupted or timed out.
pub fn wait(addr: u64, expected: u32, timeout_ms: u64) -> Result<u64, isize> {
    let k = key(addr)?;
    let me = sched::current_id();
    {
        // The value is read under the lock that `wake` takes, so a wakeup
        // between the check and the sleep cannot be lost.
        let mut w = WAITERS.lock();
        let value = crate::syscall::user::read_u32(addr)?;
        if value != expected {
            return Err(EAGAIN);
        }
        w.entry(k).or_default().push(me);
    }
    let deadline = crate::time::uptime_ms().saturating_add(timeout_ms);
    loop {
        let now = crate::time::uptime_ms();
        if !queued(k, me) {
            return Ok(0);
        }
        if super::interrupted() {
            dequeue(k, me);
            return Err(EAGAIN);
        }
        if now >= deadline {
            dequeue(k, me);
            return Err(ETIMEDOUT);
        }
        sched::wait_until(deadline - now, || !queued(k, me) || super::interrupted());
    }
}

/// Wakes up to `count` waiters on `addr`; returns how many were woken.
pub fn wake(addr: u64, count: u64) -> Result<u64, isize> {
    let k = key(addr)?;
    Ok(wake_key(k, count))
}

fn wake_key(k: Key, count: u64) -> u64 {
    let woken: Vec<TaskId> = {
        let mut w = WAITERS.lock();
        let Some(v) = w.get_mut(&k) else { return 0 };
        let n = (count as usize).min(v.len());
        let woken: Vec<TaskId> = v.drain(..n).collect();
        if v.is_empty() {
            w.remove(&k);
        }
        woken
    };
    for &t in &woken {
        sched::wake(t);
    }
    woken.len() as u64
}

/// `thread_exit(notify)`: stores 1 into `notify` and wakes everyone joining on it.
pub fn notify_exit(addr: u64) {
    if let Ok(k) = key(addr) {
        let _ = crate::syscall::user::write_u32(addr, 1);
        wake_key(k, u64::MAX);
    }
}
