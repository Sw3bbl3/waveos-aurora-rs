//! Transcoder timing registers.
//!
//! Each register holds two values, both minus one: `HTOTAL` is
//! (total − 1) in 28:16 and (active − 1) in 12:0; `HBLANK` is (blank end − 1)
//! and (blank start − 1); `HSYNC` is (sync end − 1) and (sync start − 1);
//! the `V*` registers are the same in lines. `PIPESRC` is the pipe's source
//! size: (width − 1) in 28:16, (height − 1) in 12:0.

use crate::regs::{self, Transcoder};
use crate::Mmio;

const FIELD: u32 = 0x1FFF;

/// Packs a (low, high) pair of 1-based values into one register.
pub fn pack(low: u32, high: u32) -> u32 {
    (high.saturating_sub(1) & FIELD) << 16 | (low.saturating_sub(1) & FIELD)
}

/// The (low, high) pair of a register, 1-based.
pub fn unpack(v: u32) -> (u32, u32) {
    ((v & FIELD) + 1, (v >> 16 & FIELD) + 1)
}

/// A transcoder's timings, in pixels and lines. Sync polarity lives in
/// `TRANS_DDI_FUNC_CTL`, so it is carried here too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Timings {
    pub hactive: u32,
    pub hblank_start: u32,
    pub hblank_end: u32,
    pub hsync_start: u32,
    pub hsync_end: u32,
    pub htotal: u32,
    pub vactive: u32,
    pub vblank_start: u32,
    pub vblank_end: u32,
    pub vsync_start: u32,
    pub vsync_end: u32,
    pub vtotal: u32,
    pub hsync_positive: bool,
    pub vsync_positive: bool,
}

/// The six timing register values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct TimingRegs {
    pub htotal: u32,
    pub hblank: u32,
    pub hsync: u32,
    pub vtotal: u32,
    pub vblank: u32,
    pub vsync: u32,
}

impl Timings {
    /// Timings without borders: blanking is everything outside the active area.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        h: (u32, u32, u32, u32),
        v: (u32, u32, u32, u32),
        hsync_positive: bool,
        vsync_positive: bool,
    ) -> Timings {
        Timings {
            hactive: h.0,
            hblank_start: h.0,
            hsync_start: h.1,
            hsync_end: h.2,
            hblank_end: h.3,
            htotal: h.3,
            vactive: v.0,
            vblank_start: v.0,
            vsync_start: v.1,
            vsync_end: v.2,
            vblank_end: v.3,
            vtotal: v.3,
            hsync_positive,
            vsync_positive,
        }
    }

    pub fn to_regs(&self) -> TimingRegs {
        TimingRegs {
            htotal: pack(self.hactive, self.htotal),
            hblank: pack(self.hblank_start, self.hblank_end),
            hsync: pack(self.hsync_start, self.hsync_end),
            vtotal: pack(self.vactive, self.vtotal),
            vblank: pack(self.vblank_start, self.vblank_end),
            vsync: pack(self.vsync_start, self.vsync_end),
        }
    }

    pub fn from_regs(r: &TimingRegs, hsync_positive: bool, vsync_positive: bool) -> Timings {
        let (hactive, htotal) = unpack(r.htotal);
        let (hblank_start, hblank_end) = unpack(r.hblank);
        let (hsync_start, hsync_end) = unpack(r.hsync);
        let (vactive, vtotal) = unpack(r.vtotal);
        let (vblank_start, vblank_end) = unpack(r.vblank);
        let (vsync_start, vsync_end) = unpack(r.vsync);
        Timings {
            hactive,
            hblank_start,
            hblank_end,
            hsync_start,
            hsync_end,
            htotal,
            vactive,
            vblank_start,
            vblank_end,
            vsync_start,
            vsync_end,
            vtotal,
            hsync_positive,
            vsync_positive,
        }
    }

    /// The timings make sense: every value in order and inside the total.
    pub fn sane(&self) -> bool {
        let h = 0 < self.hactive
            && self.hactive <= self.hblank_start
            && self.hblank_start < self.hblank_end
            && self.hblank_end <= self.htotal
            && self.hactive <= self.hsync_start
            && self.hsync_start < self.hsync_end
            && self.hsync_end <= self.htotal;
        let v = 0 < self.vactive
            && self.vactive <= self.vblank_start
            && self.vblank_start < self.vblank_end
            && self.vblank_end <= self.vtotal
            && self.vactive <= self.vsync_start
            && self.vsync_start < self.vsync_end
            && self.vsync_end <= self.vtotal;
        h && v && self.hactive >= 320 && self.vactive >= 200
    }

    /// Refresh in millihertz for a pixel clock in kHz.
    pub fn refresh_mhz(&self, clock_khz: u32) -> u32 {
        let frame = self.htotal as u64 * self.vtotal as u64;
        if frame == 0 {
            return 0;
        }
        (clock_khz as u64 * 1_000_000 / frame) as u32
    }
}

pub fn read(m: &impl Mmio, t: Transcoder) -> TimingRegs {
    TimingRegs {
        htotal: m.read(regs::htotal(t)),
        hblank: m.read(regs::hblank(t)),
        hsync: m.read(regs::hsync(t)),
        vtotal: m.read(regs::vtotal(t)),
        vblank: m.read(regs::vblank(t)),
        vsync: m.read(regs::vsync(t)),
    }
}

pub fn write(m: &impl Mmio, t: Transcoder, r: &TimingRegs) {
    m.write(regs::htotal(t), r.htotal);
    m.write(regs::hblank(t), r.hblank);
    m.write(regs::hsync(t), r.hsync);
    m.write(regs::vtotal(t), r.vtotal);
    m.write(regs::vblank(t), r.vblank);
    m.write(regs::vsync(t), r.vsync);
}

/// `PIPESRC` for a `w`×`h` source.
pub fn pipesrc(w: u32, h: u32) -> u32 {
    pack(h, w)
}

/// The (width, height) of a `PIPESRC` value.
pub fn pipesrc_size(v: u32) -> (u32, u32) {
    let (h, w) = unpack(v);
    (w, h)
}
