//! The display core: GPU and display drivers, modelled on Linux KMS.
//!
//! Every display driver — the firmware framebuffer (`gop`), Bochs/QEMU
//! std VGA (`bochs`), the native virtio-gpu driver (`virtio`), and the
//! ported Linux virtio-gpu driver (`linux`) — implements [`KmsDevice`] and
//! exposes the same objects as Linux's kernel mode setting:
//!
//! - **CRTCs** scan a picture out with a [`Mode`] (one per head/scanout).
//! - **Planes** feed a CRTC from a [`Framebuffer`]: one primary plane per
//!   CRTC, optionally a cursor plane and overlays.
//! - **Encoders** link CRTCs to connectors (mostly bookkeeping today).
//! - **Connectors** are the outputs, with status, EDID and mode list.
//! - **Buffer objects** ([`BufferObject`], refcounted, GEM-like) hold
//!   pixels; a [`Framebuffer`] describes one as an image (size, pitch,
//!   format). They are what a future DRM node hands out as GEM handles
//!   and dma-buf fds.
//!
//! State changes go through [`KmsDevice::atomic_commit`] with a partial
//! [`Commit`] (the objects that change), exactly like DRM's atomic ioctl;
//! [`validate`] merges a request into the current state and checks the
//! generic rules so every driver behaves alike. Completed page flips and
//! hotplug are reported as [`Event`]s (with vblank sequence numbers and
//! timestamps, as DRM events will need them).
//!
//! The compositor does not deal with the objects directly: it uses
//! [`output::Output`], which picks the primary device's first connected
//! connector and gives it a CPU-drawable back buffer, a damage flush, a
//! cursor and mode changes. See `docs/developer/display-core.md` for the
//! object model, the multi-monitor plan and where a DRM ioctl layer attaches.

// The object model is the interface a DRM node will expose; parts of it
// (overlays, encoder kinds, events, vblank queries) have no user yet.
#![allow(dead_code)]

pub mod bo;
pub mod bochs;
pub mod gop;
#[cfg(linuxkpi)]
pub mod linux;
pub mod mode;
pub mod output;
pub mod owner;
#[cfg(feature = "ktest")]
pub mod selftest;
pub mod simple;
pub mod virtio;

use crate::sync::IrqMutex;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

pub use bo::{fourcc, BufferObject, Framebuffer};
pub use mode::Mode;

/// A KMS object id (CRTC, plane, encoder, connector, framebuffer, buffer).
/// Unique across all devices, never 0.
pub type ObjId = u32;

static NEXT_ID: AtomicU32 = AtomicU32::new(1);

pub fn new_id() -> ObjId {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The request breaks a rule (DRM's EINVAL).
    Invalid,
    /// Out of memory, in the guest or on the device (ENOMEM).
    NoMemory,
    /// The device stopped answering or reported an error (EIO).
    Io,
    /// A mode change was needed but not allowed (DRM's EINVAL too; kept apart for logs).
    NeedsModeset,
    /// The driver can't do this (EOPNOTSUPP).
    Unsupported,
}

impl Error {
    /// The errno a system call would return.
    pub fn errno(self) -> isize {
        use aurora_abi::err::*;
        match self {
            Error::Invalid | Error::NeedsModeset => EINVAL,
            Error::NoMemory => ENOMEM,
            Error::Io => EIO,
            Error::Unsupported => EOPNOTSUPP,
        }
    }
}

pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlaneKind {
    Primary,
    Cursor,
    Overlay,
}

#[derive(Clone, Debug)]
pub struct PlaneInfo {
    pub id: ObjId,
    pub kind: PlaneKind,
    /// Bit `i` set: the plane can feed the CRTC with index `i`.
    pub possible_crtcs: u32,
    pub formats: &'static [u32],
    /// Largest framebuffer the plane takes (the cursor's size).
    pub max_size: (u32, u32),
}

#[derive(Clone, Debug)]
pub struct CrtcInfo {
    pub id: ObjId,
    /// Position in the device's CRTC list (for `possible_crtcs` masks).
    pub index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EncoderKind {
    None,
    Dac,
    Tmds,
    Lvds,
    DpMst,
    Virtual,
}

#[derive(Clone, Debug)]
pub struct EncoderInfo {
    pub id: ObjId,
    pub kind: EncoderKind,
    pub possible_crtcs: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectorKind {
    Unknown,
    Vga,
    DviD,
    HdmiA,
    DisplayPort,
    Edp,
    Virtual,
}

impl ConnectorKind {
    pub fn name(self) -> &'static str {
        match self {
            ConnectorKind::Unknown => "Unknown",
            ConnectorKind::Vga => "VGA",
            ConnectorKind::DviD => "DVI-D",
            ConnectorKind::HdmiA => "HDMI-A",
            ConnectorKind::DisplayPort => "DP",
            ConnectorKind::Edp => "eDP",
            ConnectorKind::Virtual => "Virtual",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Connected,
    Disconnected,
    Unknown,
}

#[derive(Clone, Debug)]
pub struct ConnectorInfo {
    pub id: ObjId,
    pub kind: ConnectorKind,
    /// 1-based number among connectors of this kind ("Virtual-1").
    pub kind_index: u32,
    pub status: Status,
    /// The preferred mode (if any) first.
    pub modes: Vec<Mode>,
    pub edid: Option<Arc<Vec<u8>>>,
    /// Physical size in millimetres (0 when unknown).
    pub mm: (u32, u32),
    pub encoders: Vec<ObjId>,
}

impl ConnectorInfo {
    pub fn name(&self) -> String {
        alloc::format!("{}-{}", self.kind.name(), self.kind_index)
    }

    pub fn preferred(&self) -> Option<&Mode> {
        self.modes.iter().find(|m| m.is_preferred()).or(self.modes.first())
    }
}

/// What a device can do.
#[derive(Clone, Copy, Debug, Default)]
pub struct Caps {
    /// The resolution can change while running.
    pub live_modeset: bool,
    /// Any size up to `max_size` can be shown (virtual displays), not just
    /// the connector's modes.
    pub any_size: bool,
    pub max_size: (u32, u32),
    /// Drawing into a framebuffer shows nothing until [`KmsDevice::dirty`].
    /// (Every driver accepts `dirty`; for some it is the copy to VRAM.)
    pub needs_dirty: bool,
    /// A cursor plane of this size exists.
    pub cursor: Option<(u32, u32)>,
    /// Vblank interrupts exist (otherwise flip events are sent when the
    /// device has taken the new picture, timestamped then).
    pub vblank_irq: bool,
    /// The preferred mode follows a host window (virtual machines).
    pub follows_host: bool,
}

/// A CRTC's state in a [`Commit`] (and as committed).
#[derive(Clone, Debug, PartialEq)]
pub struct CrtcState {
    pub crtc: ObjId,
    pub active: bool,
    pub mode: Option<Mode>,
    pub connectors: Vec<ObjId>,
}

/// A plane's state: which framebuffer, which part of it (`src`, in 16.16
/// fixed point as in DRM), and where on the CRTC (`dst`).
#[derive(Clone, Debug)]
pub struct PlaneState {
    pub plane: ObjId,
    pub crtc: Option<ObjId>,
    pub fb: Option<Arc<Framebuffer>>,
    pub src: (u32, u32, u32, u32),
    pub dst: (i32, i32, u32, u32),
    /// The cursor's hot spot inside the framebuffer (cursor planes).
    pub hotspot: (i32, i32),
}

impl PlaneState {
    pub fn disabled(plane: ObjId) -> PlaneState {
        PlaneState { plane, crtc: None, fb: None, src: (0, 0, 0, 0), dst: (0, 0, 0, 0), hotspot: (0, 0) }
    }

    /// The whole of `fb` at (`x`, `y`) on `crtc`, unscaled.
    pub fn show(plane: ObjId, crtc: ObjId, fb: &Arc<Framebuffer>, x: i32, y: i32) -> PlaneState {
        PlaneState {
            plane,
            crtc: Some(crtc),
            fb: Some(fb.clone()),
            src: (0, 0, fb.width << 16, fb.height << 16),
            dst: (x, y, fb.width, fb.height),
            hotspot: (0, 0),
        }
    }

    /// Same framebuffer and source: only the position (or nothing) differs.
    pub fn same_image(&self, o: &PlaneState) -> bool {
        self.crtc == o.crtc
            && self.src == o.src
            && self.hotspot == o.hotspot
            && match (&self.fb, &o.fb) {
                (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                (None, None) => true,
                _ => false,
            }
    }
}

/// Commit flags, with DRM's values.
pub mod commit {
    /// Send a [`super::Event::Flip`] when the new state is on screen.
    pub const PAGE_FLIP_EVENT: u32 = 0x01;
    pub const TEST_ONLY: u32 = 0x100;
    /// Return before the change is on screen (drivers may still finish it first).
    pub const NONBLOCK: u32 = 0x200;
    /// A full mode set may happen (otherwise a commit that needs one fails).
    pub const ALLOW_MODESET: u32 = 0x400;
}

/// An atomic request: the CRTCs and planes that change, all or nothing.
#[derive(Clone, Debug, Default)]
pub struct Commit {
    pub crtcs: Vec<CrtcState>,
    pub planes: Vec<PlaneState>,
    pub flags: u32,
    /// Returned in the flip event.
    pub user_data: u64,
}

/// A rectangle of a framebuffer that changed, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Damage {
    pub fn clamp(self, w: u32, h: u32) -> Damage {
        let x = self.x.min(w);
        let y = self.y.min(h);
        Damage { x, y, w: self.w.min(w - x), h: self.h.min(h - y) }
    }

    pub fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }
}

/// A CRTC's vblank counter: frames shown so far and when the last began.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vblank {
    pub seq: u64,
    pub time_ns: u64,
}

/// A display driver, as KMS objects. All methods may block (they run in
/// task context); none may be called with an `IrqMutex` held.
pub trait KmsDevice: Send + Sync {
    fn name(&self) -> &str;
    fn caps(&self) -> Caps;
    /// The firmware's framebuffer: replaced as soon as a real driver for
    /// the same screen registers.
    fn firmware(&self) -> bool {
        false
    }
    fn crtcs(&self) -> Vec<CrtcInfo>;
    fn planes(&self) -> Vec<PlaneInfo>;
    fn encoders(&self) -> Vec<EncoderInfo>;
    /// The connectors as last probed (cheap).
    fn connectors(&self) -> Vec<ConnectorInfo>;
    /// Detects the connectors again (EDID, status) and returns them.
    fn probe(&self) -> Vec<ConnectorInfo> {
        self.connectors()
    }
    /// The committed state of every CRTC and plane.
    fn state(&self) -> (Vec<CrtcState>, Vec<PlaneState>);

    /// A zeroed CPU-mapped buffer for a `w`×`h` image (DRM's dumb buffer).
    fn create_dumb(&self, w: u32, h: u32, format: u32) -> Result<Arc<Framebuffer>> {
        Framebuffer::new_system(w, h, format)
    }

    fn atomic_check(&self, c: &Commit) -> Result<()>;
    fn atomic_commit(&self, c: &Commit) -> Result<()>;

    /// `rects` of `fb` changed (DRM's DIRTYFB): show them wherever `fb` is
    /// scanned out.
    fn dirty(&self, fb: &Arc<Framebuffer>, rects: &[Damage]) -> Result<()>;

    fn vblank(&self, crtc: ObjId) -> Vblank;

    /// Before sleep, and after it: restore the committed state (the device
    /// was reset), including the picture if the device lost it.
    fn suspend(&self) {}
    fn resume(&self) {}

    /// A CPU-writable physical framebuffer showing the first CRTC, for the
    /// panic screen (firmware and VGA-style devices only).
    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        None
    }
}

// ------------------------------------------------------------- registry

static DEVICES: IrqMutex<Vec<Arc<dyn KmsDevice>>> = IrqMutex::new(Vec::new());

/// Adds a display device. A real driver retires the firmware framebuffer
/// (its memory may be about to change under it).
pub fn register(dev: Arc<dyn KmsDevice>) {
    let mut devs = DEVICES.lock();
    if !dev.firmware() {
        devs.retain(|d| !d.firmware());
    }
    log!("gpu", "{}: {} connector(s), {} CRTC(s)", dev.name(), dev.connectors().len(), dev.crtcs().len());
    devs.push(dev);
}

pub fn devices() -> Vec<Arc<dyn KmsDevice>> {
    DEVICES.lock().clone()
}

/// The device the desktop runs on: the first real driver, else the firmware's.
pub fn primary() -> Option<Arc<dyn KmsDevice>> {
    DEVICES.lock().first().cloned()
}

// --------------------------------------------------------------- events

#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// A commit with [`commit::PAGE_FLIP_EVENT`] is on screen.
    Flip { crtc: ObjId, seq: u64, time_ns: u64, user_data: u64 },
    /// Connectors changed (plugged, unplugged, new preferred mode).
    Hotplug,
}

static EVENTS: IrqMutex<VecDeque<Event>> = IrqMutex::new(VecDeque::new());
const MAX_EVENTS: usize = 64;

/// Queues an event for whoever reads them (a future DRM file). Old events
/// fall off when nobody does.
pub fn post(e: Event) {
    let mut q = EVENTS.lock();
    if q.len() >= MAX_EVENTS {
        q.pop_front();
    }
    q.push_back(e);
}

pub fn take_events() -> Vec<Event> {
    EVENTS.lock().drain(..).collect()
}

/// A driver saw its connectors change: tell the desktop.
pub fn hotplug() {
    post(Event::Hotplug);
    crate::gui::display_changed();
}

/// After sleep: every device restores its committed state.
pub fn resume_all() {
    for d in devices() {
        d.resume();
    }
}

// ---------------------------------------------------- atomic validation

/// The rules every driver shares. Merges `c` into the device's current
/// state and checks the result: objects exist, planes go to CRTCs they
/// can feed, sources lie inside framebuffers, a primary plane covers its
/// CRTC unscaled, a cursor fits the cursor plane, an active CRTC has a
/// mode and a connector, and a mode change has `ALLOW_MODESET`.
/// `current` is the committed state (drivers pass it in, since they hold
/// their state lock while committing). Returns the merged state and
/// whether a mode set is needed.
pub fn validate(
    dev: &dyn KmsDevice,
    current: (Vec<CrtcState>, Vec<PlaneState>),
    c: &Commit,
) -> Result<(Vec<CrtcState>, Vec<PlaneState>, bool)> {
    let crtcs = dev.crtcs();
    let planes = dev.planes();
    let connectors = dev.connectors();
    let (mut cs, mut ps) = current;
    let mut modeset = false;
    for want in &c.crtcs {
        let Some(cur) = cs.iter_mut().find(|s| s.crtc == want.crtc) else { return Err(Error::Invalid) };
        if want.active != cur.active || want.mode != cur.mode || want.connectors != cur.connectors {
            modeset = true;
        }
        *cur = want.clone();
    }
    for want in &c.planes {
        let Some(cur) = ps.iter_mut().find(|s| s.plane == want.plane) else { return Err(Error::Invalid) };
        *cur = want.clone();
    }
    if modeset && c.flags & commit::ALLOW_MODESET == 0 {
        return Err(Error::NeedsModeset);
    }
    for s in &cs {
        if !s.active {
            continue;
        }
        let Some(mode) = s.mode else { return Err(Error::Invalid) };
        if s.connectors.is_empty() || !s.connectors.iter().all(|id| connectors.iter().any(|k| k.id == *id)) {
            return Err(Error::Invalid);
        }
        let caps = dev.caps();
        if mode.hdisplay == 0
            || mode.vdisplay == 0
            || mode.hdisplay > caps.max_size.0
            || mode.vdisplay > caps.max_size.1
        {
            return Err(Error::Invalid);
        }
    }
    for p in &ps {
        let info = planes.iter().find(|i| i.id == p.plane).ok_or(Error::Invalid)?;
        let (Some(crtc), Some(fb)) = (p.crtc, &p.fb) else {
            if p.fb.is_some() != p.crtc.is_some() {
                return Err(Error::Invalid);
            }
            continue;
        };
        let index = crtcs.iter().find(|i| i.id == crtc).ok_or(Error::Invalid)?.index;
        if info.possible_crtcs & (1 << index) == 0 || !info.formats.contains(&fb.format) {
            return Err(Error::Invalid);
        }
        let (sx, sy, sw, sh) = (p.src.0 >> 16, p.src.1 >> 16, p.src.2 >> 16, p.src.3 >> 16);
        if sw == 0 || sh == 0 || sx + sw > fb.width || sy + sh > fb.height {
            return Err(Error::Invalid);
        }
        if fb.width > info.max_size.0 || fb.height > info.max_size.1 {
            return Err(Error::Invalid);
        }
        let state = cs.iter().find(|s| s.crtc == crtc).ok_or(Error::Invalid)?;
        if !state.active {
            return Err(Error::Invalid);
        }
        if info.kind == PlaneKind::Primary {
            let mode = state.mode.ok_or(Error::Invalid)?;
            // No scaling and no partial coverage on the primary plane.
            if p.dst != (0, 0, mode.hdisplay, mode.vdisplay) || (sw, sh) != (mode.hdisplay, mode.vdisplay) {
                return Err(Error::Invalid);
            }
        } else if (p.dst.2, p.dst.3) != (sw, sh) {
            return Err(Error::Invalid);
        }
    }
    Ok((cs, ps, modeset))
}

/// Modes for a virtual display of at most `max`: the common sizes that fit.
pub fn virtual_modes(max: (u32, u32)) -> Vec<Mode> {
    mode::COMMON.iter().filter(|&&(w, h)| w <= max.0 && h <= max.1).filter_map(|&(w, h)| Mode::cvt(w, h, 60)).collect()
}
