//! wl-bridge: a Wayland compositor for Linux programs on WaveOS.
//!
//! Linux programs that draw with GTK, Qt or their own Wayland code connect
//! to `$XDG_RUNTIME_DIR/wayland-0`. Each top-level window (`xdg_toplevel`)
//! becomes a WaveOS window: its surface, sub-surfaces and pop-up menus
//! are drawn into that window's pixels whenever the program commits a new
//! frame, and WaveOS's input events for the window go back to the program
//! as Wayland pointer and keyboard events. WaveOS draws the window frame
//! (programs are told decorations are the server's), and its clipboard is
//! the programs' selection.
//!
//! Buffers are shared memory (`wl_shm`) or dma-bufs (`zwp_linux_dmabuf_v1`,
//! and Mesa's `wl_drm`, through which Mesa's EGL renders on the DRM device).
//! A window that shows one surface takes its buffers as they are: WaveOS
//! reads them where they are (or scans them out), and they are released
//! when it says it no longer reads them. Windows with sub-surfaces or
//! pop-ups are composed by the bridge, from copies (see `buffers`). Frame
//! callbacks and presentation feedback follow WaveOS's frames: they are
//! answered when the window's picture is on screen (`FRAME` events).
//!
//! `WL_BRIDGE_COPY=1` in its environment makes it copy everything and offer
//! no dma-bufs, as before (for comparisons).
//!
//! `wl-bridge --background` returns once the bridge is listening (and at
//! once if one already is), leaving it running: launchers start it that way
//! before a program.

mod buffers;
mod damage;
use buffers::{Buffer, Pool};
use damage::Rect;
mod keys;
mod protocol;
mod sys;
mod wire;

use protocol::*;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use sys::Fd;
use wire::{Arg, Message, Out};

type ClientId = u32;
type Id = u32;

/// Ids of objects the bridge makes (data offers) start here.
const SERVER_IDS: u32 = 0xff00_0000;
/// Milliseconds between frames.
const FRAME_MS: u32 = 16;
/// A frame callback waits this long for its window's picture to be shown
/// (a hidden window's never is) before it is answered anyway.
const SHOWN_WAIT_MS: u32 = 100;
/// The DRM device programs render on (`wl_drm`, dma-buf feedback): card0.
const DRM_DEVICE: &str = "/dev/dri/card0";
const DRM_DEV_T: u64 = 226 << 8;

const BTN_LEFT: u32 = 0x110;
const BTN_RIGHT: u32 = 0x111;

/// The interfaces the bridge offers, by global name (index + 1).
const GLOBALS: &[(&str, u32)] = &[
    (wl_compositor::NAME, 5),
    (wl_subcompositor::NAME, 1),
    (wl_shm::NAME, 1),
    (wl_output::NAME, 3),
    (wl_seat::NAME, 7),
    (xdg_wm_base::NAME, 5),
    (zxdg_decoration_manager_v1::NAME, 1),
    (wl_data_device_manager::NAME, 3),
    (wp_viewporter::NAME, 1),
    (zwp_linux_dmabuf_v1::NAME, 4),
    (wl_drm::NAME, 2),
    (wp_presentation::NAME, 1),
];

/// Globals not offered with `WL_BRIDGE_COPY=1`.
fn dmabuf_global(name: &str) -> bool {
    name == zwp_linux_dmabuf_v1::NAME || name == wl_drm::NAME
}

// ------------------------------------------------------------------ objects

#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    None,
    Cursor,
    /// An `xdg_surface` (its id).
    Xdg(Id),
    /// A sub-surface of this surface.
    Sub(Id),
}

/// A `wl_region`: rectangles added and taken away, in order.
type Region = Vec<(bool, (i32, i32, i32, i32))>;

fn region_contains(r: &Region, x: i32, y: i32) -> bool {
    let mut inside = false;
    for &(add, (rx, ry, rw, rh)) in r {
        if x >= rx && y >= ry && x < rx.saturating_add(rw) && y < ry.saturating_add(rh) {
            inside = add;
        }
    }
    inside
}

#[derive(Default)]
struct Pending {
    /// `Some(None)`: the buffer was detached.
    buffer: Option<Option<Id>>,
    offset: (i32, i32),
    scale: Option<i32>,
    damage: Option<Rect>,
    buffer_damage: Option<Rect>,
    frames: Vec<Id>,
    /// `wp_presentation_feedback`s.
    feedbacks: Vec<Id>,
    /// `Some(None)`: input everywhere again.
    input: Option<Option<Region>>,
}

struct Surface {
    pending: Pending,
    attached: Option<Id>,
    geometry_changed: bool,
    pixels: Vec<u32>,
    /// Buffer size in pixels.
    bw: i32,
    bh: i32,
    opaque: bool,
    has_content: bool,
    scale: i32,
    /// `wp_viewport`: the source rectangle (surface coordinates) and the
    /// size it is shown at.
    src: Option<(f64, f64, f64, f64)>,
    dst: Option<(i32, i32)>,
    role: Role,
    /// Sub-surfaces, bottom to top.
    children: Vec<Id>,
    /// Position in the parent (sub-surfaces), applied on commit.
    pos: (i32, i32),
    pending_pos: Option<(i32, i32)>,
    /// Where the surface takes pointer input (`None`: everywhere).
    input: Option<Region>,
    /// The attached buffer was not copied (`pixels` are stale): its window
    /// shows it as it is, or copies it when it composes.
    direct: bool,
    /// What changed in it since the window last took it (buffer pixels).
    direct_damage: Option<Rect>,
    /// The buffer WaveOS shows of it now (held until WaveOS releases it).
    on_screen: Option<Id>,
    /// `pixels` hold its last committed picture (not just an older one).
    pixels_current: bool,
}

impl Surface {
    fn new() -> Surface {
        Surface {
            pending: Pending::default(),
            attached: None,
            geometry_changed: false,
            pixels: Vec::new(),
            bw: 0,
            bh: 0,
            opaque: false,
            has_content: false,
            scale: 1,
            src: None,
            dst: None,
            role: Role::None,
            children: Vec::new(),
            pos: (0, 0),
            pending_pos: None,
            input: None,
            direct: false,
            direct_damage: None,
            on_screen: None,
            pixels_current: false,
        }
    }

    /// Size in surface coordinates (points).
    fn size(&self) -> (i32, i32) {
        if let Some(d) = self.dst {
            return d;
        }
        if let Some((_, _, w, h)) = self.src {
            return (w as i32, h as i32);
        }
        (self.bw / self.scale.max(1), self.bh / self.scale.max(1))
    }

    /// An explicitly set identity viewport needs no damage expansion. Firefox
    /// uses viewports even at integer scales; treating all of them as scaled
    /// made a changing video or caret repaint the entire browser window.
    fn identity_viewport(&self) -> bool {
        let k = self.scale.max(1);
        self.bw % k == 0
            && self.bh % k == 0
            && self.dst.is_none_or(|d| d == (self.bw / k, self.bh / k))
            && self.src.is_none_or(|(x, y, w, h)| {
                x == 0.0 && y == 0.0 && w * k as f64 == self.bw as f64 && h * k as f64 == self.bh as f64
            })
    }
}

#[derive(Clone, Copy, PartialEq)]
enum XdgRole {
    None,
    Toplevel(Id),
    Popup(Id),
}

struct XdgSurface {
    surface: Id,
    role: XdgRole,
    geometry: Option<(i32, i32, i32, i32)>,
    /// The first configure was sent.
    configured: bool,
}

struct Toplevel {
    xdg: Id,
    title: String,
    window: Option<u32>,
    /// The size last asked for in a configure.
    asked: Option<(i32, i32)>,
    /// The size it last drew.
    drawn: (i32, i32),
    /// It asked to be full screen.
    fullscreen: bool,
}

struct Popup {
    xdg: Id,
    parent: Id,
    /// Position relative to the parent's window geometry.
    rect: (i32, i32, i32, i32),
    positioner: Positioner,
}

#[derive(Clone, Copy, Default)]
struct Positioner {
    size: (i32, i32),
    anchor_rect: (i32, i32, i32, i32),
    anchor: u32,
    gravity: u32,
    constraint: u32,
    offset: (i32, i32),
}

impl Positioner {
    /// Where the pop-up goes, relative to the parent's window geometry,
    /// kept inside `bounds` (the parent window, in the same coordinates).
    fn place(&self, bounds: (i32, i32, i32, i32)) -> (i32, i32, i32, i32) {
        let (ax, ay, aw, ah) = self.anchor_rect;
        let (w, h) = self.size;
        // Anchor point: 1 top, 2 bottom, 3 left, 4 right, 5 top-left,
        // 6 bottom-left, 7 top-right, 8 bottom-right.
        let horiz = |e: u32| match e {
            3 | 5 | 6 => -1,
            4 | 7 | 8 => 1,
            _ => 0,
        };
        let vert = |e: u32| match e {
            1 | 5 | 7 => -1,
            2 | 6 | 8 => 1,
            _ => 0,
        };
        let px = ax
            + match horiz(self.anchor) {
                -1 => 0,
                1 => aw,
                _ => aw / 2,
            };
        let py = ay
            + match vert(self.anchor) {
                -1 => 0,
                1 => ah,
                _ => ah / 2,
            };
        let mut x = px
            + self.offset.0
            + match horiz(self.gravity) {
                -1 => -w,
                1 => 0,
                _ => -w / 2,
            };
        let mut y = py
            + self.offset.1
            + match vert(self.gravity) {
                -1 => -h,
                1 => 0,
                _ => -h / 2,
            };
        let (bx, by, bw, bh) = bounds;
        // Flip, then slide, to stay in the window (the bridge can't draw
        // outside it).
        if y + h > by + bh && vert(self.gravity) == 1 && py - ah - h >= by {
            y = py - ah - h - self.offset.1;
        }
        if x + w > bx + bw && horiz(self.gravity) == 1 && px - aw - w >= bx {
            x = px - aw - w - self.offset.0;
        }
        x = x.min(bx + bw - w).max(bx);
        y = y.min(by + bh - h).max(by);
        (x, y, w, h)
    }
}

/// One connected program.
struct Client {
    fd: Fd,
    inbuf: Vec<u8>,
    infds: VecDeque<Fd>,
    out: Vec<u8>,
    outfds: Vec<Fd>,
    /// Descriptors to close once the queued events are sent.
    close_after: Vec<Fd>,
    objects: HashMap<Id, (&'static str, u32)>,
    surfaces: HashMap<Id, Surface>,
    buffers: HashMap<Id, Buffer>,
    pools: HashMap<Id, Rc<RefCell<Pool>>>,
    xdg: HashMap<Id, XdgSurface>,
    toplevels: HashMap<Id, Toplevel>,
    popups: Vec<(Id, Popup)>,
    positioners: HashMap<Id, Positioner>,
    /// Sub-surface object → its surface.
    subsurfaces: HashMap<Id, Id>,
    viewports: HashMap<Id, Id>,
    data_sources: HashMap<Id, Vec<String>>,
    regions: HashMap<Id, Region>,
    /// `zwp_linux_buffer_params_v1`: the plane added (fd, offset, stride, modifier).
    params: HashMap<Id, Option<(Fd, u32, u32, u64)>>,
    next_server_id: u32,
    dead: bool,
}

impl Client {
    fn new(fd: Fd) -> Client {
        let mut objects = HashMap::new();
        objects.insert(1, (wl_display::NAME, 1));
        Client {
            fd,
            inbuf: Vec::new(),
            infds: VecDeque::new(),
            out: Vec::new(),
            outfds: Vec::new(),
            close_after: Vec::new(),
            objects,
            surfaces: HashMap::new(),
            buffers: HashMap::new(),
            pools: HashMap::new(),
            xdg: HashMap::new(),
            toplevels: HashMap::new(),
            popups: Vec::new(),
            positioners: HashMap::new(),
            subsurfaces: HashMap::new(),
            viewports: HashMap::new(),
            data_sources: HashMap::new(),
            regions: HashMap::new(),
            params: HashMap::new(),
            next_server_id: SERVER_IDS,
            dead: false,
        }
    }

    fn send(&mut self, object: Id, opcode: u16, args: &[Out]) {
        if !self.dead {
            wire::encode(&mut self.out, &mut self.outfds, object, opcode, args);
        }
    }

    fn version(&self, id: Id) -> u32 {
        self.objects.get(&id).map_or(1, |o| o.1)
    }

    /// Objects of interface `iface`.
    fn all(&self, iface: &str) -> Vec<Id> {
        let mut v: Vec<Id> = self.objects.iter().filter(|(_, o)| o.0 == iface).map(|(id, _)| *id).collect();
        v.sort();
        v
    }

    /// Sends what is queued; false if the program has gone.
    fn flush(&mut self) -> bool {
        while !self.out.is_empty() {
            let fds: Vec<Fd> = self.outfds.drain(..self.outfds.len().min(28)).collect();
            match sys::send(self.fd, &self.out, &fds) {
                Ok(n) => {
                    self.out.drain(..n);
                }
                Err(-11) => {
                    // Full: try again when it can take more.
                    let mut rest = fds;
                    rest.append(&mut self.outfds);
                    self.outfds = rest;
                    return true;
                }
                Err(_) => return false,
            }
        }
        for fd in self.close_after.drain(..) {
            sys::close(fd);
        }
        true
    }

    /// The `xdg_surface` a surface belongs to (through sub-surfaces).
    fn xdg_of(&self, mut sid: Id) -> Option<Id> {
        for _ in 0..64 {
            match self.surfaces.get(&sid)?.role {
                Role::Xdg(x) => return Some(x),
                Role::Sub(parent) => sid = parent,
                _ => return None,
            }
        }
        None
    }

    /// The toplevel an `xdg_surface` belongs to (through pop-ups).
    fn toplevel_of(&self, mut xid: Id) -> Option<Id> {
        for _ in 0..64 {
            match self.xdg.get(&xid)?.role {
                XdgRole::Toplevel(t) => return Some(t),
                XdgRole::Popup(p) => xid = self.popup(p)?.parent,
                XdgRole::None => return None,
            }
        }
        None
    }

    fn popup(&self, id: Id) -> Option<&Popup> {
        self.popups.iter().find(|(p, _)| *p == id).map(|(_, p)| p)
    }

    fn geometry(&self, xid: Id) -> (i32, i32, i32, i32) {
        let Some(x) = self.xdg.get(&xid) else { return (0, 0, 0, 0) };
        x.geometry.unwrap_or_else(|| {
            let (w, h) = self.surfaces.get(&x.surface).map_or((0, 0), Surface::size);
            (0, 0, w, h)
        })
    }

    /// Where an `xdg_surface`'s surface sits, in its toplevel's surface
    /// coordinates.
    fn origin(&self, xid: Id) -> (i32, i32) {
        let mut at = (0, 0);
        let mut xid = xid;
        for _ in 0..64 {
            let Some(x) = self.xdg.get(&xid) else { break };
            let XdgRole::Popup(pid) = x.role else { break };
            let Some(p) = self.popup(pid) else { break };
            let own = self.geometry(xid);
            let parent = self.geometry(p.parent);
            at.0 += parent.0 + p.rect.0 - own.0;
            at.1 += parent.1 + p.rect.1 - own.1;
            xid = p.parent;
        }
        at
    }
}

/// A WaveOS window showing a toplevel.
struct Window {
    client: ClientId,
    toplevel: Id,
    /// Size in points.
    size: (i32, i32),
    surface: sys::Surface,
    focused: bool,
    dirty: bool,
    damage: Option<Rect>,
    /// The WaveOS buffers the window shows as they are (`win_attach`).
    attached: Vec<u32>,
    /// How many layers it showed last (1: a buffer alone).
    layers: usize,
    /// How it reaches the screen (logged when it changes).
    mode: String,
    /// A picture was presented and is not on screen yet.
    showing: bool,
    /// For the statistics line: frames shown as they are, and composed.
    direct_frames: u64,
    composed_frames: u64,
}

/// A frame callback or presentation feedback, answered when its window's
/// picture is on screen.
struct Frame {
    client: ClientId,
    id: Id,
    feedback: bool,
    /// The window it waits for; `None`: the next tick of the bridge's timer.
    window: Option<u32>,
    since: u32,
}

/// Where a client's frames go, measured (milliseconds and frames since the
/// last line, which goes to the system log every 10 seconds).
#[derive(Default)]
struct Stats {
    frames: u64,
    direct: u64,
    /// Client time: from a frame callback answered to the next commit
    /// (programs that draw on frame callbacks), and how many such frames.
    client_ns: u64,
    client: u64,
    /// The bridge's own time from a commit to the window's present.
    handoff_ns: u64,
    /// From the present until WaveOS said it was on screen.
    shown_ns: u64,
    shown: u64,
    since: u32,
}

struct Server {
    listener: Fd,
    clients: HashMap<ClientId, Client>,
    next_client: ClientId,
    windows: HashMap<u32, Window>,
    serial: u32,
    screen: (i32, i32),
    scale: i32,
    keymap: (Fd, usize),
    /// Surface under the pointer: (client, surface, window).
    pointer: Option<(ClientId, Id, u32)>,
    /// Pointer position over it, in surface coordinates.
    pointer_at: (f64, f64),
    button_down: bool,
    mods: u32,
    frames: Vec<Frame>,
    last_frame: u32,
    /// The program owning the selection, and its data source.
    selection: Option<(ClientId, Id)>,
    /// Clipboard text being read from a program (pipe, bytes so far).
    clip_in: Vec<(Fd, Vec<u8>)>,
    /// Clipboard text being written to a program.
    clip_out: Vec<(Fd, Vec<u8>)>,
    /// WaveOS window buffers by id: the client and `wl_buffer` they are.
    kernel_buffers: HashMap<u32, (ClientId, Id)>,
    /// `WL_BRIDGE_COPY=1`: no dma-bufs, every frame copied.
    copy_only: bool,
    stats: Stats,
    /// When the last frame callbacks were answered (client time starts) and
    /// presents were made (until shown), in monotonic ns.
    answered_at: u64,
    presented_at: HashMap<u32, u64>,
    /// When a window's first commit since its last present began (the
    /// hand-off is measured from there: copies happen at the commit).
    committed_at: HashMap<u32, u64>,
}

fn main() {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| String::from("/run"));
    let name = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| String::from("wayland-0"));
    let path = format!("{dir}/{name}");
    let background = std::env::args().any(|a| a == "--background");
    if sys::answers(&path) {
        if !background {
            eprintln!("wl-bridge: a compositor is already running on {path}");
        }
        std::process::exit(if background { 0 } else { 1 });
    }
    let listener = match sys::listen_unix(&path) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("wl-bridge: can't listen on {path} (error {})", -e);
            std::process::exit(1);
        }
    };
    let mut keymap = keys::KEYMAP.as_bytes().to_vec();
    keymap.push(0);
    let keymap = (sys::memfd_with(&keymap), keymap.len());
    if background {
        sys::detach();
    }

    // WaveOS events arrive on their own thread, which wakes the main loop.
    let (wake_r, wake_w) = sys::pipe().expect("pipe");
    let (tx, rx) = std::sync::mpsc::channel::<sys::Event>();
    std::thread::spawn(move || loop {
        if let Some(e) = sys::next_event(1000) {
            if tx.send(e).is_err() {
                break;
            }
            sys::write(wake_w, b"e");
        }
    });

    let mut s = Server {
        listener,
        clients: HashMap::new(),
        next_client: 1,
        windows: HashMap::new(),
        serial: 1,
        screen: sys::screen_pixels(),
        scale: sys::scale(),
        keymap,
        pointer: None,
        pointer_at: (0.0, 0.0),
        button_down: false,
        mods: 0,
        frames: Vec::new(),
        last_frame: 0,
        selection: None,
        clip_in: Vec::new(),
        clip_out: Vec::new(),
        kernel_buffers: HashMap::new(),
        copy_only: std::env::var("WL_BRIDGE_COPY").is_ok_and(|v| v == "1"),
        stats: Stats::default(),
        answered_at: 0,
        presented_at: HashMap::new(),
        committed_at: HashMap::new(),
    };
    sys::say_to(1, &format!("wl-bridge: listening on {path}"));
    loop {
        s.run_once(wake_r, &rx);
    }
}

impl Server {
    fn next_serial(&mut self) -> u32 {
        self.serial = self.serial.wrapping_add(1);
        self.serial
    }

    fn run_once(&mut self, wake: Fd, events: &std::sync::mpsc::Receiver<sys::Event>) {
        let now = sys::now_ms();
        let timeout = if self.frames.is_empty() {
            // Client sockets, native events and clipboard pipes all wake poll.
            // No timer is needed when no frame callback is outstanding.
            -1
        } else {
            FRAME_MS.saturating_sub(now.wrapping_sub(self.last_frame)) as i32
        };
        let mut fds = vec![
            sys::PollFd { fd: self.listener, events: sys::POLLIN, revents: 0 },
            sys::PollFd { fd: wake, events: sys::POLLIN, revents: 0 },
        ];
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in &ids {
            let c = &self.clients[id];
            let events = sys::POLLIN | if c.out.is_empty() { 0 } else { sys::POLLOUT };
            fds.push(sys::PollFd { fd: c.fd, events, revents: 0 });
        }
        let clip_at = fds.len();
        for (fd, _) in &self.clip_in {
            fds.push(sys::PollFd { fd: *fd, events: sys::POLLIN, revents: 0 });
        }
        for (fd, _) in &self.clip_out {
            fds.push(sys::PollFd { fd: *fd, events: sys::POLLOUT, revents: 0 });
        }
        sys::poll(&mut fds, timeout);

        if fds[0].revents & sys::POLLIN != 0 {
            while let Some(fd) = sys::accept(self.listener) {
                let id = self.next_client;
                self.next_client += 1;
                self.clients.insert(id, Client::new(fd));
            }
        }
        if fds[1].revents != 0 {
            let mut b = [0u8; 256];
            while sys::read(wake, &mut b) > 0 {}
        }
        while let Ok(e) = events.try_recv() {
            self.wave_event(e);
        }
        for (i, id) in ids.iter().enumerate() {
            let rev = fds[2 + i].revents;
            if rev & sys::POLLIN != 0 {
                self.read_client(*id);
            } else if rev & (sys::POLLHUP | sys::POLLERR) != 0 {
                if let Some(c) = self.clients.get_mut(id) {
                    c.dead = true;
                }
            }
        }
        self.clipboard_io(&fds[clip_at..]);

        let dirty: Vec<u32> = self.windows.iter().filter(|(_, w)| w.dirty).map(|(id, _)| *id).collect();
        for id in dirty {
            self.compose(id);
        }
        let now = sys::now_ms();
        if !self.frames.is_empty() && now.wrapping_sub(self.last_frame) >= FRAME_MS {
            self.last_frame = now;
            // Frames for no window, or a window with nothing on its way to
            // the screen, go on the timer; the others wait for their window
            // (a while at most: a hidden window's picture is never shown).
            let windows = &self.windows;
            let due = |f: &Frame| match f.window.and_then(|w| windows.get(&w)) {
                Some(w) if w.showing => now.wrapping_sub(f.since) >= SHOWN_WAIT_MS,
                _ => true,
            };
            let (go, keep): (Vec<Frame>, Vec<Frame>) = std::mem::take(&mut self.frames).into_iter().partition(due);
            self.frames = keep;
            self.answer(go, None);
        }
        self.log_stats(false);
        let ids: Vec<ClientId> = self.clients.keys().copied().collect();
        for id in ids {
            let c = self.clients.get_mut(&id).unwrap();
            if c.dead || !c.flush() {
                self.disconnect(id);
            }
        }
    }

    // -------------------------------------------------------------- input

    fn read_client(&mut self, cid: ClientId) {
        let mut buf = vec![0u8; 65536];
        loop {
            let Some(c) = self.clients.get_mut(&cid) else { return };
            match sys::recv(c.fd, &mut buf) {
                Ok((0, _)) => {
                    c.dead = true;
                    return;
                }
                Ok((n, fds)) => {
                    c.inbuf.extend_from_slice(&buf[..n]);
                    c.infds.extend(fds);
                }
                Err(-11) => break,
                Err(_) => {
                    c.dead = true;
                    return;
                }
            }
        }
        loop {
            let Some(c) = self.clients.get_mut(&cid) else { return };
            if c.dead {
                return;
            }
            let objects = &c.objects;
            let parsed = wire::parse(&c.inbuf, &mut c.infds, |obj, op| {
                let (iface, _) = objects.get(&obj)?;
                requests(iface).get(op as usize).copied()
            });
            match parsed {
                Ok(Some((msg, n))) => {
                    c.inbuf.drain(..n);
                    self.request(cid, msg);
                }
                Ok(None) => break,
                Err(e) => {
                    let obj = u32::from_le_bytes(c.inbuf[0..4].try_into().unwrap());
                    sys::say(&format!("wl-bridge: client {cid}: {e}"));
                    c.send(1, wl_display::ev::ERROR, &[Out::O(obj), Out::U(1), Out::S(&e)]);
                    c.flush();
                    c.dead = true;
                    return;
                }
            }
        }
    }

    fn disconnect(&mut self, cid: ClientId) {
        let Some(c) = self.clients.remove(&cid) else { return };
        // What its frames cost, up to now.
        self.log_stats(true);
        let windows: Vec<u32> = self.windows.iter().filter(|(_, w)| w.client == cid).map(|(id, _)| *id).collect();
        for id in windows {
            self.windows.remove(&id);
            self.presented_at.remove(&id);
            self.committed_at.remove(&id);
            sys::win_close(id);
        }
        if self.pointer.is_some_and(|p| p.0 == cid) {
            self.pointer = None;
        }
        if self.selection.is_some_and(|s| s.0 == cid) {
            self.selection = None;
        }
        self.frames.retain(|f| f.client != cid);
        self.kernel_buffers.retain(|_, b| b.0 != cid);
        for fd in c.infds.iter().chain(c.outfds.iter()).chain(c.close_after.iter()) {
            sys::close(*fd);
        }
        sys::close(c.fd);
        // Pools unmap as they drop.
    }

    /// A destroyed object: forgotten, and the program told its id is free.
    fn destroyed(&mut self, cid: ClientId, id: Id) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        c.objects.remove(&id);
        if id < SERVER_IDS {
            c.send(1, wl_display::ev::DELETE_ID, &[Out::U(id)]);
        }
    }

    fn new_object(&mut self, cid: ClientId, id: Id, iface: &'static str, version: u32) {
        if let Some(c) = self.clients.get_mut(&cid) {
            c.objects.insert(id, (iface, version));
        }
    }

    fn request(&mut self, cid: ClientId, m: Message) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(&(iface, version)) = c.objects.get(&m.object) else { return };
        let a = &m.args;
        let obj = m.object;
        match (iface, m.opcode) {
            (wl_display::NAME, wl_display::req::SYNC) => {
                let serial = self.next_serial();
                let c = self.clients.get_mut(&cid).unwrap();
                let cb = a[0].uint();
                c.send(cb, wl_callback::ev::DONE, &[Out::U(serial)]);
                c.send(1, wl_display::ev::DELETE_ID, &[Out::U(cb)]);
            }
            (wl_display::NAME, wl_display::req::GET_REGISTRY) => {
                let reg = a[0].uint();
                self.new_object(cid, reg, wl_registry::NAME, 1);
                let copy_only = self.copy_only;
                let c = self.clients.get_mut(&cid).unwrap();
                for (i, (name, v)) in GLOBALS.iter().enumerate() {
                    if copy_only && dmabuf_global(name) {
                        continue;
                    }
                    c.send(reg, wl_registry::ev::GLOBAL, &[Out::U(i as u32 + 1), Out::S(name), Out::U(*v)]);
                }
            }
            (wl_registry::NAME, wl_registry::req::BIND) => {
                self.bind(cid, a[0].uint(), a[1].str(), a[2].uint(), a[3].uint())
            }

            (wl_compositor::NAME, wl_compositor::req::CREATE_SURFACE) => {
                self.new_object(cid, a[0].uint(), wl_surface::NAME, version);
                let c = self.clients.get_mut(&cid).unwrap();
                c.surfaces.insert(a[0].uint(), Surface::new());
                self.surface_enter(cid, a[0].uint());
            }
            (wl_compositor::NAME, wl_compositor::req::CREATE_REGION) => {
                self.new_object(cid, a[0].uint(), wl_region::NAME, 1)
            }
            (wl_region::NAME, wl_region::req::DESTROY) => {
                self.clients.get_mut(&cid).unwrap().regions.remove(&obj);
                self.destroyed(cid, obj);
            }
            (wl_region::NAME, op) => {
                let rect = (a[0].int(), a[1].int(), a[2].int(), a[3].int());
                let add = op == wl_region::req::ADD;
                self.clients.get_mut(&cid).unwrap().regions.entry(obj).or_default().push((add, rect));
            }

            (wl_shm::NAME, wl_shm::req::CREATE_POOL) => {
                let (id, fd, size) = (a[0].uint(), a[1].fd(), a[2].int().max(0) as usize);
                match Pool::map(fd, size) {
                    Some(p) => {
                        self.new_object(cid, id, wl_shm_pool::NAME, 1);
                        self.clients.get_mut(&cid).unwrap().pools.insert(id, Rc::new(RefCell::new(p)));
                    }
                    None => {
                        sys::close(fd);
                        self.error(cid, obj, 2, "can't map the pool");
                    }
                }
            }
            (wl_shm_pool::NAME, wl_shm_pool::req::CREATE_BUFFER) => {
                let c = self.clients.get_mut(&cid).unwrap();
                let Some(pool) = c.pools.get(&obj).cloned() else { return };
                let b = Buffer::shm(pool, a[1].int().max(0) as usize, a[2].int(), a[3].int(), a[4].int(), a[5].uint());
                let id = a[0].uint();
                c.buffers.insert(id, b);
                self.new_object(cid, id, wl_buffer::NAME, 1);
            }
            (wl_shm_pool::NAME, wl_shm_pool::req::DESTROY) => {
                self.clients.get_mut(&cid).unwrap().pools.remove(&obj);
                self.destroyed(cid, obj);
            }
            (wl_shm_pool::NAME, wl_shm_pool::req::RESIZE) => {
                let c = self.clients.get_mut(&cid).unwrap();
                if let Some(pool) = c.pools.get(&obj) {
                    let mut p = pool.borrow_mut();
                    let size = a[0].int().max(0) as usize;
                    if size > p.size {
                        if let Some(ptr) = sys::mmap_shared(p.fd, size) {
                            sys::munmap(p.ptr, p.size.max(1));
                            p.ptr = ptr;
                            p.size = size;
                        }
                    }
                }
            }
            (wl_buffer::NAME, wl_buffer::req::DESTROY) => {
                if let Some(b) = self.clients.get_mut(&cid).unwrap().buffers.remove(&obj) {
                    if let Some(k) = b.kernel.get() {
                        self.kernel_buffers.remove(&k);
                    }
                }
                self.destroyed(cid, obj);
            }

            (zwp_linux_dmabuf_v1::NAME, op) => self.dmabuf_request(cid, obj, op, a),
            (zwp_linux_buffer_params_v1::NAME, op) => self.params_request(cid, obj, op, a),
            (zwp_linux_dmabuf_feedback_v1::NAME, _) => self.destroyed(cid, obj),
            (wl_drm::NAME, op) => self.drm_request(cid, obj, op, a),
            (wp_presentation::NAME, wp_presentation::req::DESTROY) => self.destroyed(cid, obj),
            (wp_presentation::NAME, wp_presentation::req::FEEDBACK) => {
                let (sid, id) = (a[0].uint(), a[1].uint());
                let c = self.clients.get_mut(&cid).unwrap();
                c.objects.insert(id, (wp_presentation_feedback::NAME, 1));
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.pending.feedbacks.push(id);
                }
            }

            (wl_surface::NAME, op) => self.surface_request(cid, obj, op, a),

            (wl_subcompositor::NAME, wl_subcompositor::req::DESTROY) => self.destroyed(cid, obj),
            (wl_subcompositor::NAME, wl_subcompositor::req::GET_SUBSURFACE) => {
                let (id, sid, parent) = (a[0].uint(), a[1].uint(), a[2].uint());
                self.new_object(cid, id, wl_subsurface::NAME, 1);
                let c = self.clients.get_mut(&cid).unwrap();
                c.subsurfaces.insert(id, sid);
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.role = Role::Sub(parent);
                }
                if let Some(p) = c.surfaces.get_mut(&parent) {
                    p.children.push(sid);
                }
            }
            (wl_subsurface::NAME, op) => {
                let c = self.clients.get_mut(&cid).unwrap();
                let Some(&sid) = c.subsurfaces.get(&obj) else { return };
                let parent = match c.surfaces.get(&sid).map(|s| s.role) {
                    Some(Role::Sub(p)) => p,
                    _ => 0,
                };
                match op {
                    wl_subsurface::req::DESTROY => {
                        c.subsurfaces.remove(&obj);
                        if let Some(p) = c.surfaces.get_mut(&parent) {
                            p.children.retain(|&x| x != sid);
                        }
                        if let Some(s) = c.surfaces.get_mut(&sid) {
                            s.role = Role::None;
                        }
                        self.destroyed(cid, obj);
                        self.surface_changed(cid, parent);
                    }
                    wl_subsurface::req::SET_POSITION => {
                        if let Some(s) = c.surfaces.get_mut(&sid) {
                            s.pending_pos = Some((a[0].int(), a[1].int()));
                        }
                    }
                    wl_subsurface::req::PLACE_ABOVE | wl_subsurface::req::PLACE_BELOW => {
                        let sibling = a[0].uint();
                        if let Some(p) = c.surfaces.get_mut(&parent) {
                            p.children.retain(|&x| x != sid);
                            let at = p.children.iter().position(|&x| x == sibling);
                            let at = match (at, op == wl_subsurface::req::PLACE_ABOVE) {
                                (Some(i), true) => i + 1,
                                (Some(i), false) => i,
                                // The parent itself: children are above it.
                                (None, true) => 0,
                                (None, false) => 0,
                            };
                            p.children.insert(at.min(p.children.len()), sid);
                        }
                    }
                    _ => {}
                }
            }

            (wl_seat::NAME, wl_seat::req::GET_POINTER) => self.new_object(cid, a[0].uint(), wl_pointer::NAME, version),
            (wl_seat::NAME, wl_seat::req::GET_KEYBOARD) => {
                let id = a[0].uint();
                self.new_object(cid, id, wl_keyboard::NAME, version);
                let (fd, len) = self.keymap;
                let c = self.clients.get_mut(&cid).unwrap();
                c.send(id, wl_keyboard::ev::KEYMAP, &[Out::U(1), Out::H(fd), Out::U(len as u32)]);
                if version >= 4 {
                    // WaveOS repeats keys itself.
                    c.send(id, wl_keyboard::ev::REPEAT_INFO, &[Out::I(0), Out::I(0)]);
                }
                // A keyboard made while the program's window has the focus.
                let focused = self.windows.values().find(|w| w.client == cid && w.focused).map(|w| w.toplevel);
                if let Some(top) = focused {
                    self.keyboard_enter(cid, top, Some(id));
                }
            }
            (wl_seat::NAME, wl_seat::req::GET_TOUCH) => self.new_object(cid, a[0].uint(), wl_touch::NAME, version),
            (wl_seat::NAME, wl_seat::req::RELEASE)
            | (wl_pointer::NAME, wl_pointer::req::RELEASE)
            | (wl_keyboard::NAME, wl_keyboard::req::RELEASE)
            | (wl_touch::NAME, wl_touch::req::RELEASE)
            | (wl_output::NAME, wl_output::req::RELEASE) => self.destroyed(cid, obj),
            (wl_pointer::NAME, wl_pointer::req::SET_CURSOR) => {
                // WaveOS draws its own pointer: the arrow, or none when the
                // program hides it (a game; the display may then scan the
                // game out even without a cursor plane).
                let c = self.clients.get_mut(&cid).unwrap();
                let surface = a[1].uint();
                if let Some(s) = c.surfaces.get_mut(&surface) {
                    s.role = Role::Cursor;
                }
                if let Some((_, _, wid)) = self.pointer.filter(|p| p.0 == cid) {
                    sys::win_set_cursor(wid, if surface == 0 { sys::CURSOR_HIDDEN } else { sys::CURSOR_ARROW });
                }
            }

            (xdg_wm_base::NAME, xdg_wm_base::req::DESTROY) => self.destroyed(cid, obj),
            (xdg_wm_base::NAME, xdg_wm_base::req::CREATE_POSITIONER) => {
                self.new_object(cid, a[0].uint(), xdg_positioner::NAME, version);
                self.clients.get_mut(&cid).unwrap().positioners.insert(a[0].uint(), Positioner::default());
            }
            (xdg_wm_base::NAME, xdg_wm_base::req::GET_XDG_SURFACE) => {
                let (id, sid) = (a[0].uint(), a[1].uint());
                self.new_object(cid, id, xdg_surface::NAME, version);
                let c = self.clients.get_mut(&cid).unwrap();
                c.xdg.insert(id, XdgSurface { surface: sid, role: XdgRole::None, geometry: None, configured: false });
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.role = Role::Xdg(id);
                }
            }
            (xdg_wm_base::NAME, xdg_wm_base::req::PONG) => {}
            (xdg_positioner::NAME, op) => {
                let c = self.clients.get_mut(&cid).unwrap();
                if op == xdg_positioner::req::DESTROY {
                    c.positioners.remove(&obj);
                    self.destroyed(cid, obj);
                    return;
                }
                let Some(p) = c.positioners.get_mut(&obj) else { return };
                match op {
                    xdg_positioner::req::SET_SIZE => p.size = (a[0].int(), a[1].int()),
                    xdg_positioner::req::SET_ANCHOR_RECT => {
                        p.anchor_rect = (a[0].int(), a[1].int(), a[2].int(), a[3].int())
                    }
                    xdg_positioner::req::SET_ANCHOR => p.anchor = a[0].uint(),
                    xdg_positioner::req::SET_GRAVITY => p.gravity = a[0].uint(),
                    xdg_positioner::req::SET_CONSTRAINT_ADJUSTMENT => p.constraint = a[0].uint(),
                    xdg_positioner::req::SET_OFFSET => p.offset = (a[0].int(), a[1].int()),
                    _ => {}
                }
            }
            (xdg_surface::NAME, op) => self.xdg_surface_request(cid, obj, op, a, version),
            (xdg_toplevel::NAME, op) => self.toplevel_request(cid, obj, op, a),
            (xdg_popup::NAME, op) => self.popup_request(cid, obj, op, a),

            (zxdg_decoration_manager_v1::NAME, zxdg_decoration_manager_v1::req::DESTROY) => self.destroyed(cid, obj),
            (zxdg_decoration_manager_v1::NAME, zxdg_decoration_manager_v1::req::GET_TOPLEVEL_DECORATION) => {
                let id = a[0].uint();
                self.new_object(cid, id, zxdg_toplevel_decoration_v1::NAME, 1);
                // WaveOS draws the frame.
                self.clients.get_mut(&cid).unwrap().send(id, zxdg_toplevel_decoration_v1::ev::CONFIGURE, &[Out::U(2)]);
            }
            (zxdg_toplevel_decoration_v1::NAME, zxdg_toplevel_decoration_v1::req::DESTROY) => self.destroyed(cid, obj),
            (zxdg_toplevel_decoration_v1::NAME, _) => {
                self.clients.get_mut(&cid).unwrap().send(obj, zxdg_toplevel_decoration_v1::ev::CONFIGURE, &[Out::U(2)])
            }

            (wp_viewporter::NAME, wp_viewporter::req::DESTROY) => self.destroyed(cid, obj),
            (wp_viewporter::NAME, wp_viewporter::req::GET_VIEWPORT) => {
                self.new_object(cid, a[0].uint(), wp_viewport::NAME, 1);
                self.clients.get_mut(&cid).unwrap().viewports.insert(a[0].uint(), a[1].uint());
            }
            (wp_viewport::NAME, op) => {
                let c = self.clients.get_mut(&cid).unwrap();
                let Some(&sid) = c.viewports.get(&obj) else { return };
                let Some(s) = c.surfaces.get_mut(&sid) else { return };
                match op {
                    wp_viewport::req::DESTROY => {
                        s.geometry_changed |= s.src.is_some() || s.dst.is_some();
                        s.src = None;
                        s.dst = None;
                        c.viewports.remove(&obj);
                        self.destroyed(cid, obj);
                    }
                    wp_viewport::req::SET_SOURCE => {
                        let r = (a[0].fixed(), a[1].fixed(), a[2].fixed(), a[3].fixed());
                        let source = (r.2 > 0.0 && r.3 > 0.0).then_some(r);
                        s.geometry_changed |= s.src != source;
                        s.src = source;
                    }
                    wp_viewport::req::SET_DESTINATION => {
                        let d = (a[0].int(), a[1].int());
                        let destination = (d.0 > 0 && d.1 > 0).then_some(d);
                        s.geometry_changed |= s.dst != destination;
                        s.dst = destination;
                    }
                    _ => {}
                }
            }

            (wl_data_device_manager::NAME, wl_data_device_manager::req::CREATE_DATA_SOURCE) => {
                self.new_object(cid, a[0].uint(), wl_data_source::NAME, version);
                self.clients.get_mut(&cid).unwrap().data_sources.insert(a[0].uint(), Vec::new());
            }
            (wl_data_device_manager::NAME, wl_data_device_manager::req::GET_DATA_DEVICE) => {
                self.new_object(cid, a[0].uint(), wl_data_device::NAME, version)
            }
            (wl_data_source::NAME, wl_data_source::req::OFFER) => {
                if let Some(m) = self.clients.get_mut(&cid).unwrap().data_sources.get_mut(&obj) {
                    m.push(a[0].str().to_string());
                }
            }
            (wl_data_source::NAME, wl_data_source::req::DESTROY) => {
                self.clients.get_mut(&cid).unwrap().data_sources.remove(&obj);
                if self.selection == Some((cid, obj)) {
                    self.selection = None;
                }
                self.destroyed(cid, obj);
            }
            (wl_data_source::NAME, _) => {}
            (wl_data_device::NAME, wl_data_device::req::SET_SELECTION) => self.set_selection(cid, a[0].uint()),
            (wl_data_device::NAME, wl_data_device::req::START_DRAG) => {
                // No drag and drop between programs yet: the drag ends at once.
                let source = a[0].uint();
                if source != 0 {
                    self.clients.get_mut(&cid).unwrap().send(source, wl_data_source::ev::CANCELLED, &[]);
                }
            }
            (wl_data_device::NAME, wl_data_device::req::RELEASE) => self.destroyed(cid, obj),
            (wl_data_offer::NAME, wl_data_offer::req::RECEIVE) => {
                let fd = a[1].fd();
                match sys::clipboard_text() {
                    Some(text) => {
                        sys::set_nonblock(fd);
                        self.clip_out.push((fd, text.into_bytes()));
                    }
                    None => sys::close(fd),
                }
            }
            (wl_data_offer::NAME, wl_data_offer::req::DESTROY) => self.destroyed(cid, obj),
            (wl_data_offer::NAME, _) => {}

            _ => {}
        }
    }

    // ------------------------------------------------------------ dma-bufs

    fn dmabuf_request(&mut self, cid: ClientId, obj: Id, op: u16, a: &[Arg]) {
        let version = self.clients.get(&cid).map_or(1, |c| c.version(obj));
        match op {
            zwp_linux_dmabuf_v1::req::DESTROY => self.destroyed(cid, obj),
            zwp_linux_dmabuf_v1::req::CREATE_PARAMS => {
                self.new_object(cid, a[0].uint(), zwp_linux_buffer_params_v1::NAME, version);
                self.clients.get_mut(&cid).unwrap().params.insert(a[0].uint(), None);
            }
            zwp_linux_dmabuf_v1::req::GET_DEFAULT_FEEDBACK | zwp_linux_dmabuf_v1::req::GET_SURFACE_FEEDBACK => {
                let id = a[0].uint();
                self.new_object(cid, id, zwp_linux_dmabuf_feedback_v1::NAME, version);
                self.send_feedback(cid, id);
            }
            _ => {}
        }
    }

    /// Where to render and in what: card0, one tranche of the formats
    /// WaveOS shows (XRGB8888, ARGB8888), linear (or the driver's implicit
    /// layout, linear too).
    fn send_feedback(&mut self, cid: ClientId, id: Id) {
        let mut table = Vec::new();
        for f in [buffers::XRGB8888, buffers::ARGB8888] {
            for m in [buffers::MOD_LINEAR, buffers::MOD_INVALID] {
                table.extend_from_slice(&f.to_le_bytes());
                table.extend_from_slice(&0u32.to_le_bytes());
                table.extend_from_slice(&m.to_le_bytes());
            }
        }
        let entries = table.len() / 16;
        let fd = sys::memfd_with(&table);
        let dev = DRM_DEV_T.to_le_bytes();
        let indices: Vec<u8> = (0..entries as u16).flat_map(u16::to_le_bytes).collect();
        let c = self.clients.get_mut(&cid).unwrap();
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::FORMAT_TABLE, &[Out::H(fd), Out::U(table.len() as u32)]);
        c.close_after.push(fd);
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::MAIN_DEVICE, &[Out::A(&dev)]);
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::TRANCHE_TARGET_DEVICE, &[Out::A(&dev)]);
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::TRANCHE_FORMATS, &[Out::A(&indices)]);
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::TRANCHE_FLAGS, &[Out::U(0)]);
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::TRANCHE_DONE, &[]);
        c.send(id, zwp_linux_dmabuf_feedback_v1::ev::DONE, &[]);
    }

    fn params_request(&mut self, cid: ClientId, obj: Id, op: u16, a: &[Arg]) {
        use zwp_linux_buffer_params_v1::{ev, req};
        // Errors: 0 already used, 1 plane index, 2 plane set, 3 incomplete,
        // 4 invalid format, 5 invalid dimensions, 7 invalid wl_buffer.
        let c = self.clients.get_mut(&cid).unwrap();
        match op {
            req::DESTROY => {
                if let Some(Some((fd, ..))) = c.params.remove(&obj) {
                    sys::close(fd);
                }
                self.destroyed(cid, obj);
            }
            req::ADD => {
                let fd = a[0].fd();
                let modifier = (a[4].uint() as u64) << 32 | a[5].uint() as u64;
                let problem = match c.params.get(&obj) {
                    None => Some((0, "already used")),
                    _ if a[1].uint() != 0 => Some((1, "one plane only")),
                    Some(Some(_)) => Some((2, "plane 0 is set")),
                    _ if modifier != buffers::MOD_LINEAR && modifier != buffers::MOD_INVALID => {
                        Some((4, "linear buffers only"))
                    }
                    _ => None,
                };
                if let Some((code, text)) = problem {
                    sys::close(fd);
                    return self.error(cid, obj, code, text);
                }
                c.params.insert(obj, Some((fd, a[2].uint(), a[3].uint(), modifier)));
            }
            req::CREATE | req::CREATE_IMMED => {
                let immed = op == req::CREATE_IMMED;
                let args = if immed { &a[1..] } else { a };
                let (w, h, format) = (args[0].int(), args[1].int(), args[2].uint());
                let Some(plane) = c.params.insert(obj, None) else { return self.error(cid, obj, 0, "already used") };
                c.params.remove(&obj);
                let Some((fd, offset, stride, _)) = plane else { return self.error(cid, obj, 3, "no plane") };
                if !Buffer::supported(format) || w <= 0 || h <= 0 || (stride as i64) < w as i64 * 4 {
                    sys::close(fd);
                    if !immed {
                        c.send(obj, ev::FAILED, &[]);
                        return;
                    }
                    return self.error(cid, obj, 7, "a buffer WaveOS can't show");
                }
                let id = if immed {
                    a[0].uint()
                } else {
                    c.next_server_id += 1;
                    c.next_server_id - 1
                };
                c.buffers.insert(id, Buffer::dma(fd, offset as usize, w, h, stride as i32, format));
                if !immed {
                    c.send(obj, ev::CREATED, &[Out::N(id)]);
                }
                self.new_object(cid, id, wl_buffer::NAME, 1);
            }
            _ => {}
        }
    }

    /// Mesa's `wl_drm`: how its EGL learns which device to render on (no
    /// render node is needed to use it) and hands over PRIME buffers.
    fn drm_request(&mut self, cid: ClientId, obj: Id, op: u16, a: &[Arg]) {
        let c = self.clients.get_mut(&cid).unwrap();
        match op {
            // WaveOS's DRM device asks no authentication of programs.
            wl_drm::req::AUTHENTICATE => c.send(obj, wl_drm::ev::AUTHENTICATED, &[]),
            wl_drm::req::CREATE_PRIME_BUFFER => {
                let (id, fd, w, h, format) = (a[0].uint(), a[1].fd(), a[2].int(), a[3].int(), a[4].uint());
                let (offset, stride) = (a[5].int(), a[6].int());
                if !Buffer::supported(format) || w <= 0 || h <= 0 || stride < w.saturating_mul(4) || offset < 0 {
                    sys::close(fd);
                    // Errors: 1 invalid format, 2 invalid name.
                    return self.error(cid, obj, 1, "a buffer WaveOS can't show");
                }
                c.buffers.insert(id, Buffer::dma(fd, offset as usize, w, h, stride, format));
                self.new_object(cid, id, wl_buffer::NAME, 1);
            }
            // GEM names (flink) are not shared between programs.
            _ => self.error(cid, obj, 2, "only PRIME buffers"),
        }
    }

    fn error(&mut self, cid: ClientId, obj: Id, code: u32, text: &str) {
        if let Some(c) = self.clients.get_mut(&cid) {
            c.send(1, wl_display::ev::ERROR, &[Out::O(obj), Out::U(code), Out::S(text)]);
            c.flush();
            c.dead = true;
        }
    }

    fn bind(&mut self, cid: ClientId, name: u32, iface: &str, version: u32, id: Id) {
        let Some(&(global, max)) = GLOBALS.get(name.wrapping_sub(1) as usize) else {
            return self.error(cid, 1, 0, "no such global");
        };
        if global != iface {
            return self.error(cid, 1, 0, "wrong interface for global");
        }
        let v = version.clamp(1, max);
        self.new_object(cid, id, global, v);
        let (scale, screen) = (self.scale, self.screen);
        let c = self.clients.get_mut(&cid).unwrap();
        match global {
            wl_shm::NAME => {
                c.send(id, wl_shm::ev::FORMAT, &[Out::U(0)]);
                c.send(id, wl_shm::ev::FORMAT, &[Out::U(1)]);
            }
            wl_seat::NAME => {
                c.send(id, wl_seat::ev::CAPABILITIES, &[Out::U(3)]);
                if v >= 2 {
                    c.send(id, wl_seat::ev::NAME, &[Out::S("seat0")]);
                }
            }
            wl_output::NAME => Self::output_info(c, id, v, screen, scale),
            zwp_linux_dmabuf_v1::NAME if v < 4 => {
                for f in [buffers::XRGB8888, buffers::ARGB8888] {
                    if v < 3 {
                        c.send(id, zwp_linux_dmabuf_v1::ev::FORMAT, &[Out::U(f)]);
                        continue;
                    }
                    for m in [buffers::MOD_LINEAR, buffers::MOD_INVALID] {
                        let (hi, lo) = ((m >> 32) as u32, m as u32);
                        c.send(id, zwp_linux_dmabuf_v1::ev::MODIFIER, &[Out::U(f), Out::U(hi), Out::U(lo)]);
                    }
                }
            }
            wl_drm::NAME => {
                c.send(id, wl_drm::ev::DEVICE, &[Out::S(DRM_DEVICE)]);
                for f in [buffers::XRGB8888, buffers::ARGB8888] {
                    c.send(id, wl_drm::ev::FORMAT, &[Out::U(f)]);
                }
                if v >= 2 {
                    // PRIME: buffers come as dma-buf descriptors.
                    c.send(id, wl_drm::ev::CAPABILITIES, &[Out::U(1)]);
                }
            }
            // CLOCK_MONOTONIC: what WaveOS's frame times are in.
            wp_presentation::NAME => c.send(id, wp_presentation::ev::CLOCK_ID, &[Out::U(1)]),
            _ => {}
        }
    }

    fn output_info(c: &mut Client, id: Id, v: u32, screen: (i32, i32), scale: i32) {
        // A 96 dpi screen (192 at 2×).
        let mm = |px: i32| px * 254 / (960 * scale.max(1));
        c.send(
            id,
            wl_output::ev::GEOMETRY,
            &[
                Out::I(0),
                Out::I(0),
                Out::I(mm(screen.0)),
                Out::I(mm(screen.1)),
                Out::I(0),
                Out::S("WaveOS"),
                Out::S("Display"),
                Out::I(0),
            ],
        );
        c.send(id, wl_output::ev::MODE, &[Out::U(3), Out::I(screen.0), Out::I(screen.1), Out::I(60000)]);
        if v >= 2 {
            c.send(id, wl_output::ev::SCALE, &[Out::I(scale)]);
            c.send(id, wl_output::ev::DONE, &[]);
        }
    }

    /// Tells a program its new surface is on the (only) screen.
    fn surface_enter(&mut self, cid: ClientId, sid: Id) {
        let c = self.clients.get_mut(&cid).unwrap();
        for out in c.all(wl_output::NAME) {
            c.send(sid, wl_surface::ev::ENTER, &[Out::O(out)]);
        }
    }

    // ----------------------------------------------------------- surfaces

    fn surface_request(&mut self, cid: ClientId, sid: Id, op: u16, a: &[Arg]) {
        let c = self.clients.get_mut(&cid).unwrap();
        match op {
            wl_surface::req::DESTROY => {
                let role = c.surfaces.get(&sid).map(|s| s.role);
                c.surfaces.remove(&sid);
                for s in c.surfaces.values_mut() {
                    s.children.retain(|&x| x != sid);
                }
                if let Some(Role::Sub(parent)) = role {
                    self.surface_changed(cid, parent);
                }
                self.destroyed(cid, sid);
                if self.pointer.is_some_and(|p| p.0 == cid && p.1 == sid) {
                    self.pointer = None;
                }
            }
            wl_surface::req::ATTACH => {
                let Some(s) = c.surfaces.get_mut(&sid) else { return };
                let b = a[0].uint();
                s.pending.buffer = Some((b != 0).then_some(b));
                s.pending.offset = (a[1].int(), a[2].int());
            }
            wl_surface::req::DAMAGE | wl_surface::req::DAMAGE_BUFFER => {
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    let r = Rect::new(a[0].int(), a[1].int(), a[2].int(), a[3].int());
                    damage::add(
                        if op == wl_surface::req::DAMAGE {
                            &mut s.pending.damage
                        } else {
                            &mut s.pending.buffer_damage
                        },
                        r,
                    );
                }
            }
            wl_surface::req::FRAME => {
                let cb = a[0].uint();
                c.objects.insert(cb, (wl_callback::NAME, 1));
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.pending.frames.push(cb);
                }
            }
            wl_surface::req::SET_INPUT_REGION => {
                let region = (a[0].uint() != 0).then(|| c.regions.get(&a[0].uint()).cloned().unwrap_or_default());
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.pending.input = Some(region);
                }
            }
            wl_surface::req::SET_BUFFER_SCALE => {
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.pending.scale = Some(a[0].int().clamp(1, 4));
                }
            }
            wl_surface::req::OFFSET => {
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.pending.offset = (a[0].int(), a[1].int());
                }
            }
            wl_surface::req::COMMIT => self.commit(cid, sid),
            // Damage, regions and transforms: everything is redrawn.
            _ => {}
        }
    }

    fn commit(&mut self, cid: ClientId, sid: Id) {
        let started = sys::monotonic_ns();
        // A surface of a window (its main surface, a sub-surface, a pop-up),
        // shown or not yet, may be shown as it is: the window decides when it
        // composes (see `compose`). Its buffer is held meanwhile.
        let shown = {
            let c = &self.clients[&cid];
            c.xdg_of(sid).and_then(|x| c.toplevel_of(x)).is_some()
        };
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(s) = c.surfaces.get_mut(&sid) else { return };
        let p = std::mem::take(&mut s.pending);
        let geometry_changed = std::mem::take(&mut s.geometry_changed)
            || p.scale.is_some_and(|scale| scale != s.scale)
            || s.pending_pos.is_some_and(|pos| pos != s.pos);
        let mut changed = geometry_changed || p.buffer.is_some() || p.damage.is_some() || p.buffer_damage.is_some();
        let mut copied = None;
        if let Some(scale) = p.scale {
            s.scale = scale;
        }
        if let Some(input) = p.input {
            s.input = input;
        }
        if let Some(pos) = s.pending_pos.take() {
            s.pos = pos;
        }
        let mut released = None;
        if let Some(buffer) = p.buffer {
            s.attached = buffer;
        }
        let buffer = if p.buffer.is_some() || p.damage.is_some() || p.buffer_damage.is_some() {
            s.attached.map(Some).or(Some(None))
        } else {
            None
        };
        match buffer {
            Some(Some(bid)) => {
                if let Some(b) = c.buffers.get(&bid) {
                    let mut region = p.buffer_damage;
                    if let Some(r) = p.damage {
                        if !s.identity_viewport() {
                            region = None;
                        } else {
                            damage::add(&mut region, r.scaled(s.scale));
                        }
                    }
                    if shown && !self.copy_only && Buffer::supported(b.format) && b.width > 0 && b.height > 0 {
                        s.direct = true;
                        let full = Rect::new(0, 0, b.width, b.height);
                        let resized = (s.bw, s.bh) != (b.width, b.height) || s.opaque != b.opaque();
                        s.direct_damage = match (s.direct_damage, if geometry_changed || resized { None } else { region }) {
                            (Some(a), Some(r)) => Some(a.union(r)),
                            (None, Some(r)) => Some(r),
                            _ => Some(full),
                        };
                        s.bw = b.width;
                        s.bh = b.height;
                        s.opaque = b.opaque();
                        copied = if geometry_changed || resized { None } else { s.direct_damage };
                    } else {
                        copied = copy_buffer(b, s, if geometry_changed { None } else { region });
                        if !geometry_changed && copied.is_some_and(Rect::empty) {
                            changed = false;
                        }
                        s.direct = false;
                        s.pixels_current = copied.is_some();
                        released = Some(bid);
                    }
                    s.has_content = true;
                }
            }
            Some(None) => {
                s.has_content = false;
                s.direct = false;
                s.pixels = Vec::new();
            }
            None => {}
        }
        let role = s.role;
        let window = c.xdg_of(sid).and_then(|x| c.toplevel_of(x)).and_then(|t| c.toplevels.get(&t)).and_then(|t| t.window);
        if let Some(w) = window {
            self.committed_at.entry(w).or_insert(started);
        }
        let now = sys::now_ms();
        if !p.frames.is_empty() && self.answered_at > 0 {
            self.stats.client_ns += sys::monotonic_ns().saturating_sub(self.answered_at);
            self.stats.client += 1;
            self.answered_at = 0;
        }
        for cb in p.frames {
            self.frames.push(Frame { client: cid, id: cb, feedback: false, window, since: now });
        }
        for f in p.feedbacks {
            self.frames.push(Frame { client: cid, id: f, feedback: true, window, since: now });
        }
        if let Some(bid) = released {
            c.send(bid, wl_buffer::ev::RELEASE, &[]);
        }
        match role {
            Role::Xdg(xid) => {
                let Some(x) = c.xdg.get(&xid) else { return };
                if !x.configured {
                    self.first_configure(cid, xid);
                    return;
                }
                if let XdgRole::Toplevel(top) = x.role {
                    let has_window = c.toplevels.get(&top).is_some_and(|t| t.window.is_some());
                    let has_content = c.surfaces.get(&sid).is_some_and(|s| s.has_content);
                    if !has_window && has_content {
                        self.open_window(cid, top);
                    } else if has_window && !has_content {
                        // Unmapped: the window goes until the next buffer.
                        self.close_window(cid, top);
                    } else {
                        self.check_size(cid, top);
                    }
                }
                if changed {
                    self.surface_changed_damage(cid, sid, if geometry_changed { None } else { copied });
                }
            }
            Role::Sub(_) if changed => {
                self.surface_changed_damage(cid, sid, if geometry_changed { None } else { copied })
            }
            _ => {}
        }
    }

    /// Marks the window showing surface `sid` for drawing again.
    fn surface_changed(&mut self, cid: ClientId, sid: Id) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(top) = c.xdg_of(sid).and_then(|x| c.toplevel_of(x)) else { return };
        let Some(wid) = c.toplevels.get(&top).and_then(|t| t.window) else { return };
        if let Some(w) = self.windows.get_mut(&wid) {
            w.dirty = true;
            w.damage = None;
        }
    }

    fn surface_changed_damage(&mut self, cid: ClientId, sid: Id, region: Option<Rect>) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(xid) = c.xdg_of(sid) else { return };
        let Some(top) = c.toplevel_of(xid) else { return };
        let Some(t) = c.toplevels.get(&top) else { return };
        let Some(wid) = t.window else { return };
        let mut partial = region;
        let mut at = sid;
        let mut offset = c.origin(xid);
        for _ in 0..64 {
            let Some(surface) = c.surfaces.get(&at) else {
                partial = None;
                break;
            };
            if !surface.identity_viewport() {
                partial = None;
            }
            if at == sid {
                partial = partial.map(|r| r.points(surface.scale));
            }
            match surface.role {
                Role::Sub(parent) => {
                    offset.0 += surface.pos.0;
                    offset.1 += surface.pos.1;
                    at = parent;
                }
                _ => break,
            }
        }
        let g = c.geometry(t.xdg);
        if let Some(w) = self.windows.get_mut(&wid) {
            partial =
                partial.map(|r| r.translated(offset.0 - g.0, offset.1 - g.1).scaled(w.surface.scale.max(1) as i32));
            if !w.dirty {
                w.damage = partial;
            } else if let (Some(a), Some(b)) = (w.damage, partial) {
                w.damage = Some(a.union(b));
            } else {
                w.damage = None;
            }
            w.dirty = true;
        }
    }

    // ---------------------------------------------------------------- xdg

    fn xdg_surface_request(&mut self, cid: ClientId, xid: Id, op: u16, a: &[Arg], version: u32) {
        let c = self.clients.get_mut(&cid).unwrap();
        match op {
            xdg_surface::req::DESTROY => {
                if let Some(x) = c.xdg.remove(&xid) {
                    if let Some(s) = c.surfaces.get_mut(&x.surface) {
                        s.role = Role::None;
                    }
                }
                self.destroyed(cid, xid);
            }
            xdg_surface::req::GET_TOPLEVEL => {
                let id = a[0].uint();
                if let Some(x) = c.xdg.get_mut(&xid) {
                    x.role = XdgRole::Toplevel(id);
                }
                c.toplevels.insert(
                    id,
                    Toplevel { xdg: xid, title: String::new(), window: None, asked: None, drawn: (0, 0), fullscreen: false },
                );
                self.new_object(cid, id, xdg_toplevel::NAME, version);
            }
            xdg_surface::req::GET_POPUP => {
                let (id, parent, pos) = (a[0].uint(), a[1].uint(), a[2].uint());
                let positioner = c.positioners.get(&pos).copied().unwrap_or_default();
                if let Some(x) = c.xdg.get_mut(&xid) {
                    x.role = XdgRole::Popup(id);
                }
                c.popups.push((id, Popup { xdg: xid, parent, rect: (0, 0, 0, 0), positioner }));
                self.new_object(cid, id, xdg_popup::NAME, version);
                self.place_popup(cid, id);
            }
            xdg_surface::req::SET_WINDOW_GEOMETRY => {
                if let Some(x) = c.xdg.get_mut(&xid) {
                    let g = (a[0].int(), a[1].int(), a[2].int(), a[3].int());
                    let geometry = (g.2 > 0 && g.3 > 0).then_some(g);
                    if x.geometry != geometry {
                        if let Some(surface) = c.surfaces.get_mut(&x.surface) {
                            surface.geometry_changed = true;
                        }
                        x.geometry = geometry;
                    }
                }
            }
            _ => {}
        }
    }

    fn toplevel_request(&mut self, cid: ClientId, top: Id, op: u16, a: &[Arg]) {
        let c = self.clients.get_mut(&cid).unwrap();
        match op {
            xdg_toplevel::req::DESTROY => {
                self.close_window(cid, top);
                let c = self.clients.get_mut(&cid).unwrap();
                if let Some(t) = c.toplevels.remove(&top) {
                    if let Some(x) = c.xdg.get_mut(&t.xdg) {
                        x.role = XdgRole::None;
                    }
                }
                self.destroyed(cid, top);
            }
            xdg_toplevel::req::SET_TITLE => {
                let Some(t) = c.toplevels.get_mut(&top) else { return };
                t.title = a[0].str().to_string();
                if let Some(w) = t.window {
                    sys::win_set_title(w, &t.title);
                }
            }
            xdg_toplevel::req::SET_FULLSCREEN | xdg_toplevel::req::UNSET_FULLSCREEN => {
                let on = op == xdg_toplevel::req::SET_FULLSCREEN;
                let Some(t) = c.toplevels.get_mut(&top) else { return };
                t.fullscreen = on;
                match t.window {
                    // WaveOS says when it is (`FULLSCREEN`, then a `RESIZE`).
                    Some(w) => sys::win_set_fullscreen(w, on),
                    // Not shown yet: it opens full screen, at the display's size.
                    None if t.asked.is_some() => {
                        let size = if on { self.fullscreen_size() } else { (0, 0) };
                        self.configure_toplevel(cid, top, size);
                    }
                    None => {}
                }
            }
            _ => {}
        }
    }

    /// The main display's size in points (a full-screen window's size).
    fn fullscreen_size(&self) -> (i32, i32) {
        let k = self.scale.max(1);
        (self.screen.0 / k, self.screen.1 / k)
    }

    fn popup_request(&mut self, cid: ClientId, pid: Id, op: u16, a: &[Arg]) {
        let c = self.clients.get_mut(&cid).unwrap();
        match op {
            xdg_popup::req::DESTROY => {
                let xdg = c.popup(pid).map(|p| p.parent);
                c.popups.retain(|(id, _)| *id != pid);
                if let Some(parent) = xdg.and_then(|x| c.xdg.get(&x)).map(|x| x.surface) {
                    self.surface_changed(cid, parent);
                }
                self.destroyed(cid, pid);
            }
            xdg_popup::req::REPOSITION => {
                let positioner = c.positioners.get(&a[0].uint()).copied();
                if let Some((_, p)) = c.popups.iter_mut().find(|(id, _)| *id == pid) {
                    if let Some(pos) = positioner {
                        p.positioner = pos;
                    }
                }
                c.send(pid, xdg_popup::ev::REPOSITIONED, &[Out::U(a[1].uint())]);
                self.place_popup(cid, pid);
                self.configure_popup(cid, pid);
            }
            _ => {}
        }
    }

    /// Works out where a pop-up goes.
    fn place_popup(&mut self, cid: ClientId, pid: Id) {
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(p) = c.popup(pid) else { return };
        let parent = p.parent;
        // The window, in the parent's geometry coordinates.
        let top = c.toplevel_of(parent);
        let win = top.and_then(|t| c.toplevels.get(&t)).and_then(|t| t.window);
        let size = win.and_then(|w| self.windows.get(&w)).map(|w| w.size);
        let (ox, oy) = c.origin(parent);
        let g = c.geometry(parent);
        let tg = top.and_then(|t| c.toplevels.get(&t)).map_or((0, 0, 0, 0), |t| c.geometry(t.xdg));
        let (bw, bh) = size.unwrap_or((tg.2, tg.3));
        let bounds = (tg.0 - ox - g.0, tg.1 - oy - g.1, bw.max(1), bh.max(1));
        let rect = p.positioner.place(bounds);
        if let Some((_, p)) = c.popups.iter_mut().find(|(id, _)| *id == pid) {
            p.rect = rect;
        }
    }

    fn configure_popup(&mut self, cid: ClientId, pid: Id) {
        let serial = self.next_serial();
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(p) = c.popup(pid) else { return };
        let (xdg, r) = (p.xdg, p.rect);
        c.send(pid, xdg_popup::ev::CONFIGURE, &[Out::I(r.0), Out::I(r.1), Out::I(r.2), Out::I(r.3)]);
        c.send(xdg, xdg_surface::ev::CONFIGURE, &[Out::U(serial)]);
    }

    fn first_configure(&mut self, cid: ClientId, xid: Id) {
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(x) = c.xdg.get_mut(&xid) else { return };
        x.configured = true;
        let role = x.role;
        let full = matches!(role, XdgRole::Toplevel(top) if c.toplevels.get(&top).is_some_and(|t| t.fullscreen));
        match role {
            XdgRole::Toplevel(top) => {
                let size = if full { self.fullscreen_size() } else { (0, 0) };
                self.configure_toplevel(cid, top, size)
            }
            XdgRole::Popup(pid) => {
                self.place_popup(cid, pid);
                self.configure_popup(cid, pid);
            }
            XdgRole::None => {}
        }
    }

    /// Asks a toplevel to take `size` (0×0: its own choice).
    fn configure_toplevel(&mut self, cid: ClientId, top: Id, size: (i32, i32)) {
        let serial = self.next_serial();
        let focused = self.windows.values().any(|w| w.client == cid && w.toplevel == top && w.focused);
        let (screen, scale) = (self.screen, self.scale.max(1));
        let c = self.clients.get_mut(&cid).unwrap();
        let v = c.version(top);
        let Some(t) = c.toplevels.get_mut(&top) else { return };
        t.asked = Some(size);
        let (xdg, fullscreen) = (t.xdg, t.fullscreen);
        if v >= 4 {
            c.send(top, xdg_toplevel::ev::CONFIGURE_BOUNDS, &[Out::I(screen.0 / scale), Out::I(screen.1 / scale)]);
        }
        if v >= 5 {
            c.send(top, xdg_toplevel::ev::WM_CAPABILITIES, &[Out::A(&[])]);
        }
        // States: 2 fullscreen, 4 activated.
        let mut states: Vec<u8> = Vec::new();
        if fullscreen {
            states.extend_from_slice(&2u32.to_le_bytes());
        }
        if focused {
            states.extend_from_slice(&4u32.to_le_bytes());
        }
        c.send(top, xdg_toplevel::ev::CONFIGURE, &[Out::I(size.0), Out::I(size.1), Out::A(&states)]);
        c.send(xdg, xdg_surface::ev::CONFIGURE, &[Out::U(serial)]);
    }

    /// The size a toplevel shows (its window geometry).
    fn content_size(&self, cid: ClientId, top: Id) -> (i32, i32) {
        let c = &self.clients[&cid];
        let xdg = c.toplevels.get(&top).map_or(0, |t| t.xdg);
        let g = c.geometry(xdg);
        (g.2, g.3)
    }

    fn open_window(&mut self, cid: ClientId, top: Id) {
        let (w, h) = self.content_size(cid, top);
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(t) = c.toplevels.get_mut(&top) else { return };
        let title = if t.title.is_empty() { "Linux program" } else { &t.title };
        let Some(id) = sys::win_create(w.max(1) as u32, h.max(1) as u32, title) else {
            sys::say("wl-bridge: no more windows");
            return;
        };
        t.window = Some(id);
        // The program gets this window's focus for /dev/input and controllers.
        if let Some(pid) = sys::peer_pid(c.fd) {
            sys::win_set_program(id, pid);
        }
        let surface = sys::win_surface(id);
        let size =
            (surface.width as i32 / surface.scale.max(1) as i32, surface.height as i32 / surface.scale.max(1) as i32);
        let fullscreen = t.fullscreen;
        self.windows.insert(
            id,
            Window {
                client: cid,
                toplevel: top,
                size,
                surface,
                focused: false,
                dirty: true,
                damage: None,
                attached: Vec::new(),
                layers: 0,
                mode: String::new(),
                showing: false,
                direct_frames: 0,
                composed_frames: 0,
            },
        );
        if fullscreen {
            sys::win_set_fullscreen(id, true);
        }
        if size != (w, h) {
            self.configure_toplevel(cid, top, size);
        }
    }

    fn close_window(&mut self, cid: ClientId, top: Id) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(id) = c.toplevels.get_mut(&top).and_then(|t| t.window.take()) else { return };
        let w = self.windows.remove(&id);
        self.presented_at.remove(&id);
        self.committed_at.remove(&id);
        sys::win_close(id);
        // What it showed as it is goes back to the program (WaveOS tells
        // a closed window nothing).
        for k in w.map(|w| w.attached).unwrap_or_default() {
            if let Some(&(owner, bid)) = self.kernel_buffers.get(&k) {
                if let Some(c) = self.clients.get_mut(&owner).filter(|c| c.buffers.contains_key(&bid)) {
                    c.send(bid, wl_buffer::ev::RELEASE, &[]);
                }
            }
        }
        if self.pointer.is_some_and(|p| p.2 == id) {
            self.pointer = None;
        }
    }

    /// After a commit: a program that drew another size than its window's
    /// (on its own, or late) is asked for the window's size again, once for
    /// each new size it draws.
    fn check_size(&mut self, cid: ClientId, top: Id) {
        let size = self.content_size(cid, top);
        let Some(t) = self.clients.get_mut(&cid).and_then(|c| c.toplevels.get_mut(&top)) else { return };
        let drawn = std::mem::replace(&mut t.drawn, size);
        let Some(want) = t.window.and_then(|w| self.windows.get(&w)).map(|w| w.size) else { return };
        if size != want && size != drawn {
            self.configure_toplevel(cid, top, want);
        }
    }

    // ------------------------------------------------------------ drawing

    /// What a window shows, as layers WaveOS can show as they are — every
    /// surface with content (its main surface, sub-surfaces, pop-ups),
    /// bottom first, each buffer not copied and in a format WaveOS takes —
    /// or `None` when one isn't so (the bridge composes the window then).
    fn layers(&self, wid: u32) -> Result<Vec<Layer>, &'static str> {
        let gone = "the window is gone";
        let w = self.windows.get(&wid).ok_or(gone)?;
        let c = self.clients.get(&w.client).ok_or(gone)?;
        let t = c.toplevels.get(&w.toplevel).ok_or(gone)?;
        let root = c.xdg.get(&t.xdg).ok_or(gone)?.surface;
        let k = w.surface.scale.max(1) as i32;
        let g = c.geometry(t.xdg);
        let mut out = Vec::new();
        collect_layers(c, root, (-g.0, -g.1), k, &mut out)?;
        for (_, p) in &c.popups {
            if c.toplevel_of(p.xdg) != Some(w.toplevel) {
                continue;
            }
            let (ox, oy) = c.origin(p.xdg);
            if let Some(px) = c.xdg.get(&p.xdg) {
                collect_layers(c, px.surface, (ox - g.0, oy - g.1), k, &mut out)?;
            }
        }
        match out.len() {
            0 => Err("nothing to show"),
            n if n > sys::MAX_LAYERS => Err("too many surfaces"),
            _ => Ok(out),
        }
    }

    /// Shows a window's surfaces as they are: their buffers attached to
    /// the WaveOS window — one alone when it is all the window shows (a
    /// display may then scan it out), else as layers. False if it can't be.
    fn attach(&mut self, wid: u32) -> bool {
        let layers = match self.layers(wid) {
            Ok(l) => l,
            Err(why) => {
                self.note_mode(wid, why);
                return false;
            }
        };
        let w = self.windows.get_mut(&wid).unwrap();
        let cid = w.client;
        let c = self.clients.get_mut(&cid).unwrap();
        let first = &layers[0];
        let s = c.surfaces.get(&first.surface).unwrap();
        let whole = Rect::new(0, 0, s.bw, s.bh);
        let alone = layers.len() == 1 && first.src == whole && (first.dst.x, first.dst.y) == (0, 0) && s.identity_viewport();
        // What changed of each layer: its damage, made tight by comparing
        // with the buffer WaveOS still shows of it (browsers say "all of it"
        // for a caret, or a scrolled strip): only that is composed again.
        let changed: Vec<Option<Rect>> = layers.iter().map(|l| tight_damage(c, l)).collect();
        let done = if alone {
            // Damage against what the window showed last, whichever buffer
            // that was; all of it when the window showed its own surface.
            let damage = if w.attached.is_empty() { whole } else { changed[0].unwrap_or_default() };
            sys::win_attach(wid, first.kernel, s.scale, damage.intersect(whole))
        } else {
            let window = Rect::new(0, 0, w.surface.width as i32, w.surface.height as i32);
            // Damage in window pixels: every layer's change where it is shown
            // (all of it after a change of layout).
            let mut damage = None;
            for (l, r) in layers.iter().zip(&changed) {
                if let Some(r) = r {
                    let sx = |v: i32| l.dst.x + ((v - l.src.x) as i64 * l.dst.w as i64 / l.src.w.max(1) as i64) as i32;
                    let sy = |v: i32| l.dst.y + ((v - l.src.y) as i64 * l.dst.h as i64 / l.src.h.max(1) as i64) as i32;
                    let (x0, y0) = (sx(r.x), sy(r.y));
                    damage::add(&mut damage, Rect::new(x0, y0, sx(r.right()) - x0 + 1, sy(r.bottom()) - y0 + 1));
                }
            }
            let damage = if w.attached.is_empty() || w.damage.is_none() { window } else { damage.unwrap_or_default() };
            let list: Vec<sys::WinLayer> = layers
                .iter()
                .map(|l| sys::WinLayer {
                    buffer: l.kernel,
                    flags: 0,
                    src: [l.src.x, l.src.y, l.src.w, l.src.h],
                    dst: [l.dst.x, l.dst.y, l.dst.w, l.dst.h],
                })
                .collect();
            sys::win_attach_layers(wid, &list, w.surface.scale as i32, damage.intersect(window))
        };
        if !done {
            self.note_mode(wid, "WaveOS refused the buffers");
            return false;
        }
        let kinds: Vec<String> = {
            let c = &self.clients[&cid];
            layers
                .iter()
                .map(|l| {
                    let b = c.buffers.get(&l.buffer);
                    let dma = b.is_some_and(|b| matches!(b.memory, buffers::Memory::Dma { .. }));
                    let format = b.map_or("", |b| if b.opaque() { "XRGB" } else { "ARGB" });
                    let scaled = if (l.src.w, l.src.h) != (l.dst.w, l.dst.h) { " scaled" } else { "" };
                    format!("{} {}x{} {format}{scaled}", if dma { "dma-buf" } else { "shm" }, l.src.w, l.src.h)
                })
                .collect()
        };
        let mode = if alone {
            format!("shown as it is ({})", kinds[0])
        } else {
            format!("{} layers: {}", kinds.len(), kinds.join(", "))
        };
        self.note_mode(wid, &mode);
        let w = self.windows.get_mut(&wid).unwrap();
        let c = self.clients.get_mut(&cid).unwrap();
        // The buffers it showed before come back from WaveOS (`BUFFER_RELEASE`).
        w.attached = layers.iter().map(|l| l.kernel).collect();
        w.damage = None;
        w.showing = true;
        w.direct_frames += 1;
        w.layers = layers.len();
        for l in &layers {
            if let Some(s) = c.surfaces.get_mut(&l.surface) {
                s.direct_damage = None;
                s.on_screen = Some(l.buffer);
                s.pixels_current = false;
            }
            self.kernel_buffers.insert(l.kernel, (cid, l.buffer));
        }
        true
    }

    /// Logs how a window reaches the screen when that changes (as it is, as
    /// layers, or composed by the bridge and why).
    fn note_mode(&mut self, wid: u32, mode: &str) {
        let Some(w) = self.windows.get_mut(&wid) else { return };
        if w.mode == mode {
            return;
        }
        w.mode = String::from(mode);
        let title = self.clients.get(&w.client).and_then(|c| c.toplevels.get(&w.toplevel)).map(|t| t.title.clone());
        let composed = if mode.starts_with("shown") || mode.contains("layers:") { "" } else { "composed by the bridge: " };
        sys::log(&format!("window {wid} ({}): {composed}{mode}", title.unwrap_or_default()));
    }

    /// Draws a window's surfaces into its WaveOS window.
    fn compose(&mut self, wid: u32) {
        let Some(w) = self.windows.get_mut(&wid) else { return };
        w.dirty = false;
        let started = sys::monotonic_ns();
        if self.attach(wid) {
            self.presented(wid, started, true);
            return;
        }
        self.take_back(wid);
        let Some(w) = self.windows.get_mut(&wid) else { return };
        let Some(c) = self.clients.get(&w.client) else { return };
        let Some(t) = c.toplevels.get(&w.toplevel) else { return };
        let Some(x) = c.xdg.get(&t.xdg) else { return };
        let surf = w.surface;
        if surf.addr == 0 {
            return;
        }
        let px = unsafe { std::slice::from_raw_parts_mut(surf.addr as *mut u32, (surf.stride * surf.height) as usize) };
        let bounds = Rect::new(0, 0, surf.width as i32, surf.height as i32);
        let clip = w.damage.take().unwrap_or(bounds).intersect(bounds);
        if clip.empty() {
            return;
        }
        let target = Target {
            clip,
            px,
            width: surf.width as i32,
            height: surf.height as i32,
            stride: surf.stride as i32,
            scale: surf.scale.max(1) as i32,
        };
        for y in clip.y..clip.bottom() {
            let start = y as usize * target.stride as usize + clip.x as usize;
            target.px[start..start + clip.w as usize].fill(0xffff_ffff);
        }
        let mut target = target;
        let g = c.geometry(t.xdg);
        draw_tree(c, &mut target, x.surface, -g.0, -g.1);
        for (pid, p) in &c.popups {
            if c.toplevel_of(p.xdg) != Some(w.toplevel) {
                continue;
            }
            let _ = pid;
            let (ox, oy) = c.origin(p.xdg);
            if let Some(px) = c.xdg.get(&p.xdg) {
                draw_tree(c, &mut target, px.surface, ox - g.0, oy - g.1);
            }
        }
        sys::win_present(wid, clip);
        if let Some(w) = self.windows.get_mut(&wid) {
            w.showing = true;
            w.composed_frames += 1;
        }
        self.presented(wid, started, false);
    }

    /// A window that showed a buffer as it is composes again: the surface
    /// that wasn't copied is copied now, and the WaveOS window shows its
    /// own surface (the buffer comes back with `BUFFER_RELEASE`).
    fn take_back(&mut self, wid: u32) {
        let Some(w) = self.windows.get(&wid) else { return };
        let (cid, top, attached) = (w.client, w.toplevel, w.attached.clone());
        let Some(c) = self.clients.get_mut(&cid) else { return };
        // Every surface of the window: its tree and its pop-ups'.
        let mut tree = Vec::new();
        if let Some(sid) = c.toplevels.get(&top).and_then(|t| c.xdg.get(&t.xdg)).map(|x| x.surface) {
            tree_of(c, sid, &mut tree);
        }
        for (_, p) in &c.popups {
            if c.toplevel_of(p.xdg) == Some(top) {
                if let Some(px) = c.xdg.get(&p.xdg) {
                    tree_of(c, px.surface, &mut tree);
                }
            }
        }
        let mut release = Vec::new();
        for sid in tree {
            let Some(s) = c.surfaces.get_mut(&sid).filter(|s| s.direct) else { continue };
            s.direct = false;
            let region = if s.pixels_current { s.direct_damage } else { None };
            s.direct_damage = None;
            s.on_screen = None;
            if let Some(b) = s.attached.and_then(|bid| c.buffers.get(&bid)) {
                copy_buffer(b, s, region);
                s.pixels_current = true;
                // Never given to WaveOS: it goes back now.
                if b.kernel.get().is_none_or(|k| !attached.contains(&k)) {
                    release.extend(s.attached);
                }
            }
        }
        for bid in release {
            c.send(bid, wl_buffer::ev::RELEASE, &[]);
        }
        if !attached.is_empty() {
            sys::win_attach(wid, 0, 1, Rect::new(0, 0, 0, 0));
            let w = self.windows.get_mut(&wid).unwrap();
            w.attached.clear();
            w.layers = 0;
            w.damage = None;
        }
    }

    /// A window's picture went to WaveOS (`started`: when the bridge began).
    fn presented(&mut self, wid: u32, started: u64, direct: bool) {
        let now = sys::monotonic_ns();
        let started = self.committed_at.remove(&wid).unwrap_or(started).min(started);
        self.stats.frames += 1;
        self.stats.direct += direct as u64;
        self.stats.handoff_ns += now - started;
        self.presented_at.insert(wid, now);
    }

    /// Answers frame callbacks and presentation feedback; `shown` is the
    /// `FRAME` event that says when the picture was on screen.
    fn answer(&mut self, frames: Vec<Frame>, shown: Option<&sys::Event>) {
        if frames.is_empty() {
            return;
        }
        let ms = sys::now_ms();
        let (time, refresh, seq, flags) = match shown {
            Some(e) => (e.c as u64 | (e.d as u64) << 32, e.b, e.a, e.x as u32),
            None => (sys::monotonic_ns(), 16_666_667, 0, 0),
        };
        for f in frames {
            let Some(c) = self.clients.get_mut(&f.client) else { continue };
            if c.objects.remove(&f.id).is_none() {
                continue;
            }
            if !f.feedback {
                c.send(f.id, wl_callback::ev::DONE, &[Out::U(ms)]);
            } else if shown.is_some() {
                for o in c.all(wl_output::NAME) {
                    c.send(f.id, wp_presentation_feedback::ev::SYNC_OUTPUT, &[Out::O(o)]);
                }
                let (sec, nsec) = (time / 1_000_000_000, (time % 1_000_000_000) as u32);
                c.send(
                    f.id,
                    wp_presentation_feedback::ev::PRESENTED,
                    &[
                        Out::U((sec >> 32) as u32),
                        Out::U(sec as u32),
                        Out::U(nsec),
                        Out::U(refresh),
                        Out::U(0),
                        Out::U(seq),
                        // vsync, hw clock, hw completion, zero copy: as WaveOS says.
                        Out::U(flags & 0xF),
                    ],
                );
            } else {
                c.send(f.id, wp_presentation_feedback::ev::DISCARDED, &[]);
            }
            c.send(1, wl_display::ev::DELETE_ID, &[Out::U(f.id)]);
        }
        self.answered_at = sys::monotonic_ns();
    }

    /// Every 10 seconds of frames (and when a program leaves): where they
    /// went, in the system log.
    fn log_stats(&mut self, now_too: bool) {
        let now = sys::now_ms();
        if self.stats.since == 0 {
            self.stats.since = now;
        }
        if now.wrapping_sub(self.stats.since) < 10_000 && !now_too {
            return;
        }
        let s = std::mem::take(&mut self.stats);
        self.stats.since = now;
        if s.frames == 0 {
            return;
        }
        let per = |ns: u64, n: u64| ns as f64 / n.max(1) as f64 / 1e6;
        let ms = now.wrapping_sub(s.since).max(1);
        sys::log(&format!(
            "{} frames in {} ms ({:.1} per second), {} shown as they are; per frame: hand-off {:.3} ms, \
             until shown {:.2} ms, client {:.2} ms after its frame callback ({} frames)",
            s.frames,
            ms,
            s.frames as f64 * 1000.0 / ms as f64,
            s.direct,
            per(s.handoff_ns, s.frames),
            per(s.shown_ns, s.shown),
            per(s.client_ns, s.client),
            s.client,
        ));
    }

    // ------------------------------------------------------ WaveOS events

    fn wave_event(&mut self, e: sys::Event) {
        use sys::event::*;
        let wid = e.window;
        if e.kind == BUFFER_RELEASE {
            // WaveOS no longer reads it: the program may draw into it again.
            if let Some((cid, bid)) = self.kernel_buffers.get(&e.a).copied() {
                if let Some(c) = self.clients.get_mut(&cid).filter(|c| c.buffers.contains_key(&bid)) {
                    c.send(bid, wl_buffer::ev::RELEASE, &[]);
                }
            }
            return;
        }
        let Some(w) = self.windows.get(&wid) else { return };
        let (cid, top) = (w.client, w.toplevel);
        match e.kind {
            FRAME => {
                if let Some(w) = self.windows.get_mut(&wid) {
                    w.showing = false;
                }
                if let Some(at) = self.presented_at.remove(&wid) {
                    self.stats.shown_ns += sys::monotonic_ns().saturating_sub(at);
                    self.stats.shown += 1;
                }
                let (go, keep): (Vec<Frame>, Vec<Frame>) =
                    std::mem::take(&mut self.frames).into_iter().partition(|f| f.window == Some(wid));
                self.frames = keep;
                self.answer(go, Some(&e));
            }
            FULLSCREEN => {
                let on = e.a != 0;
                if let Some(t) = self.clients.get_mut(&cid).and_then(|c| c.toplevels.get_mut(&top)) {
                    t.fullscreen = on;
                }
                let size = self.windows.get(&wid).map_or((0, 0), |w| w.size);
                self.configure_toplevel(cid, top, size);
            }
            KEY => self.key(cid, &e),
            POINTER_MOVE => {
                if e.x < 0 && e.y < 0 && e.a == 0 {
                    self.pointer_leave();
                } else {
                    self.pointer_motion(wid, e.x, e.y);
                }
            }
            POINTER_DOWN => {
                self.pointer_motion(wid, e.x, e.y);
                if self.dismiss_popups(wid, e.x, e.y) {
                    return;
                }
                if e.a == 2 {
                    self.button(BTN_RIGHT, true);
                    self.button(BTN_RIGHT, false);
                } else {
                    self.button_down = true;
                    self.button(BTN_LEFT, true);
                }
            }
            POINTER_UP => {
                self.pointer_motion(wid, e.x, e.y);
                self.button_down = false;
                self.button(BTN_LEFT, false);
            }
            SCROLL => self.scroll(e.x, e.y),
            RESIZE => {
                let size = (e.x, e.y);
                if e.a > 0 && e.a as i32 != self.scale {
                    self.scale = e.a as i32;
                    self.output_changed();
                }
                sys::win_resize(wid, size.0.max(1) as u32, size.1.max(1) as u32);
                let surface = sys::win_surface(wid);
                if let Some(w) = self.windows.get_mut(&wid) {
                    w.size = size;
                    w.surface = surface;
                    w.dirty = true;
                    w.damage = None;
                }
                self.configure_toplevel(cid, top, size);
            }
            CLOSE_REQUESTED => {
                if e.a == 1 {
                    // Ctrl+W: the program decides (a browser closes a tab).
                    let ctrl = 2;
                    for (pressed, code) in [(true, 29), (true, 17), (false, 17), (false, 29)] {
                        let bits = if pressed || code == 17 { self.mods | ctrl } else { self.mods & !ctrl };
                        self.send_key(cid, code, pressed, bits);
                    }
                } else if let Some(c) = self.clients.get_mut(&cid) {
                    c.send(top, xdg_toplevel::ev::CLOSE, &[]);
                }
            }
            FOCUS => {
                let focused = e.a != 0;
                if let Some(w) = self.windows.get_mut(&wid) {
                    if w.focused == focused {
                        return;
                    }
                    w.focused = focused;
                }
                if focused {
                    self.keyboard_enter(cid, top, None);
                } else {
                    self.keyboard_leave(cid, top);
                }
                let size = self.windows.get(&wid).map_or((0, 0), |w| w.size);
                self.configure_toplevel(cid, top, size);
            }
            _ => {}
        }
    }

    fn output_changed(&mut self) {
        let (screen, scale) = (self.screen, self.scale);
        for c in self.clients.values_mut() {
            for o in c.all(wl_output::NAME) {
                let v = c.version(o);
                Self::output_info(c, o, v, screen, scale);
            }
        }
    }

    /// The surface at a point of window `wid` (points), and the point in
    /// its coordinates.
    fn surface_at(&self, wid: u32, x: i32, y: i32) -> Option<(ClientId, Id, f64, f64)> {
        let w = self.windows.get(&wid)?;
        let c = self.clients.get(&w.client)?;
        let t = c.toplevels.get(&w.toplevel)?;
        let g = c.geometry(t.xdg);
        let (rx, ry) = (x + g.0, y + g.1);
        for (_, p) in c.popups.iter().rev() {
            if c.toplevel_of(p.xdg) != Some(w.toplevel) {
                continue;
            }
            let (ox, oy) = c.origin(p.xdg);
            let Some(sid) = c.xdg.get(&p.xdg).map(|x| x.surface) else { continue };
            if let Some(hit) = hit_tree(c, sid, rx - ox, ry - oy) {
                return Some((w.client, hit.0, hit.1 as f64, hit.2 as f64));
            }
        }
        let root = c.xdg.get(&t.xdg)?.surface;
        let hit = hit_tree(c, root, rx, ry).unwrap_or((root, rx, ry));
        Some((w.client, hit.0, hit.1 as f64, hit.2 as f64))
    }

    fn pointers(&self, cid: ClientId) -> Vec<(Id, u32)> {
        let Some(c) = self.clients.get(&cid) else { return Vec::new() };
        c.all(wl_pointer::NAME).into_iter().map(|p| (p, c.version(p))).collect()
    }

    fn pointer_frame(&mut self, cid: ClientId) {
        for (p, v) in self.pointers(cid) {
            if v >= 5 {
                self.clients.get_mut(&cid).unwrap().send(p, wl_pointer::ev::FRAME, &[]);
            }
        }
    }

    fn pointer_leave(&mut self) {
        if self.button_down {
            return;
        }
        let Some((cid, sid, _)) = self.pointer.take() else { return };
        let serial = self.next_serial();
        for (p, _) in self.pointers(cid) {
            self.clients.get_mut(&cid).unwrap().send(p, wl_pointer::ev::LEAVE, &[Out::U(serial), Out::O(sid)]);
        }
        self.pointer_frame(cid);
    }

    fn pointer_motion(&mut self, wid: u32, x: i32, y: i32) {
        // While a button is held, the surface pressed keeps the pointer.
        let hit = if self.button_down && self.pointer.is_some_and(|p| p.2 == wid) {
            let (cid, sid, _) = self.pointer.unwrap();
            let (hx, hy) = self.surface_origin(wid, sid);
            Some((cid, sid, (x - hx) as f64, (y - hy) as f64))
        } else {
            self.surface_at(wid, x, y)
        };
        let Some((cid, sid, sx, sy)) = hit else { return };
        let time = sys::now_ms();
        if self.pointer.map(|p| (p.0, p.1)) != Some((cid, sid)) {
            self.button_down = false;
            self.pointer_leave();
            let serial = self.next_serial();
            for (p, _) in self.pointers(cid) {
                self.clients.get_mut(&cid).unwrap().send(
                    p,
                    wl_pointer::ev::ENTER,
                    &[Out::U(serial), Out::O(sid), Out::F(sx), Out::F(sy)],
                );
            }
            self.pointer = Some((cid, sid, wid));
        } else if (sx, sy) != self.pointer_at {
            for (p, _) in self.pointers(cid) {
                self.clients.get_mut(&cid).unwrap().send(
                    p,
                    wl_pointer::ev::MOTION,
                    &[Out::U(time), Out::F(sx), Out::F(sy)],
                );
            }
        }
        self.pointer_at = (sx, sy);
        self.pointer_frame(cid);
    }

    /// Where surface `sid` sits in window `wid`, in points.
    fn surface_origin(&self, wid: u32, sid: Id) -> (i32, i32) {
        let Some(w) = self.windows.get(&wid) else { return (0, 0) };
        let Some(c) = self.clients.get(&w.client) else { return (0, 0) };
        let g = c.toplevels.get(&w.toplevel).map_or((0, 0, 0, 0), |t| c.geometry(t.xdg));
        let mut at = (-g.0, -g.1);
        let mut s = sid;
        for _ in 0..64 {
            let Some(surface) = c.surfaces.get(&s) else { break };
            match surface.role {
                Role::Sub(parent) => {
                    at.0 += surface.pos.0;
                    at.1 += surface.pos.1;
                    s = parent;
                }
                Role::Xdg(x) => {
                    let o = c.origin(x);
                    at.0 += o.0;
                    at.1 += o.1;
                    break;
                }
                _ => break,
            }
        }
        at
    }

    fn button(&mut self, button: u32, pressed: bool) {
        let Some((cid, _, _)) = self.pointer else { return };
        let serial = self.next_serial();
        let time = sys::now_ms();
        for (p, _) in self.pointers(cid) {
            self.clients.get_mut(&cid).unwrap().send(
                p,
                wl_pointer::ev::BUTTON,
                &[Out::U(serial), Out::U(time), Out::U(button), Out::U(pressed as u32)],
            );
        }
        self.pointer_frame(cid);
    }

    /// A click outside a window's pop-ups closes them; true if it did.
    fn dismiss_popups(&mut self, wid: u32, x: i32, y: i32) -> bool {
        let Some(w) = self.windows.get(&wid) else { return false };
        let (cid, top) = (w.client, w.toplevel);
        let Some(c) = self.clients.get(&cid) else { return false };
        let open: Vec<Id> =
            c.popups.iter().filter(|(_, p)| c.toplevel_of(p.xdg) == Some(top)).map(|(id, _)| *id).collect();
        if open.is_empty() {
            return false;
        }
        let on_popup = self.surface_at(wid, x, y).is_some_and(|(_, sid, _, _)| {
            c.xdg_of(sid).is_some_and(|x| matches!(c.xdg.get(&x).map(|x| x.role), Some(XdgRole::Popup(_))))
        });
        if on_popup {
            return false;
        }
        let c = self.clients.get_mut(&cid).unwrap();
        for id in open.iter().rev() {
            c.send(*id, xdg_popup::ev::POPUP_DONE, &[]);
        }
        true
    }

    fn scroll(&mut self, dx: i32, dy: i32) {
        let Some((cid, _, _)) = self.pointer else { return };
        let time = sys::now_ms();
        for (p, v) in self.pointers(cid) {
            let c = self.clients.get_mut(&cid).unwrap();
            if v >= 5 {
                c.send(p, wl_pointer::ev::AXIS_SOURCE, &[Out::U(0)]);
            }
            for (axis, d) in [(0u32, dy), (1, dx)] {
                if d == 0 {
                    continue;
                }
                if v >= 5 && d % 120 == 0 {
                    c.send(p, wl_pointer::ev::AXIS_DISCRETE, &[Out::U(axis), Out::I(d / 120)]);
                }
                // 10 pixels a notch, like other compositors.
                c.send(p, wl_pointer::ev::AXIS, &[Out::U(time), Out::U(axis), Out::F(d as f64 / 12.0)]);
            }
        }
        self.pointer_frame(cid);
    }

    fn keyboards(&self, cid: ClientId) -> Vec<Id> {
        self.clients.get(&cid).map(|c| c.all(wl_keyboard::NAME)).unwrap_or_default()
    }

    fn keyboard_enter(&mut self, cid: ClientId, top: Id, only: Option<Id>) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(sid) = c.toplevels.get(&top).and_then(|t| c.xdg.get(&t.xdg)).map(|x| x.surface) else { return };
        let serial = self.next_serial();
        let (dep, locked) = keys::modifiers(self.mods);
        let kbs: Vec<Id> = only.map_or_else(|| self.keyboards(cid), |k| vec![k]);
        let c = self.clients.get_mut(&cid).unwrap();
        for k in kbs {
            c.send(k, wl_keyboard::ev::ENTER, &[Out::U(serial), Out::O(sid), Out::A(&[])]);
            c.send(k, wl_keyboard::ev::MODIFIERS, &[Out::U(serial), Out::U(dep), Out::U(0), Out::U(locked), Out::U(0)]);
        }
        self.offer_selection(cid);
    }

    fn keyboard_leave(&mut self, cid: ClientId, top: Id) {
        let Some(c) = self.clients.get(&cid) else { return };
        let Some(sid) = c.toplevels.get(&top).and_then(|t| c.xdg.get(&t.xdg)).map(|x| x.surface) else { return };
        let serial = self.next_serial();
        for k in self.keyboards(cid) {
            self.clients.get_mut(&cid).unwrap().send(k, wl_keyboard::ev::LEAVE, &[Out::U(serial), Out::O(sid)]);
        }
    }

    fn key(&mut self, cid: ClientId, e: &sys::Event) {
        // The native ABI carries translated text, not a physical identity, for
        // Char keys. Their releases have no character and cannot be matched.
        // Deliver balanced text keystrokes; native repeat supplies repetition.
        if e.a == 0 && e.c == 0 {
            return;
        }
        let Some(code) = keys::evdev(e.a, e.b) else { return };
        self.send_key(cid, code, e.c != 0, e.d);
        if e.a == 0 {
            self.send_key(cid, code, false, e.d);
        }
    }

    fn send_key(&mut self, cid: ClientId, code: u32, pressed: bool, mods: u32) {
        let time = sys::now_ms();
        if mods != self.mods {
            self.mods = mods;
            let serial = self.next_serial();
            let (dep, locked) = keys::modifiers(mods);
            for k in self.keyboards(cid) {
                self.clients.get_mut(&cid).unwrap().send(
                    k,
                    wl_keyboard::ev::MODIFIERS,
                    &[Out::U(serial), Out::U(dep), Out::U(0), Out::U(locked), Out::U(0)],
                );
            }
        }
        let serial = self.next_serial();
        for k in self.keyboards(cid) {
            self.clients.get_mut(&cid).unwrap().send(
                k,
                wl_keyboard::ev::KEY,
                &[Out::U(serial), Out::U(time), Out::U(code), Out::U(pressed as u32)],
            );
        }
    }

    // ---------------------------------------------------------- clipboard

    /// A program copied: its text goes to WaveOS's clipboard.
    fn set_selection(&mut self, cid: ClientId, source: Id) {
        if let Some((old_c, old)) = self.selection.take() {
            if (old_c, old) != (cid, source) {
                if let Some(c) = self.clients.get_mut(&old_c) {
                    c.send(old, wl_data_source::ev::CANCELLED, &[]);
                }
            }
        }
        if source == 0 {
            return;
        }
        self.selection = Some((cid, source));
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(mimes) = c.data_sources.get(&source) else { return };
        let Some(mime) = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "TEXT", "STRING"]
            .iter()
            .find(|m| mimes.iter().any(|x| x == *m))
        else {
            return;
        };
        let Some((r, w)) = sys::pipe() else { return };
        c.send(source, wl_data_source::ev::SEND, &[Out::S(mime), Out::H(w)]);
        c.close_after.push(w);
        self.clip_in.push((r, Vec::new()));
    }

    /// Offers WaveOS's clipboard to a program as its selection.
    fn offer_selection(&mut self, cid: ClientId) {
        let text = sys::clipboard_text();
        let Some(c) = self.clients.get_mut(&cid) else { return };
        for dev in c.all(wl_data_device::NAME) {
            if text.is_none() {
                c.send(dev, wl_data_device::ev::SELECTION, &[Out::O(0)]);
                continue;
            }
            let offer = c.next_server_id;
            c.next_server_id += 1;
            let v = c.version(dev);
            c.objects.insert(offer, (wl_data_offer::NAME, v));
            c.send(dev, wl_data_device::ev::DATA_OFFER, &[Out::N(offer)]);
            for m in ["text/plain;charset=utf-8", "text/plain", "UTF8_STRING", "TEXT", "STRING"] {
                c.send(offer, wl_data_offer::ev::OFFER, &[Out::S(m)]);
            }
            c.send(dev, wl_data_device::ev::SELECTION, &[Out::O(offer)]);
        }
    }

    fn clipboard_io(&mut self, ready: &[sys::PollFd]) {
        let n_in = self.clip_in.len();
        let mut done_in = Vec::new();
        for (i, (fd, data)) in self.clip_in.iter_mut().enumerate() {
            if ready.get(i).is_none_or(|p| p.revents == 0) {
                continue;
            }
            let mut buf = [0u8; 16384];
            loop {
                let n = sys::read(*fd, &mut buf);
                if n > 0 {
                    data.extend_from_slice(&buf[..n as usize]);
                } else {
                    if n != -11 {
                        done_in.push(i);
                    }
                    break;
                }
            }
        }
        for i in done_in.into_iter().rev() {
            let (fd, data) = self.clip_in.remove(i);
            sys::close(fd);
            sys::set_clipboard_text(&data);
        }
        let mut done_out = Vec::new();
        for (i, (fd, data)) in self.clip_out.iter_mut().enumerate() {
            if ready.get(n_in + i).is_none_or(|p| p.revents == 0) {
                continue;
            }
            let n = sys::write(*fd, data);
            if n > 0 {
                data.drain(..n as usize);
            }
            if data.is_empty() || (n < 0 && n != -11) {
                done_out.push(i);
            }
        }
        for i in done_out.into_iter().rev() {
            let (fd, _) = self.clip_out.remove(i);
            sys::close(fd);
        }
    }
}

// ------------------------------------------------------------------ pixels

struct Target<'a> {
    clip: Rect,
    px: &'a mut [u32],
    width: i32,
    height: i32,
    stride: i32,
    scale: i32,
}

/// Copies a committed buffer into the surface.
fn copy_buffer(b: &Buffer, s: &mut Surface, requested: Option<Rect>) -> Option<Rect> {
    let (w, h, stride) = (b.width.max(0) as usize, b.height.max(0) as usize, b.stride.max(0) as usize);
    if w == 0 || h == 0 || stride < w * 4 {
        return None;
    }
    let need = stride.checked_mul(h - 1)?.checked_add(w.checked_mul(4)?)?;
    let (base, size) = b.bytes()?;
    if need > size {
        return None;
    }
    let bounds = Rect::new(0, 0, w as i32, h as i32);
    let same = s.bw == w as i32 && s.bh == h as i32 && s.opaque == b.opaque() && s.has_content;
    let clip = if same { requested.unwrap_or(bounds).intersect(bounds) } else { bounds };
    s.pixels.resize(w * h, 0);
    let mut actual = if same { Rect::default() } else { bounds };
    let mask = if b.opaque() { 0x00ff_ffff } else { u32::MAX };
    for y in clip.y as usize..clip.bottom() as usize {
        let row = unsafe { std::slice::from_raw_parts(base.add(y * stride) as *const u32, w) };
        let start = y * w + clip.x as usize;
        let source = &row[clip.x as usize..clip.right() as usize];
        let destination = &mut s.pixels[start..start + clip.w as usize];
        let (left, right) = if same {
            // GTK/WebKit may declare full damage for a caret or an identical
            // buffer swap. Equal rows use the optimized slice comparison;
            // only changed rows need a precise scan. Ignore unused XRGB bits.
            if source == destination {
                continue;
            }
            let differs = |(a, b): (&u32, &u32)| (a ^ b) & mask != 0;
            let Some(left) = source.iter().zip(destination.iter()).position(differs) else { continue };
            let right = source.iter().zip(destination.iter()).rposition(differs).unwrap() + 1;
            actual = actual.union(Rect::new(clip.x + left as i32, y as i32, (right - left) as i32, 1));
            (left, right)
        } else {
            (0, source.len())
        };
        destination[left..right].copy_from_slice(&source[left..right]);
    }
    s.bw = w as i32;
    s.bh = h as i32;
    s.opaque = b.opaque();
    Some(actual)
}

/// A surface shown as it is: `src` of its buffer (buffer pixels) at `dst`
/// (window pixels).
struct Layer {
    surface: Id,
    buffer: Id,
    kernel: u32,
    src: Rect,
    dst: Rect,
}

/// Adds the layers of surface `sid` and its sub-surfaces, at `at` points
/// in the window, to `out`; `None` if one can't be shown as it is.
fn collect_layers(c: &Client, sid: Id, at: (i32, i32), k: i32, out: &mut Vec<Layer>) -> Result<(), &'static str> {
    let Some(s) = c.surfaces.get(&sid) else { return Ok(()) };
    if s.has_content {
        if !s.direct {
            return Err("a surface was copied (committed before its window showed, or an unusual format)");
        }
        let bid = s.attached.ok_or("a surface lost its buffer")?;
        let b = c.buffers.get(&bid).ok_or("a buffer was destroyed")?;
        let kernel = b.kernel_id().ok_or("WaveOS can't show a buffer")?;
        let (lw, lh) = s.size();
        let src = match s.src {
            // The viewport's source rectangle, in buffer pixels.
            Some((x, y, w, h)) => {
                let sc = s.scale as f64;
                let r = Rect::new((x * sc) as i32, (y * sc) as i32, ((w * sc) as i32).max(1), ((h * sc) as i32).max(1));
                r.intersect(Rect::new(0, 0, s.bw, s.bh))
            }
            None => Rect::new(0, 0, s.bw, s.bh),
        };
        if src.empty() || lw <= 0 || lh <= 0 {
            return Err("an empty viewport");
        }
        out.push(Layer { surface: sid, buffer: bid, kernel, src, dst: Rect::new(at.0 * k, at.1 * k, lw * k, lh * k) });
    }
    for child in &s.children {
        if let Some(cs) = c.surfaces.get(child) {
            collect_layers(c, *child, (at.0 + cs.pos.0, at.1 + cs.pos.1), k, out)?;
        }
    }
    Ok(())
}

/// What changed of layer `l` (buffer pixels; `None`: nothing): the
/// surface's damage, made tight by comparing its new buffer with the one
/// WaveOS still shows of it, row by row (it is held until WaveOS releases
/// it, so it can't change meanwhile). Without such a buffer, the damage as
/// the program gave it.
fn tight_damage(c: &Client, l: &Layer) -> Option<Rect> {
    let s = c.surfaces.get(&l.surface)?;
    let whole = Rect::new(0, 0, s.bw, s.bh);
    let damage = s.direct_damage?.intersect(whole);
    let (Some(old), Some(new)) = (s.on_screen.filter(|&b| b != l.buffer), Some(l.buffer)) else { return Some(damage) };
    let (Some(old), Some(new)) = (c.buffers.get(&old), c.buffers.get(&new)) else { return Some(damage) };
    if (old.width, old.height, old.stride, old.format) != (new.width, new.height, new.stride, new.format) {
        return Some(damage);
    }
    let (Some((a, asize)), Some((b, bsize))) = (old.bytes(), new.bytes()) else { return Some(damage) };
    let stride = new.stride as usize;
    if damage.empty() || (damage.bottom() as usize).saturating_sub(1) * stride + damage.right() as usize * 4 > asize.min(bsize) {
        return Some(damage);
    }
    let mask = if new.opaque() { 0x00ff_ffff } else { u32::MAX };
    let row = |y: i32| {
        let at = y as usize * stride + damage.x as usize * 4;
        unsafe {
            (
                std::slice::from_raw_parts(a.add(at) as *const u32, damage.w as usize),
                std::slice::from_raw_parts(b.add(at) as *const u32, damage.w as usize),
            )
        }
    };
    // Comparing costs as much as a copy: first a few rows spread over the
    // damage. When most of them changed (a scroll, a new page) the damage
    // stands as given; only small changes are worth narrowing down.
    if damage.h >= 64 {
        let samples = 24;
        let changed = (0..samples)
            .filter(|i| {
                let (ra, rb) = row(damage.y + i * (damage.h - 1) / (samples - 1));
                ra != rb
            })
            .count();
        if changed * 2 >= samples as usize {
            return Some(damage);
        }
    }
    let mut tight: Option<Rect> = None;
    for y in damage.y..damage.bottom() {
        let (ra, rb) = row(y);
        if ra == rb {
            continue;
        }
        let differs = |(p, q): (&u32, &u32)| (p ^ q) & mask != 0;
        let Some(left) = ra.iter().zip(rb).position(differs) else { continue };
        let right = ra.iter().zip(rb).rposition(differs).unwrap() + 1;
        damage::add(&mut tight, Rect::new(damage.x + left as i32, y, (right - left) as i32, 1));
    }
    tight
}

/// Surface `sid` and all its sub-surfaces.
fn tree_of(c: &Client, sid: Id, out: &mut Vec<Id>) {
    out.push(sid);
    if let Some(s) = c.surfaces.get(&sid) {
        for child in &s.children {
            tree_of(c, *child, out);
        }
    }
}

/// Draws a surface and its sub-surfaces at (`x`, `y`) points.
fn draw_tree(c: &Client, t: &mut Target, sid: Id, x: i32, y: i32) {
    let Some(s) = c.surfaces.get(&sid) else { return };
    if s.has_content {
        blit(t, s, x, y);
    }
    for child in &s.children {
        if let Some(cs) = c.surfaces.get(child) {
            draw_tree(c, t, *child, x + cs.pos.0, y + cs.pos.1);
        }
    }
}

/// The topmost surface of a tree at (`x`, `y`) in its coordinates.
fn hit_tree(c: &Client, sid: Id, x: i32, y: i32) -> Option<(Id, i32, i32)> {
    let s = c.surfaces.get(&sid)?;
    for child in s.children.iter().rev() {
        if let Some(cs) = c.surfaces.get(child) {
            if let Some(hit) = hit_tree(c, *child, x - cs.pos.0, y - cs.pos.1) {
                return Some(hit);
            }
        }
    }
    let (w, h) = s.size();
    let accepts = s.input.as_ref().is_none_or(|r| region_contains(r, x, y));
    (s.has_content && accepts && x >= 0 && y >= 0 && x < w && y < h).then_some((sid, x, y))
}

fn blit(t: &mut Target, s: &Surface, x: i32, y: i32) {
    let (lw, lh) = s.size();
    let k = t.scale;
    let (x0, y0, tw, th) = (x * k, y * k, lw * k, lh * k);
    if tw <= 0 || th <= 0 || s.bw <= 0 || s.bh <= 0 {
        return;
    }
    // The part of the buffer shown (pixels).
    let (sx, sy, sw, sh) = match s.src {
        Some((a, b, w, h)) => {
            let sc = s.scale as f64;
            (
                ((a * sc) as i32).clamp(0, s.bw - 1),
                ((b * sc) as i32).clamp(0, s.bh - 1),
                ((w * sc) as i32).max(1),
                ((h * sc) as i32).max(1),
            )
        }
        None => (0, 0, s.bw, s.bh),
    };
    let same = sw == tw && sh == th;
    let rows = y0.max(t.clip.y).max(0)..(y0 + th).min(t.clip.bottom()).min(t.height);
    let cols = x0.max(t.clip.x).max(0)..(x0 + tw).min(t.clip.right()).min(t.width);
    if cols.is_empty() {
        return;
    }
    for ty in rows {
        let src_y = if same { sy + ty - y0 } else { sy + ((ty - y0) as i64 * sh as i64 / th as i64) as i32 };
        if src_y < 0 || src_y >= s.bh {
            continue;
        }
        let src_row = &s.pixels[(src_y * s.bw) as usize..((src_y + 1) * s.bw) as usize];
        let dst_row = &mut t.px[(ty * t.stride) as usize..(ty * t.stride + t.width) as usize];
        for tx in cols.clone() {
            let src_x = if same { sx + tx - x0 } else { sx + ((tx - x0) as i64 * sw as i64 / tw as i64) as i32 };
            if src_x < 0 || src_x >= s.bw {
                continue;
            }
            let p = src_row[src_x as usize];
            let d = &mut dst_row[tx as usize];
            if s.opaque {
                *d = p | 0xff00_0000;
                continue;
            }
            let a = p >> 24;
            if a == 255 {
                *d = p;
            } else if a != 0 {
                // Premultiplied "over".
                let inv = 255 - a;
                let ch = |sh: u32| {
                    let sv = (p >> sh) & 0xff;
                    let dv = (*d >> sh) & 0xff;
                    (sv + (dv * inv + 127) / 255).min(255) << sh
                };
                *d = 0xff00_0000 | ch(16) | ch(8) | ch(0);
            }
        }
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    #[test]
    fn identity_viewport_partial_repaint_matches_full_at_both_scales() {
        for scale in [1, 2] {
            let mut s = Surface::new();
            s.bw = 8;
            s.bh = 8;
            s.scale = scale;
            s.has_content = true;
            s.opaque = true;
            s.pixels = vec![0xff123456; 64];
            s.src = Some((0.0, 0.0, 8.0 / scale as f64, 8.0 / scale as f64));
            s.dst = Some((8 / scale, 8 / scale));
            assert!(s.identity_viewport());
            let mut full = vec![0; 64];
            blit(
                &mut Target { px: &mut full, width: 8, height: 8, stride: 8, scale, clip: Rect::new(0, 0, 8, 8) },
                &s,
                0,
                0,
            );
            let mut partial = full.clone();
            s.pixels[19] = 0xffabcdef;
            blit(
                &mut Target { px: &mut full, width: 8, height: 8, stride: 8, scale, clip: Rect::new(0, 0, 8, 8) },
                &s,
                0,
                0,
            );
            let clip = Rect::new(3, 2, 1, 1).points(scale).scaled(scale);
            blit(&mut Target { px: &mut partial, width: 8, height: 8, stride: 8, scale, clip }, &s, 0, 0);
            assert_eq!(partial, full);
            s.dst = Some((4 / scale, 4 / scale));
            assert!(!s.identity_viewport(), "real scaling must still expand damage");
            s.dst = None;
            s.src = Some((1.0, 0.0, 4.0, 4.0));
            assert!(!s.identity_viewport(), "real cropping must still expand damage");
        }
    }

    #[test]
    fn full_damage_is_reduced_to_visible_pixel_changes() {
        let mut source = vec![0xff112233u32; 16];
        let pool = Rc::new(RefCell::new(Pool { fd: -1, ptr: source.as_mut_ptr() as *mut u8, size: 64 }));
        let mut buffer = Buffer::shm(pool.clone(), 0, 4, 4, 16, 1);
        let mut surface = Surface::new();
        assert_eq!(copy_buffer(&buffer, &mut surface, None), Some(Rect::new(0, 0, 4, 4)));
        surface.has_content = true;
        assert!(copy_buffer(&buffer, &mut surface, None).unwrap().empty());
        source[14] = 0xff445566;
        assert_eq!(copy_buffer(&buffer, &mut surface, None), Some(Rect::new(2, 3, 1, 1)));
        assert_eq!(surface.pixels, source);
        // XRGB's unused byte is not a visible change. ARGB's alpha is.
        source[0] ^= 0xff000000;
        assert!(copy_buffer(&buffer, &mut surface, None).unwrap().empty());
        buffer.format = buffers::ARGB8888;
        assert_eq!(copy_buffer(&buffer, &mut surface, None), Some(Rect::new(0, 0, 4, 4)));
        source[0] ^= 0xff000000;
        assert_eq!(copy_buffer(&buffer, &mut surface, None), Some(Rect::new(0, 0, 1, 1)));
        std::mem::forget(pool);
        std::mem::forget(buffer);
    }

    #[test]
    fn damage_copy_preserves_unchanged_pixels_and_resizes_in_full() {
        let mut source = vec![0xff224466u32; 16];
        let pool = Rc::new(RefCell::new(Pool { fd: -1, ptr: source.as_mut_ptr() as *mut u8, size: 64 }));
        let mut buffer = Buffer::shm(pool.clone(), 0, 4, 4, 16, 1);
        let mut surface = Surface::new();
        assert_eq!(copy_buffer(&buffer, &mut surface, Some(Rect::new(1, 1, 1, 1))), Some(Rect::new(0, 0, 4, 4)));
        surface.has_content = true;
        source.fill(0xffabcdef);
        let patch = Rect::new(1, 1, 1, 1);
        assert_eq!(copy_buffer(&buffer, &mut surface, Some(patch)), Some(patch));
        assert_eq!(surface.pixels[5], 0xffabcdef);
        assert_eq!(surface.pixels[6], 0xff224466);
        buffer.width = 2;
        buffer.height = 2;
        assert_eq!(copy_buffer(&buffer, &mut surface, Some(patch)), Some(Rect::new(0, 0, 2, 2)));
        assert!(surface.pixels.iter().all(|p| *p == 0xffabcdef));
        buffer.offset = usize::MAX;
        assert_eq!(copy_buffer(&buffer, &mut surface, None), None);
        // The pool refers to host-owned memory, not a guest mmap.
        std::mem::forget(pool);
        std::mem::forget(buffer);
    }
    #[test]
    fn partial_composition_matches_full_with_transparency_and_scaling() {
        for scale in [1, 2] {
            for opaque in [true, false] {
                let mut surface = Surface::new();
                surface.bw = 4;
                surface.bh = 4;
                surface.scale = scale;
                surface.has_content = true;
                surface.opaque = opaque;
                surface.pixels = vec![0x80402010; 16];
                let mut full = vec![0xff202830; 64];
                let mut partial = full.clone();
                let clip = Rect::new(1, 1, 2, 2);
                blit(
                    &mut Target { px: &mut full, width: 8, height: 8, stride: 8, scale, clip: Rect::new(0, 0, 8, 8) },
                    &surface,
                    0,
                    0,
                );
                blit(&mut Target { px: &mut partial, width: 8, height: 8, stride: 8, scale, clip }, &surface, 0, 0);
                for y in 0..8 {
                    for x in 0..8 {
                        assert_eq!(
                            partial[y * 8 + x],
                            if (1..3).contains(&x) && (1..3).contains(&y) { full[y * 8 + x] } else { 0xff202830 }
                        );
                    }
                }
            }
        }
    }
}
