//! Descriptors Linux event loops are made of: `eventfd` (a counter),
//! `timerfd` (a timer read as a count of expirations) and `epoll` (a set of
//! descriptors to watch). All of them are level-triggered here: `epoll`
//! reports a descriptor for as long as it is ready (`EPOLLET` users read
//! until `EAGAIN`, which works the same), and `EPOLLONESHOT` disables a
//! descriptor once reported.

use super::fs::{Special, O_CLOEXEC};
use crate::proc::{readiness, Handle, Process};
use crate::sync::{IrqMutex, Mutex};
use crate::syscall::user;
use crate::time;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

const NONBLOCK: u64 = 0o4000;

// ------------------------------------------------------------------ eventfd

pub struct EventFd {
    value: IrqMutex<u64>,
    semaphore: bool,
    pub nonblocking: bool,
}

const EVENT_MAX: u64 = u64::MAX - 1;

impl EventFd {
    pub fn readable(&self) -> bool {
        *self.value.lock() > 0
    }

    pub fn writable(&self) -> bool {
        *self.value.lock() < EVENT_MAX
    }

    pub fn read(&self, out: &mut [u8]) -> Result<usize, isize> {
        if out.len() < 8 {
            return Err(EINVAL);
        }
        loop {
            let gen = readiness::generation();
            {
                let mut v = self.value.lock();
                if *v > 0 {
                    let n = if self.semaphore { 1 } else { *v };
                    *v -= n;
                    drop(v);
                    out[..8].copy_from_slice(&n.to_le_bytes());
                    readiness::notify(readiness::key(self));
                    return Ok(8);
                }
            }
            if self.nonblocking {
                return Err(EAGAIN);
            }
            if crate::proc::interrupted() {
                return Err(EINTR);
            }
            readiness::wait(gen, &[readiness::key(self)], 1000);
        }
    }

    pub fn write(&self, data: &[u8]) -> Result<usize, isize> {
        let add = u64::from_le_bytes(data.get(..8).ok_or(EINVAL)?.try_into().unwrap());
        if add == u64::MAX {
            return Err(EINVAL);
        }
        loop {
            let gen = readiness::generation();
            {
                let mut v = self.value.lock();
                if EVENT_MAX - *v >= add {
                    *v += add;
                    drop(v);
                    readiness::notify(readiness::key(self));
                    return Ok(8);
                }
            }
            if self.nonblocking {
                return Err(EAGAIN);
            }
            if crate::proc::interrupted() {
                return Err(EINTR);
            }
            readiness::wait(gen, &[readiness::key(self)], 1000);
        }
    }
}

pub fn eventfd(p: &Process, initial: u64, flags: u64) -> Result<u64, isize> {
    let e = EventFd { value: IrqMutex::new(initial), semaphore: flags & 1 != 0, nonblocking: flags & NONBLOCK != 0 };
    super::fs::install_special(p, Special::Event(Arc::new(e)), flags & O_CLOEXEC != 0)
}

// ------------------------------------------------------- WaveOS window events

/// `WAVE_EVENT_FD(flags)`: a descriptor that is readable while the process
/// has window events waiting (reading gives how many, as 8 bytes, without
/// taking them: `next_event` does), so a program's `poll` or `epoll` loop
/// can wait for them with everything else. `flags` takes `O_CLOEXEC`.
pub fn wave_event_fd(p: &Process, flags: u64) -> Result<u64, isize> {
    super::fs::install_special(p, Special::WaveEvents(p.pid), flags & O_CLOEXEC != 0)
}

/// Reading a [`wave_event_fd`] descriptor.
pub fn wave_events_read(pid: u32, out: &mut [u8]) -> Result<usize, isize> {
    if out.len() < 8 {
        return Err(EINVAL);
    }
    let n = crate::gui::server::events_waiting(pid) as u64;
    if n == 0 {
        return Err(EAGAIN);
    }
    out[..8].copy_from_slice(&n.to_le_bytes());
    Ok(8)
}

// ------------------------------------------------------------------ timerfd

pub struct TimerFd {
    state: IrqMutex<Timer>,
    realtime: bool,
    pub nonblocking: bool,
}

#[derive(Clone, Copy, Default)]
struct Timer {
    /// When it next expires (monotonic ns; 0: disarmed).
    next: u64,
    interval: u64,
    /// Expirations not read yet.
    count: u64,
}

impl Timer {
    /// Counts expirations up to now.
    fn advance(&mut self, now: u64) {
        if self.next == 0 || now < self.next {
            return;
        }
        match (now - self.next).checked_div(self.interval) {
            // Periodic: every interval that has passed counts.
            Some(k) => {
                self.count += k + 1;
                self.next += (k + 1) * self.interval;
            }
            None => {
                self.count += 1;
                self.next = 0;
            }
        }
    }
}

impl TimerFd {
    pub fn readable(&self) -> bool {
        let mut t = self.state.lock();
        t.advance(time::now_ns());
        t.count > 0
    }

    /// Milliseconds until it next expires, if it is armed.
    pub fn due_in(&self) -> Option<u64> {
        let t = self.state.lock();
        (t.next != 0).then(|| t.next.saturating_sub(time::now_ns()).div_ceil(1_000_000))
    }

    pub fn read(&self, out: &mut [u8]) -> Result<usize, isize> {
        if out.len() < 8 {
            return Err(EINVAL);
        }
        loop {
            let wait_ms = {
                let mut t = self.state.lock();
                t.advance(time::now_ns());
                if t.count > 0 {
                    out[..8].copy_from_slice(&t.count.to_le_bytes());
                    t.count = 0;
                    return Ok(8);
                }
                if t.next == 0 {
                    // Never expires: wait for a signal (or a re-arm by another thread).
                    1000
                } else {
                    t.next.saturating_sub(time::now_ns()).div_ceil(1_000_000).max(1)
                }
            };
            if self.nonblocking {
                return Err(EAGAIN);
            }
            if crate::proc::interrupted() {
                return Err(EINTR);
            }
            crate::sched::wait_until(wait_ms, crate::proc::interrupted);
        }
    }
}

pub fn timerfd_create(p: &Process, clock: u64, flags: u64) -> Result<u64, isize> {
    if !matches!(clock, 0 | 1 | 7 | 8 | 9) {
        return Err(EINVAL);
    }
    let t = TimerFd {
        state: IrqMutex::new(Timer::default()),
        realtime: clock == 0 || clock == 8,
        nonblocking: flags & NONBLOCK != 0,
    };
    super::fs::install_special(p, Special::Timer(Arc::new(t)), flags & O_CLOEXEC != 0)
}

fn timer_of(p: &Process, fd: u64) -> Result<Arc<TimerFd>, isize> {
    match &*p.fd(fd as usize)? {
        Handle::Special(s) => match &*s.lock() {
            Special::Timer(t) => Ok(t.clone()),
            _ => Err(EINVAL),
        },
        _ => Err(EINVAL),
    }
}

fn ns(ts: [i64; 2]) -> u64 {
    ts[0].max(0) as u64 * 1_000_000_000 + ts[1].max(0) as u64
}

fn put_itimerspec(ptr: u64, t: &Timer) -> Result<(), isize> {
    let left = if t.next == 0 { 0 } else { t.next.saturating_sub(time::now_ns()).max(1) };
    let v = [
        (t.interval / 1_000_000_000) as i64,
        (t.interval % 1_000_000_000) as i64,
        (left / 1_000_000_000) as i64,
        (left % 1_000_000_000) as i64,
    ];
    user::put(ptr, &v)
}

pub fn timerfd_settime(p: &Process, fd: u64, flags: u64, new: u64, old: u64) -> Result<u64, isize> {
    let tf = timer_of(p, fd)?;
    let [isec, insec, vsec, vnsec]: [i64; 4] = user::get(new)?;
    let interval = ns([isec, insec]);
    let value = ns([vsec, vnsec]);
    let now = time::now_ns();
    let next = if value == 0 {
        0
    } else if flags & 1 != 0 {
        // TFD_TIMER_ABSTIME, on the timer's clock.
        let clock_now = if tf.realtime { super::misc::realtime_ns() } else { now };
        now + value.saturating_sub(clock_now).max(1)
    } else {
        now + value
    };
    let prev = {
        let mut t = tf.state.lock();
        let prev = *t;
        *t = Timer { next, interval, count: 0 };
        prev
    };
    readiness::notify(readiness::key(&*tf));
    if old != 0 {
        put_itimerspec(old, &prev)?;
    }
    Ok(0)
}

pub fn timerfd_gettime(p: &Process, fd: u64, cur: u64) -> Result<u64, isize> {
    let tf = timer_of(p, fd)?;
    let t = *tf.state.lock();
    put_itimerspec(cur, &t).map(|_| 0)
}

// ------------------------------------------------------------------ epoll

pub struct Epoll {
    /// Watched descriptors: events and the data to report back.
    interest: Mutex<BTreeMap<i32, (u32, u64)>>,
}

const EPOLLIN: u32 = 0x1;
const EPOLLOUT: u32 = 0x4;
const EPOLLERR: u32 = 0x8;
const EPOLLHUP: u32 = 0x10;
const EPOLLONESHOT: u32 = 1 << 30;

pub fn epoll_create(p: &Process, flags: u64) -> Result<u64, isize> {
    let e = Epoll { interest: Mutex::new(BTreeMap::new()) };
    super::fs::install_special(p, Special::Epoll(Arc::new(e)), flags & O_CLOEXEC != 0)
}

fn epoll_of(p: &Process, fd: u64) -> Result<Arc<Epoll>, isize> {
    match &*p.fd(fd as usize)? {
        Handle::Special(s) => match &*s.lock() {
            Special::Epoll(e) => Ok(e.clone()),
            _ => Err(EINVAL),
        },
        _ => Err(EINVAL),
    }
}

pub fn epoll_ctl(p: &Process, epfd: u64, op: u64, fd: u64, event: u64) -> Result<u64, isize> {
    let ep = epoll_of(p, epfd)?;
    if fd == epfd {
        return Err(EINVAL);
    }
    p.fd(fd as usize)?;
    let mut set = ep.interest.lock();
    let fd = fd as i32;
    match op {
        // ADD, MOD
        1 | 3 => {
            // struct epoll_event is packed: u32 events, u64 data.
            let raw: [u8; 12] = user::get(event)?;
            let events = u32::from_le_bytes(raw[0..4].try_into().unwrap());
            let data = u64::from_le_bytes(raw[4..12].try_into().unwrap());
            if op == 1 && set.contains_key(&fd) {
                return Err(EEXIST);
            }
            if op == 3 && !set.contains_key(&fd) {
                return Err(ENOENT);
            }
            set.insert(fd, (events, data));
        }
        // DEL
        2 => {
            set.remove(&fd).ok_or(ENOENT)?;
        }
        _ => return Err(EINVAL),
    }
    drop(set);
    readiness::notify(readiness::key(&*ep));
    Ok(0)
}

impl Epoll {
    /// The ready descriptors (at most `max`), as (events, data).
    fn ready(&self, p: &Process, max: usize, depth: u32) -> Vec<(u32, u64)> {
        let mut out = Vec::new();
        let mut set = self.interest.lock();
        let mut closed = Vec::new();
        for (&fd, (events, data)) in set.iter_mut() {
            if out.len() >= max {
                break;
            }
            if *events & !(EPOLLONESHOT | 1 << 31) == 0 {
                continue; // disabled (one-shot already reported)
            }
            let Some((rd, wr, hup)) = super::fs::readiness_of(p, fd as usize, depth + 1) else {
                closed.push(fd);
                continue;
            };
            let mut got = 0;
            if rd && *events & EPOLLIN != 0 {
                got |= EPOLLIN;
            }
            if wr && *events & EPOLLOUT != 0 {
                got |= EPOLLOUT;
            }
            if hup {
                got |= EPOLLHUP;
            }
            if got != 0 {
                out.push((got, *data));
                if *events & EPOLLONESHOT != 0 {
                    *events &= EPOLLONESHOT | 1 << 31;
                }
            }
        }
        // Closed descriptors leave the set, as their files go away on Linux.
        for fd in closed {
            set.remove(&fd);
        }
        let _ = EPOLLERR;
        out
    }

    pub fn readable(&self, p: &Process, depth: u32) -> bool {
        depth < 4 && !self.ready_peek(p, depth).is_empty()
    }

    fn ready_peek(&self, p: &Process, depth: u32) -> Vec<(u32, u64)> {
        let set = self.interest.lock().clone();
        set.iter()
            .filter(|(_, (ev, _))| *ev & !(EPOLLONESHOT | 1 << 31) != 0)
            .filter(|(&fd, (ev, _))| {
                super::fs::readiness_of(p, fd as usize, depth + 1)
                    .is_some_and(|(rd, wr, hup)| hup || (rd && ev & EPOLLIN != 0) || (wr && ev & EPOLLOUT != 0))
            })
            .map(|(_, v)| *v)
            .collect()
    }

    /// The soonest a watched timer expires (ms), to bound the sleep.
    /// What to wait on for a change: this set (`epoll_ctl`) and what it watches.
    fn wait_keys(&self, p: &Process) -> Vec<usize> {
        let mut keys = alloc::vec![readiness::key(self)];
        for &fd in self.interest.lock().keys() {
            super::fs::wait_keys(p, fd as usize, &mut keys);
        }
        keys
    }

    fn next_timer(&self, p: &Process) -> Option<u64> {
        let set = self.interest.lock();
        set.keys()
            .filter_map(|&fd| match &*p.fd(fd as usize).ok()? {
                Handle::Special(s) => match &*s.lock() {
                    Special::Timer(t) => t.due_in(),
                    _ => None,
                },
                _ => None,
            })
            .min()
    }
}

/// `epoll_wait`/`epoll_pwait`.
pub fn epoll_wait(p: &Process, epfd: u64, events: u64, max: u64, timeout_ms: i64, mask: u64) -> Result<u64, isize> {
    if max == 0 || max > 4096 {
        return Err(EINVAL);
    }
    let ep = epoll_of(p, epfd)?;
    if mask != 0 {
        super::signal::wait_with_mask(p, mask)?;
    }
    let deadline = (timeout_ms >= 0).then(|| time::uptime_ms() + timeout_ms as u64);
    loop {
        let gen = readiness::generation();
        let ready = ep.ready(p, max as usize, 0);
        if !ready.is_empty() {
            for (i, (ev, data)) in ready.iter().enumerate() {
                let mut raw = [0u8; 12];
                raw[0..4].copy_from_slice(&ev.to_le_bytes());
                raw[4..12].copy_from_slice(&data.to_le_bytes());
                user::put(events + i as u64 * 12, &raw)?;
            }
            return Ok(ready.len() as u64);
        }
        let now = time::uptime_ms();
        if deadline.is_some_and(|d| now >= d) {
            return Ok(0);
        }
        if crate::proc::interrupted() {
            return Err(EINTR);
        }
        let mut wait = deadline.map_or(1000, |d| d - now);
        if let Some(t) = ep.next_timer(p) {
            wait = wait.min(t.max(1));
        }
        readiness::wait(gen, &ep.wait_keys(p), wait.max(1));
    }
}
