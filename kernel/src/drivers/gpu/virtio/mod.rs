//! Native virtio-gpu (2D): QEMU's `virtio-vga` / `virtio-gpu-pci`, and
//! other hypervisors' virtio GPUs.
//!
//! Every scanout the device reports is a head: a CRTC with a primary and
//! a cursor plane, an encoder and a connector (its EDID from `GET_EDID`).
//! A framebuffer is shown without any copy in the guest: its buffer
//! object's memory becomes the backing of a host resource
//! (`RESOURCE_CREATE_2D` + `ATTACH_BACKING`), the primary plane is
//! `SET_SCANOUT`, and [`KmsDevice::dirty`] sends `TRANSFER_TO_HOST_2D` and
//! `RESOURCE_FLUSH` for the damaged rectangles only. The cursor plane
//! uses the cursor queue (`UPDATE_CURSOR`, `MOVE_CURSOR`).
//!
//! A configuration interrupt with `EVENT_DISPLAY` means the host changed
//! the displays (a window resized, an output enabled): the `virtio-gpu`
//! task probes them again and reports hotplug.
//!
//! The command layer (`cmd`) is separate and display-agnostic, for a
//! later 3D context implementation.

pub mod cmd;
pub mod proto;

use super::*;
use crate::sync::Mutex;
use alloc::sync::Weak;
use cmd::Gpu;
use proto::Rect;

const CURSOR_SIZE: u32 = 64;
const PRIMARY_FORMATS: &[u32] = &[fourcc::XRGB8888, fourcc::ARGB8888];
const CURSOR_FORMATS: &[u32] = &[fourcc::ARGB8888];
/// Largest scanout we create (QEMU's default host memory limit is 256 MiB).
const MAX_SIZE: (u32, u32) = (8192, 8192);

struct Head {
    index: u32,
    crtc: ObjId,
    primary: ObjId,
    cursor: ObjId,
    encoder: ObjId,
    connector: ObjId,
}

struct State {
    crtcs: Vec<CrtcState>,
    /// Primary then cursor plane of each head.
    planes: Vec<PlaneState>,
}

pub struct VirtioGpu {
    gpu: Arc<Gpu>,
    heads: Vec<Head>,
    connectors: IrqMutex<Vec<ConnectorInfo>>,
    /// The host's display size per head at the last probe.
    host_sizes: IrqMutex<Vec<(u32, u32, bool)>>,
    state: Mutex<State>,
    vblank: IrqMutex<Vec<Vblank>>,
}

/// Host resources alive, with their buffers (recreated after sleep).
struct Live {
    id: u32,
    bo: Weak<BufferObject>,
    format: u32,
    size: (u32, u32),
}

static LIVE: IrqMutex<Vec<Live>> = IrqMutex::new(Vec::new());

pub(super) fn forget_resource(id: u32) {
    LIVE.lock().retain(|l| l.id != id);
}

/// The unbind hook needs an `Arc<dyn Unbind>`.
struct Release(Arc<Gpu>);

impl bo::Unbind for Release {
    fn unbind(&self, id: u32) {
        bo::Unbind::unbind(&*self.0, id)
    }
}

fn virtio_format(fourcc: u32) -> Option<u32> {
    match fourcc {
        fourcc::XRGB8888 => Some(proto::FORMAT_B8G8R8X8_UNORM),
        fourcc::ARGB8888 => Some(proto::FORMAT_B8G8R8A8_UNORM),
        _ => None,
    }
}

impl VirtioGpu {
    /// The host resource behind `fb`, created on first use.
    fn resource(&self, fb: &Framebuffer) -> Result<u32> {
        let id = fb.bo.handle();
        if id != 0 {
            return Ok(id);
        }
        // The host reads rows `width * 4` apart from the start of the backing.
        if fb.pitch != fb.width * 4 || fb.offset != 0 {
            return Err(Error::Unsupported);
        }
        let format = virtio_format(fb.format).ok_or(Error::Invalid)?;
        let id = self.gpu.alloc_resource_id();
        if let Err(e) = self.gpu.resource_create_2d(id, format, fb.width, fb.height) {
            self.gpu.free_resource_id(id);
            return Err(e);
        }
        if let Err(e) = self.gpu.attach_backing(id, fb.bo.chunks()) {
            let _ = self.gpu.resource_unref(id);
            self.gpu.free_resource_id(id);
            return Err(e);
        }
        fb.bo.bind(id, Arc::new(Release(self.gpu.clone())));
        LIVE.lock().push(Live { id, bo: Arc::downgrade(&fb.bo), format, size: (fb.width, fb.height) });
        Ok(id)
    }

    fn full(fb: &Framebuffer) -> Rect {
        Rect { x: 0, y: 0, w: fb.width, h: fb.height }
    }

    /// Asks the host about its displays again. Returns whether anything a
    /// client cares about changed (status or preferred size).
    pub fn refresh(&self) -> bool {
        let Ok(info) = self.gpu.get_display_info() else { return false };
        let mut list = Vec::new();
        let mut sizes = Vec::new();
        for h in &self.heads {
            let one = info[h.index as usize];
            let edid = self.gpu.get_edid(h.index).ok().filter(|e| e.len() >= 128);
            let parsed = edid.as_deref().and_then(|e| aurora_edid::parse(e).ok());
            let mut modes: Vec<Mode> = parsed
                .iter()
                .flat_map(|e| e.timings.iter())
                .filter(|t| t.hactive >= 640 && t.vactive >= 480 && !t.interlaced)
                .filter(|t| t.hactive <= MAX_SIZE.0 && t.vactive <= MAX_SIZE.1)
                .map(|t| Mode::from_timing(t).preferred(false))
                .collect();
            // Any size works on a virtual display: offer the common ones too.
            modes.extend(virtual_modes((3840, 2400)));
            // The host's current window size is what it prefers.
            let (pw, ph) = (one.rect.w, one.rect.h);
            if pw >= 320 && ph >= 200 {
                modes.retain(|m| m.size() != (pw, ph));
                modes.push(Mode::cvt(pw, ph, 60).unwrap_or(Mode::fixed(pw, ph)).preferred(true));
            }
            mode::tidy(&mut modes);
            // Like DRM: a disconnected output lists no modes.
            if !one.enabled {
                modes.clear();
            }
            let mm = parsed.as_ref().map_or((0, 0), |e| (e.width_mm, e.height_mm));
            sizes.push((pw, ph, one.enabled));
            list.push(ConnectorInfo {
                id: h.connector,
                kind: ConnectorKind::Virtual,
                kind_index: h.index + 1,
                status: if one.enabled { Status::Connected } else { Status::Disconnected },
                modes,
                edid: edid.map(Arc::new),
                mm,
                encoders: alloc::vec![h.encoder],
            });
        }
        let changed = *self.host_sizes.lock() != sizes;
        *self.connectors.lock() = list;
        *self.host_sizes.lock() = sizes;
        changed
    }

    /// Shows the committed state again (after a device reset).
    fn restore(&self, s: &State) -> Result<()> {
        for (i, h) in self.heads.iter().enumerate() {
            let c = &s.crtcs[i];
            let (p, k) = (&s.planes[2 * i], &s.planes[2 * i + 1]);
            match (&p.fb, c.active) {
                (Some(fb), true) => {
                    let id = self.resource(fb)?;
                    self.gpu.present_no_flush(id, fb.pitch, Self::full(fb))?;
                    self.gpu.set_scanout(h.index, id, src_rect(p))?;
                    self.gpu.flush(id, src_rect(p))?;
                }
                _ => self.gpu.set_scanout(h.index, 0, Rect::default())?,
            }
            if let (Some(fb), true) = (&k.fb, c.active) {
                self.show_cursor(h, k, fb)?;
            }
        }
        Ok(())
    }

    fn show_cursor(&self, h: &Head, k: &PlaneState, fb: &Framebuffer) -> Result<()> {
        let id = self.resource(fb)?;
        // The host reads the cursor image from its copy of the resource.
        self.gpu.present_no_flush(id, fb.pitch, Self::full(fb))?;
        let (x, y) = cursor_pos(k);
        self.gpu.cursor_update(h.index, x, y, id, (k.hotspot.0.max(0) as u32, k.hotspot.1.max(0) as u32))
    }
}

/// The source rectangle of a plane, in whole pixels.
fn src_rect(p: &PlaneState) -> Rect {
    Rect { x: p.src.0 >> 16, y: p.src.1 >> 16, w: p.src.2 >> 16, h: p.src.3 >> 16 }
}

/// virtio's cursor position is the pointer's: the image's corner plus the hot spot.
fn cursor_pos(k: &PlaneState) -> (i32, i32) {
    (k.dst.0 + k.hotspot.0, k.dst.1 + k.hotspot.1)
}

impl KmsDevice for VirtioGpu {
    fn name(&self) -> &str {
        "virtio-gpu"
    }

    fn caps(&self) -> Caps {
        Caps {
            live_modeset: true,
            any_size: true,
            max_size: MAX_SIZE,
            needs_dirty: true,
            cursor: Some((CURSOR_SIZE, CURSOR_SIZE)),
            vblank_irq: false,
            follows_host: true,
        }
    }

    fn crtcs(&self) -> Vec<CrtcInfo> {
        self.heads.iter().map(|h| CrtcInfo { id: h.crtc, index: h.index }).collect()
    }

    fn planes(&self) -> Vec<PlaneInfo> {
        let mut out = Vec::new();
        for h in &self.heads {
            out.push(PlaneInfo {
                id: h.primary,
                kind: PlaneKind::Primary,
                possible_crtcs: 1 << h.index,
                formats: PRIMARY_FORMATS,
                max_size: MAX_SIZE,
            });
            out.push(PlaneInfo {
                id: h.cursor,
                kind: PlaneKind::Cursor,
                possible_crtcs: 1 << h.index,
                formats: CURSOR_FORMATS,
                max_size: (CURSOR_SIZE, CURSOR_SIZE),
            });
        }
        out
    }

    fn encoders(&self) -> Vec<EncoderInfo> {
        self.heads
            .iter()
            .map(|h| EncoderInfo { id: h.encoder, kind: EncoderKind::Virtual, possible_crtcs: 1 << h.index })
            .collect()
    }

    fn connectors(&self) -> Vec<ConnectorInfo> {
        self.connectors.lock().clone()
    }

    fn probe(&self) -> Vec<ConnectorInfo> {
        self.refresh();
        self.connectors()
    }

    fn state(&self) -> (Vec<CrtcState>, Vec<PlaneState>) {
        let s = self.state.lock();
        (s.crtcs.clone(), s.planes.clone())
    }

    fn atomic_check(&self, c: &Commit) -> Result<()> {
        let current = self.state();
        let (cs, ps, _) = validate(self, current, c)?;
        // A CRTC drives the connector of its own head.
        for (i, s) in cs.iter().enumerate() {
            if s.active && s.connectors != [self.heads[i].connector] {
                return Err(Error::Invalid);
            }
        }
        for p in &ps {
            if let Some(fb) = &p.fb {
                if fb.pitch != fb.width * 4 || fb.offset != 0 {
                    return Err(Error::Unsupported);
                }
            }
        }
        Ok(())
    }

    fn atomic_commit(&self, c: &Commit) -> Result<()> {
        self.atomic_check(c)?;
        if c.flags & commit::TEST_ONLY != 0 {
            return Ok(());
        }
        let mut s = self.state.lock();
        let (cs, ps, _) = validate(self, (s.crtcs.clone(), s.planes.clone()), c)?;
        // Host resources first, so a failure changes nothing.
        for p in &ps {
            if let Some(fb) = &p.fb {
                self.resource(fb)?;
            }
        }
        let mut touched = Vec::new();
        for (i, h) in self.heads.iter().enumerate() {
            let (old_c, new_c) = (&s.crtcs[i], &cs[i]);
            let (old_p, new_p) = (&s.planes[2 * i], &ps[2 * i]);
            let (old_k, new_k) = (&s.planes[2 * i + 1], &ps[2 * i + 1]);
            let crtc_changed = old_c != new_c;
            if crtc_changed || !new_p.same_image(old_p) {
                match (&new_p.fb, new_c.active) {
                    (Some(fb), true) => {
                        let id = self.resource(fb)?;
                        // The new picture reaches the host before it is shown.
                        self.gpu.present_no_flush(id, fb.pitch, Self::full(fb))?;
                        self.gpu.set_scanout(h.index, id, src_rect(new_p))?;
                        self.gpu.flush(id, src_rect(new_p))?;
                    }
                    _ => self.gpu.set_scanout(h.index, 0, Rect::default())?,
                }
                touched.push(i);
            }
            let cursor_on = new_c.active && new_k.fb.is_some();
            let was_on = old_c.active && old_k.fb.is_some();
            if cursor_on && (!new_k.same_image(old_k) || crtc_changed || !was_on) {
                self.show_cursor(h, new_k, new_k.fb.as_ref().unwrap())?;
            } else if cursor_on && new_k.dst != old_k.dst {
                let (x, y) = cursor_pos(new_k);
                self.gpu.cursor_move(h.index, x, y)?;
            } else if !cursor_on && was_on {
                self.gpu.cursor_update(h.index, 0, 0, 0, (0, 0))?;
            }
        }
        // Replaced framebuffers are released (and their host resources
        // unreferenced) as `old` drops, after the new ones are on screen.
        let old = core::mem::replace(&mut *s, State { crtcs: cs, planes: ps });
        drop(s);
        drop(old);
        let now = crate::time::now_ns();
        for i in touched {
            let vb = {
                let mut v = self.vblank.lock();
                v[i].seq += 1;
                v[i].time_ns = now;
                v[i]
            };
            if c.flags & commit::PAGE_FLIP_EVENT != 0 {
                post(Event::Flip {
                    crtc: self.heads[i].crtc,
                    seq: vb.seq,
                    time_ns: vb.time_ns,
                    user_data: c.user_data,
                });
            }
        }
        Ok(())
    }

    fn dirty(&self, fb: &Arc<Framebuffer>, rects: &[Damage]) -> Result<()> {
        let id = fb.bo.handle();
        if id == 0 {
            return Ok(()); // never shown, nothing to update
        }
        let rects: Vec<Rect> = rects
            .iter()
            .map(|r| r.clamp(fb.width, fb.height))
            .filter(|r| !r.is_empty())
            .map(|r| Rect { x: r.x, y: r.y, w: r.w, h: r.h })
            .collect();
        self.gpu.present(id, fb.pitch, &rects)?;
        // Without vblank interrupts a frame counts when the host has it.
        let s = self.active_heads();
        let now = crate::time::now_ns();
        let mut v = self.vblank.lock();
        for i in s.iter().copied() {
            v[i].seq += 1;
            v[i].time_ns = now;
        }
        Ok(())
    }

    fn vblank(&self, crtc: ObjId) -> Vblank {
        let i = self.heads.iter().position(|h| h.crtc == crtc).unwrap_or(0);
        self.vblank.lock()[i]
    }

    fn resume(&self) {
        // The device was reset by sleep: host resources are gone, the
        // guest memory behind them is not. Create them again with the
        // same ids, then show the committed state.
        if !self.gpu.reinit() {
            log!("virtio-gpu", "the device did not come back after sleep");
            return;
        }
        // (The buffers are held while their resources are recreated.)
        let list: Vec<Live> =
            LIVE.lock().iter().map(|l| Live { id: l.id, bo: l.bo.clone(), format: l.format, size: l.size }).collect();
        let live: Vec<(Arc<BufferObject>, &Live)> = list.iter().filter_map(|l| Some((l.bo.upgrade()?, l))).collect();
        for (bo, l) in &live {
            let ok = self.gpu.resource_create_2d(l.id, l.format, l.size.0, l.size.1).is_ok()
                && self.gpu.attach_backing(l.id, bo.chunks()).is_ok();
            if !ok {
                log!("virtio-gpu", "could not recreate resource {} after sleep", l.id);
            }
        }
        drop(live);
        let s = self.state.lock();
        match self.restore(&s) {
            Ok(()) => log!("virtio-gpu", "display restored after sleep"),
            Err(e) => log!("virtio-gpu", "could not restore the display after sleep: {:?}", e),
        }
    }
}

impl VirtioGpu {
    /// The heads whose primary plane is on (for the frame counter).
    fn active_heads(&self) -> Vec<usize> {
        let s = self.state.lock();
        (0..s.crtcs.len()).filter(|&i| s.crtcs[i].active && s.planes[2 * i].fb.is_some()).collect()
    }
}

/// The `virtio-gpu` task: reads device events (display changes) and
/// probes the heads again.
fn events(dev: Arc<VirtioGpu>) {
    loop {
        let gpu = &dev.gpu;
        // With interrupts the handler flags events; without, poll twice a second.
        let wait = if gpu.has_irq() { 5000 } else { 500 };
        gpu.events.wait(wait, || gpu.event_pending.load(core::sync::atomic::Ordering::Acquire));
        gpu.event_pending.store(false, core::sync::atomic::Ordering::Release);
        let ev = gpu.read_events();
        if ev == 0 {
            continue;
        }
        gpu.clear_events(ev);
        if ev & proto::EVENT_DISPLAY != 0 && dev.refresh() {
            let c = dev.connectors();
            let first = c.first().and_then(|k| k.preferred()).map(|m| m.size()).unwrap_or((0, 0));
            log!("virtio-gpu", "displays changed; head 1 prefers {}x{}", first.0, first.1);
            super::hotplug();
        }
    }
}

static DEVICE: spin::Once<Arc<VirtioGpu>> = spin::Once::new();

/// The native driver's device, if it bound one (for the self-tests).
pub fn device() -> Option<Arc<VirtioGpu>> {
    DEVICE.get().cloned()
}

impl VirtioGpu {
    /// The command layer (self-tests, and later the 3D code).
    pub fn gpu(&self) -> &Arc<Gpu> {
        &self.gpu
    }
}

/// Binds the first virtio GPU (virtio-vga or virtio-gpu-pci). The screen
/// keeps showing the firmware's picture (the VGA side of virtio-vga)
/// until the first commit.
pub fn probe() -> bool {
    let Some(d) = crate::drivers::pci::devices().into_iter().find(|d| d.vendor == 0x1AF4 && d.device == 0x1050) else {
        return false;
    };
    let Some(gpu) = Gpu::new(&d) else {
        log!("virtio-gpu", "{:02x}:{:02x}.{}: could not set up the device", d.bus, d.dev, d.func);
        return false;
    };
    let heads: Vec<Head> = (0..gpu.num_scanouts)
        .map(|index| Head {
            index,
            crtc: new_id(),
            primary: new_id(),
            cursor: new_id(),
            encoder: new_id(),
            connector: new_id(),
        })
        .collect();
    let state = State {
        crtcs: heads
            .iter()
            .map(|h| CrtcState { crtc: h.crtc, active: false, mode: None, connectors: Vec::new() })
            .collect(),
        planes: heads.iter().flat_map(|h| [PlaneState::disabled(h.primary), PlaneState::disabled(h.cursor)]).collect(),
    };
    let n = heads.len();
    let dev = Arc::new(VirtioGpu {
        gpu,
        heads,
        connectors: IrqMutex::new(Vec::new()),
        host_sizes: IrqMutex::new(Vec::new()),
        state: Mutex::new(state),
        vblank: IrqMutex::new(alloc::vec![Vblank::default(); n]),
    });
    dev.refresh();
    let g = &dev.gpu;
    let connected = dev.connectors().iter().filter(|c| c.status == Status::Connected).count();
    log!(
        "virtio-gpu",
        "native driver: {} scanout(s), {} connected{}{}",
        n,
        connected,
        if g.features & proto::F_EDID != 0 { ", EDID" } else { "" },
        if g.has_irq() { ", interrupts via MSI-X" } else { ", polled" }
    );
    for c in dev.connectors() {
        match c.preferred() {
            Some(m) => log!(
                "virtio-gpu",
                "{}: {:?}, prefers {}x{}, {} modes",
                c.name(),
                c.status,
                m.hdisplay,
                m.vdisplay,
                c.modes.len()
            ),
            None => log!("virtio-gpu", "{}: {:?}", c.name(), c.status),
        }
    }
    DEVICE.call_once(|| dev.clone());
    crate::sched::spawn("virtio-gpu", || events(DEVICE.get().unwrap().clone()));
    super::register(dev);
    true
}
