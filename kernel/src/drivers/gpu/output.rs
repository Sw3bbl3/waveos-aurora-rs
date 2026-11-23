//! The compositor's view of a display: one connector of the primary
//! device with its CRTC, primary plane and (if any) cursor plane, a
//! back buffer the CPU draws into, and the few operations a compositor
//! needs — present damage, change size, follow the host window, move the
//! cursor. Everything goes through atomic commits on the device, so the
//! compositor is just another KMS client.
//!
//! The back buffer is the framebuffer the primary plane scans out. On
//! virtio-gpu that means no copy at all in the guest (the host reads the
//! damaged rectangles straight from it); on VGA-style devices presenting
//! copies the damaged rectangles to video memory.

use super::*;

struct Cursor {
    fb: Arc<Framebuffer>,
    hot: (i32, i32),
    pos: (i32, i32),
    visible: bool,
}

pub struct Output {
    pub dev: Arc<dyn KmsDevice>,
    pub crtc: ObjId,
    pub connector: ObjId,
    primary: ObjId,
    cursor_plane: Option<ObjId>,
    fb: Arc<Framebuffer>,
    mode: Mode,
    cursor: Option<Cursor>,
    last_preferred: Option<(u32, u32)>,
}

/// A mode of size `want` the connector (or, for virtual displays, the device) can show.
fn find_mode(dev: &dyn KmsDevice, conn: &ConnectorInfo, want: (u32, u32)) -> Option<Mode> {
    let listed = conn.modes.iter().filter(|m| m.size() == want).max_by_key(|m| (m.is_preferred(), m.vrefresh));
    if let Some(m) = listed {
        return Some(*m);
    }
    let caps = dev.caps();
    (caps.any_size && want.0 <= caps.max_size.0 && want.1 <= caps.max_size.1 && want.0 >= 320 && want.1 >= 200)
        .then(|| Mode::cvt(want.0, want.1, 60))
        .flatten()
}

impl Output {
    /// Opens the primary device's first connected display in mode
    /// `wanted` if it has it, else the mode it is in, else its preferred.
    pub fn open(wanted: Option<(u32, u32)>) -> Option<Output> {
        let dev = primary()?;
        let conns = dev.probe();
        let conn = conns.iter().find(|c| c.status == Status::Connected).or(conns.first())?.clone();
        Self::open_connector(dev, &conn, wanted, &[])
    }

    /// Opens every connected display of every device (the primary device's
    /// first), each on a CRTC of its own, in the size `wanted` gives for
    /// its connector when it can show it.
    pub fn open_all(wanted: impl Fn(&ConnectorInfo) -> Option<(u32, u32)>) -> Vec<Output> {
        let mut out: Vec<Output> = Vec::new();
        for dev in devices() {
            let conns = dev.probe();
            for conn in conns.iter().filter(|c| c.status == Status::Connected) {
                let taken: Vec<ObjId> = out.iter().map(|o| o.crtc).collect();
                if let Some(o) = Self::open_connector(dev.clone(), conn, wanted(conn), &taken) {
                    out.push(o);
                }
            }
        }
        out
    }

    /// Opens connector `conn` of `dev` on a CRTC that can drive it and is
    /// not in `taken`, in mode `wanted` if it has it, else the mode it is
    /// in, else its preferred.
    pub fn open_connector(
        dev: Arc<dyn KmsDevice>,
        conn: &ConnectorInfo,
        wanted: Option<(u32, u32)>,
        taken: &[ObjId],
    ) -> Option<Output> {
        let conn = conn.clone();
        let encoders = dev.encoders();
        let enc = encoders.iter().find(|e| conn.encoders.contains(&e.id))?;
        let crtc =
            dev.crtcs().into_iter().find(|c| enc.possible_crtcs & (1 << c.index) != 0 && !taken.contains(&c.id))?;
        let planes = dev.planes();
        let on_crtc = |k: PlaneKind| {
            planes.iter().find(|p| p.kind == k && p.possible_crtcs & (1 << crtc.index) != 0).map(|p| p.id)
        };
        let primary = on_crtc(PlaneKind::Primary)?;
        let cursor_plane = on_crtc(PlaneKind::Cursor);
        let (cs, _) = dev.state();
        let current = cs.iter().find(|s| s.crtc == crtc.id && s.active).and_then(|s| s.mode);
        let mode = wanted.and_then(|w| find_mode(&*dev, &conn, w)).or(current).or(conn.preferred().copied())?;
        let fb = Self::commit_mode(&*dev, crtc.id, conn.id, primary, mode).ok()?;
        log!("gpu", "{} on {}: {}x{}@{}", dev.name(), conn.name(), mode.hdisplay, mode.vdisplay, mode.vrefresh);
        Some(Output {
            last_preferred: conn.preferred().map(|m| m.size()),
            dev,
            crtc: crtc.id,
            connector: conn.id,
            primary,
            cursor_plane,
            fb,
            mode,
            cursor: None,
        })
    }

    /// A new back buffer of `mode`'s size, shown in `mode`.
    fn commit_mode(
        dev: &dyn KmsDevice,
        crtc: ObjId,
        conn: ObjId,
        primary: ObjId,
        mode: Mode,
    ) -> Result<Arc<Framebuffer>> {
        let fb = dev.create_dumb(mode.hdisplay, mode.vdisplay, fourcc::XRGB8888)?;
        let c = Commit {
            crtcs: alloc::vec![CrtcState { crtc, active: true, mode: Some(mode), connectors: alloc::vec![conn] }],
            planes: alloc::vec![PlaneState::show(primary, crtc, &fb, 0, 0)],
            flags: commit::ALLOW_MODESET,
            user_data: 0,
        };
        dev.atomic_commit(&c)?;
        Ok(fb)
    }

    pub fn size(&self) -> (u32, u32) {
        (self.fb.width, self.fb.height)
    }

    /// The back buffer's framebuffer (to present from another task).
    pub fn framebuffer(&self) -> Arc<Framebuffer> {
        self.fb.clone()
    }

    /// The connector as last probed (its status, modes and EDID).
    pub fn connector_info(&self) -> Option<ConnectorInfo> {
        self.dev.connectors().into_iter().find(|c| c.id == self.connector)
    }

    /// Turns the display off: its CRTC inactive, its planes empty (its
    /// connector went away, or the desktop no longer uses it).
    pub fn disable(&mut self) -> Result<()> {
        let mut planes = alloc::vec![PlaneState::disabled(self.primary)];
        planes.extend(self.cursor_plane.map(PlaneState::disabled));
        self.cursor = None;
        let c = Commit {
            crtcs: alloc::vec![CrtcState { crtc: self.crtc, active: false, mode: None, connectors: Vec::new() }],
            planes,
            flags: commit::ALLOW_MODESET,
            user_data: 0,
        };
        self.dev.atomic_commit(&c)
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// The back buffer: `width` pixels per row.
    ///
    /// # Safety
    /// Valid until the next mode change; the compositor is its only user.
    pub unsafe fn back_buffer(&self) -> &'static mut [u32] {
        let px = self.fb.pixels();
        core::slice::from_raw_parts_mut(px.as_mut_ptr(), px.len())
    }

    /// Shows the damaged rectangles of the back buffer.
    pub fn present(&self, rects: &[Damage]) -> Result<()> {
        self.dev.dirty(&self.fb, rects)
    }

    /// Switches to `w`×`h`. The old back buffer is gone afterwards.
    pub fn set_size(&mut self, w: u32, h: u32) -> Result<()> {
        let conns = self.dev.connectors();
        let conn = conns.iter().find(|c| c.id == self.connector).ok_or(Error::Invalid)?;
        let mode = find_mode(&*self.dev, conn, (w, h)).ok_or(Error::Invalid)?;
        self.fb = Self::commit_mode(&*self.dev, self.crtc, self.connector, self.primary, mode)?;
        self.mode = mode;
        Ok(())
    }

    /// After a hotplug event: if the display's preferred size changed (the
    /// host window was resized), switch to it. Returns `Some(true)` after a
    /// switch, `Some(false)` if the preferred size changed to the current one.
    pub fn follow_preferred(&mut self) -> Option<bool> {
        let conns = self.dev.probe();
        let pref = conns.iter().find(|c| c.id == self.connector)?.preferred()?.size();
        if Some(pref) == self.last_preferred {
            return None;
        }
        self.last_preferred = Some(pref);
        if pref == self.size() {
            return Some(false);
        }
        match self.set_size(pref.0, pref.1) {
            Ok(()) => Some(true),
            Err(e) => {
                log!("gpu", "could not follow the display to {}x{}: {:?}", pref.0, pref.1, e);
                None
            }
        }
    }

    /// The device can show a cursor plane.
    pub fn has_cursor(&self) -> bool {
        self.cursor_plane.is_some()
    }

    /// The largest cursor image the plane takes.
    pub fn cursor_size(&self) -> (u32, u32) {
        self.dev.caps().cursor.unwrap_or((0, 0))
    }

    fn cursor_state(&self) -> PlaneState {
        let plane = self.cursor_plane.unwrap_or(0);
        match &self.cursor {
            Some(c) if c.visible => {
                let mut s = PlaneState::show(plane, self.crtc, &c.fb, c.pos.0 - c.hot.0, c.pos.1 - c.hot.1);
                s.hotspot = c.hot;
                s
            }
            _ => PlaneState::disabled(plane),
        }
    }

    fn commit_cursor(&self) -> Result<()> {
        let c = Commit { planes: alloc::vec![self.cursor_state()], ..Commit::default() };
        self.dev.atomic_commit(&c)
    }

    /// Shows a `w`×`h` cursor image (0xAARRGGBB, straight alpha) with its
    /// hot spot at `hot`, at the current cursor position.
    pub fn set_cursor(&mut self, pixels: &[u32], w: u32, h: u32, hot: (i32, i32)) -> Result<()> {
        let (cw, ch) = self.dev.caps().cursor.ok_or(Error::Unsupported)?;
        if w > cw || h > ch || pixels.len() < (w * h) as usize {
            return Err(Error::Invalid);
        }
        let fb = self.dev.create_dumb(cw, ch, fourcc::ARGB8888)?;
        let dst = unsafe { fb.pixels() };
        let stride = (fb.pitch / 4) as usize;
        for y in 0..h as usize {
            dst[y * stride..y * stride + w as usize].copy_from_slice(&pixels[y * w as usize..(y + 1) * w as usize]);
        }
        let pos = self.cursor.as_ref().map_or((0, 0), |c| c.pos);
        self.cursor = Some(Cursor { fb, hot, pos, visible: true });
        self.commit_cursor()
    }

    /// Moves the cursor's hot spot to (`x`, `y`) in pixels.
    pub fn move_cursor(&mut self, x: i32, y: i32) -> Result<()> {
        let Some(c) = &mut self.cursor else { return Ok(()) };
        if c.pos == (x, y) {
            return Ok(());
        }
        c.pos = (x, y);
        if !c.visible {
            return Ok(());
        }
        self.commit_cursor()
    }

    pub fn hide_cursor(&mut self) -> Result<()> {
        match &mut self.cursor {
            Some(c) if c.visible => {
                c.visible = false;
                self.commit_cursor()
            }
            _ => Ok(()),
        }
    }

    /// Shows the last cursor image again after [`Output::hide_cursor`].
    pub fn show_cursor(&mut self) -> Result<()> {
        match &mut self.cursor {
            Some(c) if !c.visible => {
                c.visible = true;
                self.commit_cursor()
            }
            _ => Ok(()),
        }
    }

    /// A physical framebuffer showing this output, for the panic screen.
    pub fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        self.dev.panic_framebuffer()
    }
}
