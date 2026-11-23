//! Display buffer (DDB) allocations and watermarks, for one thing: giving
//! the cursor plane room on a pipe the firmware set up for its primary
//! plane alone.
//!
//! Since display 9 every plane fetches into its own slice of the display
//! buffer (`PLANE_BUF_CFG`/`CUR_BUF_CFG`, in 512-byte blocks, end
//! inclusive) and has watermark levels 0–7 (`PLANE_WM`/`CUR_WM`: enable,
//! lines, blocks) that say how much must be buffered before memory may
//! sleep for that level's latency. The hardware only enters a level when
//! every enabled plane has it enabled, so a plane with fewer levels costs
//! power, never correctness.
//!
//! The firmware (GOP) gives the whole pipe allocation to plane 1 and none
//! to the cursor. [`plan_cursor`] carves a few blocks off the end of plane
//! 1's slice, disables plane 1's levels that no longer fit, and derives the
//! cursor's level 0 from plane 1's: at the same pixel rate and line time,
//! the same number of *lines* covers the same latency, and a 64-pixel
//! ARGB cursor line is one block. Levels 1–7 stay off for the cursor.

/// A watermark register value: bit 31 enables, lines in 26:14, blocks in 11:0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Wm {
    pub enabled: bool,
    pub lines: u32,
    pub blocks: u32,
}

impl Wm {
    pub fn decode(v: u32) -> Wm {
        Wm { enabled: v & 1 << 31 != 0, lines: v >> 14 & 0x1FFF, blocks: v & 0xFFF }
    }
    pub fn encode(&self) -> u32 {
        if !self.enabled {
            return 0;
        }
        1 << 31 | (self.lines & 0x1FFF) << 14 | (self.blocks & 0xFFF)
    }
}

/// A DDB slice, [start, end) in blocks; empty when `start == end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Ddb {
    pub start: u32,
    pub end: u32,
}

impl Ddb {
    /// `*_BUF_CFG`: end (inclusive) in 27:16, start in 11:0; 0 means none.
    pub fn decode(v: u32) -> Ddb {
        if v == 0 {
            return Ddb::default();
        }
        Ddb { start: v & 0xFFF, end: (v >> 16 & 0xFFF) + 1 }
    }
    pub fn encode(&self) -> u32 {
        if self.size() == 0 {
            return 0;
        }
        (self.end - 1) << 16 | self.start
    }
    pub fn size(&self) -> u32 {
        self.end.saturating_sub(self.start)
    }
}

/// Blocks given to the cursor (what a single-pipe Skylake setup commonly
/// uses for a 64×64 cursor; enough for level 0 at any common pixel rate).
pub const CURSOR_BLOCKS: u32 = 8;
/// Plane 1 must keep at least this much.
const MIN_PLANE_BLOCKS: u32 = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorPlan {
    pub plane_ddb: Ddb,
    /// Plane 1's levels after the carve (higher ones that no longer fit off).
    pub plane_wm: [Wm; 8],
    pub cursor_ddb: Ddb,
    pub cursor_wm0: Wm,
    /// Display 12+: the cursor's SAGV watermark (derived like level 0).
    pub cursor_sagv: Option<Wm>,
}

/// Why the cursor plane can't be set up; the driver falls back to drawing
/// the pointer in software.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoCursor {
    /// Plane 1 has no allocation or no level 0 (not what a lit pipe has).
    PlaneUnset,
    /// Plane 1's slice is too small to share.
    TooSmall,
    /// Plane 1's level 0 would no longer fit.
    Level0,
}

/// The cursor's watermark for plane 1's `wm` at the same pixel rate: the
/// same lines, one block per cursor line, plus one line and one block of
/// margin (the rounding the manuals' method 2 adds).
pub fn cursor_wm_from(wm: Wm, room: u32) -> Option<Wm> {
    if !wm.enabled {
        return None;
    }
    let blocks = wm.lines + 2;
    (blocks < room).then_some(Wm { enabled: true, lines: wm.lines.max(1), blocks })
}

/// Plans the cursor's share of a pipe whose plane 1 has `plane_buf`
/// (`PLANE_BUF_CFG`) and watermarks `plane_wm` (`PLANE_WM_0..7`), and
/// `plane_sagv` (`PLANE_WM_SAGV`, display 12+).
pub fn plan_cursor(plane_buf: u32, plane_wm: [u32; 8], plane_sagv: Option<u32>) -> Result<CursorPlan, NoCursor> {
    let ddb = Ddb::decode(plane_buf);
    let mut wm = plane_wm.map(Wm::decode);
    if ddb.size() == 0 || !wm[0].enabled {
        return Err(NoCursor::PlaneUnset);
    }
    if ddb.size() < MIN_PLANE_BLOCKS + CURSOR_BLOCKS {
        return Err(NoCursor::TooSmall);
    }
    let lines0 = wm[0].lines;
    // Room for the cursor's level 0 (and its SAGV level, if any).
    let sagv_lines = plane_sagv.map(Wm::decode).filter(|w| w.enabled).map_or(0, |w| w.lines);
    let need = lines0.max(sagv_lines) + 3;
    let carve = CURSOR_BLOCKS.max(need);
    if ddb.size() < MIN_PLANE_BLOCKS + carve {
        return Err(NoCursor::TooSmall);
    }
    let plane_ddb = Ddb { start: ddb.start, end: ddb.end - carve };
    let cursor_ddb = Ddb { start: plane_ddb.end, end: ddb.end };
    // A level is usable only while its blocks fit below the allocation;
    // once one doesn't, the higher ones are off too.
    let mut off = false;
    for (i, w) in wm.iter_mut().enumerate() {
        if !w.enabled || w.blocks >= plane_ddb.size() {
            if i == 0 {
                return Err(NoCursor::Level0);
            }
            off = true;
        }
        if off {
            *w = Wm::default();
        }
    }
    let cursor_wm0 = cursor_wm_from(Wm::decode(plane_wm[0]), carve).ok_or(NoCursor::Level0)?;
    let cursor_sagv = plane_sagv.map(Wm::decode).and_then(|w| cursor_wm_from(w, carve));
    Ok(CursorPlan { plane_ddb, plane_wm: wm, cursor_ddb, cursor_wm0, cursor_sagv })
}
