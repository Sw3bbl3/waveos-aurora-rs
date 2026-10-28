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
//! Buffers are shared memory (`wl_shm`); the bridge copies each committed
//! buffer and releases it straight away.

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
];

// ------------------------------------------------------------------ objects

struct Pool {
    fd: Fd,
    ptr: *mut u8,
    size: usize,
}

impl Pool {
    fn map(fd: Fd, size: usize) -> Option<Pool> {
        let ptr = sys::mmap_shared(fd, size.max(1))?;
        Some(Pool { fd, ptr, size })
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        sys::munmap(self.ptr, self.size.max(1));
        sys::close(self.fd);
    }
}

struct Buffer {
    pool: Rc<RefCell<Pool>>,
    offset: usize,
    width: i32,
    height: i32,
    stride: i32,
    /// `wl_shm` format: 0 ARGB8888 (premultiplied), 1 XRGB8888.
    format: u32,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Role {
    None,
    Cursor,
    /// An `xdg_surface` (its id).
    Xdg(Id),
    /// A sub-surface of this surface.
    Sub(Id),
}

#[derive(Default)]
struct Pending {
    /// `Some(None)`: the buffer was detached.
    buffer: Option<Option<Id>>,
    offset: (i32, i32),
    scale: Option<i32>,
    frames: Vec<Id>,
}

struct Surface {
    pending: Pending,
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
}

impl Surface {
    fn new() -> Surface {
        Surface {
            pending: Pending::default(),
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
    frames: Vec<(ClientId, Id)>,
    last_frame: u32,
    /// The program owning the selection, and its data source.
    selection: Option<(ClientId, Id)>,
    /// Clipboard text being read from a program (pipe, bytes so far).
    clip_in: Vec<(Fd, Vec<u8>)>,
    /// Clipboard text being written to a program.
    clip_out: Vec<(Fd, Vec<u8>)>,
}

fn main() {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| String::from("/run"));
    let name = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| String::from("wayland-0"));
    let path = format!("{dir}/{name}");
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
    };
    println!("wl-bridge: listening on {path}");
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
            1000
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
            for (cid, cb) in std::mem::take(&mut self.frames) {
                if let Some(c) = self.clients.get_mut(&cid) {
                    if c.objects.remove(&cb).is_some() {
                        c.send(cb, wl_callback::ev::DONE, &[Out::U(now)]);
                        c.send(1, wl_display::ev::DELETE_ID, &[Out::U(cb)]);
                    }
                }
            }
        }
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
                    eprintln!("wl-bridge: client {cid}: {e}");
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
        let windows: Vec<u32> = self.windows.iter().filter(|(_, w)| w.client == cid).map(|(id, _)| *id).collect();
        for id in windows {
            self.windows.remove(&id);
            sys::win_close(id);
        }
        if self.pointer.is_some_and(|p| p.0 == cid) {
            self.pointer = None;
        }
        if self.selection.is_some_and(|s| s.0 == cid) {
            self.selection = None;
        }
        self.frames.retain(|f| f.0 != cid);
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
                let c = self.clients.get_mut(&cid).unwrap();
                for (i, (name, v)) in GLOBALS.iter().enumerate() {
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
            (wl_region::NAME, wl_region::req::DESTROY) => self.destroyed(cid, obj),
            (wl_region::NAME, _) => {}

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
                let b = Buffer {
                    pool,
                    offset: a[1].int().max(0) as usize,
                    width: a[2].int(),
                    height: a[3].int(),
                    stride: a[4].int(),
                    format: a[5].uint(),
                };
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
                self.clients.get_mut(&cid).unwrap().buffers.remove(&obj);
                self.destroyed(cid, obj);
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
                // WaveOS draws its own pointer.
                let c = self.clients.get_mut(&cid).unwrap();
                if let Some(s) = c.surfaces.get_mut(&a[1].uint()) {
                    s.role = Role::Cursor;
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
                        s.src = None;
                        s.dst = None;
                        c.viewports.remove(&obj);
                        self.destroyed(cid, obj);
                    }
                    wp_viewport::req::SET_SOURCE => {
                        let r = (a[0].fixed(), a[1].fixed(), a[2].fixed(), a[3].fixed());
                        s.src = (r.2 > 0.0 && r.3 > 0.0).then_some(r);
                    }
                    wp_viewport::req::SET_DESTINATION => {
                        let d = (a[0].int(), a[1].int());
                        s.dst = (d.0 > 0 && d.1 > 0).then_some(d);
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
            wl_surface::req::FRAME => {
                let cb = a[0].uint();
                c.objects.insert(cb, (wl_callback::NAME, 1));
                if let Some(s) = c.surfaces.get_mut(&sid) {
                    s.pending.frames.push(cb);
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
        let c = self.clients.get_mut(&cid).unwrap();
        let Some(s) = c.surfaces.get_mut(&sid) else { return };
        let p = std::mem::take(&mut s.pending);
        if let Some(scale) = p.scale {
            s.scale = scale;
        }
        if let Some(pos) = s.pending_pos.take() {
            s.pos = pos;
        }
        let mut released = None;
        match p.buffer {
            Some(Some(bid)) => {
                if let Some(b) = c.buffers.get(&bid) {
                    copy_buffer(b, s);
                    s.has_content = true;
                    released = Some(bid);
                }
            }
            Some(None) => {
                s.has_content = false;
                s.pixels = Vec::new();
            }
            None => {}
        }
        let role = s.role;
        for cb in p.frames {
            self.frames.push((cid, cb));
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
                self.surface_changed(cid, sid);
            }
            Role::Sub(_) => self.surface_changed(cid, sid),
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
                c.toplevels.insert(id, Toplevel { xdg: xid, title: String::new(), window: None, asked: None });
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
                    x.geometry = (g.2 > 0 && g.3 > 0).then_some(g);
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
            _ => {}
        }
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
        match x.role {
            XdgRole::Toplevel(top) => self.configure_toplevel(cid, top, (0, 0)),
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
        let xdg = t.xdg;
        if v >= 4 {
            c.send(top, xdg_toplevel::ev::CONFIGURE_BOUNDS, &[Out::I(screen.0 / scale), Out::I(screen.1 / scale)]);
        }
        if v >= 5 {
            c.send(top, xdg_toplevel::ev::WM_CAPABILITIES, &[Out::A(&[])]);
        }
        // States: 4 activated.
        let states: Vec<u8> = if focused { 4u32.to_le_bytes().to_vec() } else { Vec::new() };
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
            eprintln!("wl-bridge: no more windows");
            return;
        };
        t.window = Some(id);
        let surface = sys::win_surface(id);
        let size =
            (surface.width as i32 / surface.scale.max(1) as i32, surface.height as i32 / surface.scale.max(1) as i32);
        self.windows.insert(id, Window { client: cid, toplevel: top, size, surface, focused: false, dirty: true });
        if size != (w, h) {
            self.configure_toplevel(cid, top, size);
        }
    }

    fn close_window(&mut self, cid: ClientId, top: Id) {
        let Some(c) = self.clients.get_mut(&cid) else { return };
        let Some(id) = c.toplevels.get_mut(&top).and_then(|t| t.window.take()) else { return };
        self.windows.remove(&id);
        sys::win_close(id);
        if self.pointer.is_some_and(|p| p.2 == id) {
            self.pointer = None;
        }
    }

    /// After a commit: a program that drew another size than its window's
    /// is asked for the window's size again (once).
    fn check_size(&mut self, cid: ClientId, top: Id) {
        let size = self.content_size(cid, top);
        let Some(wid) = self.clients[&cid].toplevels.get(&top).and_then(|t| t.window) else { return };
        let Some(w) = self.windows.get(&wid) else { return };
        let want = w.size;
        let asked = self.clients[&cid].toplevels.get(&top).and_then(|t| t.asked);
        if size != want && asked != Some(want) {
            self.configure_toplevel(cid, top, want);
        }
    }

    // ------------------------------------------------------------ drawing

    /// Draws a window's surfaces into its WaveOS window.
    fn compose(&mut self, wid: u32) {
        let Some(w) = self.windows.get_mut(&wid) else { return };
        w.dirty = false;
        let Some(c) = self.clients.get(&w.client) else { return };
        let Some(t) = c.toplevels.get(&w.toplevel) else { return };
        let Some(x) = c.xdg.get(&t.xdg) else { return };
        let surf = w.surface;
        if surf.addr == 0 {
            return;
        }
        let px = unsafe { std::slice::from_raw_parts_mut(surf.addr as *mut u32, (surf.stride * surf.height) as usize) };
        let target = Target {
            px,
            width: surf.width as i32,
            height: surf.height as i32,
            stride: surf.stride as i32,
            scale: surf.scale.max(1) as i32,
        };
        target.px.fill(0xffff_ffff);
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
        sys::win_present(wid, surf.width, surf.height);
    }

    // ------------------------------------------------------ WaveOS events

    fn wave_event(&mut self, e: sys::Event) {
        use sys::event::*;
        let wid = e.window;
        let Some(w) = self.windows.get(&wid) else { return };
        let (cid, top) = (w.client, w.toplevel);
        match e.kind {
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
        let Some(code) = keys::evdev(e.a, e.b) else { return };
        self.send_key(cid, code, e.c != 0, e.d);
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
    px: &'a mut [u32],
    width: i32,
    height: i32,
    stride: i32,
    scale: i32,
}

/// Copies a committed buffer into the surface.
fn copy_buffer(b: &Buffer, s: &mut Surface) {
    let pool = b.pool.borrow();
    let (w, h, stride) = (b.width.max(0) as usize, b.height.max(0) as usize, b.stride.max(0) as usize);
    let need = b.offset + stride * h.saturating_sub(1) + w * 4;
    if w == 0 || h == 0 || stride < w * 4 || need > pool.size {
        return;
    }
    s.pixels.resize(w * h, 0);
    for y in 0..h {
        let row = unsafe { std::slice::from_raw_parts(pool.ptr.add(b.offset + y * stride) as *const u32, w) };
        s.pixels[y * w..(y + 1) * w].copy_from_slice(row);
    }
    s.bw = w as i32;
    s.bh = h as i32;
    s.opaque = b.format == 1;
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
    (s.has_content && x >= 0 && y >= 0 && x < w && y < h).then_some((sid, x, y))
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
    let rows = y0.max(0)..(y0 + th).min(t.height);
    let cols = x0.max(0)..(x0 + tw).min(t.width);
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
