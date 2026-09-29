//! USB: the xHCI host controller driver ([`xhci`]) and class drivers for
//! hubs ([`hub`]), keyboards and pointing devices ([`hid`]) and storage
//! ([`msc`]).
//!
//! Every controller has two kernel tasks. `xhci-events` drains the event
//! ring: it completes waiting transfers and commands, feeds interrupt pipes
//! (key presses, pointer motion, hub status) to their drivers, and reports
//! port changes. `usb` handles those changes: it enumerates new devices —
//! addressing them, reading descriptors, picking a class driver — and tears
//! down unplugged ones. Transfers may be issued from any task; the
//! controller lock is held only while TRBs are queued, never while waiting.

pub mod hid;
pub mod hub;
pub mod msc;
pub mod xhci;

use alloc::string::String;
use alloc::vec::Vec;

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
    let units: Vec<u16> = b[2..n].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    String::from_utf16_lossy(&units).trim().into()
}

/// An addressed and configured device, as class drivers see it.
pub struct UsbDevice {
    pub ctrl: &'static xhci::Controller,
    pub slot: u8,
    pub speed: Speed,
    pub root_port: u8,
    /// Hub ports on the way to it, 4 bits per tier (0 on a root port).
    pub route: u32,
    /// Hubs between it and the root port.
    pub depth: u32,
    pub desc: DeviceDescriptor,
    pub config: Configuration,
    pub name: String,
}

impl UsbDevice {
    pub fn control(&self, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        self.ctrl.control(self.slot, setup, data)
    }
}

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
    xhci::controllers().iter().flat_map(|c| c.describe_devices()).collect()
}

/// After sleep: every controller starts over and re-enumerates.
pub fn resume() {
    for c in xhci::controllers() {
        c.resume();
    }
}

pub fn init() {
    for d in crate::drivers::pci::devices() {
        if d.class == 0x0C && d.subclass == 0x03 && d.prog_if == 0x30 {
            xhci::probe(&d);
        }
    }
}
