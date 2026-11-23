//! Display engine registers, named as in Intel's Programmer's Reference
//! Manuals (Volume 2c "Command Reference: Registers" and Volume 12
//! "Display Engine" of the Skylake, Kaby Lake, Ice Lake, Tiger Lake and
//! Alder Lake PRMs). Offsets are from the start of BAR 0 (GTTMMADR).
//!
//! Field positions that differ between display versions are given per
//! version; anything not confirmed from the manuals is marked
//! `UNCONFIRMED`.

/// A transcoder: A–D are tied to the pipe of the same letter; `Edp` (display
/// 9–11 only) takes its input from any pipe (`TRANS_DDI_FUNC_CTL_EDP`'s
/// input select).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transcoder {
    Pipe(u8),
    Edp,
}

impl Transcoder {
    /// The base of the transcoder's timing block (`HTOTAL` and friends).
    pub fn base(self) -> u32 {
        match self {
            Transcoder::Pipe(p) => 0x60000 + p as u32 * 0x1000,
            Transcoder::Edp => 0x6F000,
        }
    }

    /// The base of the pipe-side block (`TRANSCONF`/`PIPECONF` at +8).
    fn conf_base(self) -> u32 {
        match self {
            Transcoder::Pipe(p) => 0x70000 + p as u32 * 0x1000,
            Transcoder::Edp => 0x7F000,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Transcoder::Pipe(0) => "A",
            Transcoder::Pipe(1) => "B",
            Transcoder::Pipe(2) => "C",
            Transcoder::Pipe(3) => "D",
            Transcoder::Pipe(_) => "?",
            Transcoder::Edp => "EDP",
        }
    }
}

// ------------------------------------------------- transcoder timings

pub fn htotal(t: Transcoder) -> u32 {
    t.base()
}
pub fn hblank(t: Transcoder) -> u32 {
    t.base() + 0x04
}
pub fn hsync(t: Transcoder) -> u32 {
    t.base() + 0x08
}
pub fn vtotal(t: Transcoder) -> u32 {
    t.base() + 0x0C
}
pub fn vblank(t: Transcoder) -> u32 {
    t.base() + 0x10
}
pub fn vsync(t: Transcoder) -> u32 {
    t.base() + 0x14
}
/// `PIPE_DATA_M1` (TU size in 30:25, data M in 23:0).
pub fn data_m1(t: Transcoder) -> u32 {
    t.base() + 0x30
}
pub fn data_n1(t: Transcoder) -> u32 {
    t.base() + 0x34
}
pub fn link_m1(t: Transcoder) -> u32 {
    t.base() + 0x40
}
pub fn link_n1(t: Transcoder) -> u32 {
    t.base() + 0x44
}
pub const TU_SIZE_SHIFT: u32 = 25;

/// `PIPESRC`: the pipe's source size, (width − 1) in 28:16, (height − 1) in 12:0.
pub fn pipesrc(pipe: u8) -> u32 {
    0x6001C + pipe as u32 * 0x1000
}

/// `TRANSCONF` (called `PIPECONF` before display 12).
pub fn transconf(t: Transcoder) -> u32 {
    t.conf_base() + 0x08
}
pub const TRANSCONF_ENABLE: u32 = 1 << 31;
/// Read-only: the transcoder is actually running.
pub const TRANSCONF_STATE: u32 = 1 << 30;
/// Interlace mode, 22:21 (0 = progressive).
pub const TRANSCONF_INTERLACE_MASK: u32 = 3 << 21;

/// `PIPEMISC`: dithering and the pipe's bits per colour (7:5).
pub fn pipemisc(pipe: u8) -> u32 {
    0x70030 + pipe as u32 * 0x1000
}
pub const PIPEMISC_BPC_MASK: u32 = 7 << 5;
pub const PIPEMISC_BPC_8: u32 = 0;

/// `PIPE_FRMCOUNT`: frames since the pipe was enabled (the vblank counter).
pub fn frame_count(pipe: u8) -> u32 {
    0x70040 + pipe as u32 * 0x1000
}

// ----------------------------------------------------- DDI functions

/// `TRANS_DDI_FUNC_CTL`: which DDI the transcoder drives, and how.
pub fn trans_ddi_func_ctl(t: Transcoder) -> u32 {
    t.base() + 0x400
}
pub const DDI_FUNC_ENABLE: u32 = 1 << 31;
/// Display 9–11: DDI select in 30:28 (0 = A).
pub const DDI_FUNC_PORT_SHIFT: u32 = 28;
pub const DDI_FUNC_PORT_MASK: u32 = 7 << 28;
/// Display 12+: DDI select in 30:27, the port's index plus one (0 = none).
pub const DDI_FUNC_PORT_SHIFT_TGL: u32 = 27;
pub const DDI_FUNC_PORT_MASK_TGL: u32 = 0xF << 27;
/// Mode select, 26:24.
pub const DDI_FUNC_MODE_SHIFT: u32 = 24;
pub const DDI_FUNC_MODE_MASK: u32 = 7 << 24;
pub const DDI_MODE_HDMI: u32 = 0;
pub const DDI_MODE_DVI: u32 = 1;
pub const DDI_MODE_DP_SST: u32 = 2;
pub const DDI_MODE_DP_MST: u32 = 3;
/// Bits per colour, 22:20 (0 = 8, 1 = 10, 2 = 6, 3 = 12).
pub const DDI_FUNC_BPC_SHIFT: u32 = 20;
pub const DDI_FUNC_BPC_MASK: u32 = 7 << 20;
pub const DDI_FUNC_VSYNC_HIGH: u32 = 1 << 17;
pub const DDI_FUNC_HSYNC_HIGH: u32 = 1 << 16;
/// eDP transcoder input select, 14:12: 0 = pipe A (always on), 4 = pipe A,
/// 5 = pipe B, 6 = pipe C, 7 = pipe D (display 11).
pub const DDI_FUNC_EDP_INPUT_SHIFT: u32 = 12;
pub const DDI_FUNC_EDP_INPUT_MASK: u32 = 7 << 12;
/// DP port width, 3:1 (lanes − 1, with 4 lanes as 3).
pub const DDI_FUNC_DP_WIDTH_SHIFT: u32 = 1;
pub const DDI_FUNC_DP_WIDTH_MASK: u32 = 7 << 1;

/// `DDI_BUF_CTL`: a DDI's output buffer.
pub fn ddi_buf_ctl(port: u8) -> u32 {
    0x64000 + port as u32 * 0x100
}
pub const DDI_BUF_ENABLE: u32 = 1 << 31;
/// Buffer translation entry select, 27:24 (display 9).
pub const DDI_BUF_TRANS_SELECT_SHIFT: u32 = 24;
pub const DDI_BUF_TRANS_SELECT_MASK: u32 = 0xF << 24;
pub const DDI_BUF_IDLE: u32 = 1 << 7;
/// DDI A only: four lanes (otherwise DDI E takes two of them).
pub const DDI_A_4_LANES: u32 = 1 << 4;
/// Port width, 3:1 (display 9: x1 = 0, x2 = 1, x4 = 3).
pub const DDI_BUF_WIDTH_SHIFT: u32 = 1;
pub const DDI_BUF_WIDTH_MASK: u32 = 7 << 1;

/// `DP_TP_CTL` (display 9–11: per port; display 12+ moved it per
/// transcoder, not used here).
pub fn dp_tp_ctl(port: u8) -> u32 {
    0x64040 + port as u32 * 0x100
}
pub fn dp_tp_status(port: u8) -> u32 {
    0x64044 + port as u32 * 0x100
}
pub const DP_TP_ENABLE: u32 = 1 << 31;
pub const DP_TP_ENHANCED_FRAMING: u32 = 1 << 18;
pub const DP_TP_PATTERN_MASK: u32 = 7 << 8;
pub const DP_TP_PATTERN_1: u32 = 0;
pub const DP_TP_PATTERN_2: u32 = 1 << 8;
pub const DP_TP_IDLE: u32 = 2 << 8;
pub const DP_TP_NORMAL: u32 = 3 << 8;
pub const DP_TP_PATTERN_3: u32 = 4 << 8;
/// `DP_TP_STATUS`: the idle pattern has been sent enough times. UNCONFIRMED bit.
pub const DP_TP_STATUS_IDLE_DONE: u32 = 1 << 25;

// ---------------------------------------------------- Skylake DPLLs

/// `LCPLL1_CTL` (DPLL 0, also the CD clock's), `LCPLL2_CTL` (DPLL 1),
/// `WRPLL_CTL1` (DPLL 2), `WRPLL_CTL2` (DPLL 3): bit 31 enables.
pub fn skl_dpll_enable(id: u8) -> u32 {
    [0x46010, 0x46014, 0x46040, 0x46060][id as usize & 3]
}
pub const DPLL_ENABLE: u32 = 1 << 31;
/// `DPLL_CTRL1`: per DPLL six bits at `id * 6`: override (bit 0), link
/// rate (3:1), SSC (4), HDMI mode (5).
pub const DPLL_CTRL1: u32 = 0x6C058;
/// `DPLL_CTRL2`: which DPLL clocks each DDI, and whether its clock is off.
pub const DPLL_CTRL2: u32 = 0x6C05C;
/// `DPLL_STATUS`: lock bit at `id * 8`.
pub const DPLL_STATUS: u32 = 0x6C060;
pub fn dpll_cfgcr1(id: u8) -> u32 {
    0x6C040 + (id as u32).saturating_sub(1) * 8
}
pub fn dpll_cfgcr2(id: u8) -> u32 {
    0x6C044 + (id as u32).saturating_sub(1) * 8
}
pub const CFGCR1_FREQ_ENABLE: u32 = 1 << 31;

pub fn dpll_ctrl1_override(id: u8) -> u32 {
    1 << (id as u32 * 6)
}
pub fn dpll_ctrl1_link_rate_shift(id: u8) -> u32 {
    id as u32 * 6 + 1
}
pub fn dpll_ctrl1_ssc(id: u8) -> u32 {
    1 << (id as u32 * 6 + 4)
}
pub fn dpll_ctrl1_hdmi(id: u8) -> u32 {
    1 << (id as u32 * 6 + 5)
}
pub fn dpll_ctrl1_mask(id: u8) -> u32 {
    0x3F << (id as u32 * 6)
}
pub fn dpll_lock(id: u8) -> u32 {
    1 << (id as u32 * 8)
}
/// `DPLL_CTRL2`: DDI `port`'s clock is gated.
pub fn ddi_clk_off(port: u8) -> u32 {
    1 << (port as u32 + 15)
}
pub fn ddi_clk_sel_shift(port: u8) -> u32 {
    port as u32 * 3 + 1
}
pub fn ddi_clk_sel_mask(port: u8) -> u32 {
    3 << ddi_clk_sel_shift(port)
}
pub fn ddi_sel_override(port: u8) -> u32 {
    1 << (port as u32 * 3)
}

/// `CDCLK_CTL`: the CD clock, 10:0 as MHz × 2 − 2 (U10.1 minus one).
pub const CDCLK_CTL: u32 = 0x46000;

// -------------------------------------------------- planes and cursor

/// Universal plane `plane` (0 = plane 1, the primary) of `pipe`.
pub fn plane_base(pipe: u8, plane: u8) -> u32 {
    0x70180 + pipe as u32 * 0x1000 + plane as u32 * 0x100
}
pub fn plane_ctl(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane)
}
pub fn plane_stride(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x08
}
pub fn plane_pos(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x0C
}
pub fn plane_size(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x10
}
/// `PLANE_SURF`: the surface's GGTT address (31:12). Writing it arms all
/// the plane's double-buffered registers for the next vblank.
pub fn plane_surf(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x1C
}
pub fn plane_offset(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x24
}
/// `PLANE_SURFLIVE`: the surface address the hardware is scanning now.
pub fn plane_surflive(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x2C
}
pub fn plane_aux_dist(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x40
}
/// `PLANE_COLOR_CTL` (display 10+: alpha and colour controls moved here).
pub fn plane_color_ctl(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0x4C
}
/// `PLANE_WM` level `level` (0–7).
pub fn plane_wm(pipe: u8, plane: u8, level: u8) -> u32 {
    plane_base(pipe, plane) + 0xC0 + level as u32 * 4
}
/// `PLANE_WM_SAGV` (display 12+).
pub fn plane_wm_sagv(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0xD8
}
pub fn plane_wm_trans(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0xE8
}
/// `PLANE_BUF_CFG`: the plane's display buffer (DDB) blocks.
pub fn plane_buf_cfg(pipe: u8, plane: u8) -> u32 {
    plane_base(pipe, plane) + 0xFC
}

pub const PLANE_CTL_ENABLE: u32 = 1 << 31;
/// Display 9: pipe gamma enable (display 10+ moved it to `PLANE_COLOR_CTL`).
pub const PLANE_CTL_PIPE_GAMMA: u32 = 1 << 30;
/// Display 9: source format, 27:24.
pub const PLANE_CTL_FORMAT_MASK_SKL: u32 = 0xF << 24;
/// Display 11+: source format, 27:23.
pub const PLANE_CTL_FORMAT_MASK_ICL: u32 = 0x1F << 23;
/// 8:8:8:8 RGB: 4 in the display 9 field, 8 in the display 11 one (the
/// same bits).
pub const PLANE_CTL_FORMAT_8888: u32 = 4 << 24;
/// 2:10:10:10 RGB (the same 32 bits per pixel).
pub const PLANE_CTL_FORMAT_2101010: u32 = 2 << 24;
/// Display 9: pipe CSC enable (bit 23, the low bit of the display 11 format field).
pub const PLANE_CTL_PIPE_CSC_SKL: u32 = 1 << 23;
/// RGBX order instead of BGRX (XBGR8888 instead of XRGB8888).
pub const PLANE_CTL_ORDER_RGBX: u32 = 1 << 20;
/// Tiling, 12:10 (0 = linear, 1 = X, 4 = Y, 5 = Yf).
pub const PLANE_CTL_TILED_MASK: u32 = 7 << 10;
pub const PLANE_CTL_FLIP_HORIZONTAL: u32 = 1 << 8;
/// Display 9: alpha mode, 5:4.
pub const PLANE_CTL_ALPHA_MASK_SKL: u32 = 3 << 4;
/// Rotation, 1:0 (0, 90, 180, 270).
pub const PLANE_CTL_ROTATE_MASK: u32 = 3;
/// Linear surfaces must start on a 256 KiB boundary of the GGTT (display
/// 9; later versions need less, so this is safe everywhere).
pub const SURF_ALIGN: u64 = 256 * 1024;
/// `PLANE_STRIDE` counts 64-byte units for linear surfaces.
pub const STRIDE_UNIT: u32 = 64;

/// `CURCNTR`, `CURBASE`, `CURPOS`: the cursor plane of `pipe`.
pub fn curcntr(pipe: u8) -> u32 {
    0x70080 + pipe as u32 * 0x1000
}
pub fn curbase(pipe: u8) -> u32 {
    0x70084 + pipe as u32 * 0x1000
}
pub fn curpos(pipe: u8) -> u32 {
    0x70088 + pipe as u32 * 0x1000
}
pub fn cur_surflive(pipe: u8) -> u32 {
    0x700AC + pipe as u32 * 0x1000
}
pub fn cur_wm(pipe: u8, level: u8) -> u32 {
    0x70140 + pipe as u32 * 0x1000 + level as u32 * 4
}
/// `CUR_WM_SAGV` (display 12+).
pub fn cur_wm_sagv(pipe: u8) -> u32 {
    0x70158 + pipe as u32 * 0x1000
}
pub fn cur_wm_trans(pipe: u8) -> u32 {
    0x70168 + pipe as u32 * 0x1000
}
pub fn cur_buf_cfg(pipe: u8) -> u32 {
    0x7017C + pipe as u32 * 0x1000
}
/// Cursor mode 6:0 for 64×64 ARGB (straight 0x27: "64x64 32bpp AX").
pub const CURSOR_MODE_64_ARGB: u32 = 0x27;
pub const CURSOR_ALIGN: u64 = 4096;

// --------------------------------------------------------- pipe scaler

/// `PS_CTRL` of scaler `id` of `pipe` (bit 31 enables). Only read: the
/// firmware may scale its mode to the panel.
pub fn ps_ctrl(pipe: u8, id: u8) -> u32 {
    0x68180 + pipe as u32 * 0x800 + id as u32 * 0x100
}
pub fn ps_win_sz(pipe: u8, id: u8) -> u32 {
    0x68174 + pipe as u32 * 0x800 + id as u32 * 0x100
}
pub const PS_ENABLE: u32 = 1 << 31;

// ---------------------------------------------------------- interrupts

/// Display engine pipe interrupts (`GEN8_DE_PIPE_ISR/IMR/IIR/IER`).
pub fn de_pipe_isr(pipe: u8) -> u32 {
    0x44400 + pipe as u32 * 0x10
}
pub fn de_pipe_imr(pipe: u8) -> u32 {
    0x44404 + pipe as u32 * 0x10
}
pub fn de_pipe_iir(pipe: u8) -> u32 {
    0x44408 + pipe as u32 * 0x10
}
pub fn de_pipe_ier(pipe: u8) -> u32 {
    0x4440C + pipe as u32 * 0x10
}
pub const PIPE_VBLANK: u32 = 1 << 0;
/// Plane 1 flip done (plane n is bit n + 2).
pub const PIPE_PLANE1_FLIP_DONE: u32 = 1 << 3;
pub const PIPE_FIFO_UNDERRUN: u32 = 1 << 31;

/// Display engine port interrupts (DDI A hotplug on display 9, all DDIs on Broxton).
pub const DE_PORT_ISR: u32 = 0x44440;
pub const DE_PORT_IIR: u32 = 0x44448;
/// Display 11+: Type-C and Thunderbolt hotplug (`GEN11_DE_HPD_*`).
pub const DE_HPD_ISR: u32 = 0x44470;
pub const DE_HPD_IIR: u32 = 0x44478;
/// South display engine interrupts (PCH hotplug, GMBUS).
pub const SDE_ISR: u32 = 0xC4000;
pub const SDE_IMR: u32 = 0xC4004;
pub const SDE_IIR: u32 = 0xC4008;

// ------------------------------------------------- panel power, backlight

/// `PP_STATUS`, `PP_CONTROL`, `PP_ON_DELAYS`, `PP_OFF_DELAYS`, `PP_DIVISOR`
/// of the panel power sequencer (instance 0).
pub const PP_STATUS: u32 = 0xC7200;
pub const PP_CONTROL: u32 = 0xC7204;
pub const PP_ON_DELAYS: u32 = 0xC7208;
pub const PP_OFF_DELAYS: u32 = 0xC720C;
/// Sunrise Point only; later PCHs keep the power cycle delay in `PP_CONTROL` 8:4.
pub const PP_DIVISOR: u32 = 0xC7210;
pub const PP_ON: u32 = 1 << 31;
/// `PP_STATUS` 29:28: the sequencer's state (0 = idle).
pub const PP_SEQUENCE_MASK: u32 = 3 << 28;
pub const PP_CYCLE_DELAY_ACTIVE: u32 = 1 << 27;
pub const PP_POWER_TARGET_ON: u32 = 1 << 0;
pub const PP_RESET: u32 = 1 << 1;
pub const PP_BLC_ENABLE: u32 = 1 << 2;
pub const PP_FORCE_VDD: u32 = 1 << 3;
/// `PP_CONTROL` writes on Sunrise Point need the unlock key in 31:16.
pub const PP_UNLOCK: u32 = 0xABCD << 16;

/// `BLC_PWM_PCH_CTL1` / `BXT_BLC_PWM_CTL1`: bit 31 enables the PWM, bit 29 inverts it.
pub const BLC_PWM_CTL1: u32 = 0xC8250;
pub const BLC_PWM_ENABLE: u32 = 1 << 31;
pub const BLC_PWM_POLARITY: u32 = 1 << 29;
/// `BLC_PWM_PCH_CTL2` (Sunrise Point: period 31:16, duty 15:0) or
/// `BXT_BLC_PWM_FREQ1` (32-bit period).
pub const BLC_PWM_CTL2: u32 = 0xC8254;
/// `BXT_BLC_PWM_DUTY1` (32-bit duty).
pub const BLC_PWM_DUTY: u32 = 0xC8258;

// ------------------------------------------------------- power wells

/// `HSW_PWR_WELL_CTL1` (the firmware's requests) and `CTL2` (the driver's).
pub const PWR_WELL_CTL_BIOS: u32 = 0x45400;
pub const PWR_WELL_CTL_DRIVER: u32 = 0x45404;
/// `DBUF_CTL` (display 9–10): bit 31 requests the display buffer, bit 30 says it is powered.
pub const DBUF_CTL: u32 = 0x45008;
pub const DBUF_POWER_STATE: u32 = 1 << 30;

// -------------------------------------------------------------- misc

/// The GGTT starts half way into BAR 0 (16 MiB on display 9+).
pub const GGTT_OFFSET: u64 = 8 << 20;
/// Bytes of BAR 0 the driver maps (registers and the GGTT).
pub const BAR0_SIZE: u64 = 16 << 20;
/// `GFX_FLSH_CNTL`: writing 1 invalidates the GT's GGTT TLBs after PTE writes.
pub const GFX_FLSH_CNTL: u32 = 0x101008;

/// PCI configuration: `GGC` (GMCH graphics control: GGTT size 7:6,
/// stolen memory size 15:8) and `BDSM` (stolen memory base, 31:20).
pub const PCI_GGC: u16 = 0x50;
pub const PCI_BDSM: u16 = 0x5C;
/// Display 11+: `BDSM` is 64 bits at 0xC0. UNCONFIRMED for every platform.
pub const PCI_BDSM_GEN11: u16 = 0xC0;
/// PCI configuration: `ASLS`, the physical address of the ACPI OpRegion
/// (which holds the Video BIOS Table). Logged; not parsed yet.
pub const PCI_ASLS: u16 = 0xFC;
