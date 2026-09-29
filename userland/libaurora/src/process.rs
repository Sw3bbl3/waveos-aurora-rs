//! Processes, system information and desktop requests.

use crate::abi::{self, nr, ProcInfo, SysInfo};
use crate::sys::{call, str_args};
use crate::Result;
use alloc::string::String;
use alloc::vec::Vec;

pub fn exit(code: i32) -> ! {
    let _ = call(nr::EXIT, &[code as u64]);
    unreachable!()
}

pub fn pid() -> u32 {
    call(nr::GETPID, &[]).unwrap_or(0) as u32
}

pub fn yield_now() {
    let _ = call(nr::YIELD, &[]);
}

/// Standard stream assignment for a child: parent fds, or `None` for the log console.
#[derive(Clone, Copy, Default)]
pub struct Stdio {
    pub stdin: Option<u64>,
    pub stdout: Option<u64>,
    pub stderr: Option<u64>,
}

/// Starts a program. `args` are passed after the program path (which becomes `args[0]`).
pub fn spawn(path: &str, args: &[&str], stdio: Stdio) -> Result<u32> {
    let mut block = Vec::new();
    for a in core::iter::once(&path).chain(args.iter()) {
        block.extend_from_slice(a.as_bytes());
        block.push(0);
    }
    let fd = |f: Option<u64>| f.map(|v| v as i32).unwrap_or(-1);
    let fds = [fd(stdio.stdin), fd(stdio.stdout), fd(stdio.stderr)];
    let [p, l] = str_args(path);
    call(nr::SPAWN, &[p, l, block.as_ptr() as u64, block.len() as u64, fds.as_ptr() as u64]).map(|pid| pid as u32)
}

/// Waits for a child; `None` timeout blocks until it exits.
pub fn wait(pid: u32, timeout_ms: Option<u64>) -> Result<i32> {
    call(nr::WAIT, &[pid as u64, timeout_ms.unwrap_or(u64::MAX)]).map(|c| c as u32 as i32)
}

pub fn kill(pid: u32) -> Result<()> {
    call(nr::KILL, &[pid as u64]).map(|_| ())
}

/// `[read_end, write_end]`
pub fn pipe(nonblocking_read: bool) -> Result<[u64; 2]> {
    let mut fds = [0i32; 2];
    call(nr::PIPE, &[fds.as_mut_ptr() as u64, nonblocking_read as u64])?;
    Ok([fds[0] as u64, fds[1] as u64])
}

pub fn close(fd: u64) {
    let _ = call(nr::CLOSE, &[fd]);
}

pub fn list() -> Vec<ProcInfo> {
    let mut buf: Vec<ProcInfo> = Vec::with_capacity(128);
    let n = call(nr::PROC_LIST, &[buf.as_mut_ptr() as u64, 128]).unwrap_or(0) as usize;
    unsafe { buf.set_len(n.min(128)) };
    buf
}

pub fn sys_info() -> SysInfo {
    let mut info = core::mem::MaybeUninit::<SysInfo>::zeroed();
    let _ = call(nr::SYS_INFO, &[info.as_mut_ptr() as u64]);
    unsafe { info.assume_init() }
}

pub fn fixed_str(bytes: &[u8], len: u32) -> String {
    String::from_utf8_lossy(&bytes[..(len as usize).min(bytes.len())]).into_owned()
}

/// Requests to the desktop shell.
pub mod desktop {
    use super::*;

    fn req(r: usize, arg: &str) -> Result<u64> {
        let [p, l] = str_args(arg);
        call(nr::DESKTOP, &[r as u64, p, l])
    }

    /// Opens an app by name ("Notes") or program path.
    pub fn open_app(name: &str) -> Result<()> {
        let path = if name.starts_with('/') { String::from(name) } else { alloc::format!("/System/Apps/{name}.elf") };
        req(abi::desktop::OPEN_APP, &path).map(|_| ())
    }

    pub fn open_file(path: &str) -> Result<()> {
        req(abi::desktop::OPEN_FILE, path).map(|_| ())
    }

    pub fn set_dark(dark: bool) {
        let _ = call(nr::DESKTOP, &[abi::desktop::SET_DARK as u64, 0, dark as u64]);
    }

    pub fn set_wallpaper(i: u8) {
        let _ = call(nr::DESKTOP, &[abi::desktop::SET_WALLPAPER as u64, 0, i as u64]);
    }

    pub fn set_wallpaper_image(path: &str) -> Result<()> {
        req(abi::desktop::SET_WALLPAPER_IMAGE, path).map(|_| ())
    }

    /// Moves a file or folder to the Trash.
    pub fn trash(path: &str) -> Result<()> {
        req(abi::desktop::TRASH, path).map(|_| ())
    }

    /// Returns an item (by its name in /Trash) to where it came from.
    pub fn put_back(name: &str) -> Result<()> {
        req(abi::desktop::PUT_BACK, name).map(|_| ())
    }

    pub fn empty_trash() -> Result<()> {
        req(abi::desktop::EMPTY_TRASH, "").map(|_| ())
    }

    /// (dark, wallpaper index)
    pub fn theme() -> (bool, u8) {
        let v = call(nr::DESKTOP, &[abi::desktop::GET_THEME as u64, 0, 0]).unwrap_or(0);
        (v >> 8 != 0, v as u8)
    }

    pub fn shutdown() {
        let _ = call(nr::DESKTOP, &[abi::desktop::SHUTDOWN as u64, 0, 0]);
    }

    pub fn reboot() {
        let _ = call(nr::DESKTOP, &[abi::desktop::REBOOT as u64, 0, 0]);
    }
}
