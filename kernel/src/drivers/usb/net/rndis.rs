//! RNDIS: Android's USB tethering and many Windows-first adapters.
//!
//! Bring-up talks to the device in control messages (see
//! `aurora_usbnet::rndis`): INITIALIZE, queries for the MAC address, the
//! link and its speed, then a packet filter, after which frames flow as
//! PACKET messages on the bulk endpoints. Later the device announces link
//! changes as status indications, flagged on the interrupt endpoint.

use super::cdc::{data_endpoints, interrupt_endpoint};
use super::{random_mac, Framing, Kind, Setup2, UsbNic};
use crate::drivers::usb::{Interface, Setup, UsbDevice};
use crate::sync::IrqMutex;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use aurora_usbnet::cdc::{self, Function, Functional};
use aurora_usbnet::rndis::{self as msg, Response};
use core::sync::atomic::Ordering;

/// The largest transfer we take from the device.
const RX_SIZE: usize = 16 * 1024;

/// The control channel to one device.
struct Channel<'a> {
    dev: &'a UsbDevice,
    iface: u8,
    next_id: u32,
}

impl Channel<'_> {
    fn send(&self, message: &[u8]) -> Result<(), u8> {
        let mut m = message.to_vec();
        let setup = Setup::class_interface(cdc::SEND_ENCAPSULATED_COMMAND, 0, self.iface, m.len() as u16, false);
        self.dev.control(setup, Some(&mut m)).map(|_| ())
    }

    /// Sends `message` and waits for the completion with its request ID
    /// (status indications that arrive meanwhile are applied to `link`).
    fn request(&mut self, message: &[u8], link: &mut bool) -> Option<(Response, Vec<u8>)> {
        self.send(message).ok()?;
        let id = u32::from_le_bytes(message[8..12].try_into().ok()?);
        for _ in 0..100 {
            let Some(answer) = response(self.dev, self.iface) else {
                crate::sched::sleep_ms(5);
                continue;
            };
            match msg::parse_response(&answer) {
                Ok(Response::Indicate { status }) => apply_indication(status, link),
                Ok(Response::Keepalive { request }) => {
                    let _ = self.send(&msg::keepalive_complete(request));
                }
                Ok(r) if request_id(&r) == Some(id) => return Some((r, answer)),
                _ => {}
            }
        }
        None
    }

    fn id(&mut self) -> u32 {
        self.next_id += 1;
        self.next_id
    }
}

fn request_id(r: &Response) -> Option<u32> {
    match r {
        Response::InitComplete { request, .. }
        | Response::QueryComplete { request, .. }
        | Response::SetComplete { request, .. }
        | Response::KeepaliveComplete { request, .. } => Some(*request),
        _ => None,
    }
}

fn apply_indication(status: u32, link: &mut bool) {
    match status {
        msg::STATUS_MEDIA_CONNECT => *link = true,
        msg::STATUS_MEDIA_DISCONNECT => *link = false,
        _ => {}
    }
}

/// One waiting response, if any (a device with nothing to say answers
/// with a single zero byte, or stalls).
fn response(dev: &UsbDevice, iface: u8) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; msg::MAX_CONTROL];
    let setup = Setup::class_interface(cdc::GET_ENCAPSULATED_RESPONSE, 0, iface, buf.len() as u16, true);
    let n = dev.control(setup, Some(&mut buf)).ok()?;
    (n >= 12).then(|| {
        buf.truncate(n);
        buf
    })
}

pub fn bring_up(dev: &UsbDevice, iface: &Interface) -> Option<Setup2> {
    let functional = Functional::parse(&iface.extra);
    let data = functional.data_interface.unwrap_or(iface.number + 1);
    let (_, bulk_in, bulk_out) = data_endpoints(dev, data)?;
    if !dev.bus.configure(dev.dev, &[bulk_in, bulk_out], None) {
        return None;
    }
    let mut ch = Channel { dev, iface: iface.number, next_id: 0 };
    let mut link = true;
    let id = ch.id();
    let (init, _) = ch.request(&msg::initialize(id, RX_SIZE as u32), &mut link)?;
    let Response::InitComplete { status: msg::STATUS_SUCCESS, init, .. } = init else {
        log!("usbnet", "{}: RNDIS initialization refused", dev.name);
        return None;
    };
    if init.medium != 0 {
        log!("usbnet", "{}: RNDIS medium {} is not Ethernet", dev.name, init.medium);
        return None;
    }
    let query = |ch: &mut Channel, oid: u32, link: &mut bool| -> Option<Vec<u8>> {
        let id = ch.id();
        match ch.request(&msg::query(id, oid), link)? {
            (Response::QueryComplete { status: msg::STATUS_SUCCESS, data, .. }, answer) => Some(answer[data].to_vec()),
            _ => None,
        }
    };
    let mac = query(&mut ch, msg::OID_802_3_PERMANENT_ADDRESS, &mut link)
        .and_then(|a| <[u8; 6]>::try_from(a.get(..6)?).ok())
        .filter(aurora_usbnet::valid_mac)
        .unwrap_or_else(random_mac);
    // In units of 100 bit/s.
    let speed = query(&mut ch, msg::OID_GEN_LINK_SPEED, &mut link)
        .and_then(|a| Some(u32::from_le_bytes(a.get(..4)?.try_into().ok()?)))
        .map_or(0, |s| s / 10_000);
    if let Some(state) = query(&mut ch, msg::OID_GEN_MEDIA_CONNECT_STATUS, &mut link) {
        link = state.first() == Some(&0);
    }
    let filter = msg::FILTER_DIRECTED | msg::FILTER_BROADCAST | msg::FILTER_ALL_MULTICAST;
    let id = ch.id();
    match ch.request(&msg::set(id, msg::OID_GEN_CURRENT_PACKET_FILTER, &filter.to_le_bytes()), &mut link) {
        Some((Response::SetComplete { status: msg::STATUS_SUCCESS, .. }, _)) => {}
        _ => {
            log!("usbnet", "{}: RNDIS packet filter refused", dev.name);
            return None;
        }
    }
    // Several packets per transfer only where the device takes them.
    let max_transfer = (init.max_transfer as usize).clamp(1600, 16 * 1024);
    let max_packets = (init.max_packets as usize).clamp(1, 32);
    Some(Setup2 {
        label: String::from("USB RNDIS"),
        kind: Kind::Cdc(Function::Rndis),
        framing: Framing::Rndis {
            rx: IrqMutex::new(Default::default()),
            shift: init.alignment_shift,
            max_packets: if max_transfer < 2 * msg::packet_len(aurora_usbnet::MAX_FRAME, init.alignment_shift) {
                1
            } else {
                max_packets
            },
        },
        control_iface: iface.number,
        bulk_in,
        bulk_out,
        interrupt: interrupt_endpoint(iface),
        mac,
        link,
        speed_mbps: speed,
        rx_size: RX_SIZE,
        tx_size: max_transfer,
        aux: ch.next_id,
    })
}

/// The interrupt endpoint said a response waits: fetch it (and any after it).
pub fn status(nic: &UsbNic, report: &[u8]) {
    if cdc::parse_notification(report) != Some(cdc::Notification::ResponseAvailable) {
        return;
    }
    for _ in 0..8 {
        let mut buf = vec![0u8; msg::MAX_CONTROL];
        let Ok(n) = nic.class_in(cdc::GET_ENCAPSULATED_RESPONSE, 0, &mut buf) else { return };
        if n < 12 {
            return;
        }
        match msg::parse_response(&buf[..n]) {
            Ok(Response::Indicate { status }) => {
                let mut link = nic.link();
                apply_indication(status, &mut link);
                if status == msg::STATUS_LINK_SPEED_CHANGE {
                    query_speed(nic);
                }
                nic.set_link(link);
            }
            Ok(Response::Keepalive { request }) => {
                let mut m = msg::keepalive_complete(request);
                let _ = nic.class_out(cdc::SEND_ENCAPSULATED_COMMAND, 0, &mut m);
            }
            // The only query after bring-up is the link speed's.
            Ok(Response::QueryComplete { status: msg::STATUS_SUCCESS, data, .. }) => {
                if let Some(speed) = msg::query_u32(&buf[..n], &data) {
                    nic.set_speed(speed / 10_000);
                }
            }
            _ => {}
        }
    }
}

/// Asks for the link speed again (its answer arrives as a response).
fn query_speed(nic: &UsbNic) {
    let id = nic.aux.fetch_add(1, Ordering::Relaxed) + 1;
    let mut m = msg::query(id, msg::OID_GEN_LINK_SPEED);
    let _ = nic.class_out(cdc::SEND_ENCAPSULATED_COMMAND, 0, &mut m);
}
