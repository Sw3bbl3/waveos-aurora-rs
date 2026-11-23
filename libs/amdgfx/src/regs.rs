//! Register offsets and fields.
//!
//! Offsets are per instance (`[u32; 6]`, byte offsets into the register
//! BAR, 0 where an ASIC has fewer instances) and come from AMD's published
//! register headers, generated into `layout.rs` by `tools/amd-regs.py`.
//! Field positions below were checked against the `*_sh_mask.h` headers of
//! every listed version (DCN 1.0, 2.0, 2.1, 3.0, 3.0.1–3.0.3, 3.1.2–3.1.6,
//! 3.2, 3.2.1; DCE 11.2, 12.0) where those headers define them; the OTG
//! and HPD fields are missing from the 3.0.1, 3.1.5, 3.1.6 and 3.2.0
//! headers and are assumed to be as in their neighbours (UNCONFIRMED).
//!
//! Field *values* (pixel format codes, cursor modes, pitch and lines per
//! chunk encodings, the AUX arbitration states) are not in the headers;
//! the ones used are marked UNCONFIRMED.

/// Instances of a register.
pub type Inst = [u32; 6];

/// A DCN block: OTG (timing generator), HUBP (plane fetch, primary surface
/// and cursor), the DPP's cursor control, HPD and DP AUX.
pub struct Dcn {
    pub otg_control: Inst,
    pub otg_h_total: Inst,
    pub otg_h_blank: Inst,
    pub otg_h_sync: Inst,
    pub otg_h_sync_cntl: Inst,
    pub otg_v_total: Inst,
    pub otg_v_blank: Inst,
    pub otg_v_sync: Inst,
    pub otg_v_sync_cntl: Inst,
    pub otg_interlace: Inst,
    pub otg_frame_count: Inst,
    pub hubp_cntl: Inst,
    pub surface_config: Inst,
    pub tiling_config: Inst,
    pub viewport_start: Inst,
    pub viewport_dim: Inst,
    pub pitch: Inst,
    pub addr: Inst,
    pub addr_high: Inst,
    pub surface_control: Inst,
    pub flip_control: Inst,
    pub cur_settings: Inst,
    pub cur_control: Inst,
    pub cur_addr: Inst,
    pub cur_addr_high: Inst,
    pub cur_size: Inst,
    pub cur_position: Inst,
    pub cur_hot_spot: Inst,
    pub cur_dst_offset: Inst,
    pub dpp_cur_control: Inst,
    pub hpd_status: Inst,
    pub aux_control: Inst,
    pub aux_sw_control: Inst,
    pub aux_arb_control: Inst,
    pub aux_interrupt_control: Inst,
    pub aux_sw_status: Inst,
    pub aux_sw_data: Inst,
}

/// A DCE block: CRTC, the display controller's graphics surface (GRPH)
/// and cursor (CUR), HPD and DP AUX. CRTC *i* is fed by DCP *i*.
pub struct Dce {
    pub crtc_control: Inst,
    pub crtc_h_total: Inst,
    pub crtc_h_blank: Inst,
    pub crtc_h_sync: Inst,
    pub crtc_h_sync_cntl: Inst,
    pub crtc_v_total: Inst,
    pub crtc_v_blank: Inst,
    pub crtc_v_sync: Inst,
    pub crtc_v_sync_cntl: Inst,
    pub crtc_interlace: Inst,
    pub crtc_frame_count: Inst,
    pub grph_enable: Inst,
    pub grph_control: Inst,
    pub grph_swap: Inst,
    pub grph_addr: Inst,
    pub grph_addr_high: Inst,
    pub grph_pitch: Inst,
    pub grph_offset_x: Inst,
    pub grph_offset_y: Inst,
    pub grph_x_start: Inst,
    pub grph_y_start: Inst,
    pub grph_x_end: Inst,
    pub grph_y_end: Inst,
    pub grph_update: Inst,
    pub grph_flip_control: Inst,
    pub cur_control: Inst,
    pub cur_addr: Inst,
    pub cur_addr_high: Inst,
    pub cur_size: Inst,
    pub cur_position: Inst,
    pub cur_hot_spot: Inst,
    pub cur_update: Inst,
    pub hpd_status: Inst,
    pub aux_control: Inst,
    pub aux_sw_control: Inst,
    pub aux_arb_control: Inst,
    pub aux_interrupt_control: Inst,
    pub aux_sw_status: Inst,
    pub aux_sw_data: Inst,
}

/// The highest register offset a layout uses (the register BAR must be larger).
pub fn max_offset(l: crate::platform::Layout) -> u32 {
    use crate::platform::Layout;
    let all: &[&Inst] = match l {
        Layout::Dcn(d) => &[&d.otg_frame_count, &d.dpp_cur_control, &d.aux_sw_data, &d.hpd_status, &d.cur_dst_offset],
        Layout::Dce(d) => &[&d.crtc_frame_count, &d.cur_update, &d.aux_sw_data, &d.hpd_status],
    };
    all.iter().flat_map(|i| i.iter()).copied().max().unwrap_or(0)
}

// ------------------------------------------------- timing (OTG and CRTC)

/// `*_MASTER_EN` (OTG_CONTROL, CRTC_CONTROL).
pub const MASTER_EN: u32 = 1 << 0;
/// `*_INTERLACE_ENABLE`.
pub const INTERLACE_ENABLE: u32 = 1 << 0;
/// `*_H_SYNC_A_POL` / `*_V_SYNC_A_POL`: set for an active-low sync
/// (UNCONFIRMED which value means which; it only changes the reported mode).
pub const SYNC_POL_NEGATIVE: u32 = 1 << 0;
/// `*_FRAME_COUNT`, 23:0.
pub const FRAME_COUNT_MASK: u32 = 0x00FF_FFFF;
/// OTG timing fields are 15 bits (14:0 and 30:16); CRTC (DCE) fields 14.
pub const OTG_FIELD: u32 = 0x7FFF;
pub const CRTC_FIELD: u32 = 0x3FFF;

// ------------------------------------------------------------ DCN HUBP

/// `DCHUBP_CNTL`.
pub const HUBP_BLANK_EN: u32 = 1 << 0;
pub const HUBP_VTG_SEL_SHIFT: u32 = 4;
pub const HUBP_VTG_SEL_MASK: u32 = 0xF << 4;

/// `DCSURF_SURFACE_CONFIG`.
pub const SURFACE_PIXEL_FORMAT_MASK: u32 = 0x7F;
pub const ROTATION_ANGLE_MASK: u32 = 3 << 8;
pub const H_MIRROR_EN: u32 = 1 << 10;
/// The pixel format code of 32-bit ARGB8888 (blue in the lowest byte, as
/// the GOP's BGRX). UNCONFIRMED: the format codes are not in the headers.
pub const PIXEL_FORMAT_ARGB8888: u32 = 8;

/// `DCSURF_TILING_CONFIG.SW_MODE` (4:0); 0 is linear.
pub const SW_MODE_MASK: u32 = 0x1F;

/// `DCSURF_PRI_VIEWPORT_START` / `_DIMENSION`: x/width 13:0, y/height 29:16.
pub const VIEWPORT_FIELD: u32 = 0x3FFF;
/// `DCSURF_SURFACE_PITCH.PITCH` (13:0), in pixels.
pub const PITCH_MASK: u32 = 0x3FFF;

/// `DCSURF_SURFACE_CONTROL`.
pub const PRIMARY_SURFACE_TMZ: u32 = 1 << 0;
pub const PRIMARY_SURFACE_DCC_EN: u32 = 1 << 1;

/// `DCSURF_FLIP_CONTROL`.
pub const SURFACE_UPDATE_LOCK: u32 = 1 << 0;
/// 1: flip immediately (tearing), 0: at the next vertical update.
pub const SURFACE_FLIP_TYPE: u32 = 1 << 1;

/// `DCSURF_FLIP_CONTROL.SURFACE_FLIP_PENDING`: bit 31 on DCN 1.0, bit 8
/// from DCN 2.0 on (both from the headers).
pub fn flip_pending_bit(v: crate::platform::DcnVer) -> u32 {
    if v == crate::platform::DcnVer::V10 {
        1 << 31
    } else {
        1 << 8
    }
}

/// `DCSURF_PRIMARY_SURFACE_ADDRESS_HIGH` (15:0): a 48-bit address.
pub const ADDR_HIGH_MASK_DCN: u32 = 0xFFFF;

// ---------------------------------------------------------- DCN cursor

/// `CURSOR_CONTROL`.
pub const CURSOR_ENABLE: u32 = 1 << 0;
pub const CURSOR_MODE_SHIFT: u32 = 8;
pub const CURSOR_PITCH_SHIFT: u32 = 16;
pub const CURSOR_LINES_PER_CHUNK_SHIFT: u32 = 24;
/// Cursor modes (DCN `CURSOR_MODE`, DPP `CUR0_MODE`, DCE `CURSOR_MODE`):
/// 2 is 32-bit colour with premultiplied alpha. UNCONFIRMED.
pub const CURSOR_MODE_ARGB_PREMULTIPLIED: u32 = 2;
/// `CURSOR_PITCH` for 64 pixels. UNCONFIRMED.
pub const CURSOR_PITCH_64: u32 = 0;
/// `CURSOR_LINES_PER_CHUNK` for a 64-pixel ARGB cursor: 8 lines, as
/// log2. UNCONFIRMED.
pub const CURSOR_LINES_PER_CHUNK_64: u32 = 3;
/// `CURSOR_SETTINGS`: `CURSOR0_DST_Y_OFFSET` (7:0) 0,
/// `CURSOR0_CHUNK_HDL_ADJUST` (9:8) 3. UNCONFIRMED.
pub const CURSOR_SETTINGS_VALUE: u32 = 3 << 8;
/// `CURSOR_POSITION`: y 13:0, x 29:16; `CURSOR_HOT_SPOT`: y 7:0, x 23:16.
pub const CURSOR_POS_FIELD: u32 = 0x3FFF;
pub const DCN_HOT_SPOT_MAX: u32 = 0xFF;
/// `CURSOR_SIZE`: height 8:0, width 24:16 (the size itself, not minus one).
pub fn dcn_cursor_size(w: u32, h: u32) -> u32 {
    (w & 0x1FF) << 16 | (h & 0x1FF)
}
/// DPP `CURSOR0_CONTROL`: `CUR0_ENABLE` 0, `CUR0_EXPANSION_MODE` 1, `CUR0_MODE` from 4.
pub const CUR0_ENABLE: u32 = 1 << 0;
pub const CUR0_MODE_SHIFT: u32 = 4;
pub const CUR0_MODE_MASK: u32 = 3 << 4;

// --------------------------------------------------------- DCE surface

/// `GRPH_ENABLE`.
pub const GRPH_ENABLE: u32 = 1 << 0;
/// `GRPH_CONTROL`: depth 1:0 (2 = 32 bpp), format 10:8 (0 = ARGB8888 at
/// 32 bpp; UNCONFIRMED values), address translation 16 (the surface
/// address goes through a page table).
pub const GRPH_DEPTH_MASK: u32 = 3;
pub const GRPH_DEPTH_32BPP: u32 = 2;
pub const GRPH_FORMAT_MASK: u32 = 7 << 8;
pub const GRPH_ADDRESS_TRANSLATION_ENABLE: u32 = 1 << 16;
/// DCE 11.2 `GRPH_ARRAY_MODE` (23:20): 0 linear general, 1 linear aligned.
pub const GRPH_ARRAY_MODE_SHIFT: u32 = 20;
pub const GRPH_ARRAY_MODE_MASK: u32 = 0xF << 20;
/// DCE 12 `GRPH_SW_MODE` (24:20): 0 linear.
pub const GRPH_SW_MODE_MASK: u32 = 0x1F << 20;
/// `GRPH_PITCH` (14:0), pixels.
pub const GRPH_PITCH_MASK: u32 = 0x7FFF;
/// `GRPH_X_END`/`GRPH_Y_END` (14:0, exclusive); starts and offsets 13:0.
pub const GRPH_END_MASK: u32 = 0x7FFF;
pub const GRPH_START_MASK: u32 = 0x3FFF;
/// `GRPH_PRIMARY_SURFACE_ADDRESS` (31:8) and `_HIGH` (7:0): a 40-bit address.
pub const GRPH_ADDR_MASK: u32 = 0xFFFF_FF00;
pub const GRPH_ADDR_HIGH_MASK: u32 = 0xFF;
/// `GRPH_UPDATE`.
pub const GRPH_SURFACE_UPDATE_PENDING: u32 = 1 << 2;
pub const GRPH_UPDATE_LOCK: u32 = 1 << 16;
/// `GRPH_FLIP_CONTROL.GRPH_SURFACE_UPDATE_H_RETRACE_EN`: flips at the next
/// line instead of the next frame.
pub const GRPH_SURFACE_UPDATE_H_RETRACE_EN: u32 = 1 << 0;

// ---------------------------------------------------------- DCE cursor

/// `CUR_CONTROL`: `CURSOR_EN` 0, `CURSOR_MODE` 9:8.
pub const CURSOR_EN: u32 = 1 << 0;
/// `CUR_SIZE`: (height − 1) 6:0, (width − 1) 22:16.
pub fn dce_cursor_size(w: u32, h: u32) -> u32 {
    (w - 1) << 16 | (h - 1)
}
/// `CUR_HOT_SPOT`: 6:0 and 22:16.
pub const DCE_HOT_SPOT_MAX: u32 = 0x7F;
/// `CUR_UPDATE.CURSOR_UPDATE_LOCK`.
pub const CURSOR_UPDATE_LOCK: u32 = 1 << 16;

// ------------------------------------------------------------- HPD, AUX

/// `DC_HPD_INT_STATUS.DC_HPD_SENSE`: something is plugged in.
pub const HPD_SENSE: u32 = 1 << 1;

/// `AUX_CONTROL.AUX_EN`.
pub const AUX_EN: u32 = 1 << 0;
/// `AUX_SW_CONTROL`: `AUX_SW_GO` 0, `AUX_SW_WR_BYTES` 20:16.
pub const AUX_SW_GO: u32 = 1 << 0;
pub const AUX_SW_WR_BYTES_SHIFT: u32 = 16;
/// `AUX_ARB_CONTROL`: `AUX_REG_RW_CNTL_STATUS` 3:2 (who owns the engine's
/// registers; 1 = software, UNCONFIRMED), `AUX_SW_USE_AUX_REG_REQ` 16,
/// `AUX_SW_DONE_USING_AUX_REG` 17.
pub const AUX_ARB_PRIORITY_MASK: u32 = 3;
pub const AUX_REG_RW_CNTL_STATUS_MASK: u32 = 3 << 2;
pub const AUX_REG_RW_CNTL_STATUS_SW: u32 = 1 << 2;
pub const AUX_SW_USE_AUX_REG_REQ: u32 = 1 << 16;
pub const AUX_SW_DONE_USING_AUX_REG: u32 = 1 << 17;
/// `AUX_INTERRUPT_CONTROL.AUX_SW_DONE_ACK`.
pub const AUX_SW_DONE_ACK: u32 = 1 << 1;
/// `AUX_SW_STATUS`.
pub const AUX_SW_DONE: u32 = 1 << 0;
pub const AUX_SW_RX_TIMEOUT: u32 = 1 << 7;
pub const AUX_SW_RX_OVERFLOW: u32 = 1 << 8;
pub const AUX_SW_HPD_DISCON: u32 = 1 << 9;
pub const AUX_SW_RX_PARTIAL_BYTE: u32 = 1 << 10;
pub const AUX_SW_NON_AUX_MODE: u32 = 1 << 11;
pub const AUX_SW_RX_MIN_COUNT_VIOL: u32 = 1 << 12;
pub const AUX_SW_RX_INVALID_STOP: u32 = 1 << 14;
pub const AUX_SW_RX_SYNC_INVALID_L: u32 = 1 << 17;
pub const AUX_SW_RX_SYNC_INVALID_H: u32 = 1 << 18;
pub const AUX_SW_RX_INVALID_START: u32 = 1 << 19;
pub const AUX_SW_RX_RECV_NO_DET: u32 = 1 << 20;
pub const AUX_SW_RX_RECV_INVALID_H: u32 = 1 << 22;
pub const AUX_SW_RX_RECV_INVALID_L: u32 = 1 << 23;
pub const AUX_SW_REPLY_BYTE_COUNT_SHIFT: u32 = 24;
pub const AUX_SW_REPLY_BYTE_COUNT_MASK: u32 = 0x1F << 24;
/// Receive errors worth a retry.
pub const AUX_SW_RX_ERRORS: u32 = AUX_SW_RX_OVERFLOW
    | AUX_SW_RX_PARTIAL_BYTE
    | AUX_SW_RX_MIN_COUNT_VIOL
    | AUX_SW_RX_INVALID_STOP
    | AUX_SW_RX_SYNC_INVALID_L
    | AUX_SW_RX_SYNC_INVALID_H
    | AUX_SW_RX_INVALID_START
    | AUX_SW_RX_RECV_NO_DET
    | AUX_SW_RX_RECV_INVALID_H
    | AUX_SW_RX_RECV_INVALID_L;
/// `AUX_SW_DATA`: `AUX_SW_DATA_RW` 0 (1 = read), `AUX_SW_DATA` 15:8,
/// `AUX_SW_INDEX` 20:16, `AUX_SW_AUTOINCREMENT_DISABLE` 31.
pub const AUX_SW_DATA_RW: u32 = 1 << 0;
pub const AUX_SW_DATA_SHIFT: u32 = 8;
pub const AUX_SW_INDEX_SHIFT: u32 = 16;
pub const AUX_SW_AUTOINCREMENT_DISABLE: u32 = 1 << 31;
