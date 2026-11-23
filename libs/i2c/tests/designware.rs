//! The DesignWare driver against a simulated controller.
//!
//! The simulation follows the controller's documented behaviour (command
//! FIFO, receive FIFO, TX_ABRT flushing, the hold on an empty FIFO without a
//! STOP, the user abort) closely enough to catch the driver's own mistakes:
//! overrunning a FIFO, changing the target while enabled, losing a byte,
//! never finishing. A HID over I²C touchpad sits on the simulated bus, so a
//! whole bring-up runs through it. It is a model, not the silicon.

use aurora_i2c::designware::{abort, intr, reg, timing_for, Config, Controller, Regs, Speed, Waiter, COMP_TYPE};
use aurora_i2c::hid::{Device, Input, ReportType};
use aurora_i2c::mock::{precision_touchpad, touch_report, HidTarget};
use aurora_i2c::{Bus, Error};
use std::cell::RefCell;
use std::collections::VecDeque;

struct State {
    enabled: bool,
    con: u32,
    tar: u32,
    tx: VecDeque<u32>,
    rx: VecDeque<u8>,
    raw: u32,
    mask: u32,
    rx_tl: u32,
    tx_tl: u32,
    abort_source: u32,
    tx_depth: usize,
    rx_depth: usize,
    /// A transaction in progress: bytes written so far, and the data being read.
    active: bool,
    writes: Vec<u8>,
    reading: Option<(Vec<u8>, usize)>,
    target: HidTarget,
    /// The device holds the clock: nothing moves.
    stuck: bool,
    /// Commands executed per register access (bus speed against CPU speed).
    pace: usize,
    /// Programming errors the driver made.
    violations: Vec<String>,
}

struct Sim(RefCell<State>);

impl Sim {
    fn new(target: HidTarget, tx_depth: usize, rx_depth: usize, pace: usize) -> Sim {
        Sim(RefCell::new(State {
            enabled: false,
            con: 0,
            tar: 0,
            tx: VecDeque::new(),
            rx: VecDeque::new(),
            raw: 0,
            mask: 0,
            rx_tl: 0,
            tx_tl: 0,
            abort_source: 0,
            tx_depth,
            rx_depth,
            active: false,
            writes: Vec::new(),
            reading: None,
            target,
            stuck: false,
            pace,
            violations: Vec::new(),
        }))
    }
}

impl State {
    fn end(&mut self) {
        self.active = false;
        self.writes.clear();
        self.reading = None;
        self.raw |= intr::STOP_DET;
    }

    fn abort(&mut self, e: Error) {
        self.abort_source = match e {
            Error::AddressNack => abort::ADDR7_NOACK,
            Error::DataNack => abort::TXDATA_NOACK,
            _ => abort::USER_ABORT,
        };
        self.raw |= intr::TX_ABRT;
        self.tx.clear();
        self.end();
    }

    fn step(&mut self) {
        for _ in 0..self.pace {
            if self.stuck || !self.enabled {
                return;
            }
            if self.raw & intr::TX_ABRT != 0 {
                // Held in abort until CLR_TX_ABRT: commands are flushed.
                self.tx.clear();
                return;
            }
            let Some(c) = self.tx.pop_front() else { return };
            self.active = true;
            let addr = (self.tar & 0x3FF) as u16;
            if c & 0x100 != 0 {
                if self.reading.is_none() || c & 0x400 != 0 {
                    let mut buf = vec![0u8; 4096];
                    let writes = std::mem::take(&mut self.writes);
                    match self.target.transfer(addr, &writes, &mut buf) {
                        Ok(()) => self.reading = Some((buf, 0)),
                        Err(e) => return self.abort(e),
                    }
                }
                let (buf, at) = self.reading.as_mut().unwrap();
                let b = buf[*at];
                *at += 1;
                if self.rx.len() >= self.rx_depth {
                    self.raw |= intr::RX_OVER;
                } else {
                    self.rx.push_back(b);
                }
            } else {
                if self.reading.is_some() {
                    self.reading = None;
                }
                self.writes.push(c as u8);
            }
            if c & 0x200 != 0 {
                if self.reading.is_none() {
                    let writes = std::mem::take(&mut self.writes);
                    if let Err(e) = self.target.transfer(addr, &writes, &mut []) {
                        return self.abort(e);
                    }
                }
                self.end();
            }
        }
    }

    fn raw_status(&self) -> u32 {
        let mut r = self.raw;
        if self.tx.len() as u32 <= self.tx_tl {
            r |= intr::TX_EMPTY;
        }
        if self.rx.len() as u32 > self.rx_tl {
            r |= intr::RX_FULL;
        }
        r
    }
}

impl Regs for Sim {
    fn read(&self, off: usize) -> u32 {
        let mut s = self.0.borrow_mut();
        s.step();
        match off {
            reg::CON => s.con,
            reg::TAR => s.tar,
            reg::DATA_CMD => s.rx.pop_front().map_or_else(
                || {
                    s.raw |= intr::RX_UNDER;
                    0
                },
                |b| b as u32,
            ),
            reg::RAW_INTR_STAT => s.raw_status(),
            reg::INTR_STAT => s.raw_status() & s.mask,
            reg::INTR_MASK => s.mask,
            reg::CLR_INTR => {
                s.raw = 0;
                s.abort_source = 0;
                0
            }
            reg::CLR_TX_ABRT => {
                s.raw &= !intr::TX_ABRT;
                s.abort_source = 0;
                0
            }
            reg::CLR_STOP_DET => {
                s.raw &= !intr::STOP_DET;
                0
            }
            reg::ENABLE => s.enabled as u32,
            reg::ENABLE_STATUS => s.enabled as u32,
            reg::STATUS => {
                let busy = s.active || !s.tx.is_empty();
                (busy as u32) | (busy as u32) << 5
            }
            reg::TXFLR => s.tx.len() as u32,
            reg::RXFLR => s.rx.len() as u32,
            reg::TX_ABRT_SOURCE => s.abort_source,
            reg::COMP_PARAM_1 => ((s.tx_depth as u32 - 1) << 16) | ((s.rx_depth as u32 - 1) << 8),
            reg::COMP_VERSION => 0x3131_312A,
            reg::COMP_TYPE => COMP_TYPE,
            _ => 0,
        }
    }

    fn write(&self, off: usize, v: u32) {
        let mut s = self.0.borrow_mut();
        match off {
            reg::CON | reg::TAR | reg::SS_SCL_HCNT | reg::SS_SCL_LCNT | reg::FS_SCL_HCNT | reg::FS_SCL_LCNT
                if s.enabled =>
            {
                s.violations.push(format!("register {off:#x} written while enabled"));
            }
            reg::CON => s.con = v,
            reg::TAR => s.tar = v,
            reg::DATA_CMD => {
                if !s.enabled {
                    s.violations.push("DATA_CMD while disabled".into());
                } else if s.tx.len() >= s.tx_depth {
                    s.raw |= intr::TX_OVER;
                    s.violations.push("transmit FIFO overrun".into());
                } else {
                    s.tx.push_back(v);
                }
            }
            reg::INTR_MASK => s.mask = v,
            reg::RX_TL => s.rx_tl = v.min(s.rx_depth as u32 - 1),
            reg::TX_TL => s.tx_tl = v.min(s.tx_depth as u32 - 1),
            reg::ENABLE => {
                if v & 2 != 0 && s.enabled {
                    // User abort: a STOP, the FIFO flushed, TX_ABRT raised.
                    s.stuck = false;
                    s.abort(Error::Abort(0));
                    s.abort_source = abort::USER_ABORT;
                } else {
                    s.enabled = v & 1 != 0;
                    if !s.enabled {
                        s.tx.clear();
                        s.rx.clear();
                        s.active = false;
                        s.writes.clear();
                        s.reading = None;
                    }
                }
            }
            _ => {}
        }
        s.step();
    }
}

/// Time passes 5 µs per question; waiting does nothing (each register access moves the bus).
struct Clock(u64);

impl Waiter for Clock {
    fn now_us(&mut self) -> u64 {
        self.0 += 5;
        self.0
    }
    fn wait(&mut self, _regs: &dyn Regs, _events: u32) {
        self.0 += 20;
    }
}

struct DwBus {
    c: Controller<Sim>,
    clock: Clock,
}

impl Bus for DwBus {
    fn transfer(&mut self, addr: u16, write: &[u8], read: &mut [u8]) -> Result<(), Error> {
        self.c.transfer(&mut self.clock, addr, write, read)
    }
}

fn config() -> Config {
    Config {
        speed: Speed::Fast,
        standard: timing_for(120_000_000, Speed::Standard),
        fast: timing_for(120_000_000, Speed::Fast),
        timeout_us: 50_000,
    }
}

fn bus(tx_depth: usize, rx_depth: usize, pace: usize) -> DwBus {
    let target = HidTarget::new(0x2C, 0x20, precision_touchpad(), 32);
    let mut c = Controller::new(Sim::new(target, tx_depth, rx_depth, pace), config()).unwrap();
    let mut clock = Clock(0);
    c.init(&mut clock).unwrap();
    DwBus { c, clock }
}

fn violations(b: &DwBus) -> Vec<String> {
    b.c.regs.0.borrow().violations.clone()
}

#[test]
fn timings_never_run_the_bus_too_fast() {
    // At 120 MHz: 4.3 µs high is 516 clocks, 5.0 µs low 600; fast 108/192.
    let t = timing_for(120_000_000, Speed::Standard);
    assert_eq!((t.hcnt, t.lcnt), (516, 600));
    let f = timing_for(120_000_000, Speed::Fast);
    assert_eq!((f.hcnt, f.lcnt), (108, 192));
    // Period never below 2.5 µs (400 kHz) whatever the clock.
    for mhz in [25u64, 100, 120, 133, 150, 216] {
        let f = timing_for(mhz * 1_000_000, Speed::Fast);
        let period_ns = (f.hcnt as u64 + f.lcnt as u64) * 1000 / mhz;
        assert!(period_ns >= 2500, "{mhz} MHz: {period_ns} ns");
    }
    assert_eq!(Speed::for_hz(1_000_000), Speed::Fast);
    assert_eq!(Speed::for_hz(100_000), Speed::Standard);
}

#[test]
fn not_a_designware_controller() {
    struct Nothing;
    impl Regs for Nothing {
        fn read(&self, _: usize) -> u32 {
            0xFFFF_FFFF
        }
        fn write(&self, _: usize, _: u32) {}
    }
    assert!(Controller::new(Nothing, config()).is_err());
}

#[test]
fn touchpad_bring_up_through_the_controller() {
    // Small FIFOs (8 deep) and a slow bus: the 200-byte report descriptor
    // needs many refills, and the driver must never ask for more than fits.
    for (depth, pace) in [(8, 1), (8, 3), (32, 1), (64, 64)] {
        let mut b = bus(depth, depth, pace);
        let dev = Device::probe(&mut b, 0x2C, 0x20).expect("HID descriptor");
        dev.set_power(&mut b, true).unwrap();
        dev.reset(&mut b).unwrap();
        let mut buf = dev.input_buffer();
        assert_eq!(dev.read_input(&mut b, &mut buf).unwrap(), Input::Empty);
        assert_eq!(dev.report_descriptor(&mut b).unwrap(), precision_touchpad(), "depth {depth}, pace {pace}");
        dev.set_report(&mut b, ReportType::Feature, 3, &[3, 3]).unwrap();
        assert_eq!(dev.get_report(&mut b, ReportType::Feature, 3, 2).unwrap(), vec![3, 3]);
        let report = touch_report(&[(1, true, 1234, 4321), (2, true, 5000, 100)], 2, true);
        b.c.regs.0.borrow_mut().target.push_input(&report);
        assert_eq!(dev.read_input(&mut b, &mut buf).unwrap(), Input::Report(&report[..]));
        assert!(violations(&b).is_empty(), "{:?}", violations(&b));
    }
}

#[test]
fn nack_then_carry_on() {
    let mut b = bus(16, 16, 2);
    let mut r = [0u8; 4];
    assert_eq!(b.transfer(0x50, &[0x00], &mut r), Err(Error::AddressNack));
    // The controller works on afterwards.
    let dev = Device::probe(&mut b, 0x2C, 0x20).unwrap();
    assert_eq!(dev.desc.max_input_len, 32);
    assert!(violations(&b).is_empty(), "{:?}", violations(&b));
}

#[test]
fn a_stuck_device_times_out_and_the_bus_recovers() {
    let mut b = bus(16, 16, 1);
    let dev = Device::probe(&mut b, 0x2C, 0x20).unwrap();
    b.c.regs.0.borrow_mut().stuck = true;
    let mut buf = dev.input_buffer();
    assert_eq!(b.transfer(0x2C, &[], &mut buf), Err(Error::Timeout));
    assert!(!b.c.regs.0.borrow().stuck, "the driver aborted the transfer");
    let dev = Device::probe(&mut b, 0x2C, 0x20).unwrap();
    assert_eq!(dev.desc.vendor, 0x1234);
    assert!(violations(&b).is_empty(), "{:?}", violations(&b));
}

#[test]
fn empty_transfers_are_refused() {
    let mut b = bus(16, 16, 1);
    assert_eq!(b.transfer(0x2C, &[], &mut []), Err(Error::Unsupported));
}
