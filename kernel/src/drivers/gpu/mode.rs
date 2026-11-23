//! Display modes, laid out like DRM's `drm_mode_modeinfo` so a future
//! DRM ioctl layer can copy them out as they are.

use aurora_edid::Timing;

pub mod flag {
    pub const PHSYNC: u32 = 1 << 0;
    pub const NHSYNC: u32 = 1 << 1;
    pub const PVSYNC: u32 = 1 << 2;
    pub const NVSYNC: u32 = 1 << 3;
    pub const INTERLACE: u32 = 1 << 4;
}

/// DRM mode types.
pub mod kind {
    pub const PREFERRED: u32 = 1 << 3;
    pub const DRIVER: u32 = 1 << 6;
    pub const USERDEF: u32 = 1 << 5;
}

/// One video mode: CRTC timings in pixels and lines, clock in kHz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mode {
    pub clock: u32,
    pub hdisplay: u32,
    pub hsync_start: u32,
    pub hsync_end: u32,
    pub htotal: u32,
    pub vdisplay: u32,
    pub vsync_start: u32,
    pub vsync_end: u32,
    pub vtotal: u32,
    /// Hz, rounded.
    pub vrefresh: u32,
    pub flags: u32,
    pub kind: u32,
}

/// Sizes offered on displays that can show any size (virtual machines).
pub const COMMON: [(u32, u32); 16] = [
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
    (2560, 1600),
    (2880, 1800),
    (3200, 1800),
    (3840, 2160),
    (3840, 2400),
];

impl Mode {
    pub fn from_timing(t: &Timing) -> Mode {
        let mut flags = if t.hsync_positive { flag::PHSYNC } else { flag::NHSYNC };
        flags |= if t.vsync_positive { flag::PVSYNC } else { flag::NVSYNC };
        if t.interlaced {
            flags |= flag::INTERLACE;
        }
        Mode {
            clock: t.clock_khz,
            hdisplay: t.hactive,
            hsync_start: t.hsync_start,
            hsync_end: t.hsync_end,
            htotal: t.htotal,
            vdisplay: t.vactive,
            vsync_start: t.vsync_start,
            vsync_end: t.vsync_end,
            vtotal: t.vtotal,
            vrefresh: t.refresh_hz(),
            flags,
            kind: kind::DRIVER | if t.preferred { kind::PREFERRED } else { 0 },
        }
    }

    /// CVT reduced-blanking timings for `w`×`h` at `hz`.
    pub fn cvt(w: u32, h: u32, hz: u32) -> Option<Mode> {
        let mut t = aurora_edid::cvt_rb(w, h, hz)?;
        // CVT rounds the width down to 8 pixels; a virtual display shows the exact size.
        let extra = w - t.hactive;
        t.hactive = w;
        t.hsync_start += extra;
        t.hsync_end += extra;
        t.htotal += extra;
        Some(Mode::from_timing(&t))
    }

    /// A mode with only a size (framebuffers whose timing the firmware owns).
    pub fn fixed(w: u32, h: u32) -> Mode {
        Mode::cvt(w, h, 60).unwrap_or(Mode {
            clock: 0,
            hdisplay: w,
            hsync_start: w,
            hsync_end: w,
            htotal: w,
            vdisplay: h,
            vsync_start: h,
            vsync_end: h,
            vtotal: h,
            vrefresh: 60,
            flags: 0,
            kind: kind::DRIVER,
        })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.hdisplay, self.vdisplay)
    }

    pub fn is_preferred(&self) -> bool {
        self.kind & kind::PREFERRED != 0
    }

    pub fn preferred(mut self, yes: bool) -> Mode {
        if yes {
            self.kind |= kind::PREFERRED;
        } else {
            self.kind &= !kind::PREFERRED;
        }
        self
    }

    /// DRM's mode name, "1920x1080" (with an "i" when interlaced).
    pub fn name(&self) -> alloc::string::String {
        alloc::format!(
            "{}x{}{}",
            self.hdisplay,
            self.vdisplay,
            if self.flags & flag::INTERLACE != 0 { "i" } else { "" }
        )
    }
}

/// Sorts `modes` preferred first, then largest and fastest first, and
/// drops repeats (same size, refresh and scan).
pub fn tidy(modes: &mut alloc::vec::Vec<Mode>) {
    modes.sort_by(|a, b| {
        b.is_preferred()
            .cmp(&a.is_preferred())
            .then((b.hdisplay as u64 * b.vdisplay as u64).cmp(&(a.hdisplay as u64 * a.vdisplay as u64)))
            .then(b.hdisplay.cmp(&a.hdisplay))
            .then(b.vrefresh.cmp(&a.vrefresh))
    });
    let mut seen: alloc::vec::Vec<(u32, u32, u32, bool)> = alloc::vec::Vec::new();
    modes.retain(|m| {
        let key = (m.hdisplay, m.vdisplay, m.vrefresh, m.flags & flag::INTERLACE != 0);
        if seen.contains(&key) {
            false
        } else {
            seen.push(key);
            true
        }
    });
}
