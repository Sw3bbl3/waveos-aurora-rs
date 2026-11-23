//! The files around input devices that Linux programs look for, made from
//! the devices there are now:
//!
//! - `/dev/input/event*`, `js*`, their `by-id` and `by-path` links,
//!   `/dev/hidraw*` and `/dev/uinput`;
//! - sysfs: each device's directory under `/sys/devices` (where Linux puts
//!   it: `virtual`, the i8042's serio ports, the USB device, interface and
//!   HID device), its attributes (`name`, `id/*`, `capabilities/*`,
//!   `uevent`, …), and the `/sys/class` and `/sys/bus` links to it;
//! - the records udev would keep in `/run/udev/data` (properties such as
//!   `ID_INPUT_JOYSTICK=1`), and `/run/udev/control`, which tells libudev
//!   that udev is there (the kernel sends udev's messages itself).
//!
//! The tree is made again only when a device comes or goes.

use super::udev;
use crate::drivers::evdev::hidraw::Hidraw;
use crate::drivers::evdev::{self, codes::*, Class, Device, Location, UsbPlace};
use crate::sync::Mutex;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};

/// A device node in `/dev`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dev {
    Event(u32),
    Js(u32),
    Hidraw(u32),
    Uinput,
}

pub const INPUT_MAJOR: u64 = 13;
pub const MISC_MAJOR: u64 = 10;
pub const UINPUT_MINOR: u64 = 223;
/// hidraw's major is given out at boot on Linux; this is the one here.
pub const HIDRAW_MAJOR: u64 = 245;

impl Dev {
    /// Major and minor numbers, as Linux numbers them.
    pub fn numbers(self) -> (u64, u64) {
        match self {
            Dev::Event(n) => (INPUT_MAJOR, 64 + n as u64),
            Dev::Js(n) => (INPUT_MAJOR, n as u64),
            Dev::Hidraw(n) => (HIDRAW_MAJOR, n as u64),
            Dev::Uinput => (MISC_MAJOR, UINPUT_MINOR),
        }
    }
}

pub enum Node {
    Dir,
    File(Vec<u8>),
    /// A symbolic link (its target, relative as Linux's are).
    Link(String),
    Char(Dev),
}

pub type Tree = BTreeMap<String, Node>;

/// Bumped when a device comes or goes.
static GENERATION: AtomicU64 = AtomicU64::new(1);
static CACHE: Mutex<Option<(u64, Arc<Tree>)>> = Mutex::new(None);

/// When a device last came or went (Unix seconds; 0: not since boot).
static CHANGED_AT: AtomicU64 = AtomicU64::new(0);

pub fn changed() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    CHANGED_AT.store(super::super::realtime_ns() / 1_000_000_000, Ordering::Release);
}

/// The time the device folders say they were changed: when a device
/// last came or went, or when the system started.
pub fn mtime() -> i64 {
    match CHANGED_AT.load(Ordering::Acquire) {
        0 => ((super::super::realtime_ns() - crate::time::now_ns()) / 1_000_000_000) as i64,
        t => t as i64,
    }
}

/// The tree as it is now.
pub fn tree() -> Arc<Tree> {
    let gen = GENERATION.load(Ordering::Acquire);
    let mut c = CACHE.lock();
    if let Some((g, t)) = &*c {
        if *g == gen {
            return t.clone();
        }
    }
    let t = Arc::new(build());
    *c = Some((gen, t.clone()));
    t
}

/// The names in directory `dir` and their `d_type`s.
pub fn children(t: &Tree, dir: &str) -> Vec<(String, u8)> {
    let prefix = format!("{dir}/");
    t.range(prefix.clone()..)
        .take_while(|(k, _)| k.starts_with(&prefix))
        .filter_map(|(k, v)| {
            let name = &k[prefix.len()..];
            (!name.contains('/')).then(|| {
                let kind = match v {
                    Node::Dir => 4,
                    Node::File(_) => 8,
                    Node::Link(_) => 10,
                    Node::Char(_) => 2,
                };
                (String::from(name), kind)
            })
        })
        .collect()
}

/// `to` relative to directory `from` (both absolute).
fn relative(from: &str, to: &str) -> String {
    let f: Vec<&str> = from.split('/').filter(|s| !s.is_empty()).collect();
    let t: Vec<&str> = to.split('/').filter(|s| !s.is_empty()).collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<&str> = core::iter::repeat_n("..", f.len() - common).collect();
    parts.extend(&t[common..]);
    parts.join("/")
}

struct Builder {
    t: Tree,
}

impl Builder {
    fn dir(&mut self, path: &str) {
        let mut p = String::new();
        for part in path.split('/').filter(|s| !s.is_empty()) {
            p.push('/');
            p.push_str(part);
            self.t.entry(p.clone()).or_insert(Node::Dir);
        }
    }

    fn file(&mut self, path: &str, text: impl Into<Vec<u8>>) {
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.dir(parent);
        }
        self.t.insert(String::from(path), Node::File(text.into()));
    }

    /// A link at `path` to the absolute `to` (stored relative).
    fn link(&mut self, path: &str, to: &str) {
        let (parent, _) = path.rsplit_once('/').unwrap_or(("", path));
        self.dir(parent);
        self.t.insert(String::from(path), Node::Link(relative(parent, to)));
    }

    fn char(&mut self, path: &str, dev: Dev) {
        if let Some((parent, _)) = path.rsplit_once('/') {
            self.dir(parent);
        }
        self.t.insert(String::from(path), Node::Char(dev));
    }
}

/// The USB device's directory (`…/usb1/1-2`), its interface's
/// (`…/1-2:1.0`), and the HID device's under it if it is one.
pub fn usb_dirs(u: &UsbPlace) -> (String, String, Option<String>) {
    let pci = if u.pci.is_empty() { "0000:00:00.0" } else { &u.pci };
    let name = format!("{}-{}", u.bus, u.ports);
    let device = format!("/sys/devices/pci0000:00/{pci}/usb{}/{name}", u.bus);
    let interface = format!("{device}/{name}:{}.{}", u.config.max(1), u.interface);
    let hid = (u.hid != 0).then(|| format!("{interface}/{}", hid_name(u)));
    (device, interface, hid)
}

/// `0003:VVVV:PPPP.NNNN`.
pub fn hid_name(u: &UsbPlace) -> String {
    format!("0003:{:04X}:{:04X}.{:04X}", u.vendor, u.product, u.hid)
}

/// The directory a device's `input/inputN` is in.
pub fn parent_dir(loc: &Location) -> String {
    match loc {
        Location::Virtual => String::from("/sys/devices/virtual"),
        Location::Serio(n) => format!("/sys/devices/platform/i8042/serio{n}"),
        Location::Usb(u) => {
            let (_, interface, hid) = usb_dirs(u);
            hid.unwrap_or(interface)
        }
        Location::I2c(path) => format!("/sys/devices/platform/i2c/{}", i2c_name(path)),
    }
}

pub fn i2c_name(path: &str) -> String {
    format!("i2c-{}:00", path.rsplit(['.', '\\']).next().unwrap_or("I2C"))
}

/// `/sys/devices/…/input/inputN`.
pub fn input_dir(d: &Device) -> String {
    format!("{}/input/input{}", parent_dir(&d.location), d.input)
}

/// Linux's way of printing a bitmap: hex words, most significant first.
pub fn bitmap(words: &[u64]) -> String {
    let Some(top) = words.iter().rposition(|&w| w != 0) else { return String::from("0") };
    let mut s = format!("{:x}", words[top]);
    for w in words[..top].iter().rev() {
        s.push_str(&format!(" {w:016x}"));
    }
    s
}

/// Each event type's bitmap as 64-bit words.
pub fn words(d: &Device, ty: u16) -> Vec<u64> {
    let c = &d.caps;
    match ty {
        0 => alloc::vec![c.ev as u64],
        EV_KEY => c.key.to_vec(),
        EV_REL => alloc::vec![c.rel],
        EV_ABS => alloc::vec![c.abs],
        EV_MSC => alloc::vec![c.msc],
        EV_LED => alloc::vec![c.led],
        EV_SND => alloc::vec![c.snd],
        EV_FF => c.ff.to_vec(),
        EV_SW => alloc::vec![c.sw],
        _ => Vec::new(),
    }
}

/// `MODALIAS` of an input device, as the kernel writes it.
pub fn modalias(d: &Device) -> String {
    let id = d.id;
    let mut s = format!("input:b{:04X}v{:04X}p{:04X}e{:04X}-", id.bustype, id.vendor, id.product, id.version);
    let kinds = [
        ('e', 0, 0),
        ('k', EV_KEY, 113),
        ('r', EV_REL, 0),
        ('a', EV_ABS, 0),
        ('m', EV_MSC, 0),
        ('l', EV_LED, 0),
        ('s', EV_SND, 0),
        ('f', EV_FF, 0),
        ('w', EV_SW, 0),
    ];
    for (letter, ty, from) in kinds {
        s.push(letter);
        for (i, w) in words(d, ty).iter().enumerate() {
            for b in 0..64 {
                let code = i * 64 + b;
                if w & (1 << b) != 0 && code >= from as usize {
                    s.push_str(&format!("{code:X},"));
                }
            }
        }
    }
    s
}

/// What the kernel puts in an input device's `uevent` file.
pub fn input_uevent(d: &Device) -> String {
    let id = d.id;
    let mut s = format!("PRODUCT={:x}/{:x}/{:x}/{:x}\n", id.bustype, id.vendor, id.product, id.version);
    s.push_str(&format!("NAME=\"{}\"\n", d.name));
    if !d.phys.is_empty() {
        s.push_str(&format!("PHYS=\"{}\"\n", d.phys));
    }
    if !d.uniq.is_empty() {
        s.push_str(&format!("UNIQ=\"{}\"\n", d.uniq));
    }
    s.push_str(&format!("PROP={}\n", bitmap(&[d.caps.prop])));
    for (name, ty) in [
        ("EV", 0),
        ("KEY", EV_KEY),
        ("REL", EV_REL),
        ("ABS", EV_ABS),
        ("MSC", EV_MSC),
        ("LED", EV_LED),
        ("SND", EV_SND),
        ("FF", EV_FF),
        ("SW", EV_SW),
    ] {
        if ty == 0 || d.caps.ev & (1 << ty) != 0 {
            s.push_str(&format!("{name}={}\n", bitmap(&words(d, ty))));
        }
    }
    s.push_str(&format!("MODALIAS={}\n", modalias(d)));
    s
}

/// A device node's `uevent`.
pub fn node_uevent(dev: Dev) -> String {
    let (major, minor) = dev.numbers();
    format!("MAJOR={major}\nMINOR={minor}\nDEVNAME={}\n", devname(dev))
}

/// The node's name under `/dev`.
pub fn devname(dev: Dev) -> String {
    match dev {
        Dev::Event(n) => format!("input/event{n}"),
        Dev::Js(n) => format!("input/js{n}"),
        Dev::Hidraw(n) => format!("hidraw{n}"),
        Dev::Uinput => String::from("uinput"),
    }
}

/// The node's sysfs directory, and its class.
pub fn node_dir(d: &Device, dev: Dev) -> String {
    match dev {
        Dev::Event(n) => format!("{}/event{n}", input_dir(d)),
        Dev::Js(n) => format!("{}/js{n}", input_dir(d)),
        _ => input_dir(d),
    }
}

pub fn hidraw_dir(h: &Hidraw) -> String {
    let parent = match &h.location {
        Location::Usb(u) => {
            let (_, interface, hid) = usb_dirs(u);
            hid.unwrap_or(interface)
        }
        other => parent_dir(other),
    };
    format!("{parent}/hidraw/hidraw{}", h.num)
}

/// What the device is for udev's `.INPUT_CLASS` (stable names).
fn class_name(d: &Device) -> &'static str {
    match d.class {
        Class::Keyboard | Class::Key => "kbd",
        Class::Joystick => "joystick",
        _ => "mouse",
    }
}

/// udev's `ID_SERIAL` for a USB device.
pub fn usb_serial(u: &UsbPlace) -> String {
    let clean = |s: &str| -> String {
        s.trim().chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else { '_' }).collect()
    };
    let vendor = if u.maker.is_empty() { format!("{:04x}", u.vendor) } else { clean(&u.maker) };
    let model = if u.model.is_empty() { format!("{:04x}", u.product) } else { clean(&u.model) };
    let mut s = format!("{vendor}_{model}");
    if !u.serial.is_empty() {
        s.push('_');
        s.push_str(&clean(&u.serial));
    }
    s
}

/// udev's `ID_PATH` (None: a virtual device has none).
pub fn id_path(loc: &Location) -> Option<String> {
    match loc {
        Location::Virtual => None,
        Location::Serio(n) => Some(format!("platform-i8042-serio-{n}")),
        Location::Usb(u) => {
            let pci = if u.pci.is_empty() { "0000:00:00.0" } else { &u.pci };
            Some(format!("pci-{pci}-usb-0:{}:{}.{}", u.ports, u.config.max(1), u.interface))
        }
        Location::I2c(path) => Some(format!("platform-{}", i2c_name(path))),
    }
}

/// The `by-id` and `by-path` names of node `dev` (relative to `/dev/input`).
pub fn stable_names(d: &Device, dev: Dev) -> Vec<String> {
    let suffix = match dev {
        Dev::Event(_) => format!("event-{}", class_name(d)),
        Dev::Js(_) => String::from("joystick"),
        _ => return Vec::new(),
    };
    let mut v = Vec::new();
    if let Location::Usb(u) = &d.location {
        let iface = if u.interface != 0 { format!("-if{:02}", u.interface) } else { String::new() };
        v.push(format!("by-id/usb-{}{iface}-{suffix}", usb_serial(u)));
    }
    if let Some(p) = id_path(&d.location) {
        v.push(format!("by-path/{p}-{suffix}"));
    }
    v
}

fn build() -> Tree {
    let mut b = Builder { t: Tree::new() };
    for dir in [
        "/sys/class/input",
        "/sys/class/hidraw",
        "/sys/class/misc",
        "/sys/bus/input/devices",
        "/sys/bus/hid/devices",
        "/sys/bus/usb/devices",
        "/sys/dev/char",
        "/sys/devices/virtual/misc",
        "/dev/input",
        "/run/udev/data",
    ] {
        b.dir(dir);
    }
    b.file("/run/udev/control", "");
    // The DRM cards' records, beside the input devices' (`super::super::drm`).
    for (name, data) in super::super::drm::udev_records() {
        b.file(&format!("/run/udev/data/{name}"), data);
    }
    // /dev/uinput (a misc device).
    let uinput = "/sys/devices/virtual/misc/uinput";
    b.file(&format!("{uinput}/uevent"), node_uevent(Dev::Uinput));
    b.file(&format!("{uinput}/dev"), format!("{MISC_MAJOR}:{UINPUT_MINOR}\n"));
    b.link(&format!("{uinput}/subsystem"), "/sys/class/misc");
    b.link("/sys/class/misc/uinput", uinput);
    b.link(&format!("/sys/dev/char/{MISC_MAJOR}:{UINPUT_MINOR}"), uinput);
    b.char("/dev/uinput", Dev::Uinput);

    let mut usb_done: Vec<String> = Vec::new();
    for d in evdev::devices() {
        let dir = input_dir(&d);
        add_parents(&mut b, &d.location, &mut usb_done);
        b.file(&format!("{dir}/uevent"), input_uevent(&d));
        b.file(&format!("{dir}/name"), format!("{}\n", d.name));
        b.file(&format!("{dir}/phys"), format!("{}\n", d.phys));
        b.file(&format!("{dir}/uniq"), format!("{}\n", d.uniq));
        b.file(&format!("{dir}/properties"), format!("{}\n", bitmap(&[d.caps.prop])));
        b.file(&format!("{dir}/modalias"), format!("{}\n", modalias(&d)));
        for (name, v) in
            [("bustype", d.id.bustype), ("vendor", d.id.vendor), ("product", d.id.product), ("version", d.id.version)]
        {
            b.file(&format!("{dir}/id/{name}"), format!("{v:04x}\n"));
        }
        for (name, ty) in [
            ("ev", 0),
            ("key", EV_KEY),
            ("rel", EV_REL),
            ("abs", EV_ABS),
            ("msc", EV_MSC),
            ("led", EV_LED),
            ("snd", EV_SND),
            ("ff", EV_FF),
            ("sw", EV_SW),
        ] {
            b.file(&format!("{dir}/capabilities/{name}"), format!("{}\n", bitmap(&words(&d, ty))));
        }
        b.link(&format!("{dir}/subsystem"), "/sys/class/input");
        if !matches!(d.location, Location::Virtual) {
            b.link(&format!("{dir}/device"), &parent_dir(&d.location));
        }
        b.link(&format!("/sys/class/input/input{}", d.input), &dir);
        b.link(&format!("/sys/bus/input/devices/input{}", d.input), &dir);
        b.file(&format!("/run/udev/data/+input:input{}", d.input), udev::db(&d, None));
        let mut nodes = alloc::vec![Dev::Event(d.num)];
        nodes.extend(d.js.map(Dev::Js));
        for dev in nodes {
            let ndir = node_dir(&d, dev);
            let (major, minor) = dev.numbers();
            b.file(&format!("{ndir}/uevent"), node_uevent(dev));
            b.file(&format!("{ndir}/dev"), format!("{major}:{minor}\n"));
            b.link(&format!("{ndir}/subsystem"), "/sys/class/input");
            b.link(&format!("{ndir}/device"), &dir);
            let name = ndir.rsplit('/').next().unwrap_or("");
            b.link(&format!("/sys/class/input/{name}"), &ndir);
            b.link(&format!("/sys/dev/char/{major}:{minor}"), &ndir);
            b.char(&format!("/dev/{}", devname(dev)), dev);
            b.file(&format!("/run/udev/data/c{major}:{minor}"), udev::db(&d, Some(dev)));
            for link in stable_names(&d, dev) {
                b.link(&format!("/dev/input/{link}"), &format!("/dev/{}", devname(dev)));
            }
        }
    }
    for h in evdev::hidraw::devices() {
        add_parents(&mut b, &h.location, &mut usb_done);
        let dir = hidraw_dir(&h);
        let dev = Dev::Hidraw(h.num);
        let (major, minor) = dev.numbers();
        b.file(&format!("{dir}/uevent"), node_uevent(dev));
        b.file(&format!("{dir}/dev"), format!("{major}:{minor}\n"));
        b.link(&format!("{dir}/subsystem"), "/sys/class/hidraw");
        let parent = dir.rsplit_once("/hidraw/").map_or(String::new(), |(p, _)| String::from(p));
        b.link(&format!("{dir}/device"), &parent);
        b.file(&format!("{parent}/report_descriptor"), h.descriptor.clone());
        b.link(&format!("/sys/class/hidraw/hidraw{}", h.num), &dir);
        b.link(&format!("/sys/dev/char/{major}:{minor}"), &dir);
        b.char(&format!("/dev/hidraw{}", h.num), dev);
        b.file(&format!("/run/udev/data/c{major}:{minor}"), udev::hidraw_db(&h));
    }
    b.t
}

/// The USB device, interface and HID device directories (once each).
fn add_parents(b: &mut Builder, loc: &Location, done: &mut Vec<String>) {
    let Location::Usb(u) = loc else {
        let dir = parent_dir(loc);
        if !matches!(loc, Location::Virtual) && !done.contains(&dir) {
            done.push(dir.clone());
            let (subsystem, uevent) = match loc {
                Location::Serio(_) => ("/sys/bus/serio", "DRIVER=atkbd\nSERIO_TYPE=06\n"),
                _ => ("/sys/bus/i2c", "DRIVER=i2c_hid_acpi\n"),
            };
            b.file(&format!("{dir}/uevent"), uevent);
            b.link(&format!("{dir}/subsystem"), subsystem);
        }
        return;
    };
    let (device, interface, hid) = usb_dirs(u);
    if !done.contains(&device) {
        done.push(device.clone());
        let name = device.rsplit('/').next().unwrap_or("");
        let product = format!("{:x}/{:x}/{:x}", u.vendor, u.product, u.version);
        b.file(
            &format!("{device}/uevent"),
            format!("DEVTYPE=usb_device\nDRIVER=usb\nPRODUCT={product}\nTYPE=0/0/0\nBUSNUM={:03}\n", u.bus),
        );
        b.file(&format!("{device}/idVendor"), format!("{:04x}\n", u.vendor));
        b.file(&format!("{device}/idProduct"), format!("{:04x}\n", u.product));
        b.file(&format!("{device}/bcdDevice"), format!("{:04x}\n", u.version));
        b.file(&format!("{device}/busnum"), format!("{}\n", u.bus));
        for (name, v) in [("manufacturer", &u.maker), ("product", &u.model), ("serial", &u.serial)] {
            if !v.is_empty() {
                b.file(&format!("{device}/{name}"), format!("{v}\n"));
            }
        }
        b.link(&format!("{device}/subsystem"), "/sys/bus/usb");
        b.link(&format!("/sys/bus/usb/devices/{name}"), &device);
    }
    if !done.contains(&interface) {
        done.push(interface.clone());
        let name = interface.rsplit('/').next().unwrap_or("");
        let class = if u.hid != 0 { 3 } else { 0xFF };
        b.file(
            &format!("{interface}/uevent"),
            format!(
                "DEVTYPE=usb_interface\nDRIVER={}\nPRODUCT={:x}/{:x}/{:x}\nTYPE=0/0/0\nINTERFACE={class}/0/0\n\
                 MODALIAS=usb:v{:04X}p{:04X}d{:04X}dc00dsc00dp00ic{class:02X}isc00ip00in{:02X}\n",
                if u.hid != 0 { "usbhid" } else { "xpad" },
                u.vendor,
                u.product,
                u.version,
                u.vendor,
                u.product,
                u.version,
                u.interface
            ),
        );
        b.file(&format!("{interface}/bInterfaceNumber"), format!("{:02x}\n", u.interface));
        b.file(&format!("{interface}/bInterfaceClass"), format!("{class:02x}\n"));
        b.link(&format!("{interface}/subsystem"), "/sys/bus/usb");
        b.link(&format!("/sys/bus/usb/devices/{name}"), &interface);
    }
    if let Some(hid) = hid {
        if !done.contains(&hid) {
            done.push(hid.clone());
            let name = hid.rsplit('/').next().unwrap_or("");
            // The name and place are those of the HID device's first input.
            let (hid_name, phys) = evdev::hidraw::devices()
                .into_iter()
                .find(|h| matches!(&h.location, Location::Usb(p) if p.hid == u.hid))
                .map(|h| (h.name.clone(), h.phys.clone()))
                .unwrap_or_default();
            b.file(
                &format!("{hid}/uevent"),
                format!(
                    "DRIVER=hid-generic\nHID_ID=0003:{:08X}:{:08X}\nHID_NAME={hid_name}\nHID_PHYS={phys}\nHID_UNIQ={}\n\
                     MODALIAS=hid:b0003g0001v{:08X}p{:08X}\n",
                    u.vendor, u.product, u.serial, u.vendor, u.product
                ),
            );
            b.link(&format!("{hid}/subsystem"), "/sys/bus/hid");
            b.link(&format!("/sys/bus/hid/devices/{name}"), &hid);
        }
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    assert_eq!(relative("/sys/class/input", "/sys/devices/virtual/input/input3"), "../../devices/virtual/input/input3");
    assert_eq!(bitmap(&[0x1b, 0, 0]), "1b");
    assert_eq!(bitmap(&[0x1, 0x2]), "2 0000000000000001");
    assert_eq!(bitmap(&[0, 0]), "0");
}
