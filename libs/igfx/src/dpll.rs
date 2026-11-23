//! Skylake-class DPLLs (display 9 on big cores: Skylake, Kaby, Coffee and
//! Comet Lake).
//!
//! Four DPLLs. DPLL 0 also makes the CD clock and is left alone. For
//! DisplayPort a DPLL runs at one of six fixed link rates, chosen by a
//! code in `DPLL_CTRL1`. For HDMI it runs in "HDMI mode": a DCO with a
//! 24 MHz reference (`DPLL_CFGCR1`: integer and 15-bit fraction) and
//! three post dividers P0·P1·P2 (`DPLL_CFGCR2`: PDIV, QDIV, KDIV) bring
//! the DCO down to the AFE clock, five times the pixel clock (10 bits per
//! TMDS character, the DDI's clock at half the bit rate).
//!
//! The DCO must stay within +1 % / −6 % of one of three central
//! frequencies (8400, 9000, 9600 MHz). The total divider P is one of
//! the values the manual lists; even dividers are preferred, and among
//! them the one closest to a central frequency.

/// The `DPLL_CTRL1` link rate code for a DP link rate (symbol clock, kHz).
/// The codes name the DPLL's output in MHz, half the bit rate: 0 = 2700
/// (5.4 Gbit/s), 1 = 1350 (2.7), 2 = 810 (1.62), 3 = 1620 (3.24), 4 = 1080
/// (2.16), 5 = 2160 (4.32; the last three are eDP rates).
pub fn dp_rate_code(rate_khz: u32) -> Option<u32> {
    match rate_khz {
        162_000 => Some(2),
        270_000 => Some(1),
        540_000 => Some(0),
        216_000 => Some(4),
        324_000 => Some(3),
        432_000 => Some(5),
        _ => None,
    }
}

/// The DP link rate a `DPLL_CTRL1` code gives (the inverse of [`dp_rate_code`]).
pub fn dp_rate_from_code(code: u32) -> Option<u32> {
    match code {
        0 => Some(540_000),
        1 => Some(270_000),
        2 => Some(162_000),
        3 => Some(324_000),
        4 => Some(216_000),
        5 => Some(432_000),
        _ => None,
    }
}

const REF_KHZ: u64 = 24_000;
const CENTRAL_MHZ: [u64; 3] = [8400, 9000, 9600];
const EVEN: [u64; 36] = [
    4, 6, 8, 10, 12, 14, 16, 18, 20, 24, 28, 30, 32, 36, 40, 42, 44, 48, 52, 54, 56, 60, 64, 66, 68, 70, 72, 76, 78,
    80, 84, 88, 90, 92, 96, 98,
];
const ODD: [u64; 7] = [3, 5, 7, 9, 15, 21, 35];

/// An HDMI-mode DPLL setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WrPll {
    pub dco_khz: u64,
    pub central_mhz: u64,
    pub p0: u32,
    pub p1: u32,
    pub p2: u32,
    /// `DPLL_CFGCR1` and `DPLL_CFGCR2`.
    pub cfgcr1: u32,
    pub cfgcr2: u32,
}

/// Splits a total divider into (P0, P1, P2) as the manual's table does.
pub fn split(p: u64) -> Option<(u32, u32, u32)> {
    let p = p as u32;
    if p.is_multiple_of(2) {
        let half = p / 2;
        if matches!(half, 1 | 2 | 3 | 5) {
            Some((2, 1, half))
        } else if half.is_multiple_of(2) {
            Some((2, half / 2, 2))
        } else if half.is_multiple_of(3) {
            Some((3, half / 3, 2))
        } else if half.is_multiple_of(7) {
            Some((7, half / 7, 2))
        } else {
            None
        }
    } else {
        match p {
            3 | 9 => Some((3, 1, p / 3)),
            // The manual's table puts 5 in P0, which PDIV can't encode
            // (1, 2, 3, 7); KDIV can (only clocks above 340 MHz need it).
            5 => Some((1, 1, 5)),
            7 => Some((7, 1, 1)),
            15 => Some((3, 1, 5)),
            21 => Some((7, 1, 3)),
            35 => Some((7, 1, 5)),
            _ => None,
        }
    }
}

/// `CFGCR2` encodings: PDIV 4:2 (1, 2, 3, 7 → 0, 1, 2, 4), KDIV 6:5
/// (5, 2, 3, 1 → 0, 1, 2, 3), QDIV mode 7 and ratio 15:8, central
/// frequency 1:0 (9600, 9000, 8400 → 0, 1, 3).
fn cfgcr2(p0: u32, p1: u32, p2: u32, central: u64) -> u32 {
    let pdiv = match p0 {
        1 => 0,
        2 => 1,
        3 => 2,
        _ => 4,
    };
    let kdiv = match p2 {
        5 => 0,
        2 => 1,
        3 => 2,
        _ => 3,
    };
    let qdiv = if p1 == 1 { 0 } else { 1 << 7 | p1 << 8 };
    let cf = match central {
        9600 => 0,
        9000 => 1,
        _ => 3,
    };
    qdiv | kdiv << 5 | pdiv << 2 | cf
}

/// The HDMI-mode setting for a pixel clock in kHz.
pub fn hdmi(clock_khz: u32) -> Option<WrPll> {
    let afe = clock_khz as u64 * 5; // kHz
                                    // (deviation in 0.01 % units, P, central MHz) of the best even and odd choices.
    let mut best_even: Option<(u64, u64, u64)> = None;
    let mut best_odd: Option<(u64, u64, u64)> = None;
    for &central in &CENTRAL_MHZ {
        let c = central * 1000;
        for (set, best) in [(&EVEN[..], &mut best_even), (&ODD[..], &mut best_odd)] {
            for &p in set {
                let dco = afe * p;
                // +1 % / −6 % around the central frequency.
                let ok = if dco >= c { (dco - c) * 100 <= c } else { (c - dco) * 100 <= 6 * c };
                if !ok {
                    continue;
                }
                let dev = dco.abs_diff(c) * 10_000 / c;
                if best.is_none_or(|b| dev < b.0) {
                    *best = Some((dev, p, central));
                }
            }
        }
    }
    let (_, p, central) = best_even.or(best_odd)?;
    let (p0, p1, p2) = split(p)?;
    let dco = afe * p;
    let ratio = dco * 0x8000 / REF_KHZ;
    let (int, frac) = ((ratio >> 15) as u32, (ratio & 0x7FFF) as u32);
    Some(WrPll {
        dco_khz: dco,
        central_mhz: central,
        p0,
        p1,
        p2,
        cfgcr1: crate::regs::CFGCR1_FREQ_ENABLE | frac << 9 | int,
        cfgcr2: cfgcr2(p0, p1, p2, central),
    })
}

/// The pixel clock (kHz) an HDMI-mode `CFGCR1`/`CFGCR2` pair makes (for
/// reading the firmware's setting back).
pub fn hdmi_clock(cfgcr1: u32, cfgcr2: u32) -> Option<u32> {
    let int = (cfgcr1 & 0x1FF) as u64;
    let frac = (cfgcr1 >> 9 & 0x7FFF) as u64;
    let dco = (int * 0x8000 + frac) * REF_KHZ / 0x8000;
    let p0 = match cfgcr2 >> 2 & 7 {
        0 => 1,
        1 => 2,
        2 => 3,
        4 => 7,
        _ => return None,
    };
    let p2 = match cfgcr2 >> 5 & 3 {
        0 => 5,
        1 => 2,
        2 => 3,
        _ => 1,
    };
    let p1 = if cfgcr2 & 1 << 7 != 0 { (cfgcr2 >> 8 & 0xFF) as u64 } else { 1 };
    let p = p0 * p1.max(1) * p2;
    Some((dco / p / 5) as u32)
}
