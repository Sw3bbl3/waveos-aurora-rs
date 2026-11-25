//! The CDC network functions: ECM (one frame per transfer), NCM (transfer
//! blocks) and EEM (a stream of headed frames).

use super::{random_mac, realtek_name, Framing, Kind, Setup2, UsbNic};
use crate::drivers::usb::{iso, Endpoint, Interface, UsbDevice};
use crate::sync::IrqMutex;
use alloc::format;
use aurora_usbnet::cdc::{self, Function, Functional, Notification};
use aurora_usbnet::ncm;

/// The bulk pair in one of `number`'s alternate settings: (setting, IN, OUT).
pub fn data_endpoints(dev: &UsbDevice, number: u8) -> Option<(u8, Endpoint, Endpoint)> {
    dev.config.interfaces.iter().filter(|i| i.number == number).find_map(|i| {
        let bulk_in = *i.endpoints.iter().find(|e| e.kind == 2 && e.is_in())?;
        let bulk_out = *i.endpoints.iter().find(|e| e.kind == 2 && !e.is_in())?;
        Some((i.alternate, bulk_in, bulk_out))
    })
}

/// The communication interface's interrupt IN endpoint, if it has one.
pub fn interrupt_endpoint(iface: &Interface) -> Option<Endpoint> {
    iface.endpoints.iter().find(|e| e.kind == 3 && e.is_in()).copied()
}

/// The MAC address from the function's string descriptor.
fn device_mac(dev: &UsbDevice, f: &Functional) -> Option<[u8; 6]> {
    if f.mac_string == 0 {
        return None;
    }
    cdc::parse_mac(&dev.string(f.mac_string))
}

fn label(dev: &UsbDevice, f: Function) -> alloc::string::String {
    match realtek_name(&dev.desc) {
        Some(chip) => format!("{chip} ({})", f.name()),
        None => format!("USB {}", f.name()),
    }
}

/// Brings up an ECM, NCM or EEM function on communication interface `iface`.
pub fn bring_up(dev: &UsbDevice, iface: &Interface, f: Function) -> Option<Setup2> {
    let functional = Functional::parse(&iface.extra);
    if f == Function::Eem {
        // One interface with the bulk pair; no MAC address, no notifications.
        let bulk_in = *iface.endpoints.iter().find(|e| e.kind == 2 && e.is_in())?;
        let bulk_out = *iface.endpoints.iter().find(|e| e.kind == 2 && !e.is_in())?;
        if !dev.bus.configure(dev.dev, &[bulk_in, bulk_out], None) {
            return None;
        }
        return Some(Setup2 {
            label: label(dev, f),
            kind: Kind::Cdc(f),
            framing: Framing::Eem(IrqMutex::new(Default::default())),
            control_iface: iface.number,
            bulk_in,
            bulk_out,
            interrupt: None,
            mac: random_mac(),
            link: true,
            speed_mbps: 0,
            rx_size: 16 * 1024,
            tx_size: 16 * 1024,
            aux: 0,
        });
    }
    let data = functional.data_interface.unwrap_or(iface.number + 1);
    let (alt, bulk_in, bulk_out) = data_endpoints(dev, data)?;
    let mac = device_mac(dev, &functional).unwrap_or_else(|| {
        log!("usbnet", "{}: no usable MAC address string; making one up", dev.name);
        random_mac()
    });
    let control = iface.number;
    let class_out = |request: u8, value: u16, data: &mut [u8]| {
        let setup = crate::drivers::usb::Setup::class_interface(request, value, control, data.len() as u16, false);
        dev.control(setup, if data.is_empty() { None } else { Some(data) })
    };
    let mut rx_size = 2048;
    let mut tx_size = 2048;
    let mut framing = Framing::Plain;
    if f == Function::Ncm {
        // Transfer block limits; 16-bit blocks (every device takes them).
        let mut raw = [0u8; ncm::Params::LEN];
        let setup =
            crate::drivers::usb::Setup::class_interface(cdc::GET_NTB_PARAMETERS, 0, control, raw.len() as u16, true);
        let n = dev.control(setup, Some(&mut raw)).ok()?;
        let params = ncm::Params::parse(&raw[..n]).ok()?;
        if params.ntb32() {
            let _ = class_out(cdc::SET_NTB_FORMAT, 0, &mut []);
        }
        // Receive blocks up to 32 KiB (more buys little and costs memory).
        rx_size = (params.in_max as usize).clamp(2048, 32 * 1024);
        let mut size = [0u8; 8];
        size[..4].copy_from_slice(&(rx_size as u32).to_le_bytes());
        let len = if functional.ncm_caps & cdc::NCM_CAP_NTB_INPUT_8 != 0 { 8 } else { 4 };
        if class_out(cdc::SET_NTB_INPUT_SIZE, 0, &mut size[..len]).is_err() {
            // Keep the device's own size, then.
            rx_size = (params.in_max as usize).clamp(2048, 64 * 1024);
        }
        let layout = params.out_layout(false, 32 * 1024);
        tx_size = layout.max;
        let max_frame = match functional.max_segment {
            0 => aurora_usbnet::MAX_FRAME,
            m => (m as usize).clamp(aurora_usbnet::MAX_FRAME, 9216),
        };
        framing = Framing::Ncm { layout, max_frame };
    }
    // Setting 0 has no endpoints; the one with the bulk pair starts traffic.
    if !iso::set_interface(dev.bus, dev.dev, data, alt, &[], &[bulk_in, bulk_out]) {
        log!("usbnet", "{}: could not select the data interface's setting {}", dev.name, alt);
        return None;
    }
    // Ours, broadcasts and all multicast (IPv6 neighbor discovery needs it).
    let filter = cdc::FILTER_DIRECTED | cdc::FILTER_BROADCAST | cdc::FILTER_ALL_MULTICAST;
    if f == Function::Ecm || functional.ncm_caps & cdc::NCM_CAP_PACKET_FILTER != 0 {
        let _ = class_out(cdc::SET_ETHERNET_PACKET_FILTER, filter, &mut []);
    }
    let interrupt = interrupt_endpoint(iface);
    Some(Setup2 {
        label: label(dev, f),
        kind: Kind::Cdc(f),
        framing,
        control_iface: control,
        bulk_in,
        bulk_out,
        interrupt,
        mac,
        // The device reports the link on its interrupt endpoint; one without
        // that endpoint is taken to be connected.
        link: interrupt.is_none(),
        speed_mbps: 0,
        rx_size,
        tx_size,
        aux: 0,
    })
}

/// A notification from the interrupt endpoint.
pub fn status(nic: &UsbNic, report: &[u8]) {
    // Several notifications may arrive in one report (connection, then speed).
    let mut at = 0;
    while at + 8 <= report.len() {
        let len = u16::from_le_bytes([report[at + 6], report[at + 7]]) as usize;
        match cdc::parse_notification(&report[at..]) {
            Some(Notification::Connection(up)) => nic.set_link(up),
            Some(Notification::Speed { down, .. }) => nic.set_speed(down / 1_000_000),
            _ => {}
        }
        at += 8 + len;
    }
}
