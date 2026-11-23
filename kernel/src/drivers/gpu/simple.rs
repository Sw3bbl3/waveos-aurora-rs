//! One CRTC, one primary plane, one connector: the KMS objects of a
//! simple display (the firmware framebuffer, Bochs VGA, the ported Linux
//! driver's screen). A [`Backend`] does the hardware part — set a mode,
//! show changed rectangles — and [`Simple`] does the KMS bookkeeping.
//!
//! These displays scan out of their own memory (VRAM or a driver's
//! shadow buffer), so framebuffers live in system RAM where the CPU draws
//! fast, and [`KmsDevice::dirty`] copies the damaged rectangles over.

use super::*;
use crate::sync::Mutex;
use bootinfo::PixelFormat;

pub trait Backend: Send + Sync {
    fn name(&self) -> &str;
    fn caps(&self) -> Caps;
    fn firmware(&self) -> bool {
        false
    }
    fn connector_kind(&self) -> ConnectorKind;
    /// The modes the screen can show now (probing again if that means anything).
    fn modes(&self) -> Vec<Mode>;
    /// The mode the hardware is in at start.
    fn current(&self) -> Mode;
    fn set_mode(&self, mode: &Mode) -> Result<()>;
    /// Shows `rects` of `fb` (copy and, if needed, flush).
    fn show(&self, fb: &Framebuffer, rects: &[Damage]) -> Result<()>;
    /// After sleep: the hardware forgot `mode`.
    fn resume(&self, _mode: &Mode) {}
    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        None
    }
}

pub struct Simple<B: Backend> {
    pub backend: B,
    crtc: ObjId,
    plane: ObjId,
    encoder: ObjId,
    connector: ObjId,
    modes: IrqMutex<Vec<Mode>>,
    state: Mutex<(CrtcState, PlaneState)>,
    vblank: IrqMutex<Vblank>,
}

const FORMATS: &[u32] = &[fourcc::XRGB8888];

impl<B: Backend> Simple<B> {
    pub fn new(backend: B) -> Simple<B> {
        let (crtc, plane, encoder, connector) = (new_id(), new_id(), new_id(), new_id());
        let mut modes = backend.modes();
        mode::tidy(&mut modes);
        let current = backend.current();
        let current = modes.iter().copied().find(|m| m.size() == current.size()).unwrap_or(current);
        Simple {
            crtc,
            plane,
            encoder,
            connector,
            modes: IrqMutex::new(modes),
            state: Mutex::new((
                CrtcState { crtc, active: true, mode: Some(current), connectors: alloc::vec![connector] },
                PlaneState::disabled(plane),
            )),
            vblank: IrqMutex::new(Vblank::default()),
            backend,
        }
    }

    fn connector_info(&self) -> ConnectorInfo {
        ConnectorInfo {
            id: self.connector,
            kind: self.backend.connector_kind(),
            kind_index: 1,
            status: Status::Connected,
            modes: self.modes.lock().clone(),
            edid: None,
            mm: (0, 0),
            encoders: alloc::vec![self.encoder],
        }
    }
}

impl<B: Backend> KmsDevice for Simple<B> {
    fn name(&self) -> &str {
        self.backend.name()
    }
    fn caps(&self) -> Caps {
        self.backend.caps()
    }
    fn firmware(&self) -> bool {
        self.backend.firmware()
    }
    fn crtcs(&self) -> Vec<CrtcInfo> {
        alloc::vec![CrtcInfo { id: self.crtc, index: 0 }]
    }
    fn planes(&self) -> Vec<PlaneInfo> {
        let max = self.caps().max_size;
        alloc::vec![PlaneInfo {
            id: self.plane,
            kind: PlaneKind::Primary,
            possible_crtcs: 1,
            formats: FORMATS,
            max_size: max
        }]
    }
    fn encoders(&self) -> Vec<EncoderInfo> {
        alloc::vec![EncoderInfo { id: self.encoder, kind: EncoderKind::None, possible_crtcs: 1 }]
    }
    fn connectors(&self) -> Vec<ConnectorInfo> {
        alloc::vec![self.connector_info()]
    }
    fn probe(&self) -> Vec<ConnectorInfo> {
        let mut modes = self.backend.modes();
        mode::tidy(&mut modes);
        if !modes.is_empty() {
            *self.modes.lock() = modes;
        }
        self.connectors()
    }
    fn state(&self) -> (Vec<CrtcState>, Vec<PlaneState>) {
        let s = self.state.lock();
        (alloc::vec![s.0.clone()], alloc::vec![s.1.clone()])
    }

    fn atomic_check(&self, c: &Commit) -> Result<()> {
        let current = self.state();
        let (cs, _, _) = validate(self, current, c)?;
        let caps = self.caps();
        // Only listed modes, unless the screen takes any size.
        if let Some(m) = cs[0].mode.filter(|_| cs[0].active && !caps.any_size) {
            if !self.modes.lock().iter().any(|k| k.size() == m.size()) {
                return Err(Error::Invalid);
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
        let current = (alloc::vec![s.0.clone()], alloc::vec![s.1.clone()]);
        let (mut cs, mut ps, modeset) = validate(self, current, c)?;
        let (crtc, plane) = (cs.remove(0), ps.remove(0));
        if modeset && crtc.active {
            let mode = crtc.mode.ok_or(Error::Invalid)?;
            if s.0.mode.map(|m| m.size()) != Some(mode.size()) {
                self.backend.set_mode(&mode)?;
            }
        }
        let new_image = !plane.same_image(&s.1) || modeset;
        if let (Some(fb), true) = (&plane.fb, crtc.active && new_image) {
            self.backend.show(fb, &[Damage { x: 0, y: 0, w: fb.width, h: fb.height }])?;
        }
        let old = core::mem::replace(&mut *s, (crtc, plane));
        drop(s);
        drop(old);
        let vb = {
            let mut v = self.vblank.lock();
            v.seq += 1;
            v.time_ns = crate::time::now_ns();
            *v
        };
        if c.flags & commit::PAGE_FLIP_EVENT != 0 {
            post(Event::Flip { crtc: self.crtc, seq: vb.seq, time_ns: vb.time_ns, user_data: c.user_data });
        }
        Ok(())
    }

    fn dirty(&self, fb: &Arc<Framebuffer>, rects: &[Damage]) -> Result<()> {
        let s = self.state.lock();
        let shown = s.0.active && s.1.fb.as_ref().is_some_and(|f| Arc::ptr_eq(f, fb));
        if shown {
            self.backend.show(fb, rects)?;
        }
        Ok(())
    }

    fn vblank(&self, _crtc: ObjId) -> Vblank {
        *self.vblank.lock()
    }

    fn resume(&self) {
        let s = self.state.lock();
        if let (true, Some(mode)) = (s.0.active, s.0.mode) {
            self.backend.resume(&mode);
            if let Some(fb) = &s.1.fb {
                let _ = self.backend.show(fb, &[Damage { x: 0, y: 0, w: fb.width, h: fb.height }]);
            }
        }
    }

    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        self.backend.panic_framebuffer()
    }
}

/// Linear video memory the CPU writes (mapped write-combining when possible).
pub struct Vram {
    pub phys: u64,
    pub vaddr: u64,
    pub size: u64,
}

impl Vram {
    pub fn map(phys: u64, size: u64) -> Vram {
        let vaddr = match crate::mm::vmm::kmap_wc(phys, size) {
            Some(v) => {
                log!("gpu", "framebuffer at {:#x} mapped write-combining", phys);
                v
            }
            None => crate::mm::paging::map_mmio(phys, size),
        };
        Vram { phys, vaddr, size }
    }

    /// Copies `rects` of `fb` to the screen image at `stride` pixels per
    /// row, swapping red and blue for RGB framebuffers.
    pub fn copy(&self, fb: &Framebuffer, rects: &[Damage], stride: u32, height: u32, format: PixelFormat) {
        let src = fb.read_pixels();
        let src_stride = (fb.pitch / 4) as usize;
        let words = (self.size / 4) as usize;
        let dst = unsafe { core::slice::from_raw_parts_mut(self.vaddr as *mut u32, words) };
        for r in rects {
            let r = r.clamp(fb.width.min(stride), fb.height.min(height));
            if r.is_empty() {
                continue;
            }
            for y in r.y as usize..(r.y + r.h) as usize {
                let s = &src[y * src_stride + r.x as usize..y * src_stride + (r.x + r.w) as usize];
                let start = y * stride as usize + r.x as usize;
                let Some(d) = dst.get_mut(start..start + r.w as usize) else { break };
                match format {
                    PixelFormat::Bgr => d.copy_from_slice(s),
                    PixelFormat::Rgb => {
                        for (d, s) in d.iter_mut().zip(s) {
                            *d = (s & 0x00FF00) | (s >> 16 & 0xFF) | (s & 0xFF) << 16;
                        }
                    }
                }
            }
        }
        // Drain the write-combining buffers so the frame reaches the screen now.
        unsafe { core::arch::asm!("sfence", options(nostack, preserves_flags)) };
    }
}
