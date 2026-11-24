//! AMD Radeon (Polaris and Vega's DCE, Raven to Phoenix and Navi 1x–3x's
//! DCN): a native display driver on the display core.
//!
//! It **takes over the firmware's mode** and never sets one: it reads back
//! which timing generators the GOP lit and which surface they scan
//! (`aurora_amdgfx::takeover`), and declines unless that surface is exactly
//! the framebuffer the bootloader was handed. From then on it only writes
//! new surface addresses (flips between two buffers of its own in VRAM,
//! placed just above the firmware's framebuffer), the cursor registers,
//! and the DP AUX engine for EDID; hot-plug is the HPD pins' sense bits,
//! polled. Frame counts come from the timing generators' counters.
//!
//! Framebuffers live in system RAM like everywhere else in the display
//! core; presenting copies the damaged rectangles into the VRAM buffer on
//! screen, and a commit with a new framebuffer copies it into the other
//! buffer and flips at the next vblank (no tearing).
//!
//! `amd=off` in boot.conf leaves the firmware framebuffer alone. Mode
//! setting (which needs the VBIOS's AtomBIOS command tables) is not
//! implemented: `amd=modeset` is accepted and does the same as the
//! default. None of this has run on real hardware yet: see
//! docs/developer/amd-display.md.

pub mod hw;

use super::*;
use crate::sync::Mutex;
use aurora_amdgfx::platform::{self, Info};
use aurora_amdgfx::takeover::{self, Firmware, Gop};
use aurora_amdgfx::{aux, hpd, scanout, vram, Mmio};
use aurora_igfx::aux::Transport;
use aurora_igfx::gtt::Space;
pub use hw::Aperture;

/// Register access and the VRAM aperture: a real GPU, or the simulated
/// engine in tests.
pub trait Hw: Mmio + Aperture + Send + Sync + 'static {}
impl<T: Mmio + Aperture + Send + Sync + 'static> Hw for T {}

const CURSOR: u32 = scanout::CURSOR;
const PRIMARY_FORMATS: &[u32] = &[fourcc::XRGB8888, fourcc::ARGB8888];
const CURSOR_FORMATS: &[u32] = &[fourcc::ARGB8888];

/// A timing generator the firmware lit.
struct Head {
    fw: takeover::Head,
    crtc: ObjId,
    primary: ObjId,
    cursor: ObjId,
    encoder: ObjId,
    connector: ObjId,
    mode: Mode,
    /// The cursor's two image slots (aperture offset), when the cursor
    /// registers took the setup.
    cursor_at: Option<u64>,
}

/// A connector on an HPD pin that isn't (known to be) one of the lit heads.
struct Pin {
    pin: u8,
    connector: ObjId,
}

/// A head's own pair of scanout buffers (aperture offsets).
#[derive(Clone, Copy)]
struct Pair {
    bufs: [u64; 2],
    front: usize,
}

struct State {
    crtcs: Vec<CrtcState>,
    /// Primary then cursor plane of each head.
    planes: Vec<PlaneState>,
    /// The framebuffer each head shows (its own), and its buffers.
    scan: Vec<Option<Arc<Framebuffer>>>,
    pairs: Vec<Option<Pair>>,
    /// The aperture offset each head's pipes scan now.
    shown: Vec<u64>,
    cursor_slot: Vec<usize>,
}

/// What was found on an HPD pin.
#[derive(Clone)]
struct Seen {
    sensed: bool,
    dp: bool,
    edid: Option<Arc<Vec<u8>>>,
}

pub struct Amd<H: Hw> {
    hw: Arc<H>,
    pub info: Info,
    gop: Gop,
    fw: Firmware,
    heads: Vec<Head>,
    /// The HPD pin of each head, when it can be told (one lit display, one
    /// sensed pin).
    head_pin: Vec<Option<u8>>,
    pins: IrqMutex<Vec<Pin>>,
    seen: IrqMutex<[Option<Seen>; hpd::PINS as usize]>,
    connectors: IrqMutex<Vec<ConnectorInfo>>,
    space: IrqMutex<Space>,
    state: Mutex<State>,
}

/// Why the driver declined.
#[derive(Debug)]
pub enum Declined {
    Firmware(takeover::Decline),
    NoVram,
}

impl core::fmt::Display for Declined {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Declined::Firmware(d) => write!(f, "{d}"),
            Declined::NoVram => write!(f, "not enough of the VRAM aperture above the firmware framebuffer"),
        }
    }
}

/// The DRM mode of a lit head: its timings, or (when the firmware scales a
/// smaller surface to the panel) the surface size with the same blanking.
fn mode_of(h: &takeover::Head, clock_khz: u32) -> Mode {
    let t = &h.timings;
    let (w, ht) = h.src;
    let mut flags = if t.hsync_positive { mode::flag::PHSYNC } else { mode::flag::NHSYNC };
    flags |= if t.vsync_positive { mode::flag::PVSYNC } else { mode::flag::NVSYNC };
    let (dw, dh) = (w as i64 - t.hactive as i64, ht as i64 - t.vactive as i64);
    let sh = |v: u32| (v as i64 + dw).max(w as i64) as u32;
    let sv = |v: u32| (v as i64 + dh).max(ht as i64) as u32;
    let frame = t.htotal as u64 * t.vtotal as u64;
    Mode {
        clock: clock_khz,
        hdisplay: w,
        hsync_start: sh(t.hsync_start),
        hsync_end: sh(t.hsync_end),
        htotal: sh(t.htotal),
        vdisplay: ht,
        vsync_start: sv(t.vsync_start),
        vsync_end: sv(t.vsync_end),
        vtotal: sv(t.vtotal),
        vrefresh: (clock_khz as u64 * 1000 + frame / 2).checked_div(frame).unwrap_or(60) as u32,
        flags,
        kind: mode::kind::DRIVER | mode::kind::PREFERRED,
    }
}

/// Reads an EDID (base block and up to three extensions) over AUX.
fn read_edid(ch: &impl Transport) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut b = [0u8; 128];
    ch.read_edid_block(0, &mut b).ok()?;
    out.extend_from_slice(&b);
    for i in 1..=b[126].min(3) {
        if ch.read_edid_block(i, &mut b).is_err() {
            break;
        }
        out.extend_from_slice(&b);
    }
    if aurora_edid::parse(&out).is_err() {
        out.truncate(128);
        out[126] = 0;
        let sum = out[..127].iter().fold(0u8, |a, x| a.wrapping_add(*x));
        out[127] = 0u8.wrapping_sub(sum);
        aurora_edid::parse(&out).ok()?;
    }
    Some(out)
}

impl<H: Hw> Amd<H> {
    /// Takes the display over from the firmware, or declines having only
    /// read registers. `gop` is the bootloader's framebuffer (its offset in
    /// the VRAM aperture), `bar_size` the aperture's size; `hw_cursor`
    /// false leaves the cursor registers alone.
    pub fn new(
        hw: Arc<H>,
        info: Info,
        gop: Gop,
        bar_size: u64,
        hw_cursor: bool,
    ) -> core::result::Result<Self, Declined> {
        let m = &*hw;
        // 1. Read; decide. Nothing is written before this succeeds.
        let fw = takeover::read(m, &info, &gop).map_err(Declined::Firmware)?;
        let mut space = vram::space(&gop, bar_size).ok_or(Declined::NoVram)?;
        let (start, end) = vram::range(&gop, bar_size).ok_or(Declined::NoVram)?;
        if end > hw.vram_mapped() {
            return Err(Declined::NoVram);
        }
        log!(
            "amd",
            "the firmware framebuffer is at GPU address {:#x} (aperture offset {:#x}); using aperture {}–{} MiB",
            fw.addr,
            gop.offset,
            start >> 20,
            end >> 20
        );

        // 2. Hot-plug pins and, when it's clear which pin a display is on, its EDID.
        let sensed = hpd::sense(m, &info);
        let one_pin = (sensed.count_ones() == 1 && fw.heads.len() == 1).then(|| sensed.trailing_zeros() as u8);
        let mut heads = Vec::new();
        for (i, h) in fw.heads.iter().enumerate() {
            let pin = if i == 0 { one_pin } else { None };
            let edid = pin.and_then(|p| read_edid(&aux::Engine { mmio: m, info, ch: p }));
            let clock = Self::firmware_clock(m, &info, h, edid.as_deref());
            let mode = mode_of(h, clock);
            log!(
                "amd",
                "OTG {} on pipe(s) {:?}: {}x{} at {} Hz ({} kHz){}{}, pitch {} px",
                h.otg,
                h.pipes,
                mode.hdisplay,
                mode.vdisplay,
                mode.vrefresh,
                clock,
                if h.scaled { ", scaled to the display" } else { "" },
                if h.immediate { ", immediate flips" } else { "" },
                h.pitch
            );
            heads.push(Head {
                fw: h.clone(),
                crtc: new_id(),
                primary: new_id(),
                cursor: new_id(),
                encoder: new_id(),
                connector: new_id(),
                mode,
                cursor_at: None,
            });
        }

        // 3. Buffers: the first head's scanout pair now (so running short
        // declines), cursor images per head.
        let bytes = gop.bytes();
        let pair0 = {
            let a = space.alloc(bytes, vram::ALIGN).ok_or(Declined::NoVram)?;
            let b = space.alloc(bytes, vram::ALIGN).ok_or(Declined::NoVram)?;
            Pair { bufs: [a, b], front: 1 }
        };
        // 4. Take over: from here on the driver writes.
        for h in heads.iter_mut() {
            if !hw_cursor {
                continue;
            }
            if h.fw.pipes.len() != 1 {
                log!("amd", "OTG {}: split across pipes; the pointer is drawn in software", h.fw.otg);
                continue;
            }
            let Some(at) = space.alloc(2 * scanout::CURSOR_BYTES, vram::ALIGN) else { continue };
            unsafe { core::ptr::write_bytes((hw.vram_va() + at) as *mut u8, 0, 2 * scanout::CURSOR_BYTES as usize) };
            if scanout::cursor_setup(m, &info, h.fw.pipes[0], fw.gpu_addr(&gop, at)) {
                h.cursor_at = Some(at);
            } else {
                scanout::cursor_hide(m, &info, h.fw.pipes[0]);
                log!("amd", "OTG {}: the cursor registers didn't keep their values; software pointer", h.fw.otg);
            }
        }
        let n = heads.len();
        let state = State {
            crtcs: heads
                .iter()
                .map(|h| CrtcState {
                    crtc: h.crtc,
                    active: true,
                    mode: Some(h.mode),
                    connectors: alloc::vec![h.connector],
                })
                .collect(),
            planes: heads
                .iter()
                .flat_map(|h| [PlaneState::disabled(h.primary), PlaneState::disabled(h.cursor)])
                .collect(),
            scan: (0..n).map(|_| None).collect(),
            pairs: (0..n).map(|i| (i == 0).then_some(pair0)).collect(),
            shown: alloc::vec![gop.offset; n],
            cursor_slot: alloc::vec![0; n],
        };
        let mut seen: [Option<Seen>; hpd::PINS as usize] = Default::default();
        if let Some(p) = one_pin {
            seen[p as usize] = Some(Seen { sensed: true, dp: false, edid: None });
        }
        let dev = Amd {
            head_pin: (0..n).map(|i| if i == 0 { one_pin } else { None }).collect(),
            hw,
            info,
            gop,
            fw,
            heads,
            pins: IrqMutex::new(Vec::new()),
            seen: IrqMutex::new(seen),
            connectors: IrqMutex::new(Vec::new()),
            space: IrqMutex::new(space),
            state: Mutex::new(state),
        };
        dev.refresh();
        Ok(dev)
    }

    /// The firmware's pixel clock: from the display's EDID (the timing with
    /// the same totals), else measured from the frame counter, else 60 Hz.
    fn firmware_clock(m: &H, info: &Info, h: &takeover::Head, edid: Option<&[u8]>) -> u32 {
        let t = &h.timings;
        if let Some(e) = edid.and_then(|e| aurora_edid::parse(e).ok()) {
            let same = |x: &&aurora_edid::Timing| {
                (x.hactive, x.htotal, x.vactive, x.vtotal) == (t.hactive, t.htotal, t.vactive, t.vtotal)
            };
            if let Some(x) = e.timings.iter().find(same) {
                return x.clock_khz;
            }
        }
        let frame = t.htotal as u64 * t.vtotal as u64;
        let count = || scanout::frame_count(m, info, h.otg);
        let edge = |from: u32| {
            for _ in 0..100 {
                if count() != from {
                    return true;
                }
                m.delay_us(1000);
            }
            false
        };
        if edge(count()) {
            let t0 = crate::time::now_ns();
            let f1 = count();
            let mut ok = true;
            for k in 0..6 {
                ok &= edge(f1.wrapping_add(k));
            }
            let dt = crate::time::now_ns() - t0;
            let frames = (count().wrapping_sub(f1) & 0xFF_FFFF) as u64;
            let hz_milli = (frames * 1_000_000_000_000).checked_div(dt).unwrap_or(0);
            if ok && (20_000..=250_000).contains(&hz_milli) {
                return (frame * hz_milli / 1_000_000) as u32;
            }
        }
        (frame * 60 / 1000) as u32
    }

    fn gpu(&self, offset: u64) -> u64 {
        self.fw.gpu_addr(&self.gop, offset)
    }

    /// Looks at the HPD pins again: new pins become connectors, DP sinks
    /// give their EDID. Returns whether anything changed.
    pub fn refresh(&self) -> bool {
        let m = &*self.hw;
        let sensed = hpd::sense(m, &self.info);
        let mut changed = false;
        let mut seen = self.seen.lock().clone();
        for p in 0..hpd::PINS {
            let on = sensed & 1 << p != 0;
            let prev = seen[p as usize].clone();
            match (&prev, on) {
                (Some(s), true) if s.sensed && (s.edid.is_some() || s.dp) => continue,
                (Some(s), false) if !s.sensed => continue,
                (None, false) => continue,
                _ => {}
            }
            let s = if on {
                let ch = aux::Engine { mmio: m, info: self.info, ch: p };
                let dp = Transport::read_byte(&ch, 0).is_ok();
                let edid = if dp { read_edid(&ch).map(Arc::new) } else { None };
                Seen { sensed: true, dp, edid }
            } else {
                Seen { sensed: false, dp: false, edid: None }
            };
            changed |= prev.as_ref().is_none_or(|o| o.sensed != s.sensed || o.edid != s.edid);
            seen[p as usize] = Some(s);
            if !self.head_pin.contains(&Some(p)) && !self.pins.lock().iter().any(|x| x.pin == p) {
                self.pins.lock().push(Pin { pin: p, connector: new_id() });
            }
        }
        *self.seen.lock() = seen.clone();

        let mut list = Vec::new();
        let info = |connector: ObjId, s: Option<&Seen>, lit: Option<&Head>| {
            let edid = s.and_then(|s| s.edid.clone());
            let parsed = edid.as_deref().and_then(|e| aurora_edid::parse(e).ok());
            let connected = lit.is_some() || s.is_some_and(|s| s.sensed);
            let mut modes: Vec<Mode> = match lit {
                Some(h) => alloc::vec![h.mode],
                None => parsed
                    .iter()
                    .flat_map(|e| e.timings.iter())
                    .filter(|t| !t.interlaced && t.hactive >= 640 && t.vactive >= 480)
                    .map(|t| Mode::from_timing(t).preferred(t.preferred))
                    .collect(),
            };
            mode::tidy(&mut modes);
            if !connected {
                modes.clear();
            }
            ConnectorInfo {
                id: connector,
                kind: if s.is_some_and(|s| s.dp) { ConnectorKind::DisplayPort } else { ConnectorKind::Unknown },
                kind_index: 0,
                status: if connected { Status::Connected } else { Status::Disconnected },
                modes,
                edid,
                mm: parsed.as_ref().map_or((0, 0), |e| (e.width_mm, e.height_mm)),
                encoders: alloc::vec![],
            }
        };
        for (i, h) in self.heads.iter().enumerate() {
            let s = self.head_pin[i].and_then(|p| seen[p as usize].clone());
            let mut c = info(h.connector, s.as_ref(), Some(h));
            c.encoders = alloc::vec![h.encoder];
            list.push(c);
        }
        for p in self.pins.lock().iter() {
            list.push(info(p.connector, seen[p.pin as usize].as_ref(), None));
        }
        for i in 0..list.len() {
            let k = list[i].kind;
            list[i].kind_index = list[..=i].iter().filter(|c| c.kind == k).count() as u32;
        }
        let mut old = self.connectors.lock();
        changed |= old.len() != list.len()
            || old.iter().zip(&list).any(|(a, b)| a.status != b.status || a.edid != b.edid || a.kind != b.kind);
        *old = list;
        changed
    }

    /// Copies rows `r` of `fb` into the buffer at aperture offset `at`.
    fn copy(&self, fb: &Framebuffer, at: u64, r: Damage) {
        let r = r.clamp(fb.width, fb.height);
        if r.is_empty() {
            return;
        }
        let pitch = self.gop.stride as u64 * 4;
        let src = fb.read_pixels();
        let stride = (fb.pitch / 4) as usize;
        let base = self.hw.vram_va() + at;
        for y in r.y..r.y + r.h {
            let s = &src[y as usize * stride + r.x as usize..y as usize * stride + (r.x + r.w) as usize];
            let d = base + y as u64 * pitch + r.x as u64 * 4;
            unsafe { core::ptr::copy_nonoverlapping(s.as_ptr(), d as *mut u32, s.len()) };
        }
        // Drain the write-combining buffers, then a read through the
        // aperture so the writes have left the CPU before the flip.
        unsafe {
            crate::arch::cache::wc_fence();
            core::ptr::read_volatile(base as *const u32);
        }
    }

    /// Points every pipe of head `i` at aperture offset `at`, waits for it
    /// to take effect.
    fn flip(&self, s: &mut State, i: usize, at: u64) {
        let m = &*self.hw;
        let h = &self.heads[i];
        for &p in &h.fw.pipes {
            scanout::flip(m, &self.info, p, self.gpu(at));
        }
        for &p in &h.fw.pipes {
            if !scanout::wait_flip(m, &self.info, p, 100_000) {
                log!("amd", "OTG {}: flip not seen within 100 ms", h.fw.otg);
            }
        }
        s.shown[i] = at;
    }

    fn show_cursor(&self, s: &mut State, i: usize, k: &PlaneState, image_changed: bool) {
        let h = &self.heads[i];
        let (Some(at), Some(fb)) = (h.cursor_at, &k.fb) else { return };
        if image_changed {
            let slot = 1 - s.cursor_slot[i];
            s.cursor_slot[i] = slot;
            let dst = self.hw.vram_va() + at + slot as u64 * scanout::CURSOR_BYTES;
            let src = fb.read_pixels();
            let stride = (fb.pitch / 4) as usize;
            let out = unsafe { core::slice::from_raw_parts_mut(dst as *mut u32, (CURSOR * CURSOR) as usize) };
            for y in 0..CURSOR as usize {
                for x in 0..CURSOR as usize {
                    let px = if x < fb.width as usize && y < fb.height as usize { src[y * stride + x] } else { 0 };
                    out[y * CURSOR as usize + x] = scanout::premultiply(px);
                }
            }
            crate::arch::cache::wc_fence();
        }
        let addr = self.gpu(at + s.cursor_slot[i] as u64 * scanout::CURSOR_BYTES);
        scanout::cursor_show(&*self.hw, &self.info, h.fw.pipes[0], addr, k.dst.0, k.dst.1);
    }

    fn pair(&self, s: &mut State, i: usize) -> Result<Pair> {
        if let Some(p) = s.pairs[i] {
            return Ok(p);
        }
        let bytes = self.gop.bytes();
        let mut space = self.space.lock();
        let a = space.alloc(bytes, vram::ALIGN).ok_or(Error::NoMemory)?;
        let Some(b) = space.alloc(bytes, vram::ALIGN) else {
            space.free(a, bytes);
            return Err(Error::NoMemory);
        };
        let p = Pair { bufs: [a, b], front: 1 };
        s.pairs[i] = Some(p);
        Ok(p)
    }

    pub fn hw(&self) -> &Arc<H> {
        &self.hw
    }
}

impl<H: Hw> KmsDevice for Amd<H> {
    fn name(&self) -> &str {
        "amd"
    }

    fn caps(&self) -> Caps {
        Caps {
            live_modeset: false,
            any_size: false,
            max_size: (self.gop.width, self.gop.height),
            needs_dirty: true,
            cursor: self.heads.iter().any(|h| h.cursor_at.is_some()).then_some((CURSOR, CURSOR)),
            vblank_irq: false,
            follows_host: false,
        }
    }

    fn crtcs(&self) -> Vec<CrtcInfo> {
        self.heads.iter().enumerate().map(|(i, h)| CrtcInfo { id: h.crtc, index: i as u32 }).collect()
    }

    fn planes(&self) -> Vec<PlaneInfo> {
        let mut out = Vec::new();
        for (i, h) in self.heads.iter().enumerate() {
            out.push(PlaneInfo {
                id: h.primary,
                kind: PlaneKind::Primary,
                possible_crtcs: 1 << i,
                formats: PRIMARY_FORMATS,
                max_size: h.fw.src,
            });
            if h.cursor_at.is_some() {
                out.push(PlaneInfo {
                    id: h.cursor,
                    kind: PlaneKind::Cursor,
                    possible_crtcs: 1 << i,
                    formats: CURSOR_FORMATS,
                    max_size: (CURSOR, CURSOR),
                });
            }
        }
        out
    }

    fn encoders(&self) -> Vec<EncoderInfo> {
        self.heads
            .iter()
            .enumerate()
            .map(|(i, h)| EncoderInfo { id: h.encoder, kind: EncoderKind::Tmds, possible_crtcs: 1 << i })
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
        let (cs, ps, _) = validate(self, current.clone(), c)?;
        for (i, h) in self.heads.iter().enumerate() {
            // The firmware's mode, always on: no mode setting.
            if cs[i] != current.0[i] {
                return Err(Error::Unsupported);
            }
            let (p, k) = (&ps[2 * i], &ps[2 * i + 1]);
            match &p.fb {
                Some(fb) if (fb.width, fb.height) != h.fw.src || p.dst.0 != 0 || p.dst.1 != 0 => {
                    return Err(Error::Unsupported)
                }
                None if current.1[2 * i].fb.is_some() => return Err(Error::Unsupported),
                _ => {}
            }
            if k.fb.is_some() && h.cursor_at.is_none() {
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
        let mut flipped = Vec::new();
        for i in 0..self.heads.len() {
            let (new_p, old_p) = (&ps[2 * i], &s.planes[2 * i]);
            if let (Some(fb), false) = (&new_p.fb, new_p.same_image(old_p)) {
                // The whole picture into the buffer not on screen, then flip.
                let mut pair = self.pair(&mut s, i)?;
                let back = 1 - pair.front;
                self.copy(fb, pair.bufs[back], Damage { x: 0, y: 0, w: fb.width, h: fb.height });
                self.flip(&mut s, i, pair.bufs[back]);
                pair.front = back;
                s.pairs[i] = Some(pair);
                s.scan[i] = Some(fb.clone());
                flipped.push(i);
                // Lit displays nobody gave a picture of their own show this one.
                for j in 0..self.heads.len() {
                    if j != i
                        && ps[2 * j].fb.is_none()
                        && s.scan[j].is_none()
                        && self.heads[j].fw.src == self.heads[i].fw.src
                    {
                        self.flip(&mut s, j, pair.bufs[back]);
                    }
                }
            }
            let (new_k, old_k) = (&ps[2 * i + 1], &s.planes[2 * i + 1]);
            let pipe = self.heads[i].fw.pipes[0];
            if new_k.fb.is_some() {
                let image = old_k.fb.as_ref().zip(new_k.fb.as_ref()).is_none_or(|(a, b)| !Arc::ptr_eq(a, b))
                    || new_k.hotspot != old_k.hotspot;
                let k = new_k.clone();
                self.show_cursor(&mut s, i, &k, image);
            } else if old_k.fb.is_some() {
                scanout::cursor_hide(&*self.hw, &self.info, pipe);
            }
        }
        let old = core::mem::replace(&mut s.crtcs, cs);
        let old_planes = core::mem::replace(&mut s.planes, ps);
        drop(s);
        drop(old);
        drop(old_planes);
        if c.flags & commit::PAGE_FLIP_EVENT != 0 {
            let now = crate::time::now_ns();
            for i in flipped {
                let seq = scanout::frame_count(&*self.hw, &self.info, self.heads[i].fw.otg) as u64;
                post(Event::Flip { crtc: self.heads[i].crtc, seq, time_ns: now, user_data: c.user_data });
            }
        }
        Ok(())
    }

    fn dirty(&self, fb: &Arc<Framebuffer>, rects: &[Damage]) -> Result<()> {
        let s = self.state.lock();
        for (i, scan) in s.scan.iter().enumerate() {
            if scan.as_ref().is_some_and(|f| Arc::ptr_eq(f, fb)) {
                // The buffer on screen (displays mirroring it show it too).
                let at = s.shown[i];
                for r in rects {
                    self.copy(fb, at, *r);
                }
            }
        }
        Ok(())
    }

    fn vblank(&self, crtc: ObjId) -> Vblank {
        let Some(h) = self.heads.iter().find(|h| h.crtc == crtc) else { return Vblank::default() };
        Vblank { seq: scanout::frame_count(&*self.hw, &self.info, h.fw.otg) as u64, time_ns: crate::time::now_ns() }
    }

    fn resume(&self) {
        // Lighting a display again needs the AtomBIOS tables this driver
        // doesn't run; firmware that lit it again on resume is used as is.
        let m = &*self.hw;
        if !self.heads.iter().all(|h| scanout::running(m, &self.info, h.fw.otg)) {
            log!("amd", "the display engine was reset by sleep; restoring it is not implemented yet");
            return;
        }
        let mut s = self.state.lock();
        for i in 0..self.heads.len() {
            let at = s.shown[i];
            if at != self.gop.offset {
                self.flip(&mut s, i, at);
            }
            if let Some(c) = self.heads[i].cursor_at {
                let pipe = self.heads[i].fw.pipes[0];
                scanout::cursor_setup(m, &self.info, pipe, self.gpu(c));
                let k = s.planes[2 * i + 1].clone();
                if k.fb.is_some() {
                    self.show_cursor(&mut s, i, &k, false);
                }
            }
        }
        log!("amd", "display still running after sleep; planes restored");
    }

    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        // VRAM is the screen: the first head's buffer on screen.
        let s = self.state.try_lock()?;
        let at = *s.shown.first()?;
        Some(bootinfo::Framebuffer {
            phys_addr: self.hw.vram_phys() + at,
            size: self.gop.bytes(),
            width: self.gop.width,
            height: self.gop.height,
            stride: self.gop.stride,
            format: bootinfo::PixelFormat::Bgr,
        })
    }
}

// --------------------------------------------------------- real hardware

static DEVICE: spin::Once<Arc<Amd<hw::Bar>>> = spin::Once::new();

/// The `amd` task: polls the HPD pins every second and probes again when
/// they change (and every 8 s anyway, for DP sinks that come and go
/// without a pin change).
fn hotplug_task() {
    let Some(dev) = DEVICE.get().cloned() else { return };
    let mut last = hpd::sense(&*dev.hw, &dev.info);
    let mut ticks = 0u32;
    loop {
        crate::sched::sleep_ms(1000);
        ticks += 1;
        let now = hpd::sense(&*dev.hw, &dev.info);
        if now == last && !ticks.is_multiple_of(8) {
            continue;
        }
        last = now;
        if dev.refresh() {
            for c in dev.connectors() {
                log!("amd", "{}: {:?}", c.name(), c.status);
            }
            super::hotplug();
        }
    }
}

/// Binds the AMD display controller that scans the firmware framebuffer,
/// if this driver knows it (and `amd=off` isn't set). Returns whether it
/// took the display over.
pub fn probe(boot_fb: Option<bootinfo::Framebuffer>) -> bool {
    let devices: Vec<_> = crate::drivers::pci::devices()
        .into_iter()
        .filter(|d| d.vendor == platform::VENDOR_AMD && d.class == platform::CLASS_DISPLAY)
        .collect();
    if devices.is_empty() {
        return false;
    }
    let option = crate::gui::prefs::boot_option("amd");
    for d in devices {
        let where_ = alloc::format!("{:02x}:{:02x}.{} [1002:{:04x}]", d.bus, d.dev, d.func, d.device);
        if option.as_deref() == Some("off") {
            log!("amd", "{}: left to the firmware framebuffer (amd=off)", where_);
            continue;
        }
        let Some(info) = platform::lookup(d.device) else {
            log!(
                "amd",
                "{}: not driven ({}); the firmware framebuffer stays",
                where_,
                platform::unsupported_reason(d.device)
            );
            continue;
        };
        let (
            crate::drivers::pci::Bar::Memory { phys: vram, size: vram_size },
            crate::drivers::pci::Bar::Memory { phys: regs, size: regs_size },
        ) = (d.bars[0], d.bars[5])
        else {
            log!("amd", "{}: BAR 0 or BAR 5 is not memory; declining", where_);
            continue;
        };
        let Some(fb) = boot_fb.filter(|f| f.phys_addr >= vram && f.phys_addr + f.size <= vram + vram_size) else {
            log!(
                "amd",
                "{}: {} {}, the firmware framebuffer is not on it; left alone",
                where_,
                info.asic,
                info.block.name()
            );
            continue;
        };
        if d.read16(0x04) & 0x2 == 0 {
            log!("amd", "{}: memory decoding is off; declining", where_);
            continue;
        }
        let max = aurora_amdgfx::regs::max_offset(info.block.layout()) as u64 + 4;
        if regs_size < max {
            log!(
                "amd",
                "{}: BAR 5 is {} KiB, the registers need {} KiB; declining",
                where_,
                regs_size >> 10,
                max.div_ceil(1024)
            );
            continue;
        }
        if option.as_deref() == Some("modeset") {
            log!("amd", "amd=modeset: mode setting is not implemented (it needs the AtomBIOS tables); keeping the firmware's mode");
        }
        log!(
            "amd",
            "{}: {} ({}{}), VRAM aperture {:#x} ({} MiB), registers {:#x} ({} KiB){}",
            where_,
            info.asic,
            info.block.name(),
            if info.apu { ", APU" } else { "" },
            vram,
            vram_size >> 20,
            regs,
            regs_size >> 10,
            if info.block.bases_confirmed() { "" } else { "; register bases assumed (UNCONFIRMED)" }
        );
        let gop = Gop {
            offset: fb.phys_addr - vram,
            width: fb.width,
            height: fb.height,
            stride: fb.stride,
            bgr: fb.format == bootinfo::PixelFormat::Bgr,
        };
        let Some((_, end)) = vram::range(&gop, vram_size) else {
            log!("amd", "declining: the VRAM aperture is too small");
            continue;
        };
        let regs_va = crate::mm::paging::map_mmio(regs, regs_size);
        let vram_va = crate::mm::vmm::kmap_wc(vram, end).unwrap_or_else(|| crate::mm::paging::map_mmio(vram, end));
        let hw = Arc::new(unsafe { hw::Bar::new(regs_va, regs_size, vram_va, vram, end) });
        let hw_cursor = crate::gui::prefs::boot_option("cursor").as_deref() != Some("software");
        let dev = match Amd::new(hw, info, gop, vram_size, hw_cursor) {
            Ok(d) => Arc::new(d),
            Err(e) => {
                log!("amd", "declining: {}; the firmware framebuffer stays", e);
                continue;
            }
        };
        for c in dev.connectors() {
            let edid = c.edid.as_deref().and_then(|e| aurora_edid::parse(e).ok());
            log!(
                "amd",
                "{}: {:?}{}",
                c.name(),
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
        log!("amd", "hardware cursor: {}", if dev.caps().cursor.is_some() { "yes" } else { "no" });
        DEVICE.call_once(|| dev.clone());
        crate::sched::spawn("amd", hotplug_task);
        log!("amd", "took the display over from the firmware");
        super::register(dev);
        return true;
    }
    false
}

#[cfg(feature = "ktest")]
pub mod selftest;
