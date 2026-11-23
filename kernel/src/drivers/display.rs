//! Display modes for Settings → Display, over the display core
//! (`drivers::gpu`).
//!
//! When the primary display device can change mode while running (Bochs
//! VGA, virtio-gpu, the ported Linux driver), its connector's modes are
//! offered and the switch happens at once. Otherwise the firmware's GOP
//! modes are offered: Settings writes the choice to `\aurora\boot.conf`
//! and the bootloader applies it on the next start.

use super::gpu;
use crate::sync::IrqMutex;
use alloc::vec::Vec;
use aurora_abi::{display, DisplayMode};
use bootinfo::{BootInfo, VideoMode};

static FIRMWARE_MODES: IrqMutex<Vec<VideoMode>> = IrqMutex::new(Vec::new());
/// The bootloader's framebuffer (which GPU scans it decides which driver
/// may take the display over).
static BOOT_FRAMEBUFFER: IrqMutex<Option<bootinfo::Framebuffer>> = IrqMutex::new(None);

/// Registers the boot display (after PCI enumeration): Bochs VGA when
/// present, else the firmware framebuffer. GPU drivers come later
/// ([`gpu_start`]) and take over from the firmware's.
pub fn init(boot: &BootInfo) {
    let firmware_modes = boot.modes[..(boot.mode_count as usize).min(boot.modes.len())].to_vec();
    let n = firmware_modes.len();
    *FIRMWARE_MODES.lock() = firmware_modes;
    *BOOT_FRAMEBUFFER.lock() = (boot.framebuffer.phys_addr != 0).then_some(boot.framebuffer);
    if !gpu::bochs::probe(&boot.framebuffer) {
        gpu::gop::register(boot.framebuffer);
        log!("display", "{} firmware mode(s); changes apply at the next start", n);
    }
}

/// Starts the GPU drivers. Needs the disks (`gpu=` in boot.conf chooses
/// the virtio-gpu driver: `native`, the default, or `linux` for the ported
/// Linux driver; `intel=` the Intel driver's policy: `off`, `takeover`, the
/// default, or `modeset`; `amd=off` keeps the AMD driver off; `nvidia=off`
/// skips reading the Nvidia GPU's identification register).
pub fn gpu_start() {
    gpu::intel::probe();
    let boot_fb = *BOOT_FRAMEBUFFER.lock();
    gpu::amd::probe(boot_fb);
    gpu::nvidia::probe(boot_fb);
    let linux = crate::gui::prefs::boot_option("gpu").as_deref() == Some("linux");
    crate::linux::init(linux);
    #[cfg(linuxkpi)]
    if linux {
        if gpu::linux::register() {
            return;
        }
        log!("display", "the Linux virtio-gpu driver found no display; trying the native driver");
    }
    #[cfg(not(linuxkpi))]
    if linux {
        log!("display", "this kernel was built without the Linux drivers; using the native virtio-gpu driver");
    }
    gpu::virtio::probe();
}

/// The resolution can change right away.
pub fn live() -> bool {
    gpu::primary().is_some_and(|d| d.caps().live_modeset)
}

/// The resolutions to offer, with the current one flagged.
pub fn modes(current: (u32, u32), at_boot: Option<(u32, u32)>) -> Vec<DisplayMode> {
    let mut out: Vec<DisplayMode> = Vec::new();
    match gpu::primary().filter(|d| d.caps().live_modeset) {
        Some(dev) => {
            let follows = if dev.caps().follows_host { display::FOLLOWS_WINDOW } else { 0 };
            let conns = dev.connectors();
            let conn = conns.iter().find(|c| c.status == gpu::Status::Connected).or(conns.first());
            for m in conn.map(|c| c.modes.as_slice()).unwrap_or(&[]) {
                if m.hdisplay < 800 || m.vdisplay < 600 {
                    continue;
                }
                let flags = display::LIVE | follows | if m.is_preferred() { display::PREFERRED } else { 0 };
                match out.iter_mut().find(|o| (o.width, o.height) == m.size()) {
                    Some(o) => o.flags |= flags,
                    None => out.push(DisplayMode { width: m.hdisplay, height: m.vdisplay, flags, _pad: 0 }),
                }
            }
        }
        None => {
            for m in FIRMWARE_MODES.lock().iter().filter(|m| m.width >= 800 && m.height >= 600) {
                if !out.iter().any(|o| (o.width, o.height) == (m.width, m.height)) {
                    out.push(DisplayMode { width: m.width, height: m.height, flags: 0, _pad: 0 });
                }
            }
        }
    }
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

/// After sleep: every display device restores its mode and picture.
pub fn resume(_w: u32, _h: u32) {
    gpu::resume_all();
}
