//! What udev would say about input devices, without a udev daemon: the
//! properties its rules give them (`input_id`, `path_id`, `usb_id`:
//! `ID_INPUT_JOYSTICK=1`, `ID_PATH`, `ID_SERIAL`, …), as the records in
//! `/run/udev/data` and in the messages libudev's monitors receive.
//!
//! The kernel sends both kinds of hot-plug message itself
//! ([`super::super::uevent`]): the kernel's own (`add@/devices/…`, group
//! one) and, with the same properties udev would add, udev's (group two,
//! with a `libudev` header). So libudev monitors and enumerators (SDL's,
//! Firefox's) work as with udev running.

use super::tree::{self, Dev};
use crate::drivers::evdev::hidraw::Hidraw;
use crate::drivers::evdev::{codes::*, Class, Device, Location};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// When the "udev database" was written (µs since boot): when the device came.
fn usec() -> u64 {
    crate::time::now_ns() / 1000
}

/// The properties udev's rules give an input device (or one of its nodes).
pub fn properties(d: &Device, node: Option<Dev>) -> Vec<(String, String)> {
    let mut p: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: String| p.push((String::from(k), v));
    set("ID_INPUT", String::from("1"));
    match d.class {
        Class::Keyboard => {
            set("ID_INPUT_KEY", String::from("1"));
            set("ID_INPUT_KEYBOARD", String::from("1"));
        }
        Class::Key => set("ID_INPUT_KEY", String::from("1")),
        Class::Mouse | Class::Tablet => set("ID_INPUT_MOUSE", String::from("1")),
        Class::Touchpad => {
            set("ID_INPUT_TOUCHPAD", String::from("1"));
            for (key, code) in [("ID_INPUT_WIDTH_MM", ABS_X), ("ID_INPUT_HEIGHT_MM", ABS_Y)] {
                let a = d.abs_info(code, true);
                if a.resolution > 0 {
                    set(key, format!("{}", (a.maximum - a.minimum) / a.resolution));
                }
            }
        }
        Class::Joystick => set("ID_INPUT_JOYSTICK", String::from("1")),
    }
    match &d.location {
        Location::Usb(u) => {
            set("ID_BUS", String::from("usb"));
            set("ID_VENDOR_ID", format!("{:04x}", u.vendor));
            set("ID_MODEL_ID", format!("{:04x}", u.product));
            set("ID_REVISION", format!("{:04x}", u.version));
            set("ID_SERIAL", tree::usb_serial(u));
            if !u.serial.is_empty() {
                set("ID_SERIAL_SHORT", u.serial.clone());
            }
            set("ID_USB_INTERFACE_NUM", format!("{:02x}", u.interface));
            set("ID_USB_DRIVER", String::from(if u.hid != 0 { "usbhid" } else { "xpad" }));
        }
        Location::Serio(_) => set("ID_BUS", String::from("i8042")),
        Location::I2c(_) => set("ID_BUS", String::from("i2c")),
        Location::Virtual => {}
    }
    if let Some(path) = tree::id_path(&d.location) {
        set("ID_PATH_TAG", path.replace([':', '.', '-'], "_"));
        set("ID_PATH", path);
    }
    let id = d.id;
    set("LIBINPUT_DEVICE_GROUP", format!("{:x}/{:x}/{:x}:{}", id.bustype, id.vendor, id.product, d.phys));
    if let Some(dev) = node {
        let links: Vec<String> = tree::stable_names(d, dev).iter().map(|l| format!("/dev/input/{l}")).collect();
        if !links.is_empty() {
            set("DEVLINKS", links.join(" "));
        }
    }
    set("TAGS", String::from(":seat:uaccess:"));
    set("CURRENT_TAGS", String::from(":seat:uaccess:"));
    p
}

/// A record of `/run/udev/data`: links (`S:`), properties (`E:`), tags.
pub fn db(d: &Device, node: Option<Dev>) -> Vec<u8> {
    let mut s = String::new();
    if let Some(dev) = node {
        for l in tree::stable_names(d, dev) {
            s.push_str(&format!("S:input/{l}\n"));
        }
    }
    s.push_str(&format!("I:{}\n", usec()));
    for (k, v) in properties(d, node) {
        if !matches!(k.as_str(), "DEVLINKS" | "TAGS" | "CURRENT_TAGS") {
            s.push_str(&format!("E:{k}={v}\n"));
        }
    }
    s.push_str("G:seat\nG:uaccess\nQ:seat\nQ:uaccess\nV:1\n");
    s.into_bytes()
}

pub fn hidraw_properties(h: &Hidraw) -> Vec<(String, String)> {
    let mut p = Vec::new();
    if let Location::Usb(u) = &h.location {
        p.push((String::from("ID_BUS"), String::from("usb")));
        p.push((String::from("ID_VENDOR_ID"), format!("{:04x}", u.vendor)));
        p.push((String::from("ID_MODEL_ID"), format!("{:04x}", u.product)));
        p.push((String::from("ID_SERIAL"), tree::usb_serial(u)));
    }
    if let Some(path) = tree::id_path(&h.location) {
        p.push((String::from("ID_PATH"), path));
    }
    // Controllers' raw reports are the logged-in account's (Steam's rules
    // do the same); other devices' only the system's.
    if h.class == Class::Joystick {
        p.push((String::from("TAGS"), String::from(":uaccess:")));
        p.push((String::from("CURRENT_TAGS"), String::from(":uaccess:")));
    }
    p
}

pub fn hidraw_db(h: &Hidraw) -> Vec<u8> {
    let mut s = format!("I:{}\n", usec());
    for (k, v) in hidraw_properties(h) {
        if !matches!(k.as_str(), "TAGS" | "CURRENT_TAGS") {
            s.push_str(&format!("E:{k}={v}\n"));
        }
    }
    if h.class == Class::Joystick {
        s.push_str("G:uaccess\nQ:uaccess\n");
    }
    s.push_str("V:1\n");
    s.into_bytes()
}

/// One device event, both ways: the kernel's message and udev's.
pub struct Message {
    pub action: &'static str,
    pub devpath: String,
    pub subsystem: &'static str,
    /// The device's `uevent` lines.
    pub uevent: String,
    /// What udev adds (absolute `DEVNAME`, `ID_*`, links, tags).
    pub udev: Vec<(String, String)>,
}

impl Message {
    /// `add@/devices/…\0ACTION=add\0…\0SEQNUM=n\0`.
    pub fn kernel(&self, seqnum: u64) -> Vec<u8> {
        let mut m = format!("{}@{}\0", self.action, self.devpath).into_bytes();
        for line in self.base(seqnum) {
            m.extend_from_slice(line.as_bytes());
            m.push(0);
        }
        m
    }

    fn base(&self, seqnum: u64) -> Vec<String> {
        let mut v = alloc::vec![
            format!("ACTION={}", self.action),
            format!("DEVPATH={}", self.devpath),
            format!("SUBSYSTEM={}", self.subsystem),
        ];
        v.extend(self.uevent.lines().filter(|l| !l.is_empty()).map(String::from));
        v.push(format!("SEQNUM={seqnum}"));
        v
    }

    /// The message udev would broadcast: a `libudev` header, then the
    /// properties (`struct monitor_netlink_header`).
    pub fn udev(&self, seqnum: u64) -> Vec<u8> {
        let mut props = Vec::new();
        let mut tags: Vec<String> = Vec::new();
        for line in self.base(seqnum) {
            let line = match line.strip_prefix("DEVNAME=") {
                Some(name) => format!("DEVNAME=/dev/{name}"),
                None => line,
            };
            props.extend_from_slice(line.as_bytes());
            props.push(0);
        }
        for (k, v) in &self.udev {
            if k == "TAGS" {
                tags = v.split(':').filter(|t| !t.is_empty()).map(String::from).collect();
            }
            props.extend_from_slice(format!("{k}={v}").as_bytes());
            props.push(0);
        }
        props.extend_from_slice(format!("USEC_INITIALIZED={}", usec()).as_bytes());
        props.push(0);
        let mut bloom = 0u64;
        for t in &tags {
            let h = murmur2(t.as_bytes());
            for shift in [0, 6, 12, 18] {
                bloom |= 1 << ((h >> shift) & 63);
            }
        }
        let mut m = Vec::with_capacity(40 + props.len());
        m.extend_from_slice(b"libudev\0");
        for v in [
            0xFEED_CAFEu32,
            40,
            40,
            props.len() as u32,
            murmur2(self.subsystem.as_bytes()),
            0,
            (bloom >> 32) as u32,
            bloom as u32,
        ] {
            m.extend_from_slice(&v.to_be_bytes());
        }
        m.extend_from_slice(&props);
        m
    }
}

/// MurmurHash2 (seed 0), which udev uses for its filter hashes.
pub fn murmur2(data: &[u8]) -> u32 {
    const M: u32 = 0x5bd1_e995;
    let mut h = data.len() as u32;
    let (chunks, rest) = data.as_chunks::<4>();
    for c in chunks {
        let mut k = u32::from_le_bytes(*c);
        k = k.wrapping_mul(M);
        k ^= k >> 24;
        k = k.wrapping_mul(M);
        h = h.wrapping_mul(M) ^ k;
    }
    if !rest.is_empty() {
        for (i, &b) in rest.iter().enumerate() {
            h ^= (b as u32) << (8 * i);
        }
        h = h.wrapping_mul(M);
    }
    h ^= h >> 13;
    h = h.wrapping_mul(M);
    h ^ (h >> 15)
}

/// The messages for an input device coming (its `inputN`, then its nodes)
/// or going (the other way round).
pub fn input_messages(d: &Device, added: bool) -> Vec<Message> {
    let action = if added { "add" } else { "remove" };
    let devpath = |dir: String| String::from(dir.strip_prefix("/sys").unwrap_or(&dir));
    let mut v = alloc::vec![Message {
        action,
        devpath: devpath(tree::input_dir(d)),
        subsystem: "input",
        uevent: tree::input_uevent(d),
        udev: properties(d, None),
    }];
    let mut nodes = alloc::vec![Dev::Event(d.num)];
    nodes.extend(d.js.map(Dev::Js));
    for dev in nodes {
        v.push(Message {
            action,
            devpath: devpath(tree::node_dir(d, dev)),
            subsystem: "input",
            uevent: tree::node_uevent(dev),
            udev: properties(d, Some(dev)),
        });
    }
    if !added {
        v.reverse();
    }
    v
}

pub fn hidraw_message(h: &Hidraw, added: bool) -> Message {
    let dir = tree::hidraw_dir(h);
    Message {
        action: if added { "add" } else { "remove" },
        devpath: String::from(dir.strip_prefix("/sys").unwrap_or(&dir)),
        subsystem: "hidraw",
        uevent: tree::node_uevent(Dev::Hidraw(h.num)),
        udev: hidraw_properties(h),
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    // Reference values of MurmurHash2 with seed 0.
    assert_eq!(murmur2(b""), 0);
    assert_ne!(murmur2(b"input"), murmur2(b"hidraw"));
}
