//! Crest's client interface: windows owned by user processes.
//!
//! A client window has a *surface* — pixel memory mapped both into the kernel
//! (so the compositor can read it) and into the owning process (so the app can
//! draw into it) — plus a queue of input events. System calls (running on the
//! client's task) and the compositor task share this state under `CLIENTS`;
//! structural changes are forwarded to the compositor through `COMMANDS`.

use super::geom::Rect;
use crate::mm::vmm::{self, Prot};
use crate::mm::{frame, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use crate::{proc, sched};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::vec::Vec;
use aurora_abi::{err::*, nr, Event, SurfaceInfo};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const MAX_EVENTS: usize = 256;

struct Surface {
    frames: Vec<u64>,
    kva: u64,
    user_va: u64,
    width: u32,
    height: u32,
}

impl Surface {
    fn pages(&self) -> u64 {
        self.frames.len() as u64
    }
}

pub struct Client {
    pub pid: u32,
    pub title: String,
    pub resizable: bool,
    pub initial: (i32, i32),
    surface: Surface,
    events: VecDeque<Event>,
}

/// Changes the compositor must apply on its next frame.
pub enum Command {
    Created(u32),
    Closed(u32),
    Present(u32, Rect),
    Title(u32),
    OpenApp(String),
    OpenFile(String),
    SetDark(bool),
    SetWallpaper(u8),
    Shutdown,
    Reboot,
}

static CLIENTS: IrqMutex<BTreeMap<u32, Client>> = IrqMutex::new(BTreeMap::new());
static COMMANDS: IrqMutex<VecDeque<Command>> = IrqMutex::new(VecDeque::new());
/// Task blocked in `next_event`, per process.
static WAITERS: IrqMutex<BTreeMap<u32, u64>> = IrqMutex::new(BTreeMap::new());
static NEXT_WINDOW: AtomicU32 = AtomicU32::new(1);
static COMPOSITOR: AtomicU64 = AtomicU64::new(u64::MAX);

pub fn set_compositor(task: u64) {
    COMPOSITOR.store(task, Ordering::Relaxed);
}

fn send(cmd: Command) {
    COMMANDS.lock().push_back(cmd);
    let t = COMPOSITOR.load(Ordering::Relaxed);
    if t != u64::MAX {
        sched::wake(t);
    }
}

pub fn take_commands() -> Vec<Command> {
    COMMANDS.lock().drain(..).collect()
}

pub fn pending() -> bool {
    !COMMANDS.lock().is_empty()
}

// ------------------------------------------------------------ surfaces

fn alloc_surface(pid: u32, width: u32, height: u32) -> Result<Surface, isize> {
    let bytes = width as u64 * height as u64 * 4;
    let pages = bytes.div_ceil(PAGE_SIZE);
    let mut frames = Vec::with_capacity(pages as usize);
    for _ in 0..pages {
        match frame::alloc() {
            Some(f) => {
                unsafe { core::ptr::write_bytes(phys_to_virt(f) as *mut u8, 0xFF, PAGE_SIZE as usize) };
                frames.push(f)
            }
            None => {
                frames.iter().for_each(|&f| frame::free(f));
                return Err(ENOMEM);
            }
        }
    }
    let Some(kva) = vmm::kmap(&frames) else {
        frames.iter().for_each(|&f| frame::free(f));
        return Err(ENOMEM);
    };
    let p = proc::get(pid).ok_or(ESRCH)?;
    let mut guard = p.aspace.lock();
    let aspace = guard.as_mut().ok_or(ESRCH)?;
    let user_va = aspace.reserve(pages).ok_or(ENOMEM)?;
    aspace.map_shared(user_va, &frames, Prot::ReadWrite)?;
    Ok(Surface { frames, kva, user_va, width, height })
}

/// Releases a surface. `unmap_user` is false when the process's address space
/// is being torn down anyway.
fn free_surface(s: Surface, pid: u32, unmap_user: bool) {
    if unmap_user {
        if let Some(p) = proc::get(pid) {
            if let Some(a) = p.aspace.lock().as_mut() {
                a.unmap(s.user_va, s.pages());
            }
        }
    }
    vmm::kunmap(s.kva, s.pages());
    for f in s.frames {
        frame::free(f);
    }
}

// ------------------------------------------------------- compositor side

/// Runs `f` with the window's pixels (row stride = width). Holds the client
/// lock, so the surface cannot be freed underneath the compositor.
pub fn with_surface<R>(id: u32, f: impl FnOnce(&[u32], u32, u32) -> R) -> Option<R> {
    let clients = CLIENTS.lock();
    let c = clients.get(&id)?;
    let s = &c.surface;
    let px = unsafe { core::slice::from_raw_parts(s.kva as *const u32, (s.width * s.height) as usize) };
    Some(f(px, s.width, s.height))
}

pub fn info(id: u32) -> Option<(u32, String, bool, (i32, i32))> {
    CLIENTS.lock().get(&id).map(|c| (c.pid, c.title.clone(), c.resizable, c.initial))
}

/// (window id, owning pid, title) of every client window.
pub fn windows() -> Vec<(u32, u32, String)> {
    CLIENTS.lock().iter().map(|(&id, c)| (id, c.pid, c.title.clone())).collect()
}

pub fn title(id: u32) -> String {
    CLIENTS.lock().get(&id).map(|c| c.title.clone()).unwrap_or_default()
}

/// Queues an input/window event for a client window and wakes its process.
pub fn push_event(id: u32, mut ev: Event) {
    ev.window = id;
    let pid = {
        let mut clients = CLIENTS.lock();
        let Some(c) = clients.get_mut(&id) else { return };
        // Coalesce pointer motion.
        if ev.kind == aurora_abi::event::POINTER_MOVE {
            if let Some(last) = c.events.back_mut() {
                if last.kind == ev.kind {
                    *last = ev;
                    return wake_pid(c.pid);
                }
            }
        }
        if c.events.len() >= MAX_EVENTS {
            c.events.pop_front();
        }
        c.events.push_back(ev);
        c.pid
    };
    wake_pid(pid);
}

/// Sends an event to every client window (e.g. theme changes).
pub fn broadcast(ev: Event) {
    let ids: Vec<u32> = CLIENTS.lock().keys().copied().collect();
    for id in ids {
        push_event(id, ev);
    }
}

fn wake_pid(pid: u32) {
    if let Some(&t) = WAITERS.lock().get(&pid) {
        sched::wake(t);
    }
}

/// Called by the reaper when a process exits: closes all of its windows.
pub fn process_exited(pid: u32) {
    let gone: Vec<(u32, Client)> = {
        let mut clients = CLIENTS.lock();
        let ids: Vec<u32> = clients.iter().filter(|(_, c)| c.pid == pid).map(|(&id, _)| id).collect();
        ids.into_iter().filter_map(|id| clients.remove(&id).map(|c| (id, c))).collect()
    };
    for (id, c) in gone {
        free_surface(c.surface, pid, false);
        send(Command::Closed(id));
        crate::telemetry::window("close", id, pid, &c.title);
    }
    WAITERS.lock().remove(&pid);
}

// ------------------------------------------------------------- syscalls

fn title_of(id: u32) -> String {
    CLIENTS.lock().get(&id).map(|c| c.title.clone()).unwrap_or_default()
}

fn owned(id: u32, pid: u32) -> Result<(), isize> {
    match CLIENTS.lock().get(&id) {
        Some(c) if c.pid == pid => Ok(()),
        _ => Err(EBADF),
    }
}

fn user_str(ptr: u64, len: u64) -> Result<String, isize> {
    if len > 256 {
        return Err(ENAMETOOLONG);
    }
    let p = proc::current().ok_or(EPERM)?;
    let ok = p.aspace.lock().as_ref().is_some_and(|a| a.check(ptr, len, false));
    if !ok {
        return Err(EFAULT);
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
    Ok(String::from_utf8_lossy(bytes).into_owned())
}

fn put<T: Copy>(ptr: u64, v: &T) -> Result<(), isize> {
    let size = core::mem::size_of::<T>() as u64;
    let p = proc::current().ok_or(EPERM)?;
    let ok = p.aspace.lock().as_ref().is_some_and(|a| a.check(ptr, size, true));
    if !ok {
        return Err(EFAULT);
    }
    unsafe { (ptr as *mut T).write_unaligned(*v) };
    Ok(())
}

fn clamp_size(w: u64, h: u64) -> (u32, u32) {
    let (sw, sh) = super::screen_size();
    ((w as i32).clamp(120, sw) as u32, (h as i32).clamp(80, sh) as u32)
}

pub fn syscall(n: usize, a: [u64; 6]) -> Result<u64, isize> {
    let pid = sched::current_pid();
    if pid == 0 {
        return Err(EPERM);
    }
    match n {
        nr::WIN_CREATE => {
            let (w, h) = clamp_size(a[0], a[1]);
            let title = user_str(a[2], a[3])?;
            if CLIENTS.lock().values().filter(|c| c.pid == pid).count() >= 8 {
                return Err(EMFILE);
            }
            let surface = alloc_surface(pid, w, h)?;
            let id = NEXT_WINDOW.fetch_add(1, Ordering::Relaxed);
            CLIENTS.lock().insert(
                id,
                Client {
                    pid,
                    title,
                    resizable: a[4] as usize & aurora_abi::win::RESIZABLE != 0,
                    initial: (w as i32, h as i32),
                    surface,
                    events: VecDeque::new(),
                },
            );
            send(Command::Created(id));
            crate::telemetry::window("open", id, pid, &title_of(id));
            Ok(id as u64)
        }
        nr::WIN_SURFACE => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let info = {
                let clients = CLIENTS.lock();
                let s = &clients.get(&id).ok_or(EBADF)?.surface;
                SurfaceInfo { addr: s.user_va, width: s.width, height: s.height, stride: s.width, _pad: 0 }
            };
            put(a[1], &info).map(|_| 0)
        }
        nr::WIN_RESIZE => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let (w, h) = clamp_size(a[1], a[2]);
            let new = alloc_surface(pid, w, h)?;
            let old = {
                let mut clients = CLIENTS.lock();
                let c = clients.get_mut(&id).ok_or(EBADF)?;
                core::mem::replace(&mut c.surface, new)
            };
            free_surface(old, pid, true);
            send(Command::Present(id, Rect::new(0, 0, w as i32, h as i32)));
            Ok(0)
        }
        nr::WIN_PRESENT => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let r = Rect::new(a[1] as i32, a[2] as i32, a[3] as i32, a[4] as i32);
            send(Command::Present(id, r));
            Ok(0)
        }
        nr::WIN_SET_TITLE => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let title = user_str(a[1], a[2])?;
            if let Some(c) = CLIENTS.lock().get_mut(&id) {
                c.title = title;
            }
            send(Command::Title(id));
            Ok(0)
        }
        nr::WIN_CLOSE => {
            let id = a[0] as u32;
            owned(id, pid)?;
            if let Some(c) = CLIENTS.lock().remove(&id) {
                free_surface(c.surface, pid, true);
            }
            send(Command::Closed(id));
            crate::telemetry::window("close", id, pid, "");
            Ok(0)
        }
        nr::NEXT_EVENT => {
            let has_event = || CLIENTS.lock().values().any(|c| c.pid == pid && !c.events.is_empty());
            if !has_event() && a[1] > 0 {
                WAITERS.lock().insert(pid, sched::current_id());
                sched::wait_until(a[1], || has_event() || proc::interrupted());
                WAITERS.lock().remove(&pid);
            }
            let ev = {
                let mut clients = CLIENTS.lock();
                clients.values_mut().filter(|c| c.pid == pid).find_map(|c| c.events.pop_front())
            };
            match ev {
                Some(ev) => put(a[0], &ev).map(|_| 1),
                None => Ok(0),
            }
        }
        _ => Err(ENOSYS),
    }
}

/// The `desktop` system call: requests to the shell.
pub fn desktop_request(req: usize, ptr: u64, len: u64) -> Result<u64, isize> {
    use aurora_abi::desktop::*;
    match req {
        OPEN_APP | OPEN_FILE => {
            let raw = user_str(ptr, len)?;
            let cwd = proc::current().map(|p| p.cwd.lock().clone()).unwrap_or_else(|| String::from("/"));
            let path = crate::fs::resolve(&cwd, &raw);
            crate::fs::stat(&path)?;
            send(if req == OPEN_APP { Command::OpenApp(path) } else { Command::OpenFile(path) });
            Ok(0)
        }
        SET_DARK => {
            send(Command::SetDark(len != 0));
            Ok(0)
        }
        SET_WALLPAPER => {
            send(Command::SetWallpaper(len as u8));
            Ok(0)
        }
        SHUTDOWN => {
            send(Command::Shutdown);
            Ok(0)
        }
        REBOOT => {
            send(Command::Reboot);
            Ok(0)
        }
        GET_THEME => Ok(((super::theme::current().dark as u64) << 8) | super::theme::wallpaper() as u64),
        _ => Err(EINVAL),
    }
}
