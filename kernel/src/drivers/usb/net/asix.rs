//! ASIX AX88772 (USB 2.0, 10/100) and AX88179 / AX88178A (USB 3.0,
//! gigabit) in their vendor configuration. These follow ASIX's reference
//! bring-up (as the chips' public drivers do): reset the embedded PHY, read
//! the MAC address from the chip, start the receiver, restart
//! autonegotiation, and set the MAC's speed and duplex whenever the link
//! comes up. No checksum offload: the stack checks every packet itself.
//!
//! Not exercised in QEMU (it emulates no such chip): see docs/HARDWARE.md.

use super::{Framing, Kind, Setup2, UsbNic};
use crate::drivers::usb::{Interface, Setup, Speed, UsbDevice};
use crate::sync::IrqMutex;
use alloc::format;
use aurora_usbnet::asix::{self, ax179, ax772, mii};
use aurora_usbnet::select::Chip;
use core::sync::atomic::Ordering;

/// `aux` bit: the device has no interrupt endpoint, so the link is polled.
const POLL_LINK: u32 = 1 << 31;

type Control<'a> = &'a dyn Fn(Setup, Option<&mut [u8]>) -> Result<usize, u8>;

fn vendor(ctl: Control, read: bool, request: u8, value: u16, index: u16, data: &mut [u8]) -> Result<usize, u8> {
    let setup = Setup {
        request_type: if read { asix::READ } else { asix::WRITE },
        request,
        value,
        index,
        length: data.len() as u16,
    };
    ctl(setup, if data.is_empty() { None } else { Some(data) })
}

/// Control access to one chip, during bring-up (a device) or later (an adapter).
struct Regs<'a> {
    ctl: Control<'a>,
}

impl Regs<'_> {
    fn cmd(&self, request: u8, value: u16, index: u16) -> bool {
        vendor(self.ctl, false, request, value, index, &mut []).is_ok()
    }

    fn read(&self, request: u8, value: u16, index: u16, data: &mut [u8]) -> bool {
        vendor(self.ctl, true, request, value, index, data).is_ok_and(|n| n == data.len())
    }

    fn write(&self, request: u8, value: u16, index: u16, data: &[u8]) -> bool {
        let mut d = data.to_vec();
        vendor(self.ctl, false, request, value, index, &mut d).is_ok()
    }

    // AX88772: PHY registers through software MII access.
    fn mdio_read(&self, phy: u8, reg: u16) -> Option<u16> {
        let mut v = [0u8; 2];
        let ok = self.cmd(ax772::SET_SW_MII, 0, 0) && self.read(ax772::READ_MII_REG, phy as u16, reg, &mut v);
        self.cmd(ax772::SET_HW_MII, 0, 0);
        ok.then(|| u16::from_le_bytes(v))
    }

    fn mdio_write(&self, phy: u8, reg: u16, value: u16) -> bool {
        let ok = self.cmd(ax772::SET_SW_MII, 0, 0)
            && self.write(ax772::WRITE_MII_REG, phy as u16, reg, &value.to_le_bytes());
        self.cmd(ax772::SET_HW_MII, 0, 0);
        ok
    }

    // AX88179: MAC registers (wIndex is the access size) and PHY registers.
    fn mac_write(&self, reg: u16, data: &[u8]) -> bool {
        self.write(ax179::ACCESS_MAC, reg, data.len() as u16, data)
    }

    fn mac_read(&self, reg: u16, data: &mut [u8]) -> bool {
        let n = data.len() as u16;
        self.read(ax179::ACCESS_MAC, reg, n, data)
    }

    fn phy179_read(&self, reg: u16) -> Option<u16> {
        let mut v = [0u8; 2];
        self.read(ax179::ACCESS_PHY, ax179::PHY_ID, reg, &mut v).then(|| u16::from_le_bytes(v))
    }

    fn phy179_write(&self, reg: u16, value: u16) -> bool {
        self.write(ax179::ACCESS_PHY, ax179::PHY_ID, reg, &value.to_le_bytes())
    }
}

pub fn bring_up(dev: &UsbDevice, iface: &Interface, chip: Chip) -> Option<Setup2> {
    let bulk_in = *iface.endpoints.iter().find(|e| e.kind == 2 && e.is_in())?;
    let bulk_out = *iface.endpoints.iter().find(|e| e.kind == 2 && !e.is_in())?;
    let interrupt = iface.endpoints.iter().find(|e| e.kind == 3 && e.is_in()).copied();
    if !dev.bus.configure(dev.dev, &[bulk_in, bulk_out], None) {
        return None;
    }
    let ctl = |s: Setup, d: Option<&mut [u8]>| dev.control(s, d);
    let r = Regs { ctl: &ctl };
    let poll = if interrupt.is_none() { POLL_LINK } else { 0 };
    let (mac, aux, framing, rx_size) = match chip {
        Chip::Ax88772 => {
            let (mac, phy) = bring_up_772(&r)?;
            (mac, phy as u32 | poll, Framing::Ax772(IrqMutex::new(Default::default())), 2048)
        }
        Chip::Ax88179 => (bring_up_179(&r, dev.pos.speed)?, poll, Framing::Ax179, 24 * 1024),
    };
    if !aurora_usbnet::valid_mac(&mac) {
        log!("usbnet", "{}: the chip gave no usable MAC address", dev.name);
        return None;
    }
    Some(Setup2 {
        label: format!("{} (USB)", chip.name()),
        kind: Kind::Chip(chip),
        framing,
        control_iface: iface.number,
        bulk_in,
        bulk_out,
        interrupt,
        mac,
        // The interrupt endpoint (or the first poll) tells.
        link: false,
        speed_mbps: 0,
        rx_size,
        tx_size: 2048,
        aux,
    })
}

/// Returns the MAC address and the PHY's address.
fn bring_up_772(r: &Regs) -> Option<([u8; 6], u8)> {
    r.cmd(ax772::WRITE_GPIOS, ax772::GPIO_RSE | ax772::GPIO_GPO_2 | ax772::GPIO_GPO2EN, 0);
    crate::sched::sleep_ms(5);
    let mut id = [0u8; 2];
    if !r.read(ax772::READ_PHY_ID, 0, 0, &mut id) {
        return None;
    }
    let phy = id[1] & 0x1F;
    let embedded = phy == ax772::EMBEDDED_PHY;
    r.cmd(ax772::SW_PHY_SELECT, embedded as u16, 0);
    if embedded {
        r.cmd(ax772::SW_RESET, ax772::SWRESET_IPPD, 0);
        crate::sched::sleep_ms(10);
        r.cmd(ax772::SW_RESET, ax772::SWRESET_CLEAR, 0);
        crate::sched::sleep_ms(60);
        r.cmd(ax772::SW_RESET, ax772::SWRESET_IPRL | ax772::SWRESET_PRL, 0);
    } else {
        r.cmd(ax772::SW_RESET, ax772::SWRESET_IPPD | ax772::SWRESET_PRL, 0);
    }
    crate::sched::sleep_ms(150);
    r.cmd(ax772::WRITE_RX_CTL, 0, 0);
    let mut mac = [0u8; 6];
    if !r.read(ax772::READ_NODE_ID, 0, 0, &mut mac) {
        return None;
    }
    // Autonegotiate 10/100, half and full duplex.
    r.mdio_write(phy, mii::BMCR, mii::BMCR_RESET);
    crate::sched::sleep_ms(10);
    r.mdio_write(phy, mii::ADVERTISE, mii::ADVERTISE_ALL);
    r.mdio_write(phy, mii::BMCR, mii::BMCR_ANENABLE | mii::BMCR_ANRESTART);
    r.cmd(ax772::WRITE_MEDIUM_MODE, ax772::MEDIUM_DEFAULT, 0);
    r.cmd(ax772::WRITE_IPG0, ax772::IPG01, ax772::IPG2);
    // Start receiving: ours, broadcasts, all multicast.
    if !r.cmd(ax772::WRITE_RX_CTL, ax772::RX_CTL_SO | ax772::RX_CTL_AB | ax772::RX_CTL_AMALL, 0) {
        return None;
    }
    Some((mac, phy))
}

fn bulkin_setting(link_mbps: u32, bus: Speed) -> [u8; 5] {
    let fast_bus = matches!(bus, Speed::Super | Speed::SuperPlus);
    match (link_mbps, fast_bus, bus) {
        (1000, true, _) => ax179::BULKIN_GIGA_USB3,
        (1000, false, Speed::High) => ax179::BULKIN_GIGA_USB2,
        (100, true, _) | (100, false, Speed::High) => ax179::BULKIN_100_FAST_BUS,
        _ => ax179::BULKIN_SLOW,
    }
}

fn bring_up_179(r: &Regs, bus: Speed) -> Option<[u8; 6]> {
    // Power the PHY up, select the clocks.
    r.mac_write(ax179::PHYPWR_RSTCTL, &0u16.to_le_bytes());
    if !r.mac_write(ax179::PHYPWR_RSTCTL, &ax179::PHYPWR_RSTCTL_IPRL.to_le_bytes()) {
        return None;
    }
    crate::sched::sleep_ms(200);
    r.mac_write(ax179::CLK_SELECT, &[ax179::CLK_SELECT_ACS | ax179::CLK_SELECT_BCS]);
    crate::sched::sleep_ms(100);
    let mut mac = [0u8; 6];
    if !r.mac_read(ax179::NODE_ID, &mut mac) {
        return None;
    }
    r.mac_write(ax179::RX_BULKIN_QCTRL, &bulkin_setting(1000, bus));
    r.mac_write(ax179::PAUSE_WATERLVL_LOW, &[0x34]);
    r.mac_write(ax179::PAUSE_WATERLVL_HIGH, &[0x52]);
    // No checksum offload.
    r.mac_write(ax179::RXCOE_CTL, &[0]);
    r.mac_write(ax179::TXCOE_CTL, &[0]);
    let rx = ax179::RX_CTL_DROPCRCERR
        | ax179::RX_CTL_IPE
        | ax179::RX_CTL_START
        | ax179::RX_CTL_AP
        | ax179::RX_CTL_AMALL
        | ax179::RX_CTL_AB;
    if !r.mac_write(ax179::RX_CTL, &rx.to_le_bytes()) {
        return None;
    }
    r.mac_write(ax179::MONITOR_MODE, &[ax179::MONITOR_PMETYPE | ax179::MONITOR_PMEPOL | ax179::MONITOR_RWMP]);
    let medium = ax179::MEDIUM_RECEIVE_EN
        | ax179::MEDIUM_TXFLOW_CTRLEN
        | ax179::MEDIUM_RXFLOW_CTRLEN
        | ax179::MEDIUM_FULL_DUPLEX
        | ax179::MEDIUM_GIGAMODE;
    r.mac_write(ax179::MEDIUM_STATUS_MODE, &medium.to_le_bytes());
    // Restart autonegotiation (the PHY advertises everything by default).
    r.phy179_write(mii::BMCR, mii::BMCR_ANENABLE | mii::BMCR_ANRESTART);
    Some(mac)
}

/// The link came up: program the MAC for what the PHY negotiated.
fn link_up(nic: &UsbNic, chip: Chip) {
    let ctl = |s: Setup, d: Option<&mut [u8]>| nic.bus.control(nic.dev, s, d);
    let r = Regs { ctl: &ctl };
    match chip {
        Chip::Ax88772 => {
            let phy = (nic.aux.load(Ordering::Relaxed) & 0x1F) as u8;
            let (Some(adv), Some(lpa)) = (r.mdio_read(phy, mii::ADVERTISE), r.mdio_read(phy, mii::LPA)) else { return };
            let (speed, full) = mii::negotiated(adv, lpa);
            let mut mode = ax772::MEDIUM_DEFAULT;
            if speed != 100 {
                mode &= !ax772::MEDIUM_PS;
            }
            if !full {
                mode &= !ax772::MEDIUM_FD;
            }
            r.cmd(ax772::WRITE_MEDIUM_MODE, mode, 0);
            nic.set_speed(speed);
        }
        Chip::Ax88179 => {
            let Some(sr) = r.phy179_read(ax179::PHY_PHYSR) else { return };
            let mut mode = ax179::MEDIUM_RECEIVE_EN | ax179::MEDIUM_TXFLOW_CTRLEN | ax179::MEDIUM_RXFLOW_CTRLEN;
            let speed = if sr & ax179::PHYSR_GIGA != 0 {
                mode |= ax179::MEDIUM_GIGAMODE;
                1000
            } else if sr & ax179::PHYSR_100 != 0 {
                mode |= ax179::MEDIUM_PS;
                100
            } else {
                10
            };
            if sr & ax179::PHYSR_FULL != 0 {
                mode |= ax179::MEDIUM_FULL_DUPLEX;
            }
            r.mac_write(ax179::RX_BULKIN_QCTRL, &bulkin_setting(speed, nic.usb_speed));
            r.mac_write(ax179::MEDIUM_STATUS_MODE, &mode.to_le_bytes());
            nic.set_speed(speed);
        }
    }
}

fn set(nic: &UsbNic, chip: Chip, up: bool) {
    if up && !nic.link() {
        link_up(nic, chip);
    }
    nic.set_link(up);
}

/// A report from the interrupt endpoint.
pub fn status(nic: &UsbNic, chip: Chip, report: &[u8]) {
    let up = match chip {
        Chip::Ax88772 => asix::ax772_link(report),
        Chip::Ax88179 => asix::ax179_link(report),
    };
    if let Some(up) = up {
        set(nic, chip, up);
    }
}

/// Once a second, for chips without an interrupt endpoint: ask the PHY.
pub fn tick(nic: &UsbNic, chip: Chip) {
    if nic.aux.load(Ordering::Relaxed) & POLL_LINK == 0 {
        return;
    }
    let ctl = |s: Setup, d: Option<&mut [u8]>| nic.bus.control(nic.dev, s, d);
    let r = Regs { ctl: &ctl };
    let up = match chip {
        Chip::Ax88772 => {
            let phy = (nic.aux.load(Ordering::Relaxed) & 0x1F) as u8;
            r.mdio_read(phy, mii::BMSR).map(|b| b & mii::BMSR_LSTATUS != 0)
        }
        Chip::Ax88179 => r.phy179_read(ax179::PHY_PHYSR).map(|s| s & ax179::PHYSR_LINK != 0),
    };
    if let Some(up) = up {
        set(nic, chip, up);
    }
}
