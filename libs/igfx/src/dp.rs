//! DisplayPort: DPCD registers, choosing a link (rate and lanes), the M/N
//! values that tell the transcoder how pixels fit the link, and link
//! training (DisplayPort 1.4, §3.5.1.2: clock recovery, then channel
//! equalization).
//!
//! Link rates are given as the link symbol clock in kHz, as DRM does:
//! 162000 (RBR, 1.62 Gbit/s per lane), 270000 (HBR), 540000 (HBR2),
//! 810000 (HBR3). With 8b/10b coding one lane carries one byte per symbol
//! clock.

use alloc::vec::Vec;

pub mod dpcd {
    pub const REV: u32 = 0x000;
    pub const MAX_LINK_RATE: u32 = 0x001;
    /// 4:0 lanes; bit 7 enhanced framing; bit 6 TPS3 supported.
    pub const MAX_LANE_COUNT: u32 = 0x002;
    pub const ENHANCED_FRAME_CAP: u8 = 1 << 7;
    pub const TPS3_SUPPORTED: u8 = 1 << 6;
    pub const DOWNSTREAMPORT_PRESENT: u32 = 0x005;
    pub const EDP_CONFIGURATION_CAP: u32 = 0x00D;
    /// 6:0: 0 = 100 µs (400 µs for channel EQ), n = n × 4 ms.
    pub const TRAINING_AUX_RD_INTERVAL: u32 = 0x00E;
    /// eDP 1.4: up to eight supported rates, little-endian u16 in 200 kHz units.
    pub const SUPPORTED_LINK_RATES: u32 = 0x010;
    pub const LINK_BW_SET: u32 = 0x100;
    /// 4:0 lanes; bit 7 enhanced framing.
    pub const LANE_COUNT_SET: u32 = 0x101;
    pub const TRAINING_PATTERN_SET: u32 = 0x102;
    pub const TRAINING_LANE0_SET: u32 = 0x103;
    /// eDP 1.4: index into `SUPPORTED_LINK_RATES` (instead of `LINK_BW_SET`).
    pub const LINK_RATE_SET: u32 = 0x115;
    pub const SINK_COUNT: u32 = 0x200;
    pub const LANE0_1_STATUS: u32 = 0x202;
    pub const LANE_ALIGN_STATUS_UPDATED: u32 = 0x204;
    pub const ADJUST_REQUEST_LANE0_1: u32 = 0x206;
    /// 1 = D0 (normal), 2 = D3 (power down).
    pub const SET_POWER: u32 = 0x600;
    pub const EDP_DPCD_REV: u32 = 0x700;
    /// Training pattern 1, 2, 3 values for `TRAINING_PATTERN_SET`.
    pub const TP1: u8 = 1;
    pub const TP2: u8 = 2;
    pub const TP3: u8 = 3;
    /// Bit 5 of `TRAINING_PATTERN_SET`: scrambling off (during training).
    pub const SCRAMBLING_DISABLE: u8 = 1 << 5;
    pub const CR_DONE: u8 = 1 << 0;
    pub const CHANNEL_EQ_DONE: u8 = 1 << 1;
    pub const SYMBOL_LOCKED: u8 = 1 << 2;
    pub const INTERLANE_ALIGN_DONE: u8 = 1 << 0;
    pub const MAX_SWING_REACHED: u8 = 1 << 2;
    pub const MAX_PRE_EMPHASIS_REACHED: u8 = 1 << 5;
}

/// The standard rates in `MAX_LINK_RATE` units (0.27 Gbit/s): 0x06, 0x0A, 0x14, 0x1E.
pub fn rate_from_bw_code(code: u8) -> Option<u32> {
    match code {
        0x06 => Some(162_000),
        0x0A => Some(270_000),
        0x14 => Some(540_000),
        0x1E => Some(810_000),
        _ => None,
    }
}

pub fn bw_code(rate: u32) -> u8 {
    (rate / 27_000) as u8
}

/// What a sink can do, from its DPCD receiver capabilities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SinkCaps {
    pub rev: u8,
    /// Link rates (kHz symbol clock), ascending.
    pub rates: Vec<u32>,
    pub max_lanes: u8,
    pub enhanced_framing: bool,
    pub tps3: bool,
    /// eDP 1.4 rate table in use: write `LINK_RATE_SET` with the index.
    pub rate_table: bool,
    /// Clock recovery and channel equalization read intervals, µs.
    pub cr_interval_us: u32,
    pub eq_interval_us: u32,
}

impl SinkCaps {
    /// Parses the first 16 DPCD bytes, and the eDP rate table when there is one.
    pub fn parse(caps: &[u8; 16], edp_rates: Option<&[u8; 16]>) -> Option<SinkCaps> {
        if caps[0] == 0 {
            return None;
        }
        let mut rates: Vec<u32> = Vec::new();
        let mut rate_table = false;
        if let Some(t) = edp_rates {
            for pair in t.chunks(2) {
                let v = u16::from_le_bytes([pair[0], pair[1]]) as u32;
                if v == 0 {
                    break;
                }
                // 200 kHz units of the bit rate per lane: ÷ 10 for the symbol clock, in kHz.
                rates.push(v * 200 / 10);
                rate_table = true;
            }
        }
        if rates.is_empty() {
            let max = rate_from_bw_code(caps[1])?;
            rates.extend([162_000, 270_000, 540_000, 810_000].iter().filter(|&&r| r <= max));
        }
        rates.sort_unstable();
        rates.dedup();
        let lanes = caps[2] & 0x1F;
        if !matches!(lanes, 1 | 2 | 4) {
            return None;
        }
        let interval = caps[0x0E] & 0x7F;
        let (cr, eq) = if interval == 0 { (100, 400) } else { (interval as u32 * 4000, interval as u32 * 4000) };
        Some(SinkCaps {
            rev: caps[0],
            rates,
            max_lanes: lanes,
            enhanced_framing: caps[2] & dpcd::ENHANCED_FRAME_CAP != 0,
            tps3: caps[2] & dpcd::TPS3_SUPPORTED != 0,
            rate_table,
            cr_interval_us: cr,
            eq_interval_us: eq,
        })
    }
}

/// A link configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Link {
    pub rate: u32,
    pub lanes: u8,
}

/// The link a mode of `clock_khz` at `bpp` bits per pixel needs: all the
/// lanes both ends have, at the lowest rate both support that carries the
/// stream (lower rates train more reliably over the same lanes).
pub fn choose_link(clock_khz: u32, bpp: u32, source_rates: &[u32], source_lanes: u8, sink: &SinkCaps) -> Option<Link> {
    let lanes = source_lanes.min(sink.max_lanes);
    let need = clock_khz as u64 * bpp as u64; // kbit/s
    let mut rates: Vec<u32> = sink.rates.iter().copied().filter(|r| source_rates.contains(r)).collect();
    rates.sort_unstable();
    rates.into_iter().find(|&r| r as u64 * 8 * lanes as u64 >= need).map(|rate| Link { rate, lanes })
}

/// Data and link M/N values (`PIPE_DATA_M1/N1`, `PIPE_LINK_M1/N1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MN {
    pub data_m: u32,
    pub data_n: u32,
    pub link_m: u32,
    pub link_n: u32,
}

/// N is fixed at 0x8000 (what most sinks expect); M = ratio × N rounded
/// down. Data ratio: pixel bytes per link byte, `clock·bpp / (8·lanes·rate)`;
/// link ratio: `clock / rate`.
pub fn m_n(clock_khz: u32, bpp: u32, link: Link) -> MN {
    const N: u64 = 0x8000;
    let data = clock_khz as u64 * bpp as u64 * N / (8 * link.lanes as u64 * link.rate as u64);
    let lnk = clock_khz as u64 * N / link.rate as u64;
    MN { data_m: data as u32, data_n: N as u32, link_m: lnk as u32, link_n: N as u32 }
}

// --------------------------------------------------------- link training

/// Voltage swing and pre-emphasis levels (0–3 each, at most 3 together).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Drive {
    pub swing: u8,
    pub pre: u8,
}

impl Drive {
    /// `TRAINING_LANEx_SET` for this drive (with the "max reached" flags).
    pub fn lane_set(&self) -> u8 {
        let mut v = self.swing | self.pre << 3;
        if self.swing == 3 || self.swing + self.pre >= 3 {
            v |= dpcd::MAX_SWING_REACHED;
        }
        if self.pre == 3 || self.swing + self.pre >= 3 {
            v |= dpcd::MAX_PRE_EMPHASIS_REACHED;
        }
        v
    }

    /// Clamped to what a source can drive.
    pub fn clamp(self) -> Drive {
        let swing = self.swing.min(3);
        let pre = self.pre.min(3 - swing);
        Drive { swing, pre }
    }

    /// Skylake's `DDI_BUF_TRANS` entry for this drive: entries 0–9 are
    /// (swing, pre-emphasis) (0,0) (0,1) (0,2) (0,3) (1,0) (1,1) (1,2)
    /// (2,0) (2,1) (3,0).
    pub fn skl_entry(&self) -> u32 {
        let d = self.clamp();
        match (d.swing, d.pre) {
            (0, p) => p as u32,
            (1, p) => 4 + p as u32,
            (2, p) => 7 + p as u32,
            _ => 9,
        }
    }
}

/// The two ends of a link being trained: the source's pattern and drive
/// (and the sink's, written over AUX), and the sink's status.
pub trait Trainer {
    type Error;
    /// Sends training pattern `tp` (1–3), or 0 for none, at both ends.
    fn set_pattern(&mut self, tp: u8) -> Result<(), Self::Error>;
    /// Drives all lanes at `d` and tells the sink.
    fn set_drive(&mut self, d: Drive, lanes: u8) -> Result<(), Self::Error>;
    /// `LANE0_1_STATUS` … `ADJUST_REQUEST_LANE2_3` (DPCD 0x202–0x207).
    fn status(&mut self) -> Result<[u8; 6], Self::Error>;
    fn delay_us(&mut self, us: u32);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrainError<E> {
    Io(E),
    ClockRecovery,
    ChannelEq,
}

fn lane_bits(status: &[u8; 6], lane: u8) -> u8 {
    status[lane as usize / 2] >> (4 * (lane % 2)) & 0xF
}

fn all_lanes(status: &[u8; 6], lanes: u8, bits: u8) -> bool {
    (0..lanes).all(|l| lane_bits(status, l) & bits == bits)
}

/// The highest drive any lane asks for (one drive for all lanes keeps the
/// source side to one buffer translation entry).
fn requested(status: &[u8; 6], lanes: u8) -> Drive {
    let mut d = Drive::default();
    for l in 0..lanes {
        let adj = status[4 + l as usize / 2] >> (4 * (l % 2)) & 0xF;
        d.swing = d.swing.max(adj & 3);
        d.pre = d.pre.max(adj >> 2 & 3);
    }
    d.clamp()
}

/// Trains `lanes` lanes. Returns the drive the link settled at.
pub fn train<T: Trainer>(
    t: &mut T,
    lanes: u8,
    tps3: bool,
    cr_us: u32,
    eq_us: u32,
) -> Result<Drive, TrainError<T::Error>> {
    // Clock recovery: pattern 1, raise the drive as the sink asks; give up
    // after five tries at one swing or when the maximum swing fails.
    let mut d = Drive::default();
    t.set_drive(d, lanes).map_err(TrainError::Io)?;
    t.set_pattern(dpcd::TP1).map_err(TrainError::Io)?;
    let mut same = 0;
    let mut tries = 0;
    loop {
        t.delay_us(cr_us);
        let s = t.status().map_err(TrainError::Io)?;
        if all_lanes(&s, lanes, dpcd::CR_DONE) {
            break;
        }
        tries += 1;
        let want = requested(&s, lanes);
        if want.swing == d.swing {
            same += 1;
        } else {
            same = 0;
        }
        if same >= 5 || (d.swing == 3 && want.swing == 3) || tries > 20 {
            let _ = t.set_pattern(0);
            return Err(TrainError::ClockRecovery);
        }
        d = want;
        t.set_drive(d, lanes).map_err(TrainError::Io)?;
    }
    // Channel equalization: pattern 2 (3 if the sink takes it), up to five tries.
    t.set_pattern(if tps3 { dpcd::TP3 } else { dpcd::TP2 }).map_err(TrainError::Io)?;
    for _ in 0..5 {
        t.delay_us(eq_us);
        let s = t.status().map_err(TrainError::Io)?;
        if !all_lanes(&s, lanes, dpcd::CR_DONE) {
            break;
        }
        let eq = dpcd::CHANNEL_EQ_DONE | dpcd::SYMBOL_LOCKED;
        if all_lanes(&s, lanes, eq) && s[2] & dpcd::INTERLANE_ALIGN_DONE != 0 {
            t.set_pattern(0).map_err(TrainError::Io)?;
            return Ok(d);
        }
        d = requested(&s, lanes);
        t.set_drive(d, lanes).map_err(TrainError::Io)?;
    }
    let _ = t.set_pattern(0);
    Err(TrainError::ChannelEq)
}
