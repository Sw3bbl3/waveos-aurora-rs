//! Reading back what the firmware lit, and deciding whether to take it over.
//!
//! The GOP leaves one or more timing generators running (an OTG on DCN, a
//! CRTC on DCE), each scanning a linear 32-bit surface in VRAM: one or two
//! HUBPs feed an OTG on DCN (two when a wide mode is split across pipes),
//! DCP *i* feeds CRTC *i* on DCE. The driver keeps all of that and only
//! ever writes a new surface address, so the one thing it must be sure of
//! is that the surface it found *is* the framebuffer the bootloader was
//! given ([`Gop`]): same size, same pitch, same format, one surface for
//! every lit display. If the register offsets were wrong for this chip,
//! or the firmware did something unexpected, that fingerprint does not
//! match and the driver declines ([`Decline`]) having only read registers.
//!
//! Timing registers count from the start of sync: `*_H_TOTAL` is the total
//! minus one, sync runs from `*_H_SYNC_A` start to end, and the active
//! area from blank end to blank start (UNCONFIRMED; it only affects the
//! mode the driver reports, never what it programs).

use crate::platform::{Block, Info, Layout};
use crate::regs::{self, Inst};
use crate::Mmio;
use alloc::vec::Vec;
use aurora_igfx::timing::Timings;

/// The firmware framebuffer as the bootloader reported it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Gop {
    /// Byte offset of the framebuffer in the VRAM aperture (BAR 0).
    pub offset: u64,
    pub width: u32,
    pub height: u32,
    /// Pixels per row.
    pub stride: u32,
    /// Blue in the lowest byte (the GOP's `PixelBlueGreenRedReserved8BitPerColor`).
    pub bgr: bool,
}

impl Gop {
    pub fn bytes(&self) -> u64 {
        self.stride as u64 * 4 * self.height as u64
    }
}

/// One running timing generator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Head {
    /// OTG (DCN) or CRTC (DCE) index.
    pub otg: u8,
    /// The HUBPs (DCN) or the DCP (DCE) scanning for it, left to right.
    pub pipes: Vec<u8>,
    pub timings: Timings,
    /// What the plane scans (differs from the active area when scaled).
    pub src: (u32, u32),
    pub scaled: bool,
    /// The surface's address in the GPU's address space.
    pub addr: u64,
    /// Pixels per row.
    pub pitch: u32,
    /// The firmware flips immediately (on the next line), not at vblank.
    pub immediate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Firmware {
    pub heads: Vec<Head>,
    /// The firmware framebuffer's address in the GPU's address space.
    pub addr: u64,
}

impl Firmware {
    /// The GPU address of byte `offset` of the VRAM aperture: the
    /// framebuffer's address moved by the same distance as in the BAR (the
    /// aperture maps VRAM linearly).
    pub fn gpu_addr(&self, gop: &Gop, offset: u64) -> u64 {
        self.addr.wrapping_sub(gop.offset).wrapping_add(offset)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decline {
    /// The bootloader's framebuffer isn't 32-bit BGRX.
    GopFormat,
    /// No timing generator runs (or the register offsets don't fit this chip).
    NoActiveOtg,
    Interlaced(u8),
    /// The timing registers don't make sense.
    Timings(u8),
    /// No plane scans for this timing generator.
    NoPlane(u8),
    /// The surface is tiled.
    Tiled(u8, u32),
    /// Not 32-bit ARGB8888, or colour channels swapped.
    Format(u8, u32),
    Rotated(u8, u32),
    /// The surface is compressed (DCC).
    Compressed(u8),
    /// The surface is a protected (TMZ) one.
    Protected(u8),
    /// The surface address goes through a page table.
    Translated(u8),
    /// The firmware holds the surface update lock.
    Locked(u8),
    /// The plane doesn't start at the surface's corner, or split pipes don't
    /// tile the screen.
    Viewport(u8),
    /// Split pipes scan different surfaces.
    Split(u8),
    /// What runs isn't the bootloader's framebuffer: (size, pitch) found
    /// against what the bootloader reported.
    NotTheGop(u8, (u32, u32), u32),
    /// Two displays scan different surfaces.
    TwoSurfaces,
}

impl core::fmt::Display for Decline {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match *self {
            Decline::GopFormat => write!(f, "the firmware framebuffer is not 32-bit BGRX"),
            Decline::NoActiveOtg => write!(f, "no timing generator is running"),
            Decline::Interlaced(o) => write!(f, "OTG {o} is interlaced"),
            Decline::Timings(o) => write!(f, "OTG {o}: the timing registers don't make sense"),
            Decline::NoPlane(o) => write!(f, "OTG {o}: no plane scans for it"),
            Decline::Tiled(o, v) => write!(f, "OTG {o}: the surface is tiled ({v:#x})"),
            Decline::Format(o, v) => write!(f, "OTG {o}: the surface is not ARGB8888 ({v:#x})"),
            Decline::Rotated(o, v) => write!(f, "OTG {o}: the surface is rotated or mirrored ({v:#x})"),
            Decline::Compressed(o) => write!(f, "OTG {o}: the surface is compressed (DCC)"),
            Decline::Protected(o) => write!(f, "OTG {o}: the surface is protected (TMZ)"),
            Decline::Translated(o) => write!(f, "OTG {o}: the surface address is translated by a page table"),
            Decline::Locked(o) => write!(f, "OTG {o}: the firmware holds the surface update lock"),
            Decline::Viewport(o) => write!(f, "OTG {o}: the plane doesn't cover the surface from its corner"),
            Decline::Split(o) => write!(f, "OTG {o}: its pipes scan different surfaces"),
            Decline::NotTheGop(o, (w, h), p) => {
                write!(f, "OTG {o} scans {w}x{h} with a pitch of {p} pixels, not the firmware framebuffer")
            }
            Decline::TwoSurfaces => write!(f, "the displays scan different surfaces"),
        }
    }
}

fn reg(i: &Inst, n: u8) -> Option<u32> {
    i.get(n as usize).copied().filter(|&r| r != 0)
}

/// Decodes AMD timing registers (total, blank start/end, sync start/end;
/// fields `field` wide) for one direction: (active, sync start, sync end,
/// total) in DRM's terms.
pub fn decode(total: u32, blank: u32, sync: u32, field: u32) -> Option<(u32, u32, u32, u32)> {
    let total = (total & field) + 1;
    let (bs, be) = (blank & field, blank >> 16 & field);
    let (ss, se) = (sync & field, sync >> 16 & field);
    let active = bs.checked_sub(be)?;
    let width = se.checked_sub(ss)?;
    let front = (total.checked_sub(bs)?) + ss;
    let start = active + front;
    (active > 0 && width > 0 && start + width <= total).then_some((active, start, start + width, total))
}

/// The registers for (active, sync start, sync end, total): (total, blank, sync).
pub fn encode(t: (u32, u32, u32, u32)) -> (u32, u32, u32) {
    let (active, start, end, total) = t;
    let blank_end = total - start;
    (total - 1, (blank_end + active) | blank_end << 16, (end - start) << 16)
}

/// Reads back every running timing generator and its surface, and checks
/// them against the bootloader's framebuffer. Only reads registers.
pub fn read(m: &impl Mmio, info: &Info, gop: &Gop) -> Result<Firmware, Decline> {
    if !gop.bgr {
        return Err(Decline::GopFormat);
    }
    let heads = match info.block.layout() {
        Layout::Dcn(l) => read_dcn(m, l)?,
        Layout::Dce(l) => read_dce(m, l, info.block)?,
    };
    if heads.is_empty() {
        return Err(Decline::NoActiveOtg);
    }
    for h in &heads {
        if h.src != (gop.width, gop.height) || h.pitch != gop.stride {
            return Err(Decline::NotTheGop(h.otg, h.src, h.pitch));
        }
    }
    let addr = heads[0].addr;
    if heads.iter().any(|h| h.addr != addr) {
        return Err(Decline::TwoSurfaces);
    }
    Ok(Firmware { heads, addr })
}

fn timings(m: &impl Mmio, otg: u8, field: u32, r: [Option<u32>; 8]) -> Result<Timings, Decline> {
    let [ht, hb, hs, hsc, vt, vb, vs, vsc] = r.map(|x| x.map_or(0, |a| m.read(a)));
    let h = decode(ht, hb, hs, field).ok_or(Decline::Timings(otg))?;
    let v = decode(vt, vb, vs, field).ok_or(Decline::Timings(otg))?;
    let t = Timings::new(h, v, hsc & regs::SYNC_POL_NEGATIVE == 0, vsc & regs::SYNC_POL_NEGATIVE == 0);
    if !t.sane() {
        return Err(Decline::Timings(otg));
    }
    Ok(t)
}

fn read_dcn(m: &impl Mmio, l: &regs::Dcn) -> Result<Vec<Head>, Decline> {
    let mut heads = Vec::new();
    for otg in 0..6u8 {
        let Some(ctl) = reg(&l.otg_control, otg) else { continue };
        if m.read(ctl) & regs::MASTER_EN == 0 {
            continue;
        }
        if reg(&l.otg_interlace, otg).is_some_and(|r| m.read(r) & regs::INTERLACE_ENABLE != 0) {
            return Err(Decline::Interlaced(otg));
        }
        let t = timings(
            m,
            otg,
            regs::OTG_FIELD,
            [
                reg(&l.otg_h_total, otg),
                reg(&l.otg_h_blank, otg),
                reg(&l.otg_h_sync, otg),
                reg(&l.otg_h_sync_cntl, otg),
                reg(&l.otg_v_total, otg),
                reg(&l.otg_v_blank, otg),
                reg(&l.otg_v_sync, otg),
                reg(&l.otg_v_sync_cntl, otg),
            ],
        )?;
        // The HUBPs that fetch for this OTG: (x, y, width, height, pipe).
        let mut pipes: Vec<(u32, u32, u32, u32, u8)> = Vec::new();
        let (mut addr, mut pitch, mut immediate) = (None, 0, false);
        for p in 0..6u8 {
            let Some(cntl) = reg(&l.hubp_cntl, p) else { continue };
            let c = m.read(cntl);
            if c & regs::HUBP_BLANK_EN != 0 || (c & regs::HUBP_VTG_SEL_MASK) >> regs::HUBP_VTG_SEL_SHIFT != otg as u32 {
                continue;
            }
            let rd = |i: &Inst| reg(i, p).map_or(0, |a| m.read(a));
            let dim = rd(&l.viewport_dim);
            let a = (rd(&l.addr_high) as u64 & regs::ADDR_HIGH_MASK_DCN as u64) << 32 | rd(&l.addr) as u64;
            let (w, h) = (dim & regs::VIEWPORT_FIELD, dim >> 16 & regs::VIEWPORT_FIELD);
            // An unblanked pipe with nothing to fetch is one nobody uses
            // (power-gated pipes read as zeros).
            if w == 0 || h == 0 || a == 0 {
                continue;
            }
            let flip = rd(&l.flip_control);
            if flip & regs::SURFACE_UPDATE_LOCK != 0 {
                return Err(Decline::Locked(otg));
            }
            let sc = rd(&l.surface_control);
            if sc & regs::PRIMARY_SURFACE_DCC_EN != 0 {
                return Err(Decline::Compressed(otg));
            }
            if sc & regs::PRIMARY_SURFACE_TMZ != 0 {
                return Err(Decline::Protected(otg));
            }
            let tiling = rd(&l.tiling_config);
            if tiling & regs::SW_MODE_MASK != 0 {
                return Err(Decline::Tiled(otg, tiling));
            }
            let config = rd(&l.surface_config);
            if config & (regs::ROTATION_ANGLE_MASK | regs::H_MIRROR_EN) != 0 {
                return Err(Decline::Rotated(otg, config));
            }
            if config & regs::SURFACE_PIXEL_FORMAT_MASK != regs::PIXEL_FORMAT_ARGB8888 {
                return Err(Decline::Format(otg, config));
            }
            let pp = rd(&l.pitch) & regs::PITCH_MASK;
            match addr {
                None => (addr, pitch) = (Some(a), pp),
                Some(x) if x == a && pitch == pp => {}
                _ => return Err(Decline::Split(otg)),
            }
            immediate |= flip & regs::SURFACE_FLIP_TYPE != 0;
            let start = rd(&l.viewport_start);
            pipes.push((start & regs::VIEWPORT_FIELD, start >> 16 & regs::VIEWPORT_FIELD, w, h, p));
        }
        let Some(addr) = addr else { return Err(Decline::NoPlane(otg)) };
        // Split pipes sit side by side, together covering the surface from
        // its top-left corner.
        pipes.sort_by_key(|p| p.0);
        let height = pipes[0].3;
        let mut x = 0;
        for p in &pipes {
            if p.0 != x || p.1 != 0 || p.3 != height {
                return Err(Decline::Viewport(otg));
            }
            x += p.2;
        }
        let src = (x, height);
        heads.push(Head {
            otg,
            pipes: pipes.iter().map(|p| p.4).collect(),
            scaled: src != (t.hactive, t.vactive),
            timings: t,
            src,
            addr,
            pitch,
            immediate,
        });
    }
    Ok(heads)
}

fn read_dce(m: &impl Mmio, l: &regs::Dce, block: Block) -> Result<Vec<Head>, Decline> {
    let mut heads = Vec::new();
    for c in 0..6u8 {
        let Some(ctl) = reg(&l.crtc_control, c) else { continue };
        if m.read(ctl) & regs::MASTER_EN == 0 {
            continue;
        }
        if reg(&l.crtc_interlace, c).is_some_and(|r| m.read(r) & regs::INTERLACE_ENABLE != 0) {
            return Err(Decline::Interlaced(c));
        }
        let t = timings(
            m,
            c,
            regs::CRTC_FIELD,
            [
                reg(&l.crtc_h_total, c),
                reg(&l.crtc_h_blank, c),
                reg(&l.crtc_h_sync, c),
                reg(&l.crtc_h_sync_cntl, c),
                reg(&l.crtc_v_total, c),
                reg(&l.crtc_v_blank, c),
                reg(&l.crtc_v_sync, c),
                reg(&l.crtc_v_sync_cntl, c),
            ],
        )?;
        let rd = |i: &Inst| reg(i, c).map_or(0, |a| m.read(a));
        if rd(&l.grph_enable) & regs::GRPH_ENABLE == 0 {
            return Err(Decline::NoPlane(c));
        }
        if rd(&l.grph_update) & regs::GRPH_UPDATE_LOCK != 0 {
            return Err(Decline::Locked(c));
        }
        let ctl = rd(&l.grph_control);
        if ctl & regs::GRPH_ADDRESS_TRANSLATION_ENABLE != 0 {
            return Err(Decline::Translated(c));
        }
        let linear = match block {
            Block::Dce112 => (ctl & regs::GRPH_ARRAY_MODE_MASK) >> regs::GRPH_ARRAY_MODE_SHIFT <= 1,
            _ => ctl & regs::GRPH_SW_MODE_MASK == 0,
        };
        if !linear {
            return Err(Decline::Tiled(c, ctl));
        }
        let swap = rd(&l.grph_swap);
        if ctl & regs::GRPH_DEPTH_MASK != regs::GRPH_DEPTH_32BPP || ctl & regs::GRPH_FORMAT_MASK != 0 || swap != 0 {
            return Err(Decline::Format(c, ctl));
        }
        let starts = [&l.grph_x_start, &l.grph_y_start, &l.grph_offset_x, &l.grph_offset_y];
        if starts.iter().any(|r| rd(r) & regs::GRPH_START_MASK != 0) {
            return Err(Decline::Viewport(c));
        }
        let src = (rd(&l.grph_x_end) & regs::GRPH_END_MASK, rd(&l.grph_y_end) & regs::GRPH_END_MASK);
        let addr = ((rd(&l.grph_addr_high) & regs::GRPH_ADDR_HIGH_MASK) as u64) << 32
            | (rd(&l.grph_addr) & regs::GRPH_ADDR_MASK) as u64;
        if src.0 == 0 || src.1 == 0 || addr == 0 {
            return Err(Decline::NoPlane(c));
        }
        heads.push(Head {
            otg: c,
            pipes: alloc::vec![c],
            scaled: src != (t.hactive, t.vactive),
            timings: t,
            src,
            addr,
            pitch: rd(&l.grph_pitch) & regs::GRPH_PITCH_MASK,
            immediate: rd(&l.grph_flip_control) & regs::GRPH_SURFACE_UPDATE_H_RETRACE_EN != 0,
        });
    }
    Ok(heads)
}
