//! The USB Communications Device Class, as far as networking goes.
//!
//! A CDC network function is two interfaces: a *communication* interface
//! (class 2) with an interrupt endpoint for notifications, and a *data*
//! interface (class 10) with the bulk endpoints. The communication
//! interface's class-specific ("functional") descriptors name the data
//! interface (Union) and the MAC address (Ethernet Networking, as the index
//! of a string descriptor holding twelve hex digits). The data interface
//! usually has no endpoints in alternate setting 0 and the bulk pair in
//! setting 1: selecting setting 1 starts the traffic.

use crate::{le16, le32};

pub const CLASS_COMM: u8 = 0x02;
pub const CLASS_DATA: u8 = 0x0A;
pub const CLASS_MISC: u8 = 0xEF;
pub const CLASS_WIRELESS: u8 = 0xE0;
pub const CLASS_VENDOR: u8 = 0xFF;

pub const SUBCLASS_ACM: u8 = 0x02;
pub const SUBCLASS_ECM: u8 = 0x06;
pub const SUBCLASS_EEM: u8 = 0x0C;
pub const SUBCLASS_NCM: u8 = 0x0D;
pub const SUBCLASS_MBIM: u8 = 0x0E;

/// Class-specific interface descriptor type.
pub const CS_INTERFACE: u8 = 0x24;
pub const FD_HEADER: u8 = 0x00;
pub const FD_UNION: u8 = 0x06;
pub const FD_ETHERNET: u8 = 0x0F;
pub const FD_NCM: u8 = 0x1A;

// Class requests (to the communication interface).
pub const SEND_ENCAPSULATED_COMMAND: u8 = 0x00;
pub const GET_ENCAPSULATED_RESPONSE: u8 = 0x01;
pub const SET_ETHERNET_MULTICAST_FILTERS: u8 = 0x40;
pub const SET_ETHERNET_PACKET_FILTER: u8 = 0x43;
pub const GET_NTB_PARAMETERS: u8 = 0x80;
pub const SET_NTB_FORMAT: u8 = 0x84;
pub const SET_NTB_INPUT_SIZE: u8 = 0x86;
pub const GET_MAX_DATAGRAM_SIZE: u8 = 0x87;
pub const SET_MAX_DATAGRAM_SIZE: u8 = 0x88;
pub const SET_CRC_MODE: u8 = 0x8A;

// SET_ETHERNET_PACKET_FILTER bits.
pub const FILTER_PROMISCUOUS: u16 = 0x01;
pub const FILTER_ALL_MULTICAST: u16 = 0x02;
pub const FILTER_DIRECTED: u16 = 0x04;
pub const FILTER_BROADCAST: u16 = 0x08;
pub const FILTER_MULTICAST: u16 = 0x10;

// Notifications on the interrupt endpoint.
pub const NOTIFY_NETWORK_CONNECTION: u8 = 0x00;
pub const NOTIFY_RESPONSE_AVAILABLE: u8 = 0x01;
pub const NOTIFY_SPEED_CHANGE: u8 = 0x2A;

/// A network function a USB interface can carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Function {
    /// CDC Ethernet Control Model: one frame per transfer.
    Ecm,
    /// CDC Network Control Model: frames aggregated in transfer blocks.
    Ncm,
    /// CDC Ethernet Emulation Model: frames on a byte stream.
    Eem,
    /// Remote NDIS (Android tethering, Windows-oriented adapters).
    Rndis,
}

impl Function {
    /// What a communication interface's class triple carries, if anything.
    pub fn of(class: u8, subclass: u8, protocol: u8) -> Option<Function> {
        match (class, subclass, protocol) {
            (CLASS_COMM, SUBCLASS_ECM, _) => Some(Function::Ecm),
            // Protocol 0 (none) or 1 (the NCM data class's own).
            (CLASS_COMM, SUBCLASS_NCM, 0 | 1) => Some(Function::Ncm),
            (CLASS_COMM, SUBCLASS_EEM, 7) => Some(Function::Eem),
            // RNDIS as Microsoft specifies it (an "ACM" interface with a
            // vendor protocol), and the class codes Android and Windows use.
            (CLASS_COMM, SUBCLASS_ACM, 0xFF) | (CLASS_WIRELESS, 0x01, 0x03) | (CLASS_MISC, 0x04, 0x01) => {
                Some(Function::Rndis)
            }
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Function::Ecm => "CDC-ECM",
            Function::Ncm => "CDC-NCM",
            Function::Eem => "CDC-EEM",
            Function::Rndis => "RNDIS",
        }
    }

    /// The function's data travels on the same interface (EEM) rather than
    /// a separate data interface.
    pub fn single_interface(self) -> bool {
        self == Function::Eem
    }
}

/// What the functional descriptors after a communication interface say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Functional {
    /// The data interface (Union descriptor's first subordinate).
    pub data_interface: Option<u8>,
    /// String descriptor index of the MAC address (Ethernet Networking).
    pub mac_string: u8,
    /// Largest frame the function carries, CRC excluded (0: not given).
    pub max_segment: u16,
    /// Multicast filters it can hold (bit 15: imperfect, hashing).
    pub mc_filters: u16,
    /// NCM's bmNetworkCapabilities (0 when there is no NCM descriptor).
    pub ncm_caps: u8,
    pub has_ncm: bool,
}

/// NCM capability: SET_ETHERNET_PACKET_FILTER is supported.
pub const NCM_CAP_PACKET_FILTER: u8 = 1 << 0;
/// NCM capability: GET/SET_MAX_DATAGRAM_SIZE are supported.
pub const NCM_CAP_MAX_DATAGRAM: u8 = 1 << 3;
/// NCM capability: SET_NTB_INPUT_SIZE takes 8 bytes (size and datagram count).
pub const NCM_CAP_NTB_INPUT_8: u8 = 1 << 5;

impl Functional {
    /// Parses the descriptors that follow an interface descriptor (up to
    /// the next interface). Unknown and malformed ones are skipped.
    pub fn parse(extra: &[u8]) -> Functional {
        let mut f = Functional::default();
        let mut i = 0;
        while i + 2 <= extra.len() {
            let len = extra[i] as usize;
            if len < 2 || i + len > extra.len() {
                break;
            }
            let d = &extra[i..i + len];
            if d[1] == CS_INTERFACE && len >= 3 {
                match d[2] {
                    FD_UNION if len >= 5 => f.data_interface = Some(d[4]),
                    FD_ETHERNET if len >= 13 => {
                        f.mac_string = d[3];
                        f.max_segment = le16(d, 8).unwrap_or(0);
                        f.mc_filters = le16(d, 10).unwrap_or(0);
                    }
                    FD_NCM if len >= 6 => {
                        f.has_ncm = true;
                        f.ncm_caps = d[5];
                    }
                    _ => {}
                }
            }
            i += len;
        }
        f
    }
}

/// A MAC address from its string descriptor's text: twelve hex digits.
pub fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let digits: alloc::vec::Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if digits.len() != 12 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (k, pair) in digits.chunks(2).enumerate() {
        let hex = |c: u8| (c as char).to_digit(16);
        mac[k] = (hex(pair[0])? << 4 | hex(pair[1])?) as u8;
    }
    crate::valid_mac(&mac).then_some(mac)
}

/// A notification from the interrupt endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notification {
    /// The cable (or the phone's network) came up or went down.
    Connection(bool),
    /// Link speeds in bits per second: downstream (to us), upstream.
    Speed { down: u32, up: u32 },
    /// An encapsulated response waits (RNDIS: fetch it with GET_ENCAPSULATED_RESPONSE).
    ResponseAvailable,
}

/// Reads one notification (8-byte header and its data).
pub fn parse_notification(b: &[u8]) -> Option<Notification> {
    if b.len() < 8 || b[0] != 0xA1 {
        // RNDIS devices often send a bare 8-byte RESPONSE_AVAILABLE (01 00 00 00 ...).
        return (b.len() >= 4 && le32(b, 0) == Some(1)).then_some(Notification::ResponseAvailable);
    }
    let value = le16(b, 2)?;
    let length = le16(b, 6)? as usize;
    match b[1] {
        NOTIFY_NETWORK_CONNECTION => Some(Notification::Connection(value != 0)),
        NOTIFY_RESPONSE_AVAILABLE => Some(Notification::ResponseAvailable),
        NOTIFY_SPEED_CHANGE if length >= 8 => Some(Notification::Speed { down: le32(b, 8)?, up: le32(b, 12)? }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_ecm_descriptors() {
        // QEMU usb-net's CDC configuration: header, union (0 → 1), Ethernet (string 3).
        let extra = [5, 0x24, 0, 0x10, 0x01, 5, 0x24, 6, 0, 1, 13, 0x24, 0x0F, 3, 0, 0, 0, 0, 0xEA, 0x05, 0, 0, 0];
        let f = Functional::parse(&extra);
        assert_eq!(f.data_interface, Some(1));
        assert_eq!(f.mac_string, 3);
        assert_eq!(f.max_segment, 1514);
        assert!(!f.has_ncm);
    }

    #[test]
    fn macs() {
        assert_eq!(parse_mac("400102030405"), Some([0x40, 1, 2, 3, 4, 5]));
        assert_eq!(parse_mac("40010203040"), None);
        assert_eq!(parse_mac("41010203040g"), None);
        assert_eq!(parse_mac("010203040506"), None, "multicast");
        assert_eq!(parse_mac("000000000000"), None);
    }

    #[test]
    fn notifications() {
        assert_eq!(parse_notification(&[0xA1, 0, 1, 0, 1, 0, 0, 0]), Some(Notification::Connection(true)));
        assert_eq!(parse_notification(&[0xA1, 0, 0, 0, 1, 0, 0, 0]), Some(Notification::Connection(false)));
        let speed = [0xA1, 0x2A, 0, 0, 0, 0, 8, 0, 0x00, 0xCA, 0x9A, 0x3B, 0x00, 0xE1, 0xF5, 0x05];
        assert_eq!(parse_notification(&speed), Some(Notification::Speed { down: 1_000_000_000, up: 100_000_000 }));
        assert_eq!(parse_notification(&[1, 0, 0, 0, 0, 0, 0, 0]), Some(Notification::ResponseAvailable));
        assert_eq!(parse_notification(&[0xA1, 0x2A, 0, 0, 0, 0, 8, 0]), None, "speed without its data");
    }

    #[test]
    fn functions() {
        assert_eq!(Function::of(2, 6, 0), Some(Function::Ecm));
        assert_eq!(Function::of(2, 0x0D, 0), Some(Function::Ncm));
        assert_eq!(Function::of(2, 2, 0xFF), Some(Function::Rndis));
        assert_eq!(Function::of(0xEF, 4, 1), Some(Function::Rndis));
        assert_eq!(Function::of(2, 2, 1), None, "a modem");
        assert_eq!(Function::of(2, 0x0E, 0), None, "MBIM");
    }
}
