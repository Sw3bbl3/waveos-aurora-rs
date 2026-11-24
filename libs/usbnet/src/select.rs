//! Which configuration of a device to use, and which vendor chips we drive.
//!
//! Many network devices offer more than one configuration: QEMU's usb-net
//! has RNDIS first and CDC-ECM second; Realtek's RTL8152/8153/8156 (most
//! USB-C docks) start in a vendor mode and also offer CDC-ECM or CDC-NCM;
//! newer ASIX chips (AX88179A, AX88772D) add CDC-NCM. The standard class
//! is the safer path wherever it exists — it is specified, and the same
//! code drives every vendor's chip — so a configuration with a network
//! function we know wins over the first one, NCM before ECM, then a vendor
//! chip we have a driver for, then EEM and RNDIS.

use crate::cdc::{Function, CLASS_VENDOR};

/// A vendor chip with a driver of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chip {
    /// AX88772, AX88772A/B/C (and adapters built on them).
    Ax88772,
    /// AX88179, AX88178A (and AX88179A in its vendor configuration).
    Ax88179,
}

impl Chip {
    pub fn name(self) -> &'static str {
        match self {
            Chip::Ax88772 => "ASIX AX88772",
            Chip::Ax88179 => "ASIX AX88179",
        }
    }
}

/// Vendor and product IDs of the chips above (adapters often keep ASIX's).
pub const CHIPS: &[(u16, u16, Chip)] = &[
    (0x0B95, 0x7720, Chip::Ax88772),
    (0x0B95, 0x772A, Chip::Ax88772),
    (0x0B95, 0x772B, Chip::Ax88772),
    (0x0B95, 0x7E2B, Chip::Ax88772),
    (0x05AC, 0x1402, Chip::Ax88772), // Apple USB Ethernet Adapter
    (0x2001, 0x1A02, Chip::Ax88772), // D-Link DUB-E100 rev. C1
    (0x2001, 0x3C05, Chip::Ax88772), // D-Link DUB-E100 rev. B1
    (0x07D1, 0x3C05, Chip::Ax88772), // D-Link DUB-E100 rev. B1 (alternate ID)
    (0x13B1, 0x0018, Chip::Ax88772), // Linksys USB200M rev. 2
    (0x0B95, 0x1790, Chip::Ax88179),
    (0x0B95, 0x178A, Chip::Ax88179), // AX88178A
    (0x2001, 0x4A00, Chip::Ax88179), // D-Link DUB-1312
    (0x0DF6, 0x0072, Chip::Ax88179), // Sitecom
    (0x04E8, 0xA100, Chip::Ax88179), // Samsung
    (0x17EF, 0x304B, Chip::Ax88179), // Lenovo OneLinkDock
    (0x0930, 0x0A13, Chip::Ax88179), // Toshiba
];

/// Realtek's USB Ethernet chips: no vendor driver here; their CDC
/// configuration (ECM, or NCM on the RTL8156) is used.
pub const REALTEK_CDC_ONLY: &[(u16, u16, &str)] = &[
    (0x0BDA, 0x8152, "Realtek RTL8152"),
    (0x0BDA, 0x8153, "Realtek RTL8153"),
    (0x0BDA, 0x8155, "Realtek RTL8155"),
    (0x0BDA, 0x8156, "Realtek RTL8156"),
];

pub fn chip(vendor: u16, product: u16) -> Option<Chip> {
    CHIPS.iter().find(|c| c.0 == vendor && c.1 == product).map(|c| c.2)
}

/// An interface as the chooser sees it (alternate setting 0 only).
#[derive(Clone, Copy, Debug)]
pub struct Iface {
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
}

/// What the caller asked to prefer (a boot option), if the device offers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prefer {
    Function(Function),
    Vendor,
}

impl Prefer {
    pub fn parse(s: &str) -> Option<Prefer> {
        Some(match s {
            "ecm" => Prefer::Function(Function::Ecm),
            "ncm" => Prefer::Function(Function::Ncm),
            "eem" => Prefer::Function(Function::Eem),
            "rndis" => Prefer::Function(Function::Rndis),
            "vendor" => Prefer::Vendor,
            _ => return None,
        })
    }
}

/// Drivable class codes other than networking (hubs, HID, storage, audio).
fn other_class(class: u8) -> bool {
    matches!(class, 0x01 | 0x03 | 0x08 | 0x09)
}

fn score(vendor: u16, product: u16, ifaces: &[Iface], prefer: Option<Prefer>) -> u32 {
    let mut best = 0;
    for i in ifaces {
        let f = Function::of(i.class, i.subclass, i.protocol);
        let vendor_net = i.class == CLASS_VENDOR && chip(vendor, product).is_some();
        let s = match (f, vendor_net) {
            (Some(Function::Ncm), _) => 50,
            (Some(Function::Ecm), _) => 40,
            (_, true) => 35,
            (Some(Function::Eem), _) => 30,
            (Some(Function::Rndis), _) => 20,
            _ if other_class(i.class) => 10,
            _ => 0,
        };
        let preferred = match prefer {
            Some(Prefer::Function(p)) => f == Some(p),
            Some(Prefer::Vendor) => vendor_net,
            None => false,
        };
        best = best.max(if preferred { 100 } else { s });
    }
    best
}

/// The index (in descriptor order) of the configuration to use: the best
/// network function, the first configuration when nothing scores.
pub fn configuration(vendor: u16, product: u16, configs: &[&[Iface]], prefer: Option<Prefer>) -> usize {
    let mut pick = 0;
    let mut best = 0;
    for (k, c) in configs.iter().enumerate() {
        let s = score(vendor, product, c, prefer);
        if s > best {
            best = s;
            pick = k;
        }
    }
    pick
}

#[cfg(test)]
mod tests {
    use super::*;

    const RNDIS: Iface = Iface { class: 2, subclass: 2, protocol: 0xFF };
    const ECM: Iface = Iface { class: 2, subclass: 6, protocol: 0 };
    const NCM: Iface = Iface { class: 2, subclass: 0x0D, protocol: 0 };
    const DATA: Iface = Iface { class: 0x0A, subclass: 0, protocol: 0 };
    const VENDOR: Iface = Iface { class: 0xFF, subclass: 0xFF, protocol: 0 };
    const MSC: Iface = Iface { class: 8, subclass: 6, protocol: 0x50 };

    #[test]
    fn qemu_usb_net() {
        let configs: [&[Iface]; 2] = [&[RNDIS, DATA], &[ECM, DATA]];
        assert_eq!(configuration(0x0525, 0xA4A2, &configs, None), 1, "ECM over RNDIS");
        let rndis = Prefer::parse("rndis");
        assert_eq!(configuration(0x0525, 0xA4A2, &configs, rndis), 0);
        assert_eq!(configuration(0x0525, 0xA4A2, &configs, Prefer::parse("ncm")), 1, "not offered");
    }

    #[test]
    fn docks_and_asix() {
        // RTL8156: vendor, ECM, NCM.
        let configs: [&[Iface]; 3] = [&[VENDOR], &[ECM, DATA], &[NCM, DATA]];
        assert_eq!(configuration(0x0BDA, 0x8156, &configs, None), 2);
        // RTL8153 asked for "vendor": no Realtek vendor driver, so ECM still.
        let configs: [&[Iface]; 2] = [&[VENDOR], &[ECM, DATA]];
        assert_eq!(configuration(0x0BDA, 0x8153, &configs, Prefer::parse("vendor")), 1);
        // AX88179 has only its vendor configuration; the 179A also NCM.
        assert_eq!(configuration(0x0B95, 0x1790, &[&[VENDOR]], None), 0);
        let configs: [&[Iface]; 2] = [&[VENDOR], &[NCM, DATA]];
        assert_eq!(configuration(0x0B95, 0x1790, &configs, None), 1);
        assert_eq!(configuration(0x0B95, 0x1790, &configs, Prefer::parse("vendor")), 0);
    }

    #[test]
    fn non_network_devices_keep_their_first_configuration() {
        let configs: [&[Iface]; 2] = [&[MSC], &[VENDOR]];
        assert_eq!(configuration(0x1234, 0x5678, &configs, None), 0);
        let configs: [&[Iface]; 2] = [&[VENDOR], &[MSC]];
        assert_eq!(configuration(0x1234, 0x5678, &configs, None), 1);
        assert_eq!(configuration(0x1234, 0x5678, &[], None), 0);
    }
}
