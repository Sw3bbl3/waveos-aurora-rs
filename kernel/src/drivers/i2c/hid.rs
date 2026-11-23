//! HID over I²C devices: touchpads, touch screens and pointing sticks.
//!
//! Each device has a task. It reads the HID descriptor, powers the device
//! on and resets it, reads the report descriptor, switches a Precision
//! Touchpad into reporting contacts (Input Mode 3, surface and button
//! switches on) or a touch screen into multi-touch (Input Mode 2), and then
//! reads input reports, feeding them through `aurora_hid` exactly as the USB
//! driver does: the same contact decoding and the same gesture engine
//! (pointer acceleration, tap to click, tap-and-drag, two-finger scrolling
//! and click, palm rejection from the confidence bit, three-finger swipes),
//! with the same Trackpad settings.
//!
//! When to read: the device pulls its interrupt line while a report waits.
//!
//! - An `Interrupt` resource (an I/O APIC input): routed, and the task
//!   sleeps until it fires (a level line is masked until the reports are read).
//! - A `GpioInt` on a GPIO controller we can read (AMD): the pin's level is
//!   sampled (a register read, no bus traffic) and the bus read only when
//!   it is asserted.
//! - Otherwise (Intel `GpioInt`, whose pad registers differ per platform):
//!   the input register is polled, 125 times a second while a finger is on
//!   the pad or reports came in the last half second, 20 times a second
//!   when idle (so the first touch takes up to 50 ms to notice). A device
//!   with nothing to say returns an empty report. The cost: the controller
//!   and the touchpad never sleep, and an idle system wakes 20 times a
//!   second for a ~1 ms transfer — a little battery, which a GPIO driver
//!   would save.

use super::gpio::LevelPin;
use super::{BusRef, I2cBus};
use crate::acpi::devices::AcpiDevice;
use crate::drivers::input::{self, InputEvent};
use crate::sync::{IrqMutex, WaitQueue};
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_acpi_res::{normalize_path, Polarity, Resource, Trigger};
use aurora_hid::gestures::Gestures;
use aurora_hid::touchpad::{self, Frames, Info};
use aurora_hid::{pointer, touchscreen, usage, Descriptor};
use aurora_i2c::hid::{Device, Error as HidError, Input, ReportType};
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// Poll intervals (ms): while touched or recently active, and idle.
const POLL_ACTIVE_MS: u64 = 8;
const POLL_IDLE_MS: u64 = 50;
/// "Recently active": a report within this long.
const ACTIVE_MS: u64 = 500;
/// With a readable GPIO level: how often the level is sampled while idle,
/// and how often the bus is read anyway, to catch a pin that reads wrong.
const LEVEL_IDLE_MS: u64 = 20;
const LEVEL_CHECK_MS: u64 = 1000;

/// How the device tells us it has a report.
pub enum IrqSource {
    /// An I/O APIC input (a global system interrupt).
    Gsi { gsi: u32, level: bool, active_low: bool },
    /// A GPIO pin whose level we can read.
    GpioLevel { pin: LevelPin, active_low: bool, what: String },
    /// Nothing usable: poll, for this reason.
    Poll { why: String },
}

impl IrqSource {
    pub fn from_resources(resources: &[Resource], gpio: &[AcpiDevice]) -> IrqSource {
        for r in resources {
            match r {
                Resource::Interrupt { interrupts, trigger, polarity, source: None, .. } if !interrupts.is_empty() => {
                    return IrqSource::Gsi {
                        gsi: interrupts[0],
                        level: *trigger == Trigger::Level,
                        active_low: *polarity == Polarity::Low,
                    };
                }
                Resource::Gpio(g) if g.interrupt => {
                    let source = normalize_path(&g.source);
                    let pin = g.pins.first().copied().unwrap_or(0);
                    let Some(ctrl) = gpio.iter().find(|d| d.path == source) else {
                        return IrqSource::Poll {
                            why: format!("GpioInt pin {} on {}: controller not found", pin, source),
                        };
                    };
                    return match LevelPin::new(ctrl, pin) {
                        Some(lp) => IrqSource::GpioLevel {
                            what: format!("GpioInt pin {} on {} ({}), level sampled", pin, source, lp.controller),
                            pin: lp,
                            active_low: g.polarity != Polarity::High,
                        },
                        None => IrqSource::Poll {
                            why: format!("GpioInt pin {} on {} ({}): no GPIO driver for it", pin, source, ctrl.hid),
                        },
                    };
                }
                _ => {}
            }
        }
        IrqSource::Poll { why: String::from("no interrupt resource") }
    }

    fn describe(&self) -> String {
        match self {
            IrqSource::Gsi { gsi, level, active_low } => format!(
                "interrupt GSI {} ({}, active {})",
                gsi,
                if *level { "level" } else { "edge" },
                if *active_low { "low" } else { "high" }
            ),
            IrqSource::GpioLevel { what, .. } => what.clone(),
            IrqSource::Poll { why } => format!("polled ({})", why),
        }
    }
}

pub struct Config {
    /// The device's ACPI path.
    pub path: String,
    /// Its controller's ACPI path.
    pub controller: String,
    pub bus: Arc<dyn I2cBus>,
    pub addr: u16,
    pub desc_reg: u16,
    pub speed_hz: u32,
    pub irq: IrqSource,
}

struct Dev {
    cfg: Config,
    wq: WaitQueue,
    fired: AtomicBool,
    resume: AtomicBool,
    vector: AtomicU8,
    status: IrqMutex<String>,
}

static DEVICES: IrqMutex<Vec<Arc<Dev>>> = IrqMutex::new(Vec::new());

pub fn start(cfg: Config) {
    let status = format!("{}: starting", cfg.path);
    let dev = Arc::new(Dev {
        cfg,
        wq: WaitQueue::new(),
        fired: AtomicBool::new(false),
        resume: AtomicBool::new(false),
        vector: AtomicU8::new(0),
        status: IrqMutex::new(status),
    });
    DEVICES.lock().push(dev.clone());
    let arg = Arc::into_raw(dev) as u64;
    crate::sched::spawn_task("i2c-hid", run, arg, 0, crate::mm::vmm::kernel_pml4());
}

/// After sleep: every device is brought up again.
pub fn resume_all() {
    for d in DEVICES.lock().iter() {
        d.resume.store(true, Ordering::Release);
        d.wq.wake_all();
    }
}

pub fn list() -> Vec<String> {
    DEVICES.lock().iter().map(|d| d.status.lock().clone()).collect()
}

fn on_gsi(arg: usize) {
    let d = unsafe { &*(arg as *const Dev) };
    if let IrqSource::Gsi { gsi, level: true, .. } = d.cfg.irq {
        // The line stays asserted until the reports are read (by the task).
        crate::arch::apic::mask_gsi(gsi);
    }
    d.fired.store(true, Ordering::Release);
    d.wq.wake_all();
}

/// Lets the line interrupt again (after the reports were read).
fn unmask(d: &Dev) {
    if let IrqSource::Gsi { gsi, level, active_low } = d.cfg.irq {
        let v = d.vector.load(Ordering::Acquire);
        if v != 0 {
            crate::arch::apic::route_gsi(gsi, v, level, active_low);
        }
    }
}

fn run(arg: u64) {
    let dev: Arc<Dev> = unsafe { Arc::from_raw(arg as *const Dev) };
    if let IrqSource::Gsi { gsi, level, active_low } = dev.cfg.irq {
        match crate::arch::irq::alloc("i2c-hid", on_gsi, Arc::as_ptr(&dev) as usize) {
            Some(v) if crate::arch::apic::route_gsi(gsi, v, level, active_low) => {
                dev.vector.store(v, Ordering::Release)
            }
            _ => log!("i2c", "{}: could not route GSI {}; polling", dev.cfg.path, gsi),
        }
    }
    let mut failures = 0u32;
    loop {
        match Session::open(&dev) {
            Ok(mut s) => {
                failures = 0;
                s.run(&dev);
                s.release();
            }
            Err(e) => {
                failures += 1;
                log!("i2c", "{}: the device did not start: {:?}", dev.cfg.path, e);
                *dev.status.lock() = format!("{}: not responding at {:#x} ({:?})", dev.cfg.path, dev.cfg.addr, e);
                // Back off: 1, 2, 4 … s, at most a minute; a resume tries at once.
                let wait = (1000u64 << failures.min(6)).min(60_000);
                dev.wq.wait(wait, || dev.resume.load(Ordering::Acquire));
            }
        }
        if dev.resume.swap(false, Ordering::AcqRel) {
            // Power the controller and the device again (_PS0), as at boot.
            crate::acpi::devices::set_power(&dev.cfg.controller, true);
            crate::acpi::devices::set_power(&dev.cfg.path, true);
        }
    }
}

struct Pad {
    frames: Frames,
    gestures: Gestures,
    buttons: u8,
}

struct Screen {
    touches: touchscreen::Touches,
    info: Info,
    down: bool,
}

struct Mouse {
    buttons: u8,
    remainder: (i32, i32),
}

struct Session {
    hid: Device,
    desc: Descriptor,
    buf: Vec<u8>,
    pad: Option<Pad>,
    screen: Option<Screen>,
    mouse: Option<Mouse>,
    last_report: u64,
    errors: u32,
    /// The GPIO level agrees with the reports (else: poll).
    trust_level: bool,
}

#[derive(Debug)]
#[allow(dead_code)] // shown in the log
enum OpenError {
    Hid(HidError),
    ResetTimeout,
    BadReportDescriptor,
}

impl From<HidError> for OpenError {
    fn from(e: HidError) -> OpenError {
        OpenError::Hid(e)
    }
}

impl Session {
    fn open(dev: &Dev) -> Result<Session, OpenError> {
        let cfg = &dev.cfg;
        let mut bus = BusRef(&*cfg.bus);
        let hid = Device::probe(&mut bus, cfg.addr, cfg.desc_reg)?;
        let d = hid.desc;
        hid.set_power(&mut bus, true)?;
        hid.reset(&mut bus)?;
        // The device answers a reset with an empty input report.
        let mut buf = hid.input_buffer();
        let t0 = crate::time::uptime_ms();
        loop {
            match cfg.irq {
                IrqSource::Gsi { .. } if dev.vector.load(Ordering::Acquire) != 0 => {
                    dev.wq.wait(100, || dev.fired.load(Ordering::Acquire));
                    dev.fired.store(false, Ordering::Release);
                }
                _ => crate::sched::sleep_ms(20),
            }
            let r = hid.read_input(&mut bus, &mut buf);
            unmask(dev);
            if matches!(r, Ok(Input::Empty)) {
                break;
            }
            if crate::time::uptime_ms() - t0 > 1000 {
                // Not fatal: some devices never send the sentinel when polled.
                log!("i2c", "{}: no reset acknowledgement; carrying on", cfg.path);
                break;
            }
        }
        let rd = hid.report_descriptor(&mut bus)?;
        let desc = Descriptor::parse(&rd).map_err(|_| OpenError::BadReportDescriptor)?;
        let numbered_id = |r: &[u8]| if desc.numbered { r[0] } else { 0 };

        let mut roles: Vec<String> = Vec::new();
        let pad = if touchpad::is_touchpad(&desc) {
            let mode = touchpad::input_mode_report(&desc)
                .map(|r| hid.set_report(&mut bus, ReportType::Feature, numbered_id(&r), &r));
            if let Some(r) = touchpad::selective_reporting_report(&desc) {
                let _ = hid.set_report(&mut bus, ReportType::Feature, numbered_id(&r), &r);
            }
            touchpad::info(&desc).map(|info| {
                roles.push(format!(
                    "touchpad ({} contacts per report, {}×{} mm, input mode {})",
                    info.max_contacts,
                    info.width_hmm.map_or(0, |w| w / 100),
                    info.height_hmm.map_or(0, |h| h / 100),
                    match mode {
                        Some(Ok(())) => "set",
                        Some(Err(_)) => "refused",
                        None => "absent",
                    }
                ));
                Pad { frames: Frames::new(), gestures: Gestures::new(&info), buttons: 0 }
            })
        } else {
            None
        };
        let screen = if touchscreen::is_touchscreen(&desc) {
            if let Some(r) = touchpad::input_mode_report_with(&desc, 2) {
                let _ = hid.set_report(&mut bus, ReportType::Feature, numbered_id(&r), &r);
            }
            touchscreen::info(&desc).map(|info| {
                roles.push(format!("touch screen ({}..{} × {}..{})", info.x_min, info.x_max, info.y_min, info.y_max));
                Screen { touches: touchscreen::Touches::new(), info, down: false }
            })
        } else {
            None
        };
        // A mouse collection: pointing sticks, and touchpads left in (or
        // refusing to leave) mouse mode.
        let mouse = pointer::is_pointer(&desc).then(|| {
            roles.push(String::from("mouse"));
            Mouse { buttons: 0, remainder: (0, 0) }
        });
        if desc.has_application(usage::KEYBOARD) {
            roles.push(String::from("keyboard (not supported over I2C yet)"));
        }
        let line = format!(
            "{}: {:04x}:{:04x} v{:#x} at {:#x}, {} kHz (HID descriptor {:#x}, report descriptor {} bytes, input ≤ {} bytes): {}; {}",
            cfg.path,
            d.vendor,
            d.product,
            d.version,
            cfg.addr,
            cfg.speed_hz / 1000,
            cfg.desc_reg,
            rd.len(),
            d.max_input_len,
            if roles.is_empty() { String::from("nothing we drive") } else { roles.join(", ") },
            cfg.irq.describe()
        );
        log!("i2c", "{}", line);
        *dev.status.lock() = line;
        unmask(dev);
        Ok(Session {
            hid,
            desc,
            buf,
            pad,
            screen,
            mouse,
            last_report: crate::time::uptime_ms(),
            errors: 0,
            trust_level: true,
        })
    }

    fn busy(&self, now: u64) -> bool {
        self.pad.as_ref().is_some_and(|p| p.gestures.busy())
            || self.screen.as_ref().is_some_and(|s| s.down)
            || now.saturating_sub(self.last_report) < ACTIVE_MS
    }

    /// Reads reports until the device has none (a few at most). False if
    /// the device stopped answering.
    fn drain(&mut self, dev: &Dev) -> bool {
        let mut bus = BusRef(&*dev.cfg.bus);
        let polled = !matches!(dev.cfg.irq, IrqSource::Gsi { .. });
        let mut got = false;
        for _ in 0..8 {
            match self.hid.read_input(&mut bus, &mut self.buf) {
                Ok(Input::Report(r)) => {
                    let r = r.to_vec();
                    self.dispatch(&r);
                    self.errors = 0;
                    got = true;
                }
                Ok(Input::Empty) => {
                    self.errors = 0;
                    break;
                }
                // Noise on the line: skip it.
                Err(HidError::BadLength(_)) => break,
                // A polled device may refuse a read when it has nothing.
                Err(HidError::Bus(aurora_i2c::Error::AddressNack)) if polled => break,
                Err(_) => {
                    self.errors += 1;
                    break;
                }
            }
        }
        if got {
            self.last_report = crate::time::uptime_ms();
        }
        self.errors < 100
    }

    fn dispatch(&mut self, report: &[u8]) {
        let d = &self.desc;
        let now = crate::time::uptime_ms();
        if let Some(p) = &mut self.pad {
            if let Some(r) = touchpad::read(d, report) {
                if let Some(frame) = p.frames.feed(r) {
                    let o = p.gestures.frame(now, &frame, &input::touchpad_settings());
                    input::touchpad_output(o, &mut p.buttons);
                }
                return;
            }
        }
        if let Some(s) = &mut self.screen {
            if let Some(r) = touchscreen::read(d, report) {
                if let Some(t) = s.touches.feed(r) {
                    let (w, h) = input::screen_size();
                    let x = pointer::scale(t.x, (s.info.x_min, s.info.x_max), w);
                    let y = pointer::scale(t.y, (s.info.y_min, s.info.y_max), h);
                    input::push(InputEvent::Pointer { x, y, buttons: t.down as u8 });
                    s.down = t.down;
                }
                return;
            }
        }
        if let Some(m) = &mut self.mouse {
            if let Some(r) = pointer::read(d, report) {
                let buttons = r.buttons & 0x07;
                if r.absolute {
                    let (w, h) = input::screen_size();
                    let (px, py) = input::pointer_position();
                    let x = r.x.map_or(px, |x| pointer::scale(x, r.x_range, w));
                    let y = r.y.map_or(py, |y| pointer::scale(y, r.y_range, h));
                    input::push(InputEvent::Pointer { x, y, buttons });
                } else {
                    let (dx, dy) = input::scale_motion(r.x.unwrap_or(0), r.y.unwrap_or(0), &mut m.remainder);
                    if dx != 0 || dy != 0 || buttons != m.buttons {
                        input::pointer_moved(dx, dy, buttons);
                    }
                }
                m.buttons = buttons;
                if r.wheel != 0 || r.pan != 0 {
                    input::push(InputEvent::Scroll { dx: r.pan * input::NOTCH, dy: -r.wheel * input::NOTCH });
                }
            }
        }
    }

    /// Taps' clicks ending, gliding: returns how soon to come back (ms).
    fn tick(&mut self, now: u64) -> Option<u64> {
        let p = self.pad.as_mut()?;
        let (out, again) = p.gestures.tick(now);
        if let Some(o) = out {
            input::touchpad_output(o, &mut p.buttons);
        }
        again
    }

    fn run(&mut self, dev: &Dev) {
        let mut last_check = 0u64;
        let mut empty_fires = 0u32;
        loop {
            if dev.resume.load(Ordering::Acquire) {
                return;
            }
            let now = crate::time::uptime_ms();
            let tick = self.tick(now);
            let busy = self.busy(now);
            let ok = match &dev.cfg.irq {
                IrqSource::Gsi { gsi, .. } if dev.vector.load(Ordering::Acquire) != 0 && empty_fires < 200 => {
                    // Sleep until the line fires; wake for gesture timers, and
                    // read once a second anyway (a lost edge must not hang us).
                    let timeout = tick.unwrap_or(1000).min(1000);
                    dev.wq.wait(timeout, || dev.fired.load(Ordering::Acquire) || dev.resume.load(Ordering::Acquire));
                    let fired = dev.fired.swap(false, Ordering::AcqRel);
                    let now = crate::time::uptime_ms();
                    let ok = if fired || now - last_check >= 1000 {
                        last_check = now;
                        let before = self.last_report;
                        let ok = self.drain(dev);
                        // A line that keeps firing with nothing to read is stuck.
                        empty_fires = if fired && self.last_report == before { empty_fires + 1 } else { 0 };
                        if empty_fires == 200 {
                            log!("i2c", "{}: GSI {} fires with nothing to read; polling instead", dev.cfg.path, gsi);
                            crate::arch::apic::mask_gsi(*gsi);
                        }
                        ok
                    } else {
                        true
                    };
                    if empty_fires < 200 {
                        unmask(dev);
                    }
                    ok
                }
                IrqSource::GpioLevel { pin, active_low, .. } if self.trust_level => {
                    let period = if busy { POLL_ACTIVE_MS } else { LEVEL_IDLE_MS };
                    dev.wq.wait(tick.unwrap_or(period).min(period), || dev.resume.load(Ordering::Acquire));
                    let asserted = pin.level() != *active_low;
                    let now = crate::time::uptime_ms();
                    if asserted || now - last_check >= LEVEL_CHECK_MS {
                        let before = self.last_report;
                        let ok = self.drain(dev);
                        if !asserted && self.last_report != before {
                            log!(
                                "i2c",
                                "{}: a report came while the GPIO level said none; polling instead",
                                dev.cfg.path
                            );
                            self.trust_level = false;
                        }
                        last_check = now;
                        ok
                    } else {
                        true
                    }
                }
                _ => {
                    let period = if busy { POLL_ACTIVE_MS } else { POLL_IDLE_MS };
                    dev.wq.wait(tick.unwrap_or(period).min(period), || dev.resume.load(Ordering::Acquire));
                    self.drain(dev)
                }
            };
            if !ok {
                log!("i2c", "{}: the device stopped answering; starting it again", dev.cfg.path);
                return;
            }
        }
    }

    /// Lets go of anything held.
    fn release(&mut self) {
        let held = self.pad.as_ref().is_some_and(|p| p.buttons != 0)
            || self.mouse.as_ref().is_some_and(|m| m.buttons != 0)
            || self.screen.as_ref().is_some_and(|s| s.down);
        if held {
            input::pointer_moved(0, 0, 0);
        }
    }
}
