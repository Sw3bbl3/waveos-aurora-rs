//! System call dispatch. See `aurora_abi` for numbers and calling convention.

pub(crate) mod user;

use crate::arch::syscall::SyscallFrame;
use crate::fs;
use crate::proc::{self, Handle};
use crate::{mm, sched, time};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::{err::*, nr, DateTime, DirEntry, ProcInfo, Stat, SysInfo, NAME_MAX};
use core::sync::atomic::Ordering;

type SysResult = Result<u64, isize>;

/// Entry from the assembly stub (`arch::syscall`), with interrupts enabled.
pub extern "sysv64" fn dispatch(f: &mut SyscallFrame) {
    let a = [f.rdi, f.rsi, f.rdx, f.r10, f.r8, f.r9];
    crate::telemetry::syscall(f.rax as usize);
    let result = handle(f.rax as usize, a);
    f.rax = match result {
        Ok(v) => v,
        Err(e) => (-e.abs()) as u64,
    };
    proc::check_killed();
}

fn handle(nr: usize, a: [u64; 6]) -> SysResult {
    match nr {
        nr::EXIT => proc::exit_current(a[0] as i32 as i64),
        nr::YIELD => {
            sched::yield_now();
            Ok(0)
        }
        nr::SLEEP => {
            let ms = a[0].min(24 * 3600 * 1000);
            let until = time::uptime_ms() + ms;
            while time::uptime_ms() < until && !proc::interrupted() {
                sched::wait_until(until - time::uptime_ms(), proc::interrupted);
            }
            Ok(0)
        }
        nr::TIME_MS => Ok(time::uptime_ms()),
        nr::GETPID => Ok(sched::current_pid() as u64),
        nr::SPAWN => spawn(a),
        nr::WAIT => proc::wait(a[0] as u32, a[1]).map(|c| c as i32 as u32 as u64),
        nr::KILL => proc::kill(a[0] as u32).map(|_| 0),
        nr::PROC_LIST => proc_list(a[0], a[1]),
        nr::MMAP => mmap(a[0]),
        nr::MUNMAP => munmap(a[0], a[1]),
        nr::LOG => {
            let text = user::slice(a[0], a[1].min(4096))?;
            log_line(&text);
            Ok(0)
        }
        nr::OPEN => {
            let path = user::path(a[0], a[1])?;
            let file = fs::open(&path, a[2] as usize)?;
            let p = proc::current().ok_or(EPERM)?;
            p.install_fd(Arc::new(Handle::File(crate::sync::Mutex::new(file)))).map(|fd| fd as u64)
        }
        nr::READ => read(a[0] as usize, a[1], a[2]),
        nr::WRITE => write(a[0] as usize, a[1], a[2]),
        nr::SEEK => match &*proc::current().ok_or(EPERM)?.fd(a[0] as usize)? {
            Handle::File(f) => f.lock().seek(a[1] as i64, a[2] as usize),
            _ => Err(ESPIPE),
        },
        nr::CLOSE => {
            let p = proc::current().ok_or(EPERM)?;
            let mut fds = p.fds.lock();
            match fds.get_mut(a[0] as usize) {
                Some(slot @ Some(_)) => {
                    slot.take();
                    Ok(0)
                }
                _ => Err(EBADF),
            }
        }
        nr::TRUNCATE => match &*proc::current().ok_or(EPERM)?.fd(a[0] as usize)? {
            Handle::File(f) => f.lock().truncate(a[1]).map(|_| 0),
            _ => Err(EINVAL),
        },
        nr::STAT => {
            let path = user::path(a[0], a[1])?;
            let (md, ro) = fs::stat(&path)?;
            let st = Stat {
                kind: if md.kind == fs::Kind::Dir { aurora_abi::KIND_DIR } else { aurora_abi::KIND_FILE },
                read_only: ro as u32,
                size: md.size,
                mtime: md.mtime,
                ctime: md.ctime,
            };
            user::put(a[2], &st).map(|_| 0)
        }
        nr::READDIR => readdir(a),
        nr::MKDIR => fs::mkdir(&user::path(a[0], a[1])?).map(|_| 0),
        nr::UNLINK => fs::unlink(&user::path(a[0], a[1])?).map(|_| 0),
        nr::RENAME => fs::rename(&user::path(a[0], a[1])?, &user::path(a[2], a[3])?).map(|_| 0),
        nr::PIPE => {
            let p = proc::current().ok_or(EPERM)?;
            user::writable(a[0], 8)?; // validate before creating anything
            let (r, w) = proc::new_pipe(a[1] & 1 != 0);
            let rfd = p.install_fd(r)?;
            let wfd = match p.install_fd(w) {
                Ok(fd) => fd,
                Err(e) => {
                    p.fds.lock()[rfd] = None;
                    return Err(e);
                }
            };
            user::put(a[0], &[rfd as i32, wfd as i32]).map(|_| 0)
        }
        nr::SYS_INFO => user::put(a[0], &sys_info()).map(|_| 0),
        nr::DATETIME => {
            let d = crate::drivers::rtc::now();
            let dt = DateTime {
                year: d.year,
                month: d.month,
                day: d.day,
                hour: d.hour,
                minute: d.minute,
                second: d.second,
                weekday: d.weekday(),
            };
            user::put(a[0], &dt).map(|_| 0)
        }
        nr::SYNC => {
            fs::sync_all();
            Ok(0)
        }
        nr::CHDIR => {
            let path = user::path(a[0], a[1])?;
            if !fs::is_dir(&path) {
                return Err(ENOTDIR);
            }
            *proc::current().ok_or(EPERM)?.cwd.lock() = path;
            Ok(0)
        }
        nr::GETCWD => {
            let cwd = proc::current().ok_or(EPERM)?.cwd.lock().clone();
            let mut out = user::slice_mut(a[0], a[1])?;
            let n = cwd.len().min(out.len());
            out[..n].copy_from_slice(&cwd.as_bytes()[..n]);
            out.commit(n)?;
            Ok(cwd.len() as u64)
        }
        nr::THREAD_SPAWN => proc::spawn_thread(a[0], a[1], a[2]),
        nr::THREAD_EXIT => {
            if a[0] != 0 {
                proc::futex::notify_exit(a[0]);
            }
            proc::exit_thread()
        }
        nr::FUTEX_WAIT => proc::futex::wait(a[0], a[1] as u32, a[2]),
        nr::FUTEX_WAKE => proc::futex::wake(a[0], a[1]),
        nr::CLIPBOARD_SET => {
            let data = user::slice(a[1], a[2].min(aurora_abi::clip::MAX_LEN as u64 + 1))?;
            crate::gui::clipboard::set(a[0] as usize, &data).map(|_| 0)
        }
        nr::CLIPBOARD_GET => {
            let mut out = user::slice_mut(a[1], a[2].min(aurora_abi::clip::MAX_LEN as u64))?;
            let n = crate::gui::clipboard::get(a[0] as usize, &mut out)?;
            out.commit(n)?;
            Ok(n as u64)
        }
        nr::DRAG_START => {
            let data = user::slice(a[1], a[2].min(aurora_abi::clip::MAX_LEN as u64 + 1))?;
            crate::gui::server::drag_start(sched::current_pid(), a[0] as usize, &data, a[3] as u32, a[4] as usize)
                .map(|_| 0)
        }
        nr::NOTIFY => {
            let title = user::str(a[0], a[1].min(512))?;
            let body = user::str(a[2], a[3].min(2048))?;
            let p = proc::current().ok_or(EPERM)?;
            crate::gui::notify::post(p.pid, &p.path, &title, &body);
            Ok(0)
        }
        nr::PREF_GET => {
            let key = user::str(a[0], a[1].min(64))?;
            let v = crate::gui::prefs::get(&key).ok_or(ENOENT)?;
            let mut out = user::slice_mut(a[2], a[3].min(4096))?;
            let n = v.len().min(out.len());
            out[..n].copy_from_slice(&v.as_bytes()[..n]);
            out.commit(n)?;
            Ok(v.len() as u64)
        }
        nr::PREF_SET => {
            let key = user::str(a[0], a[1].min(64))?;
            let value = user::str(a[2], a[3].min(1024))?;
            crate::gui::prefs::set(&key, &value).map(|_| 0)
        }
        nr::SET_DATETIME => {
            let d: DateTime = user::get(a[0])?;
            let ok = (2000..2100).contains(&d.year)
                && (1..=12).contains(&d.month)
                && (1..=31).contains(&d.day)
                && d.hour < 24
                && d.minute < 60
                && d.second < 60;
            if !ok {
                return Err(EINVAL);
            }
            let rd = crate::drivers::rtc::DateTime {
                year: d.year,
                month: d.month,
                day: d.day,
                hour: d.hour,
                minute: d.minute,
                second: d.second,
            };
            time::set_wall_clock(&rd);
            Ok(0)
        }
        nr::DISPLAY_MODES => {
            let (w, h) = crate::gui::screen_size();
            let modes = crate::drivers::display::modes((w as u32, h as u32), crate::gui::prefs::boot_resolution());
            let max = (a[1] as usize).min(64);
            let size = core::mem::size_of::<aurora_abi::DisplayMode>();
            let mut out = user::slice_mut(a[0], (max * size) as u64)?;
            let n = modes.len().min(max);
            for (i, m) in modes.iter().take(n).enumerate() {
                let bytes = unsafe { core::slice::from_raw_parts(m as *const _ as *const u8, size) };
                out[i * size..(i + 1) * size].copy_from_slice(bytes);
            }
            out.commit(n * size)?;
            Ok(modes.len() as u64)
        }
        nr::SET_DISPLAY => crate::gui::prefs::set_display(a[0] as u32, a[1] as u32),
        nr::SYS_STATS => user::put(a[0], &sys_stats()).map(|_| 0),
        nr::DRAG_DATA => {
            let mut out = user::slice_mut(a[0], a[1].min(aurora_abi::clip::MAX_LEN as u64))?;
            let n = crate::gui::dnd::data(sched::current_pid(), &mut out)?;
            out.commit(n)?;
            Ok(n as u64)
        }
        nr::DESKTOP => crate::gui::server::desktop_request(a[0] as usize, a[1], a[2]),
        nr::WIN_CREATE
        | nr::WIN_SURFACE
        | nr::WIN_PRESENT
        | nr::WIN_SET_TITLE
        | nr::WIN_CLOSE
        | nr::WIN_RESIZE
        | nr::NEXT_EVENT => crate::gui::server::syscall(nr, a),
        _ => Err(ENOSYS),
    }
}

fn log_line(text: &[u8]) {
    let name = proc::current().map(|p| p.name.clone()).unwrap_or_default();
    let text = String::from_utf8_lossy(text);
    for line in text.trim_end_matches('\n').split('\n') {
        log!(name.as_str(), "{}", line);
    }
}

fn spawn(a: [u64; 6]) -> SysResult {
    let me = proc::current().ok_or(EPERM)?;
    let path = user::path(a[0], a[1])?;
    let block = user::slice(a[2], a[3].min(64 * 1024))?;
    let args: Vec<&str> =
        block.split(|&b| b == 0).filter(|s| !s.is_empty()).map(|s| core::str::from_utf8(s).unwrap_or("")).collect();
    if args.len() > 256 {
        return Err(EINVAL);
    }
    let mut stdio: [Option<Arc<Handle>>; 3] = [None, None, None];
    if a[4] != 0 {
        let fds: [i32; 3] = user::get(a[4])?;
        for (slot, fd) in stdio.iter_mut().zip(fds) {
            if fd >= 0 {
                *slot = Some(me.fd(fd as usize)?);
            }
        }
    }
    proc::spawn(&path, &args, stdio, me.pid).map(|pid| pid as u64)
}

fn read(fd: usize, ptr: u64, len: u64) -> SysResult {
    let h = proc::current().ok_or(EPERM)?.fd(fd)?;
    let mut buf = user::slice_mut(ptr, len)?;
    let n = match &*h {
        Handle::File(f) => f.lock().read(&mut buf)?,
        Handle::PipeRead { pipe, nonblocking } => pipe.read(&mut buf, *nonblocking)?,
        Handle::PipeWrite(_) | Handle::Log => return Err(EBADF),
    };
    buf.commit(n)?;
    Ok(n as u64)
}

fn write(fd: usize, ptr: u64, len: u64) -> SysResult {
    let h = proc::current().ok_or(EPERM)?.fd(fd)?;
    let buf = user::slice(ptr, len)?;
    let n = match &*h {
        Handle::File(f) => f.lock().write(&buf)?,
        Handle::PipeWrite(pipe) => pipe.write(&buf)?,
        Handle::Log => {
            log_line(&buf);
            buf.len()
        }
        Handle::PipeRead { .. } => return Err(EBADF),
    };
    Ok(n as u64)
}

fn mmap(len: u64) -> SysResult {
    if len == 0 || len > 1 << 30 {
        return Err(EINVAL);
    }
    let pages = len.div_ceil(mm::PAGE_SIZE);
    let p = proc::current().ok_or(EPERM)?;
    let mut guard = p.aspace.lock();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    let va = aspace.reserve(pages).ok_or(ENOMEM)?;
    if let Err(e) = aspace.map_anon(va, pages, mm::vmm::Prot::ReadWrite) {
        aspace.unmap(va, pages);
        return Err(e);
    }
    p.resident_kib.store(aspace.resident / 1024, Ordering::Relaxed);
    Ok(va)
}

fn munmap(addr: u64, len: u64) -> SysResult {
    use aurora_abi::layout::{MMAP_BASE, MMAP_END};
    if addr % mm::PAGE_SIZE != 0 || addr < MMAP_BASE || addr.saturating_add(len) > MMAP_END {
        return Err(EINVAL);
    }
    let p = proc::current().ok_or(EPERM)?;
    let mut guard = p.aspace.lock();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    aspace.unmap(addr, len.div_ceil(mm::PAGE_SIZE));
    p.resident_kib.store(aspace.resident / 1024, Ordering::Relaxed);
    Ok(0)
}

fn readdir(a: [u64; 6]) -> SysResult {
    let path = user::path(a[0], a[1])?;
    let entries = fs::readdir(&path)?;
    let max = (a[3] as usize).min(4096);
    let mut out = user::slice_mut(a[2], (max * core::mem::size_of::<DirEntry>()) as u64)?;
    let size = core::mem::size_of::<DirEntry>();
    let count = entries.len().min(max);
    for (i, e) in entries.iter().take(max).enumerate() {
        let mut d = DirEntry::zeroed();
        d.kind = if e.kind == fs::Kind::Dir { aurora_abi::KIND_DIR } else { aurora_abi::KIND_FILE };
        let n = e.name.len().min(NAME_MAX);
        d.name[..n].copy_from_slice(&e.name.as_bytes()[..n]);
        d.name_len = n as u32;
        d.size = e.size;
        d.mtime = e.mtime;
        let bytes = unsafe { core::slice::from_raw_parts(&d as *const DirEntry as *const u8, size) };
        out[i * size..(i + 1) * size].copy_from_slice(bytes);
    }
    out.commit(count * size)?;
    Ok(entries.len() as u64)
}

fn fixed<const N: usize>(s: &str) -> ([u8; N], u32) {
    let mut buf = [0u8; N];
    let n = s.len().min(N);
    buf[..n].copy_from_slice(&s.as_bytes()[..n]);
    (buf, n as u32)
}

fn proc_list(ptr: u64, max: u64) -> SysResult {
    let max = max.min(512) as usize;
    let mut out = user::slice_mut(ptr, (max * core::mem::size_of::<ProcInfo>()) as u64)?;
    let tasks = sched::list();
    let mut rows: Vec<ProcInfo> = Vec::new();
    // Idle tasks are left out: their time is the "idle" share in SYS_STATS.
    for t in tasks.iter().filter(|t| t.pid == 0 && !t.idle) {
        let (name, name_len) = fixed::<32>(&t.name);
        rows.push(ProcInfo {
            pid: t.id as u32,
            parent: 0,
            state: state_of(t.state),
            threads: 1,
            cpu_ms: t.cpu_ticks,
            mem_kib: 0,
            user: 0,
            name_len,
            name,
        });
    }
    for p in proc::all() {
        let mine: Vec<&sched::TaskInfo> = tasks.iter().filter(|t| t.pid == p.pid).collect();
        let (name, name_len) = fixed::<32>(&p.name);
        let state = match (p.exit_code(), mine.first()) {
            (Some(_), _) | (None, None) => aurora_abi::PROC_EXITED,
            (None, Some(t)) => state_of(t.state),
        };
        rows.push(ProcInfo {
            pid: p.pid,
            parent: p.parent,
            state,
            threads: mine.len() as u32,
            cpu_ms: mine.iter().map(|t| t.cpu_ticks).sum(),
            mem_kib: p.resident_kib.load(Ordering::Relaxed),
            user: 1,
            name_len,
            name,
        });
    }
    let size = core::mem::size_of::<ProcInfo>();
    for (i, r) in rows.iter().take(max).enumerate() {
        let bytes = unsafe { core::slice::from_raw_parts(r as *const ProcInfo as *const u8, size) };
        out[i * size..(i + 1) * size].copy_from_slice(bytes);
    }
    out.commit(rows.len().min(max) * size)?;
    Ok(rows.len() as u64)
}

fn state_of(s: sched::State) -> u32 {
    match s {
        sched::State::Running => aurora_abi::PROC_RUNNING,
        sched::State::Ready => aurora_abi::PROC_READY,
        sched::State::Sleeping(_) => aurora_abi::PROC_SLEEPING,
        sched::State::Dead => aurora_abi::PROC_EXITED,
    }
}

fn sys_stats() -> aurora_abi::SysStats {
    let mut s = aurora_abi::SysStats { uptime_ms: time::uptime_ms(), ..Default::default() };
    let tasks = sched::list();
    s.tasks = tasks.iter().filter(|t| !t.idle).count() as u32;
    let cpus = sched::cpu_times();
    s.cpus = cpus.len().min(aurora_abi::MAX_CPUS) as u32;
    for (i, (busy, idle)) in cpus.iter().take(aurora_abi::MAX_CPUS).enumerate() {
        s.cpu[i] = aurora_abi::CpuTime { busy_ms: *busy, idle_ms: *idle };
    }
    let m = mm::stats();
    (s.mem_total, s.mem_used, s.heap_size, s.heap_used) = (m.total_bytes, m.used_bytes, m.heap_size, m.heap_used);
    (s.interrupts, s.syscalls, s.context_switches) = crate::telemetry::totals();
    for d in crate::drivers::block::stats() {
        s.disk_reads += d.reads;
        s.disk_writes += d.writes;
        s.disk_read_bytes += d.read_bytes;
        s.disk_write_bytes += d.write_bytes;
    }
    s.frames = crate::telemetry::FRAMES.load(Ordering::Relaxed);
    s.windows = crate::gui::server::windows().len() as u32;
    s.processes = proc::all().iter().filter(|p| p.exit_code().is_none()).count() as u32;
    s
}

fn sys_info() -> SysInfo {
    let m = mm::stats();
    let (sw, sh) = crate::gui::screen_size();
    let (cpu, cpu_len) = fixed::<48>(&crate::arch::cpu::brand());
    let (version, version_len) = fixed::<16>(crate::VERSION);
    let root_fs = fs::lookup("/").ok().map(|(f, _)| f);
    let (root, root_len) = fixed::<48>(&root_fs.as_ref().map(|f| f.describe()).unwrap_or_default());
    let (disk_total, disk_free) = root_fs.and_then(|f| f.space()).unwrap_or((0, 0));
    SysInfo {
        mem_total: m.total_bytes,
        mem_used: m.used_bytes,
        heap_size: m.heap_size,
        heap_used: m.heap_used,
        uptime_ms: time::uptime_ms(),
        screen_w: sw as u32,
        screen_h: sh as u32,
        processes: proc::all().iter().filter(|p| p.exit_code().is_none()).count() as u32,
        cpu_len,
        cpu,
        version_len,
        version,
        root_len,
        root,
        disk_total,
        disk_free,
    }
}
