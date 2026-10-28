//! New processes for Linux programs: `fork`, `vfork` and process-making
//! `clone`, `execve`, `wait4`/`waitid`, process groups and sessions.

use super::{exec, fs, signal, Linux};
use crate::arch::syscall::SyscallFrame;
use crate::mm::vmm::AddressSpace;
use crate::proc::{self, Process};
use crate::sched;
use crate::syscall::user;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::Ordering;

const CLONE_VM: u64 = 0x100;
const CLONE_VFORK: u64 = 0x4000;
const CLONE_THREAD: u64 = 0x10000;
const CLONE_SETTLS: u64 = 0x80000;
const CLONE_PARENT_SETTID: u64 = 0x100000;
const CLONE_CHILD_CLEARTID: u64 = 0x200000;
const CLONE_CHILD_SETTID: u64 = 0x1000000;

/// `clone`: a thread (`CLONE_THREAD`) or a new process — a copy of the
/// caller (`fork`), or, with `CLONE_VM | CLONE_VFORK`, a child that runs in
/// the caller's memory while the caller waits for it to `exec` or end.
pub fn clone(
    p: &Process,
    f: &SyscallFrame,
    flags: u64,
    stack: u64,
    ptid: u64,
    ctid: u64,
    tls: u64,
) -> Result<u64, isize> {
    if flags & CLONE_THREAD != 0 {
        return super::misc::clone_thread(p, f, flags, stack, ptid, ctid, tls);
    }
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    let share_vm = flags & CLONE_VM != 0;
    if share_vm && flags & CLONE_VFORK == 0 {
        // Threads that aren't threads (LinuxThreads-style): not supported.
        return Err(EINVAL);
    }
    let mut frame = *f;
    frame.rax = 0;
    if stack != 0 {
        frame.rsp = stack;
    }
    let fs_base = if flags & CLONE_SETTLS != 0 { tls } else { sched::fs_base() };
    let child_linux = l.for_child();
    let child = proc::fork(p, frame, fs_base, share_vm, child_linux)?;
    let pid = child.pid as u64;
    if flags & CLONE_PARENT_SETTID != 0 {
        user::put(ptid, &(pid as u32))?;
    }
    if flags & CLONE_CHILD_SETTID != 0 {
        // In the child's memory (the same as ours when it is shared).
        if let Some(a) = child.mem().as_ref() {
            let _ = a.write_bytes(ctid, &(pid as u32).to_le_bytes());
        }
    }
    if flags & CLONE_CHILD_CLEARTID != 0 {
        if let Some(cl) = &child.linux {
            cl.clear_tid.lock().insert(child.main_task.load(Ordering::Acquire), ctid);
        }
    }
    if flags & CLONE_VFORK != 0 {
        // The parent waits until the child no longer needs its memory.
        while !child.vfork_done.load(Ordering::Acquire) {
            if p.killed.load(Ordering::Acquire) {
                break;
            }
            sched::wait_until(50, || child.vfork_done.load(Ordering::Acquire) || p.killed.load(Ordering::Acquire));
        }
    }
    Ok(pid)
}

/// Reads a NULL-terminated array of string pointers (`argv`, `envp`).
fn string_array(ptr: u64) -> Result<Vec<String>, isize> {
    let mut out = Vec::new();
    if ptr == 0 {
        return Ok(out);
    }
    let mut total = 0usize;
    for i in 0..65536u64 {
        let s: u64 = user::get(ptr + i * 8)?;
        if s == 0 {
            return Ok(out);
        }
        let v = user::cstr(s)?;
        total += v.len() + 1;
        if total > 2 << 20 {
            return Err(E2BIG);
        }
        out.push(v);
    }
    Err(E2BIG)
}

/// `execve`/`execveat`.
pub fn execve(
    p: &Arc<Process>,
    f: &mut SyscallFrame,
    dirfd: u64,
    path: u64,
    argv: u64,
    envp: u64,
) -> Result<u64, isize> {
    let guest = fs::path_arg(p, dirfd, path)?;
    let argv = string_array(argv)?;
    let envp = string_array(envp)?;
    exec_guest(p, f, guest, argv, envp, 0)
}

/// Runs the Linux program at `guest` in place of the caller's.
fn exec_guest(
    p: &Arc<Process>,
    f: &mut SyscallFrame,
    guest: String,
    argv: Vec<String>,
    envp: Vec<String>,
    depth: u32,
) -> Result<u64, isize> {
    let host = fs::resolve_host(&guest)?;
    if crate::fs::is_dir(&host) {
        return Err(EACCES);
    }
    let data = crate::fs::read_all(&host)?;
    // A script: run its interpreter, with the script's path as an argument.
    if data.starts_with(b"#!") {
        if depth >= 4 {
            return Err(ELOOP);
        }
        let line_end = data.iter().position(|&b| b == b'\n').unwrap_or(data.len()).min(256);
        let line = core::str::from_utf8(&data[2..line_end]).map_err(|_| ENOEXEC)?.trim();
        let (interp, arg) = match line.split_once([' ', '\t']) {
            Some((i, a)) => (i, Some(a.trim())),
            None => (line, None),
        };
        if interp.is_empty() {
            return Err(ENOEXEC);
        }
        let mut new_argv = Vec::with_capacity(argv.len() + 2);
        new_argv.push(String::from(interp));
        if let Some(a) = arg.filter(|a| !a.is_empty()) {
            new_argv.push(String::from(a));
        }
        new_argv.push(guest.clone());
        new_argv.extend(argv.into_iter().skip(1));
        let interp = fs::absolute(p, interp);
        return exec_guest(p, f, interp, new_argv, envp, depth + 1);
    }
    let image = aurora_elf::parse(&data).ok_or(ENOEXEC)?;
    if !super::is_linux(&image, &host) {
        return run_waveos(p, &host, &argv);
    }
    let mut aspace = AddressSpace::new().ok_or(ENOMEM)?;
    let args: Vec<&str> = argv.iter().map(String::as_str).collect();
    let start = exec::load(&data, &image, &guest, &args, &envp, &mut aspace)?;
    drop(data);

    // From here on the old program is gone.
    proc::exec_single_thread(p)?;
    let l = p.linux.as_ref().ok_or(EINVAL)?;
    l.exec_reset(start.brk);
    close_on_exec(p, l);
    proc::exec_replace(p, aspace, &host);
    sched::set_fs_base(0);
    *f = SyscallFrame { rip: start.entry, rsp: start.sp, rflags: 0x202, ..Default::default() };
    Ok(0)
}

/// Closes the descriptors marked close-on-exec.
fn close_on_exec(p: &Process, l: &Linux) {
    let mut bits = l.cloexec.lock();
    let mut closed = Vec::new();
    {
        let mut fds = p.fds.lock();
        for (i, slot) in fds.iter_mut().enumerate() {
            if bits[i / 64] & (1 << (i % 64)) != 0 {
                closed.extend(slot.take());
            }
        }
    }
    *bits = [0; 16];
    drop(bits);
    drop(closed); // pipe ends notice their peers going outside the fd lock
}

/// `exec` of a WaveOS program: it runs as a process of its own with the
/// caller's standard streams, and the caller ends with its exit code.
fn run_waveos(p: &Process, host: &str, argv: &[String]) -> Result<u64, isize> {
    let stdio = [p.fd(0).ok(), p.fd(1).ok(), p.fd(2).ok()];
    let args: Vec<&str> = argv.iter().skip(1).map(String::as_str).collect();
    let mut full = Vec::with_capacity(args.len() + 1);
    full.push(host);
    full.extend(args);
    let pid = proc::spawn(host, &full, stdio, p.pid)?;
    let code = loop {
        match proc::wait(pid, u64::MAX) {
            Ok(c) => break c,
            Err(_) if p.killed.load(Ordering::Acquire) => {
                let _ = proc::kill(pid);
                break 1;
            }
            Err(_) => continue,
        }
    };
    proc::exit_current(if code < 0 { 128 - code } else { code & 0xFF });
}

/// The `wait` status word for exit code `code` (negative: ended by signal `-code`).
fn status(code: i64) -> u32 {
    if code < 0 {
        ((-code) as u32) & 0x7F
    } else {
        ((code as u32) & 0xFF) << 8
    }
}

const WNOHANG: u64 = 1;
const WNOWAIT: u64 = 0x0100_0000;

fn which(p: &Process, pid: i64) -> proc::Which {
    match pid {
        -1 => proc::Which::Any,
        0 => proc::Which::Group(p.pgid.load(Ordering::Relaxed)),
        n if n < -1 => proc::Which::Group((-n) as u32),
        n => proc::Which::Pid(n as u32),
    }
}

/// `wait4`.
pub fn wait4(p: &Process, pid: i64, status_ptr: u64, options: u64, rusage: u64) -> Result<u64, isize> {
    match proc::wait_child(which(p, pid), options & WNOHANG != 0, true)? {
        None => Ok(0),
        Some((child, code)) => {
            if status_ptr != 0 {
                user::put(status_ptr, &status(code))?;
            }
            if rusage != 0 {
                user::put(rusage, &[0u8; 144])?;
            }
            Ok(child as u64)
        }
    }
}

/// `waitid`.
pub fn waitid(p: &Process, idtype: u64, id: u64, info_ptr: u64, options: u64) -> Result<u64, isize> {
    let w = match idtype {
        0 => proc::Which::Any,
        1 => proc::Which::Pid(id as u32),
        2 => proc::Which::Group(if id == 0 { p.pgid.load(Ordering::Relaxed) } else { id as u32 }),
        _ => return Err(EINVAL),
    };
    let found = proc::wait_child(w, options & WNOHANG != 0, options & WNOWAIT == 0)?;
    if info_ptr != 0 {
        let mut si = [0u8; 128];
        if let Some((child, code)) = found {
            si[0..4].copy_from_slice(&signal::SIGCHLD.to_le_bytes());
            let (cld, st) = if code < 0 { (2i32, (-code) as i32) } else { (1i32, code as i32) };
            si[8..12].copy_from_slice(&cld.to_le_bytes());
            si[16..20].copy_from_slice(&child.to_le_bytes());
            si[20..24].copy_from_slice(&(super::UID as u32).to_le_bytes());
            si[24..28].copy_from_slice(&st.to_le_bytes());
        }
        user::put_bytes(info_ptr, &si)?;
    }
    Ok(0)
}

/// `setpgid`.
pub fn setpgid(p: &Process, pid: u64, pgid: u64) -> Result<u64, isize> {
    let target = if pid == 0 || pid == p.pid as u64 {
        proc::get(p.pid).ok_or(ESRCH)?
    } else {
        let c = proc::get(pid as u32).ok_or(ESRCH)?;
        if c.parent != p.pid {
            return Err(ESRCH);
        }
        c
    };
    let g = if pgid == 0 { target.pid } else { pgid as u32 };
    target.pgid.store(g, Ordering::Relaxed);
    Ok(0)
}

pub fn getpgid(p: &Process, pid: u64) -> Result<u64, isize> {
    let t = if pid == 0 { proc::get(p.pid) } else { proc::get(pid as u32) }.ok_or(ESRCH)?;
    Ok(t.pgid.load(Ordering::Relaxed) as u64)
}

pub fn getsid(p: &Process, pid: u64) -> Result<u64, isize> {
    let t = if pid == 0 { proc::get(p.pid) } else { proc::get(pid as u32) }.ok_or(ESRCH)?;
    Ok(t.sid.load(Ordering::Relaxed) as u64)
}

/// `setsid`: a new session and group, led by the caller.
pub fn setsid(p: &Process) -> Result<u64, isize> {
    if p.pgid.load(Ordering::Relaxed) == p.pid && p.sid.load(Ordering::Relaxed) != p.pid {
        // Already leads a group (but not a session): Linux refuses.
        return Err(EPERM);
    }
    p.pgid.store(p.pid, Ordering::Relaxed);
    p.sid.store(p.pid, Ordering::Relaxed);
    Ok(p.pid as u64)
}
