//! The cards' registers: CID, CSD, SCR, the SD switch-function status and
//! the eMMC `EXT_CSD`, decoded as the SD Physical Layer Simplified
//! Specification (3.01 and later) and JEDEC JESD84-B51 define them.

use alloc::string::String;

/// Card status (R1) bits.
pub mod status {
    pub const OUT_OF_RANGE: u32 = 1 << 31;
    pub const ADDRESS_ERROR: u32 = 1 << 30;
    pub const BLOCK_LEN_ERROR: u32 = 1 << 29;
    pub const ERASE_SEQ_ERROR: u32 = 1 << 28;
    pub const ERASE_PARAM: u32 = 1 << 27;
    pub const WP_VIOLATION: u32 = 1 << 26;
    pub const LOCK_UNLOCK_FAILED: u32 = 1 << 24;
    pub const COM_CRC_ERROR: u32 = 1 << 23;
    pub const ILLEGAL_COMMAND: u32 = 1 << 22;
    pub const CARD_ECC_FAILED: u32 = 1 << 21;
    pub const CC_ERROR: u32 = 1 << 20;
    pub const ERROR: u32 = 1 << 19;
    pub const READY_FOR_DATA: u32 = 1 << 8;
    /// eMMC: the last SWITCH was refused.
    pub const SWITCH_ERROR: u32 = 1 << 7;
    pub const APP_CMD: u32 = 1 << 5;
    /// Errors a read or write reports about itself.
    pub const DATA_ERRORS: u32 =
        OUT_OF_RANGE | ADDRESS_ERROR | BLOCK_LEN_ERROR | WP_VIOLATION | CARD_ECC_FAILED | CC_ERROR | ERROR;
    pub const ERASE_ERRORS: u32 = DATA_ERRORS | ERASE_SEQ_ERROR | ERASE_PARAM;

    /// The card's state (bits 12:9).
    pub fn state(r1: u32) -> u32 {
        (r1 >> 9) & 0xF
    }
    pub const STATE_STBY: u32 = 3;
    pub const STATE_TRAN: u32 = 4;
    pub const STATE_DATA: u32 = 5;
    pub const STATE_RCV: u32 = 6;
    pub const STATE_PRG: u32 = 7;
}

/// The 128-bit register a 136-bit response carries, as 16 bytes, most
/// significant first (bit 127 is the top bit of byte 0). The controller
/// strips the CRC, so byte 15 (CRC and end bit) reads 0.
pub fn r2_bytes(resp: [u32; 4]) -> [u8; 16] {
    let v = (resp[3] as u128) << 96 | (resp[2] as u128) << 64 | (resp[1] as u128) << 32 | resp[0] as u128;
    (v << 8).to_be_bytes()
}

/// Bits `hi..=lo` of a big-endian register (bit 0 is the last byte's LSB).
pub fn bits(reg: &[u8], hi: usize, lo: usize) -> u64 {
    let total = reg.len() * 8;
    let mut v = 0u64;
    for b in (lo..=hi).rev() {
        let byte = reg[(total - 1 - b) / 8];
        let bit = (byte >> (b % 8)) & 1;
        v = v << 1 | bit as u64;
    }
    v
}

/// Card identification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cid {
    pub manufacturer: u8,
    pub name: String,
    pub revision: u8,
    pub serial: u32,
    pub year: u16,
    pub month: u8,
}

fn text(b: &[u8]) -> String {
    b.iter().map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '?' }).collect::<String>().trim().into()
}

impl Cid {
    pub fn parse_sd(r: &[u8; 16]) -> Cid {
        let mdt = bits(r, 19, 8) as u16;
        Cid {
            manufacturer: r[0],
            name: text(&r[3..8]),
            revision: r[8],
            serial: u32::from_be_bytes([r[9], r[10], r[11], r[12]]),
            year: 2000 + (mdt >> 4),
            month: (mdt & 0xF) as u8,
        }
    }
    pub fn parse_mmc(r: &[u8; 16]) -> Cid {
        let mdt = r[14];
        Cid {
            manufacturer: r[0],
            name: text(&r[3..9]),
            revision: r[9],
            serial: u32::from_be_bytes([r[10], r[11], r[12], r[13]]),
            // Years from 1997 (EXT_CSD_REV 4 and later reinterpret the
            // first 13 from 2013; the date is informational only).
            year: 1997 + (mdt & 0xF) as u16,
            month: mdt >> 4,
        }
    }
}

/// What the CSD says that matters here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Csd {
    pub structure: u8,
    /// MMC: the specification version (4 and up have `EXT_CSD`).
    pub spec_vers: u8,
    /// Capacity in bytes (MMC above 2 GiB: 0, see `EXT_CSD`).
    pub capacity: u64,
    pub read_bl_len: u8,
    pub write_protected: bool,
    /// Command classes (SD; bit 10 is "switch", for CMD6).
    pub ccc: u16,
    /// MMC: erase group size in 512-byte sectors (from ERASE_GRP_SIZE/MULT).
    pub erase_group: u32,
}

impl Csd {
    fn classic_capacity(r: &[u8; 16]) -> (u64, u8) {
        let c_size = bits(r, 73, 62);
        let mult = bits(r, 49, 47);
        let bl_len = bits(r, 83, 80) as u8;
        ((c_size + 1) << (mult + 2) << bl_len, bl_len)
    }

    pub fn parse_sd(r: &[u8; 16]) -> Csd {
        let structure = bits(r, 127, 126) as u8;
        let (capacity, read_bl_len) = match structure {
            0 => Csd::classic_capacity(r),
            1 => ((bits(r, 69, 48) + 1) * 512 * 1024, 9),
            _ => ((bits(r, 75, 48) + 1) * 512 * 1024, 9),
        };
        Csd {
            structure,
            spec_vers: 0,
            capacity,
            read_bl_len,
            write_protected: bits(r, 13, 12) != 0,
            ccc: bits(r, 95, 84) as u16,
            erase_group: 1,
        }
    }

    pub fn parse_mmc(r: &[u8; 16]) -> Csd {
        let (capacity, read_bl_len) = if bits(r, 73, 62) == 0xFFF { (0, 9) } else { Csd::classic_capacity(r) };
        Csd {
            structure: bits(r, 127, 126) as u8,
            spec_vers: bits(r, 125, 122) as u8,
            capacity,
            read_bl_len,
            write_protected: bits(r, 13, 12) != 0,
            ccc: bits(r, 95, 84) as u16,
            erase_group: ((bits(r, 46, 42) + 1) * (bits(r, 41, 37) + 1)) as u32,
        }
    }
}

/// The SD Configuration Register.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scr {
    /// SD_SPEC: 0 is 1.0, 1 is 1.10, 2 is 2.00 (3.0x with SD_SPEC3).
    pub spec: u8,
    pub spec3: bool,
    pub bus4: bool,
    pub cmd23: bool,
    /// Erased data reads as 0xFF (else 0x00).
    pub erased_ones: bool,
}

impl Scr {
    pub fn parse(r: &[u8; 8]) -> Scr {
        Scr {
            spec: r[0] & 0xF,
            spec3: r[2] & 0x80 != 0,
            bus4: r[1] & 0x4 != 0,
            cmd23: r[3] & 0x2 != 0,
            erased_ones: r[1] & 0x80 != 0,
        }
    }
}

/// The 64-byte status CMD6 returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwitchStatus {
    /// Supported functions per group (index 0 is group 1), one bit each.
    pub support: [u16; 6],
    /// The function each group has (or would have) selected; 0xF: error.
    pub selected: [u8; 6],
}

impl SwitchStatus {
    pub fn parse(r: &[u8]) -> SwitchStatus {
        let mut support = [0u16; 6];
        for (g, s) in support.iter_mut().enumerate() {
            // Group 6 comes first (bytes 2-3), group 1 last (bytes 12-13).
            let at = 2 + (5 - g) * 2;
            *s = u16::from_be_bytes([r[at], r[at + 1]]);
        }
        let mut selected = [0u8; 6];
        for (g, s) in selected.iter_mut().enumerate() {
            let byte = r[16 - g / 2];
            *s = if g % 2 == 0 { byte & 0xF } else { byte >> 4 };
        }
        SwitchStatus { support, selected }
    }
}

/// CMD6 (SD) argument: query (`set` false) or switch group `group` (1-6) to `function`.
pub fn sd_switch_arg(set: bool, group: u32, function: u32) -> u32 {
    let shift = 4 * (group - 1);
    (set as u32) << 31 | (0x00FF_FFFF & !(0xF << shift)) | function << shift
}

/// `EXT_CSD` byte offsets.
pub mod ext {
    pub const FLUSH_CACHE: usize = 32;
    pub const CACHE_CTRL: usize = 33;
    pub const RPMB_SIZE_MULT: usize = 168;
    pub const ERASE_GROUP_DEF: usize = 175;
    pub const PARTITION_CONFIG: usize = 179;
    pub const ERASED_MEM_CONT: usize = 181;
    pub const BUS_WIDTH: usize = 183;
    pub const HS_TIMING: usize = 185;
    pub const REV: usize = 192;
    pub const DEVICE_TYPE: usize = 196;
    pub const SEC_COUNT: usize = 212;
    pub const HC_ERASE_GRP_SIZE: usize = 224;
    pub const BOOT_SIZE_MULT: usize = 226;
    pub const SEC_FEATURE_SUPPORT: usize = 231;
    pub const GENERIC_CMD6_TIME: usize = 248;
    pub const CACHE_SIZE: usize = 249;
    pub const PARTITION_SWITCH_TIME: usize = 199;
    pub const ERASE_TIMEOUT_MULT: usize = 223;

    /// DEVICE_TYPE bits.
    pub const HS26: u8 = 1 << 0;
    pub const HS52: u8 = 1 << 1;
    pub const HS200_18: u8 = 1 << 4;
    pub const HS400_18: u8 = 1 << 6;
    /// BUS_WIDTH values.
    pub const WIDTH_1: u8 = 0;
    pub const WIDTH_4: u8 = 1;
    pub const WIDTH_8: u8 = 2;
    pub const WIDTH_8_DDR: u8 = 6;
    /// HS_TIMING values.
    pub const TIMING_LEGACY: u8 = 0;
    pub const TIMING_HS: u8 = 1;
    pub const TIMING_HS200: u8 = 2;
    pub const TIMING_HS400: u8 = 3;
    /// SEC_FEATURE_SUPPORT: TRIM and garbage-collect erases.
    pub const SEC_GB_CL_EN: u8 = 1 << 4;
}

/// The parts of `EXT_CSD` this driver uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtCsd {
    pub rev: u8,
    pub sectors: u64,
    pub device_type: u8,
    pub partition_config: u8,
    pub boot_bytes: u64,
    pub rpmb_bytes: u64,
    pub hc_erase_group_sectors: u32,
    pub trim: bool,
    pub cache_bytes: u64,
    /// CMD6 busy timeout in µs.
    pub cmd6_us: u64,
    pub partition_switch_us: u64,
    pub erase_timeout_us: u64,
    pub erased_ones: bool,
}

impl ExtCsd {
    pub fn parse(e: &[u8]) -> ExtCsd {
        let le32 = |o: usize| u32::from_le_bytes([e[o], e[o + 1], e[o + 2], e[o + 3]]);
        let rev = e[ext::REV];
        let cmd6 = if rev >= 6 && e[ext::GENERIC_CMD6_TIME] != 0 {
            e[ext::GENERIC_CMD6_TIME] as u64 * 10_000
        } else {
            500_000
        };
        ExtCsd {
            rev,
            sectors: le32(ext::SEC_COUNT) as u64,
            device_type: e[ext::DEVICE_TYPE],
            partition_config: e[ext::PARTITION_CONFIG],
            boot_bytes: e[ext::BOOT_SIZE_MULT] as u64 * 128 * 1024,
            rpmb_bytes: e[ext::RPMB_SIZE_MULT] as u64 * 128 * 1024,
            hc_erase_group_sectors: e[ext::HC_ERASE_GRP_SIZE] as u32 * 1024,
            trim: e[ext::SEC_FEATURE_SUPPORT] & ext::SEC_GB_CL_EN != 0,
            cache_bytes: if rev >= 5 { le32(ext::CACHE_SIZE) as u64 * 1024 } else { 0 },
            cmd6_us: cmd6,
            partition_switch_us: (e[ext::PARTITION_SWITCH_TIME] as u64 * 10_000).max(cmd6),
            erase_timeout_us: (e[ext::ERASE_TIMEOUT_MULT] as u64 * 300_000).max(1_000_000),
            erased_ones: e[ext::ERASED_MEM_CONT] != 0,
        }
    }
}

/// CMD6 (MMC) argument: write `value` to `EXT_CSD[index]`.
pub fn mmc_switch_arg(index: usize, value: u8) -> u32 {
    3 << 24 | (index as u32) << 16 | (value as u32) << 8
}

/// The tuning block an SD card or a 4-bit eMMC returns (SD Physical Layer
/// 3.01, table 4-2; JESD84 for the 8-bit eMMC pattern below).
pub const TUNING_4BIT: [u8; 64] = [
    0xff, 0x0f, 0xff, 0x00, 0x0f, 0xfc, 0xc3, 0xcc, 0xc3, 0x3c, 0xcc, 0xff, 0xfe, 0xff, 0xfe, 0xef, 0xff, 0xdf, 0xff,
    0xdd, 0xff, 0xfb, 0xff, 0xfb, 0xbf, 0xff, 0x7f, 0xff, 0x77, 0xf7, 0xbd, 0xef, 0xff, 0xf0, 0xff, 0xf0, 0x0f, 0xfc,
    0xcc, 0x3c, 0xcc, 0x33, 0xcc, 0xcf, 0xff, 0xef, 0xff, 0xee, 0xff, 0xfd, 0xff, 0xfd, 0xdf, 0xff, 0xbf, 0xff, 0xbb,
    0xff, 0xf7, 0xff, 0xf7, 0x7f, 0x7b, 0xde,
];

pub const TUNING_8BIT: [u8; 128] = [
    0xff, 0xff, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xcc, 0xcc, 0xcc, 0x33, 0xcc, 0xcc, 0xcc, 0x33, 0x33,
    0xcc, 0xcc, 0xcc, 0xff, 0xff, 0xff, 0xee, 0xff, 0xff, 0xff, 0xee, 0xee, 0xff, 0xff, 0xff, 0xdd, 0xff, 0xff, 0xff,
    0xdd, 0xdd, 0xff, 0xff, 0xff, 0xbb, 0xff, 0xff, 0xff, 0xbb, 0xbb, 0xff, 0xff, 0xff, 0x77, 0xff, 0xff, 0xff, 0x77,
    0x77, 0xff, 0x77, 0xbb, 0xdd, 0xee, 0xff, 0xff, 0xff, 0xff, 0x00, 0xff, 0xff, 0xff, 0x00, 0x00, 0xff, 0xff, 0xcc,
    0xcc, 0xcc, 0x33, 0xcc, 0xcc, 0xcc, 0x33, 0x33, 0xcc, 0xcc, 0xcc, 0xff, 0xff, 0xff, 0xee, 0xff, 0xff, 0xff, 0xee,
    0xee, 0xff, 0xff, 0xff, 0xdd, 0xff, 0xff, 0xff, 0xdd, 0xdd, 0xff, 0xff, 0xff, 0xbb, 0xff, 0xff, 0xff, 0xbb, 0xbb,
    0xff, 0xff, 0xff, 0x77, 0xff, 0xff, 0xff, 0x77, 0x77, 0xff, 0x77, 0xbb, 0xdd, 0xee,
];
