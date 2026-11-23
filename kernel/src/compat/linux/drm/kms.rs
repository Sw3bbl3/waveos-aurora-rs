//! Mode setting: resources, CRTCs, encoders, connectors, planes,
//! framebuffers, dumb buffers, the cursor, legacy mode sets and page flips.
//!
//! Object ids are the core's (`gpu::ObjId`), framebuffer ids too (a
//! framebuffer is a core `Framebuffer` over a GEM handle's buffer object),
//! and every state change is a core `Commit` made through `gpu::owner`, so
//! the master's first change takes the display from the desktop.

use super::props;
use super::{get_array, put_array, vblank, Args, File};
use crate::drivers::gpu::{
    self, commit, fourcc, BufferObject, Commit, ConnectorKind, CrtcState, EncoderKind, Framebuffer, KmsDevice, ObjId,
    PlaneKind, PlaneState, Status,
};
use crate::sync::Mutex;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use aurora_abi::err::*;

// ------------------------------------------------------------ framebuffers

/// Every framebuffer programs made: (id, file, the framebuffer).
static FBS: Mutex<Vec<(ObjId, u64, Weak<Framebuffer>)>> = Mutex::new(Vec::new());

/// A framebuffer by id: one a program made, or one on screen now (the
/// desktop's).
pub fn fb_by_id(f: &File, id: u32) -> Option<Arc<Framebuffer>> {
    if id == 0 {
        return None;
    }
    if let Some(fb) = FBS.lock().iter().find(|e| e.0 == id).and_then(|e| e.2.upgrade()) {
        return Some(fb);
    }
    f.dev.state().1.into_iter().filter_map(|p| p.fb).find(|fb| fb.id == id)
}

/// A file's framebuffers go with it.
pub fn forget_file(file: u64) {
    FBS.lock().retain(|e| e.1 != file && e.2.strong_count() > 0);
}

/// Bytes per pixel and depth of the formats the core can show.
fn format_info(format: u32) -> Option<(u32, u32)> {
    match format {
        fourcc::XRGB8888 => Some((4, 24)),
        fourcc::ARGB8888 => Some((4, 32)),
        _ => None,
    }
}

/// The format legacy `ADDFB` means by `bpp` and `depth`.
fn legacy_format(bpp: u32, depth: u32) -> Option<u32> {
    match (bpp, depth) {
        (32, 24) => Some(fourcc::XRGB8888),
        (32, 32) => Some(fourcc::ARGB8888),
        _ => None,
    }
}

/// Makes a framebuffer for `ADDFB`/`ADDFB2` (and the cursor ioctls).
fn make_fb(
    f: &File,
    w: u32,
    h: u32,
    format: u32,
    handle: u32,
    pitch: u32,
    offset: u32,
) -> Result<Arc<Framebuffer>, isize> {
    let dev = &*f.dev;
    // Some plane must take the format.
    if !dev.planes().iter().any(|p| p.formats.contains(&format)) {
        return Err(EINVAL);
    }
    let (cpp, _) = format_info(format).ok_or(EINVAL)?;
    let max = dev.caps().max_size;
    if w == 0 || h == 0 || w > max.0.max(16384) || h > max.1.max(16384) {
        return Err(EINVAL);
    }
    let bo = f.bo(handle).ok_or(ENOENT)?;
    let min_pitch = w as u64 * cpp as u64;
    if (pitch as u64) < min_pitch || !pitch.is_multiple_of(4) {
        return Err(EINVAL);
    }
    let end = (h as u64 - 1) * pitch as u64 + min_pitch + offset as u64;
    if end > bo.size as u64 {
        return Err(EINVAL);
    }
    Ok(Arc::new(Framebuffer { id: gpu::new_id(), bo, width: w, height: h, pitch, offset, format }))
}

fn register_fb(f: &File, fb: &Arc<Framebuffer>) {
    FBS.lock().push((fb.id, f.id, Arc::downgrade(fb)));
    f.state.lock().fbs.push(fb.clone());
}

/// `struct drm_mode_fb_cmd`: fb_id, width, height, pitch, bpp, depth, handle.
pub fn add_fb(f: &File, a: &mut Args) -> Result<(), isize> {
    let format = legacy_format(a.u32(16), a.u32(20)).ok_or(EINVAL)?;
    let fb = make_fb(f, a.u32(4), a.u32(8), format, a.u32(24), a.u32(12), 0)?;
    register_fb(f, &fb);
    a.set32(0, fb.id);
    Ok(())
}

/// `struct drm_mode_fb_cmd2`: fb_id, width, height, pixel_format, flags,
/// handles[4], pitches[4], offsets[4], modifier[4].
pub fn add_fb2(f: &File, a: &mut Args) -> Result<(), isize> {
    const INTERLACED: u32 = 1;
    const MODIFIERS: u32 = 2;
    // The only modifier is DRM_FORMAT_MOD_LINEAR (0), which is also what
    // a framebuffer without modifiers is; others (and MOD_INVALID) are
    // refused, as Linux refuses what a plane's IN_FORMATS doesn't list.
    let flags = a.u32(16);
    if flags & !(INTERLACED | MODIFIERS) != 0 || (flags & MODIFIERS != 0 && a.u64(72) != 0) {
        return Err(EINVAL);
    }
    // One plane per format: the other slots must be empty.
    for i in 1..4 {
        if a.u32(20 + 4 * i) != 0 || a.u32(36 + 4 * i) != 0 || a.u32(52 + 4 * i) != 0 {
            return Err(EINVAL);
        }
    }
    let fb = make_fb(f, a.u32(4), a.u32(8), a.u32(12), a.u32(20), a.u32(36), a.u32(52))?;
    register_fb(f, &fb);
    a.set32(0, fb.id);
    Ok(())
}

/// `RMFB` (`disable`: planes showing it are turned off, as on Linux) and
/// `CLOSEFB` (they keep showing it until replaced).
pub fn rm_fb(f: &File, id: u32, disable: bool) -> Result<(), isize> {
    let fb = {
        let mut st = f.state.lock();
        let i = st.fbs.iter().position(|fb| fb.id == id).ok_or(ENOENT)?;
        st.fbs.remove(i)
    };
    FBS.lock().retain(|e| e.0 != id);
    let Some(holder) = gpu::owner::holder(&*f.dev).filter(|_| disable) else { return Ok(()) };
    let (cs, ps) = f.dev.state();
    let using: Vec<&PlaneState> = ps.iter().filter(|p| p.fb.as_ref().is_some_and(|x| Arc::ptr_eq(x, &fb))).collect();
    if using.is_empty() {
        return Ok(());
    }
    let planes = f.dev.planes();
    let mut c = Commit { flags: commit::ALLOW_MODESET, ..Commit::default() };
    for p in using {
        let primary = planes.iter().any(|i| i.id == p.plane && i.kind == PlaneKind::Primary);
        match (primary, p.crtc) {
            // Without its primary plane the CRTC goes off, and all its planes.
            (true, Some(crtc)) => {
                c.crtcs.push(CrtcState { crtc, active: false, mode: None, connectors: Vec::new() });
                for q in ps.iter().filter(|q| q.crtc == Some(crtc)) {
                    c.planes.push(PlaneState::disabled(q.plane));
                }
            }
            _ => c.planes.push(PlaneState::disabled(p.plane)),
        }
    }
    let _ = cs;
    gpu::owner::client_commit(&*f.dev, holder, &c).map_err(errno)
}

/// `GETFB`: the buffer's handle only for the master (as on Linux).
pub fn get_fb(f: &File, a: &mut Args) -> Result<(), isize> {
    let fb = fb_by_id(f, a.u32(0)).ok_or(ENOENT)?;
    let (cpp, depth) = format_info(fb.format).unwrap_or((4, 24));
    a.set32(4, fb.width);
    a.set32(8, fb.height);
    a.set32(12, fb.pitch);
    a.set32(16, cpp * 8);
    a.set32(20, depth);
    a.set32(24, if f.is_master() { f.handle_for(&fb.bo) } else { 0 });
    Ok(())
}

pub fn get_fb2(f: &File, a: &mut Args) -> Result<(), isize> {
    let fb = fb_by_id(f, a.u32(0)).ok_or(ENOENT)?;
    a.set32(4, fb.width);
    a.set32(8, fb.height);
    a.set32(12, fb.format);
    // DRM_MODE_FB_MODIFIERS, with the linear modifier (0) in `modifier[0]`.
    a.set32(16, 2);
    a.set32(20, if f.is_master() { f.handle_for(&fb.bo) } else { 0 });
    a.set32(36, fb.pitch);
    a.set32(52, fb.offset);
    a.set64(72, 0);
    Ok(())
}

/// `struct drm_mode_fb_dirty_cmd`: fb_id, flags, color, num_clips, clips_ptr.
pub fn dirty_fb(f: &File, a: &mut Args) -> Result<(), isize> {
    const ANNOTATE_COPY: u32 = 1;
    const ANNOTATE_FILL: u32 = 2;
    let fb = fb_by_id(f, a.u32(0)).ok_or(ENOENT)?;
    let (flags, count, clips) = (a.u32(4), a.u32(12), a.u64(16));
    if flags & !(ANNOTATE_COPY | ANNOTATE_FILL) != 0 || count > 256 || (count > 0 && clips == 0) {
        return Err(EINVAL);
    }
    if flags & ANNOTATE_COPY != 0 && count % 2 != 0 {
        return Err(EINVAL);
    }
    // struct drm_clip_rect { u16 x1, y1, x2, y2 }
    let raw: Vec<[u16; 4]> = get_array(clips, count)?;
    let mut rects: Vec<gpu::Damage> = raw
        .iter()
        .filter(|r| r[2] > r[0] && r[3] > r[1])
        .map(|r| gpu::Damage { x: r[0] as u32, y: r[1] as u32, w: (r[2] - r[0]) as u32, h: (r[3] - r[1]) as u32 })
        .collect();
    if count == 0 {
        rects.push(gpu::Damage { x: 0, y: 0, w: fb.width, h: fb.height });
    }
    // Only what a program holds is shown; the desktop's buffers are its own.
    if gpu::owner::holder(&*f.dev) == Some(f.id) {
        f.dev.dirty(&fb, &rects).map_err(errno)?;
    }
    Ok(())
}

// ------------------------------------------------------------ dumb buffers

/// `struct drm_mode_create_dumb`: height, width, bpp, flags, handle, pitch, size.
pub fn create_dumb(f: &File, a: &mut Args) -> Result<(), isize> {
    let (h, w, bpp, flags) = (a.u32(0), a.u32(4), a.u32(8), a.u32(12));
    if flags != 0 || w == 0 || h == 0 || bpp == 0 || bpp > 32 {
        return Err(EINVAL);
    }
    let cpp = bpp.div_ceil(8);
    let pitch = w.checked_mul(cpp).ok_or(EINVAL)?;
    let size = (pitch as u64).checked_mul(h as u64).ok_or(EINVAL)?;
    if size > 1 << 30 {
        return Err(EINVAL);
    }
    // 32-bit buffers come from the device (it may want them its own way).
    let (bo, pitch) = if cpp == 4 && w <= 16384 && h <= 16384 {
        let fb = f.dev.create_dumb(w, h, fourcc::XRGB8888).map_err(errno)?;
        (fb.bo.clone(), fb.pitch)
    } else {
        (BufferObject::alloc_system(size as usize).map_err(errno)?, pitch)
    };
    a.set32(16, f.handle_for(&bo));
    a.set32(20, pitch);
    a.set64(24, bo.size as u64);
    Ok(())
}

/// `struct drm_mode_map_dumb`: handle, pad, offset (what `mmap` takes).
pub fn map_dumb(f: &File, a: &mut Args) -> Result<(), isize> {
    let bo = f.bo(a.u32(0)).ok_or(ENOENT)?;
    a.set64(8, (bo.id as u64) << 32);
    Ok(())
}

// ------------------------------------------------------------ queries

fn connector_type(k: ConnectorKind) -> u32 {
    match k {
        ConnectorKind::Unknown => 0,
        ConnectorKind::Vga => 1,
        ConnectorKind::DviD => 3,
        ConnectorKind::DisplayPort => 10,
        ConnectorKind::HdmiA => 11,
        ConnectorKind::Edp => 14,
        ConnectorKind::Virtual => 15,
    }
}

fn encoder_type(k: EncoderKind) -> u32 {
    match k {
        EncoderKind::None => 0,
        EncoderKind::Dac => 1,
        EncoderKind::Tmds => 2,
        EncoderKind::Lvds => 3,
        EncoderKind::Virtual => 5,
        EncoderKind::DpMst => 7,
    }
}

/// `struct drm_mode_card_res`.
pub fn get_resources(f: &File, a: &mut Args) -> Result<(), isize> {
    let dev = &*f.dev;
    let fbs: Vec<u32> = f.state.lock().fbs.iter().map(|fb| fb.id).collect();
    let crtcs: Vec<u32> = dev.crtcs().iter().map(|c| c.id).collect();
    let conns: Vec<u32> = dev.connectors().iter().map(|c| c.id).collect();
    let encs: Vec<u32> = dev.encoders().iter().map(|e| e.id).collect();
    for (i, list) in [&fbs, &crtcs, &conns, &encs].into_iter().enumerate() {
        put_array(a.u64(8 * i), a.u32(32 + 4 * i), list)?;
        a.set32(32 + 4 * i, list.len() as u32);
    }
    let max = dev.caps().max_size;
    a.set32(48, 1);
    a.set32(52, max.0);
    a.set32(56, 1);
    a.set32(60, max.1);
    Ok(())
}

/// The primary plane feeding `crtc`.
fn primary_of(dev: &dyn KmsDevice, crtc: ObjId) -> Option<ObjId> {
    let index = dev.crtcs().iter().find(|c| c.id == crtc)?.index;
    dev.planes().iter().find(|p| p.kind == PlaneKind::Primary && p.possible_crtcs & (1 << index) != 0).map(|p| p.id)
}

fn cursor_of(dev: &dyn KmsDevice, crtc: ObjId) -> Option<ObjId> {
    let index = dev.crtcs().iter().find(|c| c.id == crtc)?.index;
    dev.planes().iter().find(|p| p.kind == PlaneKind::Cursor && p.possible_crtcs & (1 << index) != 0).map(|p| p.id)
}

/// `struct drm_mode_crtc`: set_connectors_ptr, count_connectors, crtc_id,
/// fb_id, x, y, gamma_size, mode_valid, mode.
pub fn get_crtc(f: &File, a: &mut Args) -> Result<(), isize> {
    let dev = &*f.dev;
    let id = a.u32(12);
    if !dev.crtcs().iter().any(|c| c.id == id) {
        return Err(ENOENT);
    }
    let (cs, ps) = dev.state();
    let s = cs.iter().find(|s| s.crtc == id).ok_or(ENOENT)?;
    let primary = primary_of(dev, id).and_then(|p| ps.iter().find(|s| s.plane == p));
    a.set32(8, 0);
    a.set32(16, primary.and_then(|p| p.fb.as_ref()).map_or(0, |fb| fb.id));
    a.set32(20, primary.map_or(0, |p| p.src.0 >> 16));
    a.set32(24, primary.map_or(0, |p| p.src.1 >> 16));
    a.set32(28, 0);
    match (s.active, s.mode) {
        (true, Some(m)) => {
            a.set32(32, 1);
            a.bytes(36, 68).copy_from_slice(&props::modeinfo(&m));
        }
        _ => {
            a.set32(32, 0);
            a.bytes(36, 68).fill(0);
        }
    }
    Ok(())
}

/// `struct drm_mode_get_encoder`: encoder_id, encoder_type, crtc_id,
/// possible_crtcs, possible_clones.
pub fn get_encoder(f: &File, a: &mut Args) -> Result<(), isize> {
    let dev = &*f.dev;
    let encoders = dev.encoders();
    let (i, e) = encoders.iter().enumerate().find(|(_, e)| e.id == a.u32(0)).ok_or(ENOENT)?;
    let conns = dev.connectors();
    let (cs, _) = dev.state();
    // The CRTC driving a connector this encoder serves.
    let crtc = cs
        .iter()
        .find(|s| s.connectors.iter().any(|c| conns.iter().any(|k| k.id == *c && k.encoders.contains(&e.id))))
        .map_or(0, |s| s.crtc);
    a.set32(4, encoder_type(e.kind));
    a.set32(8, crtc);
    a.set32(12, e.possible_crtcs);
    a.set32(16, 1 << i);
    Ok(())
}

/// `struct drm_mode_get_connector`. Asking for no modes probes again.
pub fn get_connector(f: &File, a: &mut Args) -> Result<(), isize> {
    let dev = &*f.dev;
    let id = a.u32(48);
    let room_modes = a.u32(32);
    let conns = if room_modes == 0 { dev.probe() } else { dev.connectors() };
    let c = conns.iter().find(|c| c.id == id).ok_or(ENOENT)?;
    // Modes and encoders: all, or none if they don't fit.
    if room_modes as usize >= c.modes.len() && !c.modes.is_empty() {
        for (i, m) in c.modes.iter().enumerate() {
            crate::syscall::user::put_bytes(a.u64(8) + i as u64 * 68, &props::modeinfo(m))?;
        }
    }
    a.set32(32, c.modes.len() as u32);
    if a.u32(40) as usize >= c.encoders.len() {
        put_array(a.u64(0), a.u32(40), &c.encoders)?;
    }
    a.set32(40, c.encoders.len() as u32);
    // Properties, through the same path as OBJ_GETPROPERTIES.
    let mut sub = [0u8; 32];
    sub[0..8].copy_from_slice(&a.u64(16).to_le_bytes());
    sub[8..16].copy_from_slice(&a.u64(24).to_le_bytes());
    sub[16..20].copy_from_slice(&a.u32(36).to_le_bytes());
    sub[20..24].copy_from_slice(&id.to_le_bytes());
    sub[24..28].copy_from_slice(&props::OBJECT_CONNECTOR.to_le_bytes());
    let mut sa = Args(&mut sub);
    props::obj_get_properties(f, &mut sa)?;
    a.set32(36, sa.u32(16));
    let (cs, _) = dev.state();
    let encoder = cs.iter().any(|s| s.active && s.connectors.contains(&id)).then(|| c.encoders.first().copied());
    a.set32(44, encoder.flatten().unwrap_or(0));
    a.set32(52, connector_type(c.kind));
    a.set32(56, c.kind_index);
    a.set32(
        60,
        match c.status {
            Status::Connected => 1,
            Status::Disconnected => 2,
            Status::Unknown => 3,
        },
    );
    a.set32(64, c.mm.0);
    a.set32(68, c.mm.1);
    a.set32(72, 0);
    a.set32(76, 0);
    Ok(())
}

/// `struct drm_mode_get_plane_res`. Clients that did not ask for universal
/// planes see the overlays only.
pub fn get_plane_resources(f: &File, a: &mut Args) -> Result<(), isize> {
    let universal = f.state.lock().caps.universal_planes;
    let ids: Vec<u32> =
        f.dev.planes().iter().filter(|p| universal || p.kind == PlaneKind::Overlay).map(|p| p.id).collect();
    put_array(a.u64(0), a.u32(8), &ids)?;
    a.set32(8, ids.len() as u32);
    Ok(())
}

/// `struct drm_mode_get_plane`: plane_id, crtc_id, fb_id, possible_crtcs,
/// gamma_size, count_format_types, format_type_ptr.
pub fn get_plane(f: &File, a: &mut Args) -> Result<(), isize> {
    let planes = f.dev.planes();
    let info = planes.iter().find(|p| p.id == a.u32(0)).ok_or(ENOENT)?;
    let (_, ps) = f.dev.state();
    let s = ps.iter().find(|s| s.plane == info.id);
    a.set32(4, s.and_then(|s| s.crtc).unwrap_or(0));
    a.set32(8, s.and_then(|s| s.fb.as_ref()).map_or(0, |fb| fb.id));
    a.set32(12, info.possible_crtcs);
    a.set32(16, 0);
    if a.u32(20) as usize >= info.formats.len() {
        put_array(a.u64(24), a.u32(20), info.formats)?;
    }
    a.set32(20, info.formats.len() as u32);
    Ok(())
}

/// `struct drm_mode_crtc_lut`: no gamma tables (`gamma_size` 0), as Linux
/// answers for devices without them (`SETGAMMA`: `ENOSYS`).
pub fn gamma(f: &File, a: &mut Args, set: bool) -> Result<(), isize> {
    if !f.dev.crtcs().iter().any(|c| c.id == a.u32(0)) {
        return Err(ENOENT);
    }
    if set {
        return Err(ENOSYS);
    }
    if a.u32(4) != 0 {
        return Err(EINVAL);
    }
    Ok(())
}

// ------------------------------------------------------------ changes

pub fn errno(e: gpu::Error) -> isize {
    match e {
        // What the device can't do is a configuration it refuses.
        gpu::Error::Unsupported => EINVAL,
        e => e.errno(),
    }
}

/// Makes a commit for the file's program: waits for (or with `NONBLOCK`,
/// refuses) CRTCs still flipping, commits through `gpu::owner`, and paces
/// the CRTCs it touched to the next frame (with a flip-complete event for
/// `event`'s user data).
pub fn commit(f: &File, c: Commit, touched: &[ObjId], event: Option<u64>) -> Result<(), isize> {
    let start = super::stats::now();
    let test = c.flags & commit::TEST_ONLY != 0;
    if !test {
        for &crtc in touched {
            if vblank::flip_pending(crtc) {
                if c.flags & commit::NONBLOCK != 0 {
                    return Err(EBUSY);
                }
                vblank::wait_flip(crtc)?;
            }
        }
    }
    gpu::owner::client_commit(&*f.dev, f.id, &c).map_err(errno)?;
    if !test {
        let mut due = 0;
        for &crtc in touched {
            due = due.max(vblank::flipped(f, crtc, event)?);
        }
        f.flips.lock().record(start, super::stats::now(), due);
        // A blocking commit returns once its frame is on screen, as on Linux.
        if c.flags & commit::NONBLOCK == 0 {
            for &crtc in touched {
                vblank::wait_flip(crtc)?;
            }
        }
    }
    Ok(())
}

/// `SETCRTC`: a mode, connectors and a framebuffer for a CRTC (or off).
pub fn set_crtc(f: &File, a: &mut Args) -> Result<(), isize> {
    let dev = &*f.dev;
    let (conns_ptr, count, crtc, fb_id, x, y, valid) =
        (a.u64(0), a.u32(8), a.u32(12), a.u32(16), a.u32(20), a.u32(24), a.u32(32) != 0);
    if !dev.crtcs().iter().any(|c| c.id == crtc) {
        return Err(ENOENT);
    }
    let primary = primary_of(dev, crtc).ok_or(EINVAL)?;
    let (cs, ps) = dev.state();
    let mut req = Commit { flags: commit::ALLOW_MODESET, ..Commit::default() };
    if !valid {
        if count > 0 {
            return Err(EINVAL);
        }
        req.crtcs.push(CrtcState { crtc, active: false, mode: None, connectors: Vec::new() });
        for p in ps.iter().filter(|p| p.crtc == Some(crtc)) {
            req.planes.push(PlaneState::disabled(p.plane));
        }
        return commit(f, req, &[crtc], None);
    }
    let mode = props::mode_from(a.bytes(36, 68))?;
    let fb = if fb_id == u32::MAX {
        ps.iter().find(|p| p.plane == primary).and_then(|p| p.fb.clone()).ok_or(EINVAL)?
    } else {
        fb_by_id(f, fb_id).ok_or(ENOENT)?
    };
    if count == 0 {
        return Err(EINVAL);
    }
    let conns: Vec<u32> = get_array(conns_ptr, count)?;
    let known = dev.connectors();
    if !conns.iter().all(|c| known.iter().any(|k| k.id == *c)) {
        return Err(ENOENT);
    }
    // The picture must cover the mode from (x, y).
    if x as u64 + mode.hdisplay as u64 > fb.width as u64 || y as u64 + mode.vdisplay as u64 > fb.height as u64 {
        return Err(ENOSPC);
    }
    // The connectors leave the CRTCs they were on.
    let mut touched = alloc::vec![crtc];
    for s in cs.iter().filter(|s| s.crtc != crtc && s.connectors.iter().any(|c| conns.contains(c))) {
        let mut s = s.clone();
        s.connectors.retain(|c| !conns.contains(c));
        touched.push(s.crtc);
        req.crtcs.push(s);
    }
    req.crtcs.push(CrtcState { crtc, active: true, mode: Some(mode), connectors: conns });
    let mut p = PlaneState::show(primary, crtc, &fb, 0, 0);
    p.src = (x << 16, y << 16, mode.hdisplay << 16, mode.vdisplay << 16);
    p.dst = (0, 0, mode.hdisplay, mode.vdisplay);
    req.planes.push(p);
    commit(f, req, &touched, None)
}

/// `struct drm_mode_set_plane`: plane_id, crtc_id, fb_id, flags, crtc_x,
/// crtc_y, crtc_w, crtc_h, src_x, src_y, src_h, src_w.
pub fn set_plane(f: &File, a: &mut Args) -> Result<(), isize> {
    let dev = &*f.dev;
    let plane = a.u32(0);
    if !dev.planes().iter().any(|p| p.id == plane) {
        return Err(ENOENT);
    }
    let old_crtc = dev.state().1.iter().find(|p| p.plane == plane).and_then(|p| p.crtc);
    if a.u32(8) == 0 {
        let c = Commit { planes: alloc::vec![PlaneState::disabled(plane)], ..Commit::default() };
        return commit(f, c, old_crtc.as_slice(), None);
    }
    let crtc = a.u32(4);
    if !dev.crtcs().iter().any(|c| c.id == crtc) {
        return Err(ENOENT);
    }
    let fb = fb_by_id(f, a.u32(8)).ok_or(ENOENT)?;
    let (src_x, src_y, src_h, src_w) = (a.u32(32), a.u32(36), a.u32(40), a.u32(44));
    let (fw, fh) = ((fb.width as u64) << 16, (fb.height as u64) << 16);
    if src_w as u64 > fw || src_x as u64 > fw - src_w as u64 || src_h as u64 > fh || src_y as u64 > fh - src_h as u64 {
        return Err(ENOSPC);
    }
    let (cx, cy, cw, ch) = (a.i32(16), a.i32(20), a.u32(24), a.u32(28));
    if cw > i32::MAX as u32
        || ch > i32::MAX as u32
        || cx.checked_add(cw as i32).is_none()
        || cy.checked_add(ch as i32).is_none()
    {
        return Err(ERANGE);
    }
    let mut p = PlaneState::show(plane, crtc, &fb, cx, cy);
    p.src = (src_x, src_y, src_w, src_h);
    p.dst = (cx, cy, cw, ch);
    let mut touched = alloc::vec![crtc];
    touched.extend(old_crtc.filter(|&c| c != crtc));
    commit(f, Commit { planes: alloc::vec![p], ..Commit::default() }, &touched, None)
}

/// Where each CRTC's cursor was put last (for image-only updates).
static CURSOR_AT: Mutex<Vec<(ObjId, (i32, i32))>> = Mutex::new(Vec::new());

/// `struct drm_mode_cursor` (`cursor2` adds hot_x, hot_y): flags (BO 1,
/// MOVE 2), crtc_id, x, y, width, height, handle.
pub fn cursor(f: &File, a: &mut Args, with_hotspot: bool) -> Result<(), isize> {
    const BO: u32 = 1;
    const MOVE: u32 = 2;
    let dev = &*f.dev;
    let (flags, crtc) = (a.u32(0), a.u32(4));
    if flags == 0 || flags & !(BO | MOVE) != 0 {
        return Err(EINVAL);
    }
    if !dev.crtcs().iter().any(|c| c.id == crtc) {
        return Err(ENOENT);
    }
    let plane = cursor_of(dev, crtc).ok_or(ENXIO)?;
    let (_, ps) = dev.state();
    let cur = ps.iter().find(|p| p.plane == plane).cloned().unwrap_or_else(|| PlaneState::disabled(plane));
    let (fb, hot) = if flags & BO != 0 {
        match a.u32(24) {
            0 => (None, (0, 0)),
            handle => {
                let fb = make_fb(f, a.u32(16), a.u32(20), fourcc::ARGB8888, handle, a.u32(16) * 4, 0)?;
                let hot = if with_hotspot { (a.i32(28), a.i32(32)) } else { (0, 0) };
                (Some(fb), hot)
            }
        }
    } else {
        (cur.fb.clone(), cur.hotspot)
    };
    let pos = {
        let mut at = CURSOR_AT.lock();
        let i = match at.iter().position(|e| e.0 == crtc) {
            Some(i) => i,
            None => {
                at.push((crtc, (0, 0)));
                at.len() - 1
            }
        };
        if flags & MOVE != 0 {
            at[i].1 = (a.i32(8), a.i32(12));
        }
        at[i].1
    };
    let state = match &fb {
        Some(fb) => {
            let mut s = PlaneState::show(plane, crtc, fb, pos.0, pos.1);
            s.hotspot = hot;
            s
        }
        None => PlaneState::disabled(plane),
    };
    // Cursor updates are not paced to frames (Linux's legacy cursor path).
    let c = Commit { planes: alloc::vec![state], ..Commit::default() };
    gpu::owner::client_commit(dev, f.id, &c).map_err(errno)
}

/// `struct drm_mode_crtc_page_flip`: crtc_id, fb_id, flags, reserved
/// (target), user_data.
pub fn page_flip(f: &File, a: &mut Args) -> Result<(), isize> {
    const EVENT: u32 = 1;
    const ASYNC: u32 = 2;
    const TARGET: u32 = 4 | 8;
    let dev = &*f.dev;
    let (crtc, fb_id, flags, target, user_data) = (a.u32(0), a.u32(4), a.u32(8), a.u32(12), a.u64(16));
    if flags & !(EVENT | ASYNC | TARGET) != 0 || flags & (ASYNC | TARGET) != 0 || target != 0 {
        return Err(EINVAL);
    }
    if !dev.crtcs().iter().any(|c| c.id == crtc) {
        return Err(ENOENT);
    }
    let fb = fb_by_id(f, fb_id).ok_or(ENOENT)?;
    let primary = primary_of(dev, crtc).ok_or(EINVAL)?;
    let (cs, ps) = dev.state();
    let s = cs.iter().find(|s| s.crtc == crtc).ok_or(ENOENT)?;
    let cur = ps.iter().find(|p| p.plane == primary).cloned().ok_or(EINVAL)?;
    // Nothing shown (yet): a flip has nothing to replace.
    let (Some(mode), true, Some(_)) = (s.mode, s.active, cur.fb.as_ref()) else { return Err(EBUSY) };
    let (x, y) = (cur.src.0 >> 16, cur.src.1 >> 16);
    if x as u64 + mode.hdisplay as u64 > fb.width as u64 || y as u64 + mode.vdisplay as u64 > fb.height as u64 {
        return Err(ENOSPC);
    }
    let mut p = cur.clone();
    p.fb = Some(fb);
    let c = Commit { planes: alloc::vec![p], flags: commit::NONBLOCK, ..Commit::default() };
    commit(f, c, &[crtc], (flags & EVENT != 0).then_some(user_data))
}
