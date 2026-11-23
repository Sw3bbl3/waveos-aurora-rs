//! The Synopsys DesignWare APB I²C controller (DW_apb_i2c), as a master.
//!
//! Register map and behaviour as Synopsys documents them, and as the Intel
//! PCH and SoC datasheets (LPSS I²C) and AMD's processor programming
//! references (FCH I²C) reproduce them:
//!
//! - The controller is configured while disabled (`IC_ENABLE` = 0): mode and
//!   speed in `IC_CON`, the target address in `IC_TAR`, SCL high and low
//!   counts per speed, SDA hold time.
//! - A transfer is a stream of commands written to `IC_DATA_CMD`: a byte to
//!   send, or (bit 8) a byte to read; bit 9 adds a STOP after it, bit 10 a
//!   RESTART before it. Read bytes come back through the same register from
//!   the receive FIFO. When the transmit FIFO runs dry without a STOP the
//!   master holds the bus (SCL low) until more commands arrive.
//! - A NACK or lost arbitration raises `TX_ABRT` (the reason is in
//!   `IC_TX_ABRT_SOURCE`) and flushes the transmit FIFO; reading
//!   `IC_CLR_TX_ABRT` lets the controller go on.
//! - `IC_ENABLE` bit 1 (ABORT) makes the master end a transfer with a STOP:
//!   our way out of a stuck device.
//!
//! The transfer loop keeps the FIFOs moving and waits through a [`Waiter`]:
//! the kernel's sleeps on the controller's interrupt, unmasking just the
//! events the loop is waiting for, or polls when there is no interrupt.

use crate::Error;

pub mod reg {
    pub const CON: usize = 0x00;
    pub const TAR: usize = 0x04;
    pub const DATA_CMD: usize = 0x10;
    pub const SS_SCL_HCNT: usize = 0x14;
    pub const SS_SCL_LCNT: usize = 0x18;
    pub const FS_SCL_HCNT: usize = 0x1C;
    pub const FS_SCL_LCNT: usize = 0x20;
    pub const INTR_STAT: usize = 0x2C;
    pub const INTR_MASK: usize = 0x30;
    pub const RAW_INTR_STAT: usize = 0x34;
    pub const RX_TL: usize = 0x38;
    pub const TX_TL: usize = 0x3C;
    pub const CLR_INTR: usize = 0x40;
    pub const CLR_TX_ABRT: usize = 0x54;
    pub const CLR_STOP_DET: usize = 0x60;
    pub const ENABLE: usize = 0x6C;
    pub const STATUS: usize = 0x70;
    pub const TXFLR: usize = 0x74;
    pub const RXFLR: usize = 0x78;
    pub const SDA_HOLD: usize = 0x7C;
    pub const TX_ABRT_SOURCE: usize = 0x80;
    pub const ENABLE_STATUS: usize = 0x9C;
    pub const COMP_PARAM_1: usize = 0xF4;
    pub const COMP_VERSION: usize = 0xF8;
    pub const COMP_TYPE: usize = 0xFC;
}

/// `IC_COMP_TYPE` of every DesignWare APB I²C ("DW" and 0x0140).
pub const COMP_TYPE: u32 = 0x4457_0140;

/// `IC_CON` bits.
pub mod con {
    pub const MASTER: u32 = 1 << 0;
    pub const SPEED_STANDARD: u32 = 1 << 1;
    pub const SPEED_FAST: u32 = 2 << 1;
    pub const TEN_BIT_MASTER: u32 = 1 << 4;
    pub const RESTART_EN: u32 = 1 << 5;
    pub const SLAVE_DISABLE: u32 = 1 << 6;
}

/// Interrupt bits (`IC_INTR_*`, `IC_RAW_INTR_STAT`).
pub mod intr {
    pub const RX_UNDER: u32 = 1 << 0;
    pub const RX_OVER: u32 = 1 << 1;
    pub const RX_FULL: u32 = 1 << 2;
    pub const TX_OVER: u32 = 1 << 3;
    pub const TX_EMPTY: u32 = 1 << 4;
    pub const TX_ABRT: u32 = 1 << 6;
    pub const ACTIVITY: u32 = 1 << 8;
    pub const STOP_DET: u32 = 1 << 9;
}

/// `IC_DATA_CMD` bits.
pub mod cmd {
    pub const READ: u32 = 1 << 8;
    pub const STOP: u32 = 1 << 9;
    pub const RESTART: u32 = 1 << 10;
}

/// `IC_TX_ABRT_SOURCE` bits.
pub mod abort {
    pub const ADDR7_NOACK: u32 = 1 << 0;
    pub const ADDR10_1_NOACK: u32 = 1 << 1;
    pub const ADDR10_2_NOACK: u32 = 1 << 2;
    pub const TXDATA_NOACK: u32 = 1 << 3;
    pub const ARB_LOST: u32 = 1 << 12;
    pub const USER_ABORT: u32 = 1 << 16;
}

/// `IC_STATUS` bits.
pub mod status {
    pub const ACTIVITY: u32 = 1 << 0;
    pub const MASTER_ACTIVITY: u32 = 1 << 5;
}

/// 32-bit register access at a byte offset.
pub trait Regs {
    fn read(&self, offset: usize) -> u32;
    fn write(&self, offset: usize, value: u32);
}

/// How the transfer loop waits.
pub trait Waiter {
    /// Microseconds from any fixed point.
    fn now_us(&mut self) -> u64;
    /// Waits a short while (up to about a millisecond) for any of the
    /// `IC_RAW_INTR_STAT` bits in `events`. May return early or spuriously.
    fn wait(&mut self, regs: &dyn Regs, events: u32);
}

/// SCL high and low periods in controller clocks, and SDA hold time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timing {
    pub hcnt: u16,
    pub lcnt: u16,
    /// `IC_SDA_HOLD`; 0 leaves the controller's default.
    pub sda_hold: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    /// 100 kHz.
    Standard,
    /// 400 kHz.
    Fast,
}

impl Speed {
    /// The fastest mode a device rated for `hz` takes (Fast-mode Plus and
    /// High-speed devices run in Fast mode).
    pub fn for_hz(hz: u32) -> Speed {
        if hz >= 400_000 {
            Speed::Fast
        } else {
            Speed::Standard
        }
    }
}

/// Timings for a controller clocked at `clock_hz`, from the I²C
/// specification's minimum SCL high and low times plus 300 ns for the
/// signal's fall, rounded up: the bus never runs faster than the mode allows.
pub fn timing_for(clock_hz: u64, speed: Speed) -> Timing {
    // (t_HIGH, t_LOW) in ns: 4000/4700 standard, 600/1300 fast; + 300 ns fall time.
    let (high, low) = match speed {
        Speed::Standard => (4_000 + 300, 4_700 + 300),
        Speed::Fast => (600 + 300, 1_300 + 300),
    };
    let clocks = |ns: u64| (clock_hz * ns).div_ceil(1_000_000_000).clamp(8, 0xFFFF) as u16;
    // SDA hold: 300 ns (the spec's minimum data hold for fast mode devices).
    let hold = (clock_hz * 300).div_ceil(1_000_000_000).max(1) as u32;
    Timing { hcnt: clocks(high), lcnt: clocks(low), sda_hold: hold }
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub speed: Speed,
    pub standard: Timing,
    pub fast: Timing,
    /// Give up on a transfer after this long (and abort it).
    pub timeout_us: u64,
}

/// A DesignWare controller, configured as the only master on its bus.
pub struct Controller<R: Regs> {
    pub regs: R,
    cfg: Config,
    tx_depth: u32,
    rx_depth: u32,
}

/// What the controller says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Identity {
    pub comp_type: u32,
    pub version: u32,
    pub tx_depth: u32,
    pub rx_depth: u32,
}

impl<R: Regs> Controller<R> {
    /// Reads the component registers. `None` if nothing (or not a
    /// DesignWare I²C) answers there.
    pub fn identify(regs: &R) -> Option<Identity> {
        let comp_type = regs.read(reg::COMP_TYPE);
        if comp_type != COMP_TYPE {
            return None;
        }
        let param = regs.read(reg::COMP_PARAM_1);
        // FIFO depths minus one, in bits 23:16 (TX) and 15:8 (RX); 0 means
        // the register isn't implemented: assume the smallest FIFO.
        let depth = |v: u32| if param == 0 { 8 } else { (v & 0xFF) + 1 };
        Some(Identity {
            comp_type,
            version: regs.read(reg::COMP_VERSION),
            tx_depth: depth(param >> 16).max(2),
            rx_depth: depth(param >> 8).max(2),
        })
    }

    pub fn new(regs: R, cfg: Config) -> Result<Controller<R>, Error> {
        let id = Self::identify(&regs).ok_or(Error::NoController)?;
        Ok(Controller { regs, cfg, tx_depth: id.tx_depth, rx_depth: id.rx_depth })
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    /// Disables the controller and waits until it says so.
    fn disable(&mut self, w: &mut dyn Waiter) -> Result<(), Error> {
        let start = w.now_us();
        loop {
            self.regs.write(reg::ENABLE, 0);
            if self.regs.read(reg::ENABLE_STATUS) & 1 == 0 {
                return Ok(());
            }
            if w.now_us().saturating_sub(start) > 25_000 {
                return Err(Error::NoController);
            }
            w.wait(&self.regs, 0);
        }
    }

    /// Programs the controller (after power-up, a reset or resume).
    pub fn init(&mut self, w: &mut dyn Waiter) -> Result<(), Error> {
        self.disable(w)?;
        let r = &self.regs;
        let speed = match self.cfg.speed {
            Speed::Standard => con::SPEED_STANDARD,
            Speed::Fast => con::SPEED_FAST,
        };
        r.write(reg::CON, con::MASTER | speed | con::RESTART_EN | con::SLAVE_DISABLE);
        r.write(reg::SS_SCL_HCNT, self.cfg.standard.hcnt as u32);
        r.write(reg::SS_SCL_LCNT, self.cfg.standard.lcnt as u32);
        r.write(reg::FS_SCL_HCNT, self.cfg.fast.hcnt as u32);
        r.write(reg::FS_SCL_LCNT, self.cfg.fast.lcnt as u32);
        let hold = match self.cfg.speed {
            Speed::Standard => self.cfg.standard.sda_hold,
            Speed::Fast => self.cfg.fast.sda_hold,
        };
        if hold != 0 {
            r.write(reg::SDA_HOLD, hold);
        }
        r.write(reg::TX_TL, 0);
        r.write(reg::RX_TL, 0);
        r.write(reg::INTR_MASK, 0);
        r.read(reg::CLR_INTR);
        Ok(())
    }

    /// Ends a stuck transfer with a STOP, then reprograms the controller.
    pub fn recover(&mut self, w: &mut dyn Waiter) -> Result<(), Error> {
        let start = w.now_us();
        if self.regs.read(reg::ENABLE_STATUS) & 1 != 0 {
            self.regs.write(reg::ENABLE, 1 | 2);
            while self.regs.read(reg::ENABLE) & 2 != 0 && w.now_us().saturating_sub(start) < 10_000 {
                w.wait(&self.regs, intr::TX_ABRT);
            }
            self.regs.read(reg::CLR_TX_ABRT);
        }
        self.init(w)
    }

    /// One combined transfer. On a timeout the transfer is aborted and the
    /// controller reprogrammed, ready for the next.
    pub fn transfer(&mut self, w: &mut dyn Waiter, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        if write.is_empty() && read.is_empty() {
            return Err(Error::Unsupported);
        }
        let result = self.run(w, addr, write, read);
        self.regs.write(reg::INTR_MASK, 0);
        if result == Err(Error::Timeout) || result == Err(Error::Overrun) {
            let _ = self.recover(w);
        }
        result
    }

    fn run(&mut self, w: &mut dyn Waiter, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        let start = w.now_us();
        let r = &self.regs;
        // The bus must be idle before the target can change.
        while r.read(reg::STATUS) & status::MASTER_ACTIVITY != 0 {
            if w.now_us().saturating_sub(start) > self.cfg.timeout_us {
                return Err(Error::Timeout);
            }
            w.wait(r, intr::STOP_DET);
        }
        self.disable(w)?;
        let r = &self.regs;
        let ten_bit = addr > 0x7F;
        let mut c = r.read(reg::CON) & !con::TEN_BIT_MASTER;
        if ten_bit {
            c |= con::TEN_BIT_MASTER;
        }
        r.write(reg::CON, c);
        r.write(reg::TAR, (addr as u32 & 0x3FF) | (ten_bit as u32) << 12);
        r.write(reg::INTR_MASK, 0);
        r.write(reg::ENABLE, 1);
        r.read(reg::CLR_INTR);

        let total = write.len() + read.len();
        let (mut sent, mut received) = (0usize, 0usize);
        let tx_depth = self.tx_depth;
        let rx_depth = self.rx_depth as usize;
        loop {
            let raw = r.read(reg::RAW_INTR_STAT);
            if raw & intr::TX_ABRT != 0 {
                let source = r.read(reg::TX_ABRT_SOURCE);
                r.read(reg::CLR_TX_ABRT);
                // The controller sends a STOP after an abort; let it finish.
                let t = w.now_us();
                while r.read(reg::STATUS) & status::MASTER_ACTIVITY != 0 && w.now_us().saturating_sub(t) < 2_000 {
                    w.wait(r, intr::STOP_DET);
                }
                r.read(reg::CLR_STOP_DET);
                return Err(if source & (abort::ADDR7_NOACK | abort::ADDR10_1_NOACK | abort::ADDR10_2_NOACK) != 0 {
                    Error::AddressNack
                } else if source & abort::TXDATA_NOACK != 0 {
                    Error::DataNack
                } else if source & abort::ARB_LOST != 0 {
                    Error::ArbitrationLost
                } else {
                    Error::Abort(source)
                });
            }
            if raw & intr::RX_OVER != 0 {
                return Err(Error::Overrun);
            }
            // Collect what arrived.
            let mut n = r.read(reg::RXFLR) as usize;
            while n > 0 && received < read.len() {
                read[received] = r.read(reg::DATA_CMD) as u8;
                received += 1;
                n -= 1;
            }
            // Queue commands while the transmit FIFO has room, and never ask
            // for more bytes than the receive FIFO can hold.
            while sent < total && r.read(reg::TXFLR) < tx_depth {
                let mut v = if sent < write.len() {
                    write[sent] as u32
                } else {
                    let j = sent - write.len();
                    let reads_queued = j;
                    if reads_queued - received >= rx_depth {
                        break;
                    }
                    let mut v = cmd::READ;
                    if j == 0 && !write.is_empty() {
                        v |= cmd::RESTART;
                    }
                    v
                };
                if sent == total - 1 {
                    v |= cmd::STOP;
                }
                r.write(reg::DATA_CMD, v);
                sent += 1;
            }
            if sent == total && received == read.len() && raw & intr::STOP_DET != 0 {
                r.read(reg::CLR_STOP_DET);
                return Ok(());
            }
            if w.now_us().saturating_sub(start) > self.cfg.timeout_us {
                return Err(Error::Timeout);
            }
            // Sleep until something happens: room in the transmit FIFO,
            // enough received bytes, the end, or an abort.
            let mut events = intr::TX_ABRT | intr::STOP_DET | intr::RX_OVER;
            if sent < total {
                r.write(reg::TX_TL, tx_depth / 2);
                events |= intr::TX_EMPTY;
            }
            let outstanding = (sent.saturating_sub(write.len())).saturating_sub(received);
            if outstanding > 0 {
                r.write(reg::RX_TL, (outstanding.min(rx_depth) - 1) as u32);
                events |= intr::RX_FULL;
            }
            w.wait(r, events);
        }
    }

    /// For an interrupt handler: whether the controller raised an
    /// interrupt; if so, masks it again (the waiting task re-arms it).
    pub fn interrupt(regs: &dyn Regs) -> bool {
        let pending = regs.read(reg::INTR_STAT);
        if pending == 0 {
            return false;
        }
        regs.write(reg::INTR_MASK, 0);
        true
    }
}
