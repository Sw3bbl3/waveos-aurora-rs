//! inotify: watching folders for files coming and going. SDL (when udev
//! isn't there) watches `/dev/input` this way for controllers plugged in.
//!
//! What is reported: in `/dev` and `/dev/input`, device nodes created
//! (`IN_CREATE`, then `IN_ATTRIB` as udev's permissions would follow) and
//! removed (`IN_DELETE`); on WaveOS's own file systems, files and folders
//! created, deleted and renamed (`IN_MOVED_FROM`/`IN_MOVED_TO`, with a
//! cookie). Changes to a file's contents (`IN_MODIFY`, `IN_CLOSE_WRITE`)
//! and opens and reads are not reported yet.

use super::chardev::{self, CharDev};
use crate::proc::readiness;
use crate::sync::{IrqMutex, Mutex};
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicU32, Ordering};

pub const IN_ATTRIB: u32 = 0x4;
pub const IN_MOVED_FROM: u32 = 0x40;
pub const IN_MOVED_TO: u32 = 0x80;
pub const IN_CREATE: u32 = 0x100;
pub const IN_DELETE: u32 = 0x200;
pub const IN_DELETE_SELF: u32 = 0x400;
const IN_Q_OVERFLOW: u32 = 0x4000;
const IN_IGNORED: u32 = 0x8000;
const IN_ONLYDIR: u32 = 0x0100_0000;
const IN_DONT_FOLLOW: u32 = 0x0200_0000;
const IN_MASK_CREATE: u32 = 0x1000_0000;
const IN_MASK_ADD: u32 = 0x2000_0000;
pub const IN_ISDIR: u32 = 0x4000_0000;
const IN_ONESHOT: u32 = 0x8000_0000;
const IN_ALL_EVENTS: u32 = 0xFFF;

/// Events a descriptor may have waiting (Linux's default `max_queued_events`).
const QUEUE: usize = 16384;

struct Watch {
    wd: i32,
    path: String,
    mask: u32,
}

pub struct Inotify {
    watches: Mutex<Vec<Watch>>,
    next_wd: AtomicU32,
    q: IrqMutex<VecDeque<Vec<u8>>>,
}

static ALL: IrqMutex<Vec<Weak<Inotify>>> = IrqMutex::new(Vec::new());
static COOKIE: AtomicU32 = AtomicU32::new(1);

/// `struct inotify_event`, its name NUL-padded to 16 bytes as Linux does.
fn record(wd: i32, mask: u32, cookie: u32, name: &str) -> Vec<u8> {
    let len = if name.is_empty() { 0 } else { (name.len() + 1).div_ceil(16) * 16 };
    let mut r = Vec::with_capacity(16 + len);
    r.extend_from_slice(&wd.to_le_bytes());
    r.extend_from_slice(&mask.to_le_bytes());
    r.extend_from_slice(&cookie.to_le_bytes());
    r.extend_from_slice(&(len as u32).to_le_bytes());
    r.extend_from_slice(name.as_bytes());
    r.resize(16 + len, 0);
    r
}

impl Inotify {
    pub fn new() -> Arc<Inotify> {
        let i = Arc::new(Inotify {
            watches: Mutex::new(Vec::new()),
            next_wd: AtomicU32::new(1),
            q: IrqMutex::new(VecDeque::new()),
        });
        let mut all = ALL.lock();
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&i));
        i
    }

    fn push(&self, r: Vec<u8>) {
        {
            let mut q = self.q.lock();
            if q.len() >= QUEUE {
                if q.len() == QUEUE {
                    q.push_back(record(-1, IN_Q_OVERFLOW, 0, ""));
                }
                return;
            }
            // Linux folds an event identical to the last one still unread.
            if q.back() == Some(&r) {
                return;
            }
            q.push_back(r);
        }
        readiness::notify(readiness::key(self));
    }

    /// `inotify_add_watch` on `path` (a resolved Linux path that exists).
    pub fn add(&self, path: &str, mask: u32, is_dir: bool) -> Result<i32, isize> {
        if mask & IN_ALL_EVENTS == 0 && mask & IN_MASK_ADD == 0 {
            return Err(EINVAL);
        }
        if mask & IN_ONLYDIR != 0 && !is_dir {
            return Err(ENOTDIR);
        }
        let mut w = self.watches.lock();
        if let Some(existing) = w.iter_mut().find(|w| w.path == path) {
            if mask & IN_MASK_CREATE != 0 {
                return Err(EEXIST);
            }
            existing.mask = if mask & IN_MASK_ADD != 0 { existing.mask | mask } else { mask };
            return Ok(existing.wd);
        }
        let wd = self.next_wd.fetch_add(1, Ordering::Relaxed) as i32;
        w.push(Watch { wd, path: String::from(path), mask });
        Ok(wd)
    }

    pub fn remove(&self, wd: i32) -> Result<(), isize> {
        let mut w = self.watches.lock();
        let i = w.iter().position(|w| w.wd == wd).ok_or(EINVAL)?;
        w.remove(i);
        drop(w);
        self.push(record(wd, IN_IGNORED, 0, ""));
        Ok(())
    }

    /// Something happened to `name` in folder `dir` (or to `dir` itself
    /// when `name` is empty).
    fn event(&self, dir: &str, name: &str, mask: u32, cookie: u32) {
        let mut out = Vec::new();
        {
            let mut w = self.watches.lock();
            let mut done = Vec::new();
            for watch in w.iter() {
                if watch.path == dir && watch.mask & mask & IN_ALL_EVENTS != 0 {
                    out.push(record(watch.wd, mask, cookie, name));
                    if watch.mask & IN_ONESHOT != 0 {
                        done.push(watch.wd);
                    }
                }
            }
            w.retain(|x| !done.contains(&x.wd));
            for wd in done {
                out.push(record(wd, IN_IGNORED, 0, ""));
            }
        }
        for r in out {
            self.push(r);
        }
    }
}

impl CharDev for Inotify {
    fn read(&self, out: &mut [u8], nonblock: bool) -> Result<usize, isize> {
        let data = chardev::wait(readiness::key(self), nonblock, || {
            let mut q = self.q.lock();
            let Some(first) = q.front() else { return Ok(None) };
            if first.len() > out.len() {
                return Err(EINVAL);
            }
            let mut data = Vec::new();
            while q.front().is_some_and(|r| data.len() + r.len() <= out.len()) {
                data.extend(q.pop_front().unwrap());
            }
            Ok(Some(data))
        })?;
        out[..data.len()].copy_from_slice(&data);
        Ok(data.len())
    }

    fn readiness(&self) -> (bool, bool, bool) {
        (!self.q.lock().is_empty(), false, false)
    }

    fn wait_key(&self) -> usize {
        readiness::key(self)
    }

    fn stat(&self) -> (u32, u64, Option<(u32, u32)>) {
        (0o600, 0, None)
    }

    fn available(&self) -> usize {
        self.q.lock().iter().map(|r| r.len()).sum()
    }
}

/// Tells every watcher of `path`'s folder (Linux path) that it changed.
pub fn notify(path: &str, mask: u32, cookie: u32) {
    let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
    let dir = if dir.is_empty() { "/" } else { dir };
    let all: Vec<Arc<Inotify>> = ALL.lock().iter().filter_map(|w| w.upgrade()).collect();
    for i in all {
        i.event(dir, name, mask, cookie);
        if mask & IN_DELETE != 0 {
            i.event(path, "", IN_DELETE_SELF, 0);
        }
    }
}

/// WaveOS's file systems: `host` (a WaveOS path) was created, removed or
/// renamed (`fs::index` tells).
pub fn host_changed(host: &str, mask: u32, dir: bool) {
    if ALL.lock().iter().all(|w| w.strong_count() == 0) {
        return;
    }
    let mask = if dir { mask | IN_ISDIR } else { mask };
    notify(&super::fs::to_guest(host), mask, 0);
}

/// A rename: `IN_MOVED_FROM` and `IN_MOVED_TO` sharing a cookie.
pub fn host_renamed(from: &str, to: &str) {
    if ALL.lock().iter().all(|w| w.strong_count() == 0) {
        return;
    }
    let cookie = COOKIE.fetch_add(1, Ordering::Relaxed);
    notify(&super::fs::to_guest(from), IN_MOVED_FROM, cookie);
    notify(&super::fs::to_guest(to), IN_MOVED_TO, cookie);
}

/// The calls.
pub fn init1(p: &crate::proc::Process, flags: u64) -> Result<u64, isize> {
    const IN_NONBLOCK: u64 = 0o4000;
    const IN_CLOEXEC: u64 = 0o2000000;
    if flags & !(IN_NONBLOCK | IN_CLOEXEC) != 0 {
        return Err(EINVAL);
    }
    let fd = super::fs::install_special(p, super::fs::Special::Inotify(Inotify::new()), flags & IN_CLOEXEC != 0)?;
    if let Some(l) = &p.linux {
        l.set_nonblock(fd as usize, flags & IN_NONBLOCK != 0);
    }
    Ok(fd)
}

fn of(p: &crate::proc::Process, fd: u64) -> Result<Arc<Inotify>, isize> {
    super::fs::inotify_of(p, fd).ok_or(EINVAL)
}

pub fn add_watch(p: &crate::proc::Process, fd: u64, path: u64, mask: u64) -> Result<u64, isize> {
    let i = of(p, fd)?;
    let mask = mask as u32;
    let guest = super::fs::path_arg(p, super::fs::AT_FDCWD, path)?;
    let guest = if mask & IN_DONT_FOLLOW != 0 { guest } else { super::fs::follow(&guest)? };
    let is_dir = super::fs::is_directory(p, &guest)?;
    i.add(&guest, mask, is_dir).map(|wd| wd as u64)
}

pub fn rm_watch(p: &crate::proc::Process, fd: u64, wd: u64) -> Result<u64, isize> {
    of(p, fd)?.remove(wd as i32).map(|_| 0)
}
