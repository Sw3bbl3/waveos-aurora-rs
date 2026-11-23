//! Input devices for Linux programs, the standard way: `/dev/input/event*`
//! (evdev), `/dev/input/js*` (the joystick API), `/dev/hidraw*` (raw HID
//! reports), `/dev/uinput` (devices made by programs), and what finds them
//! — sysfs, udev's records and messages, inotify on `/dev/input` — so
//! SDL, libinput, Steam and Wine need nothing WaveOS-specific.
//!
//! The devices themselves are `drivers::evdev`'s; this is their files.
//!
//! Who may open a node: it belongs to the account logged in at the screen
//! (`crw-rw----`), as logind's `uaccess` gives it on Linux; raw HID nodes
//! of keyboards and pointers belong to the system only (`crw-------`).
//! Who reads what once it is open: see `drivers::evdev` (the window with
//! the focus, and Wave Play's rule for controllers).

pub mod evdev;
pub mod hidraw;
pub mod joydev;
pub mod tree;
pub mod udev;
pub mod uinput;

use super::fs::Special;
use crate::drivers::evdev as core_evdev;
use crate::drivers::evdev::hidraw::Hidraw;
use crate::proc::Process;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use aurora_abi::err::*;
use tree::{Dev, Node};

/// Folders this module makes (everything under them is its).
fn owns(guest: &str) -> bool {
    ["/sys", "/dev/input", "/run/udev"]
        .iter()
        .any(|r| guest.strip_prefix(r).is_some_and(|rest| rest.is_empty() || rest.starts_with('/')))
        || guest == "/dev/uinput"
        || guest.strip_prefix("/dev/hidraw").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

/// What is at `guest`, if it is this module's.
pub enum Found {
    Dir,
    File(Vec<u8>),
    Link(String),
    Char { dev: Dev, mode: u32, owner: (u32, u32) },
    Missing,
}

/// The owner and permissions of a device node.
fn node_owner(dev: Dev) -> (u32, (u32, u32)) {
    let session = crate::users::session();
    let user = (0o660, (session.uid, session.gid));
    match dev {
        Dev::Hidraw(n) => match core_evdev::hidraw::by_num(n) {
            Some(h) if h.class == core_evdev::Class::Joystick => user,
            _ => (0o600, (0, 0)),
        },
        _ => user,
    }
}

/// Looks `guest` up (None: not this module's path).
pub fn lookup(guest: &str) -> Option<Found> {
    if !owns(guest) {
        return None;
    }
    let t = tree::tree();
    Some(match t.get(guest) {
        Some(Node::Dir) => Found::Dir,
        Some(Node::File(b)) => Found::File(b.clone()),
        Some(Node::Link(to)) => Found::Link(to.clone()),
        Some(Node::Char(dev)) => {
            let (mode, owner) = node_owner(*dev);
            Found::Char { dev: *dev, mode, owner }
        }
        // The rest of /sys is the display's (the DRM cards and their PCI
        // devices, `super::sysfs`).
        None if in_sys(guest) => match super::sysfs::link(guest) {
            Some(to) => Found::Link(to),
            None => match super::sysfs::open(guest) {
                Ok(None) => Found::Dir,
                Ok(Some(data)) => Found::File(data),
                Err(()) => Found::Missing,
            },
        },
        None => Found::Missing,
    })
}

fn in_sys(guest: &str) -> bool {
    guest.strip_prefix("/sys").is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// The target of the link at `guest`, if it is one of this module's.
pub fn link(guest: &str) -> Option<String> {
    match lookup(guest)? {
        Found::Link(to) => Some(to),
        _ => None,
    }
}

/// The entries of folder `guest`, with what this module adds to `/dev`,
/// `/run` and `/` (None: not a folder it knows).
pub fn entries(guest: &str) -> Option<Vec<(String, u8)>> {
    let t = tree::tree();
    let dir = if guest == "/" { "" } else { guest };
    let mut v = tree::children(&t, dir);
    match guest {
        "/" => v.retain(|(n, _)| n == "sys"),
        "/run" => v.retain(|(n, _)| n == "udev"),
        "/dev" => v.retain(|(n, _)| n == "input" || n == "uinput" || n.starts_with("hidraw")),
        // A folder of /sys: what the input devices put there and what the
        // display does, as one listing.
        g if in_sys(g) => {
            let mine = matches!(t.get(g), Some(Node::Dir));
            let drm = matches!(super::sysfs::open(g), Ok(None));
            if !mine && !drm {
                return None;
            }
            if drm {
                for e in super::sysfs::entries(g) {
                    if !v.iter().any(|(n, _)| *n == e.0) {
                        v.push(e);
                    }
                }
            }
        }
        g if owns(g) && matches!(t.get(g), Some(Node::Dir)) => {}
        _ => return None,
    }
    Some(v)
}

/// Opens a node of this module's (`flags`: the `open` flags).
pub fn open(p: &Process, guest: &str, flags: u64) -> Option<Result<Special, isize>> {
    let (dev, mode, owner) = match lookup(guest)? {
        Found::Char { dev, mode, owner } => (dev, mode, owner),
        Found::Missing => return Some(Err(ENOENT)),
        // Folders, files and links: the file layer's.
        _ => return None,
    };
    let want = match flags & 3 {
        0 => 4,
        1 => 2,
        _ => 6,
    };
    // The system (and root) may open anything.
    if let Some(c) = crate::fs::perm::caller() {
        let o = crate::fs::Owner { uid: owner.0, gid: owner.1, mode: mode as u16 };
        if !crate::fs::perm::allowed(&c, &o, want) {
            return Some(Err(EACCES));
        }
    }
    let pid = p.pid;
    let node: Result<Arc<dyn super::chardev::CharDev>, isize> = match dev {
        Dev::Event(n) => core_evdev::by_num(n)
            .ok_or(ENODEV)
            .and_then(|d| d.open(pid))
            .map(|c| Arc::new(evdev::EvdevFile::new(c)) as Arc<dyn super::chardev::CharDev>),
        Dev::Js(n) => core_evdev::by_js(n)
            .ok_or(ENODEV)
            .and_then(|d| d.open_js(pid))
            .map(|c| Arc::new(joydev::JsFile { c }) as Arc<dyn super::chardev::CharDev>),
        Dev::Hidraw(n) => core_evdev::hidraw::by_num(n)
            .ok_or(ENODEV)
            .and_then(|h| h.open(pid))
            .map(|c| Arc::new(hidraw::HidrawFile { c }) as Arc<dyn super::chardev::CharDev>),
        Dev::Uinput => Ok(uinput::Uinput::new(pid) as Arc<dyn super::chardev::CharDev>),
    };
    Some(node.map(Special::Device))
}

/// `st_mode`, `st_rdev` and owner of a node.
pub fn node_stat(dev: Dev) -> (u32, u64, Option<(u32, u32)>) {
    let (mode, owner) = node_owner(dev);
    let (major, minor) = dev.numbers();
    (0o020000 | mode, makedev(major, minor), Some(owner))
}

/// Linux's `makedev` (as glibc decodes `st_rdev`).
pub fn makedev(major: u64, minor: u64) -> u64 {
    ((major & 0xfff) << 8) | ((major & !0xfff) << 32) | (minor & 0xff) | ((minor & !0xff) << 12)
}

// ------------------------------------------------------------------ hot-plug

/// An input device came or went: sysfs and udev's records change, and
/// programs hear of it (uevents, inotify on `/dev/input`).
pub fn device_changed(d: &Arc<core_evdev::Device>, added: bool) {
    tree::changed();
    for m in udev::input_messages(d, added) {
        super::uevent::send(&m);
    }
    let mut nodes = alloc::vec![Dev::Event(d.num)];
    nodes.extend(d.js.map(Dev::Js));
    for dev in nodes {
        let path = alloc::format!("/dev/{}", tree::devname(dev));
        node_changed(&path, added);
        for l in tree::stable_names(d, dev) {
            node_changed(&alloc::format!("/dev/input/{l}"), added);
        }
    }
}

pub fn hidraw_changed(h: &Arc<Hidraw>, added: bool) {
    tree::changed();
    super::uevent::send(&udev::hidraw_message(h, added));
    node_changed(&alloc::format!("/dev/hidraw{}", h.num), added);
}

fn node_changed(path: &str, added: bool) {
    use super::inotify::*;
    if added {
        notify(path, IN_CREATE, 0);
        notify(path, IN_ATTRIB, 0);
    } else {
        notify(path, IN_DELETE, 0);
    }
}

#[cfg(feature = "ktest")]
pub fn self_test() {
    tree::self_test();
    udev::self_test();
    assert!(owns("/dev/input/event0") && owns("/dev/hidraw3") && !owns("/dev/hidrawx") && !owns("/devices"));
    assert!(matches!(lookup("/run/udev/control"), Some(Found::File(_))));
    assert!(matches!(lookup("/sys/class/input"), Some(Found::Dir)));
    assert_eq!(makedev(13, 64), 13 << 8 | 64);
}
