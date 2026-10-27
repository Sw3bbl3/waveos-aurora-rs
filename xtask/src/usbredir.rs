//! Virtual USB devices for the kernel tests, plugged into the VM through
//! QEMU's `usb-redir`: we play the device side of the usbredir protocol
//! (normally a real device's host), so the VM sees a keyboard with media
//! keys, a Precision Touchpad and game controllers that QEMU can't emulate.
//!
//! Each device answers its descriptors and class requests and sends
//! scripted reports, repeating them so the tests can't miss them. It also
//! reacts to what the kernel sends, which tests the output side: the
//! touchpad only reports contacts once switched to touchpad mode, the
//! keyboard echoes its Caps Lock light back as a key, and the controllers
//! press a button once they feel rumble.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

// Packet types.
const HELLO: u32 = 0;
const DEVICE_CONNECT: u32 = 1;
const RESET: u32 = 3;
const INTERFACE_INFO: u32 = 4;
const EP_INFO: u32 = 5;
const SET_CONFIGURATION: u32 = 6;
const GET_CONFIGURATION: u32 = 7;
const CONFIGURATION_STATUS: u32 = 8;
const SET_ALT_SETTING: u32 = 9;
const GET_ALT_SETTING: u32 = 10;
const ALT_SETTING_STATUS: u32 = 11;
const START_INTERRUPT_RECEIVING: u32 = 15;
const STOP_INTERRUPT_RECEIVING: u32 = 16;
const INTERRUPT_RECEIVING_STATUS: u32 = 17;
const CONTROL_PACKET: u32 = 100;
const INTERRUPT_PACKET: u32 = 103;

// Capabilities we offer (no 64-bit ids: headers stay 12 bytes).
const CAP_CONNECT_DEVICE_VERSION: u32 = 1 << 1;
const CAP_EP_INFO_MAX_PACKET_SIZE: u32 = 1 << 4;

const STATUS_OK: u8 = 0;
const STATUS_STALL: u8 = 4;

/// Which device to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// N-key rollover keyboard with media keys (report ids 1 and 2).
    Keyboard,
    /// A Precision Touchpad (two contacts per report).
    Touchpad,
    /// Sony DualShock 4.
    DualShock4,
    /// Xbox 360 controller (vendor protocol).
    Xbox360,
}

pub const ALL: [Kind; 4] = [Kind::Keyboard, Kind::Touchpad, Kind::DualShock4, Kind::Xbox360];

struct Endpoint {
    address: u8,
    kind: u8, // 3 interrupt
    interval: u8,
    max_packet: u16,
}

struct Spec {
    vendor: u16,
    product: u16,
    class: (u8, u8, u8),
    name: &'static str,
    report_descriptor: Vec<u8>,
    endpoints: Vec<Endpoint>,
}

fn spec(kind: Kind) -> Spec {
    let hid = (3, 0, 0);
    let ep = |address, max_packet| Endpoint { address, kind: 3, interval: 4, max_packet };
    match kind {
        Kind::Keyboard => Spec {
            vendor: 0x1209,
            product: 0x0001,
            class: hid,
            name: "WaveOS Test Keyboard",
            report_descriptor: NKRO_KEYBOARD.to_vec(),
            endpoints: vec![ep(0x81, 32)],
        },
        Kind::Touchpad => Spec {
            vendor: 0x1209,
            product: 0x0002,
            class: hid,
            name: "WaveOS Test Touchpad",
            report_descriptor: precision_touchpad(),
            endpoints: vec![ep(0x81, 32)],
        },
        Kind::DualShock4 => Spec {
            vendor: 0x054C,
            product: 0x05C4,
            class: hid,
            name: "Wireless Controller",
            report_descriptor: DUALSHOCK4.to_vec(),
            endpoints: vec![ep(0x84, 64), ep(0x03, 64)],
        },
        Kind::Xbox360 => Spec {
            vendor: 0x045E,
            product: 0x028E,
            class: (0xFF, 0x5D, 0x01),
            name: "Controller",
            report_descriptor: Vec::new(),
            endpoints: vec![ep(0x81, 32), ep(0x01, 32)],
        },
    }
}

fn device_descriptor(s: &Spec) -> Vec<u8> {
    let mut d = vec![18, 1, 0x00, 0x02, 0, 0, 0, 64];
    d.extend_from_slice(&s.vendor.to_le_bytes());
    d.extend_from_slice(&s.product.to_le_bytes());
    d.extend_from_slice(&[0x00, 0x01, 1, 2, 0, 1]);
    d
}

fn config_descriptor(s: &Spec) -> Vec<u8> {
    let mut body = vec![9, 4, 0, 0, s.endpoints.len() as u8, s.class.0, s.class.1, s.class.2, 0];
    if s.class.0 == 3 {
        let len = s.report_descriptor.len() as u16;
        body.extend_from_slice(&[9, 0x21, 0x11, 0x01, 0, 1, 0x22, len as u8, (len >> 8) as u8]);
    }
    for e in &s.endpoints {
        body.extend_from_slice(&[7, 5, e.address, e.kind, e.max_packet as u8, (e.max_packet >> 8) as u8, e.interval]);
    }
    let total = 9 + body.len() as u16;
    let mut d = vec![9, 2, total as u8, (total >> 8) as u8, 1, 1, 0, 0x80, 50];
    d.extend(body);
    d
}

fn string_descriptor(text: &str) -> Vec<u8> {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut d = vec![(2 + units.len() * 2) as u8, 3];
    for u in units {
        d.extend_from_slice(&u.to_le_bytes());
    }
    d
}

/// What a device does over time and in response to the kernel.
struct Behaviour {
    kind: Kind,
    /// Touchpad: switched to reporting contacts (Input Mode = 3).
    touch_mode: bool,
    /// Keyboard: the Caps Lock light, as last set.
    caps_light: Option<bool>,
    /// Controllers: rumble felt.
    rumbled: bool,
    /// Xbox 360: its ring light was set.
    lit: bool,
}

impl Behaviour {
    /// Reports due `ms` into a 2-second cycle (sent on the first IN endpoint).
    fn reports(&self, ms: u64) -> Vec<Vec<u8>> {
        let t = ms % 2000;
        match self.kind {
            Kind::Keyboard => {
                let mut out = Vec::new();
                // Volume up pressed at 0 ms, released at 100 ms.
                if t < 50 {
                    out.push(vec![2, 0xE9, 0x00]);
                } else if t < 150 {
                    out.push(vec![2, 0x00, 0x00]);
                }
                // Once the kernel has lit Caps Lock, say so with F12 (usage 0x45).
                if self.caps_light == Some(true) && (500..550).contains(&t) {
                    let mut r = vec![0u8; 17];
                    r[0] = 1;
                    let bit = 8 + 0x45;
                    r[1 + bit / 8] |= 1 << (bit % 8);
                    out.push(r);
                } else if (600..650).contains(&t) {
                    let mut r = vec![0u8; 17];
                    r[0] = 1;
                    out.push(r);
                }
                out
            }
            Kind::Touchpad if self.touch_mode => {
                // One finger moves right 20 mm (0–400 ms), lifts; two fingers
                // scroll up 10 mm (800–1200 ms), lift; one quick tap (1600 ms).
                let one = |x: u16| touch_report(&[(1, true, x, 3000)], 1);
                let two = |y: u16| touch_report(&[(1, true, 4000, y), (2, true, 5000, y)], 2);
                let lift = || touch_report(&[(1, false, 0, 0)], 1);
                match t {
                    0..=400 => vec![one(3000 + (t as u16 / 20) * 100)],
                    401..=500 => vec![lift()],
                    800..=1200 => vec![two(4000 - ((t - 800) as u16 / 40) * 100)],
                    1201..=1300 => vec![lift()],
                    1600..=1650 => vec![one(5000)],
                    1651..=1750 => vec![lift()],
                    _ => Vec::new(),
                }
            }
            Kind::DualShock4 => {
                let mut r = vec![0u8; 64];
                r[0] = 0x01;
                r[1..5].copy_from_slice(&[0x80, 0x00, 0x80, 0x80]); // left stick up
                r[5] = 0x08 | 0x20; // hat centred, cross
                if self.rumbled {
                    r[5] |= 0x80; // triangle: "I felt that"
                }
                vec![r]
            }
            Kind::Xbox360 if self.lit => {
                let mut r = vec![0u8; 20];
                r[1] = 0x14;
                r[3] = 0x10; // A
                if self.rumbled {
                    r[3] |= 0x80; // Y: "I felt that"
                }
                r[6..8].copy_from_slice(&i16::MAX.to_le_bytes()); // left stick right
                vec![r]
            }
            _ => Vec::new(),
        }
    }

    /// Data the kernel sent to an OUT endpoint.
    fn output(&mut self, data: &[u8]) {
        match self.kind {
            Kind::Xbox360 => match data {
                [0x01, 0x03, _] => self.lit = true,
                [0x00, 0x08, 0x00, strong, weak, ..] if *strong > 0 || *weak > 0 => self.rumbled = true,
                _ => {}
            },
            Kind::DualShock4 if data.first() == Some(&0x05) && data.len() > 5 && (data[4] > 0 || data[5] > 0) => {
                self.rumbled = true
            }
            _ => {}
        }
    }

    /// A SET_REPORT control request.
    fn set_report(&mut self, kind: u8, data: &[u8]) {
        match (self.kind, kind) {
            (Kind::Touchpad, 3) if data == [3, 3] => self.touch_mode = true,
            (Kind::Keyboard, 2) if data.first() == Some(&1) => self.caps_light = data.get(1).map(|b| b & 0x02 != 0),
            (Kind::DualShock4, 2) => self.output(data),
            _ => {}
        }
    }
}

fn touch_report(contacts: &[(u8, bool, u16, u16)], count: u8) -> Vec<u8> {
    let mut r = vec![0u8; 15];
    r[0] = 1;
    for (slot, &(id, tip, x, y)) in contacts.iter().enumerate() {
        let base = 1 + slot * 5;
        r[base] = 0x01 | (tip as u8) << 1 | id << 2;
        r[base + 1..base + 3].copy_from_slice(&x.to_le_bytes());
        r[base + 3..base + 5].copy_from_slice(&y.to_le_bytes());
    }
    r[13] = count;
    r
}

struct Conn {
    stream: UnixStream,
    buf: Vec<u8>,
}

impl Conn {
    fn send(&mut self, kind: u32, id: u32, header: &[u8], data: &[u8]) -> std::io::Result<()> {
        let mut p = Vec::with_capacity(12 + header.len() + data.len());
        p.extend_from_slice(&kind.to_le_bytes());
        p.extend_from_slice(&((header.len() + data.len()) as u32).to_le_bytes());
        p.extend_from_slice(&id.to_le_bytes());
        p.extend_from_slice(header);
        p.extend_from_slice(data);
        self.stream.write_all(&p)
    }

    /// The next whole packet, if one has arrived.
    fn packet(&mut self) -> std::io::Result<Option<(u32, u32, Vec<u8>)>> {
        let mut tmp = [0u8; 4096];
        match self.stream.read(&mut tmp) {
            Ok(0) => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(e) => return Err(e),
        }
        if self.buf.len() < 12 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buf[4..8].try_into().unwrap()) as usize;
        if self.buf.len() < 12 + len {
            return Ok(None);
        }
        let kind = u32::from_le_bytes(self.buf[0..4].try_into().unwrap());
        let id = u32::from_le_bytes(self.buf[8..12].try_into().unwrap());
        let body = self.buf[12..12 + len].to_vec();
        self.buf.drain(..12 + len);
        Ok(Some((kind, id, body)))
    }
}

/// Serves one virtual device on a Unix socket QEMU connects to; returns the
/// socket's path. Runs until QEMU goes away.
pub fn start(kind: Kind, dir: &Path) -> PathBuf {
    let path = dir.join(format!("usbredir-{kind:?}.sock").to_lowercase());
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).expect("usbredir socket");
    std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            let _ = serve(kind, stream);
        }
    });
    path
}

fn serve(kind: Kind, stream: UnixStream) -> std::io::Result<()> {
    let s = spec(kind);
    stream.set_read_timeout(Some(Duration::from_millis(5)))?;
    let mut c = Conn { stream, buf: Vec::new() };
    let mut b = Behaviour { kind, touch_mode: false, caps_light: None, rumbled: false, lit: false };

    // Hello, then describe the device and plug it in.
    let mut hello = [0u8; 64];
    hello[..12].copy_from_slice(b"waveos-xtask");
    c.send(HELLO, 0, &hello, &(CAP_CONNECT_DEVICE_VERSION | CAP_EP_INFO_MAX_PACKET_SIZE).to_le_bytes())?;
    let mut info = vec![0u8; 4 + 32 * 4];
    info[0] = 1;
    info[4 + 32] = s.class.0;
    info[4 + 64] = s.class.1;
    info[4 + 96] = s.class.2;
    c.send(INTERFACE_INFO, 0, &info, &[])?;
    // type[32] (255: none), interval[32], interface[32], max_packet_size[32] (u16).
    let mut eps = vec![255u8; 32];
    eps.extend(vec![0u8; 64 + 64]);
    let index = |a: u8| ((a & 0x80) >> 3 | (a & 0x0F)) as usize;
    for i in [0usize, 16] {
        eps[i] = 0; // control
        eps[96 + i * 2] = 64;
    }
    for e in &s.endpoints {
        let i = index(e.address);
        eps[i] = e.kind;
        eps[32 + i] = e.interval;
        eps[64 + i] = 0;
        eps[96 + i * 2..96 + i * 2 + 2].copy_from_slice(&e.max_packet.to_le_bytes());
    }
    c.send(EP_INFO, 0, &eps, &[])?;
    // Full speed; the class is per interface.
    let mut connect = vec![1u8, 0, 0, 0];
    connect.extend_from_slice(&s.vendor.to_le_bytes());
    connect.extend_from_slice(&s.product.to_le_bytes());
    connect.extend_from_slice(&0x0100u16.to_le_bytes());
    c.send(DEVICE_CONNECT, 0, &connect, &[])?;

    let in_ep = s.endpoints.iter().find(|e| e.address & 0x80 != 0).map(|e| e.address).unwrap_or(0x81);
    let mut streaming: Option<Instant> = None;
    let mut next_id = 1u32;
    let mut last_sent = u64::MAX;
    loop {
        while let Some((kind, id, body)) = c.packet()? {
            match kind {
                RESET | HELLO => {}
                SET_CONFIGURATION => c.send(CONFIGURATION_STATUS, id, &[STATUS_OK, body[0]], &[])?,
                GET_CONFIGURATION => c.send(CONFIGURATION_STATUS, id, &[STATUS_OK, 1], &[])?,
                SET_ALT_SETTING => c.send(ALT_SETTING_STATUS, id, &[STATUS_OK, body[0], body[1]], &[])?,
                GET_ALT_SETTING => c.send(ALT_SETTING_STATUS, id, &[STATUS_OK, body[0], 0], &[])?,
                START_INTERRUPT_RECEIVING => {
                    if body[0] == in_ep {
                        streaming.get_or_insert_with(Instant::now);
                    }
                    c.send(INTERRUPT_RECEIVING_STATUS, id, &[STATUS_OK, body[0]], &[])?;
                }
                STOP_INTERRUPT_RECEIVING => c.send(INTERRUPT_RECEIVING_STATUS, id, &[STATUS_OK, body[0]], &[])?,
                CONTROL_PACKET => {
                    let (h, data) = body.split_at(10);
                    let (request, request_type) = (h[1], h[2]);
                    let value = u16::from_le_bytes([h[4], h[5]]);
                    let length = u16::from_le_bytes([h[8], h[9]]) as usize;
                    let reply: Option<Vec<u8>> = match (request_type, request, value >> 8) {
                        (0x80, 6, 1) => Some(device_descriptor(&s)),
                        (0x80, 6, 2) => Some(config_descriptor(&s)),
                        (0x80, 6, 3) => Some(match value & 0xFF {
                            0 => vec![4, 3, 0x09, 0x04],
                            1 => string_descriptor("WaveOS"),
                            _ => string_descriptor(s.name),
                        }),
                        (0x81, 6, 0x22) if !s.report_descriptor.is_empty() => Some(s.report_descriptor.clone()),
                        (0x80, 0, _) => Some(vec![0, 0]),
                        // HID class: SET_IDLE, SET_PROTOCOL; SET_REPORT carries data.
                        (0x21, 0x0A | 0x0B, _) => Some(Vec::new()),
                        (0x21, 0x09, kind) => {
                            b.set_report(kind as u8, data);
                            Some(Vec::new())
                        }
                        (0x00..=0x02, _, _) => Some(Vec::new()),
                        _ => None,
                    };
                    let mut hdr = h.to_vec();
                    match reply {
                        Some(mut r) => {
                            let in_dir = request_type & 0x80 != 0;
                            r.truncate(length);
                            hdr[3] = STATUS_OK;
                            let n = if in_dir { r.len() } else { data.len() };
                            hdr[8..10].copy_from_slice(&(n as u16).to_le_bytes());
                            c.send(CONTROL_PACKET, id, &hdr, if in_dir { &r } else { &[] })?;
                        }
                        None => {
                            hdr[3] = STATUS_STALL;
                            hdr[8..10].copy_from_slice(&0u16.to_le_bytes());
                            c.send(CONTROL_PACKET, id, &hdr, &[])?;
                        }
                    }
                }
                INTERRUPT_PACKET => {
                    // Data for an OUT endpoint: take it, say it went through.
                    let (h, data) = body.split_at(4);
                    b.output(data);
                    c.send(INTERRUPT_PACKET, id, &[h[0], STATUS_OK, 0, 0], &[])?;
                }
                _ => {}
            }
        }
        // Scripted reports, every 10 ms of the cycle at most once.
        if let Some(t0) = streaming {
            let ms = t0.elapsed().as_millis() as u64;
            let slot = ms / 10;
            if slot != last_sent {
                last_sent = slot;
                for r in b.reports(ms) {
                    let len = r.len() as u16;
                    c.send(INTERRUPT_PACKET, next_id, &[in_ep, STATUS_OK, len as u8, (len >> 8) as u8], &r)?;
                    next_id = next_id.wrapping_add(1);
                }
            }
        }
    }
}

/// QEMU arguments plugging the devices into `bus` behind hub port `hub`.
pub fn qemu_args(sockets: &[(Kind, PathBuf)], bus: &str, hub: &str) -> Vec<String> {
    let mut args = Vec::new();
    for (i, (_, path)) in sockets.iter().enumerate() {
        args.push(String::from("-chardev"));
        args.push(format!("socket,id=hidredir{i},path={}", path.display()));
        args.push(String::from("-device"));
        // Hub ports 1 and 2 hold the keyboard and (on UHCI) the tablet.
        args.push(format!("usb-redir,chardev=hidredir{i},id=hidredir{i},bus={bus},port={hub}.{}", i + 3));
    }
    args
}

// Report descriptors (the same devices as libs/hid's tests).

const NKRO_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01, 0x75,
    0x01, 0x95, 0x08, 0x81, 0x02, 0x19, 0x00, 0x29, 0x77, 0x95, 0x78, 0x81, 0x02, 0x05, 0x08, 0x19, 0x01, 0x29, 0x03,
    0x95, 0x03, 0x91, 0x02, 0x95, 0x05, 0x91, 0x01, 0xC0, 0x05, 0x0C, 0x09, 0x01, 0xA1, 0x01, 0x85, 0x02, 0x19, 0x00,
    0x2A, 0x3C, 0x02, 0x15, 0x00, 0x26, 0x3C, 0x02, 0x75, 0x10, 0x95, 0x01, 0x81, 0x00, 0xC0,
];

const DUALSHOCK4: &[u8] = &[
    0x05, 0x01, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x32, 0x09, 0x35, 0x15, 0x00, 0x26,
    0xFF, 0x00, 0x75, 0x08, 0x95, 0x04, 0x81, 0x02, 0x09, 0x39, 0x15, 0x00, 0x25, 0x07, 0x35, 0x00, 0x46, 0x3B, 0x01,
    0x65, 0x14, 0x75, 0x04, 0x95, 0x01, 0x81, 0x42, 0x65, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x0E, 0x15, 0x00, 0x25,
    0x01, 0x75, 0x01, 0x95, 0x0E, 0x81, 0x02, 0x06, 0x00, 0xFF, 0x09, 0x20, 0x75, 0x06, 0x95, 0x01, 0x15, 0x00, 0x25,
    0x7F, 0x81, 0x02, 0x05, 0x01, 0x09, 0x33, 0x09, 0x34, 0x15, 0x00, 0x26, 0xFF, 0x00, 0x75, 0x08, 0x95, 0x02, 0x81,
    0x02, 0x06, 0x00, 0xFF, 0x09, 0x21, 0x95, 0x36, 0x81, 0x02, 0x85, 0x05, 0x09, 0x22, 0x95, 0x1F, 0x91, 0x02, 0xC0,
];

fn precision_touchpad() -> Vec<u8> {
    let finger: &[u8] = &[
        0x05, 0x0D, 0x09, 0x22, 0xA1, 0x02, 0x15, 0x00, 0x25, 0x01, 0x09, 0x47, 0x09, 0x42, 0x95, 0x02, 0x75, 0x01,
        0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x25, 0x05, 0x09, 0x51, 0x81, 0x02, 0x75, 0x01, 0x95, 0x03, 0x81, 0x03,
        0x05, 0x01, 0x15, 0x00, 0x26, 0x10, 0x27, 0x75, 0x10, 0x55, 0x0E, 0x65, 0x11, 0x09, 0x30, 0x35, 0x00, 0x46,
        0xE8, 0x03, 0x95, 0x01, 0x81, 0x02, 0x26, 0x70, 0x17, 0x46, 0x58, 0x02, 0x09, 0x31, 0x81, 0x02, 0xC0,
    ];
    let mut d = vec![0x05, 0x0D, 0x09, 0x05, 0xA1, 0x01, 0x85, 0x01];
    d.extend_from_slice(finger);
    d.extend_from_slice(finger);
    d.extend_from_slice(&[
        0x05, 0x0D, 0x55, 0x0C, 0x66, 0x01, 0x10, 0x47, 0xFF, 0xFF, 0x00, 0x00, 0x27, 0xFF, 0xFF, 0x00, 0x00, 0x75,
        0x10, 0x95, 0x01, 0x09, 0x56, 0x81, 0x02, 0x09, 0x54, 0x25, 0x7F, 0x95, 0x01, 0x75, 0x08, 0x81, 0x02, 0x05,
        0x09, 0x09, 0x01, 0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0x05, 0x0D, 0x85,
        0x02, 0x09, 0x55, 0x09, 0x59, 0x75, 0x04, 0x95, 0x02, 0x25, 0x0F, 0xB1, 0x02, 0xC0, 0x05, 0x0D, 0x09, 0x0E,
        0xA1, 0x01, 0x85, 0x03, 0x09, 0x22, 0xA1, 0x02, 0x09, 0x52, 0x15, 0x00, 0x25, 0x0A, 0x75, 0x08, 0x95, 0x01,
        0xB1, 0x02, 0xC0, 0x09, 0x22, 0xA1, 0x00, 0x85, 0x04, 0x09, 0x57, 0x09, 0x58, 0x75, 0x01, 0x95, 0x02, 0x25,
        0x01, 0xB1, 0x02, 0x95, 0x06, 0xB1, 0x03, 0xC0, 0xC0, 0x05, 0x01, 0x09, 0x02, 0xA1, 0x01, 0x85, 0x05, 0x09,
        0x01, 0xA1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x02, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95,
        0x06, 0x81, 0x03, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7F, 0x75, 0x08, 0x95, 0x02, 0x81,
        0x06, 0xC0, 0xC0,
    ]);
    d
}
