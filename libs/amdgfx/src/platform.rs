//! Which AMD GPU this is: PCI device id → ASIC → display block.
//!
//! Covered (display block version in AMD's numbering):
//!
//! | Block | ASICs | Products |
//! |---|---|---|
//! | DCE 11.2 | Polaris 10/20/30, 11/21, 12 | RX 470–590, RX 460/560, RX 540/550, WX 2100–7100 |
//! | DCE 12.0/12.1 | Vega 10, 12, 20 | RX Vega 56/64, Radeon VII, Pro Vega |
//! | DCN 1.0 | Raven, Picasso (Raven 2) | Ryzen 2000/3000 APUs (Vega 3–11) |
//! | DCN 2.0 | Navi 10, 12, 14 | RX 5500–5700 XT |
//! | DCN 2.1 | Renoir, Lucienne, Cezanne, Barcelo | Ryzen 4000/5000 APUs |
//! | DCN 3.0 | Navi 21, 22 | RX 6700–6950 XT |
//! | DCN 3.0.1 | Van Gogh | Steam Deck |
//! | DCN 3.0.2 | Navi 23 | RX 6600/6650 XT |
//! | DCN 3.0.3 | Navi 24 | RX 6400/6500 XT |
//! | DCN 3.1.2 | Rembrandt | Ryzen 6000 APUs (680M) |
//! | DCN 3.1.4 | Phoenix, Hawk Point | Ryzen 7040/8040 APUs (780M) |
//! | DCN 3.1.5 | Raphael, Granite Ridge | Ryzen 7000/9000 desktop iGPU |
//! | DCN 3.1.6 | Mendocino | Ryzen 7020 APUs |
//! | DCN 3.2 | Navi 31, 32 | RX 7700 XT–7900 XTX |
//! | DCN 3.2.1 | Navi 33 | RX 7600 |
//!
//! Not covered, and left to the firmware framebuffer with a log line: GCN
//! 1–3 (DCE 6–10: Southern Islands to Fiji), Carrizo/Stoney (DCE 11.0),
//! Kaby Lake-G's Vega M, Strix Point and newer APUs (DCN 3.5+), and RDNA 4
//! (Navi 4x, DCN 4.0.1).

use crate::regs::{Dce, Dcn};

pub const VENDOR_AMD: u16 = 0x1002;
pub const CLASS_DISPLAY: u8 = 0x03;

/// A DCN version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcnVer {
    V10,
    V20,
    V21,
    V30,
    V301,
    V302,
    V303,
    V312,
    V314,
    V315,
    V316,
    V32,
    V321,
}

/// The display block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Dce112,
    Dce120,
    Dcn(DcnVer),
}

/// Register layout of a display block.
#[derive(Clone, Copy)]
pub enum Layout {
    Dce(&'static Dce),
    Dcn(&'static Dcn),
}

#[derive(Clone, Copy, Debug)]
pub struct Info {
    pub device: u16,
    /// The ASIC's code name.
    pub asic: &'static str,
    pub block: Block,
    /// An APU: "VRAM" is a carve-out of system memory.
    pub apu: bool,
}

impl Block {
    pub fn name(self) -> &'static str {
        match self {
            Block::Dce112 => "DCE 11.2",
            Block::Dce120 => "DCE 12",
            Block::Dcn(v) => match v {
                DcnVer::V10 => "DCN 1.0",
                DcnVer::V20 => "DCN 2.0",
                DcnVer::V21 => "DCN 2.1",
                DcnVer::V30 => "DCN 3.0",
                DcnVer::V301 => "DCN 3.0.1",
                DcnVer::V302 => "DCN 3.0.2",
                DcnVer::V303 => "DCN 3.0.3",
                DcnVer::V312 => "DCN 3.1.2",
                DcnVer::V314 => "DCN 3.1.4",
                DcnVer::V315 => "DCN 3.1.5",
                DcnVer::V316 => "DCN 3.1.6",
                DcnVer::V32 => "DCN 3.2",
                DcnVer::V321 => "DCN 3.2.1",
            },
        }
    }

    pub fn layout(self) -> Layout {
        use crate::layout::*;
        match self {
            Block::Dce112 => Layout::Dce(&DCE112),
            Block::Dce120 => Layout::Dce(&DCE120),
            Block::Dcn(v) => Layout::Dcn(match v {
                DcnVer::V10 => &DCN10,
                DcnVer::V20 => &DCN20,
                DcnVer::V21 => &DCN21,
                DcnVer::V30 => &DCN30,
                DcnVer::V301 => &DCN301,
                DcnVer::V302 => &DCN302,
                DcnVer::V303 => &DCN303,
                DcnVer::V312 => &DCN31,
                DcnVer::V314 => &DCN314,
                DcnVer::V315 => &DCN315,
                DcnVer::V316 => &DCN316,
                DcnVer::V32 => &DCN32,
                DcnVer::V321 => &DCN321,
            }),
        }
    }

    /// The register segment bases these offsets assume (0x12, 0xC0,
    /// 0x34C0, 0x9000, 0x02403C00) are printed in the ip_offset headers of
    /// Vega 10/20, Navi 1x, Renoir, Van Gogh, Navi 2x and Yellow Carp. The
    /// other ASICs get theirs from the IP discovery table, which this
    /// driver does not read: UNCONFIRMED for Raven, Phoenix, Raphael,
    /// Mendocino and Navi 3x (the takeover's readback declines if they
    /// differ, since nothing would match the firmware framebuffer).
    pub fn bases_confirmed(self) -> bool {
        !matches!(
            self,
            Block::Dcn(DcnVer::V10 | DcnVer::V314 | DcnVer::V315 | DcnVer::V316 | DcnVer::V32 | DcnVer::V321)
        )
    }
}

const fn info(device: u16, asic: &'static str, block: Block, apu: bool) -> Info {
    Info { device, asic, block, apu }
}

/// Single device ids (APUs and the Polaris/Vega families, as the PCI id
/// database lists them).
static IDS: &[(u16, &str, Block, bool)] = &[
    // Polaris 10/20/30 (Ellesmere).
    (0x67C0, "Polaris 10", Block::Dce112, false),
    (0x67C2, "Polaris 10", Block::Dce112, false),
    (0x67C4, "Polaris 10", Block::Dce112, false),
    (0x67C7, "Polaris 10", Block::Dce112, false),
    (0x67CA, "Polaris 10", Block::Dce112, false),
    (0x67CC, "Polaris 10", Block::Dce112, false),
    (0x67CF, "Polaris 10", Block::Dce112, false),
    (0x67D0, "Polaris 10", Block::Dce112, false),
    (0x67D4, "Polaris 10", Block::Dce112, false),
    (0x67D7, "Polaris 10", Block::Dce112, false),
    (0x67DF, "Polaris 10", Block::Dce112, false),
    (0x6FDF, "Polaris 20", Block::Dce112, false),
    // Polaris 11/21 (Baffin).
    (0x67E0, "Polaris 11", Block::Dce112, false),
    (0x67E1, "Polaris 11", Block::Dce112, false),
    (0x67E3, "Polaris 11", Block::Dce112, false),
    (0x67E8, "Polaris 11", Block::Dce112, false),
    (0x67E9, "Polaris 11", Block::Dce112, false),
    (0x67EB, "Polaris 11", Block::Dce112, false),
    (0x67EF, "Polaris 11", Block::Dce112, false),
    (0x67FF, "Polaris 11", Block::Dce112, false),
    // Polaris 12 (Lexa).
    (0x6980, "Polaris 12", Block::Dce112, false),
    (0x6981, "Polaris 12", Block::Dce112, false),
    (0x6985, "Polaris 12", Block::Dce112, false),
    (0x6986, "Polaris 12", Block::Dce112, false),
    (0x6987, "Polaris 12", Block::Dce112, false),
    (0x698F, "Polaris 12", Block::Dce112, false),
    (0x6995, "Polaris 12", Block::Dce112, false),
    (0x699F, "Polaris 12", Block::Dce112, false),
    // Vega 10, 12, 20.
    (0x6860, "Vega 10", Block::Dce120, false),
    (0x6861, "Vega 10", Block::Dce120, false),
    (0x6862, "Vega 10", Block::Dce120, false),
    (0x6863, "Vega 10", Block::Dce120, false),
    (0x6864, "Vega 10", Block::Dce120, false),
    (0x6867, "Vega 10", Block::Dce120, false),
    (0x6868, "Vega 10", Block::Dce120, false),
    (0x6869, "Vega 10", Block::Dce120, false),
    (0x686A, "Vega 10", Block::Dce120, false),
    (0x686B, "Vega 10", Block::Dce120, false),
    (0x686C, "Vega 10", Block::Dce120, false),
    (0x686D, "Vega 10", Block::Dce120, false),
    (0x686E, "Vega 10", Block::Dce120, false),
    (0x687F, "Vega 10", Block::Dce120, false),
    (0x69A0, "Vega 12", Block::Dce120, false),
    (0x69A1, "Vega 12", Block::Dce120, false),
    (0x69A2, "Vega 12", Block::Dce120, false),
    (0x69A3, "Vega 12", Block::Dce120, false),
    (0x69AF, "Vega 12", Block::Dce120, false),
    (0x66A0, "Vega 20", Block::Dce120, false),
    (0x66A1, "Vega 20", Block::Dce120, false),
    (0x66A2, "Vega 20", Block::Dce120, false),
    (0x66A3, "Vega 20", Block::Dce120, false),
    (0x66A7, "Vega 20", Block::Dce120, false),
    (0x66AF, "Vega 20", Block::Dce120, false),
    // APUs.
    (0x15DD, "Raven", Block::Dcn(DcnVer::V10), true),
    (0x15D8, "Picasso", Block::Dcn(DcnVer::V10), true),
    (0x1636, "Renoir", Block::Dcn(DcnVer::V21), true),
    (0x164C, "Lucienne", Block::Dcn(DcnVer::V21), true),
    (0x1638, "Cezanne", Block::Dcn(DcnVer::V21), true),
    (0x15E7, "Barcelo", Block::Dcn(DcnVer::V21), true),
    (0x163F, "Van Gogh", Block::Dcn(DcnVer::V301), true),
    (0x164D, "Rembrandt", Block::Dcn(DcnVer::V312), true),
    (0x1681, "Rembrandt", Block::Dcn(DcnVer::V312), true),
    (0x15BF, "Phoenix", Block::Dcn(DcnVer::V314), true),
    (0x15C8, "Phoenix 2", Block::Dcn(DcnVer::V314), true),
    (0x164F, "Phoenix", Block::Dcn(DcnVer::V314), true),
    (0x1900, "Hawk Point", Block::Dcn(DcnVer::V314), true),
    (0x1901, "Hawk Point 2", Block::Dcn(DcnVer::V314), true),
    (0x164E, "Raphael", Block::Dcn(DcnVer::V315), true),
    (0x13C0, "Granite Ridge", Block::Dcn(DcnVer::V315), true),
    (0x1506, "Mendocino", Block::Dcn(DcnVer::V316), true),
    // Navi 1x.
    (0x7310, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x7312, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x7318, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x7319, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x731A, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x731B, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x731E, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x731F, "Navi 10", Block::Dcn(DcnVer::V20), false),
    (0x7340, "Navi 14", Block::Dcn(DcnVer::V20), false),
    (0x7341, "Navi 14", Block::Dcn(DcnVer::V20), false),
    (0x7347, "Navi 14", Block::Dcn(DcnVer::V20), false),
    (0x734F, "Navi 14", Block::Dcn(DcnVer::V20), false),
    (0x7360, "Navi 12", Block::Dcn(DcnVer::V20), false),
    (0x7362, "Navi 12", Block::Dcn(DcnVer::V20), false),
    // A Navi 33 inside Navi 23's range.
    (0x73F0, "Navi 33", Block::Dcn(DcnVer::V321), false),
];

/// Navi 2x and 3x: AMD gives each ASIC a block of 32 ids (the display
/// function is told apart by its PCI class).
static RANGES: &[(u16, u16, &str, Block)] = &[
    (0x73A0, 0x73BF, "Navi 21", Block::Dcn(DcnVer::V30)),
    (0x73C0, 0x73DF, "Navi 22", Block::Dcn(DcnVer::V30)),
    (0x73E0, 0x73FF, "Navi 23", Block::Dcn(DcnVer::V302)),
    (0x7420, 0x743F, "Navi 24", Block::Dcn(DcnVer::V303)),
    (0x7440, 0x745F, "Navi 31", Block::Dcn(DcnVer::V32)),
    (0x7460, 0x747F, "Navi 32", Block::Dcn(DcnVer::V32)),
    (0x7480, 0x749F, "Navi 33", Block::Dcn(DcnVer::V321)),
];

pub fn lookup(device: u16) -> Option<Info> {
    if let Some(&(d, asic, block, apu)) = IDS.iter().find(|e| e.0 == device) {
        return Some(info(d, asic, block, apu));
    }
    RANGES.iter().find(|r| (r.0..=r.1).contains(&device)).map(|&(_, _, asic, block)| info(device, asic, block, false))
}

/// Why an AMD display device isn't driven (for the log).
pub fn unsupported_reason(device: u16) -> &'static str {
    match device {
        0x9870..=0x9877 | 0x98E4 => "Carrizo/Stoney (DCE 11.0)",
        0x6600..=0x666F | 0x6780..=0x67BF | 0x6900..=0x693F | 0x7300 | 0x9830..=0x98FF => "an older GCN GPU (DCE 6–10)",
        0x694C..=0x694F => "Vega M (Kaby Lake-G)",
        0x150E | 0x1586 | 0x1114 => "Strix and newer APUs (DCN 3.5+)",
        0x7550..=0x75FF => "RDNA 4 (DCN 4)",
        _ => "not in the device table",
    }
}
