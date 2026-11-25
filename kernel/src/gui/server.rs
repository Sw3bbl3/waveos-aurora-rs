//! Lumen Server's client interface: windows owned by user processes.
//!
//! A client window has a *surface* — pixel memory mapped both into the kernel
//! (so the compositor can read it) and into the owning process (so the app can
//! draw into it) — plus a queue of input events. System calls (running on the
//! client's task) and the compositor task share this state under `CLIENTS`;
//! structural changes are forwarded to the compositor through `COMMANDS`.

use super::geom::Rect;
use crate::drivers::gpu::{fourcc, BufferObject};
use crate::mm::vmm::{self, Prot};
use crate::mm::{frame, phys_to_virt, PAGE_SIZE};
use crate::sync::IrqMutex;
use crate::{proc, sched};
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::{err::*, nr, Event, SurfaceInfo};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const MAX_EVENTS: usize = 256;

/// A window's pixels, shared with its process. `width` and `height` are in
/// device pixels: `scale` times the window's size in points.
struct Surface {
    frames: Vec<u64>,
    kva: u64,
    user_va: u64,
    width: u32,
    height: u32,
    scale: u32,
}

impl Surface {
    fn pages(&self) -> u64 {
        self.frames.len() as u64
    }

    /// The size in points.
    fn points(&self) -> (i32, i32) {
        ((self.width / self.scale) as i32, (self.height / self.scale) as i32)
    }
}

pub struct Client {
    materials: Vec<Rect>,
    pub pid: u32,
    pub title: String,
    pub resizable: bool,
    pub initial: (i32, i32),
    minimum: (i32, i32),
    surface: Surface,
    events: VecDeque<Event>,
    /// The pointer's shape over the window (`aurora_abi::cursor`).
    cursor: u32,
    /// The process whose drawing the window shows, for a bridge's window
    /// (`win_set_program`): it gets the window's input focus too.
    program: Option<u32>,
    /// The scale of the display the window is on (its next surface's).
    scale: u32,
    /// The id of the display the window is on (0 until it appears).
    display: u32,
    /// Shown instead of the surface (`win_attach`): a buffer of the
    /// program's and the scale it was drawn at.
    attached: Option<Attached>,
    /// Counts `win_attach` calls (a display scanning a buffer out shows
    /// it again when the program attached it again with new pixels).
    attaches: u64,
    /// Presents and attaches since the window opened.
    pictures: u64,
    /// The window asked to be full screen (`win_set_fullscreen`).
    pub fullscreen: bool,
}

/// Pixels a program's windows show instead of their own surfaces
/// (`win_buffer`): a dma-buf, or pages of shared memory, read where they
/// are — never copied into the window — by the compositor, or scanned out
/// by a display. Held until the program destroys it and nothing shows it.
pub struct WinBuffer {
    pub id: u32,
    pub pid: u32,
    pub bo: Arc<BufferObject>,
    /// Byte offset of the first pixel in `bo`.
    pub offset: u32,
    pub width: u32,
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: u32,
    /// DRM fourcc: `XRGB8888`, or `ARGB8888` (premultiplied, as Wayland's).
    pub format: u32,
    /// A dma-buf (a DRM buffer), not shared memory.
    pub dmabuf: bool,
}

impl WinBuffer {
    /// The pixels, `stride / 4` words a row (the last row may stop at `width`).
    pub fn pixels(&self) -> &[u32] {
        let words = (self.stride / 4) as usize * (self.height as usize - 1) + self.width as usize;
        unsafe { core::slice::from_raw_parts((self.bo.vaddr() + self.offset as u64) as *const u32, words) }
    }

    /// No alpha: what the display shows is exactly the pixels.
    pub fn opaque(&self) -> bool {
        self.format == fourcc::XRGB8888
    }
}

/// Buffers a window shows (`win_attach`), bottom first, and the scale they
/// were drawn at.
#[derive(Clone)]
pub struct Attached {
    pub layers: Vec<Layer>,
    pub scale: u32,
}

/// The part `src` (its pixels) of `buffer` at `dst` (window pixels).
#[derive(Clone)]
pub struct Layer {
    pub buffer: Arc<WinBuffer>,
    pub src: Rect,
    pub dst: Rect,
}

impl Attached {
    fn holds(&self, id: u32) -> bool {
        self.layers.iter().any(|l| l.buffer.id == id)
    }

    /// One buffer shown whole and unscaled at the window's corner.
    fn single(&self) -> Option<&Arc<WinBuffer>> {
        let [l] = &self.layers[..] else { return None };
        let full = Rect::new(0, 0, l.buffer.width as i32, l.buffer.height as i32);
        (l.src == full && l.dst == full).then_some(&l.buffer)
    }

    /// The buffer that is all a `w`×`h`-pixel window shows: alone, or the
    /// topmost layer, opaque, whole and unscaled over all of it (what lies
    /// under it never shows), for a display to scan out.
    fn covering(&self, w: u32, h: u32) -> Option<&Arc<WinBuffer>> {
        if let Some(b) = self.single() {
            return Some(b);
        }
        let l = self.layers.last()?;
        let full = Rect::new(0, 0, l.buffer.width as i32, l.buffer.height as i32);
        let window = Rect::new(0, 0, w as i32, h as i32);
        (l.buffer.opaque() && l.src == full && l.dst == full && full == window).then_some(&l.buffer)
    }
}

/// What a window shows, for the compositor: its surface, or a buffer the
/// program attached.
pub struct Content<'a> {
    pub px: &'a [u32],
    /// Device pixels.
    pub width: u32,
    pub height: u32,
    /// Pixels from one row to the next.
    pub stride: u32,
    /// The scale it was drawn at.
    pub scale: u32,
    /// A program's buffer (`Some(opaque)`), or the window's own surface.
    pub buffer: Option<bool>,
    /// Several of the program's buffers stacked (empty: `px` is all).
    pub layers: &'a [Layer],
}

/// Changes the compositor must apply on its next frame.
pub enum Command {
    Created(u32),
    Closed(u32),
    Present(u32, Rect),
    Title(u32),
    /// A window asked to be full screen, or not (`server::fullscreen`).
    Fullscreen(u32),
    /// A window's pointer shape changed.
    Cursor,
    /// A program, its arguments, and (asked by a sandboxed app) the files
    /// among them the program is given (None: every one).
    OpenApp(String, Vec<String>, Option<Vec<(String, bool)>>),
    OpenFile(String),
    SetDark(bool),
    SetWallpaper(u8),
    SetWallpaperImage(String),
    StartTour,
    Shutdown,
    Reboot,
    /// An app called `drag_start` (the payload waits in `dnd`).
    DragStart,
    /// Items went into or out of the Trash (the dock icon may change).
    TrashChanged,
    AppsChanged,
    /// A preference changed (see `prefs`).
    Pref(String),
    /// Switch the screen resolution (live-capable displays only).
    SetResolution(u32, u32),
    /// The screen's modes changed (a ported GPU driver's hotplug event).
    DisplayChanged,
    /// The hardware power button (ACPI).
    PowerButton,
    /// Battery / power adapter / lid state changed (ACPI).
    PowerChanged,
    /// Back from sleep.
    Resumed,
    /// A network interface got (or lost) an address.
    NetworkChanged,
    /// A process asks for an administrator's password (see `users::elevate`).
    Elevate,
    /// An app asks to use something guarded (see `sandbox::consent`).
    Consent,
    /// An app asks for the open or save panel (see `sandbox::panel`).
    Panel,
    /// Something started or stopped recording (the menu bar's dot).
    Indicators,
    /// Accounts were added, removed or changed (`users::db`).
    AccountsChanged,
    /// Enter (true) or leave Wave Play.
    Play(bool),
    /// The Desktop folder's items changed, or a shortcut was resolved.
    DesktopItems,
}

/// Queues a command for the compositor.
pub fn command(cmd: Command) {
    send(cmd);
}

/// Starts a drag from one of `pid`'s windows (see [`super::dnd`]).
pub fn drag_start(pid: u32, kind: usize, data: &[u8], count: u32, icon: usize) -> Result<(), isize> {
    let window = CLIENTS.lock().iter().find(|(_, c)| c.pid == pid).map(|(&id, _)| id).ok_or(EBADF)?;
    super::dnd::start(pid, window, kind, data, count, icon)?;
    send(Command::DragStart);
    Ok(())
}

/// The process owning client window `id`.
pub fn pid_of(id: u32) -> Option<u32> {
    CLIENTS.lock().get(&id).map(|c| c.pid)
}

/// The process whose window is in front (0: none, or a built-in one).
static FRONT: AtomicU32 = AtomicU32::new(0);

pub fn frontmost() -> u32 {
    FRONT.load(Ordering::Relaxed)
}

/// The desktop focused client window `id` (None: no client window).
pub fn set_frontmost(id: Option<u32>) {
    FRONT.store(id.and_then(pid_of).unwrap_or(0), Ordering::Relaxed);
}

/// Self-tests: process `pid` is in front.
#[cfg_attr(not(feature = "ktest"), allow(dead_code))]
pub fn set_frontmost_pid(pid: u32) {
    FRONT.store(pid, Ordering::Relaxed);
}

/// The process whose drawing client window `id` shows, if a bridge said so.
pub fn program_of(id: u32) -> Option<u32> {
    CLIENTS.lock().get(&id).and_then(|c| c.program)
}

static CLIENTS: IrqMutex<BTreeMap<u32, Client>> = IrqMutex::new(BTreeMap::new());
/// Every process's window buffers, by id.
static BUFFERS: IrqMutex<BTreeMap<u32, Arc<WinBuffer>>> = IrqMutex::new(BTreeMap::new());
static NEXT_BUFFER: AtomicU32 = AtomicU32::new(1);
/// Buffers a display scans out directly: (display, window, buffer).
static ON_PLANES: IrqMutex<Vec<(u32, u32, Arc<WinBuffer>)>> = IrqMutex::new(Vec::new());
/// A process may hold this many window buffers.
const MAX_BUFFERS: usize = 64;
static COMMANDS: IrqMutex<VecDeque<Command>> = IrqMutex::new(VecDeque::new());
/// Task blocked in `next_event`, per process.
static WAITERS: IrqMutex<BTreeMap<u32, u64>> = IrqMutex::new(BTreeMap::new());
static NEXT_WINDOW: AtomicU32 = AtomicU32::new(1);
static COMPOSITOR: AtomicU64 = AtomicU64::new(u64::MAX);
/// The scale new windows get: the display with the pointer's (0: the main display's).
static NEW_SCALE: AtomicU32 = AtomicU32::new(0);

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

/// True once the window server runs (system prompts can be shown).
pub fn running() -> bool {
    COMPOSITOR.load(Ordering::Relaxed) != u64::MAX
}

/// Wakes the compositor (e.g. a notification arrived).
pub fn poke() {
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

/// A surface for a `width`×`height`-point window at `scale`.
fn alloc_surface(pid: u32, width: u32, height: u32, scale: u32) -> Result<Surface, isize> {
    let scale = scale.clamp(1, 2);
    let (width, height) = (width * scale, height * scale);
    let bytes = width as u64 * height as u64 * 4;
    let pages = bytes.div_ceil(PAGE_SIZE);
    let mut frames = Vec::with_capacity(pages as usize);
    for _ in 0..pages {
        match frame::alloc_user() {
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
    let mapped = (|| {
        let p = proc::get(pid).ok_or(ESRCH)?;
        let mut guard = p.mem();
        let aspace = guard.as_mut().ok_or(ESRCH)?;
        let user_va = aspace.reserve(pages).ok_or(ENOMEM)?;
        if let Err(e) = aspace.map_shared(user_va, &frames, Prot::ReadWrite) {
            aspace.unmap(user_va, pages);
            return Err(e);
        }
        Ok(user_va)
    })();
    match mapped {
        Ok(user_va) => Ok(Surface { frames, kva, user_va, width, height, scale }),
        Err(e) => {
            // (Each `?` here used to leak the frames and the kernel mapping.)
            vmm::kunmap(kva, pages);
            frames.iter().for_each(|&f| frame::free(f));
            Err(e)
        }
    }
}

/// Releases a surface. `unmap_user` is false when the process's address space
/// is being torn down anyway.
fn free_surface(s: Surface, pid: u32, unmap_user: bool) {
    if unmap_user {
        if let Some(p) = proc::get(pid) {
            if let Some(a) = p.mem().as_mut() {
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

/// Runs `f` with what window `id` shows: the buffer the program attached,
/// or its own surface. Holds the client lock, so neither can go (nor the
/// program attach another) while the compositor reads it.
pub fn with_content<R>(id: u32, f: impl FnOnce(&Content) -> R) -> Option<R> {
    let clients = CLIENTS.lock();
    let c = clients.get(&id)?;
    let content = match &c.attached {
        Some(a) => {
            let b = &a.layers[0].buffer;
            Content {
                px: b.pixels(),
                width: b.width,
                height: b.height,
                stride: b.stride / 4,
                scale: a.scale,
                buffer: Some(b.opaque()),
                layers: if a.single().is_some() { &[] } else { &a.layers[..] },
            }
        }
        None => {
            let s = &c.surface;
            Content {
                px: unsafe { core::slice::from_raw_parts(s.kva as *const u32, (s.width * s.height) as usize) },
                width: s.width,
                height: s.height,
                stride: s.width,
                scale: s.scale,
                buffer: None,
                layers: &[],
            }
        }
    };
    Some(f(&content))
}

/// The buffer window `id` shows, when the program attached one, with its
/// scale and how many attaches there have been (a display may scan it out
/// instead of compositing it).
pub fn attached(id: u32) -> Option<(Arc<WinBuffer>, u32, u64)> {
    let clients = CLIENTS.lock();
    let c = clients.get(&id)?;
    let a = c.attached.as_ref()?;
    // The window's size in its pixels at the attach's scale.
    let (pw, ph) = c.surface.points();
    a.covering(pw as u32 * a.scale, ph as u32 * a.scale).map(|b| (b.clone(), a.scale, c.attaches))
}

/// The buffers window `id` shows as layers (self-tests).
#[cfg(feature = "ktest")]
pub fn layers(id: u32) -> Vec<Arc<WinBuffer>> {
    CLIENTS.lock().get(&id).and_then(|c| c.attached.as_ref()).map_or(Vec::new(), |a| {
        a.layers.iter().map(|l| l.buffer.clone()).collect()
    })
}

/// Sets the pointer's shape over window `id` as its program would.
#[cfg(feature = "ktest")]
pub fn set_cursor(id: u32, shape: u32) {
    if let Some(c) = CLIENTS.lock().get_mut(&id) {
        c.cursor = shape;
    }
    send(Command::Cursor);
}

/// How many pictures window `id` presented (or attached) since it opened.
#[cfg(feature = "ktest")]
pub fn pictures(id: u32) -> u64 {
    CLIENTS.lock().get(&id).map_or(0, |c| c.pictures)
}

/// The display window `id` is on (0: none yet).
pub fn display_of(id: u32) -> u32 {
    CLIENTS.lock().get(&id).map_or(0, |c| c.display)
}

/// Display `display` scans out `on` (window, buffer) now, or nothing of a
/// window's (`None`): the buffer it scanned out before is released unless
/// its window still shows it. Called once the display has let go of it.
pub fn set_on_plane(display: u32, on: Option<(u32, Arc<WinBuffer>)>) {
    let before = {
        let mut planes = ON_PLANES.lock();
        let at = planes.iter().position(|p| p.0 == display);
        let before = at.map(|i| planes.remove(i));
        if let Some((w, b)) = on {
            planes.push((display, w, b));
        }
        before
    };
    if let Some((_, window, b)) = before {
        let busy = ON_PLANES.lock().iter().any(|p| p.2.id == b.id)
            || CLIENTS.lock().get(&window).and_then(|c| c.attached.as_ref()).is_some_and(|a| a.holds(b.id));
        if !busy {
            release(window, b.id);
        }
    }
}

/// Tells the program its buffer `buffer` is no longer read (`BUFFER_RELEASE`
/// on the window that showed it).
fn release(window: u32, buffer: u32) {
    push_event(window, Event { kind: aurora_abi::event::BUFFER_RELEASE, a: buffer, ..Default::default() });
}

/// Whether window `id` asked to be full screen.
pub fn fullscreen(id: u32) -> bool {
    CLIENTS.lock().get(&id).is_some_and(|c| c.fullscreen)
}

/// The window is shown at scale `k` (it moved to a display with that
/// scale): its next surface is drawn at it.
pub fn set_window_scale(id: u32, k: i32) {
    if let Some(c) = CLIENTS.lock().get_mut(&id) {
        c.scale = k.clamp(1, 2) as u32;
    }
}

/// New windows are drawn at scale `k` (that of the display with the pointer).
pub fn set_new_window_scale(k: i32) {
    NEW_SCALE.store(k.clamp(1, 2) as u32, Ordering::Relaxed);
}

fn new_window_scale() -> u32 {
    match NEW_SCALE.load(Ordering::Relaxed) {
        0 => super::scale().max(1) as u32,
        k => k,
    }
}

/// Window `id` is on display `display` (for `desktop::WINDOW_DISPLAY`).
pub fn set_window_display(id: u32, display: u32) {
    if let Some(c) = CLIENTS.lock().get_mut(&id) {
        c.display = display;
    }
}

pub fn info(id: u32) -> Option<(u32, String, bool, (i32, i32))> {
    CLIENTS.lock().get(&id).map(|c| (c.pid, c.title.clone(), c.resizable, c.initial))
}

pub fn minimum(id: u32) -> (i32, i32) {
    CLIENTS.lock().get(&id).map_or((240, 160), |c| c.minimum)
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

/// The process whose window has the focus (0: none): the program in use.
static FOREGROUND: AtomicU32 = AtomicU32::new(0);

pub fn foreground() -> u32 {
    FOREGROUND.load(Ordering::Relaxed)
}

/// Makes `pid` the program in front (tests: no compositor runs).
#[cfg(feature = "ktest")]
pub fn set_foreground(pid: u32) {
    FOREGROUND.store(pid, Ordering::Relaxed);
}

/// Window `id` gained or lost the focus.
pub fn note_focus(id: u32, focused: bool) {
    let Some(pid) = pid_of(id) else { return };
    if focused {
        FOREGROUND.store(pid, Ordering::Relaxed);
    } else {
        let _ = FOREGROUND.compare_exchange(pid, 0, Ordering::Relaxed, Ordering::Relaxed);
    }
}

/// The processes with windows.
pub fn window_pids() -> Vec<u32> {
    let mut pids: Vec<u32> = CLIENTS.lock().values().map(|c| c.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    pids
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
    crate::proc::readiness::notify(event_key(pid));
}

/// The readiness key of process `pid`'s window events (Linux `event_fd`).
pub fn event_key(pid: u32) -> usize {
    1 << 60 | pid as usize
}

/// True if process `pid` has window events waiting.
pub fn has_events(pid: u32) -> bool {
    CLIENTS.lock().values().any(|c| c.pid == pid && !c.events.is_empty())
}

/// How many window events process `pid` has waiting.
pub fn events_waiting(pid: u32) -> usize {
    CLIENTS.lock().values().filter(|c| c.pid == pid).map(|c| c.events.len()).sum()
}

/// When a frame was shown (see `aurora_abi::frame`).
#[derive(Clone, Copy, Default, Debug)]
pub struct Shown {
    /// The display's frame counter, its refresh period and the frame's start (ns).
    pub seq: u64,
    pub refresh_ns: u64,
    pub time_ns: u64,
    /// `aurora_abi::frame` flags.
    pub flags: i32,
}

/// The compositor has shown window `id`'s last present: a `FRAME` event,
/// unless one is already waiting (its time is then brought up to date).
/// (The self-tests answer presents with it when no compositor runs.)
#[cfg(feature = "ktest")]
pub fn frame_done(id: u32) {
    frame_shown(id, Shown { time_ns: crate::time::now_ns(), refresh_ns: 16_666_667, ..Default::default() });
}

/// Like [`frame_done`], saying when the frame was shown.
pub fn frame_shown(id: u32, at: Shown) {
    let ev = Event {
        kind: aurora_abi::event::FRAME,
        x: at.flags,
        a: at.seq as u32,
        b: at.refresh_ns.min(u32::MAX as u64) as u32,
        c: at.time_ns as u32,
        d: (at.time_ns >> 32) as u32,
        ..Default::default()
    };
    {
        let mut clients = CLIENTS.lock();
        let Some(c) = clients.get_mut(&id) else { return };
        if let Some(e) = c.events.iter_mut().find(|e| e.kind == aurora_abi::event::FRAME) {
            *e = Event { window: id, ..ev };
            return;
        }
    }
    push_event(id, ev);
}

/// The pointer shape window `id` asked for.
pub fn cursor_of(id: u32) -> u32 {
    CLIENTS.lock().get(&id).map_or(aurora_abi::cursor::ARROW, |c| c.cursor)
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
    // Its buffers go once nothing shows them (a display scanning one out
    // keeps it until the compositor has moved on). Dropped after the lock:
    // freeing the last reference unmaps the buffer, which waits for every
    // other processor to drop its mapping, and one of them may be spinning
    // on this lock with its interrupts off.
    let buffers: Vec<Arc<WinBuffer>> = {
        let mut all = BUFFERS.lock();
        let ids: Vec<u32> = all.iter().filter(|(_, b)| b.pid == pid).map(|(&id, _)| id).collect();
        ids.iter().filter_map(|id| all.remove(id)).collect()
    };
    drop(buffers);
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
    let bytes = crate::syscall::user::slice(ptr, len)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn put<T: Copy>(ptr: u64, v: &T) -> Result<(), isize> {
    crate::syscall::user::put(ptr, v)
}

fn clamp_size(w: u64, h: u64) -> (u32, u32) {
    // No larger than the largest display.
    let (sw, sh) = super::screens::largest().unwrap_or_else(super::screen_size);
    ((w as i32).clamp(120, sw.max(120)) as u32, (h as i32).clamp(80, sh.max(80)) as u32)
}

pub fn syscall(n: usize, a: [u64; 6]) -> Result<u64, isize> {
    let pid = sched::current_pid();
    if pid == 0 {
        return Err(EPERM);
    }
    match n {
        nr::WIN_CREATE => {
            let minimum = if a[4] as usize & aurora_abi::win::MINIMUM_SIZE != 0 {
                let (w, h) = ((a[5] >> 32) as u32, a[5] as u32);
                if !(120..=8192).contains(&w) || !(80..=8192).contains(&h) {
                    return Err(EINVAL);
                }
                (w as i32, h as i32)
            } else {
                (240, 160)
            };
            let (w, h) = clamp_size(a[0].max(minimum.0 as u64), a[1].max(minimum.1 as u64));
            let title = user_str(a[2], a[3])?;
            if CLIENTS.lock().values().filter(|c| c.pid == pid).count() >= 8 {
                return Err(EMFILE);
            }
            let scale = new_window_scale();
            let surface = alloc_surface(pid, w, h, scale)?;
            let id = NEXT_WINDOW.fetch_add(1, Ordering::Relaxed);
            CLIENTS.lock().insert(
                id,
                Client {
                    materials: Vec::new(),
                    pid,
                    title,
                    resizable: a[4] as usize & aurora_abi::win::RESIZABLE != 0,
                    initial: (w as i32, h as i32),
                    minimum,
                    surface,
                    events: VecDeque::new(),
                    cursor: aurora_abi::cursor::ARROW,
                    program: None,
                    scale,
                    display: 0,
                    attached: None,
                    attaches: 0,
                    pictures: 0,
                    fullscreen: false,
                },
            );
            send(Command::Created(id));
            crate::telemetry::window("open", id, pid, &title_of(id));
            crate::sandbox::window_opened(pid);
            Ok(id as u64)
        }
        nr::WIN_MATERIALS => {
            let id = a[0] as u32;
            owned(id, pid)?;
            if a[2] > 8 {
                return Err(EINVAL);
            }
            let bytes = crate::syscall::user::slice(a[1], a[2] * 16)?;
            let mut regions = Vec::new();
            let mut clients = CLIENTS.lock();
            let c = clients.get_mut(&id).ok_or(EBADF)?;
            for data in bytes.as_chunks::<16>().0 {
                let n = |at: usize| i32::from_le_bytes(data[at..at + 4].try_into().unwrap());
                let r = Rect::new(n(0), n(4), n(8), n(12));
                let (pw, ph) = c.surface.points();
                if r.x < 0
                    || r.y < 0
                    || r.w <= 0
                    || r.h <= 0
                    || r.x as i64 + r.w as i64 > pw as i64
                    || r.y as i64 + r.h as i64 > ph as i64
                {
                    return Err(EINVAL);
                }
                if regions.iter().any(|p: &Rect| p.intersects(&r)) {
                    return Err(EINVAL);
                }
                regions.push(r);
            }
            if regions != c.materials {
                c.materials = regions;
                let (pw, ph) = c.surface.points();
                send(Command::Present(id, Rect::new(0, 0, pw, ph)));
            }
            Ok(0)
        }
        nr::WIN_SURFACE => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let info = {
                let clients = CLIENTS.lock();
                let s = &clients.get(&id).ok_or(EBADF)?.surface;
                SurfaceInfo { addr: s.user_va, width: s.width, height: s.height, stride: s.width, scale: s.scale }
            };
            put(a[1], &info).map(|_| 0)
        }
        nr::WIN_RESIZE => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let (w, h) = clamp_size(a[1], a[2]);
            let scale = CLIENTS.lock().get(&id).map_or_else(new_window_scale, |c| c.scale);
            let mut new = Some(alloc_surface(pid, w, h, scale)?);
            let old = CLIENTS.lock().get_mut(&id).map(|c| core::mem::replace(&mut c.surface, new.take().unwrap()));
            let Some(old) = old else {
                // Another thread closed the window meanwhile: the new
                // surface goes too (it used to leak, found by `fuzz`).
                free_surface(new.take().unwrap(), pid, true);
                return Err(EBADF);
            };
            free_surface(old, pid, true);
            send(Command::Present(id, Rect::new(0, 0, w as i32, h as i32)));
            Ok(0)
        }
        nr::WIN_PRESENT => {
            let id = a[0] as u32;
            owned(id, pid)?;
            // Device pixels in, points out (rounded outwards).
            let k = CLIENTS.lock().get_mut(&id).map_or(1, |c| {
                c.pictures += 1;
                c.surface.scale as i32
            });
            let (x0, y0) = (a[1] as i32 / k, a[2] as i32 / k);
            let (x1, y1) = ((a[1] as i32 + a[3] as i32 + k - 1) / k, (a[2] as i32 + a[4] as i32 + k - 1) / k);
            send(Command::Present(id, Rect::new(x0, y0, x1 - x0, y1 - y0)));
            Ok(0)
        }
        nr::WIN_BUFFER => {
            let (width, height) = (a[2] as u32, (a[2] >> 32) as u32);
            let (offset, stride, format) = (a[1], a[3], a[4] as u32);
            if !(1..=16384).contains(&width) || !(1..=16384).contains(&height) {
                return Err(EINVAL);
            }
            if format != fourcc::XRGB8888 && format != fourcc::ARGB8888 {
                return Err(EINVAL);
            }
            if stride < width as u64 * 4 || !stride.is_multiple_of(4) || !offset.is_multiple_of(4) || stride > 1 << 20 {
                return Err(EINVAL);
            }
            let bytes = stride * (height as u64 - 1) + width as u64 * 4;
            if BUFFERS.lock().values().filter(|b| b.pid == pid).count() >= MAX_BUFFERS {
                return Err(EMFILE);
            }
            let p = proc::current().ok_or(EPERM)?;
            let (bo, at, dmabuf) = crate::compat::linux::drm::pixels_of(&p, a[0], offset, bytes)?;
            let id = NEXT_BUFFER.fetch_add(1, Ordering::Relaxed);
            let stride = stride as u32;
            let b = WinBuffer { id, pid, bo, offset: at as u32, width, height, stride, format, dmabuf };
            BUFFERS.lock().insert(id, Arc::new(b));
            Ok(id as u64)
        }
        nr::WIN_BUFFER_DESTROY => {
            let id = a[0] as u32;
            // (Released after the lock, as in `process_exited`.)
            let removed = {
                let mut buffers = BUFFERS.lock();
                match buffers.get(&id) {
                    Some(b) if b.pid == pid => buffers.remove(&id),
                    _ => return Err(EBADF),
                }
            };
            drop(removed);
            Ok(0)
        }
        nr::WIN_ATTACH => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let scale = ((a[1] >> 32) as u32).clamp(1, 4);
            let buffer = |b: u32| match BUFFERS.lock().get(&b) {
                Some(b) if b.pid == pid => Ok(b.clone()),
                _ => Err(EBADF),
            };
            let layered = a[1] & aurora_abi::attach::LAYERS != 0;
            let (new, damage) = if layered {
                let n = (a[1] as u32 & !(aurora_abi::attach::LAYERS as u32)) as usize;
                if n == 0 || n > aurora_abi::attach::MAX_LAYERS {
                    return Err(EINVAL);
                }
                let raw = crate::syscall::user::slice(a[2], (n * core::mem::size_of::<aurora_abi::WinLayer>()) as u64)?;
                let mut layers = Vec::with_capacity(n);
                for chunk in raw.as_chunks::<{ core::mem::size_of::<aurora_abi::WinLayer>() }>().0 {
                    let w = |at: usize| i32::from_le_bytes(chunk[at..at + 4].try_into().unwrap());
                    let b = buffer(w(0) as u32)?;
                    let src = Rect::new(w(8), w(12), w(16), w(20));
                    let dst = Rect::new(w(24), w(28), w(32), w(36));
                    let whole = Rect::new(0, 0, b.width as i32, b.height as i32);
                    if src.is_empty() || dst.is_empty() || src.intersect(&whole) != src {
                        return Err(EINVAL);
                    }
                    layers.push(Layer { buffer: b, src, dst });
                }
                let d = Rect::new(a[3] as i32, (a[3] >> 32) as i32, a[4] as i32, (a[4] >> 32) as i32);
                (Some(Attached { layers, scale }), d)
            } else {
                let d = Rect::new(a[2] as i32, (a[2] >> 32) as i32, a[3] as i32, (a[3] >> 32) as i32);
                match a[1] as u32 {
                    0 => (None, d),
                    b => {
                        let b = buffer(b)?;
                        let full = Rect::new(0, 0, b.width as i32, b.height as i32);
                        (Some(Attached { layers: alloc::vec![Layer { buffer: b, src: full, dst: full }], scale }), d)
                    }
                }
            };
            let (gone, k, old) = {
                let mut clients = CLIENTS.lock();
                let c = clients.get_mut(&id).ok_or(EBADF)?;
                let k = if new.is_some() { scale } else { c.surface.scale };
                let old = core::mem::replace(&mut c.attached, new);
                c.attaches += 1;
                c.pictures += 1;
                let now = c.attached.as_ref();
                let mut gone: Vec<u32> = old
                    .as_ref()
                    .map(|o| o.layers.iter().map(|l| l.buffer.id).filter(|&b| !now.is_some_and(|n| n.holds(b))).collect())
                    .unwrap_or_default();
                gone.dedup();
                // The old buffers are let go after the lock (see `process_exited`).
                (gone, k as i32, old)
            };
            drop(old);
            // Those it no longer shows are read no more (the compositor
            // reads under the lock), unless a display scans one out.
            for b in gone {
                if !ON_PLANES.lock().iter().any(|p| p.2.id == b) {
                    release(id, b);
                }
            }
            let (x, y, w, h) = (damage.x.max(0), damage.y.max(0), damage.w, damage.h);
            let (x0, y0) = (x / k, y / k);
            let (x1, y1) = ((x.saturating_add(w.max(0)) + k - 1) / k, (y.saturating_add(h.max(0)) + k - 1) / k);
            send(Command::Present(id, Rect::new(x0, y0, x1 - x0, y1 - y0)));
            Ok(0)
        }
        nr::WIN_SET_FULLSCREEN => {
            let id = a[0] as u32;
            owned(id, pid)?;
            if let Some(c) = CLIENTS.lock().get_mut(&id) {
                c.fullscreen = a[1] != 0;
            }
            send(Command::Fullscreen(id));
            Ok(0)
        }
        nr::WIN_SET_CURSOR => {
            let id = a[0] as u32;
            owned(id, pid)?;
            if a[1] > aurora_abi::cursor::MAX as u64 {
                return Err(EINVAL);
            }
            if let Some(c) = CLIENTS.lock().get_mut(&id) {
                c.cursor = a[1] as u32;
            }
            send(Command::Cursor);
            Ok(0)
        }
        nr::WIN_SET_PROGRAM => {
            let id = a[0] as u32;
            owned(id, pid)?;
            let program = match a[1] as u32 {
                0 => None,
                other => {
                    // Only a program of the caller's own account.
                    let target = crate::proc::get(other).ok_or(ESRCH)?;
                    let mine = *crate::proc::current().ok_or(EPERM)?.cred.lock();
                    if target.cred.lock().uid != mine.uid && !mine.is_root() {
                        return Err(EPERM);
                    }
                    // The window's focus gives its program the keyboard,
                    // pointer and controllers: a sandboxed bridge may hand
                    // it only to its own app's processes.
                    if !crate::sandbox::may_designate(&target) {
                        return Err(EPERM);
                    }
                    Some(other)
                }
            };
            if let Some(c) = CLIENTS.lock().get_mut(&id) {
                c.program = program;
            }
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
            // Unmapping needs other CPUs to answer an IPI: never under the clients lock.
            let removed = CLIENTS.lock().remove(&id);
            if let Some(c) = removed {
                free_surface(c.surface, pid, true);
            }
            send(Command::Closed(id));
            crate::telemetry::window("close", id, pid, "");
            crate::sandbox::window_closed(pid);
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
    use aurora_abi::sandbox as cap;
    // What a sandboxed app needs for each request.
    let need = match req {
        OPEN_APP | OPEN_FILE => cap::LAUNCH,
        SET_DARK | SET_WALLPAPER | SET_WALLPAPER_IMAGE | START_TOUR | PLAY => cap::PREFS_WRITE,
        SHUTDOWN | REBOOT | EJECT | EMPTY_TRASH | PUT_BACK => cap::SYSTEM,
        _ => 0,
    };
    if need != 0 {
        crate::sandbox::require(need)?;
    }
    match req {
        START_TOUR => {
            if ptr != 0 || len != 0 {
                return Err(EINVAL);
            }
            send(Command::StartTour);
            Ok(0)
        }
        PLAY => {
            if ptr != 0 || len > 1 {
                return Err(EINVAL);
            }
            send(Command::Play(len == 1));
            Ok(0)
        }
        DISPLAYS => {
            let list = super::screens::published();
            let size = core::mem::size_of::<aurora_abi::DisplayInfo>();
            let max = (len as usize).min(16).min(list.len());
            if max > 0 {
                let mut out = crate::syscall::user::slice_mut(ptr, (max * size) as u64)?;
                for (i, d) in list.iter().take(max).enumerate() {
                    let bytes = unsafe { core::slice::from_raw_parts(d as *const _ as *const u8, size) };
                    out[i * size..(i + 1) * size].copy_from_slice(bytes);
                }
                out.commit(max * size)?;
            }
            Ok(list.len() as u64)
        }
        WINDOW_DISPLAY => {
            let pid = sched::current_pid();
            let clients = CLIENTS.lock();
            match clients.get(&(len as u32)) {
                Some(c) if c.pid == pid && c.display != 0 => Ok(c.display as u64),
                Some(c) if c.pid == pid => Err(EAGAIN),
                _ => Err(EBADF),
            }
        }
        DISPLAY_MODES => {
            let modes = super::screens::modes_of((len >> 32) as u32).ok_or(ENODEV)?;
            let size = core::mem::size_of::<aurora_abi::DisplayMode>();
            let max = ((len & 0xFFFF_FFFF) as usize).min(64).min(modes.len());
            if max > 0 {
                let mut out = crate::syscall::user::slice_mut(ptr, (max * size) as u64)?;
                for (i, m) in modes.iter().take(max).enumerate() {
                    let bytes = unsafe { core::slice::from_raw_parts(m as *const _ as *const u8, size) };
                    out[i * size..(i + 1) * size].copy_from_slice(bytes);
                }
                out.commit(max * size)?;
            }
            Ok(modes.len() as u64)
        }
        OPEN_APP | OPEN_FILE => {
            let raw = user_str(ptr, len)?;
            let cwd = proc::current().map(|p| p.cwd.lock().clone()).unwrap_or_else(|| String::from("/"));
            // `open_app` may name arguments after the program, NUL-separated.
            let mut parts = raw.split('\0');
            let path = crate::fs::resolve(&cwd, parts.next().unwrap_or(""));
            let args: Vec<String> = parts.filter(|a| !a.is_empty()).take(16).map(String::from).collect();
            crate::fs::stat(&path)?;
            if req == OPEN_FILE {
                // Only what the app may read itself goes to another app.
                crate::sandbox::check_path(&path, crate::sandbox::Access::Read)?;
                send(Command::OpenFile(path));
                return Ok(0);
            }
            // Files among the arguments: those the app may use, as far as it may.
            let grants = crate::sandbox::current().map(|s| {
                let mut g = Vec::new();
                for a in args.iter().filter(|a| a.starts_with('/')) {
                    let p = crate::fs::resolve("/", a);
                    if s.check_path(&p, crate::sandbox::Access::Write).is_ok() {
                        g.push((p, true));
                    } else if s.check_path(&p, crate::sandbox::Access::Read).is_ok() {
                        g.push((p, false));
                    }
                }
                g
            });
            send(Command::OpenApp(path, args, grants));
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
        TRASH => {
            let raw = user_str(ptr, len)?;
            let cwd = proc::current().map(|p| p.cwd.lock().clone()).unwrap_or_else(|| String::from("/"));
            crate::fs::trash::move_to_trash(&crate::fs::resolve(&cwd, &raw))?;
            crate::drivers::audio::play_sound("trash");
            send(Command::TrashChanged);
            Ok(0)
        }
        PUT_BACK => {
            let name = user_str(ptr, len)?;
            crate::fs::trash::put_back(&name)?;
            send(Command::TrashChanged);
            Ok(0)
        }
        EJECT => {
            let path = crate::fs::resolve("/", &user_str(ptr, len)?);
            crate::drivers::block::removable::eject(&path)?;
            super::notify::system("Safe to remove", path.trim_start_matches("/Volumes/"));
            Ok(0)
        }
        PLAY_SOUND => {
            let name = user_str(ptr, len)?;
            if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
                return Err(EINVAL);
            }
            crate::drivers::audio::play_sound(&name);
            Ok(0)
        }
        EMPTY_TRASH => {
            crate::fs::trash::empty()?;
            crate::drivers::audio::play_sound("trash");
            send(Command::TrashChanged);
            Ok(0)
        }
        SET_WALLPAPER_IMAGE => {
            let raw = user_str(ptr, len)?;
            let cwd = proc::current().map(|p| p.cwd.lock().clone()).unwrap_or_else(|| String::from("/"));
            let path = crate::fs::resolve(&cwd, &raw);
            crate::fs::stat(&path)?;
            crate::sandbox::check_path(&path, crate::sandbox::Access::Read)?;
            send(Command::SetWallpaperImage(path));
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
        REFRESH_APPS => {
            send(Command::AppsChanged);
            Ok(0)
        }
        // Bits 0-7 wallpaper, 8 dark, 16-23 accent choice, 24-47 the accent
        // fill sampled from a custom picture (apps derive the other tones).
        GET_THEME => Ok((super::theme::custom_fill() as u64) << 24
            | (super::theme::accent_index() as u64) << 16
            | ((super::theme::current().dark as u64) << 8)
            | super::theme::wallpaper() as u64),
        _ => Err(EINVAL),
    }
}

pub fn material_regions(id: u32) -> Vec<Rect> {
    CLIENTS.lock().get(&id).map(|c| c.materials.clone()).unwrap_or_default()
}
