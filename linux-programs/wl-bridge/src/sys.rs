//! The system calls the bridge makes: Linux ones (local sockets with
//! descriptors, `poll`, `mmap`, `memfd`) and WaveOS's window calls, which
//! Linux programs reach at `0x5741_0000 + n` (see `libs/linux/include/waveos.h`).

use std::arch::asm;

pub type Fd = i32;

/// A Linux system call; negative results are `-errno`.
pub fn call(n: i64, a: [i64; 6]) -> i64 {
    let ret: i64;
    unsafe {
        asm!("syscall",
            inlateout("rax") n => ret,
            in("rdi") a[0], in("rsi") a[1], in("rdx") a[2],
            in("r10") a[3], in("r8") a[4], in("r9") a[5],
            lateout("rcx") _, lateout("r11") _,
            options(nostack));
    }
    ret
}

const SOCKET: i64 = 41;
const CONNECT: i64 = 42;
const FORK: i64 = 57;
const SETSID: i64 = 112;
const ACCEPT4: i64 = 288;
const SENDMSG: i64 = 46;
const RECVMSG: i64 = 47;
const BIND: i64 = 49;
const LISTEN: i64 = 50;
const CLOSE: i64 = 3;
const POLL: i64 = 7;
const MMAP: i64 = 9;
const MUNMAP: i64 = 11;
const MEMFD_CREATE: i64 = 319;
const WRITE: i64 = 1;
const FTRUNCATE: i64 = 77;
const UNLINK: i64 = 87;

const AF_UNIX: i64 = 1;
const SOCK_STREAM: i64 = 1;
const SOCK_NONBLOCK: i64 = 0o4000;
const SOCK_CLOEXEC: i64 = 0o2000000;
const MSG_DONTWAIT: i64 = 0x40;
const MSG_NOSIGNAL: i64 = 0x4000;
const MSG_CMSG_CLOEXEC: i64 = 0x4000_0000;

pub fn close(fd: Fd) {
    call(CLOSE, [fd as i64, 0, 0, 0, 0, 0]);
}

/// A listening local socket at `path` (an old one there is removed).
pub fn listen_unix(path: &str) -> Result<Fd, i64> {
    let fd = call(SOCKET, [AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC | SOCK_NONBLOCK, 0, 0, 0, 0]);
    if fd < 0 {
        return Err(fd);
    }
    let mut addr = [0u8; 110];
    addr[0] = AF_UNIX as u8;
    addr[2..2 + path.len()].copy_from_slice(path.as_bytes());
    let cpath = format!("{path}\0");
    call(UNLINK, [cpath.as_ptr() as i64, 0, 0, 0, 0, 0]);
    let r = call(BIND, [fd, addr.as_ptr() as i64, (2 + path.len() + 1) as i64, 0, 0, 0]);
    if r < 0 {
        return Err(r);
    }
    let r = call(LISTEN, [fd, 16, 0, 0, 0, 0]);
    if r < 0 {
        return Err(r);
    }
    Ok(fd as Fd)
}

/// True if something answers on the local socket at `path`.
pub fn answers(path: &str) -> bool {
    let fd = call(SOCKET, [AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, 0, 0, 0]);
    if fd < 0 {
        return false;
    }
    let mut addr = [0u8; 110];
    addr[0] = AF_UNIX as u8;
    addr[2..2 + path.len()].copy_from_slice(path.as_bytes());
    let ok = call(CONNECT, [fd, addr.as_ptr() as i64, (2 + path.len() + 1) as i64, 0, 0, 0]) == 0;
    close(fd as Fd);
    ok
}

/// Carries on in a new process of its own (a new session); the caller's
/// process ends.
pub fn detach() {
    match call(FORK, [0; 6]) {
        0 => {
            call(SETSID, [0; 6]);
        }
        n if n > 0 => std::process::exit(0),
        _ => {}
    }
}

pub fn accept(fd: Fd) -> Option<Fd> {
    let c = call(ACCEPT4, [fd as i64, 0, 0, SOCK_CLOEXEC | SOCK_NONBLOCK, 0, 0]);
    (c >= 0).then_some(c as Fd)
}

#[repr(C)]
struct Iovec {
    base: *mut u8,
    len: usize,
}

#[repr(C)]
struct MsgHdr {
    name: *mut u8,
    namelen: u32,
    iov: *mut Iovec,
    iovlen: usize,
    control: *mut u8,
    controllen: usize,
    flags: i32,
}

/// Receives bytes and any descriptors sent with them, without waiting.
/// `Ok((0, _))` means the peer has gone; `Err(-11)` that nothing is waiting.
pub fn recv(fd: Fd, buf: &mut [u8]) -> Result<(usize, Vec<Fd>), i64> {
    let mut iov = Iovec { base: buf.as_mut_ptr(), len: buf.len() };
    let mut control = [0u64; 64];
    let mut m = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: control.as_mut_ptr() as *mut u8,
        controllen: std::mem::size_of_val(&control),
        flags: 0,
    };
    let n = call(RECVMSG, [fd as i64, &mut m as *mut MsgHdr as i64, MSG_DONTWAIT | MSG_CMSG_CLOEXEC, 0, 0, 0]);
    if n < 0 {
        return Err(n);
    }
    let mut fds = Vec::new();
    let bytes = unsafe { std::slice::from_raw_parts(control.as_ptr() as *const u8, m.controllen) };
    let mut at = 0;
    while at + 16 <= bytes.len() {
        let len = u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap()) as usize;
        let level = i32::from_le_bytes(bytes[at + 8..at + 12].try_into().unwrap());
        let ty = i32::from_le_bytes(bytes[at + 12..at + 16].try_into().unwrap());
        if len < 16 || at + len > bytes.len() {
            break;
        }
        if level == 1 && ty == 1 {
            for k in (at + 16..at + len).step_by(4) {
                fds.push(i32::from_le_bytes(bytes[k..k + 4].try_into().unwrap()));
            }
        }
        at += (len + 7) & !7;
    }
    Ok((n as usize, fds))
}

/// Sends bytes, with descriptors attached. Returns what was sent.
pub fn send(fd: Fd, data: &[u8], fds: &[Fd]) -> Result<usize, i64> {
    let mut iov = Iovec { base: data.as_ptr() as *mut u8, len: data.len() };
    let mut control = [0u8; 16 + 4 * 28];
    let mut m = MsgHdr {
        name: std::ptr::null_mut(),
        namelen: 0,
        iov: &mut iov,
        iovlen: 1,
        control: std::ptr::null_mut(),
        controllen: 0,
        flags: 0,
    };
    if !fds.is_empty() {
        let len = 16 + 4 * fds.len().min(28);
        control[0..8].copy_from_slice(&(len as u64).to_le_bytes());
        control[8..12].copy_from_slice(&1i32.to_le_bytes());
        control[12..16].copy_from_slice(&1i32.to_le_bytes());
        for (i, f) in fds.iter().take(28).enumerate() {
            control[16 + i * 4..20 + i * 4].copy_from_slice(&f.to_le_bytes());
        }
        m.control = control.as_mut_ptr();
        m.controllen = (len + 7) & !7;
    }
    let n = call(SENDMSG, [fd as i64, &m as *const MsgHdr as i64, MSG_DONTWAIT | MSG_NOSIGNAL, 0, 0, 0]);
    if n < 0 {
        Err(n)
    } else {
        Ok(n as usize)
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: Fd,
    pub events: i16,
    pub revents: i16,
}

pub const POLLIN: i16 = 1;

pub fn poll(fds: &mut [PollFd], timeout_ms: i32) -> i64 {
    call(POLL, [fds.as_mut_ptr() as i64, fds.len() as i64, timeout_ms as i64, 0, 0, 0])
}

/// Maps `size` bytes of `fd` shared, readable.
pub fn mmap_shared(fd: Fd, size: usize) -> Option<*mut u8> {
    let p = call(MMAP, [0, size as i64, 1, 1, fd as i64, 0]);
    (p > 0).then_some(p as *mut u8)
}

pub fn munmap(ptr: *mut u8, size: usize) {
    call(MUNMAP, [ptr as i64, size as i64, 0, 0, 0, 0]);
}

/// A memory file holding `data` (the keyboard map clients read).
pub fn memfd_with(data: &[u8]) -> Fd {
    let name = b"keymap\0";
    let fd = call(MEMFD_CREATE, [name.as_ptr() as i64, 1, 0, 0, 0, 0]) as Fd;
    call(FTRUNCATE, [fd as i64, data.len() as i64, 0, 0, 0, 0]);
    call(WRITE, [fd as i64, data.as_ptr() as i64, data.len() as i64, 0, 0, 0]);
    fd
}

// ---------------------------------------------------------------- WaveOS

const WAVE: i64 = 0x5741_0000;
const WIN_CREATE: i64 = 25;
const WIN_SURFACE: i64 = 26;
const WIN_PRESENT: i64 = 27;
const WIN_SET_TITLE: i64 = 28;
const WIN_CLOSE: i64 = 29;
const NEXT_EVENT: i64 = 30;
const WIN_RESIZE: i64 = 34;

/// A window's pixels (0xAARRGGBB), in device pixels.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Surface {
    pub addr: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub scale: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Event {
    pub kind: u32,
    pub window: u32,
    pub x: i32,
    pub y: i32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
}

pub mod event {
    pub const KEY: u32 = 1;
    pub const POINTER_MOVE: u32 = 2;
    pub const POINTER_DOWN: u32 = 3;
    pub const POINTER_UP: u32 = 4;
    pub const SCROLL: u32 = 5;
    pub const RESIZE: u32 = 6;
    pub const CLOSE_REQUESTED: u32 = 7;
    pub const FOCUS: u32 = 8;
}

pub fn win_create(w: u32, h: u32, title: &str) -> Option<u32> {
    let id = call(WAVE + WIN_CREATE, [w as i64, h as i64, title.as_ptr() as i64, title.len() as i64, 1, 0]);
    (id >= 0).then_some(id as u32)
}

pub fn win_surface(id: u32) -> Surface {
    let mut s = Surface::default();
    call(WAVE + WIN_SURFACE, [id as i64, &mut s as *mut Surface as i64, 0, 0, 0, 0]);
    s
}

pub fn win_present(id: u32, w: u32, h: u32) {
    call(WAVE + WIN_PRESENT, [id as i64, 0, 0, w as i64, h as i64, 0]);
}

pub fn win_set_title(id: u32, title: &str) {
    call(WAVE + WIN_SET_TITLE, [id as i64, title.as_ptr() as i64, title.len() as i64, 0, 0, 0]);
}

pub fn win_resize(id: u32, w: u32, h: u32) {
    call(WAVE + WIN_RESIZE, [id as i64, w as i64, h as i64, 0, 0, 0]);
}

pub fn win_close(id: u32) {
    call(WAVE + WIN_CLOSE, [id as i64, 0, 0, 0, 0, 0]);
}

/// The next window event, waiting up to `timeout_ms`.
pub fn next_event(timeout_ms: i64) -> Option<Event> {
    let mut e = Event::default();
    (call(WAVE + NEXT_EVENT, [&mut e as *mut Event as i64, timeout_ms, 0, 0, 0, 0]) == 1).then_some(e)
}

/// Milliseconds since start-up (Wayland timestamps).
pub fn now_ms() -> u32 {
    call(WAVE + 3, [0; 6]) as u32
}

const DISPLAY_MODES: i64 = 49;
const PREF_GET: i64 = 46;
const CLIPBOARD_SET: i64 = 41;
const CLIPBOARD_GET: i64 = 42;

/// The screen's size in device pixels.
pub fn screen_pixels() -> (i32, i32) {
    let mut modes = [[0u32; 4]; 64];
    let n = call(WAVE + DISPLAY_MODES, [modes.as_mut_ptr() as i64, 64, 0, 0, 0, 0]).clamp(0, 64) as usize;
    modes[..n].iter().find(|m| m[2] & 1 != 0).map_or((1920, 1080), |m| (m[0] as i32, m[1] as i32))
}

/// A system preference.
pub fn pref(key: &str) -> Option<String> {
    let mut out = [0u8; 256];
    let n = call(WAVE + PREF_GET, [key.as_ptr() as i64, key.len() as i64, out.as_mut_ptr() as i64, 256, 0, 0]);
    (n >= 0).then(|| String::from_utf8_lossy(&out[..(n as usize).min(256)]).into_owned())
}

/// Device pixels per point, as WaveOS chooses it.
pub fn scale() -> i32 {
    match pref("display_scale").as_deref() {
        Some("1") => 1,
        Some("2") => 2,
        _ => {
            let (w, h) = screen_pixels();
            if w >= 2560 && h >= 1440 {
                2
            } else {
                1
            }
        }
    }
}

pub fn clipboard_text() -> Option<String> {
    let mut out = vec![0u8; 4 << 20];
    let n = call(WAVE + CLIPBOARD_GET, [1, out.as_mut_ptr() as i64, out.len() as i64, 0, 0, 0]);
    (n > 0).then(|| {
        out.truncate(n as usize);
        String::from_utf8_lossy(&out).into_owned()
    })
}

pub fn set_clipboard_text(text: &[u8]) {
    call(WAVE + CLIPBOARD_SET, [1, text.as_ptr() as i64, text.len() as i64, 0, 0, 0]);
}

const READ: i64 = 0;
const PIPE2: i64 = 293;
const FCNTL: i64 = 72;
const O_NONBLOCK: i64 = 0o4000;
const O_CLOEXEC: i64 = 0o2000000;

/// A pipe (read end, write end); the read end doesn't block.
pub fn pipe() -> Option<(Fd, Fd)> {
    let mut fds = [0i32; 2];
    if call(PIPE2, [fds.as_mut_ptr() as i64, O_CLOEXEC, 0, 0, 0, 0]) < 0 {
        return None;
    }
    set_nonblock(fds[0]);
    Some((fds[0], fds[1]))
}

pub fn set_nonblock(fd: Fd) {
    let flags = call(FCNTL, [fd as i64, 3, 0, 0, 0, 0]);
    if flags >= 0 {
        call(FCNTL, [fd as i64, 4, flags | O_NONBLOCK, 0, 0, 0]);
    }
}

pub fn read(fd: Fd, buf: &mut [u8]) -> i64 {
    call(READ, [fd as i64, buf.as_mut_ptr() as i64, buf.len() as i64, 0, 0, 0])
}

pub fn write(fd: Fd, data: &[u8]) -> i64 {
    call(WRITE, [fd as i64, data.as_ptr() as i64, data.len() as i64, 0, 0, 0])
}

pub const POLLOUT: i16 = 4;
pub const POLLERR: i16 = 8;
pub const POLLHUP: i16 = 16;
