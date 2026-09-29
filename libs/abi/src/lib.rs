//! The WaveOS Aurora system-call ABI, shared by the Tide kernel and user space.
//!
//! Calling convention (`syscall` instruction): number in `rax`, arguments in
//! `rdi, rsi, rdx, r10, r8, r9`, result in `rax`. Results `>= 0` are success;
//! negative results are `-errno` (see [`err`]).
//!
//! Every struct that crosses the boundary is `#[repr(C)]` and contains only
//! integers, so the kernel never has to trust user-written enum discriminants.

#![no_std]

pub mod input;

/// System call numbers.
pub mod nr {
    pub const EXIT: usize = 0;
    pub const YIELD: usize = 1;
    pub const SLEEP: usize = 2;
    pub const TIME_MS: usize = 3;
    pub const GETPID: usize = 4;
    /// `spawn(path, path_len, argv_block, argv_len, stdio: *const [i32; 3]) -> pid`
    pub const SPAWN: usize = 5;
    /// `wait(pid, timeout_ms) -> exit code` (`-EAGAIN` on timeout; `u64::MAX` timeout blocks)
    pub const WAIT: usize = 6;
    pub const KILL: usize = 7;
    /// `proc_list(buf: *mut ProcInfo, max) -> count`
    pub const PROC_LIST: usize = 8;
    /// `mmap(len) -> address` (anonymous, zeroed, read/write)
    pub const MMAP: usize = 9;
    pub const MUNMAP: usize = 10;
    /// `log(ptr, len)` — writes to the kernel serial log.
    pub const LOG: usize = 11;
    /// `open(path, len, flags) -> fd`
    pub const OPEN: usize = 12;
    pub const READ: usize = 13;
    pub const WRITE: usize = 14;
    /// `seek(fd, offset, whence) -> new offset`
    pub const SEEK: usize = 15;
    pub const CLOSE: usize = 16;
    /// `stat(path, len, *mut Stat)`
    pub const STAT: usize = 17;
    /// `readdir(path, len, *mut DirEntry, max) -> count`
    pub const READDIR: usize = 18;
    pub const MKDIR: usize = 19;
    pub const UNLINK: usize = 20;
    /// `rename(from, from_len, to, to_len)`
    pub const RENAME: usize = 21;
    /// `pipe(*mut [i32; 2], flags)` — `[read_end, write_end]`; flags bit 0 = non-blocking reads
    pub const PIPE: usize = 22;
    /// `sys_info(*mut SysInfo)`
    pub const SYS_INFO: usize = 23;
    /// `desktop(request, arg_ptr, arg_len) -> value` (see [`desktop`])
    pub const DESKTOP: usize = 24;
    /// `win_create(width, height, title, title_len, flags) -> window id`
    pub const WIN_CREATE: usize = 25;
    /// `win_surface(window, *mut SurfaceInfo)` — current pixel buffer of a window
    pub const WIN_SURFACE: usize = 26;
    /// `win_present(window, x, y, w, h)` — mark a region as ready to composite
    pub const WIN_PRESENT: usize = 27;
    pub const WIN_SET_TITLE: usize = 28;
    pub const WIN_CLOSE: usize = 29;
    /// `next_event(*mut Event, timeout_ms) -> 1 if an event was written, 0 on timeout`
    pub const NEXT_EVENT: usize = 30;
    /// `datetime(*mut DateTime)` — wall-clock time from the RTC
    pub const DATETIME: usize = 31;
    /// `sync()` — flush filesystem caches to disk
    pub const SYNC: usize = 32;
    /// `truncate(fd, size)`
    pub const TRUNCATE: usize = 33;
    /// `win_resize(window, width, height)` — reallocates the surface
    pub const WIN_RESIZE: usize = 34;
    /// `chdir(path, len)`
    pub const CHDIR: usize = 35;
    /// `getcwd(buf, len) -> length`
    pub const GETCWD: usize = 36;
    pub const COUNT: usize = 37;
}

/// Error numbers (returned negated).
pub mod err {
    pub const EPERM: isize = 1;
    pub const ENOENT: isize = 2;
    pub const ESRCH: isize = 3;
    pub const EIO: isize = 5;
    pub const EBADF: isize = 9;
    pub const ECHILD: isize = 10;
    pub const EAGAIN: isize = 11;
    pub const ENOMEM: isize = 12;
    pub const EFAULT: isize = 14;
    pub const EEXIST: isize = 17;
    pub const ENOTDIR: isize = 20;
    pub const EISDIR: isize = 21;
    pub const EINVAL: isize = 22;
    pub const EMFILE: isize = 24;
    pub const ENOSPC: isize = 28;
    pub const ESPIPE: isize = 29;
    pub const EROFS: isize = 30;
    pub const EPIPE: isize = 32;
    pub const ENAMETOOLONG: isize = 36;
    pub const ENOSYS: isize = 38;
    pub const ENOTEMPTY: isize = 39;

    pub fn name(e: isize) -> &'static str {
        match e.abs() {
            EPERM => "operation not permitted",
            ENOENT => "no such file or directory",
            ESRCH => "no such process",
            EIO => "input/output error",
            EBADF => "bad file descriptor",
            ECHILD => "no child process",
            EAGAIN => "try again",
            ENOMEM => "out of memory",
            EFAULT => "bad address",
            EEXIST => "already exists",
            ENOTDIR => "not a directory",
            EISDIR => "is a directory",
            EINVAL => "invalid argument",
            EMFILE => "too many open files",
            ENOSPC => "no space left on device",
            ESPIPE => "illegal seek",
            EROFS => "read-only file system",
            EPIPE => "broken pipe",
            ENAMETOOLONG => "name too long",
            ENOSYS => "not implemented",
            ENOTEMPTY => "directory not empty",
            _ => "unknown error",
        }
    }
}

/// `open` flags.
pub mod open {
    pub const READ: usize = 1;
    pub const WRITE: usize = 2;
    pub const CREATE: usize = 4;
    pub const TRUNCATE: usize = 8;
    pub const APPEND: usize = 16;
    /// Fail with `EEXIST` if CREATE and the file exists.
    pub const EXCLUSIVE: usize = 32;
}

/// `seek` whence.
pub mod seek {
    pub const SET: usize = 0;
    pub const CUR: usize = 1;
    pub const END: usize = 2;
}

pub const KIND_FILE: u32 = 1;
pub const KIND_DIR: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Stat {
    pub kind: u32,
    pub read_only: u32,
    pub size: u64,
    /// Seconds since 2000-01-01 00:00 (local time from the RTC).
    pub mtime: u64,
    pub ctime: u64,
}

pub const NAME_MAX: usize = 255;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DirEntry {
    pub kind: u32,
    pub name_len: u32,
    pub size: u64,
    pub mtime: u64,
    pub name: [u8; NAME_MAX + 1],
}

impl DirEntry {
    pub const fn zeroed() -> Self {
        Self { kind: 0, name_len: 0, size: 0, mtime: 0, name: [0; NAME_MAX + 1] }
    }
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..(self.name_len as usize).min(NAME_MAX)]).unwrap_or("?")
    }
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Sunday
    pub weekday: u8,
}

pub const PROC_RUNNING: u32 = 0;
pub const PROC_READY: u32 = 1;
pub const PROC_SLEEPING: u32 = 2;
pub const PROC_EXITED: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: u32,
    pub state: u32,
    pub threads: u32,
    pub cpu_ms: u64,
    pub mem_kib: u64,
    /// 0 for kernel tasks, 1 for user processes.
    pub user: u32,
    pub name_len: u32,
    pub name: [u8; 32],
}

impl ProcInfo {
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..(self.name_len as usize).min(32)]).unwrap_or("?")
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct SysInfo {
    pub mem_total: u64,
    pub mem_used: u64,
    pub heap_size: u64,
    pub heap_used: u64,
    pub uptime_ms: u64,
    pub screen_w: u32,
    pub screen_h: u32,
    pub processes: u32,
    pub cpu_len: u32,
    pub cpu: [u8; 48],
    pub version_len: u32,
    pub version: [u8; 16],
    /// Human-readable description of the root volume, e.g. "WaveFS on AHCI".
    pub root_len: u32,
    pub root: [u8; 48],
    pub disk_total: u64,
    pub disk_free: u64,
}

/// A window's pixel buffer, mapped into the owning process.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SurfaceInfo {
    pub addr: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per row.
    pub stride: u32,
    pub _pad: u32,
}

/// Window creation flags.
pub mod win {
    pub const RESIZABLE: usize = 1;
}

/// Event kinds delivered by `next_event`.
pub mod event {
    pub const KEY: u32 = 1;
    pub const POINTER_MOVE: u32 = 2;
    pub const POINTER_DOWN: u32 = 3;
    pub const POINTER_UP: u32 = 4;
    pub const SCROLL: u32 = 5;
    /// The window was resized; call `win_surface` for the new buffer.
    pub const RESIZE: u32 = 6;
    pub const CLOSE_REQUESTED: u32 = 7;
    /// `a` = 1 when focused, 0 when unfocused.
    pub const FOCUS: u32 = 8;
    /// `a` = 1 for dark mode, `b` = wallpaper index.
    pub const THEME: u32 = 9;
}

/// A single input/window event. Field meaning depends on `kind`:
/// - pointer events: `x`, `y` (window-local), `a` = button (1 left, 2 right), `b` = click count
/// - `SCROLL`: `a` = wheel delta (positive = down)
/// - `KEY`: `a` = [`input::KeyCode`] as u32, `b` = char (0 = none), `c` = pressed, `d` = modifier bits
/// - `RESIZE`: `x` = width, `y` = height
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
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

/// `desktop` requests.
pub mod desktop {
    /// Open an app by program path (arg = path).
    pub const OPEN_APP: usize = 1;
    /// Open a document in the default app (arg = path).
    pub const OPEN_FILE: usize = 2;
    /// arg_len = 0 (light) or 1 (dark)
    pub const SET_DARK: usize = 3;
    /// arg_len = wallpaper index
    pub const SET_WALLPAPER: usize = 4;
    pub const SHUTDOWN: usize = 5;
    pub const REBOOT: usize = 6;
    /// Returns `(dark << 8) | wallpaper`.
    pub const GET_THEME: usize = 7;
}

/// Where user programs are linked and where the kernel places things.
pub mod layout {
    pub const PROGRAM_BASE: u64 = 0x40_0000;
    pub const MMAP_BASE: u64 = 0x100_0000_0000;
    pub const MMAP_END: u64 = 0x6000_0000_0000;
    pub const STACK_TOP: u64 = 0x7fff_f000_0000;
    pub const STACK_SIZE: u64 = 1 << 20;
    /// First address above user space.
    pub const USER_END: u64 = 0x0000_8000_0000_0000;
}
