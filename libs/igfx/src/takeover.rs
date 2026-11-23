//! Reading back what the firmware lit, and deciding whether to take it over.
//!
//! The GOP driver leaves one or more pipes running: a transcoder with
//! timings, feeding a DDI port in HDMI, DVI or DisplayPort mode, and the
//! pipe's plane 1 scanning a linear framebuffer in stolen memory. The
//! driver keeps all of that (no mode change) and only ever points plane 1
//! at its own surfaces, of the same size, format width and tiling, and
//! adds the cursor plane.
//!
//! Anything unexpected declines the takeover ([`Decline`]); the firmware
//! framebuffer then stays in charge, exactly as without this driver.

use crate::platform::Info;
use crate::regs::{self, Transcoder};
use crate::timing::{self, Timings};
use crate::{dpll, plane, Mmio};
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Output {
    Hdmi,
    Dvi,
    Dp,
    Edp,
}

/// Plane 1 as the firmware left it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plane {
    pub ctl: u32,
    pub stride_bytes: u32,
    pub surf: u32,
    pub size: (u32, u32),
    pub buf_cfg: u32,
    pub wm: [u32; 8],
    /// `PLANE_WM_SAGV` (display 12+).
    pub wm_sagv: Option<u32>,
}

/// One running pipe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pipe {
    pub pipe: u8,
    pub trans: Transcoder,
    pub port: u8,
    pub output: Output,
    /// DP lanes (4 for HDMI/DVI's TMDS channels).
    pub lanes: u8,
    pub bpc: u8,
    pub timings: Timings,
    /// `PIPESRC`: what the planes see (differs from the timings when the
    /// firmware scales to the panel).
    pub src: (u32, u32),
    pub scaled: bool,
    pub primary: Plane,
    /// The firmware's own cursor is on (unusual).
    pub cursor_on: bool,
    pub ddi_func: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware {
    pub pipes: Vec<Pipe>,
    /// The CD clock, kHz (display 9–10 encoding; `None` elsewhere).
    pub cdclk_khz: Option<u32>,
    /// The internal panel's power sequencer says the panel is on.
    pub panel_on: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decline {
    /// No transcoder is running: nothing to take over (headless, or a
    /// DSI panel this driver doesn't know).
    NoActivePipe,
    /// Two transcoders feed one pipe.
    PipeConflict(u8),
    Interlaced(u8),
    /// The transcoder isn't driving a DDI in HDMI, DVI or single-stream DP mode.
    OutputMode(u8, u32),
    /// The timing registers don't make sense.
    Timings(u8),
    /// Wider than one pipe can drive (two pipes joined).
    Joined(u8),
    PlaneOff(u8),
    /// The firmware's surface is tiled: its watermarks don't fit a linear one.
    Tiled(u8, u32),
    /// Not a 32-bit RGB format.
    Format(u8, u32),
    /// Rotated or mirrored.
    Rotated(u8, u32),
    /// Plane 1 doesn't cover the pipe exactly.
    Coverage(u8),
    /// The stride is shorter than a row.
    Stride(u8),
}

impl core::fmt::Display for Decline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Decline::NoActivePipe => write!(f, "no pipe is running"),
            Decline::PipeConflict(p) => write!(f, "two transcoders feed pipe {}", pipe_name(p)),
            Decline::Interlaced(p) => write!(f, "pipe {} is interlaced", pipe_name(p)),
            Decline::OutputMode(p, v) => {
                write!(f, "pipe {}: TRANS_DDI_FUNC_CTL {:#010x} is not HDMI, DVI or DP SST", pipe_name(p), v)
            }
            Decline::Timings(p) => write!(f, "pipe {}: the timing registers don't make sense", pipe_name(p)),
            Decline::Joined(p) => write!(f, "pipe {} is wider than one pipe drives", pipe_name(p)),
            Decline::PlaneOff(p) => write!(f, "pipe {}: plane 1 is off", pipe_name(p)),
            Decline::Tiled(p, c) => write!(f, "pipe {}: plane 1 is tiled (PLANE_CTL {:#010x})", pipe_name(p), c),
            Decline::Format(p, c) => {
                write!(f, "pipe {}: plane 1 is not 32-bit RGB (PLANE_CTL {:#010x})", pipe_name(p), c)
            }
            Decline::Rotated(p, c) => {
                write!(f, "pipe {}: plane 1 is rotated or mirrored (PLANE_CTL {:#010x})", pipe_name(p), c)
            }
            Decline::Coverage(p) => write!(f, "pipe {}: plane 1 does not cover the pipe", pipe_name(p)),
            Decline::Stride(p) => write!(f, "pipe {}: plane 1's stride is shorter than a row", pipe_name(p)),
        }
    }
}

pub fn pipe_name(p: u8) -> char {
    (b'A' + p) as char
}

/// The transcoders this platform has, the eDP one first.
pub fn transcoders(info: &Info) -> Vec<Transcoder> {
    let mut t = Vec::new();
    if info.edp_transcoder() {
        t.push(Transcoder::Edp);
    }
    t.extend((0..info.pipes).map(Transcoder::Pipe));
    t
}

/// The DDI a `TRANS_DDI_FUNC_CTL` value selects.
pub fn ddi_port(func: u32, display: u8) -> Option<u8> {
    if display >= 12 {
        let v = (func & regs::DDI_FUNC_PORT_MASK_TGL) >> regs::DDI_FUNC_PORT_SHIFT_TGL;
        (v != 0).then(|| v as u8 - 1)
    } else {
        Some(((func & regs::DDI_FUNC_PORT_MASK) >> regs::DDI_FUNC_PORT_SHIFT) as u8)
    }
}

/// The pipe the eDP transcoder takes its input from.
pub fn edp_input_pipe(func: u32) -> u8 {
    match (func & regs::DDI_FUNC_EDP_INPUT_MASK) >> regs::DDI_FUNC_EDP_INPUT_SHIFT {
        5 => 1,
        6 => 2,
        7 => 3,
        _ => 0,
    }
}

pub fn bpc(func: u32) -> u8 {
    match (func & regs::DDI_FUNC_BPC_MASK) >> regs::DDI_FUNC_BPC_SHIFT {
        1 => 10,
        2 => 6,
        3 => 12,
        _ => 8,
    }
}

pub fn dp_lanes(func: u32) -> u8 {
    match (func & regs::DDI_FUNC_DP_WIDTH_MASK) >> regs::DDI_FUNC_DP_WIDTH_SHIFT {
        0 => 1,
        1 => 2,
        _ => 4,
    }
}

fn read_plane(m: &impl Mmio, info: &Info, pipe: u8) -> Plane {
    let mut wm = [0u32; 8];
    for (l, w) in wm.iter_mut().enumerate() {
        *w = m.read(regs::plane_wm(pipe, 0, l as u8));
    }
    let size = m.read(regs::plane_size(pipe, 0));
    Plane {
        ctl: m.read(regs::plane_ctl(pipe, 0)),
        stride_bytes: (m.read(regs::plane_stride(pipe, 0)) & 0x7FF) * regs::STRIDE_UNIT,
        surf: m.read(regs::plane_surf(pipe, 0)) & !0xFFF,
        size: ((size & 0x1FFF) + 1, (size >> 16 & 0x1FFF) + 1),
        buf_cfg: m.read(regs::plane_buf_cfg(pipe, 0)),
        wm,
        wm_sagv: (info.display >= 12).then(|| m.read(regs::plane_wm_sagv(pipe, 0))),
    }
}

/// Reads every running pipe and checks it can be taken over.
pub fn read(m: &impl Mmio, info: &Info) -> Result<Firmware, Decline> {
    let mut pipes: Vec<Pipe> = Vec::new();
    for t in transcoders(info) {
        let conf = m.read(regs::transconf(t));
        if conf & regs::TRANSCONF_ENABLE == 0 {
            continue;
        }
        let func = m.read(regs::trans_ddi_func_ctl(t));
        let pipe = match t {
            Transcoder::Edp => edp_input_pipe(func),
            Transcoder::Pipe(p) => p,
        };
        if pipe >= info.pipes || pipes.iter().any(|p| p.pipe == pipe) {
            return Err(Decline::PipeConflict(pipe));
        }
        if conf & regs::TRANSCONF_INTERLACE_MASK != 0 {
            return Err(Decline::Interlaced(pipe));
        }
        let mode = (func & regs::DDI_FUNC_MODE_MASK) >> regs::DDI_FUNC_MODE_SHIFT;
        let port = ddi_port(func, info.display);
        let (Some(port), true) = (port, func & regs::DDI_FUNC_ENABLE != 0) else {
            return Err(Decline::OutputMode(pipe, func));
        };
        let panel_on = m.read(regs::PP_STATUS) & regs::PP_ON != 0;
        let output = match mode {
            regs::DDI_MODE_HDMI => Output::Hdmi,
            regs::DDI_MODE_DVI => Output::Dvi,
            regs::DDI_MODE_DP_SST if t == Transcoder::Edp || (port == 0 && panel_on) => Output::Edp,
            regs::DDI_MODE_DP_SST => Output::Dp,
            _ => return Err(Decline::OutputMode(pipe, func)),
        };
        let timings = Timings::from_regs(
            &timing::read(m, t),
            func & regs::DDI_FUNC_HSYNC_HIGH != 0,
            func & regs::DDI_FUNC_VSYNC_HIGH != 0,
        );
        if !timings.sane() {
            return Err(Decline::Timings(pipe));
        }
        let src = timing::pipesrc_size(m.read(regs::pipesrc(pipe)));
        if src.0 > 5120 || timings.hactive > 5120 {
            return Err(Decline::Joined(pipe));
        }
        if src.0 < 320 || src.1 < 200 || src.0 > timings.hactive * 4 || src.1 > timings.vactive * 4 {
            return Err(Decline::Timings(pipe));
        }
        let primary = read_plane(m, info, pipe);
        let ctl = primary.ctl;
        if ctl & regs::PLANE_CTL_ENABLE == 0 {
            return Err(Decline::PlaneOff(pipe));
        }
        if ctl & regs::PLANE_CTL_TILED_MASK != 0 {
            return Err(Decline::Tiled(pipe, ctl));
        }
        if !plane::rgb32(ctl, info.display) {
            return Err(Decline::Format(pipe, ctl));
        }
        if ctl & (regs::PLANE_CTL_ROTATE_MASK | regs::PLANE_CTL_FLIP_HORIZONTAL) != 0 {
            return Err(Decline::Rotated(pipe, ctl));
        }
        if m.read(regs::plane_pos(pipe, 0)) != 0 || primary.size != src {
            return Err(Decline::Coverage(pipe));
        }
        if primary.stride_bytes < src.0 * 4 {
            return Err(Decline::Stride(pipe));
        }
        let lanes = match output {
            Output::Dp | Output::Edp => dp_lanes(func),
            _ => 4,
        };
        pipes.push(Pipe {
            pipe,
            trans: t,
            port,
            output,
            lanes,
            bpc: bpc(func),
            timings,
            src,
            scaled: src != (timings.hactive, timings.vactive),
            primary,
            cursor_on: m.read(regs::curcntr(pipe)) & 0x3F != 0,
            ddi_func: func,
        });
    }
    if pipes.is_empty() {
        return Err(Decline::NoActivePipe);
    }
    pipes.sort_by_key(|p| (p.output != Output::Edp, p.pipe));
    let cdclk_khz = (info.display <= 10).then(|| (m.read(regs::CDCLK_CTL) & 0x7FF) * 500 + 1000);
    Ok(Firmware { pipes, cdclk_khz, panel_on: m.read(regs::PP_STATUS) & regs::PP_ON != 0 })
}

/// The pixel clock the firmware programmed, in kHz, where the PLL can be
/// read back: Skylake-class DPLLs only (HDMI mode from the DCO and
/// dividers; DP from the link rate code and the link M/N). `None`
/// elsewhere: the driver then measures the refresh.
pub fn pixel_clock(m: &impl Mmio, info: &Info, p: &Pipe) -> Option<u32> {
    if !info.skylake_class() {
        return None;
    }
    let ctrl2 = m.read(regs::DPLL_CTRL2);
    if ctrl2 & regs::ddi_sel_override(p.port) == 0 {
        return None;
    }
    let id = ((ctrl2 & regs::ddi_clk_sel_mask(p.port)) >> regs::ddi_clk_sel_shift(p.port)) as u8;
    let ctrl1 = m.read(regs::DPLL_CTRL1);
    if ctrl1 & regs::dpll_ctrl1_hdmi(id) != 0 {
        if id == 0 {
            return None;
        }
        return dpll::hdmi_clock(m.read(regs::dpll_cfgcr1(id)), m.read(regs::dpll_cfgcr2(id)));
    }
    let code = (ctrl1 >> regs::dpll_ctrl1_link_rate_shift(id)) & 7;
    let rate = dpll::dp_rate_from_code(code)?;
    let lm = (m.read(regs::link_m1(p.trans)) & 0xFF_FFFF) as u64;
    let ln = (m.read(regs::link_n1(p.trans)) & 0xFF_FFFF) as u64;
    (ln != 0).then(|| (rate as u64 * lm / ln) as u32)
}
