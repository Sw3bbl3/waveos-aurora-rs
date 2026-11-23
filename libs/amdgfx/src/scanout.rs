//! What the driver writes once it has taken over: new surface addresses
//! (flips), the cursor, and what it polls: flip completion and the frame
//! counter.
//!
//! A flip writes the address's high half, then the low half; the low write
//! arms the update, which the hardware applies at the next vertical
//! update (or line, if the firmware asked for immediate flips) and then
//! clears its pending bit. Nothing else about the plane changes: format,
//! pitch, viewport and watermarks stay the firmware's.

use crate::platform::{Block, Info, Layout};
use crate::regs::{self, Inst};
use crate::Mmio;

fn reg(i: &Inst, n: u8) -> Option<u32> {
    i.get(n as usize).copied().filter(|&r| r != 0)
}

/// Points `pipe`'s primary surface at `addr`.
pub fn flip(m: &impl Mmio, info: &Info, pipe: u8, addr: u64) {
    match info.block.layout() {
        Layout::Dcn(l) => {
            if let (Some(hi), Some(lo)) = (reg(&l.addr_high, pipe), reg(&l.addr, pipe)) {
                m.write(hi, (addr >> 32) as u32 & regs::ADDR_HIGH_MASK_DCN);
                m.write(lo, addr as u32);
            }
        }
        Layout::Dce(l) => {
            if let (Some(hi), Some(lo)) = (reg(&l.grph_addr_high, pipe), reg(&l.grph_addr, pipe)) {
                m.write(hi, (addr >> 32) as u32 & regs::GRPH_ADDR_HIGH_MASK);
                m.write(lo, addr as u32 & regs::GRPH_ADDR_MASK);
            }
        }
    }
}

/// A flip on `pipe` hasn't reached the screen yet.
pub fn flip_pending(m: &impl Mmio, info: &Info, pipe: u8) -> bool {
    match (info.block, info.block.layout()) {
        (Block::Dcn(v), Layout::Dcn(l)) => {
            reg(&l.flip_control, pipe).is_some_and(|r| m.read(r) & regs::flip_pending_bit(v) != 0)
        }
        (_, Layout::Dce(l)) => {
            reg(&l.grph_update, pipe).is_some_and(|r| m.read(r) & regs::GRPH_SURFACE_UPDATE_PENDING != 0)
        }
        _ => false,
    }
}

/// The surface address `pipe` is programmed with.
pub fn surface(m: &impl Mmio, info: &Info, pipe: u8) -> u64 {
    let rd = |i: &Inst| reg(i, pipe).map_or(0, |a| m.read(a)) as u64;
    match info.block.layout() {
        Layout::Dcn(l) => (rd(&l.addr_high) & regs::ADDR_HIGH_MASK_DCN as u64) << 32 | rd(&l.addr),
        Layout::Dce(l) => {
            (rd(&l.grph_addr_high) & regs::GRPH_ADDR_HIGH_MASK as u64) << 32
                | rd(&l.grph_addr) & regs::GRPH_ADDR_MASK as u64
        }
    }
}

/// Waits (polling, at most `timeout_us`) until no flip is pending on
/// `pipe`. Returns whether it completed.
pub fn wait_flip(m: &impl Mmio, info: &Info, pipe: u8, timeout_us: u32) -> bool {
    let mut waited = 0;
    loop {
        if !flip_pending(m, info, pipe) {
            return true;
        }
        if waited >= timeout_us {
            return false;
        }
        m.delay_us(100);
        waited += 100;
    }
}

/// The frame counter of timing generator `otg` (24 bits).
pub fn frame_count(m: &impl Mmio, info: &Info, otg: u8) -> u32 {
    let r = match info.block.layout() {
        Layout::Dcn(l) => reg(&l.otg_frame_count, otg),
        Layout::Dce(l) => reg(&l.crtc_frame_count, otg),
    };
    r.map_or(0, |r| m.read(r) & regs::FRAME_COUNT_MASK)
}

/// The timing generator `otg` is still running (after sleep, say).
pub fn running(m: &impl Mmio, info: &Info, otg: u8) -> bool {
    let r = match info.block.layout() {
        Layout::Dcn(l) => reg(&l.otg_control, otg),
        Layout::Dce(l) => reg(&l.crtc_control, otg),
    };
    r.is_some_and(|r| m.read(r) & regs::MASTER_EN != 0)
}

// ------------------------------------------------------------- cursor

/// The cursor is `SIZE`×`SIZE` ARGB, premultiplied, `SIZE` pixels per row.
pub const CURSOR: u32 = 64;
pub const CURSOR_BYTES: u64 = (CURSOR * CURSOR * 4) as u64;

/// Where the cursor registers put an image whose top-left corner is at
/// (`x`, `y`): the position can't be negative, so a cursor hanging off the
/// top or left edge is placed at 0 with its hot spot moved into the image
/// by as much. `None`: nothing of it is on screen.
pub fn cursor_place(x: i32, y: i32, max_hot: u32) -> Option<(u32, u32, u32, u32)> {
    if x <= -(CURSOR as i32) || y <= -(CURSOR as i32) {
        return None;
    }
    let (hx, hy) = ((-x).max(0) as u32, (-y).max(0) as u32);
    if hx > max_hot || hy > max_hot {
        return None;
    }
    Some((x.max(0) as u32 & regs::CURSOR_POS_FIELD, y.max(0) as u32 & regs::CURSOR_POS_FIELD, hx, hy))
}

/// Sets up the cursor of `pipe` (64×64 premultiplied ARGB at `addr`),
/// still hidden. Reads back what it wrote; false if the registers didn't
/// keep it (wrong offsets: the caller then leaves the cursor alone).
pub fn cursor_setup(m: &impl Mmio, info: &Info, pipe: u8, addr: u64) -> bool {
    match info.block.layout() {
        Layout::Dcn(l) => {
            let (Some(set), Some(ctl), Some(size), Some(dpp)) = (
                reg(&l.cur_settings, pipe),
                reg(&l.cur_control, pipe),
                reg(&l.cur_size, pipe),
                reg(&l.dpp_cur_control, pipe),
            ) else {
                return false;
            };
            let control = regs::CURSOR_MODE_ARGB_PREMULTIPLIED << regs::CURSOR_MODE_SHIFT
                | regs::CURSOR_PITCH_64 << regs::CURSOR_PITCH_SHIFT
                | regs::CURSOR_LINES_PER_CHUNK_64 << regs::CURSOR_LINES_PER_CHUNK_SHIFT;
            m.write(set, regs::CURSOR_SETTINGS_VALUE);
            m.write(size, regs::dcn_cursor_size(CURSOR, CURSOR));
            m.write(ctl, control);
            let d = m.read(dpp) & !(regs::CUR0_ENABLE | regs::CUR0_MODE_MASK | 2);
            m.write(dpp, d | regs::CURSOR_MODE_ARGB_PREMULTIPLIED << regs::CUR0_MODE_SHIFT);
            if let Some(r) = reg(&l.cur_dst_offset, pipe) {
                m.write(r, 0);
            }
            cursor_address(m, info, pipe, addr);
            m.read(size) == regs::dcn_cursor_size(CURSOR, CURSOR) && m.read(ctl) == control
        }
        Layout::Dce(l) => {
            let (Some(upd), Some(ctl), Some(size)) =
                (reg(&l.cur_update, pipe), reg(&l.cur_control, pipe), reg(&l.cur_size, pipe))
            else {
                return false;
            };
            let control = regs::CURSOR_MODE_ARGB_PREMULTIPLIED << regs::CURSOR_MODE_SHIFT;
            m.modify(upd, 0, regs::CURSOR_UPDATE_LOCK);
            m.write(ctl, control);
            m.write(size, regs::dce_cursor_size(CURSOR, CURSOR));
            cursor_address(m, info, pipe, addr);
            m.modify(upd, regs::CURSOR_UPDATE_LOCK, 0);
            m.read(size) == regs::dce_cursor_size(CURSOR, CURSOR) && m.read(ctl) == control
        }
    }
}

fn cursor_address(m: &impl Mmio, info: &Info, pipe: u8, addr: u64) {
    let (hi, lo) = match info.block.layout() {
        Layout::Dcn(l) => (reg(&l.cur_addr_high, pipe), reg(&l.cur_addr, pipe)),
        Layout::Dce(l) => (reg(&l.cur_addr_high, pipe), reg(&l.cur_addr, pipe)),
    };
    if let (Some(hi), Some(lo)) = (hi, lo) {
        m.write(hi, (addr >> 32) as u32);
        m.write(lo, addr as u32);
    }
}

/// Shows the cursor of `pipe` with its image at `addr` and its top-left
/// corner at (`x`, `y`); hides it when it is entirely off screen.
pub fn cursor_show(m: &impl Mmio, info: &Info, pipe: u8, addr: u64, x: i32, y: i32) {
    let max_hot = match info.block {
        Block::Dcn(_) => regs::DCN_HOT_SPOT_MAX,
        _ => regs::DCE_HOT_SPOT_MAX,
    };
    let Some((px, py, hx, hy)) = cursor_place(x, y, max_hot) else {
        cursor_hide(m, info, pipe);
        return;
    };
    match info.block.layout() {
        Layout::Dcn(l) => {
            let (Some(ctl), Some(pos), Some(hot), Some(dpp)) = (
                reg(&l.cur_control, pipe),
                reg(&l.cur_position, pipe),
                reg(&l.cur_hot_spot, pipe),
                reg(&l.dpp_cur_control, pipe),
            ) else {
                return;
            };
            cursor_address(m, info, pipe, addr);
            m.write(pos, px << 16 | py);
            m.write(hot, hx << 16 | hy);
            m.modify(ctl, 0, regs::CURSOR_ENABLE);
            m.modify(dpp, 0, regs::CUR0_ENABLE);
        }
        Layout::Dce(l) => {
            let (Some(upd), Some(ctl), Some(pos), Some(hot)) = (
                reg(&l.cur_update, pipe),
                reg(&l.cur_control, pipe),
                reg(&l.cur_position, pipe),
                reg(&l.cur_hot_spot, pipe),
            ) else {
                return;
            };
            m.modify(upd, 0, regs::CURSOR_UPDATE_LOCK);
            cursor_address(m, info, pipe, addr);
            m.write(pos, px << 16 | py);
            m.write(hot, hx << 16 | hy);
            m.modify(ctl, 0, regs::CURSOR_EN);
            m.modify(upd, regs::CURSOR_UPDATE_LOCK, 0);
        }
    }
}

pub fn cursor_hide(m: &impl Mmio, info: &Info, pipe: u8) {
    match info.block.layout() {
        Layout::Dcn(l) => {
            if let Some(r) = reg(&l.cur_control, pipe) {
                m.modify(r, regs::CURSOR_ENABLE, 0);
            }
            if let Some(r) = reg(&l.dpp_cur_control, pipe) {
                m.modify(r, regs::CUR0_ENABLE, 0);
            }
        }
        Layout::Dce(l) => {
            if let Some(r) = reg(&l.cur_control, pipe) {
                m.modify(r, regs::CURSOR_EN, 0);
            }
        }
    }
}

/// The cursor of `pipe` is on (for the self-test and logs).
pub fn cursor_on(m: &impl Mmio, info: &Info, pipe: u8) -> bool {
    match info.block.layout() {
        Layout::Dcn(l) => reg(&l.cur_control, pipe).is_some_and(|r| m.read(r) & regs::CURSOR_ENABLE != 0),
        Layout::Dce(l) => reg(&l.cur_control, pipe).is_some_and(|r| m.read(r) & regs::CURSOR_EN != 0),
    }
}

/// The (x, y) of the cursor position register and the hot spot of `pipe`.
pub fn cursor_regs(m: &impl Mmio, info: &Info, pipe: u8) -> ((u32, u32), (u32, u32)) {
    let (pos, hot) = match info.block.layout() {
        Layout::Dcn(l) => (reg(&l.cur_position, pipe), reg(&l.cur_hot_spot, pipe)),
        Layout::Dce(l) => (reg(&l.cur_position, pipe), reg(&l.cur_hot_spot, pipe)),
    };
    let split = |r: Option<u32>| {
        r.map_or((0, 0), |r| {
            let v = m.read(r);
            (v >> 16, v & 0xFFFF)
        })
    };
    (split(pos), split(hot))
}

/// `px` (straight-alpha ARGB) premultiplied, as the cursor wants it.
pub fn premultiply(px: u32) -> u32 {
    aurora_igfx::plane::premultiply(px)
}
