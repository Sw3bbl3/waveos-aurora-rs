//! An SD Host Controller with a card in it, in software.
//!
//! The controller follows the specification's register behaviour closely
//! enough to catch the driver's mistakes: commands start on the write of
//! the Command register's upper byte; statuses latch only when enabled and
//! clear when written with ones; PIO goes a block at a time through the
//! Buffer Data Port; SDMA stops at its buffer boundary until the address is
//! written again; ADMA2 walks 32- or 64-bit descriptor tables (an invalid
//! descriptor is an ADMA error); Auto CMD12 ends multi-block transfers;
//! Execute Tuning runs until the sampling point is found.
//!
//! The cards follow the SD Physical Layer and JESD84 state machines:
//! identification at no more than 400 kHz, ACMD41/CMD1 power-up, the CMD11
//! voltage switch with its DAT-line handshake, CMD6 switch functions and
//! `EXT_CSD` writes, UHS-I and HS200/HS400 that only work at 1.8 V and once
//! tuned, CMD23, erase and TRIM, an eMMC cache, boot and RPMB partitions.
//!
//! What the driver gets wrong goes into [`Mock::violations`] (and the
//! command usually fails); [`Mock::inject`] makes a correct driver meet the
//! errors real cards produce. DMA reaches memory through a translation from
//! "physical" addresses that the user supplies (identity on the host, the
//! kernel's direct map in its self-test). It is a model, not the silicon.

use crate::card_regs::{ext, mmc_switch_arg, status, TUNING_4BIT, TUNING_8BIT};
use crate::regs::{self, adma, clk, cmd, err, hc, hc2, int, power, ps, reset, tm};
use crate::Regs;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, Ordering};

/// Turns a DMA address into a pointer the simulation can use.
pub type Translate = fn(u64) -> *mut u8;

/// An error to produce.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// The card does not answer.
    CmdTimeout,
    /// The response arrives with a bad CRC (the card did act on the command).
    CmdCrc,
    /// A data block arrives (or is sent) corrupt after this many good ones.
    DataCrc { after: u32 },
    /// Data stops coming after this many blocks.
    DataTimeout { after: u32 },
    /// The ADMA engine fails on the first descriptor.
    Adma,
    /// Tuning never finds a sampling point.
    TuningFails,
}

/// A fault for the next `count` matching commands (`cmd` None: any).
#[derive(Clone, Copy, Debug)]
pub struct Injection {
    pub cmd: Option<u8>,
    pub fault: Fault,
    pub count: u32,
}

/// What kind of card, and what it can do.
#[derive(Clone, Debug)]
pub struct CardSpec {
    pub emmc: bool,
    /// SDHC/SDXC, or a sector-addressed eMMC.
    pub high_capacity: bool,
    pub sectors: u64,
    /// SD: accepts 1.8 V and offers SDR50, SDR104 and DDR50.
    pub uhs: bool,
    /// SD: CMD23 in the SCR.
    pub cmd23: bool,
    pub hs200: bool,
    pub hs400: bool,
    pub trim: bool,
    pub cache_kib: u32,
    pub boot_kib: u32,
    pub rpmb_kib: u32,
    /// Tuning blocks needed to find the sampling point.
    pub tuning_steps: u32,
    /// The card comes up with this partition selected (firmware left it so).
    pub start_partition: u8,
    pub perm_write_protect: bool,
}

impl CardSpec {
    pub fn sdhc(sectors: u64) -> CardSpec {
        CardSpec {
            emmc: false,
            high_capacity: true,
            sectors,
            uhs: false,
            cmd23: false,
            hs200: false,
            hs400: false,
            trim: false,
            cache_kib: 0,
            boot_kib: 0,
            rpmb_kib: 0,
            tuning_steps: 6,
            start_partition: 0,
            perm_write_protect: false,
        }
    }
    pub fn sdsc(sectors: u64) -> CardSpec {
        CardSpec { high_capacity: false, ..CardSpec::sdhc(sectors) }
    }
    pub fn uhs(sectors: u64) -> CardSpec {
        CardSpec { uhs: true, cmd23: true, ..CardSpec::sdhc(sectors) }
    }
    pub fn emmc(sectors: u64) -> CardSpec {
        CardSpec {
            emmc: true,
            high_capacity: true,
            hs200: true,
            hs400: true,
            trim: true,
            cache_kib: 512,
            boot_kib: 512,
            rpmb_kib: 128,
            ..CardSpec::sdhc(sectors)
        }
    }
}

/// Card states (R1 bits 12:9).
const IDLE: u32 = 0;
const READY: u32 = 1;
const IDENT: u32 = 2;
const STBY: u32 = 3;
const TRAN: u32 = 4;
const DATA: u32 = 5;
const RCV: u32 = 6;

enum Reply {
    /// No response.
    None,
    R1(u32),
    R2([u8; 16]),
    R3(u32),
}

#[derive(Clone)]
enum Phase {
    None,
    /// A fixed block the card sends (registers, status, tuning).
    Fixed(Vec<u8>),
    /// Sectors from partition `part` starting at `sector`; `count` if bounded.
    Read { part: usize, sector: u64, count: Option<u32> },
    Write { part: usize, sector: u64, count: Option<u32> },
}

pub struct Card {
    pub spec: CardSpec,
    /// User area, boot 1, boot 2, RPMB.
    pub parts: [Vec<u8>; 4],
    state: u32,
    rca: u16,
    app: bool,
    polls: u32,
    s18a: bool,
    /// CMD11 accepted: DAT lines held low until the host is at 1.8 V.
    switching: bool,
    pub v18: bool,
    width: u8,
    /// SD: the group 1 function; eMMC: HS_TIMING.
    pub timing: u8,
    switch_to: Option<u8>,
    block_count: Option<u32>,
    /// Error bits for the next R1.
    pending: u32,
    erase: (Option<u64>, Option<u64>),
    phase: Phase,
    pub ext: [u8; 512],
    pub dirty: bool,
    pub flushes: u32,
    /// Writes and switches that touched boot or RPMB partitions.
    pub hidden_touches: u32,
    pub erased: Vec<(u64, u64, u32)>,
}

impl Card {
    pub fn new(spec: CardSpec) -> Card {
        let bytes = spec.sectors as usize * 512;
        let mut c = Card {
            parts: [
                vec![0; bytes],
                vec![0xB1; spec.boot_kib as usize * 1024],
                vec![0xB2; spec.boot_kib as usize * 1024],
                vec![0xEE; spec.rpmb_kib as usize * 1024],
            ],
            state: IDLE,
            rca: 0,
            app: false,
            polls: 0,
            s18a: false,
            switching: false,
            v18: false,
            width: 1,
            timing: 0,
            switch_to: None,
            block_count: None,
            pending: 0,
            erase: (None, None),
            phase: Phase::None,
            ext: [0; 512],
            dirty: false,
            flushes: 0,
            hidden_touches: 0,
            erased: Vec::new(),
            spec,
        };
        c.power_on();
        c
    }

    fn power_on(&mut self) {
        self.reset();
        self.v18 = false;
        self.switching = false;
        let s = &self.spec;
        let mut e = [0u8; 512];
        e[ext::REV] = 8;
        e[ext::DEVICE_TYPE] = ext::HS26
            | ext::HS52
            | if s.hs200 { ext::HS200_18 } else { 0 }
            | if s.hs400 { ext::HS400_18 } else { 0 };
        e[ext::SEC_COUNT..ext::SEC_COUNT + 4].copy_from_slice(&(s.sectors as u32).to_le_bytes());
        e[ext::HC_ERASE_GRP_SIZE] = 1;
        e[ext::BOOT_SIZE_MULT] = (s.boot_kib / 128) as u8;
        e[ext::RPMB_SIZE_MULT] = (s.rpmb_kib / 128) as u8;
        e[ext::SEC_FEATURE_SUPPORT] = if s.trim { ext::SEC_GB_CL_EN } else { 0 };
        e[ext::GENERIC_CMD6_TIME] = 10;
        e[ext::PARTITION_SWITCH_TIME] = 10;
        e[ext::CACHE_SIZE..ext::CACHE_SIZE + 4].copy_from_slice(&s.cache_kib.to_le_bytes());
        e[ext::PARTITION_CONFIG] = s.start_partition;
        self.ext = e;
    }

    /// CMD0, or power-on.
    fn reset(&mut self) {
        self.state = IDLE;
        self.rca = 0;
        self.app = false;
        self.polls = 0;
        self.s18a = false;
        self.width = 1;
        self.timing = 0;
        self.block_count = None;
        self.pending = 0;
        self.phase = Phase::None;
        self.ext[ext::HS_TIMING] = 0;
        self.ext[ext::BUS_WIDTH] = 0;
    }

    fn r1(&mut self, before: u32) -> Reply {
        let mut s = before << 9 | self.pending;
        if matches!(before, TRAN) || self.state == TRAN {
            s |= status::READY_FOR_DATA;
        }
        if self.app {
            s |= status::APP_CMD;
        }
        self.pending = 0;
        Reply::R1(s)
    }

    fn illegal(&mut self) -> Reply {
        self.pending |= status::ILLEGAL_COMMAND;
        self.app = false;
        Reply::None
    }

    fn cid(&self) -> [u8; 16] {
        let mut c = [0u8; 16];
        c[0] = 0x42;
        if self.spec.emmc {
            c[1] = 1;
            c[3..9].copy_from_slice(b"MOCKMC");
            c[9] = 0x10;
            c[10..14].copy_from_slice(&0x1234_5678u32.to_be_bytes());
            c[14] = 0x3B;
        } else {
            c[1..3].copy_from_slice(b"WV");
            c[3..8].copy_from_slice(b"MOCK!");
            c[8] = 0x10;
            c[9..13].copy_from_slice(&0x1234_5678u32.to_be_bytes());
            c[13] = 0x01;
            c[14] = 0x95;
        }
        c
    }

    fn csd(&self) -> [u8; 16] {
        let mut c = [0u8; 16];
        let mut set = |hi: usize, lo: usize, v: u64| {
            for b in lo..=hi {
                let bit = (v >> (b - lo)) & 1;
                let byte = 15 - b / 8;
                c[byte] = (c[byte] & !(1 << (b % 8))) | (bit as u8) << (b % 8);
            }
        };
        let bytes = self.spec.sectors * 512;
        if self.spec.emmc {
            set(127, 126, 3);
            set(125, 122, 4);
            set(95, 84, 0x0F5);
            if bytes > 2 << 30 {
                set(73, 62, 0xFFF);
                set(49, 47, 7);
                set(83, 80, 9);
            } else {
                // 512-byte blocks, multiplier 512.
                set(83, 80, 9);
                set(49, 47, 7);
                set(73, 62, bytes / 512 / 512 - 1);
            }
            set(46, 42, 31);
            set(41, 37, 31);
        } else if self.spec.high_capacity {
            set(127, 126, 1);
            set(95, 84, 0x5B5);
            set(69, 48, bytes / (512 * 1024) - 1);
        } else {
            set(127, 126, 0);
            set(95, 84, 0x5F5);
            set(83, 80, 9);
            set(49, 47, 7);
            set(73, 62, bytes / 512 / 512 - 1);
        }
        if self.spec.perm_write_protect {
            set(13, 13, 1);
        }
        c
    }

    fn scr(&self) -> Vec<u8> {
        vec![0x02, 0x05, 0x80, if self.spec.cmd23 { 0x02 } else { 0 }, 0, 0, 0, 0]
    }

    /// Group 1 functions the card offers now.
    fn functions(&self) -> u16 {
        let mut f = 0b11;
        if self.spec.uhs && self.v18 {
            f |= 1 << 2 | 1 << 3 | 1 << 4;
        }
        f
    }

    fn switch_status(&mut self, arg: u32) -> Vec<u8> {
        let mut d = vec![0u8; 64];
        d[1] = 100;
        for g in 0..6 {
            let at = 2 + (5 - g) * 2;
            let support: u16 = if g == 0 { self.functions() } else { 1 };
            d[at..at + 2].copy_from_slice(&(0x8000 | support).to_be_bytes());
        }
        let set = arg & 1 << 31 != 0;
        for g in 0..6 {
            let want = (arg >> (4 * g)) & 0xF;
            let support = if g == 0 { self.functions() } else { 1 };
            let sel = if want == 0xF {
                if g == 0 {
                    self.timing as u32
                } else {
                    0
                }
            } else if support & 1 << want != 0 {
                if set && g == 0 {
                    // Takes effect once the status block has gone out.
                    self.switch_to = Some(want as u8);
                }
                want
            } else {
                0xF
            };
            let byte = 16 - g / 2;
            d[byte] |= (sel as u8) << (if g % 2 == 0 { 0 } else { 4 });
        }
        d
    }

    fn address(&self, arg: u32) -> u64 {
        if self.spec.high_capacity {
            arg as u64
        } else {
            arg as u64 / 512
        }
    }

    fn partition(&self) -> usize {
        if self.spec.emmc {
            (self.ext[ext::PARTITION_CONFIG] & 7).min(3) as usize
        } else {
            0
        }
    }

    fn part_sectors(&self, part: usize) -> u64 {
        self.parts[part].len() as u64 / 512
    }

    /// The data bus as the card sees it.
    pub fn bus_width(&self) -> u8 {
        if !self.spec.emmc {
            return self.width;
        }
        match self.ext[ext::BUS_WIDTH] {
            1 | 5 => 4,
            2 | 6 => 8,
            _ => 1,
        }
    }

    /// The fastest clock the card's current mode allows.
    pub fn max_clock(&self) -> u32 {
        if self.state <= IDENT {
            return 400_000;
        }
        if self.spec.emmc {
            match self.ext[ext::HS_TIMING] {
                0 => 26_000_000,
                1 => 52_000_000,
                _ => 200_000_000,
            }
        } else {
            match self.timing {
                0 => 25_000_000,
                1 | 4 => 50_000_000,
                2 => 100_000_000,
                _ => 208_000_000,
            }
        }
    }

    /// The current mode samples with a tuned clock.
    pub fn needs_tuning(&self) -> bool {
        if self.spec.emmc {
            self.ext[ext::HS_TIMING] >= 2
        } else {
            self.timing == 2 || self.timing == 3
        }
    }

    pub fn needs_1v8(&self) -> bool {
        if self.spec.emmc {
            self.ext[ext::HS_TIMING] >= 2
        } else {
            self.timing >= 2
        }
    }

    fn mmc_switch(&mut self, arg: u32) {
        let access = (arg >> 24) & 3;
        let index = ((arg >> 16) & 0xFF) as usize;
        let value = ((arg >> 8) & 0xFF) as u8;
        if access != 3 || index >= 192 {
            self.pending |= status::SWITCH_ERROR;
            return;
        }
        let ok = match index {
            ext::BUS_WIDTH => matches!(value, 0 | 1 | 2 | 5 | 6),
            ext::HS_TIMING => match value {
                0 | 1 => true,
                2 => self.spec.hs200 && self.bus_width() >= 4,
                3 => self.spec.hs400 && self.ext[ext::BUS_WIDTH] == 6,
                _ => false,
            },
            ext::PARTITION_CONFIG => {
                let p = value & 7;
                if p != 0 {
                    self.hidden_touches += 1;
                }
                p == 0 || (p <= 2 && self.spec.boot_kib > 0) || (p == 3 && self.spec.rpmb_kib > 0)
            }
            ext::FLUSH_CACHE => {
                if value & 1 != 0 {
                    self.dirty = false;
                    self.flushes += 1;
                }
                return;
            }
            ext::CACHE_CTRL | ext::ERASE_GROUP_DEF => true,
            _ => true,
        };
        if ok {
            self.ext[index] = value;
        } else {
            self.pending |= status::SWITCH_ERROR;
        }
    }

    fn erase_now(&mut self, arg: u32) {
        let (Some(a), Some(b)) = self.erase else {
            self.pending |= status::ERASE_SEQ_ERROR;
            return;
        };
        self.erase = (None, None);
        let part = self.partition();
        if b < a || b >= self.part_sectors(part) {
            self.pending |= status::ERASE_PARAM;
            return;
        }
        if self.spec.emmc && arg == 0 {
            // Whole erase groups only.
            // Both erase group definitions come to 512 KiB on this card.
            let g = 1024;
            if a % g != 0 || (b + 1) % g != 0 {
                self.pending |= status::ERASE_PARAM;
                return;
            }
        }
        if self.spec.emmc && arg == 1 && !self.spec.trim {
            self.pending |= status::ERASE_PARAM;
            return;
        }
        let fill = if self.spec.emmc { 0 } else { 0xFF };
        self.parts[part][a as usize * 512..(b as usize + 1) * 512].fill(fill);
        self.erased.push((a, b, arg));
    }

    /// One command; `None` for no response. Sets the data phase.
    fn command(&mut self, index: u8, arg: u32) -> Reply {
        let app = core::mem::take(&mut self.app);
        let before = self.state;
        let rca_ok = (arg >> 16) as u16 == self.rca;
        self.phase = Phase::None;
        let emmc = self.spec.emmc;
        if app && !emmc {
            match index {
                41 if matches!(self.state, IDLE | READY) => {
                    self.polls += 1;
                    let ready = self.polls >= 2;
                    let mut ocr = 0x00FF_8000;
                    if ready {
                        ocr |= 1 << 31;
                        if self.spec.high_capacity && arg & 1 << 30 != 0 {
                            ocr |= 1 << 30;
                        }
                        if self.spec.uhs && arg & 1 << 24 != 0 && arg & 1 << 30 != 0 {
                            ocr |= 1 << 24;
                            self.s18a = true;
                        }
                        self.state = READY;
                    }
                    return Reply::R3(ocr);
                }
                6 if self.state == TRAN => {
                    self.width = if arg & 3 == 2 { 4 } else { 1 };
                    return self.r1(before);
                }
                51 if self.state == TRAN => {
                    self.phase = Phase::Fixed(self.scr());
                    self.state = DATA;
                    return self.r1(before);
                }
                _ => {}
            }
        }
        match index {
            0 => {
                self.reset();
                if emmc {
                    self.ext[ext::PARTITION_CONFIG] = self.spec.start_partition;
                }
                Reply::None
            }
            1 if emmc && matches!(self.state, IDLE | READY) => {
                self.polls += 1;
                let mut ocr = 0x00FF_8080;
                if self.polls >= 2 {
                    ocr |= 1 << 31 | if self.spec.high_capacity { 2 << 29 } else { 0 };
                    self.state = READY;
                }
                Reply::R3(ocr)
            }
            2 if self.state == READY => {
                self.state = IDENT;
                Reply::R2(self.cid())
            }
            3 if matches!(self.state, IDENT | STBY) => {
                self.state = STBY;
                if emmc {
                    self.rca = (arg >> 16) as u16;
                    self.r1(before)
                } else {
                    self.rca = 0xB368;
                    Reply::R1((self.rca as u32) << 16 | before << 9)
                }
            }
            6 if self.state == TRAN => {
                if emmc {
                    self.mmc_switch(arg);
                    self.r1(before)
                } else {
                    self.phase = Phase::Fixed(self.switch_status(arg));
                    self.state = DATA;
                    self.r1(before)
                }
            }
            7 => {
                if rca_ok && self.state == STBY {
                    self.state = TRAN;
                    self.r1(before)
                } else if !rca_ok && matches!(self.state, TRAN | DATA) {
                    self.state = STBY;
                    Reply::None
                } else {
                    self.illegal()
                }
            }
            8 if !emmc && self.state == IDLE => {
                if arg & 0xF00 == 0x100 {
                    Reply::R1(arg & 0xFFF)
                } else {
                    Reply::None
                }
            }
            8 if emmc && self.state == TRAN => {
                self.phase = Phase::Fixed(self.ext.to_vec());
                self.state = DATA;
                self.r1(before)
            }
            9 if self.state == STBY && rca_ok => Reply::R2(self.csd()),
            11 if !emmc && self.state == READY && self.s18a => {
                self.switching = true;
                self.r1(before)
            }
            12 if matches!(self.state, DATA | RCV) => {
                self.state = TRAN;
                self.block_count = None;
                self.r1(before)
            }
            13 if rca_ok && self.state >= STBY => self.r1(before),
            16 if self.state == TRAN => {
                if arg != 512 {
                    self.pending |= status::BLOCK_LEN_ERROR;
                }
                self.r1(before)
            }
            17 | 18 | 24 | 25 if self.state == TRAN => {
                let part = self.partition();
                let sector = self.address(arg);
                if sector >= self.part_sectors(part) {
                    self.pending |= status::OUT_OF_RANGE;
                    return self.r1(before);
                }
                if part != 0 && (index == 24 || index == 25) {
                    self.hidden_touches += 1;
                }
                let count = match index {
                    17 | 24 => Some(1),
                    _ => self.block_count.take(),
                };
                if index >= 24 && self.spec.perm_write_protect {
                    self.pending |= status::WP_VIOLATION;
                    return self.r1(before);
                }
                if index >= 24 {
                    self.phase = Phase::Write { part, sector, count };
                    self.state = RCV;
                } else {
                    self.phase = Phase::Read { part, sector, count };
                    self.state = DATA;
                }
                self.r1(before)
            }
            19 if !emmc && self.state == TRAN && self.needs_tuning() => {
                self.phase = Phase::Fixed(TUNING_4BIT.to_vec());
                self.state = DATA;
                self.r1(before)
            }
            21 if emmc && self.state == TRAN && self.ext[ext::HS_TIMING] == 2 => {
                let p = if self.bus_width() == 8 { TUNING_8BIT.to_vec() } else { TUNING_4BIT.to_vec() };
                self.phase = Phase::Fixed(p);
                self.state = DATA;
                self.r1(before)
            }
            23 if self.state == TRAN && (emmc || self.spec.cmd23) => {
                self.block_count = Some(arg & 0xFFFF);
                self.r1(before)
            }
            32 | 35 if self.state == TRAN && (index == 35) == emmc => {
                self.erase.0 = Some(self.address(arg));
                self.r1(before)
            }
            33 | 36 if self.state == TRAN && (index == 36) == emmc => {
                self.erase.1 = Some(self.address(arg));
                self.r1(before)
            }
            38 if self.state == TRAN => {
                self.erase_now(arg);
                self.r1(before)
            }
            55 if !emmc && (rca_ok || self.state == IDLE) => {
                self.app = true;
                let r = self.r1(before);
                self.app = true;
                match r {
                    Reply::R1(s) => Reply::R1(s | status::APP_CMD),
                    r => r,
                }
            }
            _ => self.illegal(),
        }
    }

    /// The next block of the current read; None when there is no more.
    fn read_block(&mut self, size: usize) -> Option<Vec<u8>> {
        match &mut self.phase {
            Phase::Fixed(d) => {
                let mut b = core::mem::take(d);
                b.resize(size, 0);
                self.phase = Phase::None;
                self.state = TRAN;
                if let Some(t) = self.switch_to.take() {
                    self.timing = t;
                }
                Some(b)
            }
            Phase::Read { part, sector, count } => {
                let (p, s) = (*part, *sector);
                if s >= self.parts[p].len() as u64 / 512 || count.is_some_and(|c| c == 0) {
                    return None;
                }
                let b = self.parts[p][s as usize * 512..s as usize * 512 + size].to_vec();
                *sector += 1;
                if let Some(c) = count {
                    *c -= 1;
                    if *c == 0 {
                        self.phase = Phase::None;
                        self.state = TRAN;
                    }
                }
                Some(b)
            }
            _ => None,
        }
    }

    fn write_block(&mut self, data: &[u8]) -> bool {
        let Phase::Write { part, sector, count } = &mut self.phase else { return false };
        let (p, s) = (*part, *sector);
        if s >= self.parts[p].len() as u64 / 512 {
            self.pending |= status::OUT_OF_RANGE;
            return false;
        }
        self.parts[p][s as usize * 512..s as usize * 512 + 512].copy_from_slice(&data[..512]);
        if self.ext[ext::CACHE_CTRL] & 1 != 0 {
            self.dirty = true;
        }
        *sector += 1;
        if let Some(c) = count {
            *c -= 1;
            if *c == 0 {
                self.phase = Phase::None;
                self.state = TRAN;
            }
        }
        true
    }
}

struct Pio {
    write: bool,
    block_size: usize,
    left: u32,
    done: u32,
    buf: Vec<u8>,
    pos: usize,
    auto12: bool,
    fault: Option<Fault>,
}

struct Sdma {
    write: bool,
    block_size: usize,
    left: u32,
    done: u32,
    auto12: bool,
    fault: Option<Fault>,
}

struct Ctl {
    regs: [u8; 256],
    caps: u64,
    translate: Translate,
    card: Option<Card>,
    write_protect: bool,
    faults: Vec<Injection>,
    violations: Vec<String>,
    log: Vec<(u8, u32)>,
    cmd_busy: bool,
    dat_busy: bool,
    pio: Option<Pio>,
    sdma: Option<Sdma>,
    tuning: u32,
}

impl Ctl {
    fn r8(&self, o: usize) -> u8 {
        self.regs[o]
    }
    fn r16(&self, o: usize) -> u16 {
        u16::from_le_bytes([self.regs[o], self.regs[o + 1]])
    }
    fn r32(&self, o: usize) -> u32 {
        u32::from_le_bytes(self.regs[o..o + 4].try_into().unwrap())
    }
    fn w16(&mut self, o: usize, v: u16) {
        self.regs[o..o + 2].copy_from_slice(&v.to_le_bytes());
    }
    fn w32(&mut self, o: usize, v: u32) {
        self.regs[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }

    fn violation(&mut self, what: String) {
        self.violations.push(what);
    }

    fn raise(&mut self, bits: u16) {
        let s = self.r16(regs::INT_STATUS) | (bits & self.r16(regs::INT_ENABLE));
        self.w16(regs::INT_STATUS, s);
    }

    fn raise_err(&mut self, bits: u16) {
        let e = self.r16(regs::ERR_STATUS) | (bits & self.r16(regs::ERR_ENABLE));
        self.w16(regs::ERR_STATUS, e);
        let s = self.r16(regs::INT_STATUS);
        self.w16(regs::INT_STATUS, if e != 0 { s | int::ERROR } else { s & !int::ERROR });
    }

    fn present(&self) -> u32 {
        let mut p = 0;
        if self.cmd_busy {
            p |= ps::CMD_INHIBIT;
        }
        if self.dat_busy {
            p |= ps::DAT_INHIBIT | ps::DAT_ACTIVE;
        }
        if let Some(x) = &self.pio {
            p |= if x.write { ps::WRITE_ACTIVE | ps::BUFFER_WRITE } else { ps::READ_ACTIVE | ps::BUFFER_READ };
        }
        if let Some(c) = &self.card {
            p |= ps::CARD_INSERTED | ps::CARD_STABLE | ps::CARD_DETECT;
            let dat = if c.switching { 0 } else { 0xF };
            p |= dat << 20 | 1 << 24;
        } else {
            p |= ps::CARD_STABLE | 0xF << 20 | 1 << 24;
        }
        if !self.write_protect {
            p |= ps::WRITE_ENABLED;
        }
        p
    }

    fn version(&self) -> u8 {
        self.r8(regs::HOST_VERSION)
    }

    /// The card clock now running, in Hz (0: stopped).
    fn clock(&self) -> u32 {
        let c = self.r16(regs::CLOCK_CONTROL);
        if c & clk::CARD_ENABLE == 0 || c & clk::INT_ENABLE == 0 {
            return 0;
        }
        let base = ((self.caps >> 8) & 0xFF) as u32 * 1_000_000;
        let n = ((c >> 8) & 0xFF) as u32 | (((c >> 6) & 3) as u32) << 8;
        if self.version() >= regs::version::V3 {
            if n == 0 {
                base
            } else {
                base / (2 * n)
            }
        } else if n == 0 {
            base
        } else {
            base / (2 * n)
        }
    }

    fn powered(&self) -> bool {
        self.r8(regs::POWER_CONTROL) & power::ON != 0
    }

    fn host_v18(&self) -> bool {
        self.r16(regs::HOST_CONTROL2) & hc2::V18 != 0
    }

    fn take_fault(&mut self, index: u8, data: bool) -> Option<Fault> {
        let i = self.faults.iter().position(|f| {
            f.count > 0
                && f.cmd.is_none_or(|c| c == index)
                && match f.fault {
                    Fault::DataCrc { .. } | Fault::DataTimeout { .. } | Fault::Adma => data,
                    Fault::TuningFails => false,
                    _ => !data,
                }
        })?;
        self.faults[i].count -= 1;
        let f = self.faults[i].fault;
        if self.faults[i].count == 0 {
            self.faults.remove(i);
        }
        Some(f)
    }

    fn set_power(&mut self, v: u8) {
        let was = self.powered();
        let sel = v & 0xE;
        let ok = self.card.is_some()
            && match sel {
                power::V33 => self.caps & 1 << 24 != 0,
                power::V30 => self.caps & 1 << 25 != 0,
                power::V18 => self.caps & 1 << 26 != 0,
                _ => false,
            };
        let on = v & power::ON != 0 && ok;
        self.regs[regs::POWER_CONTROL] = if on { v } else { v & !power::ON };
        if was && !on {
            if let Some(c) = &mut self.card {
                c.power_on();
            }
        }
    }

    fn software_reset(&mut self, bits: u8) {
        if bits & reset::ALL != 0 {
            let caps = self.regs[0x40..0x50].to_vec();
            let ver = self.r16(regs::HOST_VERSION);
            self.regs[..0x40].fill(0);
            self.regs[0x50..0xFC].fill(0);
            self.regs[0x40..0x50].copy_from_slice(&caps);
            self.w16(regs::HOST_VERSION, ver);
            self.cmd_busy = false;
            self.dat_busy = false;
            self.pio = None;
            self.sdma = None;
            self.set_power(0);
        }
        if bits & reset::CMD != 0 {
            self.cmd_busy = false;
        }
        if bits & reset::DAT != 0 {
            self.dat_busy = false;
            self.pio = None;
            self.sdma = None;
            let s = self.r16(regs::INT_STATUS)
                & !(int::TRANSFER_COMPLETE | int::DMA | int::BUFFER_READ | int::BUFFER_WRITE | int::BLOCK_GAP);
            self.w16(regs::INT_STATUS, s);
        }
        self.regs[regs::SOFTWARE_RESET] = 0;
    }

    fn write_hc2(&mut self, v: u16) {
        let old = self.r16(regs::HOST_CONTROL2);
        if self.version() < regs::version::V3 {
            return;
        }
        if v & hc2::V18 != 0 && self.caps & 1 << 26 == 0 {
            self.violation(String::from("1.8 V signalling without the capability"));
        }
        if (v ^ old) & hc2::UHS_MASK != 0 && self.r16(regs::CLOCK_CONTROL) & clk::CARD_ENABLE != 0 {
            self.violation(String::from("UHS mode changed with the card clock running"));
        }
        if v & hc2::EXECUTE_TUNING != 0 && old & hc2::EXECUTE_TUNING == 0 {
            self.tuning = 0;
        }
        // An eMMC's I/O voltage (VCCQ) follows the host's signalling.
        if let Some(c) = self.card.as_mut().filter(|c| c.spec.emmc) {
            c.v18 = v & hc2::V18 != 0;
        }
        self.w16(regs::HOST_CONTROL2, v);
    }

    fn write_clock(&mut self, v: u16) {
        let mut v = v;
        if v & clk::INT_ENABLE != 0 {
            v |= clk::INT_STABLE;
        } else {
            v &= !clk::INT_STABLE;
        }
        let was_on = self.r16(regs::CLOCK_CONTROL) & clk::CARD_ENABLE != 0;
        self.w16(regs::CLOCK_CONTROL, v);
        let on = v & clk::CARD_ENABLE != 0;
        if on && !was_on {
            let v18 = self.host_v18();
            if let Some(c) = &mut self.card {
                if c.switching && v18 {
                    c.switching = false;
                    c.v18 = true;
                }
            }
        }
        if on {
            if let Some(c) = &self.card {
                if c.switching {
                    self.violations.push(String::from("card clock restarted at 3.3 V during the voltage switch"));
                }
            }
        }
    }

    /// The Command register's upper byte was written: run the command.
    fn issue(&mut self) {
        let tmode = self.r16(regs::TRANSFER_MODE);
        let c = self.r16(regs::COMMAND);
        let index = (c >> 8) as u8 & 0x3F;
        let arg = self.r32(regs::ARGUMENT);
        let data = c & cmd::DATA != 0;
        let resp = c & 3;
        let abort = c & cmd::ABORT == cmd::ABORT;
        self.log.push((index, arg));
        if self.cmd_busy {
            self.violation(format!("CMD{} issued while CMD inhibit was set", index));
        }
        if (data || resp == cmd::RESP_48_BUSY) && self.dat_busy && !abort {
            self.violation(format!("CMD{} issued while DAT inhibit was set", index));
        }
        let hz = self.clock();
        if hz == 0 || !self.powered() {
            self.violation(format!("CMD{} with {}", index, if hz == 0 { "the card clock stopped" } else { "bus power off" }));
            self.raise_err(err::CMD_TIMEOUT);
            return;
        }
        let tuning = self.r16(regs::HOST_CONTROL2) & hc2::EXECUTE_TUNING != 0;
        let host_v18 = self.host_v18();
        let sampling = self.r16(regs::HOST_CONTROL2) & hc2::SAMPLING_CLOCK != 0;
        let Some(card) = self.card.as_mut() else {
            self.raise_err(err::CMD_TIMEOUT);
            return;
        };
        if hz > card.max_clock() {
            let max = card.max_clock();
            self.violation(format!("CMD{} at {} Hz; the card allows {} Hz now", index, hz, max));
        }
        let card = self.card.as_mut().unwrap();
        if card.v18 != host_v18 && !card.switching {
            let v18 = card.v18;
            self.violation(format!("CMD{}: card at {} V, host signalling at {} V", index, if v18 { "1.8" } else { "3.3" }, if host_v18 { "1.8" } else { "3.3" }));
        }
        let fault = self.take_fault(index, false);
        if fault == Some(Fault::CmdTimeout) {
            self.raise_err(err::CMD_TIMEOUT);
            return;
        }
        let card = self.card.as_mut().unwrap();
        let reply = card.command(index, arg);
        if resp == cmd::RESP_NONE {
            // Nothing to wait for: the command went out.
            self.raise(int::CMD_COMPLETE);
            return;
        }
        let expect = match reply {
            Reply::None => {
                self.raise_err(err::CMD_TIMEOUT);
                return;
            }
            Reply::R2(_) => cmd::RESP_136,
            _ => resp,
        };
        if expect != resp && !(resp == cmd::RESP_48_BUSY && expect == cmd::RESP_48) {
            self.violation(format!("CMD{}: response type {} programmed, card sends {}", index, resp, expect));
        }
        match reply {
            Reply::R1(v) | Reply::R3(v) => {
                self.w32(regs::RESPONSE, v);
                self.w32(regs::RESPONSE + 4, 0);
            }
            Reply::R2(b) => {
                let v = u128::from_be_bytes(b) >> 8;
                for i in 0..4 {
                    self.w32(regs::RESPONSE + i * 4, (v >> (32 * i)) as u32);
                }
            }
            Reply::None => {}
        }
        if fault == Some(Fault::CmdCrc) {
            self.raise_err(err::CMD_CRC);
            return;
        }
        self.raise(int::CMD_COMPLETE);
        if !data {
            if resp == cmd::RESP_48_BUSY {
                self.raise(int::TRANSFER_COMPLETE);
            }
            return;
        }
        // The data phase.
        let card = self.card.as_mut().unwrap();
        let card_reads = matches!(card.phase, Phase::Fixed(_) | Phase::Read { .. });
        let card_writes = matches!(card.phase, Phase::Write { .. });
        let read = tmode & tm::READ != 0;
        let block_size = (self.r16(regs::BLOCK_SIZE) & 0xFFF) as usize;
        let multi = tmode & tm::MULTI != 0;
        let blocks = if multi { self.r16(regs::BLOCK_COUNT) as u32 } else { 1 };
        if multi && tmode & tm::BLOCK_COUNT == 0 {
            self.violation(format!("CMD{}: multi-block without block count", index));
        }
        if block_size == 0 || blocks == 0 {
            self.violation(format!("CMD{}: empty data phase", index));
            return;
        }
        if (read && !card_reads) || (!read && !card_writes) {
            // The card won't send or take data (an error in its status).
            self.dat_busy = true;
            self.raise_err(err::DATA_TIMEOUT);
            return;
        }
        if tuning {
            self.tune_step(index, block_size);
            return;
        }
        let mut fault = self.take_fault(index, true);
        if self.card.as_ref().unwrap().needs_tuning() && !sampling {
            fault = Some(Fault::DataCrc { after: 0 });
        }
        let host_width = match self.r8(regs::HOST_CONTROL) & (hc::WIDTH_4 | hc::WIDTH_8) {
            0 => 1,
            hc::WIDTH_4 => 4,
            _ => 8,
        };
        let card = self.card.as_ref().unwrap();
        if host_width != card.bus_width() && !matches!(index, 6 | 8 | 51 | 19 | 21 | 13) {
            let w = card.bus_width();
            self.violation(format!("CMD{}: host bus {}-bit, card {}-bit", index, host_width, w));
        }
        let auto12 = multi && tmode & tm::AUTO_CMD12 != 0;
        self.dat_busy = true;
        if tmode & tm::DMA == 0 {
            let mut p = Pio { write: !read, block_size, left: blocks, done: 0, buf: Vec::new(), pos: 0, auto12, fault };
            if read {
                if !self.pio_load(&mut p) {
                    return;
                }
                self.pio = Some(p);
                self.raise(int::BUFFER_READ);
            } else {
                p.buf = vec![0; block_size];
                self.pio = Some(p);
                self.raise(int::BUFFER_WRITE);
            }
            return;
        }
        match self.r8(regs::HOST_CONTROL) & hc::DMA_MASK {
            hc::DMA_SDMA => {
                let s = Sdma { write: !read, block_size, left: blocks, done: 0, auto12, fault };
                self.run_sdma(s);
            }
            hc::DMA_ADMA32 | hc::DMA_ADMA64 => self.run_adma(!read, block_size, blocks, auto12, fault),
            other => self.violation(format!("DMA select {:#x} is not ADMA2 or SDMA", other)),
        }
    }

    /// Checks a fault before block `done` moves; true if the transfer must stop.
    fn data_fault(&mut self, fault: Option<Fault>, done: u32) -> bool {
        match fault {
            Some(Fault::DataCrc { after }) if done >= after => {
                self.raise_err(err::DATA_CRC);
                true
            }
            Some(Fault::DataTimeout { after }) if done >= after => {
                self.raise_err(err::DATA_TIMEOUT);
                true
            }
            _ => false,
        }
    }

    fn pio_load(&mut self, p: &mut Pio) -> bool {
        if self.data_fault(p.fault, p.done) {
            return false;
        }
        let Some(b) = self.card.as_mut().and_then(|c| c.read_block(p.block_size)) else {
            self.raise_err(err::DATA_TIMEOUT);
            return false;
        };
        p.buf = b;
        p.pos = 0;
        true
    }

    fn finish(&mut self, auto12: bool) {
        if auto12 {
            if let Some(c) = self.card.as_mut() {
                if let Reply::R1(v) = c.command(12, 0) {
                    self.w32(regs::RESPONSE + 12, v);
                }
            }
        }
        self.dat_busy = false;
        self.raise(int::TRANSFER_COMPLETE);
    }

    fn buffer_read(&mut self) -> u32 {
        let Some(mut p) = self.pio.take() else {
            self.violation(String::from("Buffer Data Port read with no data ready"));
            return 0;
        };
        if p.write || p.pos + 4 > p.buf.len() {
            self.violation(String::from("Buffer Data Port read out of turn"));
            return 0;
        }
        let v = u32::from_le_bytes(p.buf[p.pos..p.pos + 4].try_into().unwrap());
        p.pos += 4;
        if p.pos == p.block_size {
            p.done += 1;
            p.left -= 1;
            if p.left == 0 {
                self.finish(p.auto12);
                return v;
            }
            if !self.pio_load(&mut p) {
                return v;
            }
            self.raise(int::BUFFER_READ);
        }
        self.pio = Some(p);
        v
    }

    fn buffer_write(&mut self, v: u32) {
        let Some(mut p) = self.pio.take() else {
            self.violation(String::from("Buffer Data Port write with no space"));
            return;
        };
        if !p.write {
            self.violation(String::from("Buffer Data Port write during a read"));
            return;
        }
        p.buf[p.pos..p.pos + 4].copy_from_slice(&v.to_le_bytes());
        p.pos += 4;
        if p.pos == p.block_size {
            if self.data_fault(p.fault, p.done) {
                return;
            }
            let ok = self.card.as_mut().is_some_and(|c| c.write_block(&p.buf));
            if !ok {
                self.raise_err(err::DATA_TIMEOUT);
                return;
            }
            p.done += 1;
            p.left -= 1;
            p.pos = 0;
            if p.left == 0 {
                self.finish(p.auto12);
                return;
            }
            self.raise(int::BUFFER_WRITE);
        }
        self.pio = Some(p);
    }

    fn mem(&self, addr: u64, len: usize) -> &'static mut [u8] {
        unsafe { core::slice::from_raw_parts_mut((self.translate)(addr), len) }
    }

    /// Moves one block between memory at `addr` and the card. False: stopped.
    fn dma_block(&mut self, write: bool, addr: u64, size: usize, fault: Option<Fault>, done: u32) -> bool {
        if self.data_fault(fault, done) {
            return false;
        }
        if write {
            let data = self.mem(addr, size).to_vec();
            if !self.card.as_mut().is_some_and(|c| c.write_block(&data)) {
                self.raise_err(err::DATA_TIMEOUT);
                return false;
            }
        } else {
            let Some(b) = self.card.as_mut().and_then(|c| c.read_block(size)) else {
                self.raise_err(err::DATA_TIMEOUT);
                return false;
            };
            self.mem(addr, size).copy_from_slice(&b);
        }
        true
    }

    fn run_sdma(&mut self, mut s: Sdma) {
        let boundary = 4096u64 << ((self.r16(regs::BLOCK_SIZE) >> 12) & 7);
        let mut addr = self.r32(regs::SDMA_ADDRESS) as u64;
        while s.left > 0 {
            if !self.dma_block(s.write, addr, s.block_size, s.fault, s.done) {
                return;
            }
            addr += s.block_size as u64;
            s.done += 1;
            s.left -= 1;
            self.w32(regs::SDMA_ADDRESS, addr as u32);
            if s.left > 0 && addr.is_multiple_of(boundary) {
                // Paused until the driver writes the next address.
                self.sdma = Some(s);
                self.raise(int::DMA);
                return;
            }
        }
        self.finish(s.auto12);
    }

    fn run_adma(&mut self, write: bool, block_size: usize, blocks: u32, auto12: bool, fault: Option<Fault>) {
        let wide = self.r8(regs::HOST_CONTROL) & hc::DMA_MASK == hc::DMA_ADMA64;
        if wide && self.caps & 1 << 28 == 0 {
            self.violation(String::from("64-bit ADMA2 without 64-bit addressing"));
        }
        let mut table = self.r32(regs::ADMA_ADDRESS) as u64;
        if wide {
            table |= (self.r32(regs::ADMA_ADDRESS_HI) as u64) << 32;
        }
        if fault == Some(Fault::Adma) {
            self.regs[regs::ADMA_ERROR] = 1;
            self.raise_err(err::ADMA);
            return;
        }
        let size = if wide { 12 } else { 8 };
        let total = block_size * blocks as usize;
        let mut moved = 0usize;
        let mut done = 0u32;
        let mut pending: Vec<u8> = Vec::new();
        for _ in 0..4096 {
            let d = self.mem(table, size).to_vec();
            let attr = u16::from_le_bytes([d[0], d[1]]);
            let len = match u16::from_le_bytes([d[2], d[3]]) {
                0 => 65536,
                n => n as usize,
            };
            let addr = if wide {
                u64::from_le_bytes(d[4..12].try_into().unwrap())
            } else {
                u32::from_le_bytes(d[4..8].try_into().unwrap()) as u64
            };
            if attr & adma::VALID == 0 {
                self.regs[regs::ADMA_ERROR] = 1;
                self.raise_err(err::ADMA);
                return;
            }
            if !wide && addr % 4 != 0 {
                self.violation(format!("ADMA2 data address {:#x} not 4-byte aligned", addr));
            }
            match attr & 0x30 {
                adma::TRAN => {
                    // Move the descriptor's bytes, a block at a time.
                    let mut off = 0;
                    while off < len {
                        if moved >= total {
                            self.violation(String::from("ADMA2 table longer than the transfer"));
                            break;
                        }
                        let n = (block_size - pending.len()).min(len - off);
                        if write {
                            pending.extend_from_slice(self.mem(addr + off as u64, n));
                            if pending.len() == block_size {
                                if self.data_fault(fault, done) {
                                    return;
                                }
                                let blk = core::mem::take(&mut pending);
                                if !self.card.as_mut().is_some_and(|c| c.write_block(&blk)) {
                                    self.raise_err(err::DATA_TIMEOUT);
                                    return;
                                }
                                done += 1;
                            }
                        } else {
                            if pending.is_empty() {
                                if self.data_fault(fault, done) {
                                    return;
                                }
                                let Some(b) = self.card.as_mut().and_then(|c| c.read_block(block_size)) else {
                                    self.raise_err(err::DATA_TIMEOUT);
                                    return;
                                };
                                pending = b;
                                pending.reverse();
                                done += 1;
                            }
                            let dst = self.mem(addr + off as u64, n);
                            for x in dst.iter_mut() {
                                *x = pending.pop().unwrap();
                            }
                        }
                        off += n;
                        moved += n;
                    }
                    table += size as u64;
                }
                adma::LINK => table = addr,
                _ => table += size as u64,
            }
            if attr & adma::END != 0 {
                if moved != total {
                    self.violation(format!("ADMA2 table moved {} bytes of {}", moved, total));
                    self.regs[regs::ADMA_ERROR] = 2;
                    self.raise_err(err::ADMA);
                    return;
                }
                self.finish(auto12);
                return;
            }
        }
        self.violation(String::from("ADMA2 table without an end"));
    }

    fn tune_step(&mut self, index: u8, block_size: usize) {
        let Some(card) = self.card.as_mut() else { return };
        let expect: Vec<u8> = if index == 21 && card.bus_width() == 8 { TUNING_8BIT.to_vec() } else { TUNING_4BIT.to_vec() };
        let steps = card.spec.tuning_steps;
        let got = card.read_block(block_size);
        if got.as_deref() != Some(&expect[..]) {
            self.violation(format!("tuning with CMD{} and {}-byte blocks: wrong pattern", index, block_size));
        }
        let fails = self.faults.iter().any(|f| f.fault == Fault::TuningFails);
        self.tuning += 1;
        let c2 = self.r16(regs::HOST_CONTROL2);
        if !fails && self.tuning >= steps {
            self.w16(regs::HOST_CONTROL2, (c2 & !hc2::EXECUTE_TUNING) | hc2::SAMPLING_CLOCK);
        } else if self.tuning >= 40 {
            self.w16(regs::HOST_CONTROL2, c2 & !(hc2::EXECUTE_TUNING | hc2::SAMPLING_CLOCK));
        }
        self.raise(int::BUFFER_READ);
    }

    fn write(&mut self, o: usize, width: usize, v: u32) {
        // Registers with side effects take the whole access at once.
        match (o, width) {
            (regs::BUFFER, 4) => return self.buffer_write(v),
            (regs::INT_STATUS, _) | (regs::ERR_STATUS, _) => {
                let bytes = v.to_le_bytes();
                for i in 0..width {
                    self.regs[o + i] &= !bytes[i];
                }
                let e = self.r16(regs::ERR_STATUS);
                let s = self.r16(regs::INT_STATUS);
                self.w16(regs::INT_STATUS, if e != 0 { s | int::ERROR } else { s & !int::ERROR });
                return;
            }
            (regs::SOFTWARE_RESET, 1) => return self.software_reset(v as u8),
            (regs::CLOCK_CONTROL, 2) => return self.write_clock(v as u16),
            (regs::CLOCK_CONTROL, 4) | (regs::TIMEOUT_CONTROL, 2) => {
                self.violation(String::from("wide write over Clock Control / Software Reset"));
            }
            (regs::POWER_CONTROL, 1) => return self.set_power(v as u8),
            (regs::HOST_CONTROL2, 2) => return self.write_hc2(v as u16),
            (regs::SDMA_ADDRESS, 4) => {
                self.w32(o, v);
                if let Some(s) = self.sdma.take() {
                    self.run_sdma(s);
                }
                return;
            }
            (regs::BLOCK_SIZE, _) | (regs::BLOCK_COUNT, _) if self.dat_busy => {
                self.violation(String::from("block size/count written during a transfer"));
            }
            _ => {}
        }
        if (0x40..0x50).contains(&o) || o >= 0xFC {
            return;
        }
        let bytes = v.to_le_bytes();
        self.regs[o..o + width].copy_from_slice(&bytes[..width]);
        if (o..o + width).contains(&(regs::COMMAND + 1)) {
            self.cmd_busy = false;
            self.issue();
        }
    }
}

/// A spinlock good enough for a simulation.
struct Lock<T> {
    busy: AtomicBool,
    value: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for Lock<T> {}

impl<T> Lock<T> {
    fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        while self.busy.swap(true, Ordering::Acquire) {
            core::hint::spin_loop();
        }
        let r = f(unsafe { &mut *self.value.get() });
        self.busy.store(false, Ordering::Release);
        r
    }
}

/// The simulated controller (cloning gives another handle to the same one).
#[derive(Clone)]
pub struct Mock(Arc<Lock<Ctl>>);

/// Capabilities of a version 3 controller with everything: 8-bit bus,
/// ADMA2 with 64-bit addresses, SDMA, high speed, 3.3 and 1.8 V, SDR50,
/// SDR104 and DDR50, a 200 MHz base clock.
pub const CAPS_FULL: u64 = 0x34 | 1 << 7 | 200 << 8 | 1 << 18 | 1 << 19 | 1 << 21 | 1 << 22 | 1 << 24 | 1 << 26 | 1 << 28 | 0x7 << 32;
/// What QEMU's sdhci-pci reports by default (version 2: 52 MHz, ADMA2 and SDMA, 3.3 and 1.8 V).
pub const CAPS_QEMU: u64 = 0x0578_34B4;

impl Mock {
    pub fn new(caps: u64, version: u8, translate: Translate) -> Mock {
        let mut regs = [0u8; 256];
        regs[0x40..0x48].copy_from_slice(&caps.to_le_bytes());
        regs[regs::HOST_VERSION] = version;
        Mock(Arc::new(Lock {
            busy: AtomicBool::new(false),
            value: UnsafeCell::new(Ctl {
                regs,
                caps,
                translate,
                card: None,
                write_protect: false,
                faults: Vec::new(),
                violations: Vec::new(),
                log: Vec::new(),
                cmd_busy: false,
                dat_busy: false,
                pio: None,
                sdma: None,
                tuning: 0,
            }),
        }))
    }

    /// Puts a card in the slot (raising card insertion).
    pub fn insert(&self, spec: CardSpec) {
        self.0.with(|c| {
            c.card = Some(Card::new(spec));
            c.raise(int::CARD_INSERT);
        });
    }

    /// Pulls the card out (raising card removal); returns it.
    pub fn eject(&self) -> Option<Card> {
        self.0.with(|c| {
            let card = c.card.take();
            let p = c.regs[regs::POWER_CONTROL] & !power::ON;
            c.regs[regs::POWER_CONTROL] = p;
            c.pio = None;
            c.sdma = None;
            c.raise(int::CARD_REMOVE);
            card
        })
    }

    pub fn set_write_protect(&self, on: bool) {
        self.0.with(|c| c.write_protect = on);
    }

    pub fn inject(&self, cmd: Option<u8>, fault: Fault, count: u32) {
        self.0.with(|c| c.faults.push(Injection { cmd, fault, count }));
    }

    pub fn clear_faults(&self) {
        self.0.with(|c| c.faults.clear());
    }

    /// The driver's mistakes so far.
    pub fn violations(&self) -> Vec<String> {
        self.0.with(|c| c.violations.clone())
    }

    /// Commands issued: (index, argument).
    pub fn commands(&self) -> Vec<(u8, u32)> {
        self.0.with(|c| c.log.clone())
    }

    pub fn clear_log(&self) {
        self.0.with(|c| c.log.clear());
    }

    /// Runs `f` on the card (None when the slot is empty).
    pub fn card<R>(&self, f: impl FnOnce(&mut Card) -> R) -> Option<R> {
        self.0.with(|c| c.card.as_mut().map(f))
    }

    /// Whether the controller asserts its interrupt.
    pub fn irq(&self) -> bool {
        self.0.with(|c| {
            c.r16(regs::INT_STATUS) & c.r16(regs::INT_SIGNAL) != 0 || c.r16(regs::ERR_STATUS) & c.r16(regs::ERR_SIGNAL) != 0
        })
    }

    /// The card's switch argument for `EXT_CSD[index] = value` (for tests).
    pub fn switch_arg(index: usize, value: u8) -> u32 {
        mmc_switch_arg(index, value)
    }
}

impl Regs for Mock {
    fn read8(&self, o: usize) -> u8 {
        (self.read32(o & !3) >> ((o & 3) * 8)) as u8
    }
    fn read16(&self, o: usize) -> u16 {
        (self.read32(o & !3) >> ((o & 2) * 8)) as u16
    }
    fn read32(&self, o: usize) -> u32 {
        self.0.with(|c| match o {
            regs::BUFFER => c.buffer_read(),
            regs::PRESENT_STATE => c.present(),
            _ => c.r32(o),
        })
    }
    fn write8(&self, o: usize, v: u8) {
        self.0.with(|c| c.write(o, 1, v as u32));
    }
    fn write16(&self, o: usize, v: u16) {
        self.0.with(|c| c.write(o, 2, v as u32));
    }
    fn write32(&self, o: usize, v: u32) {
        self.0.with(|c| match o {
            regs::TRANSFER_MODE => {
                c.write(o, 2, v & 0xFFFF);
                c.write(o + 2, 2, v >> 16);
            }
            regs::INT_STATUS => {
                c.write(o, 2, v & 0xFFFF);
                c.write(o + 2, 2, v >> 16);
            }
            _ => c.write(o, 4, v),
        });
    }
}
