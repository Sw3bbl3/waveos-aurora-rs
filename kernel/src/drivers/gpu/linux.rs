//! The screen of the ported Linux virtio-gpu driver (`crate::linux`, run
//! through the LinuxKPI layer), as a display device. Selected with
//! `gpu=linux` in boot.conf (`cargo xtask run --gpu virtio-linux`); the
//! native driver (`super::virtio`) is the default.
//!
//! The Linux driver keeps its own shadow framebuffer: `show` copies the
//! damaged rectangles into it and asks the driver to flush them.

use super::simple::{Backend, Simple};
use super::*;

pub struct LinuxDrm;

impl Backend for LinuxDrm {
    fn name(&self) -> &str {
        "linux-virtio-gpu"
    }
    fn caps(&self) -> Caps {
        Caps { live_modeset: true, max_size: (8192, 8192), needs_dirty: true, follows_host: true, ..Caps::default() }
    }
    fn connector_kind(&self) -> ConnectorKind {
        ConnectorKind::Virtual
    }
    fn modes(&self) -> Vec<Mode> {
        crate::linux::display_modes()
            .iter()
            .filter(|m| m.width >= 800 && m.height >= 600)
            .filter_map(|m| {
                let hz = if m.refresh == 0 { 60 } else { m.refresh };
                Some(Mode::cvt(m.width, m.height, hz)?.preferred(m.preferred != 0))
            })
            .collect()
    }
    fn current(&self) -> Mode {
        let d = crate::linux::display();
        Mode::fixed(d.map_or(1024, |d| d.width), d.map_or(768, |d| d.height))
    }
    fn set_mode(&self, mode: &Mode) -> Result<()> {
        let (w, h) = mode.size();
        crate::linux::display_set_mode(w, h).ok_or(Error::Io)?;
        log!("display", "switched to {}x{}", w, h);
        Ok(())
    }
    fn show(&self, fb: &Framebuffer, rects: &[Damage]) -> Result<()> {
        let d = crate::linux::display().ok_or(Error::Io)?;
        let src = fb.read_pixels();
        let (src_stride, stride) = ((fb.pitch / 4) as usize, (d.pitch / 4) as usize);
        let front = unsafe { core::slice::from_raw_parts_mut(d.pixels, stride * d.height as usize) };
        for r in rects {
            // XRGB8888 in memory is the framebuffer's own layout.
            let r = r.clamp(fb.width.min(d.width), fb.height.min(d.height));
            if r.is_empty() {
                continue;
            }
            for y in r.y as usize..(r.y + r.h) as usize {
                front[y * stride + r.x as usize..y * stride + (r.x + r.w) as usize]
                    .copy_from_slice(&src[y * src_stride + r.x as usize..y * src_stride + (r.x + r.w) as usize]);
            }
            crate::linux::display_flush(r.x as i32, r.y as i32, r.w as i32, r.h as i32);
        }
        Ok(())
    }
    fn resume(&self, mode: &Mode) {
        if self.set_mode(mode).is_err() {
            log!("display", "could not restore {}x{} after sleep", mode.hdisplay, mode.vdisplay);
        }
    }
}

/// Registers the Linux driver's screen, if it brought one up.
pub fn register() -> bool {
    if crate::linux::display().is_none() {
        return false;
    }
    super::register(Arc::new(Simple::new(LinuxDrm)));
    true
}
