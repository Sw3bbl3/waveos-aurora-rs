//! Files for Linux programs: the Linux-shaped file tree, virtual files in
//! `/dev`, `/proc` and `/etc`, and the file system calls.
//!
//! The process's working directory is kept as a WaveOS path; Linux programs
//! see it through [`to_guest`].

use crate::fs::{self, Kind};
use crate::proc::{Handle, Process};
use crate::sync::Mutex;
use crate::syscall::user;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;

pub const AT_FDCWD: u64 = -100i64 as u64;
pub const AT_REMOVEDIR: u64 = 0x200;
const AT_EMPTY_PATH: u64 = 0x1000;
pub const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const O_NOFOLLOW: u64 = 0o400000;
pub const O_CLOEXEC: u64 = 0o2000000;

pub const O_WRONLY: u64 = 1;
const O_RDWR: u64 = 2;
pub const O_CREAT: u64 = 0o100;
const O_EXCL: u64 = 0o200;
pub const O_TRUNC: u64 = 0o1000;
const O_APPEND: u64 = 0o2000;
const O_NONBLOCK: u64 = 0o4000;
const O_DIRECTORY: u64 = 0o200000;

/// The read-only Linux tree (from the system image) and the writable one.
pub const SYSTEM_ROOT: &str = "/System/Linux";
pub const WRITABLE_ROOT: &str = "/Linux";
/// Top-level Linux folders that come from the system image.
const SYSTEM_DIRS: [&str; 7] = ["bin", "etc", "lib", "lib64", "opt", "sbin", "usr"];
/// Top-level Linux folders kept on the writable volume.
const WRITABLE_DIRS: [&str; 8] = ["home", "media", "mnt", "root", "run", "srv", "tmp", "var"];

/// True if `guest` is in one of the system folders (`/usr`, `/lib`, `/etc`…).
fn in_system_dirs(guest: &str) -> bool {
    SYSTEM_DIRS.contains(&guest.trim_start_matches('/').split('/').next().unwrap_or(""))
}

/// A WaveOS path for the Linux path `guest` (absolute, normalised).
///
/// The system folders are layered: Linux packages installed on the disk
/// (`/Linux/usr`, …) come first, then the system image (`/System/Linux`).
/// Something new is made on the disk when its folder is there.
pub fn to_host(guest: &str) -> String {
    let first = guest.trim_start_matches('/').split('/').next().unwrap_or("");
    if SYSTEM_DIRS.contains(&first) {
        let disk = format!("{WRITABLE_ROOT}{guest}");
        if fs::stat(&disk).is_ok() {
            return disk;
        }
        let image = format!("{SYSTEM_ROOT}{guest}");
        if fs::stat(&image).is_err() && fs::is_dir(&fs::parent_path(&disk)) {
            return disk;
        }
        image
    } else if WRITABLE_DIRS.contains(&first) {
        format!("{WRITABLE_ROOT}{guest}")
    } else {
        String::from(guest)
    }
}

/// The Linux path for the WaveOS path `host`.
pub fn to_guest(host: &str) -> String {
    for (root, dirs) in
        [(SYSTEM_ROOT, &SYSTEM_DIRS[..]), (WRITABLE_ROOT, &WRITABLE_DIRS[..]), (WRITABLE_ROOT, &SYSTEM_DIRS[..])]
    {
        if let Some(rest) = host.strip_prefix(root) {
            let first = rest.trim_start_matches('/').split('/').next().unwrap_or("");
            if rest.starts_with('/') && dirs.contains(&first) {
                return String::from(rest);
            }
        }
    }
    String::from(host)
}

/// Makes the writable Linux folders (once per boot is enough; cheap after).
pub fn prepare_root(user: &str) {
    for dir in ["", "/home", "/tmp", "/var", "/var/tmp", "/run", "/root"] {
        let path = format!("{WRITABLE_ROOT}{dir}");
        if !fs::is_dir(&path) {
            let _ = fs::mkdir(&path);
        }
    }
    let home = format!("{WRITABLE_ROOT}/home/{user}");
    if !fs::is_dir(&home) {
        let _ = fs::mkdir(&home);
    }
}

/// An open object only Linux programs have: a directory, a generated file,
/// or a device.
pub enum Special {
    /// A directory (`getdents64`), by WaveOS path; listed when first read.
    Dir {
        host: String,
        entries: Option<Vec<(String, u8)>>,
        pos: usize,
    },
    /// A file whose contents are made up when opened (`/proc`, `/etc/passwd`).
    Mem {
        data: Vec<u8>,
        pos: usize,
    },
    Null,
    Zero,
    Random,
    /// `eventfd`, `timerfd` and `epoll` descriptors (see [`super::events`]).
    Event(Arc<super::events::EventFd>),
    Timer(Arc<super::events::TimerFd>),
    Epoll(Arc<super::events::Epoll>),
    /// A local (`AF_UNIX`) socket.
    Unix(Arc<super::unix::Socket>),
    /// Shared memory (`memfd_create`, `/dev/shm`), and where reads are.
    Shm {
        file: Arc<super::shm::ShmFile>,
        pos: u64,
    },
}

impl Special {
    pub fn read(&mut self, out: &mut [u8]) -> Result<usize, isize> {
        match self {
            Special::Event(e) => e.read(out),
            Special::Timer(t) => t.read(out),
            Special::Epoll(_) => Err(EINVAL),
            Special::Unix(u) => u.recv(out, false, false).map(|r| r.len),
            Special::Shm { file, pos } => {
                let n = file.read_at(*pos, out);
                *pos += n as u64;
                Ok(n)
            }
            Special::Dir { .. } => Err(EISDIR),
            Special::Mem { data, pos } => {
                let n = out.len().min(data.len().saturating_sub(*pos));
                out[..n].copy_from_slice(&data[*pos..*pos + n]);
                *pos += n;
                Ok(n)
            }
            Special::Null => Ok(0),
            Special::Zero => {
                out.fill(0);
                Ok(out.len())
            }
            Special::Random => {
                crate::random::fill(out);
                Ok(out.len())
            }
        }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, isize> {
        match self {
            Special::Dir { .. } => Err(EISDIR),
            Special::Mem { .. } => Err(EACCES),
            Special::Event(e) => e.write(data),
            Special::Timer(_) | Special::Epoll(_) => Err(EINVAL),
            Special::Unix(u) => u.send(data, Vec::new(), None, false),
            Special::Shm { file, pos } => {
                let n = file.write_at(*pos, data)?;
                *pos += n as u64;
                Ok(n)
            }
            _ => Ok(data.len()),
        }
    }

    /// `st_mode` and `st_rdev`.
    fn mode(&self) -> (u32, u64) {
        match self {
            Special::Dir { .. } => (S_IFDIR | 0o755, 0),
            Special::Mem { .. } => (S_IFREG | 0o444, 0),
            Special::Null => (S_IFCHR | 0o666, makedev(1, 3)),
            Special::Zero => (S_IFCHR | 0o666, makedev(1, 5)),
            Special::Random => (S_IFCHR | 0o666, makedev(1, 9)),
            // Anonymous inodes on Linux.
            Special::Event(_) | Special::Timer(_) | Special::Epoll(_) => (0o600, 0),
            Special::Unix(_) => (S_IFSOCK | 0o777, 0),
            Special::Shm { .. } => (S_IFREG | 0o600, 0),
        }
    }
}

const S_IFIFO: u32 = 0o010000;
const S_IFCHR: u32 = 0o020000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFSOCK: u32 = 0o140000;

fn makedev(major: u64, minor: u64) -> u64 {
    (major << 8) | minor
}

fn special(s: Special) -> Arc<Handle> {
    Arc::new(Handle::Special(Mutex::new(s)))
}

fn mem(text: String) -> Special {
    Special::Mem { data: text.into_bytes(), pos: 0 }
}

// ------------------------------------------------------------------ paths

/// The Linux path of the directory `dirfd` stands for (`AT_FDCWD`: the cwd).
fn base_dir(p: &Process, dirfd: u64) -> Result<String, isize> {
    if dirfd == AT_FDCWD || dirfd as i32 == -100 {
        return Ok(to_guest(&p.cwd.lock()));
    }
    match &*p.fd(dirfd as usize)? {
        Handle::Special(s) => match &*s.lock() {
            Special::Dir { host, .. } => Ok(to_guest(host)),
            _ => Err(ENOTDIR),
        },
        _ => Err(ENOTDIR),
    }
}

/// The absolute Linux path for `raw` relative to `dirfd`.
fn guest_path(p: &Process, dirfd: u64, raw: &str) -> Result<String, isize> {
    if raw.is_empty() {
        return Err(ENOENT);
    }
    let base = if raw.starts_with('/') { String::from("/") } else { base_dir(p, dirfd)? };
    let mut path = super::links::resolve(&fs::resolve(&base, raw), false)?;
    // A trailing slash means the folder itself (follow a link to it).
    if raw.ends_with('/') && path != "/" {
        path = follow(&path)?;
    }
    Ok(path)
}

/// `guest` with a link in its last component followed too.
pub fn follow(guest: &str) -> Result<String, isize> {
    super::links::resolve(guest, true)
}

/// The WaveOS path a Linux path finally leads to.
pub fn resolve_host(guest: &str) -> Result<String, isize> {
    Ok(to_host(&follow(guest)?))
}

/// `path` made absolute against the caller's working folder.
pub fn absolute(p: &Process, path: &str) -> String {
    if path.starts_with('/') {
        String::from(path)
    } else {
        fs::resolve(&to_guest(&p.cwd.lock()), path)
    }
}

pub fn path_arg(p: &Process, dirfd: u64, ptr: u64) -> Result<String, isize> {
    guest_path(p, dirfd, &user::cstr(ptr)?)
}

/// Files that exist only for Linux programs.
fn virtual_file(p: &Process, guest: &str) -> Option<Result<Special, isize>> {
    let user = account_user();
    Some(Ok(match guest {
        "/dev" | "/proc" | "/proc/self" | "/dev/shm" => {
            Special::Dir { host: String::from(guest), entries: None, pos: 0 }
        }
        "/dev/null" => Special::Null,
        "/dev/zero" => Special::Zero,
        "/dev/random" | "/dev/urandom" => Special::Random,
        "/dev/tty" | "/dev/console" => return Some(Err(ENXIO)),
        "/proc/cpuinfo" => mem(cpuinfo()),
        "/proc/meminfo" => mem(meminfo()),
        "/proc/self/maps" => mem(super::mm::maps(p)),
        "/proc/self/cmdline" => mem(format!("{}\0", p.name())),
        "/proc/self/comm" | "/proc/self/task/self/comm" => mem(format!("{}\n", p.name())),
        "/proc/self/stat" => mem(format!(
            "{} ({}) R {} {} {} 0 -1 0\n",
            p.pid,
            p.name(),
            p.parent,
            p.pgid.load(core::sync::atomic::Ordering::Relaxed),
            p.sid.load(core::sync::atomic::Ordering::Relaxed)
        )),
        "/proc/mounts" | "/proc/self/mounts" | "/etc/mtab" => mem(mounts()),
        "/proc/version" => mem(String::from("Linux version 6.12.0-waveos (WaveOS Aurora)\n")),
        "/proc/sys/kernel/osrelease" => mem(String::from("6.12.0-waveos\n")),
        "/etc/passwd" => mem(format!(
            "root:x:0:0:root:/root:/bin/sh\n{user}:x:{uid}:{uid}:{user}:/home/{user}:/bin/sh\n",
            uid = super::UID
        )),
        "/etc/group" => mem(format!("root:x:0:\n{user}:x:{uid}:\n", uid = super::UID)),
        "/etc/hostname" => mem(String::from("aurora\n")),
        "/etc/hosts" => mem(String::from("127.0.0.1 localhost aurora\n::1 localhost aurora\n")),
        "/etc/resolv.conf" => mem(resolv_conf()),
        _ => return None,
    }))
}

/// The user name Linux programs see ("alex" for Alex Rivera).
pub fn account_user() -> String {
    let full = crate::gui::prefs::get("account_name").unwrap_or_default();
    let first: String = full.split_whitespace().next().unwrap_or("user").to_lowercase();
    let name: String = first.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    if name.is_empty() {
        String::from("user")
    } else {
        name
    }
}

fn cpuinfo() -> String {
    let n = crate::arch::percpu::count();
    let mut s = String::new();
    for i in 0..n {
        s.push_str(&format!(
            "processor\t: {i}\nvendor_id\t: GenuineIntel\nmodel name\t: x86-64 processor\nflags\t\t: fpu sse sse2\n\n"
        ));
    }
    s
}

fn meminfo() -> String {
    let m = crate::mm::stats();
    let kib = |b: u64| b / 1024;
    format!(
        "MemTotal:       {} kB\nMemFree:        {} kB\nMemAvailable:   {} kB\n",
        kib(m.total_bytes),
        kib(m.total_bytes - m.used_bytes),
        kib(m.total_bytes - m.used_bytes)
    )
}

fn mounts() -> String {
    let mut s = String::new();
    for (path, _, _) in fs::mounts() {
        let kind = if path == "/Boot" { "vfat" } else { "aurorafs" };
        s.push_str(&format!("{kind} {} {kind} rw 0 0\n", to_guest(&path)));
    }
    s
}

fn resolv_conf() -> String {
    let mut s = String::new();
    for i in crate::net::interfaces() {
        for d in i.dns.iter().filter(|d| **d != [0; 4]) {
            s.push_str(&format!("nameserver {}.{}.{}.{}\n", d[0], d[1], d[2], d[3]));
        }
    }
    if s.is_empty() {
        s.push_str("nameserver 10.0.2.3\n");
    }
    s
}

// ------------------------------------------------------------------ files

/// Read and write share WaveOS's own (they know every kind of handle).
pub fn read(p: &Process, fd: u64, ptr: u64, len: u64) -> Result<u64, isize> {
    // Event and timer reads may block: not while holding the descriptor's lock.
    if let Handle::Special(s) = &*p.fd(fd as usize)? {
        let unix = match &*s.lock() {
            Special::Unix(u) => Some(u.clone()),
            _ => None,
        };
        if let Some(u) = unix {
            let nb = p.linux.as_ref().is_some_and(|l| l.nonblock(fd as usize));
            let mut out = user::slice_mut(ptr, len)?;
            let r = u.recv(&mut out, false, nb)?;
            out.commit(r.len)?;
            return Ok(r.len as u64);
        }
        let blocking = match &*s.lock() {
            Special::Event(e) => Some((Some(e.clone()), None)),
            Special::Timer(t) => Some((None, Some(t.clone()))),
            _ => None,
        };
        if let Some((e, t)) = blocking {
            let mut out = user::slice_mut(ptr, len)?;
            let n = match (e, t) {
                (Some(e), _) => e.read(&mut out)?,
                (_, Some(t)) => t.read(&mut out)?,
                _ => 0,
            };
            out.commit(n)?;
            return Ok(n as u64);
        }
    }
    if nonblocking(p, fd) {
        if let Handle::PipeRead { pipe, .. } = &*p.fd(fd as usize)? {
            if !pipe.readable() {
                return Err(EAGAIN);
            }
        }
    }
    match crate::syscall::read(fd as usize, ptr, len) {
        // A blocking read cut short by a signal.
        Err(EAGAIN) if crate::proc::interrupted() && !nonblocking(p, fd) => Err(EINTR),
        r => r,
    }
}

pub fn write(p: &Process, fd: u64, ptr: u64, len: u64) -> Result<u64, isize> {
    if let Handle::Special(s) = &*p.fd(fd as usize)? {
        let (event, unix) = match &*s.lock() {
            Special::Event(e) => (Some(e.clone()), None),
            Special::Unix(u) => (None, Some(u.clone())),
            _ => (None, None),
        };
        if let Some(e) = event {
            let data = user::slice(ptr, len)?;
            return e.write(&data).map(|n| n as u64);
        }
        if let Some(u) = unix {
            let data = user::slice(ptr, len)?;
            let nb = p.linux.as_ref().is_some_and(|l| l.nonblock(fd as usize));
            let r = u.send(&data, Vec::new(), None, nb).map(|n| n as u64);
            if r == Err(EPIPE) {
                let info = super::signal::Info { pid: p.pid, ..Default::default() };
                super::signal::send(p, super::signal::SIGPIPE, info, Some(crate::sched::current_id()));
            }
            return r;
        }
    }
    if p.linux.as_ref().is_some_and(|l| l.nonblock(fd as usize)) {
        if let Handle::PipeWrite(pipe) = &*p.fd(fd as usize)? {
            if !pipe.writable() {
                return Err(EAGAIN);
            }
        }
    }
    let r = crate::syscall::write(fd as usize, ptr, len);
    if r == Err(EPIPE) {
        // Nobody reads the pipe any more: SIGPIPE (which ends the program unless handled).
        let info = super::signal::Info { code: super::signal::SI_USER, pid: p.pid, ..Default::default() };
        super::signal::send(p, super::signal::SIGPIPE, info, Some(crate::sched::current_id()));
    }
    r
}

fn nonblocking(p: &Process, fd: u64) -> bool {
    p.linux.as_ref().is_some_and(|l| l.nonblock(fd as usize))
        || p.fd(fd as usize).is_ok_and(|h| matches!(&*h, Handle::PipeRead { nonblocking: true, .. }))
}

/// Whether descriptor `fd` could be read and written without blocking, and
/// whether its other end has gone; `None` if it isn't open. (`depth`
/// bounds epoll sets watching epoll sets.)
pub fn readiness_of(p: &Process, fd: usize, depth: u32) -> Option<(bool, bool, bool)> {
    let h = p.fd(fd).ok()?;
    Some(match &*h {
        Handle::PipeRead { pipe, .. } => (pipe.readable(), false, false),
        Handle::PipeWrite(pipe) => (false, pipe.writable(), false),
        Handle::Special(s) => {
            enum Inner {
                Ready(bool, bool),
                Epoll(Arc<super::events::Epoll>),
                Unix(Arc<super::unix::Socket>),
            }
            let inner = match &*s.lock() {
                Special::Event(e) => Inner::Ready(e.readable(), e.writable()),
                Special::Timer(t) => Inner::Ready(t.readable(), false),
                Special::Epoll(e) => Inner::Epoll(e.clone()),
                Special::Unix(u) => Inner::Unix(u.clone()),
                _ => Inner::Ready(true, true),
            };
            match inner {
                Inner::Ready(rd, wr) => (rd, wr, false),
                Inner::Epoll(ep) => (ep.readable(p, depth), false, false),
                Inner::Unix(u) => u.readiness(),
            }
        }
        Handle::Socket(sock) => sock.readiness(),
        _ => (true, true, false),
    })
}

/// Adds to `keys` what to wait on for a change in descriptor `fd`'s
/// readiness (see `proc::readiness`).
pub fn wait_keys(p: &Process, fd: usize, keys: &mut Vec<usize>) {
    use crate::proc::readiness::{key, ANY, NET};
    let Ok(h) = p.fd(fd) else { return };
    match &*h {
        Handle::PipeRead { pipe, .. } | Handle::PipeWrite(pipe) => keys.push(key(Arc::as_ptr(pipe))),
        Handle::Special(s) => match &*s.lock() {
            Special::Event(e) => keys.push(key(Arc::as_ptr(e))),
            Special::Timer(t) => keys.push(key(Arc::as_ptr(t))),
            // A set inside the set: any change.
            Special::Epoll(_) => keys.push(ANY),
            Special::Unix(u) => u.wait_keys(keys),
            _ => {}
        },
        Handle::Socket(_) => keys.push(NET),
        // The rest is always ready.
        _ => {}
    }
}

/// When the soonest timer among `fds` expires (ms from now).
fn next_timer(p: &Process, fds: impl Iterator<Item = usize>) -> Option<u64> {
    fds.filter_map(|fd| match &*p.fd(fd).ok()? {
        Handle::Special(s) => match &*s.lock() {
            Special::Timer(t) => t.due_in(),
            _ => None,
        },
        _ => None,
    })
    .min()
}

pub fn openat(p: &Process, dirfd: u64, ptr: u64, flags: u64, _mode: u64) -> Result<u64, isize> {
    let guest = path_arg(p, dirfd, ptr)?;
    if super::links::target(&guest).is_some() && flags & O_NOFOLLOW != 0 {
        return Err(ELOOP);
    }
    let handle = open_guest(p, &follow(&guest)?, flags)?;
    install(p, handle, flags & O_CLOEXEC != 0)
}

/// Installs a new made-up descriptor (event, timer, epoll).
pub fn install_special(p: &Process, s: Special, cloexec: bool) -> Result<u64, isize> {
    install(p, special(s), cloexec)
}

/// Installs a new descriptor, close-on-exec or not.
fn install(p: &Process, h: Arc<Handle>, cloexec: bool) -> Result<u64, isize> {
    let fd = p.install_fd(h)?;
    if let Some(l) = &p.linux {
        l.set_cloexec(fd, cloexec);
        l.set_nonblock(fd, false);
    }
    Ok(fd as u64)
}

/// [`install`] for other modules (sockets, received descriptors).
pub fn install_handle(p: &Process, h: Arc<Handle>, cloexec: bool) -> Result<u64, isize> {
    install(p, h, cloexec)
}

/// Opens a Linux path as a handle.
pub fn open_guest(p: &Process, guest: &str, flags: u64) -> Result<Arc<Handle>, isize> {
    let fd_alias = guest.strip_prefix("/dev/fd/").or_else(|| guest.strip_prefix("/proc/self/fd/"));
    let std_alias = match guest {
        "/dev/stdin" => Some(0),
        "/dev/stdout" => Some(1),
        "/dev/stderr" => Some(2),
        _ => fd_alias.and_then(|n| n.parse::<usize>().ok()),
    };
    if let Some(n) = std_alias {
        return p.fd(n);
    }
    // The kernel log (what WaveOS's system log and serial port show), as
    // on Linux: `2>/dev/kmsg` keeps a program's messages.
    if guest == "/dev/kmsg" {
        return Ok(Arc::new(Handle::Log));
    }
    // OSS sound, which GStreamer and SDL can play through: a WaveOS sound
    // stream (`ENODEV` without a sound device).
    if guest == "/dev/dsp" {
        return crate::drivers::audio::open().map(|s| Arc::new(Handle::Audio(s)));
    }
    if let Some(name) = guest.strip_prefix("/dev/shm/") {
        let file = super::shm::open(name, flags & O_CREAT != 0, flags & O_EXCL != 0, flags & O_TRUNC != 0)?;
        return Ok(special(Special::Shm { file, pos: 0 }));
    }
    if let Some(v) = virtual_file(p, guest) {
        return v.map(special);
    }
    let host = to_host(guest);
    let writes = flags & 3 != 0 || flags & (O_TRUNC | O_APPEND) != 0;
    if fs::is_dir(&host) {
        if writes {
            return Err(EISDIR);
        }
        return Ok(special(Special::Dir { host, entries: None, pos: 0 }));
    }
    if flags & O_DIRECTORY != 0 {
        return Err(if fs::stat(&host).is_ok() { ENOTDIR } else { ENOENT });
    }
    use aurora_abi::open::*;
    let mut f = match flags & 3 {
        0 => READ,
        1 => WRITE,
        _ => READ | WRITE,
    };
    if flags & O_CREAT != 0 {
        f |= CREATE;
    }
    if flags & O_EXCL != 0 {
        f |= EXCLUSIVE;
    }
    if flags & O_TRUNC != 0 {
        f |= TRUNCATE;
    }
    if flags & O_APPEND != 0 {
        f |= APPEND;
    }
    let file = fs::open(&host, f)?;
    Ok(Arc::new(Handle::File(Mutex::new(file))))
}

pub fn close(p: &Process, fd: u64) -> Result<u64, isize> {
    let old = {
        let mut fds = p.fds.lock();
        match fds.get_mut(fd as usize) {
            Some(slot @ Some(_)) => slot.take(),
            _ => return Err(EBADF),
        }
    };
    if let Some(l) = &p.linux {
        l.set_cloexec(fd as usize, false);
        l.set_nonblock(fd as usize, false);
    }
    // The handle goes after the lock (a pipe end wakes its peer).
    drop(old);
    Ok(0)
}

pub fn lseek(p: &Process, fd: u64, offset: i64, whence: u64) -> Result<u64, isize> {
    match &*p.fd(fd as usize)? {
        Handle::File(f) => f.lock().seek(offset, whence as usize),
        Handle::Special(s) => match &mut *s.lock() {
            Special::Dir { pos, .. } if offset == 0 && whence == 0 => {
                *pos = 0;
                Ok(0)
            }
            Special::Mem { data, pos } => {
                let base = match whence {
                    0 => 0,
                    1 => *pos as i64,
                    2 => data.len() as i64,
                    _ => return Err(EINVAL),
                };
                let new = base.checked_add(offset).filter(|&n| n >= 0).ok_or(EINVAL)?;
                *pos = new as usize;
                Ok(new as u64)
            }
            Special::Shm { file, pos } => {
                let base = match whence {
                    0 => 0,
                    1 => *pos as i64,
                    2 => file.size() as i64,
                    _ => return Err(EINVAL),
                };
                let new = base.checked_add(offset).filter(|&n| n >= 0).ok_or(EINVAL)?;
                *pos = new as u64;
                Ok(new as u64)
            }
            Special::Dir { .. } => Err(EINVAL),
            _ => Ok(0),
        },
        _ => Err(ESPIPE),
    }
}

/// The shared memory object behind descriptor `fd`, if it is one.
pub fn shm_of(p: &Process, fd: u64) -> Option<Arc<super::shm::ShmFile>> {
    match &*p.fd(fd as usize).ok()? {
        Handle::Special(s) => match &*s.lock() {
            Special::Shm { file, .. } => Some(file.clone()),
            _ => None,
        },
        _ => None,
    }
}

/// `memfd_create`.
pub fn memfd_create(p: &Process, _name: u64, flags: u64) -> Result<u64, isize> {
    let file = super::shm::ShmFile::new(0)?;
    install(p, special(Special::Shm { file, pos: 0 }), flags & 1 != 0)
}

fn at_offset(
    p: &Process,
    fd: u64,
    offset: u64,
    f: impl FnOnce(&mut fs::OpenFile) -> Result<usize, isize>,
) -> Result<u64, isize> {
    match &*p.fd(fd as usize)? {
        Handle::File(file) => {
            let mut file = file.lock();
            let saved = file.pos;
            file.pos = offset;
            let r = f(&mut file);
            file.pos = saved;
            r.map(|n| n as u64)
        }
        _ => Err(ESPIPE),
    }
}

pub fn pread(p: &Process, fd: u64, ptr: u64, len: u64, offset: u64) -> Result<u64, isize> {
    if let Some(shm) = shm_of(p, fd) {
        let mut buf = user::slice_mut(ptr, len)?;
        let n = shm.read_at(offset, &mut buf);
        buf.commit(n)?;
        return Ok(n as u64);
    }
    let mut buf = user::slice_mut(ptr, len)?;
    let n = at_offset(p, fd, offset, |f| f.read(&mut buf))?;
    buf.commit(n as usize)?;
    Ok(n)
}

pub fn pwrite(p: &Process, fd: u64, ptr: u64, len: u64, offset: u64) -> Result<u64, isize> {
    if let Some(shm) = shm_of(p, fd) {
        let data = user::slice(ptr, len)?;
        return shm.write_at(offset, &data).map(|n| n as u64);
    }
    let buf = user::slice(ptr, len)?;
    at_offset(p, fd, offset, |f| f.write(&buf))
}

/// `readv`/`writev`: one transfer per `iovec`, stopping at a short one.
fn vectored(iov: u64, count: u64, mut each: impl FnMut(u64, u64) -> Result<u64, isize>) -> Result<u64, isize> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let mut total = 0u64;
    for i in 0..count {
        let [base, len]: [u64; 2] = user::get(iov + i * 16)?;
        if len == 0 {
            continue;
        }
        match each(base, len) {
            Ok(n) => {
                total += n;
                if n < len {
                    break;
                }
            }
            Err(e) if total == 0 => return Err(e),
            Err(_) => break,
        }
    }
    Ok(total)
}

pub fn readv(p: &Process, fd: u64, iov: u64, count: u64) -> Result<u64, isize> {
    vectored(iov, count, |b, l| read(p, fd, b, l))
}

pub fn writev(p: &Process, fd: u64, iov: u64, count: u64) -> Result<u64, isize> {
    vectored(iov, count, |b, l| write(p, fd, b, l))
}

pub fn getdents64(p: &Process, fd: u64, ptr: u64, len: u64) -> Result<u64, isize> {
    let h = p.fd(fd as usize)?;
    let Handle::Special(s) = &*h else { return Err(ENOTDIR) };
    let mut s = s.lock();
    let Special::Dir { host, entries, pos } = &mut *s else { return Err(ENOTDIR) };
    if entries.is_none() {
        let mut list = alloc::vec![(String::from("."), DT_DIR), (String::from(".."), DT_DIR)];
        list.extend(dir_entries(host));
        *entries = Some(list);
    }
    let entries = entries.as_ref().unwrap();
    let mut out = user::slice_mut(ptr, len)?;
    let mut used = 0usize;
    while let Some((name, kind)) = entries.get(*pos) {
        let reclen = (19 + name.len() + 1 + 7) & !7;
        if used + reclen > out.len() {
            if used == 0 {
                return Err(EINVAL);
            }
            break;
        }
        let rec = &mut out[used..used + reclen];
        rec.fill(0);
        let ino = fnv(&format!("{host}/{name}"));
        rec[0..8].copy_from_slice(&ino.to_le_bytes());
        rec[8..16].copy_from_slice(&((*pos + 1) as u64).to_le_bytes());
        rec[16..18].copy_from_slice(&(reclen as u16).to_le_bytes());
        rec[18] = *kind;
        rec[19..19 + name.len()].copy_from_slice(name.as_bytes());
        used += reclen;
        *pos += 1;
    }
    out.commit(used)?;
    Ok(used as u64)
}

const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

/// The entries of a directory: real ones (and links), or the virtual `/dev` and `/proc`.
fn dir_entries(host: &str) -> Vec<(String, u8)> {
    let mut v = plain_entries(host);
    for name in super::links::in_dir(&to_guest(host)) {
        if !v.iter().any(|(n, _)| *n == name) {
            v.push((name, DT_LNK));
        }
    }
    v
}

fn plain_entries(host: &str) -> Vec<(String, u8)> {
    let kind = |dir: bool| if dir { DT_DIR } else { DT_REG };
    // A layered system folder: what the disk has, then the system image.
    let guest = to_guest(host);
    if in_system_dirs(&guest) {
        let mut v: Vec<(String, u8)> = Vec::new();
        for root in [WRITABLE_ROOT, SYSTEM_ROOT] {
            for e in fs::readdir(&format!("{root}{guest}")).unwrap_or_default() {
                if !v.iter().any(|(n, _)| *n == e.name) {
                    v.push((e.name, kind(e.kind == Kind::Dir)));
                }
            }
        }
        return v;
    }
    let names: &[(&str, bool)] = match host {
        "/dev" => &[("null", false), ("zero", false), ("random", false), ("urandom", false), ("shm", true)],
        "/proc" => &[("self", true), ("cpuinfo", false), ("meminfo", false), ("version", false)],
        "/proc/self" => &[("exe", false), ("maps", false), ("cmdline", false), ("comm", false), ("stat", false)],
        "/dev/shm" => return super::shm::names().into_iter().map(|n| (n, DT_REG)).collect(),
        _ => {
            let mut v: Vec<(String, u8)> = fs::readdir(host)
                .unwrap_or_default()
                .into_iter()
                .filter(|e| !(e.name == ".links" && (host == SYSTEM_ROOT || host == WRITABLE_ROOT)))
                .map(|e| (e.name, kind(e.kind == Kind::Dir)))
                .collect();
            // The Linux tree shows its top folders at the root.
            if host == "/" {
                for d in SYSTEM_DIRS.iter().chain(WRITABLE_DIRS.iter()).chain(["dev", "proc"].iter()) {
                    if !v.iter().any(|(n, _)| n == d) {
                        v.push((String::from(*d), DT_DIR));
                    }
                }
            }
            return v;
        }
    };
    names.iter().map(|&(n, d)| (String::from(n), kind(d))).collect()
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3)) >> 1
}

// ------------------------------------------------------------------ stat

/// What `stat` reports, gathered from either a path or a handle.
struct Info {
    mode: u32,
    size: u64,
    ino: u64,
    rdev: u64,
    /// Unix seconds.
    mtime: i64,
    ctime: i64,
}

/// WaveOS keeps local time since 2000; Linux wants UTC since 1970.
pub fn unix_time(since_2000: u64) -> i64 {
    let offset = crate::gui::prefs::get("utc_offset").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0);
    since_2000 as i64 + 946_684_800 - offset * 60
}

/// Program and library folders hold executables; everything else is data.
fn file_mode(host: &str) -> u32 {
    let exec = ["/bin/", "/sbin/", "/libexec/", "/lib/", "/lib64/"].iter().any(|d| host.contains(d))
        || host.ends_with(".elf")
        || host.contains(".so");
    S_IFREG | if exec { 0o755 } else { 0o644 }
}

fn info_for_path(p: &Process, guest: &str, follow_link: bool) -> Result<Info, isize> {
    if let Some(to) = super::links::target(guest) {
        if !follow_link {
            let mode = S_IFLNK | 0o777;
            return Ok(Info { mode, size: to.len() as u64, ino: fnv(guest), rdev: 0, mtime: 0, ctime: 0 });
        }
        return info_for_path(p, &follow(guest)?, false);
    }
    if let Some(shm) = guest.strip_prefix("/dev/shm/").filter(|n| !n.is_empty()) {
        let f = super::shm::lookup(shm).ok_or(ENOENT)?;
        return Ok(Info { mode: S_IFREG | 0o600, size: f.size(), ino: fnv(guest), rdev: 0, mtime: 0, ctime: 0 });
    }
    if let Some(v) = virtual_file(p, guest) {
        let s = v?;
        let (mode, rdev) = s.mode();
        let size = if let Special::Mem { data, .. } = &s { data.len() as u64 } else { 0 };
        return Ok(Info { mode, size, ino: fnv(guest), rdev, mtime: 0, ctime: 0 });
    }
    let host = to_host(guest);
    // The Linux tree's top folders exist even before anything is in them.
    let (md, _) = match fs::stat(&host) {
        Err(ENOENT) if guest.trim_end_matches('/').matches('/').count() == 1 && host != guest => {
            return Ok(Info { mode: S_IFDIR | 0o755, size: 0, ino: fnv(guest), rdev: 0, mtime: 0, ctime: 0 });
        }
        r => r?,
    };
    let ino = fs::lookup(&host).map(|(_, i)| i).unwrap_or_else(|_| fnv(&host));
    let mode = if md.kind == Kind::Dir { S_IFDIR | 0o755 } else { file_mode(&host) };
    Ok(Info { mode, size: md.size, ino, rdev: 0, mtime: unix_time(md.mtime), ctime: unix_time(md.ctime) })
}

fn info_for_handle(h: &Handle) -> Result<Info, isize> {
    let fifo = |ino| Info { mode: S_IFIFO | 0o600, size: 0, ino, rdev: 0, mtime: 0, ctime: 0 };
    Ok(match h {
        Handle::File(f) => {
            let f = f.lock();
            let md = f.fs.metadata(f.ino)?;
            let mode = if md.kind == Kind::Dir { S_IFDIR | 0o755 } else { S_IFREG | 0o755 };
            Info { mode, size: md.size, ino: f.ino, rdev: 0, mtime: unix_time(md.mtime), ctime: unix_time(md.ctime) }
        }
        Handle::Special(s) => {
            let s = s.lock();
            if let Special::Dir { host, .. } = &*s {
                return Ok(Info { mode: S_IFDIR | 0o755, size: 0, ino: fnv(host), rdev: 0, mtime: 0, ctime: 0 });
            }
            let (mode, rdev) = s.mode();
            let size = match &*s {
                Special::Mem { data, .. } => data.len() as u64,
                Special::Shm { file, .. } => file.size(),
                _ => 0,
            };
            Info { mode, size, ino: h as *const Handle as u64 >> 4, rdev, mtime: 0, ctime: 0 }
        }
        Handle::PipeRead { pipe, .. } => fifo(Arc::as_ptr(pipe) as u64 >> 4),
        Handle::PipeWrite(pipe) => fifo(Arc::as_ptr(pipe) as u64 >> 4),
        Handle::Socket(_) => Info { mode: S_IFSOCK | 0o777, size: 0, ino: 1, rdev: 0, mtime: 0, ctime: 0 },
        Handle::Log | Handle::Audio(_) => {
            Info { mode: S_IFCHR | 0o620, size: 0, ino: 2, rdev: makedev(5, 1), mtime: 0, ctime: 0 }
        }
    })
}

/// Linux's `struct stat` (144 bytes on x86-64).
fn put_stat(ptr: u64, i: &Info) -> Result<u64, isize> {
    let mut b = [0u8; 144];
    let mut w = |off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    w(0, 0x803); // st_dev
    w(8, i.ino);
    w(16, if i.mode & S_IFDIR != 0 { 2 } else { 1 }); // st_nlink
    w(24, i.mode as u64 | (super::UID << 32)); // st_mode, st_uid
    w(32, super::UID); // st_gid
    w(40, i.rdev);
    w(48, i.size);
    w(56, 4096); // st_blksize
    w(64, i.size.div_ceil(512)); // st_blocks
    w(72, i.mtime as u64); // st_atime
    w(88, i.mtime as u64);
    w(104, i.ctime as u64);
    user::put(ptr, &b).map(|_| 0)
}

pub fn stat_path(p: &Process, dirfd: u64, ptr: u64, buf: u64, flags: u64) -> Result<u64, isize> {
    let raw = user::cstr(ptr)?;
    if raw.is_empty() && flags & AT_EMPTY_PATH != 0 {
        return fstat(p, dirfd, buf);
    }
    let guest = guest_path(p, dirfd, &raw)?;
    put_stat(buf, &info_for_path(p, &guest, flags & AT_SYMLINK_NOFOLLOW == 0)?)
}

pub fn fstat(p: &Process, fd: u64, buf: u64) -> Result<u64, isize> {
    let h = p.fd(fd as usize)?;
    put_stat(buf, &info_for_handle(&h)?)
}

/// `statx` (glibc and coreutils use it).
pub fn statx(p: &Process, dirfd: u64, ptr: u64, flags: u64, buf: u64) -> Result<u64, isize> {
    let raw = user::cstr(ptr)?;
    let info = if raw.is_empty() && flags & AT_EMPTY_PATH != 0 {
        info_for_handle(&*p.fd(dirfd as usize)?)?
    } else {
        info_for_path(p, &guest_path(p, dirfd, &raw)?, flags & AT_SYMLINK_NOFOLLOW == 0)?
    };
    let mut b = [0u8; 256];
    let put32 = |b: &mut [u8; 256], off: usize, v: u32| b[off..off + 4].copy_from_slice(&v.to_le_bytes());
    let put64 = |b: &mut [u8; 256], off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut b, 0, 0x7FF); // stx_mask: the basic fields
    put32(&mut b, 4, 4096);
    put32(&mut b, 16, if info.mode & S_IFDIR != 0 { 2 } else { 1 });
    put32(&mut b, 20, super::UID as u32);
    put32(&mut b, 24, super::UID as u32);
    b[28..30].copy_from_slice(&(info.mode as u16).to_le_bytes());
    put64(&mut b, 32, info.ino);
    put64(&mut b, 40, info.size);
    put64(&mut b, 48, info.size.div_ceil(512));
    for off in [64, 80, 112] {
        put64(&mut b, off, info.mtime as u64); // atime, btime, mtime
    }
    put64(&mut b, 96, info.ctime as u64);
    put32(&mut b, 128, (info.rdev >> 8) as u32);
    put32(&mut b, 132, (info.rdev & 0xFF) as u32);
    put32(&mut b, 136, 8);
    put32(&mut b, 140, 3);
    user::put(buf, &b).map(|_| 0)
}

pub fn access(p: &Process, dirfd: u64, ptr: u64, mode: u64) -> Result<u64, isize> {
    let guest = follow(&path_arg(p, dirfd, ptr)?)?;
    let info = info_for_path(p, &guest, true)?;
    if mode & 2 != 0 && virtual_file(p, &guest).is_none() {
        let host = to_host(&guest);
        if fs::stat(&host).is_ok_and(|(_, ro)| ro) {
            return Err(EROFS);
        }
    }
    if mode & 1 != 0 && info.mode & 0o111 == 0 {
        return Err(EACCES);
    }
    Ok(0)
}

// ------------------------------------------------------------------ fds

pub fn ioctl(p: &Process, fd: u64, request: u64, arg: u64) -> Result<u64, isize> {
    // The request is 32 bits, as on Linux (musl passes it sign-extended).
    let request = request as u32 as u64;
    if let Handle::Audio(stream) = &*p.fd(fd as usize)? {
        return oss(stream, request, arg);
    }
    match request {
        // Terminal queries: no file is a terminal yet.
        0x5401 | 0x5413 | 0x540F | 0x5410 | 0x5402..=0x5404 => Err(ENOTTY),
        // FIONBIO, FIOCLEX, FIONCLEX.
        0x5421 | 0x5451 | 0x5450 => Ok(0),
        // FIONREAD: bytes waiting to be read (the next datagram's, for those).
        0x541B => {
            let n = match &*p.fd(fd as usize)? {
                Handle::PipeRead { pipe, .. } => pipe.available() as u64,
                Handle::Socket(s) => s.option(aurora_abi::net::OPT_READABLE, 0)?,
                Handle::Special(s) => match &*s.lock() {
                    Special::Unix(u) => u.available() as u64,
                    Special::Mem { data, pos } => data.len().saturating_sub(*pos) as u64,
                    _ => 0,
                },
                Handle::File(f) => {
                    let f = f.lock();
                    f.fs.metadata(f.ino).map(|m| m.size.saturating_sub(f.pos)).unwrap_or(0)
                }
                _ => 0,
            };
            user::put(arg, &(n.min(i32::MAX as u64) as i32)).map(|_| 0)
        }
        _ => Err(ENOTTY),
    }
}

/// OSS's `/dev/dsp` requests. The stream plays 48 kHz, 16-bit little-endian
/// stereo; a program that asks for something else is told so (as OSS
/// drivers do) and converts.
fn oss(stream: &crate::drivers::audio::Stream, request: u64, arg: u64) -> Result<u64, isize> {
    use crate::drivers::audio::RATE;
    const AFMT_S16_LE: i32 = 0x10;
    // Fragments as programs see them: 4 KiB, 23 ms of sound.
    const FRAGMENT: usize = 4096;
    let put = |v: i32| user::put(arg, &v).map(|_| 0);
    match request {
        // SNDCTL_DSP_RESET: drop what is queued.
        0x5000 => stream.control(aurora_abi::audio::FLUSH),
        // SNDCTL_DSP_SYNC, SNDCTL_DSP_POST: play what is queued.
        0x5001 => stream.drain().map(|_| 0),
        0x5008 | 0x500E => Ok(0),
        // SNDCTL_DSP_SPEED, _STEREO, _GETBLKSIZE, _SETFMT, _CHANNELS, _GETFMTS.
        0xC004_5002 => put(RATE as i32),
        0xC004_5003 => put(1),
        0xC004_5004 => put(FRAGMENT as i32),
        0xC004_5005 | 0x8004_500B => put(AFMT_S16_LE),
        0xC004_5006 => put(2),
        // SNDCTL_DSP_SETFRAGMENT, _SUBDIVIDE, _SETTRIGGER: accepted.
        0xC004_500A | 0xC004_5009 | 0x4004_5010 => Ok(0),
        // SNDCTL_DSP_GETCAPS (none of the optional ones), _GETTRIGGER (output on).
        0x8004_500F => put(0),
        0x8004_5010 => put(2),
        // SNDCTL_DSP_GETOSPACE: audio_buf_info { fragments, fragstotal, fragsize, bytes }.
        0x8010_500C => {
            let free = stream.room() * 2;
            let total = (stream.room() + stream.queued()) * 2;
            let info = [(free / FRAGMENT) as i32, (total / FRAGMENT) as i32, FRAGMENT as i32, free as i32];
            user::put(arg, &info).map(|_| 0)
        }
        // SNDCTL_DSP_GETODELAY: bytes written but not played yet.
        0x8004_5017 => put((stream.queued() * 2) as i32),
        // SNDCTL_DSP_GETOPTR: count_info { bytes played, blocks, ptr }.
        0x800C_5012 => {
            let played = stream.control(aurora_abi::audio::POSITION)? * 4;
            user::put(arg, &[played as i32, 0, 0]).map(|_| 0)
        }
        _ => Err(EINVAL),
    }
}

pub fn fcntl(p: &Process, fd: u64, cmd: u64, arg: u64) -> Result<u64, isize> {
    let h = p.fd(fd as usize)?;
    match cmd {
        // F_DUPFD, F_DUPFD_CLOEXEC: the lowest free descriptor at or above `arg`.
        0 | 1030 => {
            let mut fds = p.fds.lock();
            let from = arg as usize;
            let slot = (from..crate::proc::MAX_FDS).find(|&i| fds.get(i).is_none_or(|f| f.is_none())).ok_or(EMFILE)?;
            if fds.len() <= slot {
                fds.resize(slot + 1, None);
            }
            fds[slot] = Some(h);
            drop(fds);
            if let Some(l) = &p.linux {
                l.set_cloexec(slot, cmd == 1030);
            }
            Ok(slot as u64)
        }
        // F_GETFD, F_SETFD: the close-on-exec flag.
        1 => Ok(p.linux.as_ref().is_some_and(|l| l.cloexec(fd as usize)) as u64),
        2 => {
            if let Some(l) = &p.linux {
                l.set_cloexec(fd as usize, arg & 1 != 0);
            }
            Ok(0)
        }
        // F_GETFL.
        3 => Ok(match &*h {
            Handle::File(f) => {
                let f = f.lock();
                let acc = match (f.readable, f.writable) {
                    (true, true) => O_RDWR,
                    (false, true) => O_WRONLY,
                    _ => 0,
                };
                acc | if f.append { O_APPEND } else { 0 }
            }
            Handle::PipeRead { nonblocking: true, .. } => O_NONBLOCK,
            Handle::PipeWrite(_) | Handle::Log => O_WRONLY,
            Handle::Socket(_) | Handle::Special(_) => O_RDWR,
            _ => 0,
        } | if p.linux.as_ref().is_some_and(|l| l.nonblock(fd as usize)) { O_NONBLOCK } else { 0 }),
        // F_SETFL: non-blocking or not (the rest of the flags stay as they are).
        4 => {
            let on = arg & O_NONBLOCK != 0;
            if let Some(l) = &p.linux {
                l.set_nonblock(fd as usize, on);
            }
            if let Handle::Socket(s) = &*h {
                s.option(aurora_abi::net::OPT_NONBLOCK, on as u64)?;
            }
            Ok(0)
        }
        // F_GETLK, F_OFD_GETLK: record locks aren't kept, so none is ever in
        // the way (`l_type`, the first field of `struct flock`, becomes
        // F_UNLCK). Left as it was, the asker would read back its own
        // request as someone else's lock (SQLite then can't open a database
        // in WAL mode).
        5 | 36 => user::put(arg, &2u16).map(|_| 0),
        // Setting locks (F_SETLK, F_SETLKW and the OFD ones), pipe sizes: accepted.
        6 | 7 | 37 | 38 | 1031 | 1032 => Ok(0),
        // Seals on memory files: accepted, none reported.
        1033 | 1034 if shm_of(p, fd).is_some() => Ok(0),
        _ => Err(EINVAL),
    }
}

pub fn dup(p: &Process, fd: u64) -> Result<u64, isize> {
    let h = p.fd(fd as usize)?;
    install(p, h, false)
}

pub fn dup3(p: &Process, old: u64, new: u64, flags: u64, dup2: bool) -> Result<u64, isize> {
    let h = p.fd(old as usize)?;
    if old == new {
        return if dup2 { Ok(new) } else { Err(EINVAL) };
    }
    let new = new as usize;
    if new >= crate::proc::MAX_FDS {
        return Err(EBADF);
    }
    let mut fds = p.fds.lock();
    if fds.len() <= new {
        fds.resize(new + 1, None);
    }
    let old_handle = fds[new].replace(h);
    drop(fds);
    if let Some(l) = &p.linux {
        l.set_cloexec(new, flags & O_CLOEXEC != 0);
    }
    // The replaced handle goes after the lock.
    drop(old_handle);
    Ok(new as u64)
}

pub fn pipe2(p: &Process, ptr: u64, flags: u64) -> Result<u64, isize> {
    user::writable(ptr, 8)?;
    let (r, w) = crate::proc::new_pipe(flags & O_NONBLOCK != 0);
    let cloexec = flags & O_CLOEXEC != 0;
    let rfd = install(p, r, cloexec)?;
    let wfd = match install(p, w, cloexec) {
        Ok(fd) => fd,
        Err(e) => {
            let _ = close(p, rfd);
            return Err(e);
        }
    };
    if let Some(l) = &p.linux {
        l.set_nonblock(rfd as usize, flags & O_NONBLOCK != 0);
        l.set_nonblock(wfd as usize, flags & O_NONBLOCK != 0);
    }
    user::put(ptr, &[rfd as i32, wfd as i32]).map(|_| 0)
}

/// Whether descriptor `fd` could be read and written without blocking:
/// files and devices always; pipes, events and timers when they are ready.
fn readiness(p: &Process, fd: usize) -> Option<(bool, bool)> {
    readiness_of(p, fd, 0).map(|(rd, wr, _)| (rd, wr))
}

/// How long `poll`/`select` may wait.
pub enum Timeout {
    /// A `struct timeval` (`select`), or none (0: forever).
    Timeval(u64),
    /// A `struct timespec` (`ppoll`, `pselect6`), or none.
    Timespec(u64),
}

/// The deadline (uptime ms) for a timeout; `None`: wait forever.
fn deadline(t: Timeout) -> Result<Option<u64>, isize> {
    let ms = match t {
        Timeout::Timeval(0) | Timeout::Timespec(0) => return Ok(None),
        Timeout::Timeval(ptr) => {
            let [sec, usec]: [i64; 2] = user::get(ptr)?;
            sec.max(0) as u64 * 1000 + (usec.max(0) as u64).div_ceil(1000)
        }
        Timeout::Timespec(ptr) => {
            let [sec, nsec]: [i64; 2] = user::get(ptr)?;
            sec.max(0) as u64 * 1000 + (nsec.max(0) as u64).div_ceil(1_000_000)
        }
    };
    Ok(Some(crate::time::uptime_ms() + ms))
}

/// Waits until `check` finds something ready (it returns a count), the
/// deadline passes (0), or a signal comes (`EINTR`). It wakes when one of
/// `fds` changes, or when the soonest of their timers expires.
fn wait_ready(
    p: &Process,
    deadline: Option<u64>,
    fds: impl Fn() -> Vec<usize>,
    mut check: impl FnMut(bool) -> Result<u64, isize>,
) -> Result<u64, isize> {
    use crate::proc::readiness;
    loop {
        let gen = readiness::generation();
        let timed_out = deadline.is_some_and(|d| crate::time::uptime_ms() >= d);
        let n = check(timed_out)?;
        if n > 0 || timed_out {
            return Ok(n);
        }
        if crate::proc::interrupted() {
            return Err(EINTR);
        }
        let fds = fds();
        let mut wait = deadline.map_or(1000, |d| d.saturating_sub(crate::time::uptime_ms()));
        if let Some(t) = next_timer(p, fds.iter().copied()) {
            wait = wait.min(t);
        }
        let mut keys = Vec::new();
        for &fd in &fds {
            wait_keys(p, fd, &mut keys);
        }
        readiness::wait(gen, &keys, wait.max(1));
    }
}

pub fn poll(p: &Process, fds_ptr: u64, count: u64, timeout_ms: i64) -> Result<u64, isize> {
    let deadline = (timeout_ms >= 0).then(|| crate::time::uptime_ms() + timeout_ms as u64);
    poll_until(p, fds_ptr, count, deadline)
}

/// `ppoll`: a timespec, and a signal mask to wait with.
pub fn ppoll(p: &Process, fds_ptr: u64, count: u64, timeout: u64, mask: u64) -> Result<u64, isize> {
    let deadline = deadline(Timeout::Timespec(timeout))?;
    if mask != 0 {
        super::signal::wait_with_mask(p, mask)?;
    }
    poll_until(p, fds_ptr, count, deadline)
}

fn poll_until(p: &Process, fds_ptr: u64, count: u64, deadline: Option<u64>) -> Result<u64, isize> {
    if count > 1024 {
        return Err(EINVAL);
    }
    let polled = || {
        (0..count).filter_map(|i| user::get::<i32>(fds_ptr + i * 8).ok()).filter(|&fd| fd >= 0).map(|fd| fd as usize)
    };
    wait_ready(
        p,
        deadline,
        || polled().collect(),
        |final_pass| {
            let mut ready = 0u64;
            let mut out = Vec::with_capacity(count as usize);
            for i in 0..count {
                let [fd, ev]: [i32; 2] = user::get(fds_ptr + i * 8)?;
                let events = (ev & 0xFFFF) as u16;
                let revents: u16 = if fd < 0 {
                    0
                } else {
                    match readiness(p, fd as usize) {
                        None => 0x20, // POLLNVAL
                        Some((rd, wr)) => (if rd { events & 0x1 } else { 0 }) | (if wr { events & 0x4 } else { 0 }),
                    }
                };
                if revents != 0 {
                    ready += 1;
                }
                out.push((fd, ev, revents));
            }
            if ready > 0 || final_pass {
                for (i, (fd, ev, rev)) in out.into_iter().enumerate() {
                    let word = (fd as u32 as u64) | ((ev as u16 as u64) << 32) | ((rev as u64) << 48);
                    user::put(fds_ptr + i as u64 * 8, &word)?;
                }
            }
            Ok(ready)
        },
    )
}

/// `select` and `pselect6`: descriptor bit sets for reading, writing and
/// exceptions (never any).
pub fn select(p: &Process, nfds: u64, sets: [u64; 3], timeout: Timeout, sigdata: u64) -> Result<u64, isize> {
    if nfds > 1024 {
        return Err(EINVAL);
    }
    let words = nfds.div_ceil(64) as usize;
    let read_set = |ptr: u64| -> Result<Vec<u64>, isize> {
        let mut v = alloc::vec![0u64; words];
        for (i, w) in v.iter_mut().enumerate() {
            if ptr != 0 {
                *w = user::get(ptr + i as u64 * 8)?;
            }
        }
        Ok(v)
    };
    let want = [read_set(sets[0])?, read_set(sets[1])?];
    for fd in 0..nfds as usize {
        let asked = (want[0][fd / 64] | want[1][fd / 64]) & (1 << (fd % 64)) != 0;
        if asked && readiness(p, fd).is_none() {
            return Err(EBADF);
        }
    }
    let deadline = deadline(timeout)?;
    if sigdata != 0 {
        // pselect6's sixth argument: { const sigset_t *mask; size_t size }.
        let [mask, _]: [u64; 2] = user::get(sigdata)?;
        if mask != 0 {
            super::signal::wait_with_mask(p, mask)?;
        }
    }
    let watched = || (0..nfds as usize).filter(|&fd| (want[0][fd / 64] | want[1][fd / 64]) & (1 << (fd % 64)) != 0);
    wait_ready(
        p,
        deadline,
        || watched().collect(),
        |final_pass| {
            let mut got = [alloc::vec![0u64; words], alloc::vec![0u64; words]];
            let mut n = 0;
            for fd in 0..nfds as usize {
                let bit = 1u64 << (fd % 64);
                if (want[0][fd / 64] | want[1][fd / 64]) & bit == 0 {
                    continue;
                }
                let (rd, wr) = readiness(p, fd).unwrap_or((true, true));
                for (k, ok) in [rd, wr].into_iter().enumerate() {
                    if ok && want[k][fd / 64] & bit != 0 {
                        got[k][fd / 64] |= bit;
                        n += 1;
                    }
                }
            }
            if n > 0 || final_pass {
                for (k, ptr) in [sets[0], sets[1], sets[2]].into_iter().enumerate() {
                    if ptr == 0 {
                        continue;
                    }
                    let none = alloc::vec![0u64; words];
                    for (i, w) in got.get(k).unwrap_or(&none).iter().enumerate() {
                        user::put(ptr + i as u64 * 8, w)?;
                    }
                }
            }
            Ok(n)
        },
    )
}

// ------------------------------------------------------------------ folders

pub fn getcwd(p: &Process, ptr: u64, size: u64) -> Result<u64, isize> {
    let mut cwd = to_guest(&p.cwd.lock()).into_bytes();
    cwd.push(0);
    if cwd.len() as u64 > size {
        return Err(ERANGE);
    }
    user::put_bytes(ptr, &cwd)?;
    Ok(cwd.len() as u64)
}

pub fn chdir(p: &Process, ptr: u64) -> Result<u64, isize> {
    let guest = follow(&path_arg(p, AT_FDCWD, ptr)?)?;
    set_cwd(p, &guest)
}

fn set_cwd(p: &Process, guest: &str) -> Result<u64, isize> {
    let host = to_host(guest);
    if !fs::is_dir(&host) {
        return Err(if fs::stat(&host).is_ok() { ENOTDIR } else { ENOENT });
    }
    *p.cwd.lock() = host;
    Ok(0)
}

pub fn fchdir(p: &Process, fd: u64) -> Result<u64, isize> {
    let guest = base_dir(p, fd)?;
    set_cwd(p, &guest)
}

pub fn mkdirat(p: &Process, dirfd: u64, ptr: u64) -> Result<u64, isize> {
    let guest = path_arg(p, dirfd, ptr)?;
    fs::mkdir(&to_host(&guest)).map(|_| 0)
}

pub fn unlinkat(p: &Process, dirfd: u64, ptr: u64, flags: u64) -> Result<u64, isize> {
    let guest = path_arg(p, dirfd, ptr)?;
    if let Some(name) = guest.strip_prefix("/dev/shm/") {
        return super::shm::unlink(name).map(|_| 0);
    }
    if super::links::target(&guest).is_some() {
        return super::links::remove(&guest).map(|_| 0);
    }
    let host = to_host(&guest);
    let dir = fs::is_dir(&host);
    if flags & AT_REMOVEDIR != 0 && !dir {
        return Err(ENOTDIR);
    }
    if flags & AT_REMOVEDIR == 0 && dir {
        return Err(EISDIR);
    }
    fs::unlink(&host).map(|_| 0)
}

pub fn renameat(p: &Process, olddir: u64, old: u64, newdir: u64, new: u64) -> Result<u64, isize> {
    let from = to_host(&path_arg(p, olddir, old)?);
    let to = to_host(&path_arg(p, newdir, new)?);
    // Linux replaces the destination.
    match fs::rename(&from, &to) {
        Err(EEXIST) if !fs::is_dir(&to) => {
            fs::unlink(&to)?;
            fs::rename(&from, &to).map(|_| 0)
        }
        r => r.map(|_| 0),
    }
}

pub fn readlinkat(p: &Process, dirfd: u64, ptr: u64, buf: u64, size: u64) -> Result<u64, isize> {
    let guest = path_arg(p, dirfd, ptr)?;
    let target = match guest.as_str() {
        "/proc/self/exe" => to_guest(&p.path()),
        "/proc/self/cwd" => to_guest(&p.cwd.lock()),
        _ => match super::links::target(&guest) {
            Some(to) => to,
            None => {
                info_for_path(p, &guest, false)?;
                return Err(EINVAL); // not a link
            }
        },
    };
    let n = target.len().min(size as usize);
    user::put_bytes(buf, &target.as_bytes()[..n])?;
    Ok(n as u64)
}

/// `symlink`/`symlinkat`: links can be made where files can be written.
pub fn symlinkat(p: &Process, target: u64, dirfd: u64, ptr: u64) -> Result<u64, isize> {
    let to = user::cstr(target)?;
    let guest = path_arg(p, dirfd, ptr)?;
    if to.is_empty() {
        return Err(ENOENT);
    }
    let host = to_host(&guest);
    if host.starts_with(SYSTEM_ROOT) || host.starts_with("/System") {
        return Err(EROFS);
    }
    if fs::stat(&host).is_ok() || virtual_file(p, &guest).is_some() {
        return Err(EEXIST);
    }
    super::links::create(&guest, &to).map(|_| 0)
}

pub fn truncate(p: &Process, ptr: u64, len: u64) -> Result<u64, isize> {
    let host = to_host(&follow(&path_arg(p, AT_FDCWD, ptr)?)?);
    let mut f = fs::open(&host, aurora_abi::open::WRITE)?;
    f.truncate(len).map(|_| 0)
}

pub fn ftruncate(p: &Process, fd: u64, len: u64) -> Result<u64, isize> {
    if let Some(shm) = shm_of(p, fd) {
        return shm.resize(len).map(|_| 0);
    }
    match &*p.fd(fd as usize)? {
        Handle::File(f) => f.lock().truncate(len).map(|_| 0),
        _ => Err(EINVAL),
    }
}

/// `fallocate`: makes room for `len` bytes at `off` (the file grows unless
/// `FALLOC_FL_KEEP_SIZE`); other modes (punching holes) aren't supported.
pub fn fallocate(p: &Process, fd: u64, mode: u64, off: u64, len: u64) -> Result<u64, isize> {
    const KEEP_SIZE: u64 = 1;
    let end = off.checked_add(len).ok_or(EFBIG)?;
    if mode & !KEEP_SIZE != 0 {
        return Err(EOPNOTSUPP);
    }
    if let Some(shm) = shm_of(p, fd) {
        if mode == 0 && end > shm.size() {
            shm.resize(end)?;
        }
        return Ok(0);
    }
    match &*p.fd(fd as usize)? {
        Handle::File(f) => {
            let mut f = f.lock();
            if mode == 0 && end > f.size()? {
                f.truncate(end)?;
            }
            Ok(0)
        }
        _ => Err(ENODEV),
    }
}

/// `statfs`/`fstatfs`: one big volume.
pub fn statfs(buf: u64) -> Result<u64, isize> {
    let (total, free) = fs::space_of("/").unwrap_or((1 << 34, 1 << 33));
    let mut b = [0u8; 120];
    let mut w = |off: usize, v: u64| b[off..off + 8].copy_from_slice(&v.to_le_bytes());
    w(0, 0xEF53);
    w(8, 4096);
    w(16, total / 4096);
    w(24, free / 4096);
    w(32, free / 4096);
    w(40, 1 << 20);
    w(48, 1 << 19);
    w(64, 255);
    w(72, 4096);
    user::put(buf, &b).map(|_| 0)
}
