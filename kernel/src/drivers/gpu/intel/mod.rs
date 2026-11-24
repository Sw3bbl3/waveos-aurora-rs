//! Intel integrated graphics (Skylake to Raptor Lake, display versions
//! 9–13): a native display driver on the display core.
//!
//! By default it **takes over the firmware's mode**: it reads back the
//! pipes the GOP lit (transcoder, timings, DDI port, plane 1) with
//! `aurora_igfx::takeover`, and if everything is as expected keeps them
//! running exactly as they are. What changes is what plane 1 scans:
//! framebuffers in ordinary RAM, mapped into the global GTT, flipped with
//! `PLANE_SURF` (completion read from `PLANE_SURFLIVE`, the frame counter
//! for vblank sequence numbers). The cursor plane gets a slice of the
//! display buffer carved off plane 1's and watermarks derived from plane
//! 1's (`aurora_igfx::ddb`). Connectors are found over GMBUS and DP AUX
//! with their EDID, and probed again when the display interrupt registers
//! latch a hotplug (and every few seconds). The internal panel's PWM
//! backlight is offered to the brightness keys.
//!
//! The display engine does not snoop the CPU caches, so presenting is a
//! cache flush of the damaged rows (`clflush`), not a copy. Framebuffers
//! whose pitch is not a multiple of 64 bytes (1366 pixels wide, say) are
//! copied to a padded scanout buffer instead.
//!
//! `intel=modeset` in boot.conf also allows mode changes on Skylake-class
//! displays, within the limits of `aurora_igfx::modeset` (a failed change
//! puts the previous mode back); `intel=off` leaves the firmware
//! framebuffer alone. Anything unexpected while probing declines the
//! takeover, and the firmware framebuffer stays in charge. None of this
//! has run on real hardware yet: see docs/developer/intel-display.md.

pub mod hw;

use super::*;
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use aurora_igfx::backlight::Backlight as Pwm;
use aurora_igfx::ddb::{self, Ddb, Wm};
use aurora_igfx::gtt::{self, Space, Table};
use aurora_igfx::platform::{self, Info};
use aurora_igfx::regs;
use aurora_igfx::takeover::{self, Output as Out};
use aurora_igfx::{aux, gmbus, modeset, plane, Mmio};

/// Register and GGTT access: a real BAR, or the simulated engine in tests.
pub trait Hw: Mmio + Table + Send + Sync + 'static {}
impl<T: Mmio + Table + Send + Sync + 'static> Hw for T {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Policy {
    /// Keep the firmware's modes; flip planes only (the default).
    Takeover,
    /// Also allow mode changes (`intel=modeset`; Skylake-class only).
    Modeset,
}

const CURSOR: u32 = 64;
const CURSOR_BYTES: usize = (CURSOR * CURSOR * 4) as usize;
const PRIMARY_FORMATS: &[u32] = &[fourcc::XRGB8888, fourcc::ARGB8888];
const CURSOR_FORMATS: &[u32] = &[fourcc::ARGB8888];

// ------------------------------------------------------------------ GGTT

/// The driver's part of the global GTT.
pub struct Ggtt<H: Hw> {
    hw: Arc<H>,
    space: IrqMutex<Space>,
    /// Mapping sizes by handle (GGTT address >> 12).
    sizes: IrqMutex<BTreeMap<u32, u64>>,
    /// A page every unused entry of ours points at.
    scratch: u64,
}

impl<H: Hw> Ggtt<H> {
    fn point(&self, at: u64, pages: impl Iterator<Item = u64>) {
        for (i, phys) in (at / gtt::PAGE..).zip(pages) {
            self.hw.set(i, gtt::pte(phys));
        }
    }

    /// Maps `bo` (once; later calls return the same address).
    fn bind(self: &Arc<Self>, bo: &BufferObject, align: u64) -> Result<u64> {
        let h = bo.handle();
        if h != 0 {
            return Ok((h as u64) << 12);
        }
        let size = bo.size as u64;
        let at = self.space.lock().alloc(size, align).ok_or(Error::NoMemory)?;
        let pages = bo.chunks().iter().flat_map(|&(p, n)| (0..n as u64 / gtt::PAGE).map(move |k| p + k * gtt::PAGE));
        self.point(at, pages);
        self.point(at + size, (0..gtt::GUARD_PAGES).map(|_| self.scratch));
        self.hw.flush();
        let handle = (at >> 12) as u32;
        self.sizes.lock().insert(handle, size);
        bo.bind(handle, Arc::new(Release(self.clone())));
        Ok(at)
    }

    fn unbind(&self, handle: u32) {
        let Some(size) = self.sizes.lock().remove(&handle) else { return };
        let at = (handle as u64) << 12;
        self.point(at, (0..size / gtt::PAGE + gtt::GUARD_PAGES).map(|_| self.scratch));
        self.hw.flush();
        self.space.lock().free(at, size);
    }
}

impl<H: Hw> Drop for Ggtt<H> {
    fn drop(&mut self) {
        // Only the self-test's devices go away; a real one lives for good.
        crate::mm::frame::free(self.scratch);
    }
}

struct Release<H: Hw>(Arc<Ggtt<H>>);

impl<H: Hw> bo::Unbind for Release<H> {
    fn unbind(&self, handle: u32) {
        self.0.unbind(handle)
    }
}

/// Writes the CPU caches covering `len` bytes at `va` back to memory.
fn flush_cache(va: u64, len: u64) {
    let mut p = va & !63;
    while p < va + len {
        crate::arch::cache::flush_line(p as *const u8);
        p += 64;
    }
}

fn fence() {
    crate::arch::cache::full_fence();
}

// ----------------------------------------------------------- the device

/// A pipe the firmware lit.
struct Lit {
    fw: takeover::Pipe,
    mode: Mode,
    clock_khz: u32,
    /// The cursor's display buffer and watermarks are set up.
    cursor_ok: bool,
    /// Two 64×64 cursor images (written alternately), and their GGTT address.
    cursor_bo: Arc<BufferObject>,
    cursor_gtt: u64,
    /// `modeset` only: the firmware's configuration.
    fw_config: Option<modeset::Config>,
}

struct Head {
    pipe: u8,
    crtc: ObjId,
    primary: ObjId,
    cursor: ObjId,
    lit: Option<Lit>,
}

/// One DDI port with a connector.
struct Port {
    port: u8,
    connector: ObjId,
    encoder: ObjId,
    /// The head the firmware drives it from.
    head: Option<usize>,
    edp: bool,
}

/// What a primary plane scans.
struct Scan {
    fb: Arc<Framebuffer>,
    /// A padded copy, when `fb`'s pitch doesn't suit the plane.
    shadow: Option<Arc<BufferObject>>,
    gtt: u64,
    pitch: u32,
}

impl Scan {
    /// The buffer the plane reads, its kernel address and pitch.
    fn target(&self) -> (u64, u32) {
        match &self.shadow {
            Some(s) => (s.vaddr(), self.pitch),
            None => (self.fb.bo.vaddr() + self.fb.offset as u64, self.fb.pitch),
        }
    }

    /// Copies (with a shadow) and flushes rectangle `r` to memory.
    fn show(&self, r: Damage) {
        let r = r.clamp(self.fb.width, self.fb.height);
        if r.is_empty() {
            return;
        }
        let (va, pitch) = self.target();
        for y in r.y..r.y + r.h {
            let dst = va + y as u64 * pitch as u64 + r.x as u64 * 4;
            if self.shadow.is_some() {
                let src = self.fb.bo.vaddr() + self.fb.offset as u64 + y as u64 * self.fb.pitch as u64 + r.x as u64 * 4;
                unsafe { core::ptr::copy_nonoverlapping(src as *const u8, dst as *mut u8, r.w as usize * 4) };
            }
            flush_cache(dst, r.w as u64 * 4);
        }
        fence();
    }
}

struct State {
    crtcs: Vec<CrtcState>,
    /// Primary then cursor plane of each head.
    planes: Vec<PlaneState>,
    scan: Vec<Option<Scan>>,
    cursor_slot: Vec<usize>,
    configs: Vec<Option<modeset::Config>>,
}

pub struct Intel<H: Hw> {
    hw: Arc<H>,
    pub info: Info,
    policy: Policy,
    ggtt: Arc<Ggtt<H>>,
    heads: Vec<Head>,
    ports: Vec<Port>,
    connectors: IrqMutex<Vec<ConnectorInfo>>,
    state: Mutex<State>,
    backlight: Option<Pwm>,
    max_size: (u32, u32),
}

/// Why the driver declined.
#[derive(Debug)]
pub enum Declined {
    Firmware(takeover::Decline),
    NoGgtt,
    NoMemory,
}

impl core::fmt::Display for Declined {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Declined::Firmware(d) => write!(f, "{d}"),
            Declined::NoGgtt => write!(f, "no global GTT space above the firmware's"),
            Declined::NoMemory => write!(f, "out of memory"),
        }
    }
}

/// The DRM mode of a taken-over pipe: its timings, or (when the firmware
/// scales a smaller source to the panel) the source size with the same
/// blanking.
fn mode_of(p: &takeover::Pipe, clock_khz: u32) -> Mode {
    let t = &p.timings;
    let (w, h) = p.src;
    let mut flags = if t.hsync_positive { mode::flag::PHSYNC } else { mode::flag::NHSYNC };
    flags |= if t.vsync_positive { mode::flag::PVSYNC } else { mode::flag::NVSYNC };
    let (dw, dh) = (w as i64 - t.hactive as i64, h as i64 - t.vactive as i64);
    let sh = |v: u32| (v as i64 + dw).max(w as i64) as u32;
    let sv = |v: u32| (v as i64 + dh).max(h as i64) as u32;
    let frame = t.htotal as u64 * t.vtotal as u64;
    Mode {
        clock: clock_khz,
        hdisplay: w,
        hsync_start: sh(t.hsync_start),
        hsync_end: sh(t.hsync_end),
        htotal: sh(t.htotal),
        vdisplay: h,
        vsync_start: sv(t.vsync_start),
        vsync_end: sv(t.vsync_end),
        vtotal: sv(t.vtotal),
        vrefresh: (clock_khz as u64 * 1000 + frame / 2).checked_div(frame).unwrap_or(60) as u32,
        flags,
        kind: mode::kind::DRIVER | mode::kind::PREFERRED,
    }
}

/// Reads an EDID (base block and up to three extensions) block by block.
fn read_edid(mut block: impl FnMut(u8, &mut [u8; 128]) -> bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut b = [0u8; 128];
    if !block(0, &mut b) {
        return None;
    }
    out.extend_from_slice(&b);
    let ext = b[126].min(3);
    for i in 1..=ext {
        if !block(i, &mut b) {
            break;
        }
        out.extend_from_slice(&b);
    }
    // Keep what parses (a bad extension shouldn't lose the base block).
    if aurora_edid::parse(&out).is_err() {
        out.truncate(128);
        out[126] = 0;
        let sum = out[..127].iter().fold(0u8, |a, x| a.wrapping_add(*x));
        out[127] = 0u8.wrapping_sub(sum);
        aurora_edid::parse(&out).ok()?;
    }
    Some(out)
}

impl<H: Hw> Intel<H> {
    /// Takes the display over from the firmware, or declines without
    /// having changed anything. `ggc` is the GPU's `GGC` configuration
    /// word (GGTT and stolen memory sizes); `hw_cursor` false keeps the
    /// display buffer as the firmware left it (no cursor plane).
    pub fn new(
        hw: Arc<H>,
        info: Info,
        policy: Policy,
        ggc: u16,
        hw_cursor: bool,
    ) -> core::result::Result<Self, Declined> {
        let m = &*hw;
        // 1. Read; decide. Nothing is written before this succeeds.
        let fw = takeover::read(m, &info).map_err(Declined::Firmware)?;
        let ggtt_size = gtt::ggtt_size(ggc).min(4 << 30);
        let stolen = gtt::stolen_size(ggc);
        // The firmware's mappings: its stolen memory at the bottom, its
        // framebuffers wherever plane 1 points, and anything else a coarse
        // scan of the table (one entry per MiB) finds in use.
        let mut reserved = stolen;
        for p in &fw.pipes {
            reserved = reserved.max(p.primary.surf as u64 + p.primary.stride_bytes as u64 * p.src.1 as u64);
        }
        // (Firmware that points every unused entry at one scratch page of
        // its own makes that page the most common one: those entries are
        // free.)
        let samples: Vec<(u64, Option<u64>)> =
            (0..ggtt_size >> 20).map(|mib| (mib << 20, gtt::pte_page(hw.get((mib << 20) / gtt::PAGE)))).collect();
        let mut counts: BTreeMap<u64, u32> = BTreeMap::new();
        for page in samples.iter().filter_map(|s| s.1) {
            *counts.entry(page).or_insert(0) += 1;
        }
        let filler = counts.iter().max_by_key(|c| *c.1).filter(|c| *c.1 > 16).map(|c| *c.0);
        for (at, page) in samples {
            if page.is_some() && page != filler {
                reserved = reserved.max(at + (1 << 20));
            }
        }
        let start = reserved.next_multiple_of(16 << 20);
        if ggtt_size < start + (64 << 20) {
            return Err(Declined::NoGgtt);
        }
        let scratch = crate::mm::frame::alloc().ok_or(Declined::NoMemory)?;
        unsafe { core::ptr::write_bytes(crate::mm::phys_to_virt(scratch) as *mut u8, 0, 4096) };
        log!(
            "intel",
            "GGTT {} MiB, stolen memory {} MiB, the firmware's mappings end at {} MiB; using {}–{} MiB",
            ggtt_size >> 20,
            stolen >> 20,
            reserved >> 20,
            start >> 20,
            ggtt_size >> 20
        );
        let ggtt = Arc::new(Ggtt {
            hw: hw.clone(),
            space: IrqMutex::new(Space::new(start, ggtt_size)),
            sizes: IrqMutex::new(BTreeMap::new()),
            scratch,
        });

        // 2. Heads and their modes.
        let mut heads: Vec<Head> = (0..info.pipes)
            .map(|pipe| Head { pipe, crtc: new_id(), primary: new_id(), cursor: new_id(), lit: None })
            .collect();
        let mut ports: Vec<Port> = Vec::new();
        for p in &fw.pipes {
            let edid = Self::edid_of(m, &info, p.port, matches!(p.output, Out::Dp | Out::Edp), p.output != Out::Edp);
            let clock = Self::firmware_clock(m, &info, p, edid.as_deref());
            let mode = mode_of(p, clock);
            log!(
                "intel",
                "pipe {} (transcoder {}): {:?} on DDI {}, {}x{} at {} Hz ({} kHz){}{}, {} bpc",
                takeover::pipe_name(p.pipe),
                p.trans.name(),
                p.output,
                platform::port_name(p.port),
                mode.hdisplay,
                mode.vdisplay,
                mode.vrefresh,
                clock,
                if p.scaled { ", scaled to the panel" } else { "" },
                if matches!(p.output, Out::Dp | Out::Edp) {
                    alloc::format!(", {} lanes", p.lanes)
                } else {
                    String::new()
                },
                p.bpc
            );
            let cursor_bo = BufferObject::alloc_system(2 * CURSOR_BYTES).map_err(|_| Declined::NoMemory)?;
            let fw_config = match policy {
                Policy::Modeset => match modeset::snapshot(m, &info, p, clock) {
                    Ok(c) => Some(c),
                    Err(e) => {
                        log!("intel", "pipe {}: no mode changes ({:?})", takeover::pipe_name(p.pipe), e);
                        None
                    }
                },
                Policy::Takeover => None,
            };
            let head = p.pipe as usize;
            heads[head].lit =
                Some(Lit { fw: *p, mode, clock_khz: clock, cursor_ok: false, cursor_bo, cursor_gtt: 0, fw_config });
            ports.push(Port {
                port: p.port,
                connector: new_id(),
                encoder: new_id(),
                head: Some(head),
                edp: p.output == Out::Edp,
            });
        }
        // Other ports worth probing (not DDI A, the internal panel's, when it's dark).
        for &port in info.probe_ports() {
            if port != 0 && !ports.iter().any(|p| p.port == port) {
                ports.push(Port { port, connector: new_id(), encoder: new_id(), head: None, edp: false });
            }
        }

        // 3. Take over: map the cursor images, latch interrupts, carve the cursor's share.
        for h in heads.iter_mut() {
            let Some(l) = &mut h.lit else { continue };
            l.cursor_gtt = ggtt.bind(&l.cursor_bo, regs::SURF_ALIGN).map_err(|_| Declined::NoGgtt)?;
            // Latch vblank and flip-done in the pipe's IIR (no interrupt is
            // enabled: IER stays as it is and nothing routes one to a CPU).
            m.modify(regs::de_pipe_imr(h.pipe), regs::PIPE_VBLANK | regs::PIPE_PLANE1_FLIP_DONE, 0);
            if hw_cursor {
                l.cursor_ok = Self::setup_cursor(m, &info, h.pipe, &l.fw, l.cursor_gtt);
            }
        }
        // Hotplug: let the identity registers latch (interrupts stay off).
        m.write(regs::SDE_IMR, 0);
        let backlight =
            fw.pipes.iter().any(|p| p.output == Out::Edp).then(|| Pwm::probe(m, info.backlight())).flatten();
        match &backlight {
            Some(b) => log!(
                "intel",
                "backlight: PWM period {}{}, at {}%",
                b.period,
                if b.inverted { " (inverted)" } else { "" },
                b.get(m)
            ),
            None if fw.panel_on => log!("intel", "backlight: no enabled PWM (DPCD-controlled panels aren't supported)"),
            None => {}
        }
        let max_size = fw.pipes.iter().fold((0, 0), |a, p| (a.0.max(p.src.0), a.1.max(p.src.1)));
        let state = State {
            crtcs: heads
                .iter()
                .map(|h| CrtcState {
                    crtc: h.crtc,
                    active: h.lit.is_some(),
                    mode: h.lit.as_ref().map(|l| l.mode),
                    connectors: ports.iter().filter(|p| p.head == Some(h.pipe as usize)).map(|p| p.connector).collect(),
                })
                .collect(),
            planes: heads
                .iter()
                .flat_map(|h| [PlaneState::disabled(h.primary), PlaneState::disabled(h.cursor)])
                .collect(),
            scan: heads.iter().map(|_| None).collect(),
            cursor_slot: alloc::vec![0; heads.len()],
            configs: heads.iter().map(|h| h.lit.as_ref().and_then(|l| l.fw_config)).collect(),
        };
        let dev = Intel {
            hw,
            info,
            policy,
            ggtt,
            heads,
            ports,
            connectors: IrqMutex::new(Vec::new()),
            state: Mutex::new(state),
            backlight,
            max_size,
        };
        dev.refresh();
        Ok(dev)
    }

    /// The firmware's pixel clock: read back from the PLL where possible,
    /// else the EDID's timing with the same totals, else measured from the
    /// frame counter, else assumed 60 Hz.
    fn firmware_clock(m: &H, info: &Info, p: &takeover::Pipe, edid: Option<&[u8]>) -> u32 {
        if let Some(c) = takeover::pixel_clock(m, info, p) {
            return c;
        }
        let t = &p.timings;
        if let Some(e) = edid.and_then(|e| aurora_edid::parse(e).ok()) {
            let same = |x: &&aurora_edid::Timing| {
                (x.hactive, x.htotal, x.vactive, x.vtotal) == (t.hactive, t.htotal, t.vactive, t.vtotal)
            };
            if let Some(x) = e.timings.iter().find(same) {
                return x.clock_khz;
            }
        }
        let frame = t.htotal as u64 * t.vtotal as u64;
        // Six frames between two counter edges.
        let count = || m.read(regs::frame_count(p.pipe));
        let edge = |from: u32| {
            for _ in 0..100 {
                if count() != from {
                    return true;
                }
                m.delay_us(1000);
            }
            false
        };
        let f0 = count();
        if edge(f0) {
            let t0 = crate::time::now_ns();
            let f1 = count();
            let mut ok = true;
            for k in 0..6 {
                ok &= edge(f1.wrapping_add(k));
            }
            let dt = crate::time::now_ns() - t0;
            let frames = count().wrapping_sub(f1) as u64;
            let hz_milli = (frames * 1_000_000_000_000).checked_div(dt).unwrap_or(0);
            if ok && (20_000..=250_000).contains(&hz_milli) {
                return (frame * hz_milli / 1_000_000) as u32;
            }
        }
        (frame * 60 / 1000) as u32
    }

    /// Carves the cursor's display buffer share off plane 1's (see
    /// `aurora_igfx::ddb`), plane 1's change first, a vblank apart.
    fn setup_cursor(m: &H, info: &Info, pipe: u8, fw: &takeover::Pipe, cursor_gtt: u64) -> bool {
        let name = takeover::pipe_name(pipe);
        if fw.cursor_on || Ddb::decode(m.read(regs::cur_buf_cfg(pipe))).size() > 0 {
            log!("intel", "pipe {}: the cursor plane already has display buffer space", name);
            return true;
        }
        let plan = match ddb::plan_cursor(fw.primary.buf_cfg, fw.primary.wm, fw.primary.wm_sagv) {
            Ok(p) => p,
            Err(e) => {
                log!("intel", "pipe {}: no cursor plane ({:?}); the pointer is drawn in software", name, e);
                return false;
            }
        };
        let wait_vblank = || {
            let f = m.read(regs::frame_count(pipe));
            m.wait_for(regs::frame_count(pipe), u32::MAX, f.wrapping_add(1), 100_000)
                || m.read(regs::frame_count(pipe)) != f
        };
        m.write(regs::plane_buf_cfg(pipe, 0), plan.plane_ddb.encode());
        for (l, w) in plan.plane_wm.iter().enumerate() {
            m.write(regs::plane_wm(pipe, 0, l as u8), w.encode());
        }
        if Wm::decode(m.read(regs::plane_wm_trans(pipe, 0))).blocks >= plan.plane_ddb.size() {
            m.write(regs::plane_wm_trans(pipe, 0), 0);
        }
        m.write(regs::plane_surf(pipe, 0), m.read(regs::plane_surf(pipe, 0)));
        if !wait_vblank() {
            log!("intel", "pipe {}: no vblank after shrinking plane 1's buffer", name);
        }
        m.write(regs::cur_buf_cfg(pipe), plan.cursor_ddb.encode());
        m.write(regs::cur_wm(pipe, 0), plan.cursor_wm0.encode());
        for l in 1..8 {
            m.write(regs::cur_wm(pipe, l), 0);
        }
        m.write(regs::cur_wm_trans(pipe), 0);
        if info.display >= 12 {
            m.write(regs::cur_wm_sagv(pipe), plan.cursor_sagv.map_or(0, |w| w.encode()));
        }
        m.write(regs::curcntr(pipe), 0);
        m.write(regs::curbase(pipe), cursor_gtt as u32);
        wait_vblank();
        log!(
            "intel",
            "pipe {}: cursor plane with display buffer blocks {}–{} (plane 1 keeps {}–{}), level 0 at {} lines / {} blocks",
            name,
            plan.cursor_ddb.start,
            plan.cursor_ddb.end,
            plan.plane_ddb.start,
            plan.plane_ddb.end,
            plan.cursor_wm0.lines,
            plan.cursor_wm0.blocks
        );
        true
    }

    /// A port's EDID: over AUX (DP and eDP) if `aux`, else (or then, if
    /// `ddc`) over GMBUS (HDMI, DVI).
    fn edid_of(m: &H, info: &Info, port: u8, aux: bool, ddc: bool) -> Option<Vec<u8>> {
        if aux {
            let ch = aux::AuxCh { mmio: m, ch: info.aux_ch(port) };
            if let Some(e) = read_edid(|b, out| ch.read_edid_block(b, out).is_ok()) {
                return Some(e);
            }
        }
        if !ddc {
            return None;
        }
        let pin = info.gmbus_pin(port)?;
        let g = gmbus::Gmbus { mmio: m, pin };
        read_edid(|b, out| g.read_edid_block(b, out).is_ok())
    }

    /// Detects every port again. Returns whether anything changed.
    pub fn refresh(&self) -> bool {
        let m = &*self.hw;
        let old = self.connectors.lock().clone();
        let mut list = Vec::new();
        for p in &self.ports {
            let prev = old.iter().find(|c| c.id == p.connector);
            let lit = p.head.and_then(|h| self.heads[h].lit.as_ref());
            // DP sinks answer on AUX; HDMI and DVI only on DDC. The internal
            // panel's EDID is read once.
            let dp = !p.edp && aux::AuxCh { mmio: m, ch: self.info.aux_ch(p.port) }.read_byte(0).is_ok();
            let edid = match prev.and_then(|c| c.edid.as_ref()) {
                Some(e) if p.edp => Some(e.as_ref().clone()),
                _ => Self::edid_of(m, &self.info, p.port, p.edp || dp, !p.edp && !dp),
            };
            let kind = match (p.edp, dp, &edid, lit.map(|l| l.fw.output)) {
                (true, ..) => ConnectorKind::Edp,
                (_, true, ..) => ConnectorKind::DisplayPort,
                (_, _, _, Some(Out::Dp)) => ConnectorKind::DisplayPort,
                (_, _, _, Some(Out::Dvi)) => ConnectorKind::DviD,
                (_, _, Some(_), _) | (_, _, _, Some(Out::Hdmi)) => ConnectorKind::HdmiA,
                _ => prev.map_or(ConnectorKind::Unknown, |c| c.kind),
            };
            let connected = lit.is_some() || dp || edid.is_some();
            let parsed = edid.as_deref().and_then(|e| aurora_edid::parse(e).ok());
            let mut modes: Vec<Mode> = match lit {
                Some(l) => alloc::vec![l.mode],
                None => Vec::new(),
            };
            // A lit pipe lists the modes it can switch to (with `intel=modeset`);
            // others list the EDID's (they can't be lit yet).
            let head = p.head.unwrap_or(0);
            let modeset = lit.is_some() && self.can_modeset(head);
            let sink = (modeset && dp).then(|| modeset::sink_caps(m, self.info.aux_ch(p.port)).ok()).flatten();
            if lit.is_none() || modeset {
                for t in parsed.iter().flat_map(|e| e.timings.iter()) {
                    if t.interlaced || t.hactive < 640 || t.vactive < 480 {
                        continue;
                    }
                    let mode = Mode::from_timing(t).preferred(lit.is_none() && t.preferred);
                    if lit.is_none() || self.plan_with(head, &mode, sink.as_ref()).is_ok() {
                        modes.push(mode);
                    }
                }
            }
            mode::tidy(&mut modes);
            if !connected {
                modes.clear();
            }
            list.push(ConnectorInfo {
                id: p.connector,
                kind,
                kind_index: 0,
                status: if connected { Status::Connected } else { Status::Disconnected },
                modes,
                edid: edid.map(Arc::new),
                mm: parsed.as_ref().map_or((0, 0), |e| (e.width_mm, e.height_mm)),
                encoders: alloc::vec![p.encoder],
            });
        }
        // "DP-1", "DP-2", … in port order.
        for i in 0..list.len() {
            let k = list[i].kind;
            list[i].kind_index = list[..=i].iter().filter(|c| c.kind == k).count() as u32;
        }
        let changed = old.len() != list.len()
            || old.iter().zip(&list).any(|(a, b)| a.status != b.status || a.edid != b.edid || a.kind != b.kind);
        *self.connectors.lock() = list;
        changed
    }

    fn can_modeset(&self, head: usize) -> bool {
        self.policy == Policy::Modeset
            && self.heads[head].lit.as_ref().is_some_and(|l| l.fw_config.is_some() && l.fw.output != Out::Edp)
    }

    /// The configuration that shows `m` on `head`, if the mode set code allows it.
    fn plan_for(&self, head: usize, m: &Mode) -> Result<modeset::Config> {
        let l = self.heads[head].lit.as_ref().ok_or(Error::Unsupported)?;
        let base = l.fw_config.as_ref().ok_or(Error::Unsupported)?;
        let sink = match base.link {
            Some(_) => Some(modeset::sink_caps(&*self.hw, self.info.aux_ch(base.port)).map_err(|_| Error::Io)?),
            None => None,
        };
        self.plan_with(head, m, sink.as_ref())
    }

    fn plan_with(&self, head: usize, m: &Mode, sink: Option<&aurora_igfx::dp::SinkCaps>) -> Result<modeset::Config> {
        let l = self.heads[head].lit.as_ref().ok_or(Error::Unsupported)?;
        let base = l.fw_config.as_ref().ok_or(Error::Unsupported)?;
        let t = aurora_igfx::timing::Timings::new(
            (m.hdisplay, m.hsync_start, m.hsync_end, m.htotal),
            (m.vdisplay, m.vsync_start, m.vsync_end, m.vtotal),
            m.flags & mode::flag::PHSYNC != 0,
            m.flags & mode::flag::PVSYNC != 0,
        );
        modeset::plan(&self.info, base, &t, m.clock, sink).map_err(|_| Error::Unsupported)
    }

    /// The buffer plane 1 of `head` should scan for `fb`.
    fn scanout(&self, fb: &Arc<Framebuffer>, old: Option<&Scan>) -> Result<Scan> {
        if fb.pitch.is_multiple_of(regs::STRIDE_UNIT) && fb.offset == 0 {
            let gtt = self.ggtt.bind(&fb.bo, regs::SURF_ALIGN)?;
            let scan = Scan { fb: fb.clone(), shadow: None, gtt, pitch: fb.pitch };
            scan.show(Damage { x: 0, y: 0, w: fb.width, h: fb.height });
            return Ok(scan);
        }
        let pitch = (fb.width * 4).next_multiple_of(regs::STRIDE_UNIT);
        let reuse =
            old.and_then(|o| o.shadow.as_ref().filter(|_| o.pitch == pitch && o.fb.height == fb.height)).cloned();
        let shadow = match reuse {
            Some(s) => s,
            None => {
                let size = pitch as usize * fb.height as usize;
                match BufferObject::alloc_contiguous(size) {
                    Some(b) => b,
                    None => BufferObject::alloc_system(size)?,
                }
            }
        };
        let gtt = self.ggtt.bind(&shadow, regs::SURF_ALIGN)?;
        let scan = Scan { fb: fb.clone(), shadow: Some(shadow), gtt, pitch };
        scan.show(Damage { x: 0, y: 0, w: fb.width, h: fb.height });
        Ok(scan)
    }

    /// Waits (at most 100 ms) until plane 1 of `pipe` scans `surf`.
    fn wait_flip(&self, pipe: u8, surf: u32) -> bool {
        let m = &*self.hw;
        let ok = m.wait_for(regs::plane_surflive(pipe, 0), !0xFFF, surf, 100_000);
        // Acknowledge what the IIR latched.
        m.write(regs::de_pipe_iir(pipe), regs::PIPE_VBLANK | regs::PIPE_PLANE1_FLIP_DONE);
        ok
    }

    fn show_cursor(&self, s: &mut State, i: usize, k: &PlaneState, image_changed: bool) {
        let h = &self.heads[i];
        let Some(l) = &h.lit else { return };
        let Some(fb) = &k.fb else { return };
        if image_changed {
            let slot = 1 - s.cursor_slot[i];
            s.cursor_slot[i] = slot;
            let dst = l.cursor_bo.vaddr() + (slot * CURSOR_BYTES) as u64;
            let src = fb.read_pixels();
            let stride = (fb.pitch / 4) as usize;
            let out = unsafe { core::slice::from_raw_parts_mut(dst as *mut u32, (CURSOR * CURSOR) as usize) };
            for y in 0..CURSOR as usize {
                for x in 0..CURSOR as usize {
                    let px = if x < fb.width as usize && y < fb.height as usize { src[y * stride + x] } else { 0 };
                    out[y * CURSOR as usize + x] = plane::premultiply(px);
                }
            }
            flush_cache(dst, CURSOR_BYTES as u64);
            fence();
        }
        let base = l.cursor_gtt + (s.cursor_slot[i] * CURSOR_BYTES) as u64;
        plane::cursor_show(&*self.hw, h.pipe, base as u32, k.dst.0, k.dst.1);
    }

    /// A full mode set on `head` (with `intel=modeset`); the previous mode
    /// comes back if the new one fails.
    fn modeset(&self, s: &mut State, head: usize, mode: &Mode) -> Result<()> {
        let to = self.plan_for(head, mode)?;
        let from = s.configs[head].ok_or(Error::Unsupported)?;
        let m = &*self.hw;
        match modeset::switch(m, &self.info, &from, &to) {
            modeset::Switch::Done => {
                log!(
                    "intel",
                    "pipe {}: {}x{} at {} kHz",
                    takeover::pipe_name(self.heads[head].pipe),
                    mode.hdisplay,
                    mode.vdisplay,
                    mode.clock
                );
                s.configs[head] = Some(to);
                Ok(())
            }
            modeset::Switch::Restored(e) => {
                log!(
                    "intel",
                    "mode set to {}x{} failed ({:?}); the previous mode is back",
                    mode.hdisplay,
                    mode.vdisplay,
                    e
                );
                // The previous picture again.
                if let Some(scan) = &s.scan[head] {
                    self.flip(head, scan);
                }
                Err(Error::Io)
            }
            modeset::Switch::Dark(e, e2) => {
                log!("intel", "mode set failed ({:?}) and restoring the previous mode failed too ({:?})", e, e2);
                Err(Error::Io)
            }
        }
    }

    fn flip(&self, head: usize, scan: &Scan) {
        let h = &self.heads[head];
        let Some(l) = &h.lit else { return };
        let p = plane::Primary {
            ctl: plane::primary_ctl(l.fw.primary.ctl, self.info.display),
            stride: plane::stride(scan.pitch),
            surf: scan.gtt as u32,
        };
        plane::flip(&*self.hw, h.pipe, &p);
    }

    /// The firmware may light several displays showing one framebuffer.
    /// Until a client gives such a pipe a picture of its own, it shows the
    /// same scanout buffer as `head` when the sizes match (as it did with
    /// the firmware framebuffer), instead of a frozen boot screen.
    /// Returns the pipes it flipped.
    fn mirror(&self, s: &State, head: usize, planes: &[PlaneState]) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(scan) = &s.scan[head] else { return out };
        for (j, h) in self.heads.iter().enumerate() {
            let Some(l) = &h.lit else { continue };
            if j == head || planes[2 * j].fb.is_some() || s.scan[j].is_some() {
                continue;
            }
            // (A pipe of another size keeps the firmware's last picture.)
            if l.fw.src == (scan.fb.width, scan.fb.height) {
                self.flip(j, scan);
                out.push(h.pipe);
            }
        }
        out
    }

    /// The internal panel's backlight.
    pub fn backlight(&self) -> Option<Pwm> {
        self.backlight
    }

    pub fn hw(&self) -> &Arc<H> {
        &self.hw
    }
}

impl<H: Hw> KmsDevice for Intel<H> {
    fn name(&self) -> &str {
        "intel"
    }

    fn caps(&self) -> Caps {
        let cursor = self.heads.iter().any(|h| h.lit.as_ref().is_some_and(|l| l.cursor_ok));
        Caps {
            live_modeset: (0..self.heads.len()).any(|i| self.can_modeset(i)),
            any_size: false,
            max_size: self.max_size,
            needs_dirty: true,
            cursor: cursor.then_some((CURSOR, CURSOR)),
            vblank_irq: false,
            follows_host: false,
        }
    }

    fn crtcs(&self) -> Vec<CrtcInfo> {
        self.heads.iter().map(|h| CrtcInfo { id: h.crtc, index: h.pipe as u32 }).collect()
    }

    fn planes(&self) -> Vec<PlaneInfo> {
        let mut out = Vec::new();
        for h in &self.heads {
            out.push(PlaneInfo {
                id: h.primary,
                kind: PlaneKind::Primary,
                possible_crtcs: 1 << h.pipe,
                formats: PRIMARY_FORMATS,
                max_size: self.max_size,
            });
            out.push(PlaneInfo {
                id: h.cursor,
                kind: PlaneKind::Cursor,
                possible_crtcs: 1 << h.pipe,
                formats: CURSOR_FORMATS,
                max_size: (CURSOR, CURSOR),
            });
        }
        out
    }

    fn encoders(&self) -> Vec<EncoderInfo> {
        self.ports
            .iter()
            .map(|p| EncoderInfo {
                id: p.encoder,
                kind: EncoderKind::Tmds,
                // Only the pipe the firmware drives it from (no routing yet).
                possible_crtcs: p.head.map_or(0, |h| 1 << h),
            })
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

    fn create_dumb(&self, w: u32, h: u32, format: u32) -> Result<Arc<Framebuffer>> {
        // Scanout-sized buffers in one physical run when possible (so the
        // panic screen can write them); rows packed as the compositor expects.
        if w == 0 || h == 0 || w > 16384 || h > 16384 {
            return Err(Error::Invalid);
        }
        let size = w as usize * h as usize * 4;
        match BufferObject::alloc_contiguous(size).filter(|_| w > CURSOR) {
            Some(bo) => Framebuffer::new(bo, w, h, w * 4, format),
            None => Framebuffer::new_system(w, h, format),
        }
    }

    fn atomic_check(&self, c: &Commit) -> Result<()> {
        let current = self.state();
        let (cs, ps, _) = validate(self, current.clone(), c)?;
        for (i, h) in self.heads.iter().enumerate() {
            let (new, old) = (&cs[i], &current.0[i]);
            if new != old {
                // A mode change: only a lit pipe, on its own connector, with `intel=modeset`.
                let ok = new.active
                    && new.connectors == old.connectors
                    && self.can_modeset(i)
                    && new.mode.is_some_and(|m| self.plan_for(i, &m).is_ok());
                if !ok {
                    return Err(Error::Unsupported);
                }
            }
            let (p, k) = (&ps[2 * i], &ps[2 * i + 1]);
            if new.active {
                // Plane 1 always shows something on a lit pipe.
                if p.fb.is_none() && current.1[2 * i].fb.is_some() {
                    return Err(Error::Unsupported);
                }
            } else if p.fb.is_some() || k.fb.is_some() {
                return Err(Error::Invalid);
            }
            if k.fb.is_some() && !h.lit.as_ref().is_some_and(|l| l.cursor_ok) {
                return Err(Error::Unsupported);
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
        // Scanout buffers first, so a failure changes nothing on screen.
        let mut scans: Vec<Option<Scan>> = Vec::new();
        for (i, _) in self.heads.iter().enumerate() {
            let (new_p, old_p) = (&ps[2 * i], &s.planes[2 * i]);
            let changed = !new_p.same_image(old_p) || cs[i] != s.crtcs[i];
            scans.push(match (&new_p.fb, changed) {
                (Some(fb), true) => Some(self.scanout(fb, s.scan[i].as_ref())?),
                _ => None,
            });
        }
        let mut flipped = Vec::new();
        for (i, h) in self.heads.iter().enumerate() {
            if cs[i] != s.crtcs[i] {
                let mode = cs[i].mode.ok_or(Error::Invalid)?;
                self.modeset(&mut s, i, &mode)?;
            }
            if let Some(scan) = scans[i].take() {
                self.flip(i, &scan);
                flipped.push((i, h.pipe, scan.gtt as u32));
                let surf = scan.gtt as u32;
                let old = s.scan[i].replace(scan);
                let mut pipes = self.mirror(&s, i, &ps);
                pipes.push(h.pipe);
                // The old scanout (and its GGTT mapping) goes once no pipe scans it.
                for pipe in pipes {
                    if !self.wait_flip(pipe, surf) {
                        log!("intel", "pipe {}: flip not seen within 100 ms", takeover::pipe_name(pipe));
                    }
                }
                drop(old);
            }
            let (new_k, old_k) = (&ps[2 * i + 1], &s.planes[2 * i + 1]);
            let on = cs[i].active && new_k.fb.is_some();
            let was_on = s.crtcs[i].active && old_k.fb.is_some();
            if on {
                let image = !was_on
                    || old_k.fb.as_ref().zip(new_k.fb.as_ref()).is_none_or(|(a, b)| !Arc::ptr_eq(a, b))
                    || new_k.hotspot != old_k.hotspot;
                let k = new_k.clone();
                self.show_cursor(&mut s, i, &k, image);
            } else if was_on {
                plane::cursor_hide(&*self.hw, h.pipe);
            }
        }
        let old = core::mem::replace(&mut s.crtcs, cs);
        let old_planes = core::mem::replace(&mut s.planes, ps);
        drop(s);
        drop(old);
        drop(old_planes);
        if c.flags & commit::PAGE_FLIP_EVENT != 0 {
            let now = crate::time::now_ns();
            for (i, pipe, _) in flipped {
                let seq = self.hw.read(regs::frame_count(pipe)) as u64;
                post(Event::Flip { crtc: self.heads[i].crtc, seq, time_ns: now, user_data: c.user_data });
            }
        }
        Ok(())
    }

    fn dirty(&self, fb: &Arc<Framebuffer>, rects: &[Damage]) -> Result<()> {
        let s = self.state.lock();
        for scan in s.scan.iter().flatten() {
            if Arc::ptr_eq(&scan.fb, fb) {
                for r in rects {
                    scan.show(*r);
                }
            }
        }
        Ok(())
    }

    fn vblank(&self, crtc: ObjId) -> Vblank {
        let Some(h) = self.heads.iter().find(|h| h.crtc == crtc) else { return Vblank::default() };
        Vblank { seq: self.hw.read(regs::frame_count(h.pipe)) as u64, time_ns: crate::time::now_ns() }
    }

    fn resume(&self) {
        // The display engine lost its state in sleep. Lighting a pipe again
        // needs the display core initialized (power wells, CD clock, display
        // buffer), which the firmware does at boot but not on resume, and
        // which this driver doesn't do yet.
        let m = &*self.hw;
        let alive = self
            .heads
            .iter()
            .filter_map(|h| h.lit.as_ref())
            .all(|l| m.read(regs::transconf(l.fw.trans)) & regs::TRANSCONF_STATE != 0);
        if alive {
            // Firmware that lit the display again on resume also set up the
            // display buffer again, without the cursor's share.
            for h in &self.heads {
                let Some(l) = h.lit.as_ref().filter(|l| l.cursor_ok) else { continue };
                m.modify(regs::de_pipe_imr(h.pipe), regs::PIPE_VBLANK | regs::PIPE_PLANE1_FLIP_DONE, 0);
                if Ddb::decode(m.read(regs::cur_buf_cfg(h.pipe))).size() == 0 {
                    Self::setup_cursor(m, &self.info, h.pipe, &l.fw, l.cursor_gtt);
                }
            }
            let s = self.state.lock();
            for (i, scan) in s.scan.iter().enumerate() {
                if let Some(scan) = scan {
                    self.flip(i, scan);
                }
            }
            log!("intel", "display still running after sleep; planes restored");
        } else {
            log!("intel", "the display engine was reset by sleep; restoring it is not implemented yet");
        }
    }

    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        // The first head's scanout buffer, if it is one physical run.
        let s = self.state.try_lock()?;
        let scan = s.scan.iter().flatten().next()?;
        let bo = scan.shadow.clone().unwrap_or_else(|| scan.fb.bo.clone());
        let (phys, bytes) = *bo.chunks().first()?;
        let size = scan.pitch as u64 * scan.fb.height as u64;
        (bo.chunks().len() == 1 && bytes as u64 >= size && scan.pitch.is_multiple_of(4)).then_some(
            bootinfo::Framebuffer {
                phys_addr: phys,
                size,
                width: scan.fb.width,
                height: scan.fb.height,
                stride: scan.pitch / 4,
                format: bootinfo::PixelFormat::Bgr,
            },
        )
    }
}

/// The panel's PWM, for the brightness keys.
struct PanelBacklight<H: Hw> {
    hw: Arc<H>,
    pwm: Pwm,
}

impl<H: Hw> super::Backlight for PanelBacklight<H> {
    fn get(&self) -> u32 {
        self.pwm.get(&*self.hw)
    }
    fn set(&self, percent: u32) {
        self.pwm.set(&*self.hw, percent)
    }
}

// --------------------------------------------------------- real hardware

static DEVICE: spin::Once<Arc<Intel<hw::Bar>>> = spin::Once::new();

/// The `intel` task: probes the connectors again when the display
/// interrupt registers latch a hotplug, and every four seconds anyway
/// (the latched bits differ between PCHs; a re-probe is cheap).
fn hotplug_task() {
    let Some(dev) = DEVICE.get().cloned() else { return };
    let m = &*dev.hw;
    let (mut ticks, mut last) = (0u32, 0u32);
    loop {
        crate::sched::sleep_ms(500);
        ticks += 1;
        let mut latched = 0;
        for reg in [regs::SDE_IIR, regs::DE_PORT_IIR] {
            let v = m.read(reg);
            if v != 0 && v != u32::MAX {
                m.write(reg, v);
                latched |= v;
            }
        }
        if dev.info.display >= 11 {
            let v = m.read(regs::DE_HPD_IIR);
            if v != 0 && v != u32::MAX {
                m.write(regs::DE_HPD_IIR, v);
                latched |= v;
            }
        }
        // A latched bit probes at once (at most every 2 s, in case some
        // source keeps latching); otherwise every 4 s.
        let due = if latched != 0 { ticks >= last + 4 } else { ticks >= last + 8 };
        if !due {
            continue;
        }
        last = ticks;
        if dev.refresh() {
            for c in dev.connectors() {
                log!("intel", "{}: {:?}", c.name(), c.status);
            }
            super::hotplug();
        }
    }
}

/// Binds the Intel display controller if there is one this driver knows
/// (and `intel=off` isn't set). Returns whether it took the display over.
pub fn probe() -> bool {
    let Some(d) = crate::drivers::pci::devices()
        .into_iter()
        .find(|d| d.vendor == platform::VENDOR_INTEL && d.class == platform::CLASS_DISPLAY)
    else {
        return false;
    };
    let where_ = alloc::format!("{:02x}:{:02x}.{} [8086:{:04x}]", d.bus, d.dev, d.func, d.device);
    let option = crate::gui::prefs::boot_option("intel");
    if option.as_deref() == Some("off") {
        log!("intel", "{}: left to the firmware framebuffer (intel=off)", where_);
        return false;
    }
    let Some(info) = platform::lookup(d.device) else {
        log!(
            "intel",
            "{}: not driven ({}); the firmware framebuffer stays",
            where_,
            platform::unsupported_reason(d.device)
        );
        return false;
    };
    let policy = if option.as_deref() == Some("modeset") { Policy::Modeset } else { Policy::Takeover };
    let crate::drivers::pci::Bar::Memory { phys, size } = d.bars[0] else {
        log!("intel", "{}: BAR 0 is not memory; declining", where_);
        return false;
    };
    if size < regs::BAR0_SIZE {
        log!("intel", "{}: BAR 0 is {} KiB, expected 16 MiB; declining", where_, size >> 10);
        return false;
    }
    let ggc = d.read16(regs::PCI_GGC);
    let bdsm = if info.display >= 11 {
        d.read32(regs::PCI_BDSM_GEN11) as u64 | (d.read32(regs::PCI_BDSM_GEN11 + 4) as u64) << 32
    } else {
        d.read32(regs::PCI_BDSM) as u64
    } & !0xFFFFF;
    log!(
        "intel",
        "{}: {} (display {}), BAR 0 at {:#x}, GGC {:#06x}, stolen memory at {:#x}, OpRegion at {:#x}, policy {:?}",
        where_,
        info.name,
        info.display,
        phys,
        ggc,
        bdsm,
        d.read32(regs::PCI_ASLS),
        policy
    );
    d.enable();
    let va = crate::mm::paging::map_mmio(phys, regs::BAR0_SIZE);
    let hw = Arc::new(unsafe { hw::Bar::new(va, gtt::ggtt_size(ggc)) });
    let hw_cursor = crate::gui::prefs::boot_option("cursor").as_deref() != Some("software");
    let dev = match Intel::new(hw, info, policy, ggc, hw_cursor) {
        Ok(d) => Arc::new(d),
        Err(e) => {
            log!("intel", "declining: {}; the firmware framebuffer stays", e);
            return false;
        }
    };
    for c in dev.connectors() {
        let edid = c.edid.as_deref().and_then(|e| aurora_edid::parse(e).ok());
        log!(
            "intel",
            "{} (DDI {}): {:?}{}",
            c.name(),
            dev.ports.iter().find(|p| p.connector == c.id).map_or('?', |p| platform::port_name(p.port)),
            c.status,
            match edid {
                Some(e) => alloc::format!(
                    ", EDID {} {:?}, {} modes",
                    e.manufacturer_str(),
                    e.name.as_deref().unwrap_or("?"),
                    c.modes.len()
                ),
                None => String::new(),
            }
        );
    }
    if let Some(pwm) = dev.backlight() {
        super::set_backlight(Arc::new(PanelBacklight { hw: dev.hw.clone(), pwm }));
    }
    DEVICE.call_once(|| dev.clone());
    crate::sched::spawn("intel", hotplug_task);
    log!("intel", "took the display over from the firmware");
    super::register(dev);
    true
}

/// The driver's device on real hardware, if it bound one.
pub fn device() -> Option<Arc<Intel<hw::Bar>>> {
    DEVICE.get().cloned()
}

#[cfg(feature = "ktest")]
pub mod selftest;
