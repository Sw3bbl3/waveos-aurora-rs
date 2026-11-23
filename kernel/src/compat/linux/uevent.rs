//! `NETLINK_KOBJECT_UEVENT` sockets: how Linux programs hear of devices
//! coming and going. A socket bound to group 1 gets the kernel's messages
//! (`add@/devices/…`, then `KEY=value` strings); group 2 gets udev's (a
//! `libudev` header, then the properties udev's rules add), which the
//! kernel sends here itself since there is no udev daemon. Messages come
//! from port 0 with the system's credentials, as libudev requires.

use crate::proc::readiness;
use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

pub const AF_NETLINK: u64 = 16;
pub const NETLINK_KOBJECT_UEVENT: u64 = 15;
pub const GROUP_KERNEL: u32 = 1;
pub const GROUP_UDEV: u32 = 2;

/// Bytes a socket may have waiting (`SO_RCVBUF`); past it, messages are dropped.
const RCVBUF: usize = 256 * 1024;

pub struct Socket {
    pub groups: AtomicU32,
    pub port: AtomicU32,
    pub passcred: AtomicBool,
    q: IrqMutex<VecDeque<(u32, Vec<u8>)>>,
}

static SOCKETS: IrqMutex<Vec<Weak<Socket>>> = IrqMutex::new(Vec::new());
static SEQNUM: AtomicU64 = AtomicU64::new(1);
static NEXT_PORT: AtomicU32 = AtomicU32::new(0x4000_0000);

impl Socket {
    pub fn new() -> Arc<Socket> {
        let s = Arc::new(Socket {
            groups: AtomicU32::new(0),
            port: AtomicU32::new(0),
            passcred: AtomicBool::new(false),
            q: IrqMutex::new(VecDeque::new()),
        });
        let mut all = SOCKETS.lock();
        all.retain(|w| w.strong_count() > 0);
        all.push(Arc::downgrade(&s));
        s
    }

    pub fn key(&self) -> usize {
        readiness::key(self)
    }

    /// `bind`: the groups to hear, and the port (0: one is chosen, the
    /// process id if free).
    pub fn bind(&self, pid: u32, port: u32, groups: u32) -> Result<(), isize> {
        let port = if port != 0 {
            port
        } else if !SOCKETS.lock().iter().filter_map(|w| w.upgrade()).any(|s| s.port.load(Ordering::Relaxed) == pid) {
            pid
        } else {
            NEXT_PORT.fetch_add(1, Ordering::Relaxed)
        };
        self.port.store(port, Ordering::Relaxed);
        self.groups.store(groups, Ordering::Relaxed);
        Ok(())
    }

    pub fn readable(&self) -> bool {
        !self.q.lock().is_empty()
    }

    /// The next message and the group it was sent to (`peek` leaves it).
    pub fn recv(&self, nonblock: bool, peek: bool) -> Result<(u32, Vec<u8>), isize> {
        super::chardev::wait(self.key(), nonblock, || {
            let mut q = self.q.lock();
            Ok(if peek { q.front().cloned() } else { q.pop_front() })
        })
    }

    pub fn available(&self) -> usize {
        self.q.lock().front().map_or(0, |m| m.1.len())
    }
}

/// Sends `msg` to every socket listening to `group`.
fn broadcast(group: u32, msg: &[u8]) {
    let all: Vec<Arc<Socket>> = SOCKETS.lock().iter().filter_map(|w| w.upgrade()).collect();
    for s in all {
        if s.groups.load(Ordering::Relaxed) & (1 << (group - 1)) == 0 {
            continue;
        }
        {
            let mut q = s.q.lock();
            let queued: usize = q.iter().map(|m| m.1.len()).sum();
            if queued + msg.len() > RCVBUF {
                continue;
            }
            q.push_back((group, msg.to_vec()));
        }
        readiness::notify(s.key());
    }
}

/// A device event to both groups (`kernel` and `udev`: the two forms).
pub fn send(m: &super::input::udev::Message) {
    let seq = SEQNUM.fetch_add(1, Ordering::Relaxed);
    broadcast(GROUP_KERNEL, &m.kernel(seq));
    broadcast(GROUP_UDEV, &m.udev(seq));
}

/// `struct sockaddr_nl` from `port` to `groups`.
pub fn sockaddr(port: u32, groups: u32) -> [u8; 12] {
    let mut a = [0u8; 12];
    a[0..2].copy_from_slice(&(AF_NETLINK as u16).to_le_bytes());
    a[4..8].copy_from_slice(&port.to_le_bytes());
    a[8..12].copy_from_slice(&groups.to_le_bytes());
    a
}

/// Reads a `struct sockaddr_nl`: (port, groups).
pub fn read_sockaddr(raw: &[u8]) -> Result<(u32, u32), isize> {
    if raw.len() < 12 || u16::from_le_bytes([raw[0], raw[1]]) as u64 != AF_NETLINK {
        return Err(EINVAL);
    }
    Ok((u32::from_le_bytes(raw[4..8].try_into().unwrap()), u32::from_le_bytes(raw[8..12].try_into().unwrap())))
}
