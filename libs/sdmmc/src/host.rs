//! The SD Host Controller: one slot's registers, driven as the SD Host
//! Controller Simplified Specification (4.20) describes.
//!
//! - **Commands** wait for the inhibit bits, clear stale status, program the
//!   argument, then write Transfer Mode and Command in one 32-bit store (the
//!   write of the command's upper byte is what starts it). A response that
//!   doesn't come, or comes corrupt, resets the CMD line (and the DAT line
//!   when data or busy was involved) so the next command can go.
//! - **Data** moves through one physically contiguous bounce buffer, by
//!   PIO (the Buffer Data Port, a block at a time on Buffer Read/Write
//!   Ready), SDMA (with the 512 KiB buffer-boundary interrupt answered by
//!   writing the next boundary's address), or ADMA2 with 32-bit (8-byte)
//!   or 64-bit (12-byte, version 3 layout) descriptors of at most 32 KiB.
//! - **Clock**: the 10-bit divided clock of version 3 and later (base /
//!   2N), or the 8-bit power-of-two divider before it; the card clock is
//!   stopped while the timing or signal voltage changes.
//! - **Tuning** (SDR104, SDR50 where the capabilities ask, eMMC HS200): the
//!   standard procedure with Execute Tuning, up to 40 tuning blocks.
//!
//! Interrupt status is read from the register (an interrupt handler that
//! acknowledges bits must make them visible here again; see the kernel's
//! driver). Waiting is the [`Platform`]'s business: it may sleep on the
//! interrupt or simply spin.

use crate::regs::{self, adma, clk, cmd, err, hc, hc2, int, power, ps, reset, tm};
use crate::{DmaBuf, Error, Platform, Regs, Result};
use alloc::boxed::Box;

/// How data moves between memory and the controller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransferMode {
    Pio,
    Sdma,
    /// ADMA2 with 32-bit descriptors.
    Adma32,
    /// ADMA2 with 64-bit descriptors.
    Adma64,
}

impl TransferMode {
    pub fn name(self) -> &'static str {
        match self {
            TransferMode::Pio => "PIO",
            TransferMode::Sdma => "SDMA",
            TransferMode::Adma32 => "ADMA2 (32-bit)",
            TransferMode::Adma64 => "ADMA2 (64-bit)",
        }
    }
}

/// Bus timing modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Timing {
    /// Default speed: SD up to 25 MHz, MMC up to 26 MHz.
    Legacy,
    /// SD high speed, 50 MHz.
    SdHs,
    /// MMC high speed, 52 MHz.
    MmcHs,
    UhsSdr25,
    UhsDdr50,
    UhsSdr50,
    UhsSdr104,
    /// eMMC HS200: 200 MHz SDR at 1.8 V, tuned.
    Hs200,
    /// eMMC HS400: 200 MHz DDR on an 8-bit bus, after HS200 tuning.
    Hs400,
}

impl Timing {
    pub fn name(self) -> &'static str {
        match self {
            Timing::Legacy => "default speed",
            Timing::SdHs => "high speed",
            Timing::MmcHs => "high speed (52 MHz)",
            Timing::UhsSdr25 => "UHS-I SDR25",
            Timing::UhsDdr50 => "UHS-I DDR50",
            Timing::UhsSdr50 => "UHS-I SDR50",
            Timing::UhsSdr104 => "UHS-I SDR104",
            Timing::Hs200 => "HS200",
            Timing::Hs400 => "HS400",
        }
    }
    /// The mode needs 1.8 V signalling.
    pub fn needs_1v8(self) -> bool {
        matches!(
            self,
            Timing::UhsSdr25 | Timing::UhsDdr50 | Timing::UhsSdr50 | Timing::UhsSdr104 | Timing::Hs200 | Timing::Hs400
        )
    }
    /// The mode samples with a tuned clock.
    pub fn tuned(self) -> bool {
        matches!(self, Timing::UhsSdr104 | Timing::UhsSdr50 | Timing::Hs200 | Timing::Hs400)
    }
}

/// Which kind of slot the capabilities describe.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotType {
    Removable,
    /// Soldered (eMMC, or an SD card that can't be removed).
    Embedded,
    SharedBus,
}

/// The Capabilities register, decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caps {
    pub raw: u64,
    /// The specification version (Host Controller Version register low byte:
    /// 0 is 1.00, 1 is 2.00, 2 is 3.00, 3 is 4.00 …).
    pub version: u8,
    /// Base clock in Hz (0: not given; the platform must know it).
    pub base_clock_hz: u32,
    pub timeout_clock_khz: u32,
    pub max_block: u32,
    pub bus8: bool,
    pub adma2: bool,
    pub high_speed: bool,
    pub sdma: bool,
    pub v33: bool,
    pub v30: bool,
    pub v18: bool,
    /// 64-bit system addresses (ADMA2 64-bit descriptors).
    pub dma64: bool,
    pub slot: SlotType,
    pub sdr50: bool,
    pub sdr104: bool,
    pub ddr50: bool,
    pub tuning_sdr50: bool,
    /// Re-tuning timer in seconds (0: none).
    pub retune_s: u32,
    /// HS400 support: not in the register; platforms that have it set this.
    pub hs400: bool,
}

impl Caps {
    pub fn parse(raw: u64, version: u8) -> Caps {
        let bit = |n: u32| raw & (1 << n) != 0;
        let base_mhz = if version >= regs::version::V3 { (raw >> 8) & 0xFF } else { (raw >> 8) & 0x3F };
        let to = (raw & 0x3F) as u32;
        let retune = ((raw >> 40) & 0xF) as u32;
        Caps {
            raw,
            version,
            base_clock_hz: base_mhz as u32 * 1_000_000,
            timeout_clock_khz: if bit(7) { to * 1000 } else { to },
            max_block: 512 << ((raw >> 16) & 3).min(2),
            bus8: bit(18),
            adma2: bit(19),
            high_speed: bit(21),
            sdma: bit(22),
            v33: bit(24),
            v30: bit(25),
            v18: bit(26),
            dma64: bit(28) || (version >= regs::version::V4 && bit(27)),
            slot: match (raw >> 30) & 3 {
                1 => SlotType::Embedded,
                2 => SlotType::SharedBus,
                _ => SlotType::Removable,
            },
            sdr50: version >= regs::version::V3 && bit(32),
            sdr104: version >= regs::version::V3 && bit(33),
            ddr50: version >= regs::version::V3 && bit(34),
            tuning_sdr50: bit(45),
            retune_s: if retune == 0 || retune > 0xB { 0 } else { 1 << (retune - 1) },
            hs400: false,
        }
    }

    /// The best way to move data the controller offers.
    pub fn best_mode(&self) -> TransferMode {
        if self.adma2 && self.dma64 {
            TransferMode::Adma64
        } else if self.adma2 {
            TransferMode::Adma32
        } else if self.sdma {
            TransferMode::Sdma
        } else {
            TransferMode::Pio
        }
    }

    pub fn supports(&self, mode: TransferMode) -> bool {
        match mode {
            TransferMode::Pio => true,
            TransferMode::Sdma => self.sdma,
            TransferMode::Adma32 => self.adma2,
            TransferMode::Adma64 => self.adma2 && self.dma64,
        }
    }
}

/// What a command expects back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resp {
    None,
    /// 48 bits with CRC and index: R1, R5, R6, R7.
    R1,
    /// R1 with busy signalled on DAT0 afterwards.
    R1b,
    /// 136 bits (CID, CSD).
    R2,
    /// 48 bits without CRC or index (OCR).
    R3,
}

impl Resp {
    fn flags(self) -> u16 {
        match self {
            Resp::None => cmd::RESP_NONE,
            Resp::R1 => cmd::RESP_48 | cmd::CRC_CHECK | cmd::INDEX_CHECK,
            Resp::R1b => cmd::RESP_48_BUSY | cmd::CRC_CHECK | cmd::INDEX_CHECK,
            Resp::R2 => cmd::RESP_136 | cmd::CRC_CHECK,
            Resp::R3 => cmd::RESP_48,
        }
    }
}

/// A data phase: `blocks` blocks of `block_size` bytes through the bounce buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Data {
    pub block_size: u32,
    pub blocks: u32,
    pub write: bool,
    /// Let the controller send CMD12 after the last block.
    pub auto_cmd12: bool,
}

/// One command.
#[derive(Clone, Copy, Debug)]
pub struct Cmd {
    pub index: u8,
    pub arg: u32,
    pub resp: Resp,
    pub data: Option<Data>,
    /// How long the card may stay busy afterwards (R1b, writes), in µs.
    pub busy_us: u64,
}

impl Cmd {
    pub fn new(index: u8, arg: u32, resp: Resp) -> Cmd {
        Cmd { index, arg, resp, data: None, busy_us: 1_000_000 }
    }
    pub fn with_data(mut self, data: Data) -> Cmd {
        self.data = Some(data);
        self
    }
    pub fn busy(mut self, us: u64) -> Cmd {
        self.busy_us = us;
        self
    }
}

/// Counters for the boot log and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub commands: u64,
    pub errors: u64,
    pub line_resets: u64,
    pub tunings: u64,
}

/// How long a command's response may take (the card answers within 64
/// clocks; this covers slow controllers and the scheduler).
const RESPONSE_US: u64 = 100_000;
const SDMA_BOUNDARY: u64 = 512 * 1024;

pub struct Host<R: Regs, P: Platform> {
    pub regs: R,
    pub platform: P,
    pub caps: Caps,
    pub mode: TransferMode,
    data: Box<dyn DmaBuf>,
    desc: Box<dyn DmaBuf>,
    /// The card clock now running, in Hz (0: stopped).
    pub clock_hz: u32,
    pub width: u8,
    pub timing: Timing,
    pub signal_1v8: bool,
    /// Treat the card as present whatever card detect says (soldered eMMC,
    /// or controllers whose card-detect line isn't wired).
    pub force_present: bool,
    pub stats: Stats,
}

impl<R: Regs, P: Platform> Host<R, P> {
    /// Takes over a slot: reads its version and capabilities and resets it.
    /// `data` is the bounce buffer (its size bounds a transfer); `desc` holds
    /// ADMA descriptors (a page is plenty). Both must be reachable by the
    /// controller: below 4 GiB unless it has 64-bit addressing.
    pub fn new(regs: R, platform: P, data: Box<dyn DmaBuf>, desc: Box<dyn DmaBuf>) -> Result<Host<R, P>> {
        let version = (regs.read16(regs::HOST_VERSION) & 0xFF) as u8;
        let raw = regs.read32(regs::CAPABILITIES) as u64 | (regs.read32(regs::CAPABILITIES_HI) as u64) << 32;
        if raw == 0xFFFF_FFFF_FFFF_FFFF {
            return Err(Error::Controller);
        }
        let caps = Caps::parse(raw, version);
        let mut host = Host {
            regs,
            platform,
            caps,
            mode: TransferMode::Pio,
            data,
            desc,
            clock_hz: 0,
            width: 1,
            timing: Timing::Legacy,
            signal_1v8: false,
            force_present: caps.slot == SlotType::Embedded,
            stats: Stats::default(),
        };
        host.mode = host.usable(caps.best_mode());
        host.reset_all()?;
        Ok(host)
    }

    /// `mode`, or the best one below it the controller and buffers allow.
    pub fn usable(&self, mode: TransferMode) -> TransferMode {
        let below_4g = self.data.phys() + self.data.bytes().len() as u64 <= 1 << 32
            && self.desc.phys() + self.desc.bytes().len() as u64 <= 1 << 32;
        let ok = |m: TransferMode| {
            self.caps.supports(m)
                && match m {
                    TransferMode::Pio | TransferMode::Adma64 => true,
                    TransferMode::Sdma | TransferMode::Adma32 => below_4g,
                }
        };
        [TransferMode::Adma64, TransferMode::Adma32, TransferMode::Sdma, TransferMode::Pio]
            .into_iter()
            .skip_while(|&m| m != mode)
            .find(|&m| ok(m))
            .unwrap_or(TransferMode::Pio)
    }

    /// Chooses how data moves from now on (falls back to what works).
    pub fn set_mode(&mut self, mode: TransferMode) -> TransferMode {
        self.mode = self.usable(mode);
        self.mode
    }

    /// The bounce buffer: what reads land in and writes are taken from.
    pub fn buffer(&self) -> &[u8] {
        self.data.bytes()
    }
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        self.data.bytes_mut()
    }

    fn now(&mut self) -> u64 {
        self.platform.now_us()
    }

    /// Waits until `done` holds, up to `us` microseconds.
    fn poll(&mut self, us: u64, mut done: impl FnMut(&Self) -> bool) -> Result<()> {
        let deadline = self.now() + us;
        loop {
            if done(self) {
                return Ok(());
            }
            if self.now() > deadline {
                return if done(self) { Ok(()) } else { Err(Error::Controller) };
            }
            self.platform.wait();
        }
    }

    /// Software reset of `bits` (see [`reset`]); waits for it to finish.
    pub fn reset(&mut self, bits: u8) -> Result<()> {
        self.regs.write8(regs::SOFTWARE_RESET, bits);
        self.poll(100_000, |h| h.regs.read8(regs::SOFTWARE_RESET) & bits == 0)
    }

    /// Resets the CMD and/or DAT line state machines after an error.
    pub fn reset_lines(&mut self, cmd_line: bool, dat_line: bool) {
        let bits = if cmd_line { reset::CMD } else { 0 } | if dat_line { reset::DAT } else { 0 };
        if bits != 0 {
            self.stats.line_resets += 1;
            if self.reset(bits).is_err() {
                self.platform.note(format_args!("line reset {:#x} did not finish", bits));
            }
        }
    }

    /// Resets the whole controller (registers to defaults, card powered off)
    /// and enables its interrupts.
    pub fn reset_all(&mut self) -> Result<()> {
        self.reset(reset::ALL)?;
        self.clock_hz = 0;
        self.width = 1;
        self.timing = Timing::Legacy;
        self.signal_1v8 = false;
        // Every status except the SDIO card interrupt; retuning events are
        // only useful with a tuned bus, but harmless.
        let normal = !int::CARD_INT & !int::ERROR;
        self.regs.write16(regs::INT_ENABLE, normal);
        self.regs.write16(regs::ERR_ENABLE, 0x0FFF);
        self.set_interrupts(true);
        self.regs.write8(regs::TIMEOUT_CONTROL, 0xE);
        Ok(())
    }

    /// Lets the enabled statuses raise the interrupt (or not).
    pub fn set_interrupts(&mut self, on: bool) {
        let normal = if on { !int::CARD_INT & !int::ERROR & !int::RETUNE } else { 0 };
        self.regs.write16(regs::INT_SIGNAL, normal);
        self.regs.write16(regs::ERR_SIGNAL, if on { 0x0FFF } else { 0 });
    }

    pub fn present_state(&self) -> u32 {
        self.regs.read32(regs::PRESENT_STATE)
    }

    pub fn card_present(&self) -> bool {
        self.force_present || self.present_state() & ps::CARD_INSERTED != 0
    }

    /// The card's write-protect switch says no (removable slots only).
    pub fn write_protect_switch(&self) -> bool {
        self.caps.slot == SlotType::Removable && self.present_state() & ps::WRITE_ENABLED == 0
    }

    /// Takes and clears pending card insert/remove statuses.
    pub fn take_card_events(&mut self) -> u16 {
        let s = self.regs.read16(regs::INT_STATUS) & int::CARD_CHANGE;
        if s != 0 {
            self.regs.write16(regs::INT_STATUS, s);
        }
        s
    }

    /// Bus power off.
    pub fn power_off(&mut self) {
        self.set_clock(0).ok();
        self.regs.write8(regs::POWER_CONTROL, 0);
    }

    /// Bus power on at the highest voltage the slot offers (1.8 V only for
    /// slots that offer nothing else: soldered 1.8 V eMMC).
    pub fn power_on(&mut self) -> Result<()> {
        let sel = if self.caps.v33 {
            power::V33
        } else if self.caps.v30 {
            power::V30
        } else if self.caps.v18 {
            power::V18
        } else {
            return Err(Error::Unsupported);
        };
        self.regs.write8(regs::POWER_CONTROL, sel);
        self.regs.write8(regs::POWER_CONTROL, sel | power::ON);
        if self.regs.read8(regs::POWER_CONTROL) & power::ON == 0 {
            // No card, or the controller refused the voltage.
            return Err(if self.card_present() { Error::Controller } else { Error::NoCard });
        }
        // Power ramp-up (the specification allows the card 1 ms; give it more).
        self.platform.delay_us(2_000);
        Ok(())
    }

    /// Power off long enough for the card to reset, then on again at 3.3 V signalling.
    pub fn power_cycle(&mut self) -> Result<()> {
        self.power_off();
        if self.signal_1v8 {
            let _ = self.set_signal_voltage(false);
        }
        self.platform.delay_us(10_000);
        self.power_on()
    }

    /// Runs the card clock as close to `hz` as possible without exceeding it
    /// (0 stops it). Returns the actual frequency.
    pub fn set_clock(&mut self, hz: u32) -> Result<u32> {
        self.regs.write16(regs::CLOCK_CONTROL, 0);
        self.clock_hz = 0;
        if hz == 0 {
            return Ok(0);
        }
        let base = if self.caps.base_clock_hz == 0 { 50_000_000 } else { self.caps.base_clock_hz };
        let (field, actual) = if self.caps.version >= regs::version::V3 {
            // Divided clock mode: base / 2N, N up to 1023 (0: base itself).
            let n = if hz >= base { 0 } else { base.div_ceil(2 * hz).min(1023) };
            let actual = if n == 0 { base } else { base / (2 * n) };
            (((n & 0xFF) << 8 | ((n >> 8) & 3) << 6) as u16, actual)
        } else {
            // Power-of-two divider: base / d, d from 1 to 256 (field d / 2).
            let mut d = 1u32;
            while d < 256 && base / d > hz {
                d *= 2;
            }
            (((d / 2) << 8) as u16, base / d)
        };
        self.regs.write16(regs::CLOCK_CONTROL, field | clk::INT_ENABLE);
        if self.poll(20_000, |h| h.regs.read16(regs::CLOCK_CONTROL) & clk::INT_STABLE != 0).is_err() {
            return Err(Error::Controller);
        }
        if self.caps.version > regs::version::V4 {
            // 4.10 and later gate the clock with a PLL.
            self.regs.write16(regs::CLOCK_CONTROL, field | clk::INT_ENABLE | clk::PLL_ENABLE);
            let _ = self.poll(20_000, |h| h.regs.read16(regs::CLOCK_CONTROL) & clk::INT_STABLE != 0);
        }
        let on = self.regs.read16(regs::CLOCK_CONTROL) | clk::CARD_ENABLE;
        self.regs.write16(regs::CLOCK_CONTROL, on);
        self.clock_hz = actual;
        Ok(actual)
    }

    /// Stops or restarts the card clock without changing its frequency.
    pub fn card_clock(&mut self, on: bool) {
        let c = self.regs.read16(regs::CLOCK_CONTROL);
        self.regs.write16(regs::CLOCK_CONTROL, if on { c | clk::CARD_ENABLE } else { c & !clk::CARD_ENABLE });
    }

    /// DAT[3:0] line levels.
    pub fn dat_level(&self) -> u32 {
        (self.present_state() & ps::DAT_LEVEL) >> 20
    }

    pub fn set_width(&mut self, width: u8) {
        let mut h = self.regs.read8(regs::HOST_CONTROL) & !(hc::WIDTH_4 | hc::WIDTH_8);
        h |= match width {
            8 => hc::WIDTH_8,
            4 => hc::WIDTH_4,
            _ => 0,
        };
        self.regs.write8(regs::HOST_CONTROL, h);
        self.width = width;
    }

    /// Selects the bus timing. The card clock is stopped meanwhile: call
    /// [`Host::set_clock`] afterwards.
    pub fn set_timing(&mut self, timing: Timing) {
        self.card_clock(false);
        let mut h = self.regs.read8(regs::HOST_CONTROL);
        if timing == Timing::Legacy {
            h &= !hc::HIGH_SPEED;
        } else {
            h |= hc::HIGH_SPEED;
        }
        self.regs.write8(regs::HOST_CONTROL, h);
        if self.caps.version >= regs::version::V3 {
            let mode = match timing {
                Timing::Legacy | Timing::SdHs | Timing::MmcHs => hc2::SDR12,
                Timing::UhsSdr25 => hc2::SDR25,
                Timing::UhsSdr50 => hc2::SDR50,
                Timing::UhsSdr104 | Timing::Hs200 => hc2::SDR104,
                Timing::UhsDdr50 => hc2::DDR50,
                Timing::Hs400 => hc2::HS400,
            };
            let c = self.regs.read16(regs::HOST_CONTROL2) & !hc2::UHS_MASK;
            self.regs.write16(regs::HOST_CONTROL2, c | mode);
        }
        self.timing = timing;
    }

    /// Switches I/O signalling to 1.8 V (or back to 3.3 V).
    pub fn set_signal_voltage(&mut self, v18: bool) -> Result<()> {
        if self.caps.version < regs::version::V3 || (v18 && !self.caps.v18) {
            return Err(Error::Unsupported);
        }
        let c = self.regs.read16(regs::HOST_CONTROL2);
        let c = if v18 { c | hc2::V18 } else { c & !hc2::V18 };
        self.regs.write16(regs::HOST_CONTROL2, c);
        // The regulator has 5 ms to settle.
        self.platform.delay_us(5_000);
        if (self.regs.read16(regs::HOST_CONTROL2) & hc2::V18 != 0) != v18 {
            return Err(Error::Controller);
        }
        self.signal_1v8 = v18;
        Ok(())
    }

    /// Waits for one of `want`'s statuses (clearing it), or an error.
    fn wait_int(&mut self, want: u16, us: u64) -> Result<u16> {
        let deadline = self.now() + us;
        loop {
            let st = self.regs.read16(regs::INT_STATUS);
            if st & int::ERROR != 0 {
                let e = self.regs.read16(regs::ERR_STATUS);
                self.regs.write16(regs::ERR_STATUS, e);
                self.stats.errors += 1;
                return Err(self.decode_error(e));
            }
            if st & want != 0 {
                self.regs.write16(regs::INT_STATUS, st & want);
                return Ok(st);
            }
            if !self.card_present() {
                return Err(Error::NoCard);
            }
            if self.now() > deadline {
                self.stats.errors += 1;
                return Err(if want & int::CMD_COMPLETE != 0 { Error::Timeout } else { Error::DataTimeout });
            }
            self.platform.wait();
        }
    }

    fn decode_error(&mut self, e: u16) -> Error {
        if e & err::CMD_TIMEOUT != 0 {
            Error::Timeout
        } else if e & (err::CMD_CRC | err::CMD_END_BIT | err::CMD_INDEX) != 0 {
            Error::Crc
        } else if e & err::ADMA != 0 {
            Error::Dma(self.regs.read8(regs::ADMA_ERROR))
        } else if e & err::DATA_TIMEOUT != 0 {
            Error::DataTimeout
        } else if e & (err::DATA_CRC | err::DATA_END_BIT) != 0 {
            Error::DataCrc
        } else if e & err::AUTO_CMD != 0 {
            let a = self.regs.read16(regs::AUTO_CMD_STATUS);
            if a & 1 != 0 {
                Error::Timeout
            } else {
                Error::Crc
            }
        } else if e & err::TUNING != 0 {
            Error::Tuning
        } else {
            Error::Controller
        }
    }

    /// Sends a command, with its data phase if it has one. Returns the
    /// response registers (R2: all four; others: the first).
    pub fn command(&mut self, c: &Cmd) -> Result<[u32; 4]> {
        self.stats.commands += 1;
        if !self.card_present() {
            return Err(Error::NoCard);
        }
        let abort = c.index == 12;
        let busy = c.resp == Resp::R1b;
        let mut inhibit = ps::CMD_INHIBIT;
        if (c.data.is_some() || busy) && !abort {
            inhibit |= ps::DAT_INHIBIT;
        }
        if self.poll(50_000, |h| h.present_state() & inhibit == 0).is_err() {
            // A line left busy by an earlier failure.
            self.reset_lines(true, inhibit & ps::DAT_INHIBIT != 0);
            if self.present_state() & inhibit != 0 {
                return Err(Error::Controller);
            }
        }
        self.regs.write16(regs::ERR_STATUS, 0xFFFF);
        self.regs.write16(regs::INT_STATUS, !int::CARD_CHANGE & !int::ERROR);
        let mode = match &c.data {
            Some(d) => self.setup_data(d)?,
            None => 0,
        };
        let mut flags = c.resp.flags();
        if c.data.is_some() {
            flags |= cmd::DATA;
        }
        if abort {
            flags |= cmd::ABORT;
        }
        self.regs.write32(regs::ARGUMENT, c.arg);
        let command = (c.index as u16) << 8 | flags;
        self.regs.write32(regs::TRANSFER_MODE, mode as u32 | (command as u32) << 16);
        if let Err(e) = self.wait_int(int::CMD_COMPLETE, RESPONSE_US) {
            self.reset_lines(true, c.data.is_some() || busy);
            return Err(e);
        }
        let mut resp = [0u32; 4];
        if c.resp == Resp::R2 {
            for (i, r) in resp.iter_mut().enumerate() {
                *r = self.regs.read32(regs::RESPONSE + i * 4);
            }
        } else if c.resp != Resp::None {
            resp[0] = self.regs.read32(regs::RESPONSE);
        }
        let done = match &c.data {
            Some(d) => self.transfer(d, c.busy_us),
            None if busy => self.wait_int(int::TRANSFER_COMPLETE, c.busy_us).map(|_| ()),
            None => Ok(()),
        };
        if let Err(e) = done {
            self.reset_lines(true, true);
            return Err(e);
        }
        Ok(resp)
    }

    /// The Auto CMD12 response (card status after a multi-block transfer).
    pub fn auto_cmd12_response(&self) -> u32 {
        self.regs.read32(regs::RESPONSE + 12)
    }

    /// Programs block size and count, timeout and DMA; returns Transfer Mode.
    fn setup_data(&mut self, d: &Data) -> Result<u16> {
        let len = d.block_size as usize * d.blocks as usize;
        if d.blocks == 0 || d.blocks > 0xFFFF || len > self.data.bytes().len() || d.block_size > 2048 {
            return Err(Error::Unsupported);
        }
        self.regs.write16(regs::BLOCK_SIZE, d.block_size as u16 | regs::BOUNDARY_512K);
        self.regs.write16(regs::BLOCK_COUNT, d.blocks as u16);
        self.regs.write8(regs::TIMEOUT_CONTROL, 0xE);
        let mut mode = tm::BLOCK_COUNT;
        if d.blocks > 1 {
            mode |= tm::MULTI;
            if d.auto_cmd12 {
                mode |= tm::AUTO_CMD12;
            }
        }
        if !d.write {
            mode |= tm::READ;
        }
        let select = match self.mode {
            TransferMode::Pio => return Ok(mode),
            TransferMode::Sdma => {
                self.regs.write32(regs::SDMA_ADDRESS, self.data.phys() as u32);
                hc::DMA_SDMA
            }
            TransferMode::Adma32 | TransferMode::Adma64 => {
                self.write_descriptors(len);
                let table = self.desc.phys();
                self.regs.write32(regs::ADMA_ADDRESS, table as u32);
                if self.mode == TransferMode::Adma64 {
                    self.regs.write32(regs::ADMA_ADDRESS_HI, (table >> 32) as u32);
                    hc::DMA_ADMA64
                } else {
                    hc::DMA_ADMA32
                }
            }
        };
        let h = self.regs.read8(regs::HOST_CONTROL) & !hc::DMA_MASK;
        self.regs.write8(regs::HOST_CONTROL, h | select);
        Ok(mode | tm::DMA)
    }

    /// An ADMA2 descriptor table covering the first `len` bytes of the buffer.
    fn write_descriptors(&mut self, len: usize) {
        let base = self.data.phys();
        let wide = self.mode == TransferMode::Adma64;
        let size = if wide { 12 } else { 8 };
        let table = self.desc.bytes_mut();
        let count = len.div_ceil(adma::MAX_LEN);
        assert!(count * size <= table.len(), "ADMA descriptor table too small");
        for i in 0..count {
            let off = i * adma::MAX_LEN;
            let n = (len - off).min(adma::MAX_LEN);
            let mut attr = adma::VALID | adma::TRAN;
            if i + 1 == count {
                attr |= adma::END;
            }
            let d = &mut table[i * size..(i + 1) * size];
            d[0..2].copy_from_slice(&attr.to_le_bytes());
            d[2..4].copy_from_slice(&(n as u16).to_le_bytes());
            let addr = base + off as u64;
            if wide {
                d[4..12].copy_from_slice(&addr.to_le_bytes());
            } else {
                d[4..8].copy_from_slice(&(addr as u32).to_le_bytes());
            }
        }
    }

    /// The data phase after the command's response.
    fn transfer(&mut self, d: &Data, busy_us: u64) -> Result<()> {
        // Generous: a slow card at 400 kHz moves a 512-byte block in ~10 ms.
        let us = 1_000_000 + d.blocks as u64 * 20_000 + if d.write { busy_us } else { 0 };
        match self.mode {
            TransferMode::Pio => {
                let words = d.block_size as usize / 4;
                for b in 0..d.blocks as usize {
                    let ready = if d.write { int::BUFFER_WRITE } else { int::BUFFER_READ };
                    self.wait_int(ready, us)?;
                    let at = b * d.block_size as usize;
                    for w in 0..words {
                        let o = at + w * 4;
                        if d.write {
                            let v = u32::from_le_bytes(self.data.bytes()[o..o + 4].try_into().unwrap());
                            self.regs.write32(regs::BUFFER, v);
                        } else {
                            let v = self.regs.read32(regs::BUFFER);
                            self.data.bytes_mut()[o..o + 4].copy_from_slice(&v.to_le_bytes());
                        }
                    }
                }
                self.wait_int(int::TRANSFER_COMPLETE, us)?;
            }
            TransferMode::Sdma => {
                let mut next = (self.data.phys() & !(SDMA_BOUNDARY - 1)) + SDMA_BOUNDARY;
                loop {
                    let st = self.wait_int(int::TRANSFER_COMPLETE | int::DMA, us)?;
                    if st & int::TRANSFER_COMPLETE != 0 {
                        break;
                    }
                    // Stopped at a buffer boundary: carry on from the next one.
                    self.regs.write32(regs::SDMA_ADDRESS, next as u32);
                    next += SDMA_BOUNDARY;
                }
            }
            TransferMode::Adma32 | TransferMode::Adma64 => {
                self.wait_int(int::TRANSFER_COMPLETE, us)?;
            }
        }
        Ok(())
    }

    /// The tuning procedure for `index` (CMD19 for SD, CMD21 for eMMC) with
    /// tuning blocks of `block_size` bytes (64 on a 4-bit bus, 128 on 8).
    pub fn tune(&mut self, index: u8, block_size: u16) -> Result<()> {
        if self.caps.version < regs::version::V3 {
            return Err(Error::Unsupported);
        }
        self.stats.tunings += 1;
        let c2 = self.regs.read16(regs::HOST_CONTROL2) & !hc2::SAMPLING_CLOCK;
        self.regs.write16(regs::HOST_CONTROL2, c2 | hc2::EXECUTE_TUNING);
        for _ in 0..40 {
            if self.poll(50_000, |h| h.present_state() & (ps::CMD_INHIBIT | ps::DAT_INHIBIT) == 0).is_err() {
                self.reset_lines(true, true);
            }
            self.regs.write16(regs::ERR_STATUS, 0xFFFF);
            self.regs.write16(regs::INT_STATUS, !int::CARD_CHANGE & !int::ERROR);
            self.regs.write16(regs::BLOCK_SIZE, block_size);
            self.regs.write16(regs::BLOCK_COUNT, 1);
            self.regs.write32(regs::ARGUMENT, 0);
            let command = (index as u16) << 8 | Resp::R1.flags() | cmd::DATA;
            let mode = tm::READ | tm::BLOCK_COUNT;
            self.regs.write32(regs::TRANSFER_MODE, mode as u32 | (command as u32) << 16);
            // The controller reads (and compares) the block itself; Buffer
            // Read Ready says it has. Some controllers just clear Execute.
            let deadline = self.now() + 50_000;
            loop {
                let st = self.regs.read16(regs::INT_STATUS);
                if st & (int::BUFFER_READ | int::ERROR) != 0 {
                    self.regs.write16(regs::INT_STATUS, st & (int::BUFFER_READ | int::CMD_COMPLETE));
                    self.regs.write16(regs::ERR_STATUS, 0xFFFF);
                    break;
                }
                if self.regs.read16(regs::HOST_CONTROL2) & hc2::EXECUTE_TUNING == 0 || self.now() > deadline {
                    break;
                }
                self.platform.wait();
            }
            if self.regs.read16(regs::HOST_CONTROL2) & hc2::EXECUTE_TUNING == 0 {
                break;
            }
        }
        self.reset_lines(true, true);
        let c2 = self.regs.read16(regs::HOST_CONTROL2);
        if c2 & hc2::EXECUTE_TUNING != 0 {
            self.regs.write16(regs::HOST_CONTROL2, c2 & !(hc2::EXECUTE_TUNING | hc2::SAMPLING_CLOCK));
            return Err(Error::Tuning);
        }
        if c2 & hc2::SAMPLING_CLOCK == 0 {
            return Err(Error::Tuning);
        }
        Ok(())
    }
}
