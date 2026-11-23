//! Primary (universal plane 1) and cursor plane register values.
//!
//! The driver keeps everything the firmware set in `PLANE_CTL` that is not
//! about the surface (gamma, CSC, keying off, trickle feed…) and replaces
//! the surface description: 8:8:8:8 BGRX (DRM's XRGB8888), linear, not
//! rotated. Watermarks and the display buffer allocation the firmware
//! computed stay valid because the bytes per pixel, the tiling and the
//! width stay the same (see [`crate::takeover`] for the checks).

use crate::regs::*;
use crate::Mmio;

/// The display version decides where the format field is.
pub fn format_mask(display: u8) -> u32 {
    if display >= 11 {
        PLANE_CTL_FORMAT_MASK_ICL
    } else {
        PLANE_CTL_FORMAT_MASK_SKL
    }
}

/// Bytes per pixel of the format in `ctl`, if it is a 32-bit RGB format.
pub fn rgb32(ctl: u32, display: u8) -> bool {
    let f = ctl & format_mask(display);
    f == PLANE_CTL_FORMAT_8888 || f == PLANE_CTL_FORMAT_2101010
}

/// `PLANE_CTL` for a linear XRGB8888 surface, keeping `firmware`'s other bits.
pub fn primary_ctl(firmware: u32, display: u8) -> u32 {
    let clear = format_mask(display)
        | PLANE_CTL_ORDER_RGBX
        | PLANE_CTL_TILED_MASK
        | PLANE_CTL_ROTATE_MASK
        | PLANE_CTL_FLIP_HORIZONTAL
        | if display < 10 { PLANE_CTL_ALPHA_MASK_SKL } else { 0 };
    (firmware & !clear) | PLANE_CTL_ENABLE | PLANE_CTL_FORMAT_8888
}

/// `PLANE_STRIDE` for a linear surface `pitch` bytes per row (a multiple of 64).
pub fn stride(pitch: u32) -> u32 {
    pitch / STRIDE_UNIT
}

/// What a primary plane shows: a surface at `surf` in the GGTT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Primary {
    pub ctl: u32,
    pub stride: u32,
    pub surf: u32,
}

/// Points `pipe`'s primary plane at a new surface. `PLANE_SURF` is written
/// last: it arms the other registers, and everything changes together at
/// the next vblank.
pub fn flip(m: &impl Mmio, pipe: u8, p: &Primary) {
    m.write(plane_ctl(pipe, 0), p.ctl);
    m.write(plane_stride(pipe, 0), p.stride);
    m.write(plane_offset(pipe, 0), 0);
    m.write(plane_surf(pipe, 0), p.surf);
}

/// `CURPOS`: sign and magnitude, y in 31 and 27:16, x in 15 and 11:0.
pub fn cursor_pos(x: i32, y: i32) -> u32 {
    let part = |v: i32| -> u32 {
        let mag = v.unsigned_abs().min(0xFFF);
        if v < 0 {
            0x8000 | mag
        } else {
            mag
        }
    };
    part(y) << 16 | part(x)
}

/// The (x, y) of a `CURPOS` value.
pub fn cursor_pos_decode(v: u32) -> (i32, i32) {
    let part = |p: u32| -> i32 {
        let mag = (p & 0xFFF) as i32;
        if p & 0x8000 != 0 {
            -mag
        } else {
            mag
        }
    };
    (part(v & 0xFFFF), part(v >> 16))
}

/// Shows (or moves) `pipe`'s 64×64 ARGB cursor at `base` with its top-left
/// corner at (`x`, `y`). `CURBASE` is written last: it arms `CURCNTR` and `CURPOS`.
pub fn cursor_show(m: &impl Mmio, pipe: u8, base: u32, x: i32, y: i32) {
    if m.read(curcntr(pipe)) & 0x3F != CURSOR_MODE_64_ARGB {
        m.write(curcntr(pipe), CURSOR_MODE_64_ARGB);
    }
    m.write(curpos(pipe), cursor_pos(x, y));
    m.write(curbase(pipe), base);
}

pub fn cursor_hide(m: &impl Mmio, pipe: u8) {
    m.write(curcntr(pipe), 0);
    let base = m.read(curbase(pipe));
    m.write(curbase(pipe), base);
}

/// Converts straight alpha (the compositor's 0xAARRGGBB) to the
/// premultiplied alpha the cursor plane blends with.
pub fn premultiply(px: u32) -> u32 {
    let a = px >> 24;
    if a == 0xFF {
        return px;
    }
    let ch = |shift: u32| ((px >> shift & 0xFF) * a + 127) / 255;
    a << 24 | ch(16) << 16 | ch(8) << 8 | ch(0)
}
