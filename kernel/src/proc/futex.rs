//! Futexes: user-space locks that sleep in the kernel only when contended.
//!
//! Waiters are keyed by (address space, virtual address), so two processes
//! using the same address never see each other's wakeups. A futex in memory
//! shared between processes (a Linux futex without `FUTEX_PRIVATE_FLAG`, in
//! a `MAP_SHARED` page) is keyed by its physical address instead, so every
//! process mapping it meets the same waiters. The waiters are spread over
//! buckets by key, each with its own lock.

use crate::sched::{self, TaskId};
use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use aurora_abi::err::*;

/// (address space, address), or (0, physical address) for shared memory.
type Key = (u64, u64);
type Bucket = IrqMutex<BTreeMap<Key, Vec<TaskId>>>;

const BUCKETS: usize = 64;
static WAITERS: [Bucket; BUCKETS] = [const { IrqMutex::new(BTreeMap::new()) }; BUCKETS];

fn bucket(k: Key) -> &'static Bucket {
    let h = (k.0 ^ k.1 >> 2).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    &WAITERS[(h >> 58) as usize % BUCKETS]
}

/// Validates a 4-byte aligned, writable user word of the current process
/// and gives its key: by physical address if `shared` and the word lives in
/// shared memory.
fn key(addr: u64, shared: bool) -> Result<Key, isize> {
    if !addr.is_multiple_of(4) {
        return Err(EINVAL);
    }
    let p = super::current().ok_or(EPERM)?;
    if crate::syscall::user::check(addr, 4, true).is_err() {
        return Err(EFAULT);
    }
    if shared {
        if let Some(phys) = p.mem().as_ref().and_then(|a| a.shared_frame(addr)) {
            return Ok((0, phys));
        }
    }
    Ok((p.cr3(), addr))
}

fn queued(k: Key, task: TaskId) -> bool {
    bucket(k).lock().get(&k).is_some_and(|v| v.contains(&task))
}

/// Takes `task` off the queue it is on (it may have been moved by a
/// requeue since it started waiting on `k`).
fn dequeue(mut k: Key, task: TaskId) {
    loop {
        {
            let mut w = bucket(k).lock();
            if let Some(v) = w.get_mut(&k) {
                v.retain(|&t| t != task);
                if v.is_empty() {
                    w.remove(&k);
                }
            }
        }
        // A requeue records the move before letting go of the queues' locks.
        match MOVED.lock().remove(&task) {
            Some(to) => k = to,
            None => return,
        }
    }
}

/// Sleeps while `*addr == expected`, until woken, interrupted or timed out.
pub fn wait(addr: u64, expected: u32, timeout_ms: u64) -> Result<u64, isize> {
    wait_shared(addr, expected, timeout_ms, false)
}

/// [`wait`], for a futex that may be shared between processes.
pub fn wait_shared(addr: u64, expected: u32, timeout_ms: u64, shared: bool) -> Result<u64, isize> {
    let k = key(addr, shared)?;
    let me = sched::current_id();
    MOVED.lock().remove(&me);
    {
        // The value is read under the lock that `wake` takes, so a wakeup
        // between the check and the sleep cannot be lost.
        let mut w = bucket(k).lock();
        let value = crate::syscall::user::read_u32(addr)?;
        if value != expected {
            return Err(EAGAIN);
        }
        w.entry(k).or_default().push(me);
    }
    let deadline = crate::time::uptime_ms().saturating_add(timeout_ms);
    // A requeue moves this waiter to another key: follow it.
    let mut at = k;
    loop {
        let now = crate::time::uptime_ms();
        match where_queued(at, me) {
            None => return Ok(0),
            Some(k) => at = k,
        }
        if super::interrupted() {
            dequeue(at, me);
            return Err(EAGAIN);
        }
        if now >= deadline {
            dequeue(at, me);
            return Err(ETIMEDOUT);
        }
        sched::wait_until(deadline - now, || !queued(at, me) || super::interrupted());
    }
}

/// Requeued waiters, by task: the key each was moved to.
static MOVED: IrqMutex<BTreeMap<TaskId, Key>> = IrqMutex::new(BTreeMap::new());

/// The key `task` waits on now, starting from `k`; `None` once woken.
fn where_queued(k: Key, task: TaskId) -> Option<Key> {
    if queued(k, task) {
        return Some(k);
    }
    let moved = MOVED.lock().remove(&task)?;
    // Moved, and perhaps woken since from its new queue.
    if queued(moved, task) {
        Some(moved)
    } else {
        None
    }
}

/// Wakes up to `count` waiters on `addr`; returns how many were woken.
pub fn wake(addr: u64, count: u64) -> Result<u64, isize> {
    wake_shared(addr, count, false)
}

/// [`wake`], for a futex that may be shared between processes.
pub fn wake_shared(addr: u64, count: u64, shared: bool) -> Result<u64, isize> {
    let k = key(addr, shared)?;
    Ok(wake_key(k, count))
}

fn wake_key(k: Key, count: u64) -> u64 {
    let woken: Vec<TaskId> = {
        let mut w = bucket(k).lock();
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

/// Linux `FUTEX_REQUEUE` / `FUTEX_CMP_REQUEUE` (with `cmp`): wakes up to
/// `count` waiters on `addr` and moves up to `count2` of the others to
/// `addr2`, where a later wake finds them. Returns how many were woken
/// (plus, for the compare form, how many were moved).
pub fn requeue(addr: u64, count: u64, addr2: u64, count2: u64, cmp: Option<u32>, shared: bool) -> Result<u64, isize> {
    let k = key(addr, shared)?;
    let k2 = key(addr2, shared)?;
    if k == k2 {
        // Nothing to move: only a wake.
        if let Some(v) = cmp {
            if crate::syscall::user::read_u32(addr)? != v {
                return Err(EAGAIN);
            }
        }
        return Ok(wake_key(k, count));
    }
    // Both buckets' locks, always in the same order.
    let (b1, b2) = (bucket(k), bucket(k2));
    let same = core::ptr::eq(b1, b2);
    let first_is_k = (b1 as *const Bucket) <= (b2 as *const Bucket);
    let mut g1 = if first_is_k { b1.lock() } else { b2.lock() };
    let mut g2 = if same { None } else { Some(if first_is_k { b2.lock() } else { b1.lock() }) };
    if let Some(v) = cmp {
        if crate::syscall::user::read_u32(addr)? != v {
            return Err(EAGAIN);
        }
    }
    let (mut woken, moved) = {
        // (Recorded while the queues are locked: a waiter that finds
        // itself gone from its queue then always finds where it went.)
        let (from, to) = match g2.as_mut() {
            None => (&mut *g1, None),
            Some(other) if first_is_k => (&mut *g1, Some(&mut **other)),
            Some(other) => (&mut **other, Some(&mut *g1)),
        };
        let mut waiting = from.remove(&k).unwrap_or_default();
        let n = (count as usize).min(waiting.len());
        let woken: Vec<TaskId> = waiting.drain(..n).collect();
        let m = (count2 as usize).min(waiting.len());
        let moved: Vec<TaskId> = waiting.drain(..m).collect();
        if !waiting.is_empty() {
            from.insert(k, waiting);
        }
        if !moved.is_empty() {
            let to = match to {
                Some(t) => t,
                None => from,
            };
            to.entry(k2).or_default().extend(moved.iter().copied());
            let mut m = MOVED.lock();
            for &t in &moved {
                m.insert(t, k2);
            }
        }
        (woken, moved)
    };
    drop(g2);
    drop(g1);
    for &t in &woken {
        sched::wake(t);
    }
    if cmp.is_some() {
        woken.extend(moved.iter().copied());
    }
    Ok(woken.len() as u64)
}

/// Linux `FUTEX_WAKE_OP`: changes `*addr2` atomically as `op` says, wakes
/// up to `count` waiters on `addr`, and, if the old value of `*addr2`
/// passes `op`'s comparison, up to `count2` waiters on `addr2`.
pub fn wake_op(addr: u64, count: u64, addr2: u64, count2: u64, op: u32, shared: bool) -> Result<u64, isize> {
    let k = key(addr, shared)?;
    let k2 = key(addr2, shared)?;
    let mut arg = (op >> 12) & 0xFFF;
    if op & (8 << 28) != 0 {
        arg = 1u32.checked_shl(arg).unwrap_or(0);
    }
    let old = super::current().ok_or(EPERM)?.mem().as_mut().ok_or(EFAULT).and_then(|a| {
        a.atomic_update_u32(addr2, |v| match (op >> 28) & 7 {
            0 => arg,
            1 => v.wrapping_add(arg),
            2 => v | arg,
            3 => v & !arg,
            _ => v ^ arg,
        })
    })?;
    let cmparg = op & 0xFFF;
    let pass = match (op >> 24) & 0xF {
        0 => old == cmparg,
        1 => old != cmparg,
        2 => (old as i32) < cmparg as i32,
        3 => (old as i32) <= cmparg as i32,
        4 => (old as i32) > cmparg as i32,
        _ => (old as i32) >= cmparg as i32,
    };
    let mut n = wake_key(k, count);
    if pass {
        n += wake_key(k2, count2);
    }
    Ok(n)
}

/// `thread_exit(notify)`: stores 1 into `notify` and wakes everyone joining on it.
pub fn notify_exit(addr: u64) {
    if let Ok(k) = key(addr, false) {
        let _ = crate::syscall::user::write_u32(addr, 1);
        wake_key(k, u64::MAX);
    }
}
