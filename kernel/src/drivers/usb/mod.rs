//! USB: host controller drivers — [`xhci`] (USB 3, and every device speed
//! on PCs since about 2012), [`ehci`] (USB 2), [`ohci`] and [`uhci`] (USB 1.1,
//! often as EHCI's companions) — and class drivers for hubs ([`hub`]),
//! keyboards, pointing devices, touchpads and game controllers ([`hid`]) and
//! storage ([`msc`]).
//!
//! A controller driver implements [`Hcd`]: root ports, addressing a device
//! and moving data. Everything above that is shared and lives in [`Bus`]:
//! every controller has a `usb` task that handles port changes — it
//! enumerates new devices (addressing them, reading descriptors, picking a
//! class driver) and tears down unplugged ones — and the devices, their
//! interrupt pipes and their unplug hooks. Class drivers see a [`UsbDevice`]
//! and never the controller underneath.

pub mod ehci;
mod hcutil;
pub mod hid;
pub mod hub;
pub mod msc;
pub mod ohci;
pub mod uhci;
pub mod xhci;

use crate::drivers::block::Dma;
use crate::sync::{IrqMutex, WaitQueue};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

/// Transfer results, as xHCI numbers them (the other controllers translate).
pub const OK: u8 = 1;
/// The device answered with an error (USB transaction error, babble, CRC).
pub const ERROR: u8 = 4;
/// The endpoint halted (STALL): the request is not supported, or the
/// endpoint needs CLEAR_FEATURE(ENDPOINT_HALT).
pub const STALL: u8 = 6;
/// Our own code for "no completion arrived in time".
pub const TIMEOUT: u8 = 0xFF;

pub const DESC_DEVICE: u8 = 1;
pub const DESC_CONFIG: u8 = 2;
pub const DESC_STRING: u8 = 3;
pub const DESC_INTERFACE: u8 = 4;
pub const DESC_ENDPOINT: u8 = 5;
pub const DESC_HID: u8 = 0x21;
pub const DESC_REPORT: u8 = 0x22;

pub const CLASS_HID: u8 = 3;
pub const CLASS_MSC: u8 = 8;
pub const CLASS_HUB: u8 = 9;

/// Port speeds as xHCI numbers them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Speed {
    Full = 1,
    Low = 2,
    High = 3,
    Super = 4,
    SuperPlus = 5,
}

impl Speed {
    pub fn from_xhci(v: u32) -> Speed {
        match v {
            2 => Speed::Low,
            3 => Speed::High,
            4 => Speed::Super,
            5 => Speed::SuperPlus,
            _ => Speed::Full,
        }
    }

    /// The control endpoint's packet size before the device descriptor says otherwise.
    pub fn default_max_packet(self) -> u16 {
        match self {
            Speed::Low | Speed::Full => 8,
            Speed::High => 64,
            Speed::Super | Speed::SuperPlus => 512,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Speed::Low => "1.5 Mb/s",
            Speed::Full => "12 Mb/s",
            Speed::High => "480 Mb/s",
            Speed::Super => "5 Gb/s",
            Speed::SuperPlus => "10 Gb/s",
        }
    }
}

/// A control request's 8-byte setup packet.
#[derive(Clone, Copy)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

impl Setup {
    pub fn bytes(&self) -> u64 {
        self.request_type as u64
            | (self.request as u64) << 8
            | (self.value as u64) << 16
            | (self.index as u64) << 32
            | (self.length as u64) << 48
    }

    pub fn is_in(&self) -> bool {
        self.request_type & 0x80 != 0
    }

    pub fn get_descriptor(kind: u8, index: u8, lang: u16, length: u16) -> Setup {
        Setup { request_type: 0x80, request: 6, value: (kind as u16) << 8 | index as u16, index: lang, length }
    }

    /// GET_DESCRIPTOR addressed to an interface (HID report descriptors).
    pub fn get_interface_descriptor(kind: u8, iface: u8, length: u16) -> Setup {
        Setup { request_type: 0x81, request: 6, value: (kind as u16) << 8, index: iface as u16, length }
    }

    pub fn set_configuration(value: u8) -> Setup {
        Setup { request_type: 0x00, request: 9, value: value as u16, index: 0, length: 0 }
    }

    /// A class request to an interface (HID SET_PROTOCOL, SET_IDLE; MSC reset).
    pub fn class_interface(request: u8, value: u16, iface: u8, length: u16, device_to_host: bool) -> Setup {
        let request_type = if device_to_host { 0xA1 } else { 0x21 };
        Setup { request_type, request, value, index: iface as u16, length }
    }
}

#[derive(Clone, Debug, Default)]
pub struct DeviceDescriptor {
    pub class: u8,
    pub vendor: u16,
    pub product: u16,
    pub manufacturer_index: u8,
    pub product_index: u8,
}

impl DeviceDescriptor {
    pub fn parse(b: &[u8]) -> Option<DeviceDescriptor> {
        if b.len() < 18 || b[1] != DESC_DEVICE {
            return None;
        }
        Some(DeviceDescriptor {
            class: b[4],
            vendor: u16::from_le_bytes([b[8], b[9]]),
            product: u16::from_le_bytes([b[10], b[11]]),
            manufacturer_index: b[14],
            product_index: b[15],
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Endpoint {
    /// Endpoint number with the direction bit (0x80 = IN).
    pub address: u8,
    /// 0 control, 1 isochronous, 2 bulk, 3 interrupt.
    pub kind: u8,
    pub max_packet: u16,
    pub interval: u8,
    /// SuperSpeed companion: max burst.
    pub burst: u8,
}

impl Endpoint {
    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
    pub fn number(&self) -> u8 {
        self.address & 0x0F
    }
    /// xHCI device context index.
    pub fn dci(&self) -> u8 {
        self.number() * 2 + self.is_in() as u8
    }
}

#[derive(Clone, Debug)]
pub struct Interface {
    pub number: u8,
    pub alternate: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
    /// Length of the HID report descriptor (HID interfaces).
    pub report_length: u16,
}

#[derive(Clone, Debug)]
pub struct Configuration {
    pub value: u8,
    pub interfaces: Vec<Interface>,
}

impl Configuration {
    /// Parses a full configuration descriptor (with its interfaces and endpoints).
    pub fn parse(b: &[u8]) -> Option<Configuration> {
        if b.len() < 9 || b[1] != DESC_CONFIG {
            return None;
        }
        let mut c = Configuration { value: b[5], interfaces: Vec::new() };
        let mut i = 0;
        while i + 2 <= b.len() {
            let len = b[i] as usize;
            if len < 2 || i + len > b.len() {
                break;
            }
            let d = &b[i..i + len];
            match d[1] {
                DESC_INTERFACE if len >= 9 => c.interfaces.push(Interface {
                    number: d[2],
                    alternate: d[3],
                    class: d[5],
                    subclass: d[6],
                    protocol: d[7],
                    endpoints: Vec::new(),
                    report_length: 0,
                }),
                DESC_ENDPOINT if len >= 7 => {
                    if let Some(iface) = c.interfaces.last_mut() {
                        iface.endpoints.push(Endpoint {
                            address: d[2],
                            kind: d[3] & 3,
                            max_packet: u16::from_le_bytes([d[4], d[5]]) & 0x7FF,
                            interval: d[6],
                            burst: 0,
                        });
                    }
                }
                // SuperSpeed endpoint companion.
                0x30 if len >= 6 => {
                    if let Some(ep) = c.interfaces.last_mut().and_then(|i| i.endpoints.last_mut()) {
                        ep.burst = d[2];
                    }
                }
                DESC_HID if len >= 9 => {
                    if let Some(iface) = c.interfaces.last_mut() {
                        // The class descriptors follow: (type, length) pairs.
                        for k in 0..d[5] as usize {
                            let o = 6 + k * 3;
                            if o + 3 <= len && d[o] == DESC_REPORT {
                                iface.report_length = u16::from_le_bytes([d[o + 1], d[o + 2]]);
                            }
                        }
                    }
                }
                _ => {}
            }
            i += len;
        }
        Some(c)
    }
}

/// A string descriptor (UTF-16LE) as a Rust string.
pub fn parse_string(b: &[u8]) -> String {
    if b.len() < 2 || b[1] != DESC_STRING {
        return String::new();
    }
    let n = (b[0] as usize).min(b.len());
    let units: Vec<u16> = b[2..n].as_chunks::<2>().0.iter().map(|&c| u16::from_le_bytes(c)).collect();
    String::from_utf16_lossy(&units).trim().into()
}

/// Where a device sits, as the controller needs to know when addressing it.
#[derive(Clone, Copy, Debug)]
pub struct Position {
    pub root_port: u8,
    /// Hub ports on the way to it, 4 bits per tier (0 on a root port).
    pub route: u32,
    /// Hubs between it and the root port.
    pub depth: u32,
    pub speed: Speed,
    /// For a low- or full-speed device behind a high-speed hub: that hub's
    /// transaction translator, which turns USB 2 split transactions into
    /// slow ones on its port.
    pub tt: Option<Tt>,
}

/// A high-speed hub's transaction translator.
#[derive(Clone, Copy, Debug)]
pub struct Tt {
    /// The hub (its device handle on this bus).
    pub hub: u8,
    /// The hub port the slow device (or the slow hub above it) is on.
    pub port: u8,
    /// One translator per port (multi-TT) rather than one for the hub.
    pub multi: bool,
}

/// What a hub tells its controller when configured.
#[derive(Clone, Copy, Debug)]
pub struct HubInfo {
    pub ports: u8,
    /// Think time of its transaction translator (0–3: 8–32 FS bit times).
    pub think_time: u32,
    pub multi_tt: bool,
}

/// A driver fed by an interrupt IN endpoint (keyboard reports, hub changes).
pub trait PipeHandler: Send + Sync {
    fn data(&self, data: &[u8]);
    /// Periodic work (key repeat); returns ms until it wants to run again.
    fn tick(&self, _now_ms: u64) -> Option<u64> {
        None
    }
}

/// A host controller driver: what differs between xHCI, EHCI, OHCI and UHCI.
///
/// Devices are named by a handle the controller picks when addressing them
/// (an xHCI slot, or the USB address on the others). Transfers may be issued
/// from any task and block until done.
pub trait Hcd: Send + Sync {
    /// "xHCI", "EHCI", …
    fn kind(&self) -> &'static str;
    /// Number of root ports (numbered from 1).
    fn ports(&self) -> u8;
    /// Root ports whose status changed since the last call. Controllers that
    /// interrupt on port changes queue them; the others are polled here.
    fn changed_ports(&self) -> Vec<u8>;
    /// Acknowledges a root port's changes; true when a device is connected.
    fn port_connected(&self, port: u8) -> bool;
    /// Resets a root port with a device on it. Returns the device's speed once
    /// the port is enabled; `None` if that failed or the device was handed to
    /// a companion controller.
    fn port_reset(&self, port: u8) -> Option<Speed>;
    /// Addresses a new device (answering at address 0) and returns its handle.
    fn new_device(&self, pos: &Position) -> Option<u8>;
    /// Endpoint 0's packet size, once the device descriptor gives it.
    fn set_ep0_max_packet(&self, dev: u8, max_packet: u16);
    /// A control transfer on endpoint 0; returns the bytes transferred.
    fn control(&self, dev: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8>;
    /// A bulk or interrupt-OUT transfer of `len` bytes at physical `phys`.
    fn transfer(&self, dev: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8>;
    /// Sets up endpoints (and, for a hub, its properties) once a class driver picks them.
    fn configure(&self, dev: u8, eps: &[Endpoint], hub: Option<HubInfo>) -> bool;
    /// Starts polling an interrupt IN endpoint: `handler` gets every report.
    fn open_pipe(&self, dev: u8, ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool;
    /// The device cleared an endpoint's halt: its data toggle starts over.
    fn reset_toggle(&self, _dev: u8, _ep_address: u8) {}
    /// The device is gone: stop its pipes and free its handle.
    fn release(&self, dev: u8);
    /// After sleep: reset and start again (every device is gone).
    fn restart(&self) -> bool;
    /// Buffers it can reach (USB 1.1 controllers address only 4 GiB).
    fn dma(&self, bytes: usize) -> Option<Dma> {
        Dma::new(bytes)
    }
    /// Interrupts so far (for the System Report).
    fn interrupts(&self) -> u64 {
        0
    }
}

/// What we know about an attached device (for listing and teardown).
#[derive(Clone)]
pub struct DeviceRecord {
    pub dev: u8,
    pub pos: Position,
    pub desc: DeviceDescriptor,
    pub name: String,
    pub driver: String,
}

/// Run when a device goes away.
type DetachHook = Box<dyn Fn() + Send + Sync>;

/// One USB bus: a host controller and the devices on it.
pub struct Bus {
    pub index: usize,
    pub hcd: &'static dyn Hcd,
    devices: IrqMutex<Vec<DeviceRecord>>,
    detach: IrqMutex<Vec<(u8, DetachHook)>>,
    /// Interrupt pipes, for their periodic work (key repeat).
    pipes: IrqMutex<Vec<(u8, Arc<dyn PipeHandler>)>>,
    port_events: IrqMutex<VecDeque<u8>>,
    hub_events: IrqMutex<VecDeque<(u8, u32)>>,
    wake: WaitQueue,
    /// Set once the devices present at boot are set up: later ones are announced.
    settled: AtomicBool,
}

static BUSES: IrqMutex<Vec<&'static Bus>> = IrqMutex::new(Vec::new());

pub fn buses() -> Vec<&'static Bus> {
    BUSES.lock().clone()
}

/// Makes a started controller a bus: starts its `usb` task, which looks at
/// every root port first.
pub fn register(hcd: &'static dyn Hcd) -> &'static Bus {
    let mut all = BUSES.lock();
    let bus: &'static Bus = Box::leak(Box::new(Bus {
        index: all.len(),
        hcd,
        devices: IrqMutex::new(Vec::new()),
        detach: IrqMutex::new(Vec::new()),
        pipes: IrqMutex::new(Vec::new()),
        port_events: IrqMutex::new((1..=hcd.ports()).collect()),
        hub_events: IrqMutex::new(VecDeque::new()),
        wake: WaitQueue::new(),
        settled: AtomicBool::new(false),
    }));
    all.push(bus);
    drop(all);
    let kernel = crate::mm::vmm::kernel_pml4();
    crate::sched::spawn_task("usb", bus_task_entry, bus as *const Bus as u64, 0, kernel);
    start_ticks();
    bus
}

fn bus_task_entry(arg: u64) {
    unsafe { &*(arg as *const Bus) }.run();
}

/// An addressed and configured device, as class drivers see it.
pub struct UsbDevice {
    pub bus: &'static Bus,
    pub dev: u8,
    pub pos: Position,
    pub desc: DeviceDescriptor,
    pub config: Configuration,
    pub name: String,
}

impl UsbDevice {
    pub fn control(&self, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        self.bus.control(self.dev, setup, data)
    }
    pub fn speed(&self) -> Speed {
        self.pos.speed
    }
}

impl Bus {
    /// Wakes the `usb` task (a controller saw a port change).
    pub fn port_changed(&self, port: u8) {
        self.port_events.lock().push_back(port);
        self.wake.wake_all();
    }

    /// Wakes the `usb` task to look at the root ports (a controller saw a change).
    pub fn poke(&self) {
        self.wake.wake_all();
    }

    /// Called by the hub driver's status pipe.
    pub fn hub_changed(&self, dev: u8, bitmap: u32) {
        self.hub_events.lock().push_back((dev, bitmap));
        self.wake.wake_all();
    }

    fn run(&'static self) {
        loop {
            // Controllers without port-change interrupts are polled.
            for p in self.hcd.changed_ports() {
                self.port_events.lock().push_back(p);
            }
            // Each pop is its own statement, so the queue lock (interrupts
            // off) isn't held while a device is enumerated.
            loop {
                let Some(port) = self.port_events.lock().pop_front() else { break };
                self.root_port_changed(port);
            }
            loop {
                let Some((hub, bitmap)) = self.hub_events.lock().pop_front() else { break };
                hub::changed(self, hub, bitmap);
            }
            self.settled.store(true, Ordering::Release);
            self.wake.wait(250, || !self.port_events.lock().is_empty() || !self.hub_events.lock().is_empty());
        }
    }

    fn root_port_changed(&'static self, port: u8) {
        if port == 0 || port > self.hcd.ports() {
            return;
        }
        let connected = self.hcd.port_connected(port);
        let attached = self.has_device(port, 0);
        if !connected {
            if attached {
                self.detach_port(port, 0, 0);
            }
            return;
        }
        if attached {
            return;
        }
        let Some(speed) = self.hcd.port_reset(port) else { return };
        crate::sched::sleep_ms(10); // reset recovery
        self.enumerate(Position { root_port: port, route: 0, depth: 0, speed, tt: None });
    }

    /// Is there a device at this position (root port + route)?
    pub fn has_device(&self, root_port: u8, route: u32) -> bool {
        self.devices.lock().iter().any(|d| d.pos.root_port == root_port && d.pos.route == route)
    }

    /// The device at root `port` + `route` (whose route has `depth` tiers) and
    /// everything behind it are gone. `depth` 0 means the whole root port.
    pub fn detach_port(&self, port: u8, route: u32, depth: u32) {
        let mask = if depth == 0 { 0 } else { (1u32 << (4 * depth)) - 1 };
        let gone: Vec<DeviceRecord> = {
            let mut devs = self.devices.lock();
            let (gone, keep): (Vec<_>, Vec<_>) =
                devs.drain(..).partition(|d| d.pos.root_port == port && d.pos.route & mask == route & mask);
            *devs = keep;
            gone
        };
        for d in gone {
            log!("usb", "{} disconnected", d.name);
            self.forget(d.dev);
            self.hcd.release(d.dev);
            if d.driver != "hub" {
                crate::gui::notify::system("USB device removed", &d.name);
            }
        }
    }

    /// Runs a device's unplug hooks and drops its pipes.
    fn forget(&self, dev: u8) {
        let hooks: Vec<_> = {
            let mut all = self.detach.lock();
            let (mine, rest): (Vec<_>, Vec<_>) = all.drain(..).partition(|(d, _)| *d == dev);
            *all = rest;
            mine
        };
        for (_, hook) in hooks {
            hook();
        }
        self.pipes.lock().retain(|(d, _)| *d != dev);
    }

    /// Plugged in while running (not present at boot): worth a notification.
    pub fn announce(&self) -> bool {
        self.settled.load(Ordering::Acquire)
    }

    /// Runs `hook` when the device is unplugged.
    pub fn on_detach(&self, dev: u8, hook: Box<dyn Fn() + Send + Sync>) {
        self.detach.lock().push((dev, hook));
    }

    pub fn control(&self, dev: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        let r = self.hcd.control(dev, setup, data);
        // CLEAR_FEATURE(ENDPOINT_HALT): both ends start the endpoint's toggle over.
        if r.is_ok() && setup.request_type == 0x02 && setup.request == 1 && setup.value == 0 {
            self.hcd.reset_toggle(dev, setup.index as u8);
        }
        r
    }

    pub fn transfer(&self, dev: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8> {
        self.hcd.transfer(dev, ep, phys, len, timeout_ms)
    }

    pub fn configure(&self, dev: u8, eps: &[Endpoint], hub: Option<HubInfo>) -> bool {
        self.hcd.configure(dev, eps, hub)
    }

    pub fn open_pipe(&self, dev: u8, ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool {
        self.pipes.lock().push((dev, handler.clone()));
        kick_ticks();
        self.hcd.open_pipe(dev, ep, handler)
    }

    pub fn dma(&self, bytes: usize) -> Option<Dma> {
        self.hcd.dma(bytes)
    }

    /// Addresses and configures a new device, then hands it to a class driver.
    pub fn enumerate(&'static self, pos: Position) -> Option<u8> {
        let dev = self.hcd.new_device(&pos)?;
        let fail = |what: &str| {
            log!("usb", "port {}: {}", pos.root_port, what);
            self.hcd.release(dev);
            None
        };
        // The first 8 bytes tell the real packet size of endpoint 0.
        let mut head = [0u8; 8];
        if self.hcd.control(dev, Setup::get_descriptor(DESC_DEVICE, 0, 0, 8), Some(&mut head)).is_err() {
            return fail("device does not answer");
        }
        let real = if pos.speed as u8 >= Speed::Super as u8 { 1u16 << head[7].min(10) } else { head[7] as u16 };
        if real >= 8 && real != pos.speed.default_max_packet() {
            self.hcd.set_ep0_max_packet(dev, real);
        }
        let mut d = [0u8; 18];
        if self.hcd.control(dev, Setup::get_descriptor(DESC_DEVICE, 0, 0, 18), Some(&mut d)).is_err() {
            return fail("no device descriptor");
        }
        let Some(desc) = DeviceDescriptor::parse(&d) else { return fail("bad device descriptor") };
        let product = self.string(dev, desc.product_index);
        let maker = self.string(dev, desc.manufacturer_index);
        let name = match (maker.is_empty(), product.is_empty()) {
            (_, true) => format!("USB device {:04x}:{:04x}", desc.vendor, desc.product),
            (true, false) => product,
            (false, false) if product.starts_with(maker.as_str()) => product,
            (false, false) => format!("{maker} {product}"),
        };
        let mut head = [0u8; 9];
        if self.hcd.control(dev, Setup::get_descriptor(DESC_CONFIG, 0, 0, 9), Some(&mut head)).is_err() {
            return fail("no configuration descriptor");
        }
        let total = u16::from_le_bytes([head[2], head[3]]).clamp(9, 4096);
        let mut full = vec![0u8; total as usize];
        let Ok(n) = self.hcd.control(dev, Setup::get_descriptor(DESC_CONFIG, 0, 0, total), Some(&mut full)) else {
            return fail("configuration descriptor unreadable");
        };
        let Some(config) = Configuration::parse(&full[..n]) else { return fail("bad configuration descriptor") };
        if self.hcd.control(dev, Setup::set_configuration(config.value), None).is_err() {
            return fail("SET_CONFIGURATION failed");
        }
        let device = UsbDevice { bus: self, dev, pos, desc: desc.clone(), config, name: name.clone() };
        // Recorded first, so an unplug during the driver's setup still finds it.
        self.devices.lock().push(DeviceRecord { dev, pos, desc, name: name.clone(), driver: String::new() });
        let driver = attach(&device);
        log!(
            "usb",
            "{} ({}, {} port {}{}) — {}",
            name,
            pos.speed.name(),
            self.hcd.kind(),
            pos.root_port,
            if pos.route != 0 { format!(", route {:x}", pos.route) } else { String::new() },
            driver
        );
        if let Some(d) = self.devices.lock().iter_mut().find(|d| d.dev == dev) {
            d.driver = driver.clone();
        }
        if driver != "hub" && self.announce() {
            crate::gui::notify::system("USB device connected", &name);
        }
        Some(dev)
    }

    fn string(&self, dev: u8, index: u8) -> String {
        if index == 0 {
            return String::new();
        }
        let mut b = [0u8; 255];
        match self.hcd.control(dev, Setup::get_descriptor(DESC_STRING, index, 0x0409, 255), Some(&mut b)) {
            Ok(n) => parse_string(&b[..n]),
            Err(_) => String::new(),
        }
    }

    /// After sleep the controller was reset and every device lost power: drop
    /// them all (their drivers see an unplug), restart the controller, and
    /// enumerate what is attached now.
    pub fn resume(&self) {
        let all: Vec<DeviceRecord> = self.devices.lock().drain(..).collect();
        for d in &all {
            self.forget(d.dev);
        }
        self.hub_events.lock().clear();
        self.port_events.lock().clear();
        if !self.hcd.restart() {
            log!("usb", "{} controller {} did not come back after sleep", self.hcd.kind(), self.index);
            return;
        }
        self.settled.store(false, Ordering::Release);
        self.port_events.lock().extend(1..=self.hcd.ports());
        self.wake.wake_all();
    }

    pub fn describe_devices(&self) -> Vec<String> {
        self.devices
            .lock()
            .iter()
            .map(|d| {
                format!(
                    "Bus {:03} Port {:>2}{} {:04x}:{:04x} {} [{}, {}, {}]",
                    self.index + 1,
                    d.pos.root_port,
                    if d.pos.route != 0 { format!(".{:x}", d.pos.route) } else { String::new() },
                    d.desc.vendor,
                    d.desc.product,
                    d.name,
                    d.pos.speed.name(),
                    self.hcd.kind(),
                    d.driver
                )
            })
            .collect()
    }
}

// ------------------------------------------------------------------- ticks

static TICK_WAKE: WaitQueue = WaitQueue::new();
static TICK_KICKED: AtomicBool = AtomicBool::new(false);
static TICKS_STARTED: AtomicBool = AtomicBool::new(false);

fn start_ticks() {
    if !TICKS_STARTED.swap(true, Ordering::AcqRel) {
        crate::sched::spawn("usb-input", tick_task);
    }
}

/// Periodic work of every interrupt pipe (key repeat, touchpad taps and
/// gliding) and rumble timeouts, across all buses.
fn tick_task() {
    loop {
        let now = crate::time::uptime_ms();
        let mut next = 1000u64;
        let handlers: Vec<Arc<dyn PipeHandler>> =
            buses().iter().flat_map(|b| b.pipes.lock().iter().map(|(_, h)| h.clone()).collect::<Vec<_>>()).collect();
        for h in handlers {
            if let Some(ms) = h.tick(now) {
                next = next.min(ms.max(1));
            }
        }
        // Game controllers' rumble ends when its time is up.
        if let Some(ms) = crate::drivers::gamepad::tick(now) {
            next = next.min(ms.max(1));
        }
        TICK_WAKE.wait(next, || TICK_KICKED.load(Ordering::Acquire));
        TICK_KICKED.store(false, Ordering::Release);
    }
}

/// Wakes the tick task early (a key went down and wants repeating).
pub fn kick_ticks() {
    TICK_KICKED.store(true, Ordering::Release);
    TICK_WAKE.wake_all();
}

// ------------------------------------------------------------------ drivers

/// Starts class drivers for a device's interfaces; returns what took it.
pub fn attach(dev: &UsbDevice) -> String {
    if dev.desc.class == CLASS_HUB || dev.config.interfaces.iter().any(|i| i.class == CLASS_HUB) {
        return String::from(if hub::attach(dev) { "hub" } else { "hub (failed)" });
    }
    let mut drivers: Vec<&str> = Vec::new();
    for iface in dev.config.interfaces.iter().filter(|i| i.alternate == 0) {
        let taken = match (iface.class, iface.subclass, iface.protocol) {
            (CLASS_HID, ..) => hid::attach(dev, iface),
            // SCSI commands over bulk-only transport.
            (CLASS_MSC, 0x06, 0x50) => msc::attach(dev, iface),
            // Xbox controllers have their own (vendor) protocols.
            (0xFF, 0x5D, 0x01) | (0xFF, 0x47, 0xD0) => hid::attach_xbox(dev, iface),
            _ => None,
        };
        if let Some(name) = taken {
            if !drivers.contains(&name) {
                drivers.push(name);
            }
        }
    }
    if drivers.is_empty() {
        String::from("no driver")
    } else {
        drivers.join(", ")
    }
}

/// One line per attached device, for `lsusb` and the System Report.
pub fn list() -> Vec<String> {
    buses().iter().flat_map(|b| b.describe_devices()).collect()
}

/// One line per controller, for the System Report.
pub fn controllers() -> Vec<String> {
    buses()
        .iter()
        .map(|b| {
            format!(
                "Bus {:03}: {}, {} ports, {} interrupts",
                b.index + 1,
                b.hcd.kind(),
                b.hcd.ports(),
                b.hcd.interrupts()
            )
        })
        .collect()
}

/// Waits (up to `ms`) until the devices present at boot are set up.
pub fn settle(ms: u64) {
    let deadline = crate::time::uptime_ms() + ms;
    while crate::time::uptime_ms() < deadline && !buses().iter().all(|b| b.announce()) {
        crate::sched::sleep_ms(20);
    }
}

/// After sleep: every controller starts over and re-enumerates.
pub fn resume() {
    for b in buses() {
        b.resume();
    }
}

/// Finds the controllers. EHCI goes first: it takes every port, then hands
/// slow devices to its companion controllers (OHCI or UHCI), which start next.
pub fn init() {
    let devices = crate::drivers::pci::devices();
    let usb =
        |prog_if: u8| devices.iter().filter(move |d| d.class == 0x0C && d.subclass == 0x03 && d.prog_if == prog_if);
    for d in usb(0x30) {
        xhci::probe(d);
    }
    for d in usb(0x20) {
        ehci::probe(d);
    }
    for d in usb(0x10) {
        ohci::probe(d);
    }
    for d in usb(0x00) {
        uhci::probe(d);
    }
}
