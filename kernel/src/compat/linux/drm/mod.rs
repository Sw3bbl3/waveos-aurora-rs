//! The Linux DRM device: `/dev/dri/card<N>` for every display device of
//! the display core (`drivers::gpu`), with Linux's ioctl numbers, struct
//! layouts and semantics, so unmodified Linux programs (libdrm, Mesa's
//! software KMS path, `modetest`) drive the display.
//!
//! - Core ioctls (version, capabilities, master, magic) and GEM/PRIME are
//!   here: a file's handle table maps GEM handles to the core's buffer
//!   objects, and a dma-buf descriptor ([`super::fs::Special::DmaBuf`]) holds
//!   one too, so it can be passed over a local socket and mapped anywhere.
//! - Mode setting ([`kms`]) maps one to one onto the core's objects (ids are
//!   the core's) and atomic commits; properties and blobs are in [`props`];
//!   vblank counting, `WAIT_VBLANK` and the events read from the file are
//!   in [`vblank`].
//! - `/sys/class/drm`, `/sys/dev/char/226:N` and the PCI device behind each
//!   card come from `super::sysfs`, which is what libdrm's
//!   `drmGetDevices2` reads.
//!
//! Who drives the display: the first program to open a card becomes its
//! master (as on Linux, when nobody else is); `SET_MASTER` and
//! `DROP_MASTER` hand it over. Reading state takes nothing from the
//! desktop. The master's first commit — a mode set, page flip, plane,
//! cursor or atomic commit — takes the display from the desktop
//! (`gpu::owner`); when the master drops master or its file closes (it
//! exited, crashed or was killed) the desktop's picture comes back.
//!
//! Every argument is copied in and out by the size encoded in the ioctl
//! number and zero-extended, as Linux does, so programs built against
//! older or newer headers work.

pub mod kms;
pub mod props;
pub mod vblank;

use super::fs::Special;
use crate::drivers::gpu::{self, BufferObject, Framebuffer, KmsDevice, ObjId};
use crate::proc::{Handle, Process};
use crate::sync::{IrqMutex, Mutex};
use crate::syscall::user;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// DRM's character device major number.
pub const MAJOR: u64 = 226;
/// Render nodes' minor numbers start here (`renderD128`).
const RENDER_MINOR: u32 = 128;

// ------------------------------------------------------------ drivers

/// What a GPU driver adds to the DRM node of its display device: the name
/// Linux programs know it by (`DRM_IOCTL_VERSION`; Mesa chooses its user
/// space driver by it), and its private ioctls (numbers `0x40`–`0x9F`:
/// i915's execbuffer, virtio-gpu's 3D commands…). A render node
/// (`renderD<128+N>`) is offered for drivers that have render ioctls.
pub trait Driver: Sync {
    fn name(&self) -> &'static str;
    fn desc(&self) -> &'static str;
    fn date(&self) -> &'static str;
    fn version(&self) -> (i32, i32, i32);
    /// Has render ioctls: a render node is offered.
    fn render(&self) -> bool {
        false
    }
    /// A private ioctl (`nr` counts from `0x40`), its argument copied in
    /// and out like the core ones'.
    fn ioctl(&self, _file: &File, _nr: u32, _data: &mut [u8]) -> Result<(), isize> {
        Err(EINVAL)
    }
}

/// A display-only driver: a name and nothing private.
struct Kms {
    name: &'static str,
    desc: &'static str,
    date: &'static str,
    version: (i32, i32, i32),
}

impl Driver for Kms {
    fn name(&self) -> &'static str {
        self.name
    }
    fn desc(&self) -> &'static str {
        self.desc
    }
    fn date(&self) -> &'static str {
        self.date
    }
    fn version(&self) -> (i32, i32, i32) {
        self.version
    }
}

/// The names Linux's drivers for the same hardware have.
static VIRTIO: Kms = Kms { name: "virtio_gpu", desc: "virtio GPU", date: "0", version: (0, 1, 0) };
static BOCHS: Kms =
    Kms { name: "bochs-drm", desc: "bochs dispi vga interface (qemu stdvga)", date: "20130925", version: (1, 0, 0) };
static SIMPLE: Kms = Kms {
    name: "simpledrm",
    desc: "DRM driver for simple-framebuffer platform devices",
    date: "20200625",
    version: (1, 0, 0),
};
static OTHER: Kms = Kms { name: "waveos", desc: "WaveOS display", date: "20261123", version: (1, 0, 0) };

pub fn driver_for(dev: &dyn KmsDevice) -> &'static dyn Driver {
    match dev.name() {
        "virtio-gpu" | "linux-virtio-gpu" => &VIRTIO,
        "bochs-vga" => &BOCHS,
        "efi-gop" => &SIMPLE,
        _ => &OTHER,
    }
}

// ------------------------------------------------------------ cards

/// The display devices as cards: `card<i>` is `gpu::devices()[i]`.
pub fn cards() -> Vec<Arc<dyn KmsDevice>> {
    gpu::devices()
}

fn dev_key(dev: &dyn KmsDevice) -> usize {
    dev as *const dyn KmsDevice as *const () as usize
}

/// The device and node kind for a `/dev/dri` name ("card0", "renderD128").
fn node(name: &str) -> Option<(Arc<dyn KmsDevice>, u32, bool)> {
    let cards = cards();
    if let Some(n) = name.strip_prefix("card").and_then(|n| n.parse::<u32>().ok()) {
        return cards.get(n as usize).map(|d| (d.clone(), n, false));
    }
    let n = name.strip_prefix("renderD").and_then(|n| n.parse::<u32>().ok())?.checked_sub(RENDER_MINOR)?;
    let d = cards.get(n as usize)?;
    driver_for(&**d).render().then(|| (d.clone(), RENDER_MINOR + n, true))
}

/// The names in `/dev/dri`.
pub fn node_names() -> Vec<String> {
    let mut v = Vec::new();
    for (i, d) in cards().iter().enumerate() {
        v.push(alloc::format!("card{i}"));
        if driver_for(&**d).render() {
            v.push(alloc::format!("renderD{}", RENDER_MINOR as usize + i));
        }
    }
    v
}

/// `st_rdev` of a `/dev/dri` node, if `guest` is one.
pub fn node_rdev(guest: &str) -> Option<u64> {
    let (_, minor, _) = node(guest.strip_prefix("/dev/dri/")?)?;
    Some(super::fs::makedev(MAJOR, minor as u64))
}

/// What a card knows beyond its files: who is master, the bus id a master set.
struct Card {
    dev: usize,
    master: Option<u64>,
    unique: String,
}

static CARDS: Mutex<Vec<Card>> = Mutex::new(Vec::new());

fn with_card<R>(dev: &dyn KmsDevice, f: impl FnOnce(&mut Card) -> R) -> R {
    let mut cards = CARDS.lock();
    let key = dev_key(dev);
    let i = match cards.iter().position(|c| c.dev == key) {
        Some(i) => i,
        None => {
            cards.push(Card { dev: key, master: None, unique: String::new() });
            cards.len() - 1
        }
    };
    f(&mut cards[i])
}

// ------------------------------------------------------------ files

/// What a client asked for with `SET_CLIENT_CAP`.
#[derive(Clone, Copy, Default)]
pub struct ClientCaps {
    pub stereo: bool,
    pub universal_planes: bool,
    pub atomic: bool,
    pub aspect_ratio: bool,
    pub writeback: bool,
    pub hotspot: bool,
}

pub struct State {
    /// GEM handles. (A driver with objects of its own — 3D resources —
    /// adds a kind here; today every object is a core buffer object.)
    pub handles: BTreeMap<u32, Arc<BufferObject>>,
    next_handle: u32,
    /// Framebuffers made with `ADDFB`/`ADDFB2` (removed with `RMFB` or on close).
    pub fbs: Vec<Arc<Framebuffer>>,
    pub caps: ClientCaps,
}

/// An open card (`struct drm_file`).
pub struct File {
    pub id: u64,
    pub dev: Arc<dyn KmsDevice>,
    pub minor: u32,
    pub render: bool,
    pub state: Mutex<State>,
    pub events: IrqMutex<vblank::Events>,
    magic: u32,
    authenticated: AtomicBool,
    /// Became master once (may take it back without privileges, as on Linux).
    was_master: AtomicBool,
}

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);
static FILES: Mutex<Vec<(u64, Weak<File>)>> = Mutex::new(Vec::new());

impl File {
    pub fn driver(&self) -> &'static dyn Driver {
        driver_for(&*self.dev)
    }

    pub fn is_master(&self) -> bool {
        !self.render && with_card(&*self.dev, |c| c.master == Some(self.id))
    }

    /// The handle for `bo` in this file (made if it has none).
    pub fn handle_for(&self, bo: &Arc<BufferObject>) -> u32 {
        let mut s = self.state.lock();
        if let Some((&h, _)) = s.handles.iter().find(|(_, b)| Arc::ptr_eq(b, bo)) {
            return h;
        }
        let h = s.next_handle;
        s.next_handle += 1;
        s.handles.insert(h, bo.clone());
        h
    }

    pub fn bo(&self, handle: u32) -> Option<Arc<BufferObject>> {
        self.state.lock().handles.get(&handle).cloned()
    }

    /// Readable: a whole event is waiting.
    pub fn readable(&self) -> bool {
        self.events.lock().promote(crate::time::now_ns())
    }
}

impl Drop for File {
    fn drop(&mut self) {
        FILES.lock().retain(|(id, _)| *id != self.id);
        kms::forget_file(self.id);
        props::forget_file(self.id);
        let was = with_card(&*self.dev, |c| {
            let was = c.master == Some(self.id);
            if was {
                c.master = None;
                c.unique.clear();
            }
            was
        });
        if was {
            give_back(&self.dev, self.id);
        }
    }
}

/// The desktop gets the display back from file `id` (if it took it).
/// Needs a task that may block; from anywhere else a helper task does it.
fn give_back(dev: &Arc<dyn KmsDevice>, id: u64) {
    if x86_64::instructions::interrupts::are_enabled() {
        gpu::owner::give_back(&**dev, id);
        return;
    }
    static QUEUE: IrqMutex<Vec<(Arc<dyn KmsDevice>, u64)>> = IrqMutex::new(Vec::new());
    QUEUE.lock().push((dev.clone(), id));
    crate::sched::spawn("drm-give-back", || loop {
        let Some((dev, id)) = QUEUE.lock().pop() else { break };
        gpu::owner::give_back(&*dev, id);
    });
}

/// Opens `/dev/dri/<name>` for `guest`.
pub fn open(guest: &str) -> Option<Result<Arc<File>, isize>> {
    let name = guest.strip_prefix("/dev/dri/")?;
    let Some((dev, minor, render)) = node(name) else { return Some(Err(ENOENT)) };
    let id = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
    let f = Arc::new(File {
        id,
        dev,
        minor,
        render,
        state: Mutex::new(State {
            handles: BTreeMap::new(),
            next_handle: 1,
            fbs: Vec::new(),
            caps: ClientCaps::default(),
        }),
        events: IrqMutex::new(vblank::Events::default()),
        // Any number that no other open file has.
        magic: id as u32 | 0x4000_0000,
        authenticated: AtomicBool::new(false),
        was_master: AtomicBool::new(false),
    });
    // The first to open a card while nobody is master becomes master.
    if !render {
        let first = with_card(&*f.dev, |c| {
            let free = c.master.is_none();
            if free {
                c.master = Some(id);
            }
            free
        });
        if first {
            f.was_master.store(true, Ordering::Relaxed);
            f.authenticated.store(true, Ordering::Relaxed);
        }
    }
    FILES.lock().push((id, Arc::downgrade(&f)));
    Some(Ok(f))
}

// ------------------------------------------------------------ dma-buf

/// The buffer object behind a dma-buf descriptor.
pub fn dmabuf_of(p: &Process, fd: u64) -> Result<Arc<BufferObject>, isize> {
    match &*p.fd(fd as usize)? {
        Handle::Special(s) => match &*s.lock() {
            Special::DmaBuf(bo) => Ok(bo.clone()),
            _ => Err(EINVAL),
        },
        _ => Err(EINVAL),
    }
}

/// ioctls on a dma-buf descriptor: `DMA_BUF_IOCTL_SYNC` (CPU access
/// brackets; the memory is coherent, so nothing to do) and `SET_NAME`.
pub fn dmabuf_ioctl(request: u64, arg: u64) -> Result<u64, isize> {
    match request {
        // DMA_BUF_IOCTL_SYNC: flags READ 1, WRITE 2 (RW 3), END 4.
        0x4008_6200 => {
            let flags: u64 = user::get(arg)?;
            if flags & !7 != 0 || flags & 3 == 0 {
                return Err(EINVAL);
            }
            Ok(0)
        }
        // DMA_BUF_SET_NAME (both sizes of the pointer argument).
        0x4008_6201 | 0x4004_6201 => Ok(0),
        _ => Err(ENOTTY),
    }
}

/// The frames `mmap` maps for `n` pages at `offset` of descriptor `fd`
/// when it is a card (an offset from `MAP_DUMB`) or a dma-buf; `None` for
/// other descriptors.
pub fn mmap_frames(p: &Process, fd: u64, offset: u64, n: u64) -> Option<Result<Vec<u64>, isize>> {
    let h = p.fd(fd as usize).ok()?;
    let Handle::Special(s) = &*h else { return None };
    let (bo, first) = match &*s.lock() {
        Special::DmaBuf(bo) => (Ok(bo.clone()), offset / crate::mm::PAGE_SIZE),
        Special::Drm(f) => {
            // `MAP_DUMB` offsets: the buffer's id in the high half.
            let id = (offset >> 32) as ObjId;
            let st = f.state.lock();
            let bo = st.handles.values().find(|b| b.id == id).cloned().ok_or(EACCES);
            (bo, (offset & 0xFFFF_FFFF) / crate::mm::PAGE_SIZE)
        }
        _ => return None,
    };
    Some(bo.and_then(|bo| {
        let frames = bo.user_frames();
        let end = first.checked_add(n).ok_or(EINVAL)?;
        if end > frames.len() as u64 {
            return Err(EINVAL);
        }
        Ok(frames[first as usize..end as usize].to_vec())
    }))
}

// ------------------------------------------------------------ ioctls

/// Little-endian fields of an ioctl's argument.
pub struct Args<'a>(pub &'a mut [u8]);

impl Args<'_> {
    pub fn u32(&self, off: usize) -> u32 {
        u32::from_le_bytes(self.0[off..off + 4].try_into().unwrap())
    }
    pub fn i32(&self, off: usize) -> i32 {
        self.u32(off) as i32
    }
    pub fn u64(&self, off: usize) -> u64 {
        u64::from_le_bytes(self.0[off..off + 8].try_into().unwrap())
    }
    pub fn set32(&mut self, off: usize, v: u32) {
        self.0[off..off + 4].copy_from_slice(&v.to_le_bytes());
    }
    pub fn set64(&mut self, off: usize, v: u64) {
        self.0[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }
    pub fn bytes(&mut self, off: usize, len: usize) -> &mut [u8] {
        &mut self.0[off..off + len]
    }
}

/// Copies up to `room` of `items` to the user array at `ptr` (each
/// `put` writes one), as the "ask for the count, then fill" ioctls do.
pub fn put_array<T: Copy>(ptr: u64, room: u32, items: &[T]) -> Result<(), isize> {
    for (i, v) in items.iter().take(room as usize).enumerate() {
        user::put(ptr + (i * core::mem::size_of::<T>()) as u64, v)?;
    }
    Ok(())
}

/// Reads `count` values from the user array at `ptr`.
pub fn get_array<T: Copy>(ptr: u64, count: u32) -> Result<Vec<T>, isize> {
    if count > 1 << 16 {
        return Err(EINVAL);
    }
    (0..count as u64).map(|i| user::get::<T>(ptr + i * core::mem::size_of::<T>() as u64)).collect()
}

/// Only the master may (DRM_MASTER).
const MASTER: u8 = 1;
/// Allowed on render nodes (DRM_RENDER_ALLOW).
const RENDER: u8 = 2;

/// (number, argument size, flags) of the ioctls there are.
const IOCTLS: &[(u32, usize, u8)] = &[
    (0x00, 64, RENDER),  // VERSION
    (0x01, 16, 0),       // GET_UNIQUE
    (0x02, 4, 0),        // GET_MAGIC
    (0x07, 16, MASTER),  // SET_VERSION
    (0x08, 8, 0),        // MODESET_CTL
    (0x09, 8, RENDER),   // GEM_CLOSE
    (0x0c, 16, RENDER),  // GET_CAP
    (0x0d, 16, 0),       // SET_CLIENT_CAP
    (0x11, 4, MASTER),   // AUTH_MAGIC
    (0x1e, 0, 0),        // SET_MASTER
    (0x1f, 0, 0),        // DROP_MASTER
    (0x2d, 12, RENDER),  // PRIME_HANDLE_TO_FD
    (0x2e, 12, RENDER),  // PRIME_FD_TO_HANDLE
    (0x3a, 24, 0),       // WAIT_VBLANK
    (0x3b, 24, 0),       // CRTC_GET_SEQUENCE
    (0x3c, 24, 0),       // CRTC_QUEUE_SEQUENCE
    (0xA0, 64, 0),       // MODE_GETRESOURCES
    (0xA1, 104, 0),      // MODE_GETCRTC
    (0xA2, 104, MASTER), // MODE_SETCRTC
    (0xA3, 28, MASTER),  // MODE_CURSOR
    (0xA4, 32, 0),       // MODE_GETGAMMA
    (0xA5, 32, MASTER),  // MODE_SETGAMMA
    (0xA6, 20, 0),       // MODE_GETENCODER
    (0xA7, 80, 0),       // MODE_GETCONNECTOR
    (0xA8, 72, MASTER),  // MODE_ATTACHMODE
    (0xA9, 72, MASTER),  // MODE_DETACHMODE
    (0xAA, 64, 0),       // MODE_GETPROPERTY
    (0xAB, 16, MASTER),  // MODE_SETPROPERTY
    (0xAC, 16, 0),       // MODE_GETPROPBLOB
    (0xAD, 28, 0),       // MODE_GETFB
    (0xAE, 28, 0),       // MODE_ADDFB
    (0xAF, 4, 0),        // MODE_RMFB
    (0xB0, 24, MASTER),  // MODE_PAGE_FLIP
    (0xB1, 24, MASTER),  // MODE_DIRTYFB
    (0xB2, 32, 0),       // MODE_CREATE_DUMB
    (0xB3, 16, 0),       // MODE_MAP_DUMB
    (0xB4, 4, 0),        // MODE_DESTROY_DUMB
    (0xB5, 16, 0),       // MODE_GETPLANERESOURCES
    (0xB6, 32, 0),       // MODE_GETPLANE
    (0xB7, 48, MASTER),  // MODE_SETPLANE
    (0xB8, 104, 0),      // MODE_ADDFB2
    (0xB9, 32, 0),       // MODE_OBJ_GETPROPERTIES
    (0xBA, 24, MASTER),  // MODE_OBJ_SETPROPERTY
    (0xBB, 36, MASTER),  // MODE_CURSOR2
    (0xBC, 56, MASTER),  // MODE_ATOMIC
    (0xBD, 16, 0),       // MODE_CREATEPROPBLOB
    (0xBE, 4, 0),        // MODE_DESTROYPROPBLOB
    (0xCE, 104, 0),      // MODE_GETFB2
    (0xD0, 8, 0),        // MODE_CLOSEFB
];

/// Driver-private ioctls: `DRM_COMMAND_BASE` up to `DRM_COMMAND_END`.
const COMMAND_BASE: u32 = 0x40;
const COMMAND_END: u32 = 0xA0;

/// An ioctl on an open card.
pub fn ioctl(p: &Process, f: &Arc<File>, request: u64, arg: u64) -> Result<u64, isize> {
    let (dir, size, kind, nr) =
        ((request >> 30) & 3, ((request >> 16) & 0x3FFF) as usize, (request >> 8) & 0xFF, (request & 0xFF) as u32);
    if kind != 0x64 {
        return Err(ENOTTY);
    }
    let private = (COMMAND_BASE..COMMAND_END).contains(&nr);
    let entry = IOCTLS.iter().find(|e| e.0 == nr);
    let (ours, flags) = match entry {
        Some(&(_, s, fl)) => (s, fl),
        None if private => (size, RENDER),
        None => return Err(EINVAL),
    };
    if flags & MASTER != 0 && !f.is_master() {
        return Err(EACCES);
    }
    if f.render && flags & RENDER == 0 {
        return Err(EACCES);
    }
    // In by the caller's size, zero-extended to ours; out by the caller's.
    let mut data = alloc::vec![0u8; size.max(ours)];
    if dir & 1 != 0 && size > 0 {
        user::get_bytes(arg, &mut data[..size])?;
    }
    let r =
        if private { f.driver().ioctl(f, nr - COMMAND_BASE, &mut data) } else { call(p, f, nr, &mut Args(&mut data)) };
    if dir & 2 != 0 && size > 0 {
        user::put_bytes(arg, &data[..size])?;
    }
    r.map(|_| 0)
}

fn call(p: &Process, f: &Arc<File>, nr: u32, a: &mut Args) -> Result<(), isize> {
    match nr {
        0x00 => version(f, a),
        0x01 => get_unique(f, a),
        0x02 => {
            a.set32(0, f.magic);
            Ok(())
        }
        0x07 => set_version(f, a),
        0x08 | 0xA8 | 0xA9 => Ok(()), // MODESET_CTL, ATTACHMODE, DETACHMODE: no-ops on Linux too
        0x09 => gem_close(f, a.u32(0)),
        0x0c => get_cap(f, a),
        0x0d => set_client_cap(f, a),
        0x11 => auth_magic(f, a.u32(0)),
        0x1e => set_master(f),
        0x1f => drop_master(f),
        0x2d => prime_handle_to_fd(p, f, a),
        0x2e => prime_fd_to_handle(p, f, a),
        0x3a => vblank::wait_vblank(f, a),
        0x3b => vblank::get_sequence(f, a),
        0x3c => vblank::queue_sequence(f, a),
        0xA0 => kms::get_resources(f, a),
        0xA1 => kms::get_crtc(f, a),
        0xA2 => kms::set_crtc(f, a),
        0xA3 => kms::cursor(f, a, false),
        0xA4 => kms::gamma(f, a, false),
        0xA5 => kms::gamma(f, a, true),
        0xA6 => kms::get_encoder(f, a),
        0xA7 => kms::get_connector(f, a),
        0xAA => props::get_property(f, a),
        0xAB => props::set_connector_property(f, a),
        0xAC => props::get_blob(f, a),
        0xAD => kms::get_fb(f, a),
        0xAE => kms::add_fb(f, a),
        0xAF => kms::rm_fb(f, a.u32(0), true),
        0xB0 => kms::page_flip(f, a),
        0xB1 => kms::dirty_fb(f, a),
        0xB2 => kms::create_dumb(f, a),
        0xB3 => kms::map_dumb(f, a),
        0xB4 => gem_close(f, a.u32(0)),
        0xB5 => kms::get_plane_resources(f, a),
        0xB6 => kms::get_plane(f, a),
        0xB7 => kms::set_plane(f, a),
        0xB8 => kms::add_fb2(f, a),
        0xB9 => props::obj_get_properties(f, a),
        0xBA => props::obj_set_property(f, a),
        0xBB => kms::cursor(f, a, true),
        0xBC => props::atomic(f, a),
        0xBD => props::create_blob(f, a),
        0xBE => props::destroy_blob(f, a.u32(0)),
        0xCE => kms::get_fb2(f, a),
        0xD0 => kms::rm_fb(f, a.u32(0), false),
        _ => Err(EINVAL),
    }
}

/// Copies `value` to a user buffer the way `drm_copy_field` does: as much
/// as fits, with the full length reported.
fn copy_field(a: &mut Args, len_off: usize, ptr_off: usize, value: &str) -> Result<(), isize> {
    let room = a.u64(len_off) as usize;
    let ptr = a.u64(ptr_off);
    let n = value.len().min(room);
    if n > 0 && ptr != 0 {
        user::put_bytes(ptr, &value.as_bytes()[..n])?;
    }
    a.set64(len_off, value.len() as u64);
    Ok(())
}

/// `struct drm_version`.
fn version(f: &File, a: &mut Args) -> Result<(), isize> {
    let d = f.driver();
    let (major, minor, patch) = d.version();
    a.set32(0, major as u32);
    a.set32(4, minor as u32);
    a.set32(8, patch as u32);
    copy_field(a, 16, 24, d.name())?;
    copy_field(a, 32, 40, d.date())?;
    copy_field(a, 48, 56, d.desc())
}

/// The PCI bus id a master asked for with `SET_VERSION` 1.1 (empty before).
fn get_unique(f: &File, a: &mut Args) -> Result<(), isize> {
    let unique = with_card(&*f.dev, |c| c.unique.clone());
    copy_field(a, 0, 8, &unique)
}

fn set_version(f: &File, a: &mut Args) -> Result<(), isize> {
    let (di_major, di_minor, dd_major, dd_minor) = (a.i32(0), a.i32(4), a.i32(8), a.i32(12));
    let (major, minor, _) = f.driver().version();
    let mut r = Ok(());
    if di_major != -1 {
        if di_major != 1 || !(0..=4).contains(&di_minor) {
            r = Err(EINVAL);
        } else if di_minor >= 1 {
            let bus = super::sysfs::pci_slot(&*f.dev);
            with_card(&*f.dev, |c| c.unique = alloc::format!("pci:{bus}"));
        }
    }
    if r.is_ok() && dd_major != -1 && (dd_major != major || dd_minor < 0 || dd_minor > minor) {
        r = Err(EINVAL);
    }
    a.set32(0, 1);
    a.set32(4, 4);
    a.set32(8, major as u32);
    a.set32(12, minor as u32);
    r
}

fn get_cap(f: &File, a: &mut Args) -> Result<(), isize> {
    let caps = f.dev.caps();
    let cursor = caps.cursor.unwrap_or((64, 64));
    let v = match a.u64(0) {
        0x1 => 1,               // DUMB_BUFFER
        0x2 => 1,               // VBLANK_HIGH_CRTC
        0x3 => 24,              // DUMB_PREFERRED_DEPTH
        0x4 => 0,               // DUMB_PREFER_SHADOW: buffers are system memory
        0x5 => 3,               // PRIME: import and export
        0x6 => 1,               // TIMESTAMP_MONOTONIC
        0x7 => 0,               // ASYNC_PAGE_FLIP
        0x8 => cursor.0 as u64, // CURSOR_WIDTH
        0x9 => cursor.1 as u64, // CURSOR_HEIGHT
        0x10 => 0,              // ADDFB2_MODIFIERS
        0x11 => 0,              // PAGE_FLIP_TARGET
        0x12 => 1,              // CRTC_IN_VBLANK_EVENT
        0x13..=0x15 => 0,       // SYNCOBJ, SYNCOBJ_TIMELINE, ATOMIC_ASYNC_PAGE_FLIP
        _ => return Err(EINVAL),
    };
    // Render nodes answer only what does not concern the display.
    if f.render && !matches!(a.u64(0), 0x5 | 0x6 | 0x13 | 0x14) {
        return Err(EINVAL);
    }
    a.set64(8, v);
    Ok(())
}

fn set_client_cap(f: &File, a: &mut Args) -> Result<(), isize> {
    let (cap, v) = (a.u64(0), a.u64(8));
    let mut s = f.state.lock();
    let c = &mut s.caps;
    let on = match v {
        0 => false,
        1 => true,
        2 if cap == 3 => true,
        _ => return Err(EINVAL),
    };
    match cap {
        1 => c.stereo = on,
        2 => c.universal_planes = on,
        // ATOMIC implies universal planes (and, as value 2, aspect ratios).
        3 => {
            c.atomic = on;
            c.universal_planes = on;
            if v == 2 {
                c.aspect_ratio = true;
            }
        }
        4 => c.aspect_ratio = on,
        5 | 6 if !c.atomic => return Err(EINVAL),
        5 => c.writeback = on,
        6 => c.hotspot = on,
        _ => return Err(EINVAL),
    }
    Ok(())
}

fn auth_magic(f: &File, magic: u32) -> Result<(), isize> {
    let files: Vec<Arc<File>> = FILES.lock().iter().filter_map(|(_, w)| w.upgrade()).collect();
    let other = files.iter().find(|o| o.magic == magic && dev_key(&*o.dev) == dev_key(&*f.dev)).ok_or(EINVAL)?;
    other.authenticated.store(true, Ordering::Relaxed);
    Ok(())
}

/// `SET_MASTER`: when nobody is master. (Programs that were master before
/// may always ask; others too, as WaveOS has no seat manager and the
/// person at the computer owns its display.)
fn set_master(f: &File) -> Result<(), isize> {
    if f.render {
        return Err(EACCES);
    }
    with_card(&*f.dev, |c| match c.master {
        Some(m) if m == f.id => Ok(()),
        Some(_) => Err(EBUSY),
        None => {
            c.master = Some(f.id);
            Ok(())
        }
    })?;
    f.was_master.store(true, Ordering::Relaxed);
    f.authenticated.store(true, Ordering::Relaxed);
    Ok(())
}

/// `DROP_MASTER`: the desktop gets the display back if this file had taken it.
fn drop_master(f: &File) -> Result<(), isize> {
    with_card(&*f.dev, |c| {
        if c.master != Some(f.id) {
            return Err(EINVAL);
        }
        c.master = None;
        c.unique.clear();
        Ok(())
    })?;
    give_back(&f.dev, f.id);
    Ok(())
}

fn gem_close(f: &File, handle: u32) -> Result<(), isize> {
    f.state.lock().handles.remove(&handle).map(|_| ()).ok_or(EINVAL)
}

/// `struct drm_prime_handle`: handle, flags (`DRM_CLOEXEC`, `DRM_RDWR`), fd.
fn prime_handle_to_fd(p: &Process, f: &File, a: &mut Args) -> Result<(), isize> {
    const CLOEXEC: u32 = 0o2000000;
    const RDWR: u32 = 2;
    let (handle, flags) = (a.u32(0), a.u32(4));
    if flags & !(CLOEXEC | RDWR) != 0 {
        return Err(EINVAL);
    }
    let bo = f.bo(handle).ok_or(ENOENT)?;
    let fd = super::fs::install_special(p, Special::DmaBuf(bo), flags & CLOEXEC != 0)?;
    a.set32(8, fd as u32);
    Ok(())
}

fn prime_fd_to_handle(p: &Process, f: &File, a: &mut Args) -> Result<(), isize> {
    let fd = a.i32(8);
    if fd < 0 {
        return Err(EBADF);
    }
    let bo = dmabuf_of(p, fd as u64)?;
    a.set32(0, f.handle_for(&bo));
    Ok(())
}
