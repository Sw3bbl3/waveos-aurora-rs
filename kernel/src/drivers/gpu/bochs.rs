//! Bochs/QEMU "std VGA" (and VirtualBox's VGA): the DISPI registers set
//! any mode the video memory holds, at run time. One CRTC, no cursor.

use super::simple::{Backend, Simple, Vram};
use super::*;
use bootinfo::PixelFormat;
use crate::arch::io::Port;

const DISPI_INDEX: u16 = 0x01CE;
const DISPI_DATA: u16 = 0x01CF;
const REG_ID: u16 = 0;
const REG_XRES: u16 = 1;
const REG_YRES: u16 = 2;
const REG_BPP: u16 = 3;
const REG_ENABLE: u16 = 4;
const REG_VIRT_WIDTH: u16 = 6;
const REG_VIRT_HEIGHT: u16 = 7;
const REG_X_OFFSET: u16 = 8;
const REG_Y_OFFSET: u16 = 9;
const REG_VIDEO_MEMORY_64K: u16 = 0xA;
const ENABLED: u16 = 0x01;
const LFB: u16 = 0x40;

fn dispi_read(reg: u16) -> u16 {
    unsafe {
        Port::<u16>::new(DISPI_INDEX).write(reg);
        Port::<u16>::new(DISPI_DATA).read()
    }
}

fn dispi_write(reg: u16, v: u16) {
    unsafe {
        Port::<u16>::new(DISPI_INDEX).write(reg);
        Port::<u16>::new(DISPI_DATA).write(v);
    }
}

pub struct Bochs {
    vram: Vram,
    /// The mode at start (the firmware's).
    boot: (u32, u32),
    /// The current width, height and stride (pixels per row).
    size: IrqMutex<(u32, u32, u32)>,
}

impl Bochs {
    /// The size the registers report now.
    pub fn registers() -> (u32, u32) {
        (dispi_read(REG_XRES) as u32, dispi_read(REG_YRES) as u32)
    }
}

impl Backend for Bochs {
    fn name(&self) -> &str {
        "bochs-vga"
    }
    fn caps(&self) -> Caps {
        Caps { live_modeset: true, max_size: (4096, 4096), needs_dirty: true, ..Caps::default() }
    }
    fn connector_kind(&self) -> ConnectorKind {
        ConnectorKind::Virtual
    }
    fn modes(&self) -> Vec<Mode> {
        let fits = |w: u32, h: u32| w as u64 * h as u64 * 4 <= self.vram.size;
        let mut modes: Vec<Mode> =
            mode::COMMON.iter().filter(|&&(w, h)| fits(w, h)).filter_map(|&(w, h)| Mode::cvt(w, h, 60)).collect();
        // The firmware's mode is the one the screen prefers (OVMF follows
        // QEMU's EDID): marked so, as Linux's bochs-drm marks one, so that
        // programs choosing "the preferred mode" don't take the largest.
        match modes.iter_mut().find(|m| m.size() == self.boot) {
            Some(m) => *m = m.preferred(true),
            None => modes.push(Mode::fixed(self.boot.0, self.boot.1).preferred(true)),
        }
        modes
    }
    fn current(&self) -> Mode {
        Mode::fixed(self.boot.0, self.boot.1)
    }
    fn set_mode(&self, mode: &Mode) -> Result<()> {
        let (w, h) = mode.size();
        if (w as u64) * (h as u64) * 4 > self.vram.size || w < 640 || h < 480 || w > 4096 || h > 4096 {
            return Err(Error::Invalid);
        }
        program(w, h);
        if Bochs::registers() != (w, h) {
            return Err(Error::Io);
        }
        *self.size.lock() = (w, h, w);
        log!("display", "switched to {}x{}", w, h);
        Ok(())
    }
    fn show(&self, fb: &Framebuffer, rects: &[Damage]) -> Result<()> {
        let (_, h, stride) = *self.size.lock();
        self.vram.copy(fb, rects, stride, h, PixelFormat::Bgr);
        Ok(())
    }
    fn resume(&self, mode: &Mode) {
        // After sleep the adapter is back in a firmware mode.
        if self.set_mode(mode).is_err() {
            log!("display", "could not restore {}x{} after sleep", mode.hdisplay, mode.vdisplay);
        }
    }
    fn panic_framebuffer(&self) -> Option<bootinfo::Framebuffer> {
        let (w, h, stride) = *self.size.lock();
        Some(bootinfo::Framebuffer {
            phys_addr: self.vram.phys,
            size: stride as u64 * h as u64 * 4,
            width: w,
            height: h,
            stride,
            format: PixelFormat::Bgr,
        })
    }
}

fn program(w: u32, h: u32) {
    dispi_write(REG_ENABLE, 0);
    dispi_write(REG_XRES, w as u16);
    dispi_write(REG_YRES, h as u16);
    dispi_write(REG_BPP, 32);
    dispi_write(REG_VIRT_WIDTH, w as u16);
    dispi_write(REG_VIRT_HEIGHT, h as u16);
    dispi_write(REG_X_OFFSET, 0);
    dispi_write(REG_Y_OFFSET, 0);
    dispi_write(REG_ENABLE, ENABLED | LFB);
    // Display output on: the attribute controller's "palette address source"
    // bit (firmware sets it at boot; after sleep the adapter is reset).
    unsafe {
        let _ = Port::<u8>::new(0x3DA).read(); // reset the index/data flip-flop
        Port::<u8>::new(0x3C0).write(0x20);
    }
}

/// Finds a Bochs-compatible adapter (after PCI enumeration) and registers it.
pub fn probe(boot: &bootinfo::Framebuffer) -> bool {
    let vga = crate::drivers::pci::devices()
        .into_iter()
        .find(|d| (d.vendor, d.device) == (0x1234, 0x1111) || (d.vendor, d.device) == (0x80EE, 0xBEEF));
    let Some(d) = vga else { return false };
    let id = dispi_read(REG_ID);
    if !(0xB0C0..=0xB0CF).contains(&id) {
        return false;
    }
    let vram = dispi_read(REG_VIDEO_MEMORY_64K) as u64 * 65536;
    let crate::drivers::pci::Bar::Memory { phys, size } = d.bars[0] else { return false };
    let vram = vram.max(size.min(256 << 20));
    log!("display", "Bochs VBE at {:#x}, {} MiB video memory: live mode changes", phys, vram >> 20);
    let (w, h) = (boot.width, boot.height);
    let dev = Bochs { vram: Vram::map(phys, vram), boot: (w, h), size: IrqMutex::new((w, h, boot.stride)) };
    super::register(Arc::new(Simple::new(dev)));
    true
}
