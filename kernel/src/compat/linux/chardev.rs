//! Character devices for Linux programs: an open node of `/dev` that reads,
//! writes and answers `ioctl`s its own way (`/dev/input/event*`, `js*`,
//! `/dev/hidraw*`, `/dev/uinput`), and inotify descriptors. The file
//! layer ([`super::fs`]) hands such a descriptor's calls to its
//! [`CharDev`], outside the descriptor's lock, so a read may block.

use crate::proc::readiness;
use aurora_abi::err::*;

pub trait CharDev: Send + Sync {
    /// Reads into `out`; waits for data unless `nonblock`.
    fn read(&self, out: &mut [u8], nonblock: bool) -> Result<usize, isize>;

    fn write(&self, _data: &[u8], _nonblock: bool) -> Result<usize, isize> {
        Err(EINVAL)
    }

    fn ioctl(&self, _request: u32, _arg: u64) -> Result<u64, isize> {
        Err(ENOTTY)
    }

    /// Readable, writable, and hung up (the device is gone).
    fn readiness(&self) -> (bool, bool, bool);

    /// What `poll` and blocking reads wait on (`proc::readiness`).
    fn wait_key(&self) -> usize;

    /// `st_mode` (type and permissions), `st_rdev`, and the owner (None:
    /// the caller).
    fn stat(&self) -> (u32, u64, Option<(u32, u32)>);

    /// Bytes waiting (`FIONREAD`).
    fn available(&self) -> usize {
        0
    }
}

/// Waits until `take` gives something (it is tried first), for a blocking
/// read: `EAGAIN` at once if `nonblock`, `EINTR` on a signal.
pub fn wait<R>(key: usize, nonblock: bool, mut take: impl FnMut() -> Result<Option<R>, isize>) -> Result<R, isize> {
    loop {
        let watch = (!nonblock).then(|| readiness::Watch::new(&[key]));
        if let Some(r) = take()? {
            return Ok(r);
        }
        if nonblock {
            return Err(EAGAIN);
        }
        if crate::proc::interrupted() {
            return Err(EINTR);
        }
        watch.unwrap().wait(1000);
    }
}

/// Linux's `_IOC` fields of an `ioctl` request.
pub fn ioc_dir(r: u32) -> u32 {
    r >> 30
}
pub fn ioc_type(r: u32) -> u32 {
    (r >> 8) & 0xFF
}
pub fn ioc_nr(r: u32) -> u32 {
    r & 0xFF
}
pub fn ioc_size(r: u32) -> usize {
    ((r >> 16) & 0x3FFF) as usize
}

/// Copies `s` and its NUL (at most `max` bytes) to `ptr`, as Linux's
/// string `ioctl`s do; returns the bytes copied.
pub fn put_str(ptr: u64, max: usize, s: &str) -> Result<u64, isize> {
    let mut b = alloc::vec::Vec::from(s.as_bytes());
    b.push(0);
    b.truncate(max);
    crate::syscall::user::put_bytes(ptr, &b)?;
    Ok(b.len() as u64)
}
