//! Where the driver's scanout and cursor buffers go.
//!
//! The display engine scans VRAM (on an APU, the carve-out of system
//! memory the firmware set aside), which the CPU sees through the VRAM
//! aperture, BAR 0. Nothing on the GPU runs (the driver starts no engine),
//! so VRAM is free except for what the firmware put there: the GOP
//! framebuffer, near the bottom, and the firmware's own reservations
//! (PSP trusted memory, the VBIOS's scratch area, the IP discovery table),
//! which AMD's firmware places at the top. The driver therefore uses the
//! aperture from just above the GOP framebuffer up to three quarters of
//! the aperture, at most 512 MiB of it. That the space just above the
//! framebuffer is unused is an assumption (UNCONFIRMED on real hardware).

use crate::takeover::Gop;
use aurora_igfx::gtt::Space;

pub const MIB: u64 = 1 << 20;
/// Buffer alignment: far more than any DCN/DCE surface needs (256 bytes
/// for a linear surface, 4 KiB for a cursor).
pub const ALIGN: u64 = MIB;

/// The range of the aperture the driver allocates from, for a GOP
/// framebuffer `gop` in an aperture of `bar_size` bytes.
pub fn range(gop: &Gop, bar_size: u64) -> Option<(u64, u64)> {
    let start = (gop.offset + gop.bytes()).next_multiple_of(ALIGN);
    let end = (bar_size / 4 * 3).min(start + 512 * MIB);
    (end > start).then_some((start, end))
}

/// An allocator over [`range`], if it holds two screens' worth plus room
/// for cursors.
pub fn space(gop: &Gop, bar_size: u64) -> Option<Space> {
    let (start, end) = range(gop, bar_size)?;
    let need = 2 * gop.bytes().next_multiple_of(ALIGN) + 2 * ALIGN;
    (end - start >= need).then(|| Space::new(start, end))
}
