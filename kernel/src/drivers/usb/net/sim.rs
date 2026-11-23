//! Simulated USB network devices, for the self-tests: QEMU emulates only
//! one USB network device (usb-net: RNDIS and CDC-ECM), so the CDC-NCM
//! driver and the ASIX chips' drivers run here against device models on a
//! simulated host controller.
//!
//! The controller ([`SimHcd`]) is a `Hcd` with three ports: plugging a
//! model in raises a port change, and the real `usb` task enumerates it —
//! descriptors, configuration choice, class or vendor driver — exactly as
//! on hardware. Behind each model sits a tiny network: a peer that answers
//! DHCP, ARP, ping and UDP echo (port 7). The models frame traffic as the
//! devices do (NCM transfer blocks with several frames each, the AX88179's
//! aggregate with its trailer, the AX88772's stream cut at odd places), so
//! batching and reassembly are exercised both ways.
//!
//! The models are written from the same understanding of the chips as the
//! drivers: they check the plumbing and the formats, not that a real chip
//! behaves this way (see docs/HARDWARE.md).

use crate::drivers::usb::{self, Endpoint, Hcd, HubInfo, PipeHandler, Position, Setup, Speed, STALL, TIMEOUT};
use crate::sync::{IrqMutex, WaitQueue};
use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use aurora_usbnet::asix::{self, ax179, ax772, mii};
use aurora_usbnet::{cdc, ncm};
use core::sync::atomic::{AtomicBool, Ordering};

pub const PORTS: u8 = 3;

/// Which device a port holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Model {
    /// A Realtek RTL8156 dock adapter: a vendor configuration first, then CDC-NCM.
    Ncm,
    Ax88179,
    Ax88772,
}

impl Model {
    pub fn port(self) -> u8 {
        match self {
            Model::Ncm => 1,
            Model::Ax88179 => 2,
            Model::Ax88772 => 3,
        }
    }
    /// The peer's address; we get .20 in the same /24.
    pub fn peer_ip(self) -> [u8; 4] {
        [192, 168, 80 + self.port(), 1]
    }
    pub fn client_ip(self) -> [u8; 4] {
        [192, 168, 80 + self.port(), 20]
    }
    pub fn mac(self) -> [u8; 6] {
        match self {
            Model::Ncm => [0x00, 0xE0, 0x4C, 0x68, 0x00, 0x01],
            Model::Ax88179 => [0x00, 0x0E, 0xC6, 0x81, 0x79, 0x01],
            Model::Ax88772 => [0x00, 0x0E, 0xC6, 0x87, 0x72, 0x01],
        }
    }
    /// Link speed the device reports, Mb/s.
    pub fn speed(self) -> u32 {
        match self {
            Model::Ncm => 2500,
            Model::Ax88179 => 1000,
            Model::Ax88772 => 100,
        }
    }
    fn bus_speed(self) -> Speed {
        match self {
            Model::Ax88772 => Speed::High,
            _ => Speed::Super,
        }
    }
}

const PEER_MAC: [u8; 6] = [0x02, 0x00, 0x5E, 0x00, 0x00, 0x01];

/// What a model saw, for the tests.
#[derive(Default, Clone, Debug)]
pub struct Seen {
    pub frames_out: usize,
    /// The most frames one transfer from the host carried.
    pub most_per_transfer: usize,
    pub transfers_in: usize,
    pub frames_in: usize,
    /// Vendor registers written (AX88179: RX_CTL with START).
    pub receiver_started: bool,
    pub bad_transfers: usize,
}

struct Device {
    model: Model,
    /// Frames from the peer, framed into IN transfers.
    inbound: VecDeque<Vec<u8>>,
    seen: Seen,
    /// AX88772: the device's stream, before it is cut into transfers.
    stream772: Vec<u8>,
}

pub struct SimHcd {
    ports: [IrqMutex<Option<Device>>; PORTS as usize],
    connected: [AtomicBool; PORTS as usize],
    changed: [AtomicBool; PORTS as usize],
    wake: WaitQueue,
}

static SIM: spin::Once<&'static SimHcd> = spin::Once::new();
static BUS: spin::Once<&'static usb::Bus> = spin::Once::new();

/// The simulated controller (registered as a USB bus the first time).
pub fn controller() -> &'static SimHcd {
    let hcd = *SIM.call_once(|| {
        Box::leak(Box::new(SimHcd {
            ports: [const { IrqMutex::new(None) }; PORTS as usize],
            connected: Default::default(),
            changed: Default::default(),
            wake: WaitQueue::new(),
        }))
    });
    BUS.call_once(|| usb::register(hcd));
    hcd
}

impl SimHcd {
    /// Plugs `model` in (its port changes; the bus task enumerates it).
    pub fn plug(&self, model: Model) {
        let p = model.port() as usize - 1;
        *self.ports[p].lock() =
            Some(Device { model, inbound: VecDeque::new(), seen: Seen::default(), stream772: Vec::new() });
        self.connected[p].store(true, Ordering::Release);
        self.changed[p].store(true, Ordering::Release);
        if let Some(bus) = BUS.get() {
            bus.port_changed(model.port());
        }
    }

    pub fn unplug(&self, model: Model) {
        let p = model.port() as usize - 1;
        self.connected[p].store(false, Ordering::Release);
        *self.ports[p].lock() = None;
        self.wake.wake_all();
        if let Some(bus) = BUS.get() {
            bus.port_changed(model.port());
        }
    }

    pub fn seen(&self, model: Model) -> Seen {
        self.ports[model.port() as usize - 1].lock().as_ref().map(|d| d.seen.clone()).unwrap_or_default()
    }

    fn with<R>(&self, dev: u8, f: impl FnOnce(&mut Device) -> R) -> Option<R> {
        let p = (dev as usize).checked_sub(1)?;
        self.ports.get(p)?.lock().as_mut().map(f)
    }
}

// ----------------------------------------------------------- descriptors

fn device_descriptor(vendor: u16, product: u16, configs: u8, mps0: u8) -> Vec<u8> {
    let mut d = vec![18, usb::DESC_DEVICE, 0x00, 0x02, 0, 0, 0, mps0];
    d.extend_from_slice(&vendor.to_le_bytes());
    d.extend_from_slice(&product.to_le_bytes());
    d.extend_from_slice(&[0x00, 0x01, 1, 2, 0, configs]);
    d
}

fn interface(number: u8, alt: u8, eps: u8, class: u8, sub: u8, proto: u8) -> Vec<u8> {
    vec![9, usb::DESC_INTERFACE, number, alt, eps, class, sub, proto, 0]
}

fn endpoint(address: u8, kind: u8, mps: u16, interval: u8) -> Vec<u8> {
    let m = mps.to_le_bytes();
    vec![7, usb::DESC_ENDPOINT, address, kind, m[0], m[1], interval]
}

fn configuration(value: u8, interfaces: u8, body: &[u8]) -> Vec<u8> {
    let total = (9 + body.len()) as u16;
    let t = total.to_le_bytes();
    let mut c = vec![9, usb::DESC_CONFIG, t[0], t[1], interfaces, value, 0, 0x80, 50];
    c.extend_from_slice(body);
    c
}

/// A string descriptor (UTF-16LE).
fn string(text: &str) -> Vec<u8> {
    let mut s = vec![0, usb::DESC_STRING];
    for u in text.encode_utf16() {
        s.extend_from_slice(&u.to_le_bytes());
    }
    s[0] = s.len() as u8;
    s
}

fn vendor_config(value: u8, bulk_mps: u16) -> Vec<u8> {
    let mut body = interface(0, 0, 3, 0xFF, 0xFF, 0);
    body.extend(endpoint(0x81, 3, 8, 11));
    body.extend(endpoint(0x82, 2, bulk_mps, 0));
    body.extend(endpoint(0x03, 2, bulk_mps, 0));
    configuration(value, 1, &body)
}

fn ncm_config(value: u8) -> Vec<u8> {
    let mut body = interface(0, 0, 1, cdc::CLASS_COMM, cdc::SUBCLASS_NCM, 0);
    body.extend_from_slice(&[5, cdc::CS_INTERFACE, cdc::FD_HEADER, 0x10, 0x01]);
    body.extend_from_slice(&[5, cdc::CS_INTERFACE, cdc::FD_UNION, 0, 1]);
    body.extend_from_slice(&[13, cdc::CS_INTERFACE, cdc::FD_ETHERNET, 3, 0, 0, 0, 0, 0xEA, 0x05, 0, 0, 0]);
    body.extend_from_slice(&[6, cdc::CS_INTERFACE, cdc::FD_NCM, 0x00, 0x01, cdc::NCM_CAP_PACKET_FILTER]);
    body.extend(endpoint(0x81, 3, 16, 8));
    body.extend(interface(1, 0, 0, cdc::CLASS_DATA, 0, 1));
    body.extend(interface(1, 1, 2, cdc::CLASS_DATA, 0, 1));
    body.extend(endpoint(0x82, 2, 1024, 0));
    body.extend(endpoint(0x01, 2, 1024, 0));
    configuration(value, 2, &body)
}

/// The NTB parameters the simulated NCM function states.
fn ntb_parameters() -> [u8; 28] {
    let mut p = [0u8; 28];
    p[0..2].copy_from_slice(&28u16.to_le_bytes());
    p[2..4].copy_from_slice(&3u16.to_le_bytes()); // NTB16 and NTB32
    p[4..8].copy_from_slice(&16384u32.to_le_bytes());
    p[8..10].copy_from_slice(&4u16.to_le_bytes());
    p[12..14].copy_from_slice(&4u16.to_le_bytes());
    p[16..20].copy_from_slice(&16384u32.to_le_bytes());
    p[20..22].copy_from_slice(&4u16.to_le_bytes());
    p[22..24].copy_from_slice(&2u16.to_le_bytes()); // datagrams at 2 mod 4
    p[24..26].copy_from_slice(&4u16.to_le_bytes());
    p
}

// -------------------------------------------------------------- the peer

fn checksum(b: &[u8]) -> u16 {
    crate::drivers::wifi::sim::checksum(b)
}

fn ethernet(dst: [u8; 6], ethertype: u16, payload: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(14 + payload.len());
    f.extend_from_slice(&dst);
    f.extend_from_slice(&PEER_MAC);
    f.extend_from_slice(&ethertype.to_be_bytes());
    f.extend_from_slice(payload);
    f.resize(f.len().max(60), 0);
    f
}

fn ipv4(src: [u8; 4], dst: [u8; 4], proto: u8, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 20];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&((20 + payload.len()) as u16).to_be_bytes());
    p[8] = 64;
    p[9] = proto;
    p[12..16].copy_from_slice(&src);
    p[16..20].copy_from_slice(&dst);
    let c = checksum(&p);
    p[10..12].copy_from_slice(&c.to_be_bytes());
    p.extend_from_slice(payload);
    p
}

/// What the network behind a model answers to one frame from the host.
fn peer(model: Model, f: &[u8]) -> Vec<Vec<u8>> {
    if f.len() < 14 {
        return Vec::new();
    }
    let host: [u8; 6] = f[6..12].try_into().unwrap();
    let ethertype = u16::from_be_bytes([f[12], f[13]]);
    let p = &f[14..];
    let me = model.peer_ip();
    match ethertype {
        // ARP request for the peer.
        0x0806 if p.len() >= 28 && p[7] == 1 && p[24..28] == me => {
            let mut r = vec![0, 1, 8, 0, 6, 4, 0, 2];
            r.extend_from_slice(&PEER_MAC);
            r.extend_from_slice(&me);
            r.extend_from_slice(&p[8..18]);
            vec![ethernet(host, 0x0806, &r)]
        }
        0x0800 if p.len() >= 20 => {
            let ihl = (p[0] & 0xF) as usize * 4;
            let total = (u16::from_be_bytes([p[2], p[3]]) as usize).min(p.len());
            if ihl < 20 || total < ihl {
                return Vec::new();
            }
            let src: [u8; 4] = p[12..16].try_into().unwrap();
            let dst: [u8; 4] = p[16..20].try_into().unwrap();
            let body = &p[ihl..total];
            if let Some(reply) = crate::drivers::wifi::sim::dhcp_reply(f, me, model.client_ip()) {
                return vec![ethernet([0xFF; 6], 0x0800, &reply)];
            }
            if dst != me {
                return Vec::new();
            }
            match p[9] {
                // Echo request → reply.
                1 if body.len() >= 8 && body[0] == 8 => {
                    let mut r = body.to_vec();
                    r[0] = 0;
                    r[2..4].fill(0);
                    let c = checksum(&r);
                    r[2..4].copy_from_slice(&c.to_be_bytes());
                    vec![ethernet(host, 0x0800, &ipv4(me, src, 1, &r))]
                }
                // UDP echo (port 7): the datagram comes back.
                17 if body.len() >= 8 && body[2..4] == [0, 7] => {
                    let mut r = body.to_vec();
                    r[0..2].copy_from_slice(&body[2..4]);
                    r[2..4].copy_from_slice(&body[0..2]);
                    r[6..8].fill(0); // no checksum
                    vec![ethernet(host, 0x0800, &ipv4(me, src, 17, &r))]
                }
                _ => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

// ------------------------------------------------------------- framing

impl Device {
    /// The host sent a transfer: take its frames, answer them.
    fn bulk_out(&mut self, data: &[u8]) {
        let mut frames = Vec::new();
        match self.model {
            Model::Ncm => match ncm::parse(data, aurora_usbnet::MAX_FRAME) {
                Ok(r) => frames.extend(r.into_iter().map(|r| data[r].to_vec())),
                Err(_) => self.seen.bad_transfers += 1,
            },
            Model::Ax88179 => {
                // 8-byte headers, back to back.
                let mut at = 0;
                while at + 8 <= data.len() {
                    let len = u32::from_le_bytes(data[at..at + 4].try_into().unwrap()) as usize;
                    if len == 0 || at + 8 + len > data.len() {
                        self.seen.bad_transfers += 1;
                        break;
                    }
                    frames.push(data[at + 8..at + 8 + len].to_vec());
                    at += 8 + len;
                }
            }
            Model::Ax88772 => {
                // One packet per transfer; the header gives its length (any
                // padding word after it is ignored, as the chip does).
                if data.len() >= 4 {
                    let h = u32::from_le_bytes(data[..4].try_into().unwrap());
                    let len = (h & 0x7FF) as usize;
                    if len == ((!h >> 16) & 0x7FF) as usize && 4 + len <= data.len() {
                        frames.push(data[4..4 + len].to_vec());
                    } else {
                        self.seen.bad_transfers += 1;
                    }
                }
            }
        }
        self.seen.frames_out += frames.len();
        self.seen.most_per_transfer = self.seen.most_per_transfer.max(frames.len());
        let replies: Vec<Vec<u8>> = frames.iter().flat_map(|f| peer(self.model, f)).collect();
        if !replies.is_empty() {
            self.frame_in(&replies);
        }
    }

    /// Frames from the peer into IN transfers, as the device would batch them.
    fn frame_in(&mut self, frames: &[Vec<u8>]) {
        self.seen.frames_in += frames.len();
        match self.model {
            Model::Ncm => {
                let params = ncm::Params::parse(&ntb_parameters()).unwrap();
                let layout = params.out_layout(false, 16384);
                let mut buf = vec![0u8; layout.max];
                let mut b = ncm::Builder::new(layout);
                for f in frames {
                    if !b.push(&mut buf, f) {
                        let n = b.finish(&mut buf, 0);
                        self.inbound.push_back(buf[..n].to_vec());
                        b.push(&mut buf, f);
                    }
                }
                if !b.is_empty() {
                    let n = b.finish(&mut buf, 0);
                    self.inbound.push_back(buf[..n].to_vec());
                }
            }
            Model::Ax88179 => {
                let mut block = Vec::new();
                let mut headers = Vec::new();
                for f in frames {
                    headers.push(((f.len() + 2) as u32) << 16);
                    block.extend_from_slice(&[0, 0]);
                    block.extend_from_slice(f);
                    block.resize((block.len() + 7) & !7, 0);
                }
                let table = block.len();
                for h in &headers {
                    block.extend_from_slice(&h.to_le_bytes());
                }
                block.extend_from_slice(&((table as u32) << 16 | headers.len() as u32).to_le_bytes());
                self.inbound.push_back(block);
            }
            Model::Ax88772 => {
                for f in frames {
                    let len = f.len() as u32;
                    self.stream772.extend_from_slice(&((!len & 0xFFFF) << 16 | len).to_le_bytes());
                    self.stream772.extend_from_slice(f);
                    if f.len() % 2 == 1 {
                        self.stream772.push(0);
                    }
                }
                // Cut at awkward places: frames and headers span transfers.
                while !self.stream772.is_empty() {
                    let n = self.stream772.len().min(700);
                    self.inbound.push_back(self.stream772.drain(..n).collect());
                }
            }
        }
    }

    /// A class or vendor request.
    fn request(&mut self, s: &Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        fn answer(data: Option<&mut [u8]>, bytes: &[u8]) -> Result<usize, u8> {
            let d = data.ok_or(STALL)?;
            let n = bytes.len().min(d.len());
            d[..n].copy_from_slice(&bytes[..n]);
            Ok(n)
        }
        match self.model {
            Model::Ncm => match (s.request_type, s.request) {
                (0xA1, cdc::GET_NTB_PARAMETERS) => answer(data, &ntb_parameters()),
                (0x21, cdc::SET_NTB_FORMAT | cdc::SET_NTB_INPUT_SIZE | cdc::SET_ETHERNET_PACKET_FILTER) => Ok(0),
                _ => Err(STALL),
            },
            Model::Ax88179 => match (s.request_type, s.request, s.value) {
                (asix::READ, ax179::ACCESS_MAC, ax179::NODE_ID) => answer(data, &self.model.mac()),
                (asix::READ, ax179::ACCESS_PHY, _) if s.index == ax179::PHY_PHYSR => {
                    answer(data, &(ax179::PHYSR_GIGA | ax179::PHYSR_FULL | ax179::PHYSR_LINK).to_le_bytes())
                }
                (asix::WRITE, ax179::ACCESS_MAC, ax179::RX_CTL) => {
                    if let Some(d) = data {
                        let v = u16::from_le_bytes([d[0], d.get(1).copied().unwrap_or(0)]);
                        self.seen.receiver_started = v & ax179::RX_CTL_START != 0;
                    }
                    Ok(2)
                }
                (asix::WRITE, ax179::ACCESS_MAC | ax179::ACCESS_PHY, _) => Ok(s.length as usize),
                _ => Err(STALL),
            },
            Model::Ax88772 => match (s.request_type, s.request) {
                (asix::READ, ax772::READ_PHY_ID) => answer(data, &[0xE0, ax772::EMBEDDED_PHY]),
                (asix::READ, ax772::READ_NODE_ID) => answer(data, &self.model.mac()),
                (asix::READ, ax772::READ_MII_REG) => {
                    let v = match s.index {
                        mii::BMSR => mii::BMSR_LSTATUS,
                        mii::ADVERTISE => mii::ADVERTISE_ALL,
                        mii::LPA => 0x45E1,
                        _ => 0,
                    };
                    answer(data, &v.to_le_bytes())
                }
                (asix::WRITE, ax772::WRITE_RX_CTL) => {
                    self.seen.receiver_started = s.value & ax772::RX_CTL_SO != 0;
                    Ok(0)
                }
                (asix::WRITE, _) => Ok(s.length as usize),
                _ => Err(STALL),
            },
        }
    }

    fn link_report(&self) -> Vec<u8> {
        match self.model {
            Model::Ncm => {
                // Connected, then 2.5 Gb/s both ways.
                let mut r = vec![0xA1, cdc::NOTIFY_NETWORK_CONNECTION, 1, 0, 0, 0, 0, 0];
                r.extend_from_slice(&[0xA1, cdc::NOTIFY_SPEED_CHANGE, 0, 0, 0, 0, 8, 0]);
                r.extend_from_slice(&2_500_000_000u32.to_le_bytes());
                r.extend_from_slice(&2_500_000_000u32.to_le_bytes());
                r
            }
            Model::Ax88179 => vec![0xA1, 0, 1, 0, 0, 0, 0, 0],
            Model::Ax88772 => vec![0xA1, 0, 1, 0, 0, 0, 0, 0],
        }
    }
}

impl Hcd for SimHcd {
    fn kind(&self) -> &'static str {
        "Simulated"
    }

    fn ports(&self) -> u8 {
        PORTS
    }

    fn changed_ports(&self) -> Vec<u8> {
        (1..=PORTS).filter(|&p| self.changed[p as usize - 1].swap(false, Ordering::AcqRel)).collect()
    }

    /// Plugging in raises a port change itself: no polling.
    fn polls_ports(&self) -> bool {
        false
    }

    fn port_connected(&self, port: u8) -> bool {
        self.connected.get(port as usize - 1).is_some_and(|c| c.load(Ordering::Acquire))
    }

    fn port_reset(&self, port: u8) -> Option<Speed> {
        self.ports.get(port as usize - 1)?.lock().as_ref().map(|d| d.model.bus_speed())
    }

    fn new_device(&self, pos: &Position) -> Option<u8> {
        self.port_connected(pos.root_port).then_some(pos.root_port)
    }

    fn set_ep0_max_packet(&self, _dev: u8, _max_packet: u16) {}

    fn control(&self, dev: u8, setup: Setup, data: Option<&mut [u8]>) -> Result<usize, u8> {
        let model = self.with(dev, |d| d.model).ok_or(TIMEOUT)?;
        let desc = |bytes: Vec<u8>, data: Option<&mut [u8]>| -> Result<usize, u8> {
            let d = data.ok_or(STALL)?;
            let n = bytes.len().min(d.len()).min(setup.length as usize);
            d[..n].copy_from_slice(&bytes[..n]);
            Ok(n)
        };
        match (setup.request_type, setup.request) {
            (0x80, 6) => {
                let index = setup.value as u8;
                let bytes = match ((setup.value >> 8) as u8, model) {
                    (usb::DESC_DEVICE, Model::Ncm) => device_descriptor(0x0BDA, 0x8156, 2, 9),
                    (usb::DESC_DEVICE, Model::Ax88179) => device_descriptor(0x0B95, 0x1790, 1, 9),
                    (usb::DESC_DEVICE, Model::Ax88772) => device_descriptor(0x0B95, 0x772B, 1, 64),
                    (usb::DESC_CONFIG, Model::Ncm) if index == 0 => vendor_config(1, 1024),
                    (usb::DESC_CONFIG, Model::Ncm) if index == 1 => ncm_config(2),
                    (usb::DESC_CONFIG, Model::Ax88179) if index == 0 => vendor_config(1, 1024),
                    (usb::DESC_CONFIG, Model::Ax88772) if index == 0 => vendor_config(1, 512),
                    (usb::DESC_STRING, _) => match index {
                        0 => vec![4, usb::DESC_STRING, 0x09, 0x04],
                        1 => string("WaveOS Simulated"),
                        2 => string(match model {
                            Model::Ncm => "USB 2.5GbE Dock Adapter",
                            Model::Ax88179 => "AX88179 Gigabit Ethernet",
                            Model::Ax88772 => "AX88772B 10/100 Ethernet",
                        }),
                        3 => {
                            let m = model.mac();
                            let text: String = m.iter().map(|b| alloc::format!("{b:02X}")).collect();
                            string(&text)
                        }
                        _ => return Err(STALL),
                    },
                    _ => return Err(STALL),
                };
                desc(bytes, data)
            }
            // SET_CONFIGURATION, SET_INTERFACE, CLEAR_FEATURE.
            (0x00, 9) | (0x01, 11) | (0x02, 1) => Ok(0),
            _ => self.with(dev, |d| d.request(&setup, data)).ok_or(TIMEOUT)?,
        }
    }

    fn transfer(&self, dev: u8, ep: &Endpoint, phys: u64, len: usize, timeout_ms: u64) -> Result<usize, u8> {
        let buf = unsafe { core::slice::from_raw_parts_mut(crate::mm::phys_to_virt(phys) as *mut u8, len) };
        if !ep.is_in() {
            // Bus time: frames queued meanwhile batch into the next transfer.
            crate::sched::sleep_ms(1);
            if len > 0 {
                self.with(dev, |d| d.bulk_out(buf)).ok_or(ERROR_GONE)?;
            }
            self.wake.wake_all();
            return Ok(len);
        }
        let gone = || self.with(dev, |_| ()).is_none();
        self.wake.wait(timeout_ms, || gone() || self.with(dev, |d| !d.inbound.is_empty()).unwrap_or(false));
        if gone() {
            return Err(ERROR_GONE);
        }
        let Some(data) = self.with(dev, |d| d.inbound.pop_front()).flatten() else { return Err(TIMEOUT) };
        let n = data.len().min(len);
        buf[..n].copy_from_slice(&data[..n]);
        self.with(dev, |d| d.seen.transfers_in += 1);
        Ok(n)
    }

    fn configure(&self, _dev: u8, _eps: &[Endpoint], _hub: Option<HubInfo>) -> bool {
        true
    }

    fn open_pipe(&self, dev: u8, _ep: &Endpoint, handler: Arc<dyn PipeHandler>) -> bool {
        // The device reports its link as soon as it is polled.
        let report = self.with(dev, |d| d.link_report());
        match report {
            Some(r) => {
                handler.data(&r);
                true
            }
            None => false,
        }
    }

    fn release(&self, dev: u8) {
        self.with(dev, |d| d.inbound.clear());
        self.wake.wake_all();
    }

    fn restart(&self) -> bool {
        true
    }
}

/// What a vanished device's transfers fail with.
const ERROR_GONE: u8 = usb::ERROR;
