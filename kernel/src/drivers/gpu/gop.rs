//! The firmware's framebuffer (UEFI GOP), as a display device: one fixed
//! mode, whatever the bootloader chose. Retired as soon as a native
//! driver for the screen registers. Other modes the firmware offered
//! apply at the next start (`display::modes` lists them).

use super::simple::{Backend, Simple, Vram};
use super::*;

pub struct Gop {
    fb: bootinfo::Framebuffer,
    vram: Vram,
}

impl Backend for Gop {
    fn name(&self) -> &str {
        "efi-gop"
    }
    fn caps(&self) -> Caps {
        Caps { max_size: (self.fb.width, self.fb.height), needs_dirty: true, ..Caps::default() }
    }
    fn firmware(&self) -> bool {
        true
    }
    fn connector_kind(&self) -> ConnectorKind {
        ConnectorKind::Unknown
    }
    fn modes(&self) -> Vec<Mode> {
        alloc::vec![self.current()]
    }
    fn current(&self) -> Mode {
        Mode::fixed(self.fb.width, self.fb.height).preferred(true)
    }
    fn set_mode(&self, mode: &Mode) -> Result<()> {
        if mode.size() == (self.fb.width, self.fb.height) {
            Ok(())
        } else {
            Err(Error::Unsupported)
        }
    }
    fn show(&self, fb: &Framebuffer, rects: &[Damage]) -> Result<()> {
        self.vram.copy(fb, rects, self.fb.stride, self.fb.height, self.fb.format);
        Ok(())
    }
    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        Some(self.fb)
    }
}

/// Registers the firmware framebuffer.
pub fn register(fb: bootinfo::Framebuffer) {
    if fb.phys_addr == 0 || fb.width == 0 {
        return;
    }
    let vram = Vram::map(fb.phys_addr, fb.stride as u64 * fb.height as u64 * 4);
    super::register(Arc::new(Simple::new(Gop { fb, vram })));
}
