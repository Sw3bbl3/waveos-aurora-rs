//! The SD Host Controller's registers (one 256-byte block per slot), as the
//! SD Host Controller Simplified Specification (version 4.20) lays them out.

pub const SDMA_ADDRESS: usize = 0x00;
pub const BLOCK_SIZE: usize = 0x04;
pub const BLOCK_COUNT: usize = 0x06;
pub const ARGUMENT: usize = 0x08;
pub const TRANSFER_MODE: usize = 0x0C;
pub const COMMAND: usize = 0x0E;
pub const RESPONSE: usize = 0x10;
pub const BUFFER: usize = 0x20;
pub const PRESENT_STATE: usize = 0x24;
pub const HOST_CONTROL: usize = 0x28;
pub const POWER_CONTROL: usize = 0x29;
pub const CLOCK_CONTROL: usize = 0x2C;
pub const TIMEOUT_CONTROL: usize = 0x2E;
pub const SOFTWARE_RESET: usize = 0x2F;
pub const INT_STATUS: usize = 0x30;
pub const ERR_STATUS: usize = 0x32;
pub const INT_ENABLE: usize = 0x34;
pub const ERR_ENABLE: usize = 0x36;
pub const INT_SIGNAL: usize = 0x38;
pub const ERR_SIGNAL: usize = 0x3A;
pub const AUTO_CMD_STATUS: usize = 0x3C;
pub const HOST_CONTROL2: usize = 0x3E;
pub const CAPABILITIES: usize = 0x40;
pub const CAPABILITIES_HI: usize = 0x44;
pub const MAX_CURRENT: usize = 0x48;
pub const ADMA_ERROR: usize = 0x54;
pub const ADMA_ADDRESS: usize = 0x58;
pub const ADMA_ADDRESS_HI: usize = 0x5C;
pub const SLOT_INT_STATUS: usize = 0xFC;
pub const HOST_VERSION: usize = 0xFE;

/// Block Size register: the SDMA buffer boundary in bits 14:12 (4 KiB << n).
pub const BOUNDARY_512K: u16 = 7 << 12;

/// Transfer Mode bits.
pub mod tm {
    pub const DMA: u16 = 1 << 0;
    pub const BLOCK_COUNT: u16 = 1 << 1;
    pub const AUTO_CMD12: u16 = 1 << 2;
    pub const AUTO_CMD23: u16 = 2 << 2;
    pub const READ: u16 = 1 << 4;
    pub const MULTI: u16 = 1 << 5;
}

/// Command register bits (the index goes in 13:8).
pub mod cmd {
    pub const RESP_NONE: u16 = 0;
    pub const RESP_136: u16 = 1;
    pub const RESP_48: u16 = 2;
    pub const RESP_48_BUSY: u16 = 3;
    pub const CRC_CHECK: u16 = 1 << 3;
    pub const INDEX_CHECK: u16 = 1 << 4;
    pub const DATA: u16 = 1 << 5;
    pub const ABORT: u16 = 3 << 6;
}

/// Present State bits.
pub mod ps {
    pub const CMD_INHIBIT: u32 = 1 << 0;
    pub const DAT_INHIBIT: u32 = 1 << 1;
    pub const DAT_ACTIVE: u32 = 1 << 2;
    pub const WRITE_ACTIVE: u32 = 1 << 8;
    pub const READ_ACTIVE: u32 = 1 << 9;
    pub const BUFFER_WRITE: u32 = 1 << 10;
    pub const BUFFER_READ: u32 = 1 << 11;
    pub const CARD_INSERTED: u32 = 1 << 16;
    pub const CARD_STABLE: u32 = 1 << 17;
    pub const CARD_DETECT: u32 = 1 << 18;
    /// The write-protect switch's level: set when writing is allowed.
    pub const WRITE_ENABLED: u32 = 1 << 19;
    /// DAT[3:0] line levels.
    pub const DAT_LEVEL: u32 = 0xF << 20;
    pub const DAT0_LEVEL: u32 = 1 << 20;
}

/// Host Control 1 bits.
pub mod hc {
    pub const LED: u8 = 1 << 0;
    pub const WIDTH_4: u8 = 1 << 1;
    pub const HIGH_SPEED: u8 = 1 << 2;
    pub const DMA_MASK: u8 = 3 << 3;
    pub const DMA_SDMA: u8 = 0 << 3;
    pub const DMA_ADMA32: u8 = 2 << 3;
    pub const DMA_ADMA64: u8 = 3 << 3;
    pub const WIDTH_8: u8 = 1 << 5;
}

/// Power Control: bus power and its voltage.
pub mod power {
    pub const ON: u8 = 1 << 0;
    pub const V18: u8 = 5 << 1;
    pub const V30: u8 = 6 << 1;
    pub const V33: u8 = 7 << 1;
}

/// Clock Control bits (the divider's low byte in 15:8, its two high bits in 7:6).
pub mod clk {
    pub const INT_ENABLE: u16 = 1 << 0;
    pub const INT_STABLE: u16 = 1 << 1;
    pub const CARD_ENABLE: u16 = 1 << 2;
    pub const PLL_ENABLE: u16 = 1 << 3;
}

/// Software Reset bits.
pub mod reset {
    pub const ALL: u8 = 1 << 0;
    pub const CMD: u8 = 1 << 1;
    pub const DAT: u8 = 1 << 2;
}

/// Normal Interrupt Status bits.
pub mod int {
    pub const CMD_COMPLETE: u16 = 1 << 0;
    pub const TRANSFER_COMPLETE: u16 = 1 << 1;
    pub const BLOCK_GAP: u16 = 1 << 2;
    pub const DMA: u16 = 1 << 3;
    pub const BUFFER_WRITE: u16 = 1 << 4;
    pub const BUFFER_READ: u16 = 1 << 5;
    pub const CARD_INSERT: u16 = 1 << 6;
    pub const CARD_REMOVE: u16 = 1 << 7;
    /// SDIO card interrupt (left disabled: memory cards only).
    pub const CARD_INT: u16 = 1 << 8;
    pub const RETUNE: u16 = 1 << 12;
    pub const ERROR: u16 = 1 << 15;
    pub const CARD_CHANGE: u16 = CARD_INSERT | CARD_REMOVE;
}

/// Error Interrupt Status bits.
pub mod err {
    pub const CMD_TIMEOUT: u16 = 1 << 0;
    pub const CMD_CRC: u16 = 1 << 1;
    pub const CMD_END_BIT: u16 = 1 << 2;
    pub const CMD_INDEX: u16 = 1 << 3;
    pub const DATA_TIMEOUT: u16 = 1 << 4;
    pub const DATA_CRC: u16 = 1 << 5;
    pub const DATA_END_BIT: u16 = 1 << 6;
    pub const CURRENT_LIMIT: u16 = 1 << 7;
    pub const AUTO_CMD: u16 = 1 << 8;
    pub const ADMA: u16 = 1 << 9;
    pub const TUNING: u16 = 1 << 10;
    pub const RESPONSE: u16 = 1 << 11;
    pub const CMD_ANY: u16 = CMD_TIMEOUT | CMD_CRC | CMD_END_BIT | CMD_INDEX;
    pub const DATA_ANY: u16 = DATA_TIMEOUT | DATA_CRC | DATA_END_BIT | AUTO_CMD | ADMA | TUNING | RESPONSE;
}

/// Host Control 2 bits.
pub mod hc2 {
    pub const UHS_MASK: u16 = 7;
    pub const SDR12: u16 = 0;
    pub const SDR25: u16 = 1;
    pub const SDR50: u16 = 2;
    /// Also eMMC HS200.
    pub const SDR104: u16 = 3;
    pub const DDR50: u16 = 4;
    /// HS400 is not in the SD specification; this value is the one Intel's
    /// and most other eMMC controllers use.
    pub const HS400: u16 = 5;
    pub const V18: u16 = 1 << 3;
    pub const EXECUTE_TUNING: u16 = 1 << 6;
    pub const SAMPLING_CLOCK: u16 = 1 << 7;
    pub const VERSION4: u16 = 1 << 12;
    pub const ADDRESSING64: u16 = 1 << 13;
    pub const PRESET_ENABLE: u16 = 1 << 15;
}

/// ADMA2 descriptor attributes.
pub mod adma {
    pub const VALID: u16 = 1 << 0;
    pub const END: u16 = 1 << 1;
    pub const INT: u16 = 1 << 2;
    pub const TRAN: u16 = 2 << 4;
    pub const LINK: u16 = 3 << 4;
    /// Largest length one descriptor carries here (a power of two every
    /// controller takes; the field's 0 meaning 64 KiB is not universal).
    pub const MAX_LEN: usize = 32 * 1024;
}

/// Host Controller Version: the specification version in the low byte.
pub mod version {
    pub const V1: u8 = 0;
    pub const V2: u8 = 1;
    pub const V3: u8 = 2;
    pub const V4: u8 = 3;
}
