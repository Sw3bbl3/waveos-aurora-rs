//! A small `/sys` for Linux programs: the DRM cards and the PCI devices
//! behind them, laid out as Linux lays them out —
//!
//! - `/sys/class/drm/card0` and `/sys/dev/char/226:0`, links to
//!   `/sys/devices/pci0000:00/<slot>/drm/card0` (with `dev`, `uevent`
//!   holding `DEVNAME=dri/card0`, a `device` link to the PCI device and
//!   one folder per connector: `status`, `enabled`, `dpms`, `modes`, `edid`);
//! - the PCI device: `vendor`, `device`, `subsystem_vendor`,
//!   `subsystem_device`, `revision`, `class`, `config`, `boot_vga`, a
//!   `uevent` with `PCI_SLOT_NAME`, `PCI_ID` and `DRIVER`, and a `subsystem`
//!   link to `/sys/bus/pci`.
//!
//! That is what libdrm reads to enumerate devices (`drmGetDevices2`,
//! `drmGetDevice2`, `drmGetDeviceNameFromFd2`) without udev. The tree is
//! made afresh on each lookup from the display devices and PCI bus.
//!
//! It is one half of `/sys`: the input devices' (`super::input`) are the
//! other, and `input::lookup` and `input::entries` answer for both (its
//! own entries first, then these; folders both have list both).

use super::drm;
use crate::drivers::gpu::{KmsDevice, Status};
use crate::drivers::pci;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

enum Entry {
    Dir,
    File(Vec<u8>),
    Link(String),
}

/// The PCI device a display device drives.
fn pci_of(dev: &dyn KmsDevice) -> Option<pci::Device> {
    let all = pci::devices();
    let by = |f: &dyn Fn(&pci::Device) -> bool| all.iter().find(|d| f(d)).copied();
    let named = match dev.name() {
        "virtio-gpu" | "linux-virtio-gpu" => by(&|d| (d.vendor, d.device) == (0x1AF4, 0x1050)),
        "bochs-vga" => by(&|d| matches!((d.vendor, d.device), (0x1234, 0x1111) | (0x80EE, 0xBEEF))),
        _ => None,
    };
    // Otherwise the display controller (the firmware's framebuffer is the boot VGA's).
    named.or_else(|| by(&|d| d.class == 0x03))
}

/// The card's PCI address, "0000:00:02.0" (what `PCI_SLOT_NAME` says).
pub fn pci_slot(dev: &dyn KmsDevice) -> String {
    let d = pci_of(dev);
    let (b, s, f) = d.map_or((0, 0, 0), |d| (d.bus, d.dev, d.func));
    format!("0000:{b:02x}:{s:02x}.{f}")
}

fn tree() -> BTreeMap<String, Entry> {
    let mut t = BTreeMap::new();
    let dir = |t: &mut BTreeMap<String, Entry>, p: &str| {
        t.insert(String::from(p), Entry::Dir);
    };
    for p in [
        "/sys",
        "/sys/bus",
        "/sys/bus/pci",
        "/sys/bus/pci/devices",
        "/sys/bus/pci/drivers",
        "/sys/class",
        "/sys/class/drm",
        "/sys/dev",
        "/sys/dev/char",
        "/sys/devices",
        "/sys/devices/pci0000:00",
    ] {
        dir(&mut t, p);
    }
    let file = |t: &mut BTreeMap<String, Entry>, p: String, s: &[u8]| {
        t.insert(p, Entry::File(s.to_vec()));
    };
    let link = |t: &mut BTreeMap<String, Entry>, p: String, to: String| {
        t.insert(p, Entry::Link(to));
    };
    file(&mut t, String::from("/sys/class/drm/version"), b"drm 1.1.0 20060810\n");
    for (i, dev) in drm::cards().iter().enumerate() {
        let driver = drm::driver_for(&**dev).name();
        let slot = pci_slot(&**dev);
        let pdir = format!("/sys/devices/pci0000:00/{slot}");
        if !t.contains_key(&pdir) {
            dir(&mut t, &pdir);
            let d = pci_of(&**dev);
            let (vendor, device) = d.map_or((0, 0), |d| (d.vendor, d.device));
            let (bus, sl, func) = d.map_or((0, 0, 0), |d| (d.bus, d.dev, d.func));
            let class = d.map_or(0x30000, |d| (d.class as u32) << 16 | (d.subclass as u32) << 8 | d.prog_if as u32);
            let mut config = alloc::vec![0u8; 256];
            if d.is_some() {
                for off in (0..256).step_by(4) {
                    config[off..off + 4].copy_from_slice(&pci::read32(bus, sl, func, off as u16).to_le_bytes());
                }
            }
            let sub = u32::from_le_bytes(config[0x2C..0x30].try_into().unwrap());
            let (sv, sd) = (sub & 0xFFFF, sub >> 16);
            let rev = config[8];
            file(&mut t, format!("{pdir}/vendor"), format!("0x{vendor:04x}\n").as_bytes());
            file(&mut t, format!("{pdir}/device"), format!("0x{device:04x}\n").as_bytes());
            file(&mut t, format!("{pdir}/subsystem_vendor"), format!("0x{sv:04x}\n").as_bytes());
            file(&mut t, format!("{pdir}/subsystem_device"), format!("0x{sd:04x}\n").as_bytes());
            file(&mut t, format!("{pdir}/revision"), format!("0x{rev:02x}\n").as_bytes());
            file(&mut t, format!("{pdir}/class"), format!("0x{class:06x}\n").as_bytes());
            file(&mut t, format!("{pdir}/boot_vga"), if i == 0 { b"1\n" } else { b"0\n" });
            file(&mut t, format!("{pdir}/enable"), b"1\n");
            file(&mut t, format!("{pdir}/config"), &config);
            let uevent = format!(
                "DRIVER={driver}\nPCI_CLASS={class:X}\nPCI_ID={vendor:04X}:{device:04X}\nPCI_SUBSYS_ID={sv:04X}:{sd:04X}\n\
                 PCI_SLOT_NAME={slot}\nMODALIAS=pci:v{vendor:08X}d{device:08X}sv{sv:08X}sd{sd:08X}bc{:02X}sc{:02X}i{:02X}\n",
                class >> 16,
                class >> 8 & 0xFF,
                class & 0xFF
            );
            file(&mut t, format!("{pdir}/uevent"), uevent.as_bytes());
            link(&mut t, format!("{pdir}/subsystem"), String::from("../../../bus/pci"));
            link(&mut t, format!("{pdir}/driver"), format!("../../../bus/pci/drivers/{driver}"));
            dir(&mut t, &format!("/sys/bus/pci/drivers/{driver}"));
            link(&mut t, format!("/sys/bus/pci/devices/{slot}"), format!("../../../devices/pci0000:00/{slot}"));
            dir(&mut t, &format!("{pdir}/drm"));
        }
        let mut nodes = alloc::vec![(format!("card{i}"), i)];
        if drm::driver_for(&**dev).render() {
            nodes.push((format!("renderD{}", 128 + i), 128 + i));
        }
        for (name, minor) in nodes {
            let ndir = format!("{pdir}/drm/{name}");
            dir(&mut t, &ndir);
            file(&mut t, format!("{ndir}/dev"), format!("{}:{minor}\n", drm::MAJOR).as_bytes());
            let uevent = format!("MAJOR={}\nMINOR={minor}\nDEVNAME=dri/{name}\nDEVTYPE=drm_minor\n", drm::MAJOR);
            file(&mut t, format!("{ndir}/uevent"), uevent.as_bytes());
            link(&mut t, format!("{ndir}/device"), format!("../../../{slot}"));
            link(&mut t, format!("{ndir}/subsystem"), String::from("../../../../../class/drm"));
            let rel = format!("../../devices/pci0000:00/{slot}/drm/{name}");
            link(&mut t, format!("/sys/class/drm/{name}"), rel.clone());
            link(&mut t, format!("/sys/dev/char/{}:{minor}", drm::MAJOR), rel);
        }
        // The connectors, as `card0-Virtual-1`.
        let (cs, _) = dev.state();
        for c in dev.connectors() {
            let name = format!("card{i}-{}", c.name());
            let cdir = format!("{pdir}/drm/card{i}/{name}");
            dir(&mut t, &cdir);
            let status = match c.status {
                Status::Connected => "connected\n",
                Status::Disconnected => "disconnected\n",
                Status::Unknown => "unknown\n",
            };
            let on = cs.iter().any(|s| s.active && s.connectors.contains(&c.id));
            file(&mut t, format!("{cdir}/status"), status.as_bytes());
            file(&mut t, format!("{cdir}/enabled"), if on { b"enabled\n" } else { b"disabled\n" });
            file(&mut t, format!("{cdir}/dpms"), if on { b"On\n" } else { b"Off\n" });
            file(&mut t, format!("{cdir}/connector_id"), format!("{}\n", c.id).as_bytes());
            let modes: String = c.modes.iter().map(|m| format!("{}\n", m.name())).collect();
            file(&mut t, format!("{cdir}/modes"), modes.as_bytes());
            file(&mut t, format!("{cdir}/edid"), c.edid.as_deref().map_or(&[][..], |e| &e[..]));
            link(
                &mut t,
                format!("/sys/class/drm/{name}"),
                format!("../../devices/pci0000:00/{slot}/drm/card{i}/{name}"),
            );
        }
    }
    t
}

fn is_sys(path: &str) -> bool {
    path == "/sys" || path.starts_with("/sys/")
}

/// The target of the link at `path` (a `/sys` path), if it is one.
pub fn link(path: &str) -> Option<String> {
    if !is_sys(path) {
        return None;
    }
    match tree().remove(path)? {
        Entry::Link(to) => Some(to),
        _ => None,
    }
}

/// What `path` (a `/sys` path, links resolved) is: a folder (`None`) or a
/// file's contents; `Err` if there is no such thing.
pub fn open(path: &str) -> Result<Option<Vec<u8>>, ()> {
    match tree().remove(path) {
        Some(Entry::Dir) => Ok(None),
        Some(Entry::File(data)) => Ok(Some(data)),
        _ => Err(()),
    }
}

/// The entries of the folder `path`, with their `d_type`.
pub fn entries(path: &str) -> Vec<(String, u8)> {
    let prefix = format!("{}/", path.trim_end_matches('/'));
    tree()
        .iter()
        .filter_map(|(p, e)| {
            let rest = p.strip_prefix(prefix.as_str())?;
            (!rest.is_empty() && !rest.contains('/')).then(|| {
                let kind = match e {
                    Entry::Dir => 4,
                    Entry::File(_) => 8,
                    Entry::Link(_) => 10,
                };
                (String::from(rest), kind)
            })
        })
        .collect()
}
