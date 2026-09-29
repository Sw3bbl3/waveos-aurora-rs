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
    /// `thread_spawn(entry, stack_top, arg) -> thread id` — `entry(arg)` runs in this process
    pub const THREAD_SPAWN: usize = 37;
    /// `thread_exit(notify)` — if `notify` is non-zero, writes 1 to that `u32` and wakes its futex waiters
    pub const THREAD_EXIT: usize = 38;
    /// `futex_wait(addr, expected, timeout_ms)` — sleeps while `*addr == expected` (`-EAGAIN` if it
    /// already differs, `-ETIMEDOUT` on timeout; `u64::MAX` blocks)
    pub const FUTEX_WAIT: usize = 39;
    /// `futex_wake(addr, count) -> woken`
    pub const FUTEX_WAKE: usize = 40;
    /// `clipboard_set(kind, ptr, len)` — replaces the clipboard (see [`super::clip`])
    pub const CLIPBOARD_SET: usize = 41;
    /// `clipboard_get(kind, buf, len) -> full length` (`-ENOENT` if nothing of that kind)
    pub const CLIPBOARD_GET: usize = 42;
    /// `drag_start(kind, ptr, len, count, icon)` — begins dragging `count` items (payload as for
    /// the clipboard, see [`super::clip`]) while the left button is held in one of the caller's
    /// windows; `icon` is one of [`super::drag`]
    pub const DRAG_START: usize = 43;
    /// `drag_data(buf, len) -> full length` — the payload of the last drop delivered to the caller
    pub const DRAG_DATA: usize = 44;
    /// `notify(title, title_len, body, body_len)` — shows a notification from the calling app
    pub const NOTIFY: usize = 45;
    /// `pref_get(key, key_len, buf, buf_len) -> full length` (`-ENOENT` if unset; see [`super::pref`])
    pub const PREF_GET: usize = 46;
    /// `pref_set(key, key_len, value, value_len)` — changes a system preference (applied at once)
    pub const PREF_SET: usize = 47;
    /// `set_datetime(*const DateTime)` — sets the clock
    pub const SET_DATETIME: usize = 48;
    /// `display_modes(*mut DisplayMode, max) -> count`
    pub const DISPLAY_MODES: usize = 49;
    /// `set_display(width, height)` — switches resolution now (`-ENOSYS` if only possible at boot)
    pub const SET_DISPLAY: usize = 50;
    /// `sys_stats(*mut SysStats)` — counters for Activity Monitor
    pub const SYS_STATS: usize = 51;
    pub const COUNT: usize = 52;
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
    pub const ETIMEDOUT: isize = 110;

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
            ETIMEDOUT => "timed out",
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

pub const MAX_CPUS: usize = 16;

/// Time a CPU spent busy and idle since boot (milliseconds).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CpuTime {
    pub busy_ms: u64,
    pub idle_ms: u64,
}

/// Live counters (all cumulative since boot, so callers compute rates).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SysStats {
    pub uptime_ms: u64,
    pub cpus: u32,
    pub tasks: u32,
    pub cpu: [CpuTime; MAX_CPUS],
    pub mem_total: u64,
    pub mem_used: u64,
    pub heap_size: u64,
    pub heap_used: u64,
    pub interrupts: u64,
    pub syscalls: u64,
    pub context_switches: u64,
    pub disk_reads: u64,
    pub disk_writes: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub frames: u64,
    pub windows: u32,
    pub processes: u32,
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

/// Clipboard content kinds.
pub mod clip {
    /// UTF-8 text.
    pub const TEXT: usize = 1;
    /// Absolute paths, one per line (copied files).
    pub const FILES: usize = 2;
    /// Largest clipboard content accepted.
    pub const MAX_LEN: usize = 4 << 20;
}

/// System preference keys (values are text; booleans are "0"/"1").
pub mod pref {
    pub const DARK: &str = "dark";
    /// Index into the accent palette.
    pub const ACCENT: &str = "accent";
    /// Keyboard layout id: us, uk, de, fr, es, se.
    pub const KEYBOARD: &str = "keyboard";
    /// Key repeat: delay before repeating (0–3 = 250–1000 ms) and rate (0 fastest – 31 slowest).
    pub const REPEAT_DELAY: &str = "repeat_delay";
    pub const REPEAT_RATE: &str = "repeat_rate";
    pub const CLOCK_24H: &str = "clock24";
    pub const CLOCK_SECONDS: &str = "clock_seconds";
    pub const CLOCK_DATE: &str = "clock_date";
    pub const DND: &str = "dnd";
    pub const VOLUME: &str = "volume";
    pub const MUTED: &str = "muted";
}

/// A screen resolution.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    /// [`display`] flags.
    pub flags: u32,
    pub _pad: u32,
}

pub mod display {
    /// The mode in use.
    pub const CURRENT: u32 = 1;
    /// Can be switched to right away (otherwise it applies at the next start).
    pub const LIVE: u32 = 2;
    /// Chosen for the next start (in `\aurora\boot.conf`).
    pub const AT_BOOT: u32 = 4;
}

/// Drag images for `drag_start`.
pub mod drag {
    pub const DOCUMENT: usize = 0;
    pub const FOLDER: usize = 1;
    pub const PICTURE: usize = 2;
    pub const TEXT: usize = 3;
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
    /// `a` = 1 for dark mode, `b` = wallpaper index, `c` = accent index.
    pub const THEME: u32 = 9;
    /// Something is being dragged over the window: `x`, `y`, `a` = kind, `b` = item count, `d` = modifiers.
    pub const DRAG_OVER: u32 = 10;
    /// The drag left the window (or was cancelled).
    pub const DRAG_LEAVE: u32 = 11;
    /// Dropped on the window at `x`, `y` (`a`, `b`, `d` as for `DRAG_OVER`); fetch it with `drag_data`.
    pub const DROP: u32 = 12;
    /// Sent to the window a drag started from: `a` = 1 if it was dropped somewhere.
    pub const DRAG_END: u32 = 13;
    /// The screen changed size: `x` = width, `y` = height.
    pub const SCREEN: u32 = 14;
}

/// A single input/window event. Field meaning depends on `kind`:
/// - pointer events: `x`, `y` (window-local), `a` = button (1 left, 2 right), `b` = click count;
///   for `POINTER_MOVE`, `a` = 1 while the left button is held (a drag, delivered even outside the window)
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
    /// Returns `(accent << 16) | (dark << 8) | wallpaper`.
    pub const GET_THEME: usize = 7;
    /// Use a picture as the wallpaper (arg = path of a PNG or BMP).
    pub const SET_WALLPAPER_IMAGE: usize = 8;
    /// Move a file or folder to the Trash (arg = path).
    pub const TRASH: usize = 9;
    /// Return an item from the Trash to where it came from (arg = its name in /Trash).
    pub const PUT_BACK: usize = 10;
    /// Permanently delete everything in the Trash.
    pub const EMPTY_TRASH: usize = 11;
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
