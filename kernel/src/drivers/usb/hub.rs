//! USB hubs (2.0 and 3.x). A hub powers its ports, then reports connects and
//! disconnects on its status-change pipe; each new device is reset and
//! enumerated behind it, with its route (hub ports on the way) and, for slow
//! devices behind a high-speed hub, the hub's transaction translator.

use super::xhci::{Controller, PipeHandler};
use super::{Setup, Speed, UsbDevice};
use crate::sync::IrqMutex;
use alloc::collections::BTreeMap;
use alloc::sync::Arc;

const GET_STATUS: u8 = 0;
const CLEAR_FEATURE: u8 = 1;
const SET_FEATURE: u8 = 3;
const SET_HUB_DEPTH: u8 = 12;

const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
/// Change features, cleared after reading: connection, enable, suspend,
/// over-current, reset; USB 3: link state, config error, warm reset.
const CHANGE_FEATURES: [(u16, u16); 8] = [(0, 16), (1, 17), (2, 18), (3, 19), (4, 20), (6, 25), (7, 26), (5, 29)];

struct Hub {
    slot: u8,
    speed: Speed,
    ports: u8,
    root_port: u8,
    route: u32,
    depth: u32,
    usb3: bool,
}

static HUBS: IrqMutex<BTreeMap<(usize, u8), Arc<Hub>>> = IrqMutex::new(BTreeMap::new());

struct StatusPipe {
    ctrl: &'static Controller,
    slot: u8,
}

impl PipeHandler for StatusPipe {
    fn data(&self, d: &[u8]) {
        let mut bitmap = 0u32;
        for (i, b) in d.iter().take(4).enumerate() {
            bitmap |= (*b as u32) << (8 * i);
        }
        if bitmap != 0 {
            // Port work needs control transfers: done on the enumeration task.
            self.ctrl.hub_changed(self.slot, bitmap);
        }
    }
}

fn port_request(request: u8, feature: u16, port: u8) -> Setup {
    Setup { request_type: 0x23, request, value: feature, index: port as u16, length: 0 }
}

fn port_status(ctrl: &Controller, slot: u8, port: u8) -> Option<(u16, u16)> {
    let mut b = [0u8; 4];
    let setup = Setup { request_type: 0xA3, request: GET_STATUS, value: 0, index: port as u16, length: 4 };
    ctrl.control(slot, setup, Some(&mut b)).ok()?;
    Some((u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]])))
}

pub fn attach(dev: &UsbDevice) -> bool {
    let usb3 = dev.speed as u8 >= Speed::Super as u8;
    let mut d = [0u8; 12];
    let kind = if usb3 { 0x2A00 } else { 0x2900 };
    let setup = Setup { request_type: 0xA0, request: 6, value: kind, index: 0, length: 12 };
    if dev.control(setup, Some(&mut d)).is_err() {
        return false;
    }
    let ports = d[2].min(15);
    let ttt = ((u16::from_le_bytes([d[3], d[4]]) >> 5) & 3) as u32;
    let power_good = (d[5] as u64 * 2).max(100);
    if usb3 {
        let depth = Setup { request_type: 0x20, request: SET_HUB_DEPTH, value: dev.depth as u16, index: 0, length: 0 };
        let _ = dev.control(depth, None);
    }
    let Some(iface) = dev.config.interfaces.first() else { return false };
    let Some(ep) = iface.endpoints.iter().find(|e| e.kind == 3 && e.is_in()).copied() else { return false };
    if !dev.ctrl.configure(dev.slot, dev.speed, &[ep], Some((ports, ttt))) {
        return false;
    }
    for p in 1..=ports {
        let _ = dev.control(port_request(SET_FEATURE, PORT_POWER, p), None);
    }
    crate::sched::sleep_ms(power_good);
    let hub = Arc::new(Hub {
        slot: dev.slot,
        speed: dev.speed,
        ports,
        root_port: dev.root_port,
        route: dev.route,
        depth: dev.depth,
        usb3,
    });
    HUBS.lock().insert((dev.ctrl.index, dev.slot), hub.clone());
    let slot = dev.slot;
    let index = dev.ctrl.index;
    dev.ctrl.on_detach(
        slot,
        alloc::boxed::Box::new(move || {
            HUBS.lock().remove(&(index, slot));
        }),
    );
    log!("usb", "{}: hub with {} ports", dev.name, ports);
    // Devices already plugged in, then changes as they come.
    for p in 1..=ports {
        port_changed(dev.ctrl, &hub, p);
    }
    dev.ctrl.open_pipe(dev.slot, &ep, Arc::new(StatusPipe { ctrl: dev.ctrl, slot: dev.slot }))
}

/// A hub's status pipe reported changes on the ports in `bitmap`.
pub fn changed(ctrl: &'static Controller, slot: u8, bitmap: u32) {
    let Some(hub) = HUBS.lock().get(&(ctrl.index, slot)).cloned() else { return };
    for p in 1..=hub.ports {
        if bitmap & (1 << p) != 0 {
            port_changed(ctrl, &hub, p);
        }
    }
}

fn port_changed(ctrl: &'static Controller, hub: &Hub, port: u8) {
    let Some((status, change)) = port_status(ctrl, hub.slot, port) else { return };
    for (bit, feature) in CHANGE_FEATURES {
        if change & (1 << bit) != 0 {
            let _ = ctrl.control(hub.slot, port_request(CLEAR_FEATURE, feature, port), None);
        }
    }
    let route = hub.route | (port as u32) << (4 * hub.depth);
    let depth = hub.depth + 1;
    let present = ctrl.has_device(hub.root_port, route);
    if status & 1 == 0 {
        if present {
            ctrl.detach_port(hub.root_port, route, depth);
        }
        return;
    }
    if present {
        return;
    }
    let _ = ctrl.control(hub.slot, port_request(SET_FEATURE, PORT_RESET, port), None);
    let deadline = crate::time::uptime_ms() + 500;
    let status = loop {
        crate::sched::sleep_ms(10);
        let Some((s, c)) = port_status(ctrl, hub.slot, port) else { return };
        if c & (1 << 4) != 0 || crate::time::uptime_ms() > deadline {
            let _ = ctrl.control(hub.slot, port_request(CLEAR_FEATURE, 20, port), None);
            break s;
        }
    };
    if status & 2 == 0 {
        log!("usb", "hub port {}: not enabled after reset", port);
        return;
    }
    let speed = if hub.usb3 {
        Speed::Super
    } else if status & (1 << 9) != 0 {
        Speed::Low
    } else if status & (1 << 10) != 0 {
        Speed::High
    } else {
        Speed::Full
    };
    crate::sched::sleep_ms(10);
    // Low/full-speed devices behind a high-speed hub use its transaction translator.
    let tt = hub.speed == Speed::High && matches!(speed, Speed::Low | Speed::Full);
    ctrl.enumerate(hub.root_port, route, depth, speed, Some((hub.slot, port, tt)));
}
