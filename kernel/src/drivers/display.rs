//! Display modes.
//!
//! On the Bochs/QEMU "std VGA" family (also VirtualBox's) the resolution can
//! change at run time through the DISPI registers. Everywhere else the mode is
//! set by the firmware (GOP) at boot: Settings writes the choice to
//! `\aurora\boot.conf` and the bootloader applies it on the next start.

use crate::sync::IrqMutex;
use alloc::vec::Vec;
use aurora_abi::{display, DisplayMode};
use bootinfo::{BootInfo, Framebuffer, PixelFormat, VideoMode};
use x86_64::instructions::port::Port;

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

/// Resolutions offered when the mode can be changed at run time.
const LIVE_MODES: [(u32, u32); 11] = [
    (1024, 768),
    (1152, 864),
    (1280, 720),
    (1280, 800),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
];

struct State {
    /// Physical address and size of the Bochs framebuffer (BAR 0), if present.
    bochs: Option<(u64, u64)>,
    firmware_modes: Vec<VideoMode>,
}

static STATE: IrqMutex<State> = IrqMutex::new(State { bochs: None, firmware_modes: Vec::new() });

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

/// Finds the display hardware (after PCI enumeration).
pub fn init(boot: &BootInfo) {
    let firmware_modes = boot.modes[..(boot.mode_count as usize).min(boot.modes.len())].to_vec();
    let vga = crate::drivers::pci::devices()
        .into_iter()
        .find(|d| (d.vendor, d.device) == (0x1234, 0x1111) || (d.vendor, d.device) == (0x80EE, 0xBEEF));
    let bochs = vga.and_then(|d| {
        let id = dispi_read(REG_ID);
        if !(0xB0C0..=0xB0CF).contains(&id) {
            return None;
        }
        let vram = dispi_read(REG_VIDEO_MEMORY_64K) as u64 * 65536;
        match d.bars[0] {
            crate::drivers::pci::Bar::Memory { phys, size } => Some((phys, vram.max(size.min(256 << 20)))),
            _ => None,
        }
    });
    match bochs {
        Some((phys, vram)) => {
            log!("display", "Bochs VBE at {:#x}, {} MiB video memory: live mode changes", phys, vram >> 20)
        }
        None => log!("display", "{} firmware mode(s); changes apply at the next start", firmware_modes.len()),
    }
    let mut s = STATE.lock();
    s.bochs = bochs;
    s.firmware_modes = firmware_modes;
}

pub fn live() -> bool {
    STATE.lock().bochs.is_some()
}

/// The resolutions to offer, with the current one flagged.
pub fn modes(current: (u32, u32), at_boot: Option<(u32, u32)>) -> Vec<DisplayMode> {
    let s = STATE.lock();
    let mut out: Vec<DisplayMode> = match s.bochs {
        Some((_, vram)) => LIVE_MODES
            .iter()
            .filter(|(w, h)| (*w as u64) * (*h as u64) * 4 <= vram)
            .map(|&(width, height)| DisplayMode { width, height, flags: display::LIVE, _pad: 0 })
            .collect(),
        None => s
            .firmware_modes
            .iter()
            .filter(|m| m.width >= 800 && m.height >= 600)
            .map(|m| DisplayMode { width: m.width, height: m.height, flags: 0, _pad: 0 })
            .collect(),
    };
    if !out.iter().any(|m| (m.width, m.height) == current) {
        out.push(DisplayMode { width: current.0, height: current.1, flags: 0, _pad: 0 });
    }
    for m in &mut out {
        if (m.width, m.height) == current {
            m.flags |= display::CURRENT;
        }
        if Some((m.width, m.height)) == at_boot {
            m.flags |= display::AT_BOOT;
        }
    }
    out.sort_by_key(|m| (m.width, m.height));
    out
}

/// After sleep the adapter is back in a firmware mode: restore `w`×`h`.
/// (With only a firmware framebuffer there is nothing we can do; on real
/// machines without a native driver the screen may stay dark.)
pub fn resume(w: u32, h: u32) {
    if live() && set_mode(w, h).is_none() {
        log!("display", "could not restore {}x{} after sleep", w, h);
    }
}

/// Switches to `w`×`h` now (Bochs VBE only). Returns the new framebuffer.
pub fn set_mode(w: u32, h: u32) -> Option<Framebuffer> {
    let (phys, vram) = STATE.lock().bochs?;
    if (w as u64) * (h as u64) * 4 > vram || w < 640 || h < 480 || w > 4096 || h > 4096 {
        return None;
    }
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
        use x86_64::instructions::port::Port;
        let _ = Port::<u8>::new(0x3DA).read(); // reset the index/data flip-flop
        Port::<u8>::new(0x3C0).write(0x20);
    }
    if (dispi_read(REG_XRES), dispi_read(REG_YRES)) != (w as u16, h as u16) {
        return None;
    }
    // The framebuffer sits below 4 GiB, inside the physical-memory window.
    crate::mm::paging::map_mmio(phys, w as u64 * h as u64 * 4);
    log!("display", "switched to {}x{}", w, h);
    Some(Framebuffer {
        phys_addr: phys,
        size: w as u64 * h as u64 * 4,
        width: w,
        height: h,
        stride: w,
        format: PixelFormat::Bgr,
    })
}
