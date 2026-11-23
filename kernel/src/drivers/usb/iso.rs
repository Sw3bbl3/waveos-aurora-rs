//! Isochronous streams: audio, and (later) video.
//!
//! An isochronous endpoint moves a bounded amount of data once per service
//! interval (its ESIT: 1 ms frames at full speed, 125 µs microframes at high
//! speed and above, times 2^(bInterval-1)), with no retries. A class driver
//! opens a stream on an endpoint of the alternate setting it selected and
//! hands the controller an [`IsoHandler`]: the controller keeps `depth`
//! transfer descriptors (one per service interval) queued ahead of the
//! device, asks the handler to [`fill`](IsoHandler::fill) each OUT descriptor
//! just before queueing it, and gives it each IN descriptor's data as it
//! completes. Handlers run on the controller's event task (never in interrupt
//! context), so they may take `IrqMutex`es but must not block.
//!
//! Only xHCI implements streams so far; the other controllers answer
//! [`IsoError::Unsupported`].
//!
//! Switching alternate settings (which is how USB devices start and stop
//! streaming) goes through [`set_interface`]: the old setting's endpoints
//! leave the controller's schedule, the new ones join it, then the device
//! is told.

use super::{Bus, Endpoint, Setup, Speed, UsbDevice};
use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

/// Completion codes an IN descriptor may report besides [`super::OK`].
/// The descriptor's service interval passed before the controller could
/// serve it: its data is lost (IN) or was never sent (OUT).
pub const MISSED: u8 = 23;
/// The controller found no descriptor queued when an OUT interval came.
pub const UNDERRUN: u8 = 14;
/// The controller found no descriptor queued when an IN interval came.
pub const OVERRUN: u8 = 15;

/// Why a stream could not be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IsoError {
    /// This host controller driver has no isochronous support.
    Unsupported,
    /// The endpoint is not part of the device's configured endpoints.
    NotConfigured,
    /// A stream is already open on the endpoint.
    Busy,
    NoMemory,
}

impl IsoError {
    pub fn name(self) -> &'static str {
        match self {
            IsoError::Unsupported => "isochronous transfers are not supported on this controller",
            IsoError::NotConfigured => "endpoint not configured",
            IsoError::Busy => "a stream is already open",
            IsoError::NoMemory => "out of memory",
        }
    }
}

/// A class driver's side of an isochronous stream.
pub trait IsoHandler: Send + Sync {
    /// OUT endpoints: writes the next service interval's data into `buf`
    /// (sized to the endpoint's largest payload per interval) and returns
    /// how many bytes to send. 0 sends nothing that interval.
    fn fill(&self, _buf: &mut [u8]) -> usize {
        0
    }
    /// IN endpoints: one service interval's data (possibly empty). `status`
    /// is [`super::OK`], or [`MISSED`] / an error code with `data` empty.
    fn data(&self, _data: &[u8], _status: u8) {}
    /// The stream ended: closed, the device went away, or the controller
    /// was reset (after sleep). No more calls follow.
    fn stopped(&self) {}
}

/// Counters for one stream (diagnostics and tests).
#[derive(Default)]
pub struct IsoStats {
    /// Service intervals completed.
    pub completed: AtomicU64,
    /// Intervals the controller missed (MISSED, or retired by an underrun).
    pub missed: AtomicU64,
    /// Ring underruns (OUT) and overruns (IN): the queue ran dry.
    pub dry: AtomicU64,
    /// Transaction errors.
    pub errors: AtomicU64,
}

impl IsoStats {
    pub fn snapshot(&self) -> (u64, u64, u64, u64) {
        (
            self.completed.load(Ordering::Relaxed),
            self.missed.load(Ordering::Relaxed),
            self.dry.load(Ordering::Relaxed),
            self.errors.load(Ordering::Relaxed),
        )
    }
}

/// An endpoint's service interval in microseconds.
pub fn interval_us(ep: &Endpoint, speed: Speed) -> u32 {
    let exp = ep.interval.clamp(1, 16) as u32 - 1;
    match speed {
        Speed::Low | Speed::Full => 1000 << exp.min(5),
        _ => 125 << exp.min(12),
    }
}

/// The most an endpoint moves per service interval: max packet × the
/// transactions (high speed) or burst (SuperSpeed) per interval.
pub fn esit_payload(ep: &Endpoint) -> usize {
    ep.max_packet as usize * (ep.burst as usize + 1)
}

/// How many descriptors to keep queued to cover `ms` milliseconds (at least
/// 4, at most 64: the transfer ring holds 255).
pub fn depth_for(ep: &Endpoint, speed: Speed, ms: u32) -> usize {
    ((ms * 1000).div_ceil(interval_us(ep, speed)) as usize).clamp(4, 64)
}

/// Spreads a fractional number of frames per interval (16.16 fixed point)
/// over whole packets: 44.1 kHz at 1 ms gives 44, 44, … and a 45 every tenth.
pub struct PacketClock {
    acc: u32,
}

impl PacketClock {
    pub const fn new() -> PacketClock {
        PacketClock { acc: 0 }
    }
    /// Frames for the next interval at `per_interval_q16` frames per interval.
    pub fn next(&mut self, per_interval_q16: u32) -> usize {
        self.acc += per_interval_q16;
        let n = self.acc >> 16;
        self.acc &= 0xFFFF;
        n as usize
    }
}

impl Default for PacketClock {
    fn default() -> Self {
        Self::new()
    }
}

/// `rate` frames per second as frames per service interval, 16.16.
pub fn frames_per_interval_q16(rate: u32, interval_us: u32) -> u32 {
    ((rate as u64 * interval_us as u64 * 65536) / 1_000_000) as u32
}

impl Bus {
    /// Starts an isochronous stream on a configured endpoint, keeping
    /// `depth` service intervals queued.
    pub fn open_iso(&self, dev: u8, ep: &Endpoint, depth: usize, handler: Arc<dyn IsoHandler>) -> Result<(), IsoError> {
        self.hcd.open_iso(dev, ep, depth, handler)
    }

    /// Stops a stream (its handler's `stopped` runs); queued data is dropped.
    pub fn close_iso(&self, dev: u8, ep: &Endpoint) {
        self.hcd.close_iso(dev, ep.address)
    }

    /// The stream's counters, if one is open on that endpoint.
    pub fn iso_stats(&self, dev: u8, ep: &Endpoint) -> Option<Arc<IsoStats>> {
        self.hcd.iso_stats(dev, ep.address)
    }
}

/// Selects alternate setting `alt` of interface `iface`: `old` endpoints (of
/// the setting in use, if it had any) leave the controller's schedule and
/// `new` ones join it before the device is told (SET_INTERFACE). Streams on
/// `old` must be closed first. Returns false if the controller refused the
/// endpoints (not enough bandwidth) or the device the request.
pub fn set_interface(bus: &Bus, dev: u8, iface: u8, alt: u8, old: &[Endpoint], new: &[Endpoint]) -> bool {
    if !old.is_empty() && !bus.hcd.drop_endpoints(dev, old) {
        return false;
    }
    if !new.is_empty() && !bus.configure(dev, new, None) {
        return false;
    }
    let setup = Setup { request_type: 0x01, request: 11, value: alt as u16, index: iface as u16, length: 0 };
    match bus.control(dev, setup, None) {
        Ok(_) => true,
        // Devices with a single setting may stall SET_INTERFACE to it.
        Err(super::STALL) => alt == 0,
        Err(_) => false,
    }
}

impl UsbDevice {
    /// The full configuration descriptor, with the class-specific
    /// descriptors [`super::Configuration`] skips (audio, video).
    pub fn raw_configuration(&self) -> Option<alloc::vec::Vec<u8>> {
        let mut head = [0u8; 9];
        let index = self.config_index;
        self.control(Setup::get_descriptor(super::DESC_CONFIG, index, 0, 9), Some(&mut head)).ok()?;
        let total = u16::from_le_bytes([head[2], head[3]]).clamp(9, 8192);
        let mut full = alloc::vec![0u8; total as usize];
        let n = self.control(Setup::get_descriptor(super::DESC_CONFIG, index, 0, total), Some(&mut full)).ok()?;
        full.truncate(n);
        Some(full)
    }
}
