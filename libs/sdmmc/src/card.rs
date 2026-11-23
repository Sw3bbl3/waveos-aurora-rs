//! Cards: bring-up of SD memory cards and eMMC, and block I/O on them.
//!
//! **SD** (Physical Layer Simplified Specification 3.01+): CMD0, CMD8 (2.00
//! cards answer it), ACMD41 until powered up (asking for high capacity and,
//! when the slot can, 1.8 V), CMD11 and the signal-voltage switch, CMD2,
//! CMD3 (the card picks its address), CMD9, CMD7; then the SCR (ACMD51),
//! a 4-bit bus (ACMD6), and the fastest bus mode both sides have: UHS-I
//! SDR104, SDR50 or DDR50 at 1.8 V (with tuning), else high speed (CMD6),
//! else default speed. SDSC cards are byte-addressed, SDHC and SDXC by block.
//!
//! **eMMC** (JESD84-B51): CMD0, CMD1 until powered up (sector mode), CMD2,
//! CMD3 (the host assigns the address), CMD9, CMD7, `EXT_CSD` (CMD8); the
//! user area is selected (boot and RPMB partitions are never touched), the
//! high-capacity erase groups and the write cache turned on, an 8-bit bus
//! (falling back to 4 and 1 when `EXT_CSD` doesn't read back the same),
//! then HS400 (only when allowed: [`Limits::hs400`]), HS200 (tuned with
//! CMD21) or high speed.
//!
//! **I/O**: CMD17/18 and CMD24/25, multi-block bounded by CMD23 when the
//! card has it, else ended by Auto CMD12. Every failure is followed by a
//! recovery (line resets, CMD12 if the card is still sending or receiving,
//! waiting for the transfer state); a second failure re-tunes a tuned bus;
//! a third brings the card up again one speed lower. Writes wait for the
//! card to finish programming (CMD13) before returning.

use crate::card_regs::{self as cr, ext, status, Cid, Csd, ExtCsd, Scr, SwitchStatus};
use crate::host::{Cmd, Data, Host, Resp, SlotType, Timing, TransferMode};
use crate::{Error, Platform, Regs, Result};
use alloc::vec;
use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    /// Standard capacity SD (up to 2 GB, byte-addressed).
    Sdsc,
    /// High capacity SD (up to 32 GB).
    Sdhc,
    /// Extended capacity SD (above 32 GB).
    Sdxc,
    /// eMMC (or MMC) up to 2 GB, byte-addressed.
    Mmc,
    /// eMMC above 2 GB, sector-addressed.
    MmcHc,
}

impl CardKind {
    pub fn is_mmc(self) -> bool {
        matches!(self, CardKind::Mmc | CardKind::MmcHc)
    }
    pub fn name(self) -> &'static str {
        match self {
            CardKind::Sdsc => "SDSC",
            CardKind::Sdhc => "SDHC",
            CardKind::Sdxc => "SDXC",
            CardKind::Mmc => "MMC",
            CardKind::MmcHc => "eMMC",
        }
    }
}

/// How fast the bus may go (each fallback after errors lowers it a step).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Speed {
    /// At most 12.5 MHz, default timing: the last resort.
    Slow,
    /// Default speed (25/26 MHz).
    Default,
    /// SD high speed / MMC high speed (50/52 MHz).
    High,
    /// UHS-I, HS200 or HS400.
    Ultra,
}

/// What the driver may use (boot options and fallbacks narrow it).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub speed: Speed,
    /// Preferred way to move data (the controller's best when `None`).
    pub mode: Option<TransferMode>,
    pub bus8: bool,
    /// eMMC HS400 (off by default: needs per-platform knowledge, see the docs).
    pub hs400: bool,
    pub max_clock_hz: u32,
}

impl Default for Limits {
    fn default() -> Limits {
        Limits { speed: Speed::Ultra, mode: None, bus8: true, hs400: false, max_clock_hz: 208_000_000 }
    }
}

/// How to discard sectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EraseKind {
    /// The data is no longer needed (TRIM on eMMC; erase on SD).
    Discard,
    /// Erase (eMMC: whole erase groups only).
    Erase,
}

/// What bring-up found and chose.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CardInfo {
    pub kind: CardKind,
    pub rca: u16,
    pub ocr: u32,
    pub cid: Cid,
    pub sectors: u64,
    pub block_addressed: bool,
    pub cmd23: bool,
    pub width: u8,
    pub timing: Timing,
    pub clock_hz: u32,
    pub signal_1v8: bool,
    pub read_only: bool,
    /// eMMC: TRIM available.
    pub trim: bool,
    /// eMMC: erase group in sectors (SD erases any range).
    pub erase_group: u32,
    /// eMMC: the volatile cache is on (flush makes writes durable).
    pub cache: bool,
    /// eMMC: each boot partition's and the RPMB partition's size (hidden).
    pub boot_bytes: u64,
    pub rpmb_bytes: u64,
    pub ext_csd_rev: u8,
    /// Erased sectors read as 0xFF (else 0x00).
    pub erased_ones: bool,
}

impl CardInfo {
    /// One line for the log: kind, name, size, bus.
    pub fn summary(&self) -> alloc::string::String {
        alloc::format!(
            "{} \"{}\" {} MiB, {}-bit {} at {} kHz{}{}",
            self.kind.name(),
            self.cid.name,
            self.sectors >> 11,
            self.width,
            self.timing.name(),
            self.clock_hz / 1000,
            if self.signal_1v8 { ", 1.8 V" } else { "" },
            if self.read_only { ", write protected" } else { "" }
        )
    }
}

const SECTOR: u32 = 512;
/// A write may keep the card busy this long (SDXC allows 500 ms; eMMC more).
const WRITE_BUSY_US: u64 = 3_000_000;
const INIT_TIMEOUT_US: u64 = 1_000_000;

/// One slot and the card in it.
pub struct Slot<R: Regs, P: Platform> {
    pub host: Host<R, P>,
    pub card: Option<CardInfo>,
    /// What bring-up may use (the boot options').
    pub limits: Limits,
    /// The current ceiling after fallbacks.
    speed: Speed,
    last_tune_us: u64,
    /// This bring-up may use UHS-I or HS200.
    ultra: bool,
    v18_failed: bool,
    /// Bring-ups done after errors (for the log and tests).
    pub fallbacks: u32,
    pub retries: u32,
}

fn r1_check(r1: u32, mask: u32) -> Result<u32> {
    if r1 & mask != 0 {
        Err(if r1 & status::OUT_OF_RANGE != 0 {
            Error::OutOfRange
        } else if r1 & status::WP_VIOLATION != 0 {
            Error::ReadOnly
        } else {
            Error::Card(r1 & mask)
        })
    } else {
        Ok(r1)
    }
}

impl<R: Regs, P: Platform> Slot<R, P> {
    pub fn new(host: Host<R, P>, limits: Limits) -> Slot<R, P> {
        Slot {
            host,
            card: None,
            limits,
            speed: limits.speed,
            last_tune_us: 0,
            ultra: false,
            v18_failed: false,
            fallbacks: 0,
            retries: 0,
        }
    }

    fn cmd(&mut self, index: u8, arg: u32, resp: Resp) -> Result<u32> {
        self.host.command(&Cmd::new(index, arg, resp)).map(|r| r[0])
    }

    fn rca_arg(&self) -> u32 {
        (self.card.as_ref().map_or(0, |c| c.rca) as u32) << 16
    }

    /// CMD55 then the application command.
    fn app_cmd(&mut self, rca: u16, c: &Cmd) -> Result<[u32; 4]> {
        self.cmd(55, (rca as u32) << 16, Resp::R1)?;
        self.host.command(c)
    }

    /// A small read into a vector (registers and status blocks).
    fn read_block(&mut self, c: Cmd, bytes: u32, app_rca: Option<u16>) -> Result<Vec<u8>> {
        let c = c.with_data(Data { block_size: bytes, blocks: 1, write: false, auto_cmd12: false });
        match app_rca {
            Some(rca) => self.app_cmd(rca, &c)?,
            None => self.host.command(&c)?,
        };
        Ok(self.host.buffer()[..bytes as usize].to_vec())
    }

    /// Takes the card in the slot through bring-up. On success the card's
    /// description is in [`Slot::card`].
    pub fn init_card(&mut self) -> Result<&CardInfo> {
        self.speed = self.limits.speed;
        self.bring_up()
    }

    fn bring_up(&mut self) -> Result<&CardInfo> {
        self.card = None;
        if !self.host.card_present() {
            return Err(Error::NoCard);
        }
        let mode = self.limits.mode.unwrap_or(self.host.caps.best_mode());
        self.host.set_mode(mode);
        self.ultra = self.speed == Speed::Ultra;
        let info = loop {
            self.v18_failed = false;
            match self.try_bring_up() {
                Ok(info) => break info,
                // A failed 1.8 V switch or tuning: power-cycle (the next try
                // starts with it) and come up without UHS-I or HS200.
                Err(e) if self.ultra && (self.v18_failed || e == Error::Tuning) => {
                    self.host.platform.note(format_args!("bring-up at 1.8 V failed ({:?}); trying without", e));
                    self.ultra = false;
                }
                Err(e) => return Err(e),
            }
        };
        self.card = Some(info);
        Ok(self.card.as_ref().unwrap())
    }

    fn try_bring_up(&mut self) -> Result<CardInfo> {
        let h = &mut self.host;
        h.power_cycle()?;
        h.set_width(1);
        h.set_timing(Timing::Legacy);
        h.set_clock(400_000)?;
        // 74 clocks before the first command (and some slack for the card).
        h.platform.delay_us(1_000);
        self.cmd(0, 0, Resp::None)?;
        self.host.platform.delay_us(1_000);
        let v2 = match self.cmd(8, 0x1AA, Resp::R1) {
            Ok(r) if r & 0xFFF == 0x1AA => true,
            Ok(_) => return Err(Error::Unsupported),
            Err(Error::Timeout) | Err(Error::Crc) => false,
            Err(e) => return Err(e),
        };
        match self.sd_power_up(v2) {
            Ok(ocr) => self.bring_up_sd(ocr),
            Err(Error::Timeout) | Err(Error::Crc) => {
                self.cmd(0, 0, Resp::None)?;
                self.host.platform.delay_us(1_000);
                self.bring_up_mmc()
            }
            Err(e) => Err(e),
        }
    }

    /// ACMD41 until the card reports power-up; its OCR.
    fn sd_power_up(&mut self, v2: bool) -> Result<u32> {
        let caps = self.host.caps;
        let s18r = v2
            && self.ultra
            && caps.v18
            && caps.version >= crate::regs::version::V3
            && (caps.sdr50 || caps.sdr104 || caps.ddr50);
        // 2.7-3.6 V; high capacity and 1.8 V (2.00 cards); maximum performance.
        let arg = 0x00FF_8000 | if v2 { 1 << 30 | 1 << 28 } else { 0 } | if s18r { 1 << 24 } else { 0 };
        let deadline = self.host.platform.now_us() + INIT_TIMEOUT_US;
        loop {
            let ocr = self.app_cmd(0, &Cmd::new(41, arg, Resp::R3))?[0];
            if ocr & 1 << 31 != 0 {
                return Ok(if s18r { ocr } else { ocr & !(1 << 24) });
            }
            if self.host.platform.now_us() > deadline {
                return Err(Error::Unsupported);
            }
            self.host.platform.delay_us(10_000);
        }
    }

    /// CMD11 and the switch to 1.8 V signalling (Physical Layer 3.01, 4.2.4.2).
    fn sd_voltage_switch(&mut self) -> Result<()> {
        self.cmd(11, 0, Resp::R1)?;
        let h = &mut self.host;
        h.card_clock(false);
        if h.dat_level() != 0 {
            return Err(Error::Unsupported);
        }
        h.set_signal_voltage(true).map_err(|_| Error::Unsupported)?;
        h.card_clock(true);
        h.platform.delay_us(1_000);
        if h.dat_level() != 0xF {
            return Err(Error::Unsupported);
        }
        Ok(())
    }

    fn bring_up_sd(&mut self, ocr: u32) -> Result<CardInfo> {
        if ocr & 1 << 24 != 0 && self.ultra {
            if let Err(e) = self.sd_voltage_switch() {
                self.host.platform.note(format_args!("SD: the 1.8 V switch failed ({:?})", e));
                self.v18_failed = true;
                return Err(e);
            }
        }
        let cid = Cid::parse_sd(&cr::r2_bytes(self.host.command(&Cmd::new(2, 0, Resp::R2))?));
        let rca = (self.cmd(3, 0, Resp::R1)? >> 16) as u16;
        let csd = Csd::parse_sd(&cr::r2_bytes(self.host.command(&Cmd::new(9, (rca as u32) << 16, Resp::R2))?));
        self.host.command(&Cmd::new(7, (rca as u32) << 16, Resp::R1b))?;
        let hc = ocr & 1 << 30 != 0;
        let sectors = csd.capacity / SECTOR as u64;
        let kind = if !hc {
            CardKind::Sdsc
        } else if csd.capacity > 32 << 30 {
            CardKind::Sdxc
        } else {
            CardKind::Sdhc
        };
        if !hc {
            self.cmd(16, SECTOR, Resp::R1)?;
        }
        let scr = Scr::parse(&self.read_block(Cmd::new(51, 0, Resp::R1), 8, Some(rca))?.try_into().unwrap());
        let mut info = CardInfo {
            kind,
            rca,
            ocr,
            cid,
            sectors,
            block_addressed: hc,
            cmd23: scr.cmd23,
            width: 1,
            timing: Timing::Legacy,
            clock_hz: 0,
            signal_1v8: self.host.signal_1v8,
            read_only: csd.write_protected || self.host.write_protect_switch(),
            trim: false,
            erase_group: 1,
            cache: false,
            boot_bytes: 0,
            rpmb_bytes: 0,
            ext_csd_rev: 0,
            erased_ones: scr.erased_ones,
        };
        self.card = Some(info.clone());
        if scr.bus4 {
            self.app_cmd(rca, &Cmd::new(6, 2, Resp::R1))?;
            self.host.set_width(4);
            info.width = 4;
        }
        let switch_ok = scr.spec >= 1 && csd.ccc & 1 << 10 != 0;
        let caps = self.host.caps;
        if self.host.signal_1v8 && switch_ok {
            // UHS-I: the fastest mode both support (group 1).
            let st = self.sd_switch(false, 1, 0xF)?;
            let pick = [
                (3, Timing::UhsSdr104, caps.sdr104, 208_000_000),
                (2, Timing::UhsSdr50, caps.sdr50, 100_000_000),
                (4, Timing::UhsDdr50, caps.ddr50, 50_000_000),
                (1, Timing::UhsSdr25, true, 50_000_000),
            ]
            .into_iter()
            .find(|&(f, _, host, _)| host && st.support[0] & 1 << f != 0);
            if let Some((f, timing, _, hz)) = pick {
                let st = self.sd_switch(true, 1, f)?;
                if st.selected[0] as u32 == f {
                    self.host.set_timing(timing);
                    info.clock_hz = self.host.set_clock(hz.min(self.limits.max_clock_hz))?;
                    info.timing = timing;
                    if timing == Timing::UhsSdr104 || (timing == Timing::UhsSdr50 && caps.tuning_sdr50) {
                        self.host.tune(19, 64)?;
                        self.last_tune_us = self.host.platform.now_us();
                    }
                }
            }
        } else if switch_ok && caps.high_speed && self.speed >= Speed::High {
            let st = self.sd_switch(false, 1, 1)?;
            if st.support[0] & 0x2 != 0 {
                let st = self.sd_switch(true, 1, 1)?;
                if st.selected[0] == 1 {
                    self.host.set_timing(Timing::SdHs);
                    info.timing = Timing::SdHs;
                    info.clock_hz = self.host.set_clock(50_000_000.min(self.limits.max_clock_hz))?;
                }
            }
        }
        if info.clock_hz == 0 {
            let hz = if self.speed == Speed::Slow { 12_500_000 } else { 25_000_000 };
            info.clock_hz = self.host.set_clock(hz.min(self.limits.max_clock_hz))?;
        }
        info.signal_1v8 = self.host.signal_1v8;
        Ok(info)
    }

    /// CMD6 for SD: query or switch one function group.
    fn sd_switch(&mut self, set: bool, group: u32, function: u32) -> Result<SwitchStatus> {
        let b = self.read_block(Cmd::new(6, cr::sd_switch_arg(set, group, function), Resp::R1), 64, None)?;
        Ok(SwitchStatus::parse(&b))
    }

    fn bring_up_mmc(&mut self) -> Result<CardInfo> {
        // Sector mode, 2.7-3.6 V, and 1.70-1.95 V when the slot has it.
        let arg = 1 << 30 | 0x00FF_8000 | if self.host.caps.v18 { 1 << 7 } else { 0 };
        let deadline = self.host.platform.now_us() + INIT_TIMEOUT_US;
        let ocr = loop {
            let ocr = self.cmd(1, arg, Resp::R3)?;
            if ocr & 1 << 31 != 0 {
                break ocr;
            }
            if self.host.platform.now_us() > deadline {
                return Err(Error::Unsupported);
            }
            self.host.platform.delay_us(10_000);
        };
        let hc = (ocr >> 29) & 3 == 2;
        let cid = Cid::parse_mmc(&cr::r2_bytes(self.host.command(&Cmd::new(2, 0, Resp::R2))?));
        let rca: u16 = 1;
        self.cmd(3, (rca as u32) << 16, Resp::R1)?;
        let csd = Csd::parse_mmc(&cr::r2_bytes(self.host.command(&Cmd::new(9, (rca as u32) << 16, Resp::R2))?));
        self.host.command(&Cmd::new(7, (rca as u32) << 16, Resp::R1b))?;
        let mut info = CardInfo {
            kind: if hc { CardKind::MmcHc } else { CardKind::Mmc },
            rca,
            ocr,
            cid,
            sectors: csd.capacity / SECTOR as u64,
            block_addressed: hc,
            cmd23: true,
            width: 1,
            timing: Timing::Legacy,
            clock_hz: 0,
            signal_1v8: false,
            read_only: csd.write_protected,
            trim: false,
            erase_group: csd.erase_group.max(1),
            cache: false,
            boot_bytes: 0,
            rpmb_bytes: 0,
            ext_csd_rev: 0,
            erased_ones: false,
        };
        self.card = Some(info.clone());
        if !hc {
            self.cmd(16, SECTOR, Resp::R1)?;
        }
        if csd.spec_vers < 4 {
            // MMC before 4.0: no EXT_CSD, no wide bus or high speed.
            info.clock_hz = self.host.set_clock(20_000_000.min(self.limits.max_clock_hz))?;
            return Ok(info);
        }
        // EXT_CSD at default speed first (faster modes come after).
        self.host.set_clock(20_000_000.min(self.limits.max_clock_hz))?;
        let raw = self.read_block(Cmd::new(8, 0, Resp::R1), 512, None)?;
        let ext = ExtCsd::parse(&raw);
        info.ext_csd_rev = ext.rev;
        info.boot_bytes = ext.boot_bytes;
        info.rpmb_bytes = ext.rpmb_bytes;
        info.erased_ones = ext.erased_ones;
        info.trim = ext.trim;
        if hc {
            info.sectors = ext.sectors;
        }
        // The user area, never a boot or RPMB partition.
        if ext.partition_config & 7 != 0 {
            self.mmc_switch(ext::PARTITION_CONFIG, ext.partition_config & !7, ext.partition_switch_us)?;
        }
        if hc && ext.hc_erase_group_sectors != 0 && ext.rev >= 3 {
            self.mmc_switch(ext::ERASE_GROUP_DEF, 1, ext.cmd6_us)?;
            info.erase_group = ext.hc_erase_group_sectors;
        }
        if ext.cache_bytes > 0 && self.mmc_switch(ext::CACHE_CTRL, 1, ext.cmd6_us).is_ok() {
            info.cache = true;
        }
        info.width = self.mmc_bus_width(&raw, &ext)?;
        self.mmc_timing(&ext, &mut info)?;
        info.signal_1v8 = self.host.signal_1v8;
        Ok(info)
    }

    /// CMD6 (write byte), its busy wait, and the status check.
    fn mmc_switch(&mut self, index: usize, value: u8, busy_us: u64) -> Result<()> {
        self.mmc_switch_raw(index, value, busy_us)?;
        self.check_switch()
    }

    fn mmc_switch_raw(&mut self, index: usize, value: u8, busy_us: u64) -> Result<()> {
        self.host.command(&Cmd::new(6, cr::mmc_switch_arg(index, value), Resp::R1b).busy(busy_us))?;
        Ok(())
    }

    fn check_switch(&mut self) -> Result<()> {
        let st = self.wait_ready(1_000_000)?;
        r1_check(st, status::SWITCH_ERROR | status::ERROR).map(|_| ())
    }

    /// The widest bus that reads `EXT_CSD` back unchanged.
    fn mmc_bus_width(&mut self, reference: &[u8], ext: &ExtCsd) -> Result<u8> {
        let mut widths = Vec::new();
        if self.host.caps.bus8 && self.limits.bus8 {
            widths.push((8, ext::WIDTH_8));
        }
        widths.push((4, ext::WIDTH_4));
        for (w, value) in widths {
            if self.mmc_switch(ext::BUS_WIDTH, value, ext.cmd6_us).is_err() {
                continue;
            }
            self.host.set_width(w);
            // The read-only part (properties segment) must match.
            match self.read_block(Cmd::new(8, 0, Resp::R1), 512, None) {
                Ok(b) if b[192..] == reference[192..] => return Ok(w),
                _ => {
                    self.host.platform.note(format_args!("eMMC: the {}-bit bus does not work", w));
                    self.host.reset_lines(true, true);
                }
            }
        }
        self.mmc_switch(ext::BUS_WIDTH, ext::WIDTH_1, ext.cmd6_us)?;
        self.host.set_width(1);
        Ok(1)
    }

    fn mmc_timing(&mut self, ext: &ExtCsd, info: &mut CardInfo) -> Result<()> {
        let caps = self.host.caps;
        let dt = ext.device_type;
        let hs200 = self.ultra
            && caps.version >= crate::regs::version::V3
            && caps.sdr104
            && caps.v18
            && dt & ext::HS200_18 != 0
            && info.width >= 4;
        let hs400 = hs200 && self.limits.hs400 && caps.hs400 && dt & ext::HS400_18 != 0 && info.width == 8;
        if hs200 {
            match self.mmc_hs200(ext, info, hs400) {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // The card is in an unknown timing now: the caller's
                    // fallback brings it up again without HS200.
                    self.host.platform.note(format_args!("eMMC: HS200 failed ({:?})", e));
                    return Err(Error::Tuning);
                }
            }
        }
        if self.speed >= Speed::High && caps.high_speed && dt & (ext::HS52 | ext::HS26) != 0 {
            self.mmc_switch_raw(ext::HS_TIMING, ext::TIMING_HS, ext.cmd6_us)?;
            self.host.set_timing(Timing::MmcHs);
            let hz = if dt & ext::HS52 != 0 { 52_000_000 } else { 26_000_000 };
            info.clock_hz = self.host.set_clock(hz.min(self.limits.max_clock_hz))?;
            self.check_switch()?;
            info.timing = Timing::MmcHs;
            return Ok(());
        }
        let hz = if self.speed == Speed::Slow { 12_500_000 } else { 20_000_000 };
        info.clock_hz = self.host.set_clock(hz.min(self.limits.max_clock_hz))?;
        Ok(())
    }

    /// HS200 (JESD84-B51 6.6.5), then HS400 (6.6.2.3) when asked.
    fn mmc_hs200(&mut self, ext: &ExtCsd, info: &mut CardInfo, hs400: bool) -> Result<()> {
        self.host.set_signal_voltage(true)?;
        self.mmc_switch_raw(ext::HS_TIMING, ext::TIMING_HS200, ext.cmd6_us)?;
        self.host.set_timing(Timing::Hs200);
        info.clock_hz = self.host.set_clock(200_000_000.min(self.limits.max_clock_hz))?;
        self.check_switch()?;
        let block = if info.width == 8 { 128 } else { 64 };
        self.host.tune(21, block)?;
        self.last_tune_us = self.host.platform.now_us();
        info.timing = Timing::Hs200;
        if !hs400 {
            return Ok(());
        }
        // HS400: back to high speed at 52 MHz, 8-bit DDR, then HS400 timing.
        self.mmc_switch_raw(ext::HS_TIMING, ext::TIMING_HS, ext.cmd6_us)?;
        self.host.set_timing(Timing::MmcHs);
        self.host.set_clock(52_000_000)?;
        self.check_switch()?;
        self.mmc_switch(ext::BUS_WIDTH, ext::WIDTH_8_DDR, ext.cmd6_us)?;
        self.mmc_switch_raw(ext::HS_TIMING, ext::TIMING_HS400, ext.cmd6_us)?;
        self.host.set_timing(Timing::Hs400);
        info.clock_hz = self.host.set_clock(200_000_000.min(self.limits.max_clock_hz))?;
        self.check_switch()?;
        info.timing = Timing::Hs400;
        Ok(())
    }

    /// CMD13: the card's status.
    pub fn status(&mut self) -> Result<u32> {
        let rca = self.rca_arg();
        self.cmd(13, rca, Resp::R1)
    }

    /// Waits until the card is back in the transfer state, ready for data.
    fn wait_ready(&mut self, us: u64) -> Result<u32> {
        let deadline = self.host.platform.now_us() + us;
        loop {
            let st = self.status()?;
            if st & status::READY_FOR_DATA != 0 && status::state(st) == status::STATE_TRAN {
                return Ok(st);
            }
            if self.host.platform.now_us() > deadline {
                return Err(Error::DataTimeout);
            }
            self.host.platform.delay_us(200);
        }
    }

    /// Sectors per transfer (bounded by the bounce buffer).
    pub fn max_sectors(&self) -> u32 {
        ((self.host.buffer().len() / SECTOR as usize) as u32).min(0xFFFF)
    }

    fn address(&self, info: &CardInfo, lba: u64) -> Result<u32> {
        let a = if info.block_addressed { lba } else { lba * SECTOR as u64 };
        u32::try_from(a).map_err(|_| Error::OutOfRange)
    }

    /// One attempt at a read or write of `count` sectors through the buffer.
    fn rw_once(&mut self, lba: u64, count: u32, write: bool) -> Result<()> {
        let info = self.card.clone().ok_or(Error::NoCard)?;
        if write && info.read_only {
            return Err(Error::ReadOnly);
        }
        if count == 0 || lba + count as u64 > info.sectors || count > self.max_sectors() {
            return Err(Error::OutOfRange);
        }
        if info.timing.tuned() && self.host.caps.retune_s != 0 {
            let due = self.last_tune_us + self.host.caps.retune_s as u64 * 1_000_000;
            if self.host.platform.now_us() > due {
                self.retune()?;
            }
        }
        let addr = self.address(&info, lba)?;
        let multi = count > 1;
        let index = match (write, multi) {
            (false, false) => 17,
            (false, true) => 18,
            (true, false) => 24,
            (true, true) => 25,
        };
        if multi && info.cmd23 {
            self.cmd(23, count, Resp::R1)?;
        }
        let data = Data { block_size: SECTOR, blocks: count, write, auto_cmd12: multi && !info.cmd23 };
        let r1 = self.host.command(&Cmd::new(index, addr, Resp::R1).with_data(data).busy(WRITE_BUSY_US))?[0];
        r1_check(r1, status::DATA_ERRORS)?;
        if data.auto_cmd12 {
            r1_check(self.host.auto_cmd12_response(), status::DATA_ERRORS & !status::OUT_OF_RANGE)?;
        }
        if write {
            let st = self.wait_ready(WRITE_BUSY_US)?;
            r1_check(st, status::DATA_ERRORS)?;
        }
        Ok(())
    }

    /// Brings the card back to the transfer state after a failed command.
    fn recover(&mut self) {
        self.host.reset_lines(true, true);
        let state = self.status().map(status::state);
        if !matches!(state, Ok(status::STATE_TRAN)) {
            // Still sending or receiving (or unknown): stop it.
            let _ = self.host.command(&Cmd::new(12, 0, Resp::R1b).busy(WRITE_BUSY_US));
            self.host.reset_lines(true, true);
        }
        let _ = self.wait_ready(WRITE_BUSY_US);
    }

    /// Re-runs tuning at the current timing.
    fn retune(&mut self) -> Result<()> {
        let Some(info) = self.card.as_ref() else { return Err(Error::NoCard) };
        let (index, block) = if info.kind.is_mmc() { (21, if info.width == 8 { 128 } else { 64 }) } else { (19, 64) };
        if info.timing == Timing::Hs400 {
            // HS400 can't tune; a fresh bring-up re-tunes in HS200 first.
            return self.fallback(false);
        }
        self.host.tune(index, block)?;
        self.last_tune_us = self.host.platform.now_us();
        Ok(())
    }

    /// Brings the card up again; `slower`: one speed step lower.
    fn fallback(&mut self, slower: bool) -> Result<()> {
        if slower {
            // One step below what the card runs now.
            let now = match self.card.as_ref().map(|c| c.timing) {
                Some(Timing::Legacy) => Speed::Default,
                Some(Timing::SdHs | Timing::MmcHs) => Speed::High,
                _ => Speed::Ultra,
            };
            self.speed = self.speed.min(match now {
                Speed::Ultra => Speed::High,
                Speed::High => Speed::Default,
                _ => Speed::Slow,
            });
        }
        self.fallbacks += 1;
        let before = self.card.as_ref().map(|c| (c.sectors, c.cid.serial));
        let after = self.bring_up().map(|c| (c.sectors, c.cid.serial))?;
        if before.is_some_and(|b| b != after) {
            // A different card: whoever had the old one must find out.
            self.card = None;
            return Err(Error::NoCard);
        }
        let s = self.card.as_ref().unwrap().summary();
        self.host.platform.note(format_args!("brought up again: {}", s));
        Ok(())
    }

    /// Runs `op` with recovery and fallbacks.
    fn with_recovery(&mut self, mut op: impl FnMut(&mut Self) -> Result<()>) -> Result<()> {
        let mut attempt = 0;
        loop {
            let e = match op(self) {
                Ok(()) => return Ok(()),
                Err(Error::NoCard) => {
                    self.card = None;
                    return Err(Error::NoCard);
                }
                Err(e @ (Error::ReadOnly | Error::OutOfRange | Error::Unsupported)) => return Err(e),
                Err(e) => e,
            };
            attempt += 1;
            self.retries += 1;
            if !self.host.card_present() {
                self.card = None;
                return Err(Error::NoCard);
            }
            self.host.platform.note(format_args!("I/O failed ({:?}), attempt {}", e, attempt));
            if attempt >= 4 {
                return Err(e);
            }
            self.recover();
            let tuned = self.card.as_ref().is_some_and(|c| c.timing.tuned());
            if attempt == 2 && e.is_signal() && tuned {
                if self.retune().is_err() {
                    self.fallback(true)?;
                }
            } else if attempt == 3 && e.is_signal() {
                self.fallback(true)?;
            }
        }
    }

    /// Reads `count` sectors at `lba` into the start of the bounce buffer.
    pub fn read(&mut self, lba: u64, count: u32) -> Result<()> {
        self.with_recovery(|s| s.rw_once(lba, count, false))
    }

    /// Writes `count` sectors at `lba` from the start of the bounce buffer.
    /// The buffer is not touched, so retries resend the same data.
    pub fn write(&mut self, lba: u64, count: u32) -> Result<()> {
        self.with_recovery(|s| s.rw_once(lba, count, true))
    }

    /// Discards or erases `count` sectors at `lba`. Returns how many were
    /// (eMMC without TRIM erases whole erase groups inside the range only).
    pub fn erase(&mut self, lba: u64, count: u64, kind: EraseKind) -> Result<u64> {
        let info = self.card.clone().ok_or(Error::NoCard)?;
        if info.read_only {
            return Err(Error::ReadOnly);
        }
        if count == 0 {
            return Ok(0);
        }
        if lba + count > info.sectors {
            return Err(Error::OutOfRange);
        }
        let (start, end, arg) = if !info.kind.is_mmc() {
            (lba, lba + count - 1, 0)
        } else if info.trim && kind == EraseKind::Discard {
            (lba, lba + count - 1, 1)
        } else {
            let g = info.erase_group.max(1) as u64;
            let first = lba.div_ceil(g) * g;
            let last = (lba + count) / g * g;
            if last <= first {
                return Ok(0);
            }
            (first, last - 1, 0)
        };
        let (a, b) = (self.address(&info, start)?, self.address(&info, end)?);
        let (cmd_start, cmd_end) = if info.kind.is_mmc() { (35, 36) } else { (32, 33) };
        self.with_recovery(|s| {
            r1_check(s.cmd(cmd_start, a, Resp::R1)?, status::ERASE_ERRORS)?;
            r1_check(s.cmd(cmd_end, b, Resp::R1)?, status::ERASE_ERRORS)?;
            // Erase time grows with the range; this covers large ones.
            let busy = 2_000_000 + (end - start + 1) / 2048 * 10_000;
            let r1 = s.host.command(&Cmd::new(38, arg, Resp::R1b).busy(busy.min(60_000_000)))?[0];
            r1_check(r1, status::ERASE_ERRORS)?;
            s.wait_ready(busy.min(60_000_000))?;
            Ok(())
        })?;
        Ok(end - start + 1)
    }

    /// Makes written data durable: flushes an eMMC's cache.
    pub fn flush(&mut self) -> Result<()> {
        let info = self.card.clone().ok_or(Error::NoCard)?;
        self.with_recovery(|s| {
            if info.cache {
                s.mmc_switch(ext::FLUSH_CACHE, 1, 30_000_000)?;
            }
            s.wait_ready(WRITE_BUSY_US).map(|_| ())
        })
    }

    /// The card went away: forget it and power the slot down.
    pub fn remove(&mut self) {
        self.card = None;
        self.host.power_off();
    }

    /// Reads `n` sectors at `lba` into `out` (several transfers if needed).
    pub fn read_into(&mut self, lba: u64, out: &mut [u8]) -> Result<()> {
        let per = self.max_sectors() as usize;
        for (i, chunk) in out.chunks_mut(per * SECTOR as usize).enumerate() {
            let n = chunk.len() / SECTOR as usize;
            self.read(lba + (i * per) as u64, n as u32)?;
            chunk.copy_from_slice(&self.host.buffer()[..chunk.len()]);
        }
        Ok(())
    }

    /// Writes `data` (whole sectors) at `lba`.
    pub fn write_from(&mut self, lba: u64, data: &[u8]) -> Result<()> {
        let per = self.max_sectors() as usize;
        for (i, chunk) in data.chunks(per * SECTOR as usize).enumerate() {
            let n = chunk.len() / SECTOR as usize;
            self.host.buffer_mut()[..chunk.len()].copy_from_slice(chunk);
            self.write(lba + (i * per) as u64, n as u32)?;
        }
        Ok(())
    }
}

/// A buffer of `n` sectors (tests and callers without their own).
pub fn sectors(n: usize) -> Vec<u8> {
    vec![0; n * SECTOR as usize]
}

/// Whether a slot is meant for removable cards.
pub fn removable(slot: SlotType) -> bool {
    slot == SlotType::Removable
}
